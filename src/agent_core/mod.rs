use crate::config::AgentConfig;
use crate::context_pack::*;
use crate::retriever::Retriever;
use crate::task_store::UserSessionStateRecord;
use crate::tool_registry::{ToolAction, ToolRegistry};
use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
#[cfg(test)]
use {std::cell::RefCell, std::collections::VecDeque};

mod command_parse;
mod context_assembly;
mod llm_client;
mod logging;
mod recovery;
mod retriever_factory;
mod trace;
mod types;
mod watchdog;

#[allow(unused_imports)]
pub(crate) use self::command_parse::{map_llm_plan, parse_llm_plan, parse_user_command, LlmPlan};
#[allow(unused_imports)]
pub(crate) use self::context_assembly::{
    append_context_section_overview, append_session_state_lines, build_context_pack,
    build_context_summary, derive_runtime_session_state, load_business_context_snapshot,
    merge_string_arrays_with_runtime_reserve, project_session_state_to_trace,
    render_context_preview, select_previous_observations, BusinessContextSnapshot,
    ContextAssembler, ContextCompactionConfig, DropReason, GoalSignal, MemoryBudget,
    RuntimeSessionStateSnapshot, SessionState,
};
pub(crate) use self::llm_client::{is_llm_auth_error, LlmClient, LlmConfig};
#[cfg(test)]
pub(crate) use self::logging::build_agent_log_payload;
pub(crate) use self::logging::{log_agent_info, log_agent_warn};
pub(crate) use self::recovery::{
    default_recovery_for_failure, failure_to_observation, FailureAction, RecoveryOutcome,
    ReplanScope, RuntimeControllerState, StepFailureKind,
};
pub(crate) use self::retriever_factory::{select_retriever, RetrieverMode};
#[allow(unused_imports)]
pub(crate) use self::trace::{AgentRunTrace, AgentTraceIndexEntry, ObservationTrace};
pub(crate) use self::types::{
    normalize_optional_text, observation_kind_for_action, resulting_source_name,
};
pub(crate) use self::watchdog::{
    classify_tool_execution_failure, default_expected_observation_for_decision,
    default_minimum_novelty_for_kind, detect_low_value_observation_failure,
    detect_repeated_action_failure, detect_stalled_trajectory_failure,
    detect_trajectory_drift_failure, parse_expected_observation, validate_expected_observation,
};
pub(crate) const DEFAULT_MAX_STEPS: usize = 8;
const MAX_STEP_RETRIES: usize = 1;
pub(crate) const DEFAULT_MAX_REPLANS: usize = 3;
const DEFAULT_OPENAI_MODEL: &str = "deepseek-chat";
const DEFAULT_MOONSHOT_MODEL: &str = "kimi-k2.5";
const LLM_PROVIDER_PRIORITY: [&str; 3] = ["DEEPSEEK", "MOONSHOT", "OPENAI"];
#[derive(Debug, Clone)]
pub struct AgentRunContext {
    source_type: String,
    trigger_type: Option<String>,
    user_id: Option<String>,
    message_ids: Vec<String>,
    task_id: Option<String>,
    article_id: Option<String>,
    session_text: Option<String>,
    context_token_present: bool,
    user_session_state: Option<UserSessionStateRecord>,
}

impl AgentRunContext {
    pub fn agent_demo() -> Self {
        Self {
            source_type: "agent_demo".to_string(),
            trigger_type: None,
            user_id: None,
            message_ids: Vec::new(),
            task_id: None,
            article_id: None,
            session_text: None,
            context_token_present: false,
            user_session_state: None,
        }
    }

    pub fn wechat_chat(
        user_id: impl Into<String>,
        trigger_type: impl Into<String>,
        message_ids: Vec<String>,
    ) -> Self {
        let user_id = user_id.into();
        let trigger_type = trigger_type.into();
        Self {
            source_type: "wechat_chat".to_string(),
            trigger_type: if trigger_type.trim().is_empty() {
                None
            } else {
                Some(trigger_type)
            },
            user_id: if user_id.trim().is_empty() {
                None
            } else {
                Some(user_id)
            },
            message_ids: message_ids
                .into_iter()
                .filter(|value| !value.trim().is_empty())
                .collect(),
            task_id: None,
            article_id: None,
            session_text: None,
            context_token_present: false,
            user_session_state: None,
        }
    }

    #[allow(dead_code)]
    pub fn with_task_id(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = normalize_optional_text(task_id.into());
        self
    }

