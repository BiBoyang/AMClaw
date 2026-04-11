use crate::tool_registry::{ToolAction, ToolRegistry};
use crate::task_store::{RecentTaskRecord, TaskStatusRecord, TaskStore, UserMemoryRecord};
use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use chrono_tz::Asia::Shanghai;
use reqwest::blocking::Client;
use reqwest::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, Instant};
use uuid::Uuid;
#[cfg(test)]
use {
    std::cell::RefCell,
    std::collections::VecDeque,
};

const DEFAULT_MAX_STEPS: usize = 8;
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
}

#[allow(dead_code)]
pub type RunContext = AgentRunContext;

#[derive(Debug, Clone)]
struct AgentObservation {
    step: usize,
    source: String,
    content: String,
}

impl AgentObservation {
    fn tool_result(step: usize, tool_name: &str, output: &str) -> Self {
        Self {
            step,
            source: format!("tool:{tool_name}"),
            content: output.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
struct PlannerInput {
    raw_user_input: String,
    assembled_user_prompt: String,
    context_summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExecutionPlan {
    steps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PlanStepStatus {
    Pending,
    Running,
    Done,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum StepFailureKind {
    Expectation,
    RepeatedAction,
    Semantic,
    Irrecoverable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum FailureAction {
    Replan,
    Abort,
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
struct RuntimePlanStep {
    description: String,
    status: PlanStepStatus,
    expected_observation: Option<ExpectedObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedDecision {
    decision: AgentDecision,
    plan: Option<ExecutionPlan>,
    progress_note: Option<String>,
    expected_observation: Option<ExpectedObservation>,
}

#[derive(Debug, Clone)]
struct FailureDecision {
    kind: StepFailureKind,
    action: FailureAction,
    detail: String,
    source: String,
}

impl PlannedDecision {
    fn new(decision: AgentDecision) -> Self {
        Self {
            decision,
            plan: None,
            progress_note: None,
            expected_observation: None,
        }
    }

    fn with_plan(mut self, plan: Option<ExecutionPlan>) -> Self {
        self.plan = plan;
        self
    }

    fn with_progress_note(mut self, progress_note: Option<String>) -> Self {
        self.progress_note = progress_note;
        self
    }

    fn with_expected_observation(
        mut self,
        expected_observation: Option<ExpectedObservation>,
    ) -> Self {
        self.expected_observation = expected_observation;
        self
    }

    fn summary(&self) -> String {
        self.decision.summary()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ObservationKind {
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
struct ExpectedObservation {
    kind: ObservationKind,
    done_rule: DoneRule,
}

impl ExpectedObservation {
    fn summary(&self) -> String {
        match &self.done_rule {
            DoneRule::ToolSuccess => format!("kind={:?}, done=tool_success", self.kind),
            DoneRule::NonEmptyOutput => format!("kind={:?}, done=non_empty_output", self.kind),
            DoneRule::RequiresJsonField { field } => {
                format!("kind={:?}, done=json_field:{field}", self.kind)
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
struct BusinessContextSnapshot {
    current_task: Option<TaskStatusRecord>,
    recent_tasks: Vec<RecentTaskRecord>,
    user_memories: Vec<UserMemoryRecord>,
}

#[derive(Debug, Clone, Copy)]
enum PlanningPolicy {
    Reactive,
}

impl PlanningPolicy {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Reactive => "reactive",
        }
    }
}

#[derive(Debug, Default)]
struct ContextAssembler;

impl ContextAssembler {
    fn assemble(
        &self,
        trace: &AgentRunTrace,
        user_input: &str,
        observation: Option<&AgentObservation>,
        available_tools: &[String],
        business_context: Option<&BusinessContextSnapshot>,
    ) -> PlannerInput {
        let mut sections = vec![
            "你正在处理一次 AMClaw agent 运行。请基于下面上下文决定下一步。".to_string(),
            String::new(),
            "## User Input".to_string(),
            user_input.trim().to_string(),
            String::new(),
            "## Runtime Context".to_string(),
            format!("- source_type: {}", trace.source_type),
            format!(
                "- trigger_type: {}",
                trace.trigger_type.as_deref().unwrap_or("(none)")
            ),
            format!("- user_id: {}", trace.user_id.as_deref().unwrap_or("(none)")),
            format!(
                "- task_id: {}",
                trace.task_id.as_deref().unwrap_or("(none)")
            ),
            format!(
                "- article_id: {}",
                trace.article_id.as_deref().unwrap_or("(none)")
            ),
            format!("- message_count: {}", trace.message_count),
            format!("- context_token_present: {}", trace.context_token_present),
        ];

        if !trace.message_ids.is_empty() {
            sections.push("- message_ids:".to_string());
            for message_id in &trace.message_ids {
                sections.push(format!("  - {}", message_id));
            }
        }

        if let Some(session_text) = &trace.session_text {
            sections.push(String::new());
            sections.push("## Session Text".to_string());
            sections.push(summarize_for_markdown(session_text, 600));
        }

        if let Some(observation) = observation {
            sections.push(String::new());
            sections.push("## Latest Observation".to_string());
            sections.push(format!("- step: {}", observation.step));
            sections.push(format!("- source: {}", observation.source));
            sections.push("```text".to_string());
            sections.push(summarize_for_markdown(&observation.content, 800));
            sections.push("```".to_string());
        }

        if !trace.active_plan_steps.is_empty() {
            sections.push(String::new());
            sections.push("## Active Plan".to_string());
            for (idx, step) in trace.active_plan_steps.iter().enumerate() {
                let mut line = format!("{}. [{}] {}", idx + 1, step.status.as_str(), step.description);
                if let Some(expected) = &step.expected_observation {
                    line.push_str(&format!(" | expect: {}", expected.summary()));
                }
                sections.push(line);
            }
            if let Some(progress_note) = &trace.last_progress_note {
                sections.push(format!("- progress_note: {}", progress_note));
            }
        }

        if let Some(business_context) = business_context {
            if let Some(task) = &business_context.current_task {
                sections.push(String::new());
                sections.push("## Current Task".to_string());
                sections.push(format!("- task_id: {}", task.task_id));
                sections.push(format!("- status: {}", task.status));
                sections.push(format!("- article_id: {}", task.article_id));
                sections.push(format!("- url: {}", task.normalized_url));
                sections.push(format!("- retry_count: {}", task.retry_count));
                if let Some(page_kind) = &task.page_kind {
                    sections.push(format!("- page_kind: {}", page_kind));
                }
                if let Some(content_source) = &task.content_source {
                    sections.push(format!("- content_source: {}", content_source));
                }
                if let Some(last_error) = &task.last_error {
                    sections.push(format!(
                        "- last_error: {}",
                        summarize_for_markdown(last_error, 200)
                    ));
                }
            }

            if !business_context.recent_tasks.is_empty() {
                sections.push(String::new());
                sections.push("## Recent Tasks".to_string());
                for task in &business_context.recent_tasks {
                    sections.push(format!(
                        "- task_id={} status={} page_kind={} url={}",
                        task.task_id,
                        task.status,
                        task.page_kind.as_deref().unwrap_or("(none)"),
                        task.normalized_url
                    ));
                }
            }

            if !business_context.user_memories.is_empty() {
                sections.push(String::new());
                sections.push("## User Memories".to_string());
                for memory in &business_context.user_memories {
                    sections.push(format!("- {}", memory.content));
                }
            }
        }

        sections.push(String::new());
        sections.push("## Available Tools".to_string());
        sections.extend(available_tools.iter().cloned());

        sections.push(String::new());
        sections.push(
            "你必须采用最小 ReAct 风格：根据当前上下文决定“继续调一个工具”或“直接结束”。请只输出 JSON，格式为 {\"action\":\"read|write|create|get_task_status|list_recent_tasks|list_manual_tasks|read_article_archive|final\",...}。".to_string(),
        );

        let assembled_user_prompt = sections.join("\n");
        let context_summary = build_context_summary(trace, observation);

        PlannerInput {
            raw_user_input: user_input.to_string(),
            assembled_user_prompt,
            context_summary,
        }
    }
}

#[derive(Debug)]
pub struct AgentCore {
    workspace_root: PathBuf,
    // 负责实际执行工具动作（读写文件等）
    tool_registry: ToolRegistry,
    task_store_db_path: Option<PathBuf>,
    llm_client: Option<LlmClient>,
    // 防止 Agent 无穷循环的安全阀
    max_steps: usize,
    planning_policy: PlanningPolicy,
    #[cfg(test)]
    scripted_decisions: RefCell<VecDeque<PlannedDecision>>,
}

impl AgentCore {
    #[allow(dead_code)]
    pub fn new(workspace_root: impl Into<PathBuf>) -> Result<Self> {
        Self::with_max_steps_and_task_store_db_path(
            workspace_root,
            DEFAULT_MAX_STEPS,
            None::<PathBuf>,
        )
    }

    #[allow(dead_code)]
    pub fn with_max_steps(workspace_root: impl Into<PathBuf>, max_steps: usize) -> Result<Self> {
        Self::with_max_steps_and_task_store_db_path(workspace_root, max_steps, None::<PathBuf>)
    }

    pub fn with_task_store_db_path(
        workspace_root: impl Into<PathBuf>,
        task_store_db_path: impl Into<PathBuf>,
    ) -> Result<Self> {
        Self::with_max_steps_and_task_store_db_path(
            workspace_root,
            DEFAULT_MAX_STEPS,
            Some(task_store_db_path),
        )
    }

    fn with_max_steps_and_task_store_db_path(
        workspace_root: impl Into<PathBuf>,
        max_steps: usize,
        task_store_db_path: Option<impl Into<PathBuf>>,
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
        Ok(Self {
            workspace_root: workspace_root.clone(),
            tool_registry,
            task_store_db_path,
            llm_client: LlmClient::from_env()?,
            max_steps,
            planning_policy: PlanningPolicy::Reactive,
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
        let agent =
            Self::with_max_steps_and_task_store_db_path(workspace_root, max_steps, None::<PathBuf>)?;
        agent
            .scripted_decisions
            .borrow_mut()
            .extend(decisions.into_iter().map(PlannedDecision::new));
        Ok(agent)
    }

    pub fn run(&self, user_input: &str) -> Result<String> {
        self.run_with_context(user_input, AgentRunContext::agent_demo())
    }

    pub fn run_with_context(&self, user_input: &str, context: AgentRunContext) -> Result<String> {
        let started = Instant::now();
        let mut trace = AgentRunTrace::new(&self.workspace_root, user_input, context);
        let mut last_observation: Option<AgentObservation> = None;
        let result = (|| -> Result<String> {
            // 最小 Agent Loop: 决策 -> 执行工具 -> 继续决策/结束
            for step in 0..self.max_steps {
                trace.step_count = step + 1;
                let planned = self.decide(user_input, last_observation.as_ref(), step, &mut trace)?;
                if let Some(failure) =
                    detect_repeated_action_failure(step, &planned, &trace, last_observation.as_ref())
                {
                    trace.record_failure(step, &failure);
                    trace.mark_next_plan_step_running(planned.expected_observation.clone());
                    trace.mark_running_plan_step_failed();
                    if self.can_replan() {
                        last_observation = Some(failure_to_observation(step, &failure));
                        continue;
                    }
                    return Err(anyhow!(failure.detail));
                }
                match planned.decision {
                    AgentDecision::CallTool(action) => {
                        trace.mark_next_plan_step_running(planned.expected_observation.clone());
                        let action_source = resulting_source_name(&action);
                        let tool_trace = trace.start_tool_call(step, &action);
                        match self.tool_registry.execute(action) {
                            Ok(result) => {
                                let observation =
                                    AgentObservation::tool_result(step, result.tool, &result.output);
                                if let Err(err) = validate_expected_observation(
                                    trace.running_plan_expected_observation(),
                                    &observation,
                                ) {
                                    let failure = FailureDecision {
                                        kind: StepFailureKind::Expectation,
                                        action: FailureAction::Replan,
                                        detail: err.to_string(),
                                        source: observation.source.clone(),
                                    };
                                    trace.finish_tool_call_error(
                                        tool_trace,
                                        &format!("expected_observation_failed: {err}"),
                                    );
                                    trace.record_observation(&observation);
                                    trace.record_failure(step, &failure);
                                    if self.can_replan() {
                                        last_observation =
                                            Some(failure_to_observation(step, &failure));
                                        continue;
                                    }
                                    return Err(anyhow!(failure.detail));
                                }
                                trace.finish_tool_call_success(
                                    tool_trace,
                                    result.tool,
                                    &result.output,
                                );
                                trace.record_observation(&observation);
                                last_observation = Some(observation);
                            }
                            Err(err) => {
                                trace.finish_tool_call_error(tool_trace, &err.to_string());
                                let failure = classify_tool_execution_failure(action_source, &err.to_string());
                                trace.record_failure(step, &failure);
                                match failure.action {
                                    FailureAction::Replan => {
                                        if self.can_replan() {
                                            last_observation =
                                                Some(failure_to_observation(step, &failure));
                                            continue;
                                        }
                                        return Err(anyhow!(failure.detail));
                                    }
                                    FailureAction::Abort => return Err(err),
                                }
                            }
                        }
                    }
                    AgentDecision::Final(answer) => return Ok(answer),
                }
            }
            bail!("达到最大步骤，未能收敛")
        })();

        match &result {
            Ok(answer) => trace.finish_success(answer, started.elapsed()),
            Err(err) => trace.finish_error(&err.to_string(), started.elapsed()),
        }

        if let Err(err) = trace.persist() {
            log_agent_warn(
                "agent_trace_persist_failed",
                vec![
                    ("error_kind", json!("agent_trace_persist_failed")),
                    ("detail", json!(err.to_string())),
                ],
            );
        }

        result
    }

    fn decide(
        &self,
        user_input: &str,
        observation: Option<&AgentObservation>,
        step: usize,
        trace: &mut AgentRunTrace,
    ) -> Result<PlannedDecision> {
        let business_context = match load_business_context_snapshot(self.task_store_db_path.as_deref(), trace) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                log_agent_warn(
                    "agent_context_snapshot_failed",
                    vec![
                        ("step", json!(step)),
                        ("error_kind", json!("agent_context_snapshot_failed")),
                        ("detail", json!(err.to_string())),
                    ],
                );
                None
            }
        };
        let planner_input = ContextAssembler.assemble(
            trace,
            user_input,
            observation,
            &self.tool_registry.available_tool_descriptions(),
            business_context.as_ref(),
        );

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
enum AgentDecision {
    // 继续行动：调用一个工具
    CallTool(ToolAction),
    // 结束循环：直接返回用户可读结果
    Final(String),
}

#[derive(Debug, Clone)]
struct LlmClient {
    http: Client,
    configs: Vec<LlmConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LlmConfig {
    source: &'static str,
    api_key: String,
    model: String,
    base_url: String,
}

impl LlmClient {
    fn from_env() -> Result<Option<Self>> {
        let mut configs = Vec::new();
        for provider in LLM_PROVIDER_PRIORITY {
            let loaded = match provider {
                "DEEPSEEK" => load_llm_config(
                    "DEEPSEEK",
                    "DEEPSEEK_API_KEY",
                    "DEEPSEEK_MODEL",
                    "DEEPSEEK_BASE_URL",
                ),
                "MOONSHOT" => load_llm_config(
                    "MOONSHOT",
                    "MOONSHOT_API_KEY",
                    "MOONSHOT_MODEL",
                    "MOONSHOT_BASE_URL",
                ),
                "OPENAI" => load_llm_config(
                    "OPENAI",
                    "OPENAI_API_KEY",
                    "OPENAI_MODEL",
                    "OPENAI_BASE_URL",
                ),
                _ => None,
            };
            if let Some(config) = loaded {
                if !configs.iter().any(|v| v == &config) {
                    configs.push(config);
                }
            }
        }
        if configs.is_empty() {
            return Ok(None);
        }
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .context("创建 LLM HTTP 客户端失败")?;
        for config in &configs {
            log_agent_info(
                "agent_llm_config_enabled",
                vec![
                    ("source", json!(config.source)),
                    ("model", json!(config.model)),
                    ("base_url", json!(config.base_url)),
                    ("key_tail", json!(key_tail(&config.api_key))),
                ],
            );
        }
        if configs.len() > 1 {
            let order = configs
                .iter()
                .map(|v| v.source)
                .collect::<Vec<_>>()
                .join("->");
            log_agent_info(
                "agent_llm_multi_provider_enabled",
                vec![("order", json!(order))],
            );
        }
        Ok(Some(Self { http, configs }))
    }

    fn plan(
        &self,
        planning_policy: PlanningPolicy,
        planner_input: &PlannerInput,
        trace: &mut AgentRunTrace,
    ) -> Result<PlannedDecision> {
        let mut last_auth_err: Option<anyhow::Error> = None;
        for (idx, config) in self.configs.iter().enumerate() {
            match self.plan_with_config(config, planning_policy, planner_input, trace) {
                Ok(decision) => {
                    if idx > 0 {
                        log_agent_info(
                            "agent_llm_fallback_success",
                            vec![
                                ("source", json!(config.source)),
                                ("model", json!(config.model)),
                                ("base_url", json!(config.base_url)),
                            ],
                        );
                    }
                    return Ok(decision);
                }
                Err(err) => {
                    let err_text = err.to_string();
                    if is_llm_auth_error(&err_text) && idx + 1 < self.configs.len() {
                        log_agent_warn(
                            "agent_llm_auth_failed",
                            vec![
                                ("source", json!(config.source)),
                                ("model", json!(config.model)),
                                ("base_url", json!(config.base_url)),
                                ("key_tail", json!(key_tail(&config.api_key))),
                                ("detail", json!("fallback_next")),
                            ],
                        );
                        last_auth_err = Some(err);
                        continue;
                    }
                    return Err(err);
                }
            }
        }
        if let Some(err) = last_auth_err {
            return Err(err);
        }
        bail!("LLM 配置为空");
    }

    fn plan_with_config(
        &self,
        config: &LlmConfig,
        planning_policy: PlanningPolicy,
        planner_input: &PlannerInput,
        trace: &mut AgentRunTrace,
    ) -> Result<PlannedDecision> {
        let system_prompt = build_system_prompt(planning_policy);
        let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
        let mut body = json!({
            "model": config.model,
            "messages": [
                {
                    "role": "system",
                    "content": system_prompt
                },
                {
                    "role": "user",
                    "content": planner_input.assembled_user_prompt
                }
            ]
        });
        if config.source == "MOONSHOT" {
            body["temperature"] = json!(1);
        } else {
            body["temperature"] = json!(0.0);
        }
        let llm_call = trace.start_llm_call(
            config,
            system_prompt,
            &planner_input.raw_user_input,
            &planner_input.assembled_user_prompt,
            &planner_input.context_summary,
            &body,
        );
        let mut last_status = 0u16;
        let mut last_text = String::new();
        let mut payload_text: Option<String> = None;
        let mut attempts = 0usize;
        for attempt in 1..=2 {
            attempts = attempt;
            let response = self
                .http
                .post(&url)
                .header(CONTENT_TYPE, "application/json")
                .header("Authorization", format!("Bearer {}", config.api_key))
                .json(&body)
                .send()
                .map_err(|err| {
                    trace.finish_llm_call_error(
                        llm_call,
                        attempts,
                        None,
                        &format!("请求 LLM 失败: {err}"),
                    );
                    anyhow!("请求 LLM 失败: {err}")
                })?;
            let status = response.status();
            let text = response.text().map_err(|err| {
                trace.finish_llm_call_error(
                    llm_call,
                    attempts,
                    Some(status.as_u16()),
                    &format!("读取 LLM 响应失败: {err}"),
                );
                anyhow!("读取 LLM 响应失败: {err}")
            })?;
            last_status = status.as_u16();
            last_text = text.clone();
            if status.is_success() {
                payload_text = Some(text);
                break;
            }
            let retryable = status.as_u16() == 401 && text.contains("governor");
            if retryable && attempt == 1 {
                log_agent_warn(
                    "agent_llm_retry",
                    vec![
                        ("reason", json!("governor")),
                        ("attempt", json!(attempt)),
                        ("source", json!(config.source)),
                    ],
                );
                sleep(Duration::from_millis(250));
                continue;
            }
            let error = format!(
                "LLM 请求失败(source={} model={} base_url={} key_tail={}): HTTP {} {}",
                config.source,
                config.model,
                config.base_url,
                key_tail(&config.api_key),
                status.as_u16(),
                text
            );
            trace.finish_llm_call_error(llm_call, attempts, Some(status.as_u16()), &error);
            bail!(error);
        }
        let text = payload_text.ok_or_else(|| {
            let error = format!(
                "LLM 请求失败(source={} model={} base_url={} key_tail={}): HTTP {} {}",
                config.source,
                config.model,
                config.base_url,
                key_tail(&config.api_key),
                last_status,
                last_text
            );
            trace.finish_llm_call_error(llm_call, attempts, Some(last_status), &error);
            anyhow!(error)
        })?;
        let payload: OpenAiCompatResponse = serde_json::from_str(&text).map_err(|err| {
            trace.finish_llm_call_error(
                llm_call,
                attempts,
                Some(last_status),
                &format!("解析 LLM 响应 JSON 失败: {err}"),
            );
            anyhow!("解析 LLM 响应 JSON 失败: {err}")
        })?;
        let content = payload
            .choices
            .first()
            .map(|v| v.message.content.trim().to_string())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                trace.finish_llm_call_error(
                    llm_call,
                    attempts,
                    Some(last_status),
                    "LLM 返回内容为空",
                );
                anyhow!("LLM 返回内容为空")
            })?;
        let planned = parse_llm_plan(&content).map_err(|err| {
            trace.finish_llm_call_error(
                llm_call,
                attempts,
                Some(last_status),
                &format!("解析 LLM 计划失败: {err}"),
            );
            err
        })?;
        trace.finish_llm_call_success(llm_call, attempts, &text, &content, last_status, &planned);
        Ok(planned)
    }
}

#[derive(Debug, Serialize)]
struct AgentRunTrace {
    trace_version: &'static str,
    run_id: String,
    started_at: String,
    finished_at: Option<String>,
    duration_ms: Option<u128>,
    success: bool,
    error: Option<String>,
    final_output: Option<String>,
    user_input: String,
    user_input_chars: usize,
    source_type: String,
    trigger_type: Option<String>,
    user_id: Option<String>,
    message_ids: Vec<String>,
    message_count: usize,
    task_id: Option<String>,
    article_id: Option<String>,
    session_text: Option<String>,
    session_text_chars: usize,
    context_token_present: bool,
    step_count: usize,
    workspace_root: String,
    llm_fallback_reason: Option<String>,
    rule_parse_error: Option<String>,
    decisions: Vec<DecisionTrace>,
    observations: Vec<ObservationTrace>,
    failures: Vec<FailureTrace>,
    active_plan_steps: Vec<RuntimePlanStep>,
    last_progress_note: Option<String>,
    llm_calls: Vec<LlmCallTrace>,
    tool_calls: Vec<ToolCallTrace>,
    #[serde(skip_serializing)]
    trace_dir_root: PathBuf,
}

#[derive(Debug, Serialize)]
struct DecisionTrace {
    step: usize,
    source: String,
    decision_type: String,
    summary: String,
    plan_steps: Vec<String>,
    progress_note: Option<String>,
    expected_observation: Option<String>,
}

#[derive(Debug, Serialize)]
struct PromptSnapshot {
    system_prompt: String,
    raw_user_input: String,
    user_prompt: String,
    context_summary: String,
    request_body: String,
    system_prompt_chars: usize,
    raw_user_input_chars: usize,
    user_prompt_chars: usize,
    context_summary_chars: usize,
    request_body_chars: usize,
    estimated_prompt_chars: usize,
}

#[derive(Debug, Serialize)]
struct LlmCallTrace {
    source: String,
    model: String,
    base_url: String,
    prompt: PromptSnapshot,
    raw_response: Option<String>,
    raw_response_chars: Option<usize>,
    message_content: Option<String>,
    message_content_chars: Option<usize>,
    response_status: Option<u16>,
    attempts: usize,
    success: bool,
    error: Option<String>,
    decision_summary: Option<String>,
}

#[derive(Debug, Serialize)]
struct ToolCallTrace {
    step: usize,
    tool_name: String,
    path: Option<String>,
    content_chars: Option<usize>,
    output: Option<String>,
    output_chars: Option<usize>,
    success: bool,
    error: Option<String>,
    duration_ms: Option<u128>,
    #[serde(skip_serializing)]
    started_at: Option<Instant>,
}

#[derive(Debug, Serialize)]
struct ObservationTrace {
    step: usize,
    source: String,
    summary: String,
    content_chars: usize,
}

#[derive(Debug, Serialize)]
struct FailureTrace {
    step: usize,
    kind: StepFailureKind,
    action: FailureAction,
    source: String,
    detail: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct AgentTraceIndexEntry {
    trace_version: String,
    run_id: String,
    started_at: String,
    finished_at: Option<String>,
    duration_ms: Option<u128>,
    success: bool,
    user_input: String,
    user_input_chars: usize,
    source_type: String,
    trigger_type: Option<String>,
    user_id: Option<String>,
    message_ids: Vec<String>,
    message_count: usize,
    task_id: Option<String>,
    article_id: Option<String>,
    session_text_chars: usize,
    context_token_present: bool,
    step_count: usize,
    llm_call_count: usize,
    tool_call_count: usize,
    observation_count: usize,
    final_output_chars: Option<usize>,
    error: Option<String>,
    llm_fallback_reason: Option<String>,
    json_file: String,
    markdown_file: String,
}

impl AgentRunTrace {
    fn new(workspace_root: &std::path::Path, user_input: &str, context: AgentRunContext) -> Self {
        let message_count = context.message_ids.len();
        let session_text_chars = context
            .session_text
            .as_deref()
            .map(|value| value.chars().count())
            .unwrap_or(0);
        Self {
            trace_version: "agent_trace_v1",
            run_id: Uuid::new_v4().to_string(),
            started_at: Utc::now().to_rfc3339(),
            finished_at: None,
            duration_ms: None,
            success: false,
            error: None,
            final_output: None,
            user_input: user_input.to_string(),
            user_input_chars: user_input.chars().count(),
            source_type: context.source_type,
            trigger_type: context.trigger_type,
            user_id: context.user_id,
            message_ids: context.message_ids,
            message_count,
            task_id: context.task_id,
            article_id: context.article_id,
            session_text: context.session_text,
            session_text_chars,
            context_token_present: context.context_token_present,
            step_count: 0,
            workspace_root: workspace_root.display().to_string(),
            llm_fallback_reason: None,
            rule_parse_error: None,
            decisions: Vec::new(),
            observations: Vec::new(),
            failures: Vec::new(),
            active_plan_steps: Vec::new(),
            last_progress_note: None,
            llm_calls: Vec::new(),
            tool_calls: Vec::new(),
            trace_dir_root: workspace_root.join("data").join("agent_traces"),
        }
    }

    fn record_decision(&mut self, step: usize, source: &str, planned: &PlannedDecision) {
        if let Some(plan) = &planned.plan {
            self.active_plan_steps = plan
                .steps
                .iter()
                .map(|description| RuntimePlanStep {
                    description: description.clone(),
                    status: PlanStepStatus::Pending,
                    expected_observation: None,
                })
                .collect();
        }
        if let Some(progress_note) = &planned.progress_note {
            self.last_progress_note = Some(progress_note.clone());
        }
        self.decisions.push(DecisionTrace {
            step,
            source: source.to_string(),
            decision_type: planned.decision.kind().to_string(),
            summary: planned.summary(),
            plan_steps: planned
                .plan
                .as_ref()
                .map(|plan| plan.steps.clone())
                .unwrap_or_default(),
            progress_note: planned.progress_note.clone(),
            expected_observation: planned
                .expected_observation
                .as_ref()
                .map(ExpectedObservation::summary),
        });
    }

    fn mark_next_plan_step_running(&mut self, expected_observation: Option<ExpectedObservation>) {
        if let Some(step) = self
            .active_plan_steps
            .iter_mut()
            .find(|step| step.status == PlanStepStatus::Pending)
        {
            step.status = PlanStepStatus::Running;
            step.expected_observation = expected_observation;
        }
    }

    fn mark_running_plan_step_done(&mut self) {
        if let Some(step) = self
            .active_plan_steps
            .iter_mut()
            .find(|step| step.status == PlanStepStatus::Running)
        {
            step.status = PlanStepStatus::Done;
        }
    }

    fn mark_running_plan_step_failed(&mut self) {
        if let Some(step) = self
            .active_plan_steps
            .iter_mut()
            .find(|step| step.status == PlanStepStatus::Running)
        {
            step.status = PlanStepStatus::Failed;
        }
    }

    fn mark_remaining_plan_steps_skipped(&mut self) {
        for step in &mut self.active_plan_steps {
            if step.status == PlanStepStatus::Pending {
                step.status = PlanStepStatus::Skipped;
            }
        }
    }

    fn running_plan_expected_observation(&self) -> Option<&ExpectedObservation> {
        self.active_plan_steps
            .iter()
            .find(|step| step.status == PlanStepStatus::Running)
            .and_then(|step| step.expected_observation.as_ref())
    }

    fn record_llm_fallback(&mut self, reason: &str) {
        self.llm_fallback_reason = Some(reason.to_string());
    }

    fn record_rule_parse_error(&mut self, error: &str) {
        self.rule_parse_error = Some(error.to_string());
    }

    fn record_observation(&mut self, observation: &AgentObservation) {
        self.observations.push(ObservationTrace {
            step: observation.step,
            source: observation.source.clone(),
            summary: truncate_for_trace(&observation.content, 240),
            content_chars: observation.content.chars().count(),
        });
    }

    fn record_failure(&mut self, step: usize, failure: &FailureDecision) {
        self.failures.push(FailureTrace {
            step,
            kind: failure.kind.clone(),
            action: failure.action.clone(),
            source: failure.source.clone(),
            detail: failure.detail.clone(),
        });
    }

    fn start_llm_call(
        &mut self,
        config: &LlmConfig,
        system_prompt: &str,
        raw_user_input: &str,
        user_prompt: &str,
        context_summary: &str,
        body: &serde_json::Value,
    ) -> usize {
        let request_body = serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string());
        self.llm_calls.push(LlmCallTrace {
            source: config.source.to_string(),
            model: config.model.clone(),
            base_url: config.base_url.clone(),
            prompt: PromptSnapshot {
                system_prompt: system_prompt.to_string(),
                raw_user_input: raw_user_input.to_string(),
                user_prompt: user_prompt.to_string(),
                context_summary: context_summary.to_string(),
                system_prompt_chars: system_prompt.chars().count(),
                raw_user_input_chars: raw_user_input.chars().count(),
                user_prompt_chars: user_prompt.chars().count(),
                context_summary_chars: context_summary.chars().count(),
                request_body_chars: request_body.chars().count(),
                estimated_prompt_chars: system_prompt.chars().count()
                    + user_prompt.chars().count()
                    + request_body.chars().count(),
                request_body,
            },
            raw_response: None,
            raw_response_chars: None,
            message_content: None,
            message_content_chars: None,
            response_status: None,
            attempts: 0,
            success: false,
            error: None,
            decision_summary: None,
        });
        self.llm_calls.len() - 1
    }

    fn finish_llm_call_success(
        &mut self,
        index: usize,
        attempts: usize,
        raw_response: &str,
        message_content: &str,
        status: u16,
        planned: &PlannedDecision,
    ) {
        if let Some(call) = self.llm_calls.get_mut(index) {
            call.raw_response = Some(raw_response.to_string());
            call.raw_response_chars = Some(raw_response.chars().count());
            call.message_content = Some(message_content.to_string());
            call.message_content_chars = Some(message_content.chars().count());
            call.response_status = Some(status);
            call.attempts = attempts;
            call.success = true;
            call.decision_summary = Some(planned.summary());
        }
    }

    fn finish_llm_call_error(
        &mut self,
        index: usize,
        attempts: usize,
        status: Option<u16>,
        error: &str,
    ) {
        if let Some(call) = self.llm_calls.get_mut(index) {
            call.attempts = attempts;
            call.response_status = status;
            call.error = Some(error.to_string());
        }
    }

    fn start_tool_call(&mut self, step: usize, action: &ToolAction) -> usize {
        self.tool_calls.push(ToolCallTrace {
            step,
            tool_name: action.name().to_string(),
            path: action.path().map(ToOwned::to_owned),
            content_chars: action.content().map(|v| v.chars().count()),
            output: None,
            output_chars: None,
            success: false,
            error: None,
            duration_ms: None,
            started_at: Some(Instant::now()),
        });
        self.tool_calls.len() - 1
    }

    fn finish_tool_call_success(&mut self, index: usize, tool_name: &str, output: &str) {
        if let Some(call) = self.tool_calls.get_mut(index) {
            call.tool_name = tool_name.to_string();
            call.output = Some(output.to_string());
            call.output_chars = Some(output.chars().count());
            call.success = true;
            call.duration_ms = call.started_at.map(|v| v.elapsed().as_millis());
            call.started_at = None;
        }
        self.mark_running_plan_step_done();
    }

    fn finish_tool_call_error(&mut self, index: usize, error: &str) {
        if let Some(call) = self.tool_calls.get_mut(index) {
            call.error = Some(error.to_string());
            call.duration_ms = call.started_at.map(|v| v.elapsed().as_millis());
            call.started_at = None;
        }
        self.mark_running_plan_step_failed();
    }

    fn finish_success(&mut self, output: &str, duration: Duration) {
        self.success = true;
        self.final_output = Some(output.to_string());
        self.finished_at = Some(Utc::now().to_rfc3339());
        self.duration_ms = Some(duration.as_millis());
        self.mark_remaining_plan_steps_skipped();
    }

    fn finish_error(&mut self, error: &str, duration: Duration) {
        self.success = false;
        self.error = Some(error.to_string());
        self.finished_at = Some(Utc::now().to_rfc3339());
        self.duration_ms = Some(duration.as_millis());
        for call in &mut self.llm_calls {
            if !call.success && call.error.is_none() {
                call.error = Some(error.to_string());
            }
        }
    }

    fn persist(&self) -> Result<()> {
        let day = Utc::now()
            .with_timezone(&Shanghai)
            .format("%Y-%m-%d")
            .to_string();
        let dir = self.trace_dir_root.join(&day);
        fs::create_dir_all(&dir)
            .with_context(|| format!("创建 agent trace 目录失败: {}", dir.display()))?;
        let timestamp = Utc::now()
            .with_timezone(&Shanghai)
            .format("%Y%m%dT%H%M%S")
            .to_string();
        let json_path = dir.join(format!("run_{}_{}.json", timestamp, self.run_id));
        let json_content = serde_json::to_string_pretty(self).context("序列化 agent trace 失败")?;
        fs::write(&json_path, format!("{json_content}\n"))
            .with_context(|| format!("写入 agent trace 失败: {}", json_path.display()))?;

        let markdown_path = dir.join(format!("run_{}_{}.md", timestamp, self.run_id));
        let markdown_content = self.to_markdown();
        fs::write(&markdown_path, markdown_content).with_context(|| {
            format!(
                "写入 agent trace markdown 失败: {}",
                markdown_path.display()
            )
        })?;

        let index_entry = AgentTraceIndexEntry {
            trace_version: self.trace_version.to_string(),
            run_id: self.run_id.clone(),
            started_at: self.started_at.clone(),
            finished_at: self.finished_at.clone(),
            duration_ms: self.duration_ms,
            success: self.success,
            user_input: summarize_for_markdown(&self.user_input, 240),
            user_input_chars: self.user_input_chars,
            source_type: self.source_type.clone(),
            trigger_type: self.trigger_type.clone(),
            user_id: self.user_id.clone(),
            message_ids: self.message_ids.clone(),
            message_count: self.message_count,
            task_id: self.task_id.clone(),
            article_id: self.article_id.clone(),
            session_text_chars: self.session_text_chars,
            context_token_present: self.context_token_present,
            step_count: self.step_count,
            llm_call_count: self.llm_calls.len(),
            tool_call_count: self.tool_calls.len(),
            observation_count: self.observations.len(),
            final_output_chars: self.final_output.as_ref().map(|v| v.chars().count()),
            error: self.error.clone().map(|v| summarize_for_markdown(&v, 240)),
            llm_fallback_reason: self
                .llm_fallback_reason
                .clone()
                .map(|v| summarize_for_markdown(&v, 240)),
            json_file: json_path
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or_default()
                .to_string(),
            markdown_file: markdown_path
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or_default()
                .to_string(),
        };
        let index_path = dir.join("index.jsonl");
        let index_line =
            serde_json::to_string(&index_entry).context("序列化 agent trace index 失败")?;
        let mut existing = if index_path.exists() {
            fs::read_to_string(&index_path)
                .with_context(|| format!("读取 agent trace index 失败: {}", index_path.display()))?
        } else {
            String::new()
        };
        existing.push_str(&index_line);
        existing.push('\n');
        fs::write(&index_path, existing)
            .with_context(|| format!("写入 agent trace index 失败: {}", index_path.display()))?;
        self.write_daily_index_markdown(&dir, &day)?;
        Ok(())
    }

    fn write_daily_index_markdown(&self, dir: &std::path::Path, day: &str) -> Result<()> {
        let index_path = dir.join("index.jsonl");
        let index_content = fs::read_to_string(&index_path)
            .with_context(|| format!("读取 agent trace index 失败: {}", index_path.display()))?;
        let mut entries = Vec::new();
        for (idx, line) in index_content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let entry: AgentTraceIndexEntry = serde_json::from_str(line)
                .with_context(|| format!("解析 agent trace index 第 {} 行失败", idx + 1))?;
            entries.push(entry);
        }

        let markdown_path = dir.join("index.md");
        let markdown = render_daily_index_markdown(day, &entries);
        fs::write(&markdown_path, markdown).with_context(|| {
            format!(
                "写入 agent trace index markdown 失败: {}",
                markdown_path.display()
            )
        })?;
        Ok(())
    }

    fn to_markdown(&self) -> String {
        let mut lines = vec![
            format!("# Agent Trace {}", self.run_id),
            String::new(),
            "## Summary".to_string(),
            String::new(),
            format!("- success: {}", self.success),
            format!("- started_at: {}", self.started_at),
            format!(
                "- finished_at: {}",
                self.finished_at.as_deref().unwrap_or("(running)")
            ),
            format!("- duration_ms: {}", self.duration_ms.unwrap_or(0)),
            format!("- step_count: {}", self.step_count),
            format!("- user_input_chars: {}", self.user_input_chars),
            format!("- source_type: {}", self.source_type),
            format!(
                "- trigger_type: {}",
                self.trigger_type.as_deref().unwrap_or("(none)")
            ),
            format!("- user_id: {}", self.user_id.as_deref().unwrap_or("(none)")),
            format!("- message_count: {}", self.message_count),
            format!("- task_id: {}", self.task_id.as_deref().unwrap_or("(none)")),
            format!(
                "- article_id: {}",
                self.article_id.as_deref().unwrap_or("(none)")
            ),
            format!("- session_text_chars: {}", self.session_text_chars),
            format!("- context_token_present: {}", self.context_token_present),
            String::new(),
            "## User Input".to_string(),
            String::new(),
            "```text".to_string(),
            self.user_input.clone(),
            "```".to_string(),
        ];

        lines.push(String::new());
        lines.push("## Upstream Messages".to_string());
        lines.push(String::new());
        if self.message_ids.is_empty() {
            lines.push("- (none)".to_string());
        } else {
            for message_id in &self.message_ids {
                lines.push(format!("- {}", message_id));
            }
        }

        if let Some(session_text) = &self.session_text {
            lines.push(String::new());
            lines.push("## Session Text".to_string());
            lines.push(String::new());
            lines.push("```text".to_string());
            lines.push(summarize_for_markdown(session_text, 1200));
            lines.push("```".to_string());
        }

        if let Some(reason) = &self.llm_fallback_reason {
            lines.push(String::new());
            lines.push("## LLM Fallback".to_string());
            lines.push(String::new());
            lines.push(format!("- reason: {}", reason));
        }

        if let Some(error) = &self.rule_parse_error {
            lines.push(String::new());
            lines.push("## Rule Parse Error".to_string());
            lines.push(String::new());
            lines.push(format!("- error: {}", error));
        }

        lines.push(String::new());
        lines.push("## Decisions".to_string());
        lines.push(String::new());
        if self.decisions.is_empty() {
            lines.push("- (none)".to_string());
        } else {
            for decision in &self.decisions {
                lines.push(format!(
                    "- step={} source={} type={} summary={}",
                    decision.step, decision.source, decision.decision_type, decision.summary
                ));
                if !decision.plan_steps.is_empty() {
                    for (idx, step_item) in decision.plan_steps.iter().enumerate() {
                        lines.push(format!("  - plan_step_{}: {}", idx + 1, step_item));
                    }
                }
                if let Some(progress_note) = &decision.progress_note {
                    lines.push(format!("  - progress_note: {}", progress_note));
                }
                if let Some(expected) = &decision.expected_observation {
                    lines.push(format!("  - expected_observation: {}", expected));
                }
            }
        }

        if !self.active_plan_steps.is_empty() {
            lines.push(String::new());
            lines.push("## Active Plan".to_string());
            lines.push(String::new());
            for (idx, step) in self.active_plan_steps.iter().enumerate() {
                let mut line = format!("{}. [{}] {}", idx + 1, step.status.as_str(), step.description);
                if let Some(expected) = &step.expected_observation {
                    line.push_str(&format!(" | expect: {}", expected.summary()));
                }
                lines.push(line);
            }
            if let Some(progress_note) = &self.last_progress_note {
                lines.push(String::new());
                lines.push(format!("- progress_note: {}", progress_note));
            }
            lines.push(String::new());
        }

        lines.push(String::new());
        lines.push("## LLM Calls".to_string());
        lines.push(String::new());
        if self.llm_calls.is_empty() {
            lines.push("- (none)".to_string());
        } else {
            for (idx, call) in self.llm_calls.iter().enumerate() {
                lines.push(format!("### LLM Call {}", idx + 1));
                lines.push(String::new());
                lines.push(format!("- source: {}", call.source));
                lines.push(format!("- model: {}", call.model));
                lines.push(format!("- base_url: {}", call.base_url));
                lines.push(format!("- success: {}", call.success));
                lines.push(format!("- attempts: {}", call.attempts));
                lines.push(format!(
                    "- response_status: {}",
                    call.response_status
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "(none)".to_string())
                ));
                lines.push(format!(
                    "- estimated_prompt_chars: {}",
                    call.prompt.estimated_prompt_chars
                ));
                lines.push(format!(
                    "- system_prompt_chars: {}",
                    call.prompt.system_prompt_chars
                ));
                lines.push(format!(
                    "- raw_user_input_chars: {}",
                    call.prompt.raw_user_input_chars
                ));
                lines.push(format!(
                    "- user_prompt_chars: {}",
                    call.prompt.user_prompt_chars
                ));
                lines.push(format!(
                    "- context_summary_chars: {}",
                    call.prompt.context_summary_chars
                ));
                lines.push(format!(
                    "- request_body_chars: {}",
                    call.prompt.request_body_chars
                ));
                lines.push(format!(
                    "- raw_response_chars: {}",
                    call.raw_response_chars
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "(none)".to_string())
                ));
                lines.push(format!(
                    "- message_content_chars: {}",
                    call.message_content_chars
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "(none)".to_string())
                ));
                if let Some(error) = &call.error {
                    lines.push(format!("- error: {}", error));
                }
                if let Some(summary) = &call.decision_summary {
                    lines.push(format!("- decision_summary: {}", summary));
                }
                lines.push(String::new());
                lines.push("#### Raw User Input".to_string());
                lines.push(String::new());
                lines.push("```text".to_string());
                lines.push(summarize_for_markdown(&call.prompt.raw_user_input, 800));
                lines.push("```".to_string());
                lines.push(String::new());
                lines.push("#### User Prompt".to_string());
                lines.push(String::new());
                lines.push("```text".to_string());
                lines.push(summarize_for_markdown(&call.prompt.user_prompt, 800));
                lines.push("```".to_string());
                lines.push(String::new());
                lines.push("#### Context Summary".to_string());
                lines.push(String::new());
                lines.push("```text".to_string());
                lines.push(summarize_for_markdown(&call.prompt.context_summary, 800));
                lines.push("```".to_string());
                if let Some(content) = &call.message_content {
                    lines.push(String::new());
                    lines.push("#### Message Content Summary".to_string());
                    lines.push(String::new());
                    lines.push("```text".to_string());
                    lines.push(summarize_for_markdown(content, 1000));
                    lines.push("```".to_string());
                }
                lines.push(String::new());
            }
        }

        lines.push("## Observations".to_string());
        lines.push(String::new());
        if self.observations.is_empty() {
            lines.push("- (none)".to_string());
        } else {
            for observation in &self.observations {
                lines.push(format!(
                    "- step={} source={} content_chars={} summary={}",
                    observation.step,
                    observation.source,
                    observation.content_chars,
                    observation.summary
                ));
            }
        }

        lines.push(String::new());
        lines.push("## Failures".to_string());
        lines.push(String::new());
        if self.failures.is_empty() {
            lines.push("- (none)".to_string());
        } else {
            for failure in &self.failures {
                lines.push(format!(
                    "- step={} kind={} action={} source={} detail={}",
                    failure.step,
                    failure.kind.as_str(),
                    failure.action.as_str(),
                    failure.source,
                    failure.detail
                ));
            }
        }

        lines.push(String::new());
        lines.push("## Tool Calls".to_string());
        lines.push(String::new());
        if self.tool_calls.is_empty() {
            lines.push("- (none)".to_string());
        } else {
            for (idx, call) in self.tool_calls.iter().enumerate() {
                lines.push(format!("### Tool Call {}", idx + 1));
                lines.push(String::new());
                lines.push(format!("- step: {}", call.step));
                lines.push(format!("- tool_name: {}", call.tool_name));
                lines.push(format!(
                    "- path: {}",
                    call.path.as_deref().unwrap_or("(none)")
                ));
                lines.push(format!(
                    "- content_chars: {}",
                    call.content_chars
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "(none)".to_string())
                ));
                lines.push(format!(
                    "- output_chars: {}",
                    call.output_chars
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "(none)".to_string())
                ));
                lines.push(format!("- success: {}", call.success));
                lines.push(format!(
                    "- duration_ms: {}",
                    call.duration_ms
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "(none)".to_string())
                ));
                if let Some(error) = &call.error {
                    lines.push(format!("- error: {}", error));
                }
                if let Some(output) = &call.output {
                    lines.push(String::new());
                    lines.push("#### Output Summary".to_string());
                    lines.push(String::new());
                    lines.push("```text".to_string());
                    lines.push(summarize_for_markdown(output, 1200));
                    lines.push("```".to_string());
                }
                lines.push(String::new());
            }
        }

        lines.push("## Final Output".to_string());
        lines.push(String::new());
        lines.push("```text".to_string());
        lines.push(summarize_for_markdown(
            self.final_output
                .as_deref()
                .unwrap_or_else(|| self.error.as_deref().unwrap_or("(none)")),
            1200,
        ));
        lines.push("```".to_string());
        lines.push(String::new());

        lines.join("\n")
    }
}

fn truncate_for_trace(input: &str, max_chars: usize) -> String {
    let count = input.chars().count();
    if count <= max_chars {
        return input.to_string();
    }
    let mut text: String = input.chars().take(max_chars).collect();
    text.push_str("...");
    text
}

fn summarize_for_markdown(input: &str, max_chars: usize) -> String {
    let count = input.chars().count();
    if count <= max_chars {
        return input.to_string();
    }
    let head_chars = max_chars.saturating_sub(80).max(40);
    let mut text: String = input.chars().take(head_chars).collect();
    text.push_str(&format!("\n\n...[truncated, total_chars={count}]"));
    text
}

fn render_daily_index_markdown(day: &str, entries: &[AgentTraceIndexEntry]) -> String {
    let total_runs = entries.len();
    let success_runs = entries.iter().filter(|entry| entry.success).count();
    let failed_runs = total_runs.saturating_sub(success_runs);
    let source_summary = summarize_sources(entries);

    let mut lines = vec![
        format!("# Agent Trace Daily Index {}", day),
        String::new(),
        format!("- total_runs: {}", total_runs),
        format!("- success_runs: {}", success_runs),
        format!("- failed_runs: {}", failed_runs),
        format!("- source_types: {}", source_summary),
        String::new(),
        "## Runs".to_string(),
        String::new(),
    ];

    if entries.is_empty() {
        lines.push("- (none)".to_string());
        lines.push(String::new());
        return lines.join("\n");
    }

    lines.push(
        "| Time | Status | Run | Source | Trigger | User | Msgs | Input | Files |".to_string(),
    );
    lines.push("| --- | --- | --- | --- | --- | --- | ---: | --- | --- |".to_string());

    let mut ordered = entries.to_vec();
    ordered.sort_by(|left, right| right.started_at.cmp(&left.started_at));

    for entry in ordered {
        let status = if entry.success { "ok" } else { "error" };
        let files = format!(
            "[json]({}) / [md]({})",
            entry.json_file, entry.markdown_file
        );
        lines.push(format!(
            "| {} | {} | `{}` | {} | {} | {} | {} | {} | {} |",
            sanitize_markdown_cell(&display_trace_time(&entry.started_at)),
            status,
            short_run_id(&entry.run_id),
            sanitize_markdown_cell(&entry.source_type),
            sanitize_markdown_cell(entry.trigger_type.as_deref().unwrap_or("(none)")),
            sanitize_markdown_cell(entry.user_id.as_deref().unwrap_or("(none)")),
            entry.message_count,
            sanitize_markdown_cell(&entry.user_input),
            files
        ));

        if !entry.success {
            let detail = entry
                .error
                .as_deref()
                .or(entry.llm_fallback_reason.as_deref())
                .unwrap_or("(none)");
            lines.push(format!(
                "| ↳ detail | {} |  |  |  |  |  | {} |  |",
                sanitize_markdown_cell(detail),
                sanitize_markdown_cell(&format!(
                    "steps={} llm={} tools={}",
                    entry.step_count, entry.llm_call_count, entry.tool_call_count
                ))
            ));
        }
    }

    lines.push(String::new());
    lines.join("\n")
}

fn log_agent_info(event: &str, fields: Vec<(&str, Value)>) {
    log_agent_event("info", event, fields);
}

fn log_agent_warn(event: &str, fields: Vec<(&str, Value)>) {
    log_agent_event("warn", event, fields);
}

fn log_agent_event(level: &str, event: &str, fields: Vec<(&str, Value)>) {
    crate::logging::emit_structured_log(level, event, fields);
}

#[cfg(test)]
fn build_agent_log_payload(level: &str, event: &str, fields: Vec<(&str, Value)>) -> Value {
    crate::logging::build_structured_log_payload(level, event, fields)
}

fn summarize_sources(entries: &[AgentTraceIndexEntry]) -> String {
    if entries.is_empty() {
        return "(none)".to_string();
    }

    let mut counts = BTreeMap::new();
    for entry in entries {
        *counts.entry(entry.source_type.as_str()).or_insert(0usize) += 1;
    }

    counts
        .into_iter()
        .map(|(source, count)| format!("{source}({count})"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn display_trace_time(started_at: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(started_at)
        .map(|value| {
            value
                .with_timezone(&Shanghai)
                .format("%H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|_| started_at.to_string())
}

fn short_run_id(run_id: &str) -> String {
    let short: String = run_id.chars().take(8).collect();
    if short.is_empty() {
        "(none)".to_string()
    } else {
        short
    }
}

fn sanitize_markdown_cell(input: &str) -> String {
    summarize_for_markdown(input, 80)
        .replace('\n', " ")
        .replace('|', "\\|")
}

fn normalize_optional_text(input: String) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn load_business_context_snapshot(
    task_store_db_path: Option<&std::path::Path>,
    trace: &AgentRunTrace,
) -> Result<Option<BusinessContextSnapshot>> {
    let Some(db_path) = task_store_db_path else {
        return Ok(None);
    };

    let store = TaskStore::open(db_path)?;
    let current_task = if let Some(task_id) = &trace.task_id {
        store.get_task_status(task_id)?
    } else {
        None
    };

    let mut recent_tasks = store.list_recent_tasks(3)?;
    if let Some(current_task) = &current_task {
        recent_tasks.retain(|task| task.task_id != current_task.task_id);
    }
    let user_memories = if let Some(user_id) = &trace.user_id {
        store.list_user_memories(user_id, 5)?
    } else {
        Vec::new()
    };

    Ok(Some(BusinessContextSnapshot {
        current_task,
        recent_tasks,
        user_memories,
    }))
}

fn build_context_summary(trace: &AgentRunTrace, observation: Option<&AgentObservation>) -> String {
    let mut parts = vec![
        format!("source={}", trace.source_type),
        format!(
            "trigger={}",
            trace.trigger_type.as_deref().unwrap_or("(none)")
        ),
        format!("user={}", trace.user_id.as_deref().unwrap_or("(none)")),
        format!("messages={}", trace.message_count),
    ];

    if let Some(task_id) = &trace.task_id {
        parts.push(format!("task_id={task_id}"));
    }
    if let Some(article_id) = &trace.article_id {
        parts.push(format!("article_id={article_id}"));
    }
    if trace.context_token_present {
        parts.push("context_token=present".to_string());
    }
    if let Some(observation) = observation {
        parts.push(format!("observation_source={}", observation.source));
        parts.push(format!(
            "observation_preview={}",
            truncate_for_trace(&observation.content, 80)
        ));
    }

    parts.join(", ")
}

fn default_expected_observation_for_decision(decision: &AgentDecision) -> Option<ExpectedObservation> {
    match decision {
        AgentDecision::CallTool(ToolAction::Create { .. })
        | AgentDecision::CallTool(ToolAction::Write { .. }) => Some(ExpectedObservation {
            kind: ObservationKind::FileMutation,
            done_rule: DoneRule::ToolSuccess,
        }),
        AgentDecision::CallTool(ToolAction::Read { .. }) => Some(ExpectedObservation {
            kind: ObservationKind::Text,
            done_rule: DoneRule::NonEmptyOutput,
        }),
        AgentDecision::CallTool(ToolAction::GetTaskStatus { .. }) => Some(ExpectedObservation {
            kind: ObservationKind::TaskStatus,
            done_rule: DoneRule::RequiresJsonField {
                field: "found".to_string(),
            },
        }),
        AgentDecision::CallTool(ToolAction::ListRecentTasks { .. })
        | AgentDecision::CallTool(ToolAction::ListManualTasks { .. }) => Some(ExpectedObservation {
            kind: ObservationKind::TaskList,
            done_rule: DoneRule::ToolSuccess,
        }),
        AgentDecision::CallTool(ToolAction::ReadArticleArchive { .. }) => {
            Some(ExpectedObservation {
                kind: ObservationKind::ArchiveContent,
                done_rule: DoneRule::RequiresJsonField {
                    field: "content".to_string(),
                },
            })
        }
        AgentDecision::Final(_) => None,
    }
}

fn validate_expected_observation(
    expected: Option<&ExpectedObservation>,
    observation: &AgentObservation,
) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };

    match expected.kind {
        ObservationKind::Text | ObservationKind::FileMutation => {}
        ObservationKind::JsonObject
        | ObservationKind::TaskStatus
        | ObservationKind::TaskList
        | ObservationKind::ArchiveContent => {
            let value: Value = serde_json::from_str(&observation.content)
                .with_context(|| format!("期望 JSON observation，但解析失败: {}", observation.source))?;
            if !value.is_object() {
                bail!("期望 JSON object observation，但返回不是 object");
            }
        }
    }

    match &expected.done_rule {
        DoneRule::ToolSuccess => Ok(()),
        DoneRule::NonEmptyOutput => {
            if observation.content.trim().is_empty() {
                bail!("期望非空输出，但 observation 为空");
            }
            Ok(())
        }
        DoneRule::RequiresJsonField { field } => {
            let value: Value = serde_json::from_str(&observation.content)
                .with_context(|| format!("done_rule 需要 JSON field，但解析失败: {}", observation.source))?;
            let object = value
                .as_object()
                .context("done_rule 需要 JSON object observation")?;
            let field_value = object
                .get(field)
                .with_context(|| format!("缺少期望字段: {field}"))?;
            if field_value.is_null() {
                bail!("期望字段 {field} 不能为空");
            }
            Ok(())
        }
    }
}

fn detect_repeated_action_failure(
    step: usize,
    planned: &PlannedDecision,
    trace: &AgentRunTrace,
    observation: Option<&AgentObservation>,
) -> Option<FailureDecision> {
    if step == 0 || observation.is_none() {
        return None;
    }
    if !matches!(planned.decision, AgentDecision::CallTool(_)) {
        return None;
    }
    let current_summary = planned.summary();
    let last_summary = trace
        .decisions
        .iter()
        .rev()
        .nth(1)
        .map(|decision| decision.summary.as_str())?;
    if current_summary != last_summary {
        return None;
    }
    Some(FailureDecision {
        kind: StepFailureKind::RepeatedAction,
        action: FailureAction::Replan,
        detail: format!("重复动作检测命中: {}", current_summary),
        source: "watchdog:repeated_action".to_string(),
    })
}

fn failure_to_observation(step: usize, failure: &FailureDecision) -> AgentObservation {
    AgentObservation {
        step,
        source: format!("failure:{}", failure.kind.as_str()),
        content: serde_json::to_string(&json!({
            "failure_kind": failure.kind.as_str(),
            "failure_action": failure.action.as_str(),
            "detail": failure.detail,
            "source": failure.source,
        }))
        .unwrap_or_else(|_| "{\"failure_kind\":\"unknown\"}".to_string()),
    }
}

fn classify_tool_execution_failure(source: String, detail: &str) -> FailureDecision {
    let lower = detail.to_ascii_lowercase();
    if lower.contains("路径越界") || lower.contains("不能为空") || lower.contains("不支持") {
        return FailureDecision {
            kind: StepFailureKind::Irrecoverable,
            action: FailureAction::Abort,
            detail: detail.to_string(),
            source,
        };
    }
    if lower.contains("读取文件失败") || lower.contains("未找到") {
        return FailureDecision {
            kind: StepFailureKind::Semantic,
            action: FailureAction::Replan,
            detail: detail.to_string(),
            source,
        };
    }
    FailureDecision {
        kind: StepFailureKind::Irrecoverable,
        action: FailureAction::Abort,
        detail: detail.to_string(),
        source,
    }
}

impl StepFailureKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Expectation => "expectation",
            Self::RepeatedAction => "repeated_action",
            Self::Semantic => "semantic",
            Self::Irrecoverable => "irrecoverable",
        }
    }
}

impl FailureAction {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Replan => "replan",
            Self::Abort => "abort",
        }
    }
}

fn resulting_source_name(action: &ToolAction) -> String {
    format!("tool:{}", action.name())
}

fn build_system_prompt(planning_policy: PlanningPolicy) -> &'static str {
    match planning_policy {
        PlanningPolicy::Reactive => {
            "你是一个工具规划器，采用最小 ReAct 风格工作：先根据上下文判断下一步，再决定是调用一个工具还是直接给出最终结果。每轮最多只调用一个工具。只输出 JSON，不要解释。格式为 {\"action\":\"read|write|create|get_task_status|list_recent_tasks|list_manual_tasks|read_article_archive|final\",\"path\":\"...\",\"content\":\"...\",\"task_id\":\"...\",\"limit\":5,\"answer\":\"...\",\"plan\":[\"步骤1\",\"步骤2\"],\"progress_note\":\"当前做到哪\",\"expected_kind\":\"text|json_object|file_mutation|task_status|task_list|archive_content\",\"done_rule\":\"tool_success|non_empty_output|required_json_field\",\"required_field\":\"field_name\"}。read 只需要 path；write/create 需要 path 与 content；get_task_status 需要 task_id；list_recent_tasks/list_manual_tasks 可选 limit；read_article_archive 需要 task_id；final 需要 answer。plan、progress_note、expected_kind、done_rule、required_field 都是可选字段，用于表达当前计划、进度和期望观测。"
        }
    }
}

impl AgentDecision {
    fn kind(&self) -> &'static str {
        match self {
            Self::CallTool(_) => "call_tool",
            Self::Final(_) => "final",
        }
    }

    fn summary(&self) -> String {
        match self {
            Self::CallTool(action) => format!("tool={} target={}", action.name(), action.target()),
            Self::Final(answer) => truncate_for_trace(answer, 240),
        }
    }
}

impl ToolAction {
    fn name(&self) -> &'static str {
        match self {
            Self::Read { .. } => "read",
            Self::Write { .. } => "write",
            Self::Create { .. } => "create",
            Self::GetTaskStatus { .. } => "get_task_status",
            Self::ListRecentTasks { .. } => "list_recent_tasks",
            Self::ListManualTasks { .. } => "list_manual_tasks",
            Self::ReadArticleArchive { .. } => "read_article_archive",
        }
    }

    fn path(&self) -> Option<&str> {
        match self {
            Self::Read { path } | Self::Write { path, .. } | Self::Create { path, .. } => {
                Some(path.as_str())
            }
            Self::GetTaskStatus { .. }
            | Self::ListRecentTasks { .. }
            | Self::ListManualTasks { .. }
            | Self::ReadArticleArchive { .. } => None,
        }
    }

    fn content(&self) -> Option<&str> {
        match self {
            Self::Read { .. } => None,
            Self::Write { content, .. } | Self::Create { content, .. } => Some(content.as_str()),
            Self::GetTaskStatus { .. }
            | Self::ListRecentTasks { .. }
            | Self::ListManualTasks { .. }
            | Self::ReadArticleArchive { .. } => None,
        }
    }

    fn target(&self) -> String {
        match self {
            Self::Read { path } | Self::Write { path, .. } | Self::Create { path, .. } => {
                path.clone()
            }
            Self::GetTaskStatus { task_id } => format!("task_id={task_id}"),
            Self::ListRecentTasks { limit } => format!("limit={limit}"),
            Self::ListManualTasks { limit } => format!("limit={limit}"),
            Self::ReadArticleArchive { task_id } => format!("task_id={task_id}"),
        }
    }
}

fn load_llm_config(
    source: &'static str,
    api_key_key: &str,
    model_key: &str,
    base_url_key: &str,
) -> Option<LlmConfig> {
    let api_key = get_env(api_key_key);
    let base_url = normalize_base_url(&get_env(base_url_key));
    if api_key.is_empty() || base_url.is_empty() {
        return None;
    }

    let model = get_env(model_key);
    let model = if model.is_empty() {
        match source {
            "MOONSHOT" => DEFAULT_MOONSHOT_MODEL.to_string(),
            _ => DEFAULT_OPENAI_MODEL.to_string(),
        }
    } else {
        model
    };

    Some(LlmConfig {
        source,
        api_key,
        model,
        base_url,
    })
}

fn normalize_base_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    let stripped = trimmed.strip_suffix("/chat/completions").unwrap_or(trimmed);
    stripped.trim_end_matches('/').to_string()
}

fn clean_env(input: String) -> String {
    input
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_matches('`')
        .trim()
        .to_string()
}

fn get_env(key: &str) -> String {
    clean_env(std::env::var(key).unwrap_or_default())
}

fn key_tail(api_key: &str) -> String {
    let suffix: String = api_key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if suffix.is_empty() {
        "none".to_string()
    } else {
        suffix
    }
}

fn is_llm_auth_error(err: &str) -> bool {
    err.contains("HTTP 401") || err.contains("Authentication Fails")
}

#[derive(Debug, Deserialize)]
struct OpenAiCompatResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAiMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct LlmPlan {
    action: String,
    path: Option<String>,
    content: Option<String>,
    answer: Option<String>,
    task_id: Option<String>,
    limit: Option<usize>,
    plan: Option<Vec<String>>,
    progress_note: Option<String>,
    expected_kind: Option<String>,
    done_rule: Option<String>,
    required_field: Option<String>,
}

fn parse_user_command(input: &str) -> Result<AgentDecision> {
    let text = normalize_user_command(input);
    if let Some(path) = text.strip_prefix("读文件 ") {
        return Ok(AgentDecision::CallTool(ToolAction::Read {
            path: path.trim().to_string(),
        }));
    }
    if let Some(rest) = text.strip_prefix("创建文件 ") {
        let (path, content) = split_path_and_content(rest)?;
        return Ok(AgentDecision::CallTool(ToolAction::Create {
            path,
            content,
        }));
    }
    if let Some(rest) = text.strip_prefix("写文件 ") {
        let (path, content) = split_path_and_content(rest)?;
        return Ok(AgentDecision::CallTool(ToolAction::Write { path, content }));
    }
    if let Some(path) = text.strip_prefix("read ") {
        return Ok(AgentDecision::CallTool(ToolAction::Read {
            path: path.trim().to_string(),
        }));
    }
    if let Some(rest) = text.strip_prefix("create ") {
        let (path, content) = split_path_and_content(rest)?;
        return Ok(AgentDecision::CallTool(ToolAction::Create {
            path,
            content,
        }));
    }
    if let Some(rest) = text.strip_prefix("write ") {
        let (path, content) = split_path_and_content(rest)?;
        return Ok(AgentDecision::CallTool(ToolAction::Write { path, content }));
    }

    bail!(
        "无法解析指令。可用格式: 读文件 <path> | 创建文件 <path> :: <content> | 写文件 <path> :: <content>"
    )
}

fn normalize_user_command(input: &str) -> &str {
    let text = input.trim();
    if let Some(rest) = text.strip_prefix("帮我运行：") {
        return rest.trim();
    }
    if let Some(rest) = text.strip_prefix("帮我运行:") {
        return rest.trim();
    }
    if let Some(rest) = text.strip_prefix("请帮我运行：") {
        return rest.trim();
    }
    if let Some(rest) = text.strip_prefix("请帮我运行:") {
        return rest.trim();
    }
    text
}

fn parse_llm_plan(raw: &str) -> Result<PlannedDecision> {
    let normalized = extract_json_object(raw).unwrap_or(raw);
    let plan: LlmPlan = serde_json::from_str(normalized)
        .with_context(|| format!("LLM 输出不是合法 JSON: {raw}"))?;
    map_llm_plan(plan)
}

fn map_llm_plan(plan: LlmPlan) -> Result<PlannedDecision> {
    let execution_plan = plan
        .plan
        .as_ref()
        .map(|steps| ExecutionPlan {
            steps: steps
                .iter()
                .map(|step| step.trim().to_string())
                .filter(|step| !step.is_empty())
                .collect(),
        })
        .filter(|plan| !plan.steps.is_empty());
    let progress_note = plan
        .progress_note
        .as_ref()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    let decision = match plan.action.trim().to_lowercase().as_str() {
        "read" => {
            let path = plan
                .path
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .context("LLM 缺少 path")?;
            AgentDecision::CallTool(ToolAction::Read { path })
        }
        "write" => {
            let path = plan
                .path
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .context("LLM 缺少 path")?;
            let content = plan.content.unwrap_or_default();
            AgentDecision::CallTool(ToolAction::Write { path, content })
        }
        "create" => {
            let path = plan
                .path
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .context("LLM 缺少 path")?;
            let content = plan.content.unwrap_or_default();
            AgentDecision::CallTool(ToolAction::Create {
                path,
                content,
            })
        }
        "get_task_status" => {
            let task_id = plan
                .task_id
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .context("LLM 缺少 task_id")?;
            AgentDecision::CallTool(ToolAction::GetTaskStatus { task_id })
        }
        "list_recent_tasks" => AgentDecision::CallTool(ToolAction::ListRecentTasks {
            limit: plan.limit.unwrap_or(5),
        }),
        "list_manual_tasks" => AgentDecision::CallTool(ToolAction::ListManualTasks {
            limit: plan.limit.unwrap_or(5),
        }),
        "read_article_archive" => {
            let task_id = plan
                .task_id
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .context("LLM 缺少 task_id")?;
            AgentDecision::CallTool(ToolAction::ReadArticleArchive {
                task_id,
            })
        }
        "final" => {
            let answer = plan
                .answer
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .context("LLM 缺少 answer")?;
            AgentDecision::Final(answer)
        }
        other => bail!("LLM action 不支持: {other}"),
    };

    let expected_observation = parse_expected_observation(
        plan.expected_kind.as_deref(),
        plan.done_rule.as_deref(),
        plan.required_field.as_deref(),
    )?
    .or_else(|| default_expected_observation_for_decision(&decision));

    Ok(PlannedDecision::new(decision)
        .with_plan(execution_plan)
        .with_progress_note(progress_note)
        .with_expected_observation(expected_observation))
}

fn parse_expected_observation(
    expected_kind: Option<&str>,
    done_rule: Option<&str>,
    required_field: Option<&str>,
) -> Result<Option<ExpectedObservation>> {
    let kind = match expected_kind.map(str::trim).filter(|value| !value.is_empty()) {
        Some("text") => Some(ObservationKind::Text),
        Some("json_object") => Some(ObservationKind::JsonObject),
        Some("file_mutation") => Some(ObservationKind::FileMutation),
        Some("task_status") => Some(ObservationKind::TaskStatus),
        Some("task_list") => Some(ObservationKind::TaskList),
        Some("archive_content") => Some(ObservationKind::ArchiveContent),
        Some(other) => bail!("expected_kind 不支持: {other}"),
        None => None,
    };
    let done_rule = match done_rule.map(str::trim).filter(|value| !value.is_empty()) {
        Some("tool_success") => Some(DoneRule::ToolSuccess),
        Some("non_empty_output") => Some(DoneRule::NonEmptyOutput),
        Some("required_json_field") => {
            let field = required_field
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .context("done_rule=required_json_field 时必须提供 required_field")?;
            Some(DoneRule::RequiresJsonField {
                field: field.to_string(),
            })
        }
        Some(other) => bail!("done_rule 不支持: {other}"),
        None => None,
    };

    match (kind, done_rule) {
        (None, None) => Ok(None),
        (Some(kind), Some(done_rule)) => Ok(Some(ExpectedObservation { kind, done_rule })),
        _ => bail!("expected_kind 与 done_rule 必须同时提供"),
    }
}

fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    raw.get(start..=end)
}

fn split_path_and_content(raw: &str) -> Result<(String, String)> {
    // 约定格式：<path> :: <content>
    let (path, content) = raw
        .split_once("::")
        .context("写入/创建指令格式错误，缺少 :: 分隔符")?;
    let path = path.trim().to_string();
    if path.is_empty() {
        bail!("文件路径不能为空");
    }
    Ok((path, content.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::{
        build_agent_log_payload, build_context_summary, load_business_context_snapshot,
        map_llm_plan, parse_llm_plan, AgentCore, AgentObservation, AgentRunContext, AgentRunTrace,
        BusinessContextSnapshot, ContextAssembler, DoneRule, ExecutionPlan,
        ExpectedObservation, LlmPlan, ObservationKind, PlannedDecision,
    };
    use crate::task_store::TaskStore;
    use serde_json::{json, Value};
    use uuid::Uuid;

    fn temp_workspace() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("amclaw_agent_test_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("创建测试目录失败");
        root
    }

    fn temp_db_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("amclaw_agent_test_{}.db", Uuid::new_v4()))
    }

    #[test]
    fn loop_create_then_read() {
        let root = temp_workspace();
        let agent = AgentCore::new(root).expect("初始化 agent 失败");

        let create = agent
            .run("创建文件 demo/hello.txt :: 你好 AMClaw")
            .expect("创建文件失败");
        assert!(create.contains("完成:"));

        let read = agent.run("读文件 demo/hello.txt").expect("读取文件失败");
        assert!(read.contains("你好 AMClaw"));
    }

    #[test]
    fn invalid_command_returns_error() {
        let root = temp_workspace();
        let agent = AgentCore::new(root).expect("初始化 agent 失败");
        let err = agent.run("unknown command").expect_err("应当返回错误");
        assert!(err.to_string().contains("无法解析指令"));
    }

    #[test]
    fn one_step_is_not_enough_for_tool_then_finalize() {
        let root = temp_workspace();
        let agent = AgentCore::with_max_steps(root, 1).expect("初始化 agent 失败");
        let err = agent
            .run("创建文件 demo/hello.txt :: 你好")
            .expect_err("单步应当无法收敛");
        assert!(err.to_string().contains("达到最大步骤"));
    }

    #[test]
    fn prefix_command_is_supported() {
        let root = temp_workspace();
        let agent = AgentCore::new(root).expect("初始化 agent 失败");
        let result = agent
            .run("帮我运行：创建文件 demo/prefix.txt :: prefix ok")
            .expect("前缀命令执行失败");
        assert!(result.contains("完成:"));
    }

    #[test]
    fn agent_run_writes_trace_file() {
        let root = temp_workspace();
        let agent = AgentCore::new(root.clone()).expect("初始化 agent 失败");

        agent.run("读文件 missing.txt").expect_err("应当返回错误");

        let trace_root = root.join("data").join("agent_traces");
        let day_dir = std::fs::read_dir(&trace_root)
            .expect("应存在 trace 根目录")
            .next()
            .expect("应存在日期目录")
            .expect("读取日期目录失败")
            .path();
        let trace_path = std::fs::read_dir(day_dir)
            .expect("应存在 trace 文件")
            .filter_map(|entry| entry.ok().map(|v| v.path()))
            .find(|path| path.extension().and_then(|v| v.to_str()) == Some("json"))
            .expect("应存在至少一个 json trace 文件");
        let payload: Value = serde_json::from_str(
            &std::fs::read_to_string(&trace_path).expect("读取 trace 文件失败"),
        )
        .expect("trace JSON 应合法");

        assert_eq!(payload["trace_version"], "agent_trace_v1");
        assert_eq!(payload["user_input"], "读文件 missing.txt");
        assert_eq!(payload["source_type"], "agent_demo");
        assert_eq!(payload["message_count"], 0);
        assert_eq!(payload["context_token_present"], false);
        assert!(payload["user_input_chars"].as_u64().unwrap_or(0) > 0);
        assert!(payload["tool_calls"].as_array().is_some());
        assert!(payload["decisions"].as_array().is_some());
        assert!(payload["observations"].as_array().is_some());

        let markdown_path = trace_path.with_extension("md");
        let markdown = std::fs::read_to_string(markdown_path).expect("应生成 markdown trace");
        assert!(markdown.contains("# Agent Trace"));
        assert!(markdown.contains("## Summary"));
        assert!(markdown.contains("## Tool Calls"));
        assert!(markdown.contains("## Observations"));

        let index_path = trace_root
            .join(
                std::fs::read_dir(&trace_root)
                    .expect("应存在日期目录")
                    .next()
                    .expect("应存在日期目录")
                    .expect("读取日期目录失败")
                    .file_name(),
            )
            .join("index.jsonl");
        let index_content = std::fs::read_to_string(index_path).expect("应生成 index.jsonl");
        assert!(index_content.contains("\"trace_version\":\"agent_trace_v1\""));
        assert!(index_content.contains("\"run_id\""));
        assert!(index_content.contains("\"source_type\":\"agent_demo\""));
    }

    #[test]
    fn agent_run_with_context_writes_upstream_metadata() {
        let root = temp_workspace();
        let agent = AgentCore::new(root.clone()).expect("初始化 agent 失败");

        agent
            .run_with_context(
                "读文件 missing.txt",
                AgentRunContext::wechat_chat(
                    "user-trace",
                    "commit",
                    vec!["msg-a".to_string(), "msg-b".to_string()],
                )
                .with_task_id("task-trace")
                .with_article_id("article-trace")
                .with_session_text("session trace text")
                .with_context_token_present(true),
            )
            .expect_err("应当返回错误");

        let trace_root = root.join("data").join("agent_traces");
        let day_dir = std::fs::read_dir(&trace_root)
            .expect("应存在 trace 根目录")
            .next()
            .expect("应存在日期目录")
            .expect("读取日期目录失败")
            .path();
        let trace_path = std::fs::read_dir(day_dir)
            .expect("应存在 trace 文件")
            .filter_map(|entry| entry.ok().map(|v| v.path()))
            .find(|path| path.extension().and_then(|v| v.to_str()) == Some("json"))
            .expect("应存在至少一个 json trace 文件");
        let payload: Value = serde_json::from_str(
            &std::fs::read_to_string(&trace_path).expect("读取 trace 文件失败"),
        )
        .expect("trace JSON 应合法");

        assert_eq!(payload["source_type"], "wechat_chat");
        assert_eq!(payload["trigger_type"], "commit");
        assert_eq!(payload["user_id"], "user-trace");
        assert_eq!(payload["message_count"], 2);
        assert_eq!(payload["task_id"], "task-trace");
        assert_eq!(payload["article_id"], "article-trace");
        assert_eq!(payload["session_text"], "session trace text");
        assert_eq!(payload["session_text_chars"], 18);
        assert_eq!(payload["context_token_present"], true);
        assert_eq!(payload["message_ids"][0], "msg-a");
        assert_eq!(payload["message_ids"][1], "msg-b");
    }

    #[test]
    fn daily_index_markdown_is_generated() {
        let root = temp_workspace();
        let agent = AgentCore::new(root.clone()).expect("初始化 agent 失败");

        agent.run("读文件 missing-a.txt").expect_err("应当返回错误");
        agent
            .run_with_context(
                "读文件 missing-b.txt",
                AgentRunContext::wechat_chat(
                    "user-index",
                    "timeout",
                    vec!["msg-index".to_string()],
                ),
            )
            .expect_err("应当返回错误");

        let trace_root = root.join("data").join("agent_traces");
        let day_name = std::fs::read_dir(&trace_root)
            .expect("应存在日期目录")
            .next()
            .expect("应存在日期目录")
            .expect("读取日期目录失败")
            .file_name();
        let index_md_path = trace_root.join(day_name).join("index.md");
        let index_md = std::fs::read_to_string(index_md_path).expect("应生成 index.md");

        assert!(index_md.contains("# Agent Trace Daily Index"));
        assert!(index_md.contains("- total_runs: 2"));
        assert!(index_md.contains("agent_demo(1)"));
        assert!(index_md.contains("wechat_chat(1)"));
        assert!(index_md
            .contains("| Time | Status | Run | Source | Trigger | User | Msgs | Input | Files |"));
        assert!(index_md.contains("[json]("));
        assert!(index_md.contains("[md]("));
        assert!(index_md.contains("user-index"));
        assert!(index_md.contains("timeout"));
    }

    #[test]
    fn agent_log_payload_keeps_contract_fields() {
        let payload = build_agent_log_payload(
            "info",
            "agent_planner_selected",
            vec![
                ("planner", json!("rule")),
                ("fallback_to", json!("none")),
                ("detail", Value::Null),
            ],
        );

        assert_eq!(payload["level"], "info");
        assert_eq!(payload["event"], "agent_planner_selected");
        assert_eq!(payload["planner"], "rule");
        assert_eq!(payload["fallback_to"], "none");
        assert!(payload.get("ts").is_some());
        assert!(payload.get("detail").is_none());
    }

    #[test]
    fn run_context_builder_keeps_optional_fields() {
        let context = AgentRunContext::wechat_chat(
            "user-builder",
            "commit",
            vec!["msg-builder".to_string()],
        )
        .with_task_id("task-builder")
        .with_article_id("article-builder")
        .with_session_text("session builder")
        .with_context_token_present(true);

        assert_eq!(context.task_id.as_deref(), Some("task-builder"));
        assert_eq!(context.article_id.as_deref(), Some("article-builder"));
        assert_eq!(context.session_text.as_deref(), Some("session builder"));
        assert!(context.context_token_present);
    }

    #[test]
    fn scripted_planner_supports_multi_step_tool_loop() {
        let root = temp_workspace();
        let agent = AgentCore::with_scripted_decisions(
            root.clone(),
            5,
            vec![
                super::AgentDecision::CallTool(super::ToolAction::Create {
                    path: "demo/loop.txt".to_string(),
                    content: "hello multi step".to_string(),
                }),
                super::AgentDecision::CallTool(super::ToolAction::Read {
                    path: "demo/loop.txt".to_string(),
                }),
                super::AgentDecision::Final("done".to_string()),
            ],
        )
        .expect("初始化 agent 失败");

        let result = agent.run("请帮我做一个多步动作").expect("多步 loop 应成功");
        assert_eq!(result, "done");

        let trace_root = root.join("data").join("agent_traces");
        let day_dir = std::fs::read_dir(&trace_root)
            .expect("应存在 trace 根目录")
            .next()
            .expect("应存在日期目录")
            .expect("读取日期目录失败")
            .path();
        let trace_path = std::fs::read_dir(day_dir)
            .expect("应存在 trace 文件")
            .filter_map(|entry| entry.ok().map(|value| value.path()))
            .find(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
            .expect("应存在至少一个 json trace 文件");
        let payload: Value = serde_json::from_str(
            &std::fs::read_to_string(trace_path).expect("读取 trace 文件失败"),
        )
        .expect("trace JSON 应合法");

        assert_eq!(payload["step_count"], 3);
        assert_eq!(payload["tool_calls"].as_array().map(|v| v.len()), Some(2));
        assert_eq!(payload["observations"].as_array().map(|v| v.len()), Some(2));
        assert_eq!(payload["final_output"], "done");
    }

    #[test]
    fn plan_step_statuses_are_tracked_in_trace() {
        let root = temp_workspace();
        let agent = AgentCore::with_max_steps_and_task_store_db_path(
            root.clone(),
            5,
            None::<std::path::PathBuf>,
        )
        .expect("初始化 agent 失败");
        agent.scripted_decisions.borrow_mut().extend([
            PlannedDecision::new(super::AgentDecision::CallTool(super::ToolAction::Create {
                path: "demo/plan.txt".to_string(),
                content: "hello".to_string(),
            }))
            .with_plan(Some(ExecutionPlan {
                steps: vec!["创建文件".to_string(), "读取文件".to_string()],
            }))
            .with_progress_note(Some("执行第一步".to_string())),
            PlannedDecision::new(super::AgentDecision::CallTool(super::ToolAction::Read {
                path: "demo/plan.txt".to_string(),
            }))
            .with_progress_note(Some("执行第二步".to_string())),
            PlannedDecision::new(super::AgentDecision::Final("done".to_string()))
                .with_progress_note(Some("计划完成".to_string())),
        ]);

        let result = agent.run("执行计划").expect("执行计划应成功");
        assert_eq!(result, "done");

        let trace_root = root.join("data").join("agent_traces");
        let day_dir = std::fs::read_dir(&trace_root)
            .expect("应存在 trace 根目录")
            .next()
            .expect("应存在日期目录")
            .expect("读取日期目录失败")
            .path();
        let trace_path = std::fs::read_dir(day_dir)
            .expect("应存在 trace 文件")
            .filter_map(|entry| entry.ok().map(|value| value.path()))
            .find(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
            .expect("应存在至少一个 json trace 文件");
        let payload: Value = serde_json::from_str(
            &std::fs::read_to_string(trace_path).expect("读取 trace 文件失败"),
        )
        .expect("trace JSON 应合法");

        assert_eq!(payload["active_plan_steps"][0]["status"], "done");
        assert_eq!(payload["active_plan_steps"][1]["status"], "done");
        assert_eq!(payload["last_progress_note"], "计划完成");
    }

    #[test]
    fn failed_tool_marks_plan_step_failed() {
        let root = temp_workspace();
        let agent = AgentCore::with_max_steps_and_task_store_db_path(
            root.clone(),
            3,
            None::<std::path::PathBuf>,
        )
        .expect("初始化 agent 失败");
        agent.scripted_decisions.borrow_mut().extend([PlannedDecision::new(
            super::AgentDecision::CallTool(super::ToolAction::Read {
                path: "missing.txt".to_string(),
            }),
        )
        .with_plan(Some(ExecutionPlan {
            steps: vec!["读取缺失文件".to_string()],
        }))]);

        let err = agent.run("读取不存在文件").expect_err("应当失败");
        assert!(err.to_string().contains("读取文件失败"));

        let trace_root = root.join("data").join("agent_traces");
        let day_dir = std::fs::read_dir(&trace_root)
            .expect("应存在 trace 根目录")
            .next()
            .expect("应存在日期目录")
            .expect("读取日期目录失败")
            .path();
        let trace_path = std::fs::read_dir(day_dir)
            .expect("应存在 trace 文件")
            .filter_map(|entry| entry.ok().map(|value| value.path()))
            .find(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
            .expect("应存在至少一个 json trace 文件");
        let payload: Value = serde_json::from_str(
            &std::fs::read_to_string(trace_path).expect("读取 trace 文件失败"),
        )
        .expect("trace JSON 应合法");

        assert_eq!(payload["active_plan_steps"][0]["status"], "failed");
    }

    #[test]
    fn successful_tool_can_fail_done_rule_validation() {
        let root = temp_workspace();
        let empty_path = root.join("demo").join("empty.txt");
        std::fs::create_dir_all(
            empty_path
                .parent()
                .expect("空文件路径应存在父目录"),
        )
        .expect("创建空文件目录失败");
        std::fs::write(&empty_path, "").expect("写入空文件失败");
        let agent = AgentCore::with_max_steps_and_task_store_db_path(
            root.clone(),
            3,
            None::<std::path::PathBuf>,
        )
        .expect("初始化 agent 失败");
        agent.scripted_decisions.borrow_mut().extend([PlannedDecision::new(
            super::AgentDecision::CallTool(super::ToolAction::Read {
                path: "demo/empty.txt".to_string(),
            }),
        )
        .with_plan(Some(ExecutionPlan {
            steps: vec!["读取非空文件".to_string()],
        }))
        .with_expected_observation(Some(ExpectedObservation {
            kind: ObservationKind::Text,
            done_rule: DoneRule::NonEmptyOutput,
        }))]);

        let err = agent.run("读取空文件").expect_err("done_rule 校验应失败");
        assert!(err.to_string().contains("期望非空输出"));

        let trace_root = root.join("data").join("agent_traces");
        let day_dir = std::fs::read_dir(&trace_root)
            .expect("应存在 trace 根目录")
            .next()
            .expect("应存在日期目录")
            .expect("读取日期目录失败")
            .path();
        let trace_path = std::fs::read_dir(day_dir)
            .expect("应存在 trace 文件")
            .filter_map(|entry| entry.ok().map(|value| value.path()))
            .find(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
            .expect("应存在至少一个 json trace 文件");
        let payload: Value = serde_json::from_str(
            &std::fs::read_to_string(trace_path).expect("读取 trace 文件失败"),
        )
        .expect("trace JSON 应合法");

        assert_eq!(payload["active_plan_steps"][0]["status"], "failed");
    }

    #[test]
    fn context_assembler_includes_runtime_fields_and_observation() {
        let workspace = temp_workspace();
        let trace = AgentRunTrace::new(
            &workspace,
            "读文件 demo.txt",
            AgentRunContext::wechat_chat(
                "user-context",
                "commit",
                vec!["msg-1".to_string(), "msg-2".to_string()],
            )
            .with_task_id("task-ctx")
            .with_article_id("article-ctx")
            .with_session_text("session merged text")
            .with_context_token_present(true),
        );
        let observation = AgentObservation::tool_result(1, "read_file", "hello from tool");
        let business_context = BusinessContextSnapshot {
            current_task: None,
            recent_tasks: Vec::new(),
            user_memories: Vec::new(),
        };
        let planner_input = ContextAssembler.assemble(
            &trace,
            "读文件 demo.txt",
            Some(&observation),
            &[
                "read: 读取工作区内文件，参数: path".to_string(),
                "get_task_status: 查询单个任务状态，参数: task_id".to_string(),
            ],
            Some(&business_context),
        );

        assert!(planner_input.assembled_user_prompt.contains("## Runtime Context"));
        assert!(planner_input.assembled_user_prompt.contains("source_type: wechat_chat"));
        assert!(planner_input.assembled_user_prompt.contains("task_id: task-ctx"));
        assert!(planner_input.assembled_user_prompt.contains("article_id: article-ctx"));
        assert!(planner_input.assembled_user_prompt.contains("session merged text"));
        assert!(planner_input.assembled_user_prompt.contains("## Latest Observation"));
        assert!(planner_input.assembled_user_prompt.contains("hello from tool"));
    }

    #[test]
    fn business_context_snapshot_reads_current_and_recent_tasks() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let current = store
            .record_link_submission("https://example.com/current-task")
            .expect("写入当前任务失败");
        let _recent = store
            .record_link_submission("https://example.com/recent-task")
            .expect("写入最近任务失败");
        let trace = AgentRunTrace::new(
            &workspace,
            "状态 task",
            AgentRunContext::wechat_chat("user-biz", "commit", vec![])
                .with_task_id(current.task_id.clone()),
        );

        let business = load_business_context_snapshot(Some(db_path.as_path()), &trace)
            .expect("读取业务上下文失败")
            .expect("应存在业务上下文");

        assert_eq!(
            business.current_task.as_ref().map(|value| value.task_id.as_str()),
            Some(current.task_id.as_str())
        );
        assert!(!business.recent_tasks.is_empty());
    }

    #[test]
    fn context_assembler_includes_business_context_sections() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let current = store
            .record_link_submission("https://example.com/current-context")
            .expect("写入任务失败");
        let _recent = store
            .record_link_submission("https://example.com/recent-context")
            .expect("写入最近任务失败");
        let trace = AgentRunTrace::new(
            &workspace,
            "帮我看任务",
            AgentRunContext::wechat_chat("user-biz-ctx", "commit", vec![])
                .with_task_id(current.task_id.clone()),
        );
        let business = load_business_context_snapshot(Some(db_path.as_path()), &trace)
            .expect("读取业务上下文失败")
            .expect("应存在业务上下文");

        let planner_input = ContextAssembler.assemble(
            &trace,
            "帮我看任务",
            None,
            &["get_task_status: 查询单个任务状态，参数: task_id".to_string()],
            Some(&business),
        );

        assert!(planner_input.assembled_user_prompt.contains("## Current Task"));
        assert!(planner_input
            .assembled_user_prompt
            .contains(&current.task_id));
        assert!(planner_input.assembled_user_prompt.contains("## Recent Tasks"));
    }

    #[test]
    fn business_context_snapshot_reads_user_memories() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        store
            .add_user_memory("user-memory", "我喜欢短摘要")
            .expect("写入 memory 失败");
        let trace = AgentRunTrace::new(
            &workspace,
            "帮我总结",
            AgentRunContext::wechat_chat("user-memory", "commit", vec![]),
        );

        let business = load_business_context_snapshot(Some(db_path.as_path()), &trace)
            .expect("读取业务上下文失败")
            .expect("应存在业务上下文");

        assert_eq!(business.user_memories.len(), 1);
        assert_eq!(business.user_memories[0].content, "我喜欢短摘要");
    }

    #[test]
    fn context_summary_contains_core_runtime_signals() {
        let workspace = temp_workspace();
        let trace = AgentRunTrace::new(
            &workspace,
            "读文件 demo.txt",
            AgentRunContext::wechat_chat("user-summary", "timeout", vec!["msg-9".to_string()])
                .with_task_id("task-summary")
                .with_context_token_present(true),
        );
        let observation = AgentObservation::tool_result(2, "read_file", "summary text");
        let summary = build_context_summary(&trace, Some(&observation));

        assert!(summary.contains("source=wechat_chat"));
        assert!(summary.contains("trigger=timeout"));
        assert!(summary.contains("user=user-summary"));
        assert!(summary.contains("messages=1"));
        assert!(summary.contains("task_id=task-summary"));
        assert!(summary.contains("context_token=present"));
        assert!(summary.contains("observation_source=tool:read_file"));
    }

    #[test]
    fn llm_plan_json_is_supported() {
        let decision = parse_llm_plan("{\"action\":\"read\",\"path\":\"demo/a.txt\"}")
            .expect("LLM JSON 解析失败");
        assert!(matches!(
            decision.decision,
            super::AgentDecision::CallTool(super::ToolAction::Read { .. })
        ));
    }

    #[test]
    fn llm_plan_markdown_json_is_supported() {
        let raw = "```json\n{\"action\":\"final\",\"answer\":\"ok\"}\n```";
        let decision = parse_llm_plan(raw).expect("Markdown JSON 解析失败");
        assert!(matches!(decision.decision, super::AgentDecision::Final(_)));
    }

    #[test]
    fn map_llm_plan_requires_path_for_read() {
        let err = map_llm_plan(LlmPlan {
            action: "read".to_string(),
            path: None,
            content: None,
            answer: None,
            task_id: None,
            limit: None,
            plan: None,
            progress_note: None,
            expected_kind: None,
            done_rule: None,
            required_field: None,
        })
        .expect_err("read 无 path 应失败");
        assert!(err.to_string().contains("path"));
    }

    #[test]
    fn llm_plan_business_tools_are_supported() {
        let status = parse_llm_plan("{\"action\":\"get_task_status\",\"task_id\":\"task-1\"}")
            .expect("业务工具 JSON 解析失败");
        assert!(matches!(
            status.decision,
            super::AgentDecision::CallTool(super::ToolAction::GetTaskStatus { .. })
        ));

        let recent = parse_llm_plan("{\"action\":\"list_recent_tasks\",\"limit\":3}")
            .expect("最近任务工具 JSON 解析失败");
        assert!(matches!(
            recent.decision,
            super::AgentDecision::CallTool(super::ToolAction::ListRecentTasks { limit: 3 })
        ));

        let archive = parse_llm_plan("{\"action\":\"read_article_archive\",\"task_id\":\"task-2\"}")
            .expect("归档工具 JSON 解析失败");
        assert!(matches!(
            archive.decision,
            super::AgentDecision::CallTool(super::ToolAction::ReadArticleArchive { .. })
        ));
    }

    #[test]
    fn llm_plan_with_plan_steps_and_progress_is_supported() {
        let planned = parse_llm_plan(
            r#"{"action":"get_task_status","task_id":"task-1","plan":["查询任务","总结结果"],"progress_note":"先查任务状态"}"#,
        )
        .expect("带计划的 LLM JSON 解析失败");

        assert!(matches!(
            planned.decision,
            super::AgentDecision::CallTool(super::ToolAction::GetTaskStatus { .. })
        ));
        assert_eq!(
            planned.plan.as_ref().map(|plan| plan.steps.clone()),
            Some(vec!["查询任务".to_string(), "总结结果".to_string()])
        );
        assert_eq!(planned.progress_note.as_deref(), Some("先查任务状态"));
    }

    #[test]
    fn llm_plan_with_expected_observation_is_supported() {
        let planned = parse_llm_plan(
            r#"{"action":"get_task_status","task_id":"task-1","expected_kind":"task_status","done_rule":"required_json_field","required_field":"found"}"#,
        )
        .expect("带 expected_observation 的 LLM JSON 解析失败");

        assert!(matches!(
            planned.expected_observation,
            Some(ExpectedObservation {
                kind: ObservationKind::TaskStatus,
                done_rule: DoneRule::RequiresJsonField { .. }
            })
        ));
    }
}