    #[allow(dead_code)]
    pub fn with_article_id(mut self, article_id: impl Into<String>) -> Self {
        self.article_id = normalize_optional_text(article_id.into());
        self
    }

    pub fn with_session_text(mut self, session_text: impl Into<String>) -> Self {
        self.session_text = normalize_optional_text(session_text.into());
        self
    }

    pub fn with_context_token_present(mut self, present: bool) -> Self {
        self.context_token_present = present;
        self
    }

    pub fn with_user_session_state(mut self, state: Option<UserSessionStateRecord>) -> Self {
        self.user_session_state = state;
        self
    }
}

#[allow(dead_code)]
pub type RunContext = AgentRunContext;

#[derive(Debug, Clone)]
pub(crate) struct AgentObservation {
    step: usize,
    source: String,
    content: String,
    kind: Option<ObservationKind>,
}

impl AgentObservation {
    fn tool_result(
        step: usize,
        tool_name: &str,
        output: &str,
        kind: Option<ObservationKind>,
    ) -> Self {
        Self {
            step,
            source: format!("tool:{tool_name}"),
            content: output.to_string(),
            kind,
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub enum ContextPreviewMode {
    Summary,
    Verbose,
}
#[derive(Debug, Clone)]
pub(crate) struct PlannerInput {
    raw_user_input: String,
    assembled_user_prompt: String,
    context_sections: Vec<ContextSectionSnapshot>,
    context_budget_summary: ContextBudgetSummary,
    context_summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionPlan {
    steps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PlanStepStatus {
    Pending,
    Running,
    Done,
    Failed,
    Skipped,
}
impl PlanStepStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RuntimePlanStep {
    description: String,
    status: PlanStepStatus,
    expected_observation: Option<ExpectedObservation>,
    retry_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedDecision {
    decision: AgentDecision,
    plan: Option<ExecutionPlan>,
    progress_note: Option<String>,
    expected_observation: Option<ExpectedObservation>,
}

#[derive(Debug, Clone)]
pub(crate) struct FailureDecision {
    kind: StepFailureKind,
    action: FailureAction,
    replan_scope: Option<ReplanScope>,
    detail: String,
    source: String,
    user_message: Option<String>,
}

impl PlannedDecision {
    pub(crate) fn new(decision: AgentDecision) -> Self {
        Self {
            decision,
            plan: None,
            progress_note: None,
            expected_observation: None,
        }
    }

    pub(crate) fn with_plan(mut self, plan: Option<ExecutionPlan>) -> Self {
        self.plan = plan;
        self
    }

    pub(crate) fn with_progress_note(mut self, progress_note: Option<String>) -> Self {
        self.progress_note = progress_note;
        self
    }

    pub(crate) fn with_expected_observation(
        mut self,
        expected_observation: Option<ExpectedObservation>,
    ) -> Self {
        self.expected_observation = expected_observation;
        self
    }

    pub(crate) fn summary(&self) -> String {
        self.decision.summary()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ObservationKind {
    Text,
    JsonObject,
    FileMutation,
    TaskStatus,
    TaskList,
    ArchiveContent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DoneRule {
    ToolSuccess,
    NonEmptyOutput,
    RequiresJsonField { field: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ExpectedObservation {
    kind: ObservationKind,
    done_rule: DoneRule,
    expected_fields: Vec<String>,
    minimum_novelty: Option<MinimumNovelty>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MinimumNovelty {
    DifferentFromLast,
}

impl ExpectedObservation {
    fn summary(&self) -> String {
        let mut parts = vec![
            format!("kind={}", self.kind.as_str()),
            match &self.done_rule {
                DoneRule::ToolSuccess => "done=tool_success".to_string(),
                DoneRule::NonEmptyOutput => "done=non_empty_output".to_string(),
                DoneRule::RequiresJsonField { field } => format!("done=json_field:{field}"),
            },
        ];
        if !self.expected_fields.is_empty() {
            parts.push(format!("fields={}", self.expected_fields.join("|")));
        }
        if let Some(minimum_novelty) = self.effective_minimum_novelty() {
            parts.push(format!("novelty={}", minimum_novelty.as_str()));
        }
        parts.join(", ")
    }

    fn effective_minimum_novelty(&self) -> Option<MinimumNovelty> {
        self.minimum_novelty
            .clone()
            .or_else(|| default_minimum_novelty_for_kind(&self.kind))
    }
}

impl ObservationKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::JsonObject => "json_object",
            Self::FileMutation => "file_mutation",
            Self::TaskStatus => "task_status",
            Self::TaskList => "task_list",
            Self::ArchiveContent => "archive_content",
        }
    }
}

impl MinimumNovelty {
    fn as_str(&self) -> &'static str {
        match self {
            Self::DifferentFromLast => "different_from_last",
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub(crate) enum PlanningPolicy {
    Reactive,
}

impl PlanningPolicy {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Reactive => "reactive",
        }
    }
}
#[derive(Debug)]
pub struct AgentRunResult {
    pub output: String,
    pub run_id: String,
    pub trace_json_path: Option<PathBuf>,
    /// 最终步推导出的 runtime session state（用于回写持久层）
    pub(crate) runtime_session_state: Option<RuntimeSessionStateSnapshot>,
}
pub struct AgentCore {
    workspace_root: PathBuf,
    // 负责实际执行工具动作（读写文件等）
    tool_registry: ToolRegistry,
    task_store_db_path: Option<PathBuf>,
    context_compaction: ContextCompactionConfig,
    llm_client: Option<LlmClient>,
    // 防止 Agent 无穷循环的安全阀
    max_steps: usize,
    max_replans: usize,
    planning_policy: PlanningPolicy,
    // 可插拔检索器（默认 RuleRetriever）
    retriever: Box<dyn Retriever + Send + Sync>,
    // 运行时模式策略
    mode: crate::mode_policy::AgentMode,
    #[cfg(test)]
    scripted_decisions: RefCell<VecDeque<PlannedDecision>>,
}

impl std::fmt::Debug for AgentCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentCore")
            .field("workspace_root", &self.workspace_root)
            .field("task_store_db_path", &self.task_store_db_path)
            .field("max_steps", &self.max_steps)
            .field("max_replans", &self.max_replans)
            .field("planning_policy", &self.planning_policy)
            .field("retriever", &"(dyn Retriever)")
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
enum LoopControl {
    Continue(Option<AgentObservation>),
    Finish(String),
}

impl AgentCore {
    #[allow(dead_code)]
    pub fn new(workspace_root: impl Into<PathBuf>) -> Result<Self> {
        Self::with_max_steps_and_task_store_db_path_with_compaction(
            workspace_root,
            DEFAULT_MAX_STEPS,
            None::<PathBuf>,
            ContextCompactionConfig::default(),
        )
    }

    #[allow(dead_code)]
    pub fn with_max_steps(workspace_root: impl Into<PathBuf>, max_steps: usize) -> Result<Self> {
        Self::with_max_steps_and_task_store_db_path_with_compaction(
            workspace_root,
            max_steps,
            None::<PathBuf>,
            ContextCompactionConfig::default(),
        )
    }

    #[allow(dead_code)]
    pub fn with_task_store_db_path(
        workspace_root: impl Into<PathBuf>,
        task_store_db_path: impl Into<PathBuf>,
    ) -> Result<Self> {
        Self::with_max_steps_and_task_store_db_path_with_compaction(
            workspace_root,
            DEFAULT_MAX_STEPS,
            Some(task_store_db_path),
            ContextCompactionConfig::default(),
        )
    }

    pub fn with_task_store_db_path_and_agent_config(
        workspace_root: impl Into<PathBuf>,
        task_store_db_path: impl Into<PathBuf>,
        agent_config: &AgentConfig,
    ) -> Result<Self> {
        Self::with_max_steps_and_task_store_db_path_with_compaction_and_retriever_mode(
            workspace_root,
            DEFAULT_MAX_STEPS,
            Some(task_store_db_path),
            ContextCompactionConfig::from_agent_config(agent_config),
            Some(agent_config.retriever_mode.as_str()),
            Some(agent_config.embedding_provider.as_str()),
            agent_config.retriever_rollout_enabled,
            &agent_config.retriever_rollout_allow_users,
            crate::mode_policy::AgentMode::from_config(&agent_config.mode),
        )
    }

    #[allow(dead_code)]
    fn with_max_steps_and_task_store_db_path(
        workspace_root: impl Into<PathBuf>,
        max_steps: usize,
        task_store_db_path: Option<impl Into<PathBuf>>,
    ) -> Result<Self> {
        Self::with_max_steps_and_task_store_db_path_with_compaction(
            workspace_root,
            max_steps,
            task_store_db_path,
            ContextCompactionConfig::default(),
        )
    }

    fn with_max_steps_and_task_store_db_path_with_compaction(
        workspace_root: impl Into<PathBuf>,
        max_steps: usize,
        task_store_db_path: Option<impl Into<PathBuf>>,
        context_compaction: ContextCompactionConfig,
    ) -> Result<Self> {
        Self::with_max_steps_and_task_store_db_path_with_compaction_and_retriever_mode(
            workspace_root,
            max_steps,
            task_store_db_path,
            context_compaction,
            None,
            None,
            false,
            &[],
            crate::mode_policy::AgentMode::Restricted,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn with_max_steps_and_task_store_db_path_with_compaction_and_retriever_mode(
        workspace_root: impl Into<PathBuf>,
        max_steps: usize,
        task_store_db_path: Option<impl Into<PathBuf>>,
        context_compaction: ContextCompactionConfig,
        retriever_mode: Option<&str>,
        embedding_provider: Option<&str>,
        retriever_rollout_enabled: bool,
        retriever_rollout_allow_users: &[String],
        agent_mode: crate::mode_policy::AgentMode,
    ) -> Result<Self> {
        if max_steps == 0 {
            bail!("max_steps 必须大于 0");
        }
        let workspace_root = workspace_root.into();
        let task_store_db_path = task_store_db_path.map(|value| value.into());
        let tool_registry = ToolRegistry::with_task_store_db_path(
            workspace_root.clone(),
            task_store_db_path.clone(),
        )?;

        let retriever_config_mode = match retriever_mode {
            Some(text) => RetrieverMode::from_config(text)?,
            None => RetrieverMode::Rule,
        };
        let embedding_provider_name = embedding_provider.unwrap_or("noop");
        let retriever = select_retriever(
            retriever_config_mode,
            task_store_db_path.as_deref(),
            embedding_provider_name,
            retriever_rollout_enabled,
            retriever_rollout_allow_users,
        );

        Ok(Self {
            workspace_root: workspace_root.clone(),
            tool_registry,
            task_store_db_path,
            context_compaction,
            llm_client: LlmClient::from_env()?,
            max_steps,
            max_replans: DEFAULT_MAX_REPLANS,
            planning_policy: PlanningPolicy::Reactive,
            retriever,
            mode: agent_mode,
            #[cfg(test)]
            scripted_decisions: RefCell::new(VecDeque::new()),
        })
    }

    #[cfg(test)]
    fn with_scripted_decisions(
        workspace_root: impl Into<PathBuf>,
        max_steps: usize,
        decisions: Vec<AgentDecision>,
    ) -> Result<Self> {
        let agent = Self::with_max_steps_and_task_store_db_path(
            workspace_root,
            max_steps,
            None::<PathBuf>,
        )?;
        agent
            .scripted_decisions
            .borrow_mut()
            .extend(decisions.into_iter().map(PlannedDecision::new));
        Ok(agent)
    }

    pub fn run(&self, user_input: &str) -> Result<String> {
        self.run_with_context(user_input, AgentRunContext::agent_demo())
            .map(|result| result.output)
    }

    pub fn run_with_context(
        &self,
        user_input: &str,
        context: AgentRunContext,
    ) -> Result<AgentRunResult> {
        let started = Instant::now();
        let mut trace = AgentRunTrace::new(&self.workspace_root, user_input, context);
        let run_id = trace.run_id.clone();
        trace.configure_controller_limits(self.max_steps, self.max_replans);
        let mut last_observation: Option<AgentObservation> = None;
        let result = (|| -> Result<String> {
            for step in 0..self.max_steps {
                trace.step_count = step + 1;
                let planned =
                    self.decide(user_input, last_observation.as_ref(), step, &mut trace)?;
                if let Some(failure) =
                    self.watchdog_review(step, &planned, &trace, last_observation.as_ref())
                {
                    match self.record_watchdog_failure(step, &planned, failure, &mut trace)? {
                        LoopControl::Continue(observation) => {
                            last_observation = observation;
                            continue;
                        }
                        LoopControl::Finish(answer) => return Ok(answer),
                    }
                }
                match self.execute_planned_decision(
                    step,
                    &planned,
                    &mut trace,
                    last_observation.as_ref(),
                )? {
                    LoopControl::Continue(observation) => {
                        last_observation = observation;
                    }
                    LoopControl::Finish(answer) => return Ok(answer),
                }
            }
            bail!("达到最大步骤，未能收敛")
        })();

        match &result {
            Ok(answer) => trace.finish_success(answer, started.elapsed()),
            Err(err) => trace.finish_error(&err.to_string(), started.elapsed()),
        }

        let trace_json_path = match trace.persist() {
            Ok(path) => Some(path),
            Err(err) => {
                log_agent_warn(
                    "agent_trace_persist_failed",
                    vec![
                        ("error_kind", json!("agent_trace_persist_failed")),
                        ("detail", json!(err.to_string())),
                    ],
                );
                None
            }
        };

        result.map(|output| AgentRunResult {
            output,
            run_id,
            trace_json_path,
            runtime_session_state: trace.final_runtime_session_state.clone(),
        })
    }

    /// 事后补更新 trace 中的 persistent_state_updated 字段
    pub fn patch_trace_persistent_state_updated(
        &self,
        json_path: &Path,
        updated: bool,
    ) -> Result<()> {
        let content = fs::read_to_string(json_path)
            .with_context(|| format!("读取 trace 文件失败: {}", json_path.display()))?;
        let mut payload: Value = serde_json::from_str(&content).context("解析 trace JSON 失败")?;
        payload["persistent_state_updated"] = json!(updated);
        let updated_content =
            serde_json::to_string_pretty(&payload).context("序列化更新后的 trace 失败")?;
        fs::write(json_path, format!("{updated_content}\n"))
            .with_context(|| format!("写入更新后的 trace 失败: {}", json_path.display()))?;
        Ok(())
    }

    pub fn preview_context_with_context(
        &self,
        user_input: &str,
        context: AgentRunContext,
    ) -> Result<String> {
        self.preview_context_with_context_mode(user_input, context, ContextPreviewMode::Summary)
    }

    pub fn preview_context_with_context_mode(
        &self,
        user_input: &str,
        context: AgentRunContext,
        mode: ContextPreviewMode,
    ) -> Result<String> {
        let mut trace = AgentRunTrace::new(&self.workspace_root, user_input, context);
        trace.configure_controller_limits(self.max_steps, self.max_replans);

        let (business_context, session_state) = load_business_context_snapshot(
            self.task_store_db_path.as_deref(),
            &trace,
            self.context_compaction.memory_budget,
            self.retriever.as_ref(),
            false,
        )?;
        project_session_state_to_trace(&mut trace, &session_state);
        let runtime_session_state =
            derive_runtime_session_state(&trace, user_input, None, business_context.as_ref());
        let planner_input = ContextAssembler {
            include_previous_observations: self.context_compaction.include_previous_observations,
        }
        .assemble_with_summary_strategy(
            &trace,
            user_input,
            None,
            Some(&runtime_session_state),
            &self.tool_registry.available_tool_descriptions(),
            business_context.as_ref(),
            self.context_compaction.session_summary_strategy,
        );

        Ok(render_context_preview(
            &trace,
            &planner_input,
            &runtime_session_state,
            &session_state,
            mode,
        ))
    }

    fn watchdog_review(
        &self,
        step: usize,
        planned: &PlannedDecision,
        trace: &AgentRunTrace,
        observation: Option<&AgentObservation>,
    ) -> Option<FailureDecision> {
        detect_repeated_action_failure(step, planned, trace, observation)
            .or_else(|| detect_stalled_trajectory_failure(trace))
            .or_else(|| detect_trajectory_drift_failure(planned, trace))
    }

    fn record_watchdog_failure(
        &self,
        step: usize,
        planned: &PlannedDecision,
        failure: FailureDecision,
        trace: &mut AgentRunTrace,
    ) -> Result<LoopControl> {
        trace.record_failure(step, &failure);
        trace.mark_next_plan_step_running(planned.expected_observation.clone());
        trace.mark_running_plan_step_failed();
        self.handle_recorded_failure(step, failure, trace)
    }

    fn execute_planned_decision(
        &self,
        step: usize,
        planned: &PlannedDecision,
        trace: &mut AgentRunTrace,
        previous_observation: Option<&AgentObservation>,
    ) -> Result<LoopControl> {
        match &planned.decision {
            AgentDecision::CallTool(action) => {
                trace.mark_next_plan_step_running(planned.expected_observation.clone());
                self.execute_tool_action(step, action.clone(), trace, previous_observation)
            }
            AgentDecision::Final(answer) => Ok(LoopControl::Finish(answer.clone())),
        }
    }

    fn execute_tool_action(
        &self,
        step: usize,
        action: ToolAction,
        trace: &mut AgentRunTrace,
        previous_observation: Option<&AgentObservation>,
    ) -> Result<LoopControl> {
        let action_source = resulting_source_name(&action);

        // restricted 运行时门禁检查
        let decision = crate::mode_policy::check_tool_action(self.mode, &action_source);
        if !decision.allowed {
            log_agent_warn(
                "tool_action_policy_denied",
                vec![
                    ("step", json!(step)),
                    ("action", json!(action_source)),
                    ("reason", json!(decision.reason)),
                ],
            );
            let failure = FailureDecision {
                kind: StepFailureKind::ManualIntervention,
                action: FailureAction::Replan,
                replan_scope: Some(ReplanScope::CurrentStep),
                detail: decision.reason,
                source: action_source.clone(),
                user_message: None,
            };
            trace.record_failure(step, &failure);
            return self.handle_recorded_failure(step, failure, trace);
        }

        for attempt in 0..=MAX_STEP_RETRIES {
            let tool_trace = trace.start_tool_call(step, &action);
            match self.tool_registry.execute(action.clone()) {
                Ok(result) => {
                    let kind = observation_kind_for_action(&action);
                    let observation =
                        AgentObservation::tool_result(step, result.tool, &result.output, kind);
                    if let Err(err) = validate_expected_observation(
                        trace.running_plan_expected_observation(),
                        &observation,
                    ) {
                        let failure = FailureDecision {
                            kind: StepFailureKind::Expectation,
                            action: FailureAction::Replan,
                            replan_scope: Some(ReplanScope::CurrentStep),
                            detail: err.to_string(),
                            source: observation.source.clone(),
                            user_message: None,
                        };
                        trace.finish_tool_call_error(
                            tool_trace,
                            &format!("expected_observation_failed: {err}"),
                        );
                        trace.record_observation(&observation);
                        trace.record_failure(step, &failure);
                        return self.handle_recorded_failure(step, failure, trace);
                    }
                    if let Some(failure) = detect_low_value_observation_failure(
                        trace.running_plan_expected_observation(),
                        &observation,
                        previous_observation,
                    ) {
                        trace.finish_tool_call_error(
                            tool_trace,
                            &format!("low_value_observation: {}", failure.detail),
                        );
                        trace.record_observation(&observation);
                        trace.record_failure(step, &failure);
                        return self.handle_recorded_failure(step, failure, trace);
                    }
                    trace.finish_tool_call_success(tool_trace, result.tool, &result.output);
                    trace.record_observation(&observation);
                    return Ok(LoopControl::Continue(Some(observation)));
                }
                Err(err) => {
                    trace.finish_tool_call_error(tool_trace, &err.to_string());
                    let failure =
                        classify_tool_execution_failure(action_source.clone(), &err.to_string());
                    trace.record_failure(step, &failure);
                    if failure.action == FailureAction::RetryStep && attempt < MAX_STEP_RETRIES {
                        let retry_count = trace.mark_running_plan_step_retrying();
                        log_agent_warn(
                            "agent_step_retry_scheduled",
                            vec![
                                ("step", json!(step)),
                                ("retry_count", json!(retry_count)),
                                ("failure_kind", json!(failure.kind.as_str())),
                                ("detail", json!(failure.detail.clone())),
                            ],
                        );
                        continue;
                    }
                    return self.handle_recorded_failure(step, failure, trace);
                }
            }
        }
        bail!("达到最大重试次数，工具执行未收敛")
    }

    fn handle_recorded_failure(
        &self,
        step: usize,
        failure: FailureDecision,
        trace: &mut AgentRunTrace,
    ) -> Result<LoopControl> {
        // 阶段 B/C：统一映射表 + 防循环保护
        let policy = default_recovery_for_failure(failure.kind);

        let kind_count = trace
            .controller_state
            .record_recovery_for_kind(&failure.kind);

        // 防循环：同一 kind 超过阈值则升级 action
        let escalated = kind_count > policy.max_attempts && policy.max_attempts > 0;
        let effective_action = if escalated {
            log_agent_warn(
                "recovery_escalated",
                vec![
                    ("step", json!(step)),
                    ("failure_kind", json!(failure.kind.as_str())),
                    ("original_action", json!(failure.action.as_str())),
                    ("escalated_action", json!(policy.escalate_to.as_str())),
                    ("kind_count", json!(kind_count)),
                    ("max_attempts", json!(policy.max_attempts)),
                ],
            );
            policy.escalate_to
        } else {
            failure.action
        };

        match effective_action {
            FailureAction::RetryStep => {
                trace.record_recovery_attempt(
                    step,
                    &failure,
                    RecoveryOutcome::Failed,
                    escalated,
                    effective_action,
                );
                Err(anyhow!(failure.detail))
            }
            FailureAction::Replan => {
                if !self.can_replan() {
                    trace.record_recovery_attempt(
                        step,
                        &failure,
                        RecoveryOutcome::Failed,
                        escalated,
                        effective_action,
                    );
                    return Err(anyhow!(failure.detail));
                }
                if trace.controller_state.try_consume_replan() {
                    trace.record_recovery_attempt(
                        step,
                        &failure,
                        RecoveryOutcome::Continued,
                        escalated,
                        effective_action,
                    );
                    return Ok(LoopControl::Continue(Some(failure_to_observation(
                        step, &failure,
                    ))));
                }
                trace.record_recovery_attempt(
                    step,
                    &failure,
                    RecoveryOutcome::Failed,
                    escalated,
                    effective_action,
                );
                let exhausted = FailureDecision {
                    kind: StepFailureKind::BudgetExhausted,
                    action: FailureAction::AskUser,
                    replan_scope: None,
                    detail: format!(
                        "replan budget exhausted: {}/{}",
                        trace.controller_state.replan_count, trace.controller_state.max_replans
                    ),
                    source: "controller:replan_budget".to_string(),
                    user_message: Some(
                        "我已经多次重规划仍未收敛。请补充更明确的目标、任务编号或关键中间结果后，我再继续。"
                            .to_string(),
                    ),
                };
                trace.record_failure(step, &exhausted);
                trace.record_recovery_attempt(
                    step,
                    &exhausted,
                    RecoveryOutcome::EscalatedToAskUser,
                    false,
                    FailureAction::AskUser,
                );
                trace.controller_state.record_ask_user();
                Ok(LoopControl::Finish(
                    exhausted
                        .user_message
                        .unwrap_or_else(|| exhausted.detail.clone()),
                ))
            }
            FailureAction::AskUser => {
                trace.record_recovery_attempt(
                    step,
                    &failure,
                    RecoveryOutcome::EscalatedToAskUser,
                    escalated,
                    effective_action,
                );
                trace.controller_state.record_ask_user();
                Ok(LoopControl::Finish(
                    failure
                        .user_message
                        .unwrap_or_else(|| failure.detail.clone()),
                ))
            }
            FailureAction::Abort => {
                trace.record_recovery_attempt(
                    step,
                    &failure,
                    RecoveryOutcome::Aborted,
                    escalated,
                    effective_action,
                );
                Err(anyhow!(failure.detail))
            }
        }
    }

    fn decide(
        &self,
        user_input: &str,
        observation: Option<&AgentObservation>,
        step: usize,
        trace: &mut AgentRunTrace,
    ) -> Result<PlannedDecision> {
        let (business_context, session_state) = match load_business_context_snapshot(
            self.task_store_db_path.as_deref(),
            trace,
            self.context_compaction.memory_budget,
            self.retriever.as_ref(),
            step == 0,
        ) {
            Ok(result) => result,
            Err(err) => {
                log_agent_warn(
                    "agent_context_snapshot_failed",
                    vec![
                        ("step", json!(step)),
                        ("error_kind", json!("agent_context_snapshot_failed")),
                        ("detail", json!(err.to_string())),
                    ],
                );
                (None, SessionState::default())
            }
        };
        // 从 SessionState 投影到 trace（仅在 step 0 写入，后续 step 不重复）
        if step == 0 {
            project_session_state_to_trace(trace, &session_state);
        }
        let runtime_session_state =
            derive_runtime_session_state(trace, user_input, observation, business_context.as_ref());
        if step == 0 && !runtime_session_state.is_empty() {
            trace.record_session_state_snapshot(runtime_session_state.clone());
        }
        trace.record_final_runtime_session_state(&runtime_session_state);
        let context_pack = build_context_pack(
            trace,
            user_input,
            observation,
            Some(&runtime_session_state),
            &self.tool_registry.available_tool_descriptions(),
            business_context.as_ref(),
            self.context_compaction.session_summary_strategy,
            self.context_compaction.include_previous_observations,
        );

        // 记录 ContextPack 级可观测字段（仅在 step 0 写入）
        if step == 0 {
            trace.context_pack_present = true;
            trace.context_pack_section_count = context_pack.section_count();
            trace.context_pack_total_chars = context_pack.total_chars();
            trace.context_pack_drop_reasons = context_pack.drop_reasons();
            let budget = context_pack.budget_summary();
            log_agent_info(
                "context_pack_built",
                vec![
                    ("section_count", json!(trace.context_pack_section_count)),
                    ("total_chars", json!(trace.context_pack_total_chars)),
                    ("trimmed_sections", json!(budget.trimmed_section_count)),
                    ("dropped_sections", json!(budget.dropped_section_count)),
                ],
            );
            if budget.trimmed_section_count > 0 || budget.dropped_section_count > 0 {
                log_agent_info(
                    "context_pack_trimmed",
                    vec![
                        ("trimmed_sections", json!(budget.trimmed_section_count)),
                        ("dropped_sections", json!(budget.dropped_section_count)),
                        (
                            "drop_reasons",
                            json!(trace.context_pack_drop_reasons.clone()),
                        ),
                    ],
                );
            }
        }

        let assembled_user_prompt = render_prompt_from_context_pack(&context_pack);
        let context_sections = context_pack.snapshot();
        let context_budget_summary = context_pack.budget_summary();
        let context_summary = build_context_summary(trace, observation);

        let planner_input = PlannerInput {
            raw_user_input: user_input.to_string(),
            assembled_user_prompt,
            context_sections,
            context_budget_summary,
            context_summary,
        };

        #[cfg(test)]
        if let Some(planned) = self.scripted_decisions.borrow_mut().pop_front() {
            trace.record_decision(step, "scripted", &planned);
            return Ok(planned);
        }

        let mut llm_auth_err: Option<anyhow::Error> = None;
        if let Some(client) = &self.llm_client {
            match client.plan(self.planning_policy, &planner_input, trace) {
                Ok(planned) => {
                    log_agent_info(
                        "agent_planner_selected",
                        vec![
                            ("planner", json!("llm")),
                            ("planning_policy", json!(self.planning_policy.as_str())),
                            ("step", json!(step)),
                        ],
                    );
                    trace.record_decision(step, "llm", &planned);
                    return Ok(planned);
                }
                Err(err) => {
                    let err_text = err.to_string();
                    if step == 0 {
                        log_agent_warn(
                            "agent_planner_fallback",
                            vec![
                                ("planner", json!("llm")),
                                ("fallback_to", json!("rule")),
                                ("planning_policy", json!(self.planning_policy.as_str())),
                                ("step", json!(step)),
                                ("detail", json!(err_text.clone())),
                            ],
                        );
                    } else {
                        log_agent_warn(
                            "agent_planner_fallback",
                            vec![
                                ("planner", json!("llm")),
                                ("fallback_to", json!("observation")),
                                ("planning_policy", json!(self.planning_policy.as_str())),
                                ("step", json!(step)),
                                ("detail", json!(err_text.clone())),
                            ],
                        );
                    }
                    trace.record_llm_fallback(&err_text);
                    if is_llm_auth_error(&err_text) {
                        llm_auth_err = Some(anyhow!(err_text));
                    }
                }
            }
        } else if step == 0 {
            log_agent_info(
                "agent_planner_fallback",
                vec![
                    ("planner", json!("none")),
                    ("fallback_to", json!("rule")),
                    ("planning_policy", json!(self.planning_policy.as_str())),
                    ("step", json!(step)),
                    ("detail", json!("no_llm_env")),
                ],
            );
            trace.record_llm_fallback("no_llm_env");
        }

        // 首轮继续保留 rule fallback，保证无 LLM 场景不回退
        if step == 0 {
            log_agent_info(
                "agent_planner_selected",
                vec![
                    ("planner", json!("rule")),
                    ("planning_policy", json!(self.planning_policy.as_str())),
                    ("step", json!(step)),
                ],
            );
            match parse_user_command(user_input) {
                Ok(decision) => {
                    let planned = PlannedDecision::new(decision);
                    trace.record_decision(step, "rule", &planned);
                    return Ok(planned);
                }
                Err(parse_err) => {
                    trace.record_rule_parse_error(&parse_err.to_string());
                    if let Some(auth_err) = llm_auth_err {
                        return Err(auth_err);
                    }
                    return Err(parse_err);
                }
            }
        }

        // 非首轮：如果没有可用 planner，则先保留 observation -> final 的收口行为
        if let Some(observation) = observation {
            let planned = PlannedDecision::new(AgentDecision::Final(format!(
                "完成: {}",
                observation.content.trim()
            )));
            trace.record_decision(step, "observation", &planned);
            return Ok(planned);
        }

        let planned = PlannedDecision::new(AgentDecision::Final("没有可执行的动作".to_string()));
        trace.record_decision(step, "default", &planned);
        Ok(planned)
    }

    fn can_replan(&self) -> bool {
        if self.llm_client.is_some() {
            return true;
        }
        #[cfg(test)]
        {
            if !self.scripted_decisions.borrow().is_empty() {
                return true;
            }
        }
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentDecision {
    // 继续行动：调用一个工具
    CallTool(ToolAction),
    // 结束循环：直接返回用户可读结果
    Final(String),
}

#[cfg(test)]
mod tests;
