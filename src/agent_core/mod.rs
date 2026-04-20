use crate::config::AgentConfig;
use crate::context_pack::*;
use crate::retriever::cached_embedding::CachedEmbeddingProvider;
use crate::retriever::embedding::NoOpEmbeddingProvider;
use crate::retriever::hybrid::HybridRetriever;
use crate::retriever::rule::RuleRetriever;
use crate::retriever::shadow::ShadowRetriever;
use crate::retriever::Retriever;
use crate::session_summary::*;
use crate::task_store::{
    FeedbackKind, MemoryFeedbackState, RecentTaskRecord, TaskStatusRecord, TaskStore,
    UserMemoryRecord, UserSessionStateRecord,
};
use crate::tool_registry::{ToolAction, ToolRegistry};
use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use chrono_tz::Asia::Shanghai;
use reqwest::blocking::Client;
use reqwest::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};
use uuid::Uuid;
#[cfg(test)]
use {std::cell::RefCell, std::collections::VecDeque};

const DEFAULT_MAX_STEPS: usize = 8;
const MAX_STEP_RETRIES: usize = 1;
const DEFAULT_MAX_REPLANS: usize = 3;
const DEFAULT_OPENAI_MODEL: &str = "deepseek-chat";
const DEFAULT_MOONSHOT_MODEL: &str = "kimi-k2.5";
const LLM_PROVIDER_PRIORITY: [&str; 3] = ["DEEPSEEK", "MOONSHOT", "OPENAI"];

/// 检索模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum RetrieverMode {
    /// 规则法（默认）：priority / useful / use_count 排序
    Rule,
    /// 语义检索（尚未实现，临时回退到 Rule）
    Semantic,
    /// 混合检索（尚未实现，临时回退到 Rule）
    Hybrid,
    /// Shadow：并行运行语义但只用规则结果（尚未实现，临时回退到 Rule）
    Shadow,
}

impl RetrieverMode {
    /// 从配置字符串解析。非法值明确报错。
    fn from_config(text: &str) -> Result<Self> {
        match text {
            "rule" => Ok(Self::Rule),
            "semantic" => Ok(Self::Semantic),
            "hybrid" => Ok(Self::Hybrid),
            "shadow" => Ok(Self::Shadow),
            other => bail!("非法 retriever_mode: {other}。合法值: rule, semantic, hybrid, shadow"),
        }
    }
}

/// 根据 mode、db_path 和 embedding_provider 选择 retriever。
///
/// - semantic / hybrid / shadow 根据 embedding_provider 配置选择 provider
/// - embedding_provider = "noop" 时 fallback 到 rule
fn select_retriever(
    mode: RetrieverMode,
    db_path: Option<&Path>,
    embedding_provider_name: &str,
) -> Box<dyn Retriever + Send + Sync> {
    match (mode, db_path) {
        (RetrieverMode::Rule, Some(path)) => Box::new(RuleRetriever::new(path)),
        (RetrieverMode::Rule, None) => Box::new(NoOpRetriever),
        (RetrieverMode::Semantic, Some(path)) => {
            log_agent_info(
                "retriever_mode_fallback",
                vec![
                    ("requested_mode", json!("semantic")),
                    ("actual_mode", json!("rule")),
                    (
                        "reason",
                        json!("semantic retriever not yet implemented, falling back to rule_v1"),
                    ),
                ],
            );
            Box::new(RuleRetriever::new(path).with_name("rule_v1_semantic_fallback"))
        }
        (RetrieverMode::Semantic, None) => {
            log_agent_info(
                "retriever_mode_fallback_noop",
                vec![
                    ("requested_mode", json!("semantic")),
                    ("actual_mode", json!("noop")),
                    ("reason", json!("no db_path, using NoOpRetriever")),
                ],
            );
            Box::new(NoOpRetriever)
        }
        (RetrieverMode::Hybrid, Some(path)) => {
            let inner_provider = match crate::retriever::embedding::create_embedding_provider(
                embedding_provider_name,
            ) {
                Ok(p) => p,
                Err(err) => {
                    log_agent_warn(
                        "embedding_provider_init_failed",
                        vec![
                            ("provider", json!(embedding_provider_name)),
                            ("error", json!(err.to_string())),
                            ("fallback", json!("NoOpEmbeddingProvider")),
                        ],
                    );
                    Box::new(NoOpEmbeddingProvider::new())
                }
            };
            let provider = Box::new(CachedEmbeddingProvider::new(inner_provider, path));
            Box::new(HybridRetriever::new(path, provider))
        }
        (RetrieverMode::Hybrid, None) => {
            log_agent_info(
                "retriever_mode_fallback_noop",
                vec![
                    ("requested_mode", json!("hybrid")),
                    ("actual_mode", json!("noop")),
                    ("reason", json!("no db_path, using NoOpRetriever")),
                ],
            );
            Box::new(NoOpRetriever)
        }
        (RetrieverMode::Shadow, Some(path)) => {
            let inner_provider = match crate::retriever::embedding::create_embedding_provider(
                embedding_provider_name,
            ) {
                Ok(p) => p,
                Err(err) => {
                    log_agent_warn(
                        "embedding_provider_init_failed",
                        vec![
                            ("provider", json!(embedding_provider_name)),
                            ("error", json!(err.to_string())),
                            ("fallback", json!("NoOpEmbeddingProvider")),
                        ],
                    );
                    Box::new(NoOpEmbeddingProvider::new())
                }
            };
            let provider = Box::new(CachedEmbeddingProvider::new(inner_provider, path));
            let hybrid = Box::new(HybridRetriever::new(path, provider));
            Box::new(ShadowRetriever::new(path, Some(hybrid)))
        }
        (RetrieverMode::Shadow, None) => {
            log_agent_info(
                "retriever_mode_fallback_noop",
                vec![
                    ("requested_mode", json!("shadow")),
                    ("actual_mode", json!("noop")),
                    ("reason", json!("no db_path, using NoOpRetriever")),
                ],
            );
            Box::new(NoOpRetriever)
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ContextCompactionConfig {
    session_summary_strategy: SessionSummaryStrategy,
    include_previous_observations: bool,
    memory_budget: MemoryBudget,
}

impl Default for ContextCompactionConfig {
    fn default() -> Self {
        Self {
            session_summary_strategy: SessionSummaryStrategy::Semantic,
            include_previous_observations: false,
            memory_budget: MemoryBudget::default(),
        }
    }
}

impl ContextCompactionConfig {
    fn from_agent_config(agent_config: &AgentConfig) -> Self {
        Self {
            session_summary_strategy: SessionSummaryStrategy::from_config_text(
                &agent_config.session_summary_strategy,
            ),
            include_previous_observations: agent_config.include_previous_observations,
            memory_budget: MemoryBudget::from_agent_config(agent_config),
        }
    }
}

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
struct AgentObservation {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
struct RuntimeSessionStateSnapshot {
    goal: Option<String>,
    current_subtask: Option<String>,
    constraints: Vec<String>,
    confirmed_facts: Vec<String>,
    done_items: Vec<String>,
    next_step: Option<String>,
    open_questions: Vec<String>,
    /// goal 信号来源标记，仅运行时用于 low-signal 判定
    #[serde(skip)]
    goal_signal: GoalSignal,
}

#[derive(Debug, Clone, Copy)]
pub enum ContextPreviewMode {
    Summary,
    Verbose,
}

impl RuntimeSessionStateSnapshot {
    fn is_empty(&self) -> bool {
        self.goal.is_none()
            && self.current_subtask.is_none()
            && self.constraints.is_empty()
            && self.confirmed_facts.is_empty()
            && self.done_items.is_empty()
            && self.next_step.is_none()
            && self.open_questions.is_empty()
    }

    /// 仅有默认 goal、且无其他高信号字段时视为低价值，不注入 prompt。
    /// 判定依据改为 goal_signal 来源标记，不再依赖中文前缀字符串。
    fn is_low_signal(&self) -> bool {
        // 若存在任何数组槽位内容，或 next_step/current_subtask 有值，则非低信号
        if self.current_subtask.is_some()
            || self.next_step.is_some()
            || !self.constraints.is_empty()
            || !self.confirmed_facts.is_empty()
            || !self.done_items.is_empty()
            || !self.open_questions.is_empty()
        {
            return false;
        }
        // 只剩 goal：按来源标记判定
        self.goal_signal == GoalSignal::RuntimeDefault
    }

    fn to_lines(&self) -> Vec<String> {
        let mut lines = vec![String::new(), "## Session State".to_string()];
        if let Some(goal) = &self.goal {
            lines.push(format!("- goal: {}", goal));
        }
        if let Some(current_subtask) = &self.current_subtask {
            lines.push(format!("- current_subtask: {}", current_subtask));
        }
        if !self.constraints.is_empty() {
            lines.push("- constraints:".to_string());
            for item in &self.constraints {
                lines.push(format!("  - {}", item));
            }
        }
        if !self.confirmed_facts.is_empty() {
            lines.push("- confirmed_facts:".to_string());
            for item in &self.confirmed_facts {
                lines.push(format!("  - {}", item));
            }
        }
        if !self.done_items.is_empty() {
            lines.push("- done_items:".to_string());
            for item in &self.done_items {
                lines.push(format!("  - {}", item));
            }
        }
        if let Some(next_step) = &self.next_step {
            lines.push(format!("- next_step: {}", next_step));
        }
        if !self.open_questions.is_empty() {
            lines.push("- open_questions:".to_string());
            for item in &self.open_questions {
                lines.push(format!("  - {}", item));
            }
        }
        lines
    }
}

#[derive(Debug, Clone)]
struct PlannerInput {
    raw_user_input: String,
    assembled_user_prompt: String,
    context_sections: Vec<ContextSectionSnapshot>,
    context_budget_summary: ContextBudgetSummary,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum StepFailureKind {
    Transient,
    Expectation,
    LowValueObservation,
    RepeatedAction,
    BudgetExhausted,
    StalledTrajectory,
    TrajectoryDrift,
    ManualIntervention,
    Semantic,
    Irrecoverable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum FailureAction {
    RetryStep,
    Replan,
    AskUser,
    Abort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RecoveryOutcome {
    Continued,
    EscalatedToAskUser,
    Aborted,
    Failed,
}

/// 统一恢复策略定义（集中映射表，不分散 if/else）。
///
/// 映射规则（代码内唯一真相源）：
/// - Transient         → RetryStep (max 1) → 失败则 Replan
/// - LowValueObservation → Replan (max 1) → 再失败 AskUser
/// - RepeatedAction    → Replan (max 1) → 再失败 AskUser
/// - StalledTrajectory → Replan (max 1) → 再失败 AskUser
/// - TrajectoryDrift   → Replan (max 1) → 再失败 AskUser
/// - Expectation       → Replan (max 1) → 再失败 AskUser
/// - Semantic          → Replan (max 1) → 再失败 AskUser
/// - ManualIntervention → AskUser (立即)
/// - BudgetExhausted   → AskUser (立即)
/// - Irrecoverable     → Abort (立即)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RecoveryPolicy {
    action: FailureAction,
    /// 该 failure kind 在当前 run 中最多允许的恢复尝试次数
    max_attempts: usize,
    /// 超过 max_attempts 后自动升级到的 action
    escalate_to: FailureAction,
}

impl RecoveryPolicy {
    fn no_recovery(action: FailureAction) -> Self {
        Self {
            action,
            max_attempts: 0,
            escalate_to: action,
        }
    }

    fn with_escalate(
        action: FailureAction,
        max_attempts: usize,
        escalate_to: FailureAction,
    ) -> Self {
        Self {
            action,
            max_attempts,
            escalate_to,
        }
    }
}

/// 默认恢复策略映射表（集中定义，所有 failure kind 的恢复行为从此处读取）。
fn default_recovery_for_failure(kind: StepFailureKind) -> RecoveryPolicy {
    match kind {
        StepFailureKind::Transient => {
            RecoveryPolicy::with_escalate(FailureAction::RetryStep, 1, FailureAction::Replan)
        }
        StepFailureKind::LowValueObservation
        | StepFailureKind::RepeatedAction
        | StepFailureKind::StalledTrajectory
        | StepFailureKind::TrajectoryDrift
        | StepFailureKind::Expectation
        | StepFailureKind::Semantic => {
            RecoveryPolicy::with_escalate(FailureAction::Replan, 1, FailureAction::AskUser)
        }
        StepFailureKind::ManualIntervention | StepFailureKind::BudgetExhausted => {
            RecoveryPolicy::no_recovery(FailureAction::AskUser)
        }
        StepFailureKind::Irrecoverable => RecoveryPolicy::no_recovery(FailureAction::Abort),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ReplanScope {
    CurrentStep,
    RemainingPlan,
    Full,
}

impl ReplanScope {
    fn as_str(&self) -> &'static str {
        match self {
            Self::CurrentStep => "current_step",
            Self::RemainingPlan => "remaining_plan",
            Self::Full => "full",
        }
    }
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
    retry_count: usize,
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
    replan_scope: Option<ReplanScope>,
    detail: String,
    source: String,
    user_message: Option<String>,
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
    expected_fields: Vec<String>,
    minimum_novelty: Option<MinimumNovelty>,
}

#[derive(Debug, Clone, Serialize)]
struct RuntimeControllerState {
    max_steps: usize,
    max_replans: usize,
    failure_count: usize,
    replan_count: usize,
    ask_user_count: usize,
    /// per-failure-kind 恢复尝试计数（防循环保护）。
    /// key: kind.as_str()，value: 该 kind 已触发的恢复次数。
    #[serde(skip)]
    recovery_kind_counts: std::collections::HashMap<String, usize>,
}

impl RuntimeControllerState {
    fn new(max_steps: usize, max_replans: usize) -> Self {
        Self {
            max_steps,
            max_replans,
            failure_count: 0,
            replan_count: 0,
            ask_user_count: 0,
            recovery_kind_counts: std::collections::HashMap::new(),
        }
    }

    fn configure_limits(&mut self, max_steps: usize, max_replans: usize) {
        self.max_steps = max_steps;
        self.max_replans = max_replans;
    }

    fn record_failure(&mut self) {
        self.failure_count += 1;
    }

    fn record_ask_user(&mut self) {
        self.ask_user_count += 1;
    }

    fn try_consume_replan(&mut self) -> bool {
        if self.replan_count >= self.max_replans {
            return false;
        }
        self.replan_count += 1;
        true
    }

    fn remaining_replans(&self) -> usize {
        self.max_replans.saturating_sub(self.replan_count)
    }

    /// 记录一次 failure kind 的恢复尝试，返回当前计数（含本次）。
    fn record_recovery_for_kind(&mut self, kind: &StepFailureKind) -> usize {
        let key = kind.as_str().to_string();
        let count = self.recovery_kind_counts.entry(key).or_insert(0);
        *count += 1;
        *count
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum MinimumNovelty {
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

#[derive(Debug, Clone, Default)]
struct BusinessContextSnapshot {
    current_task: Option<TaskStatusRecord>,
    recent_tasks: Vec<RecentTaskRecord>,
    user_memories: Vec<UserMemoryRecord>,
}

/// Memory 预算配置
#[derive(Debug, Clone, Copy)]
struct MemoryBudget {
    max_items: usize,
    max_total_chars: usize,
    max_single_chars: usize,
}

impl Default for MemoryBudget {
    fn default() -> Self {
        Self {
            max_items: 5,
            max_total_chars: 500,
            max_single_chars: 160,
        }
    }
}

impl MemoryBudget {
    fn from_agent_config(agent_config: &AgentConfig) -> Self {
        Self {
            max_items: agent_config.memory_max_items,
            max_total_chars: agent_config.memory_max_total_chars,
            max_single_chars: agent_config.memory_max_single_chars,
        }
    }

    /// 轻量动态上调：有 current_task 或计划步较多时上调 20%
    fn with_dynamic_adjustment(self, has_current_task: bool, plan_step_count: usize) -> Self {
        if !has_current_task && plan_step_count <= 3 {
            return self;
        }
        Self {
            max_items: (self.max_items * 12) / 10,
            max_total_chars: (self.max_total_chars * 12) / 10,
            max_single_chars: self.max_single_chars,
        }
    }
}

/// 被裁剪掉的记忆及其原因
#[derive(Debug, Clone)]
struct DroppedMemory {
    id: String,
    content_preview: String,
    reason: DropReason,
}

/// 裁剪原因
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DropReason {
    /// 规范化后与更高优先级的记忆重复
    Deduplicated,
    /// 单条字符数超过 max_single_chars
    SingleItemTooLong,
    /// 总字符数或条数超过预算
    BudgetExceeded,
}

impl DropReason {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Deduplicated => "deduplicated",
            Self::SingleItemTooLong => "single_item_too_long",
            Self::BudgetExceeded => "budget_exceeded",
        }
    }
}

/// 单次 agent run 的 memory 生命周期状态
///
/// 设计原则：
/// - 只管"本次请求生命周期"，不管长期存储
/// - retrieved → injected / dropped 的完整链路
/// - trace / log / markdown 都从这里投影，不各自维护
#[derive(Debug, Clone, Default)]
struct SessionState {
    /// 注入预算
    budget: MemoryBudget,
    /// 从 DB 检索出的候选记忆（已排序、未裁剪）
    retrieved: Vec<UserMemoryRecord>,
    /// 经裁剪后实际注入 prompt 的记忆
    injected: Vec<UserMemoryRecord>,
    /// 被裁剪掉的记忆及原因
    dropped: Vec<DroppedMemory>,
    /// 使用的检索器名称（用于 trace / A/B 对比）
    retriever_name: String,
    /// 检索耗时（毫秒）
    retrieval_latency_ms: u128,
    /// 检索器原始候选条数
    retrieval_candidate_count: usize,
    /// 预算裁剪后命中（注入）条数
    retrieval_hit_count: usize,
    /// 检索模式（rule / hybrid / semantic / shadow）
    retrieval_mode: String,
    /// 检索回退原因（如 embedding 失败、query_text 为空）
    retrieval_fallback_reason: Option<String>,
    /// 候选结果是否包含语义分数（hybrid/semantic 时为 true）
    retrieval_scores_present: bool,
}

impl SessionState {
    /// 从检索结果构建 SessionState，执行去重 + 预算裁剪
    ///
    /// 裁剪逻辑（从 task_store 上移到此处）：
    /// 1. 规范化去重（trim + 多空格压缩）
    /// 2. 单条超长跳过
    /// 3. 总预算检查
    fn from_retrieved(retrieved: Vec<UserMemoryRecord>, budget: MemoryBudget) -> Self {
        let mut seen_normalized = std::collections::HashSet::new();
        let mut injected = Vec::new();
        let mut dropped = Vec::new();
        let mut total_chars = 0;

        for mem in &retrieved {
            let normalized: String = mem
                .content
                .to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if seen_normalized.contains(&normalized) {
                dropped.push(DroppedMemory {
                    id: mem.id.clone(),
                    content_preview: summarize_for_markdown(&mem.content, 40),
                    reason: DropReason::Deduplicated,
                });
                continue;
            }
            if mem.content.chars().count() > budget.max_single_chars {
                dropped.push(DroppedMemory {
                    id: mem.id.clone(),
                    content_preview: summarize_for_markdown(&mem.content, 40),
                    reason: DropReason::SingleItemTooLong,
                });
                continue;
            }
            total_chars += mem.content.chars().count();
            if injected.len() >= budget.max_items || total_chars > budget.max_total_chars {
                dropped.push(DroppedMemory {
                    id: mem.id.clone(),
                    content_preview: summarize_for_markdown(&mem.content, 40),
                    reason: DropReason::BudgetExceeded,
                });
                continue;
            }
            seen_normalized.insert(normalized);
            injected.push(mem.clone());
        }

        Self {
            budget,
            retrieved,
            injected,
            dropped,
            retriever_name: String::new(),
            retrieval_latency_ms: 0,
            retrieval_candidate_count: 0,
            retrieval_hit_count: 0,
            retrieval_mode: String::new(),
            retrieval_fallback_reason: None,
            retrieval_scores_present: false,
        }
    }

    /// 检索到的候选记忆条数
    fn retrieved_count(&self) -> usize {
        self.retrieved.len()
    }

    /// 实际注入 prompt 的记忆条数
    fn injected_count(&self) -> usize {
        self.injected.len()
    }

    /// 注入记忆的总字符数
    fn injected_total_chars(&self) -> usize {
        self.injected
            .iter()
            .map(|m| m.content.chars().count())
            .sum()
    }

    /// 注入记忆的 ID 列表
    fn injected_ids(&self) -> Vec<String> {
        self.injected.iter().map(|m| m.id.clone()).collect()
    }

    /// 是否有任何记忆活动（检索到或注入）
    fn has_memory_activity(&self) -> bool {
        !self.retrieved.is_empty()
    }

    /// 是否记录了 retriever 级可观测信息
    fn has_retrieval_observability(&self) -> bool {
        !self.retriever_name.is_empty()
    }
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

#[derive(Debug)]
struct ContextAssembler {
    include_previous_observations: bool,
}

impl Default for ContextAssembler {
    fn default() -> Self {
        Self {
            include_previous_observations: false,
        }
    }
}

impl ContextAssembler {
    #[cfg(test)]
    fn assemble(
        &self,
        trace: &AgentRunTrace,
        user_input: &str,
        observation: Option<&AgentObservation>,
        runtime_session_state: Option<&RuntimeSessionStateSnapshot>,
        available_tools: &[String],
        business_context: Option<&BusinessContextSnapshot>,
    ) -> PlannerInput {
        self.assemble_with_summary_strategy(
            trace,
            user_input,
            observation,
            runtime_session_state,
            available_tools,
            business_context,
            SessionSummaryStrategy::Semantic,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble_with_summary_strategy(
        &self,
        trace: &AgentRunTrace,
        user_input: &str,
        observation: Option<&AgentObservation>,
        runtime_session_state: Option<&RuntimeSessionStateSnapshot>,
        available_tools: &[String],
        business_context: Option<&BusinessContextSnapshot>,
        session_summary_strategy: SessionSummaryStrategy,
    ) -> PlannerInput {
        let context_pack = self.build_pack_with_summary_strategy(
            trace,
            user_input,
            observation,
            runtime_session_state,
            available_tools,
            business_context,
            session_summary_strategy,
        );
        let assembled_user_prompt = context_pack.render();
        let context_sections = context_pack.snapshot();
        let context_budget_summary = context_pack.budget_summary();
        let context_summary = build_context_summary(trace, observation);

        PlannerInput {
            raw_user_input: user_input.to_string(),
            assembled_user_prompt,
            context_sections,
            context_budget_summary,
            context_summary,
        }
    }

    #[cfg(test)]
    fn build_pack(
        &self,
        trace: &AgentRunTrace,
        user_input: &str,
        observation: Option<&AgentObservation>,
        runtime_session_state: Option<&RuntimeSessionStateSnapshot>,
        available_tools: &[String],
        business_context: Option<&BusinessContextSnapshot>,
    ) -> ContextPack {
        self.build_pack_with_summary_strategy(
            trace,
            user_input,
            observation,
            runtime_session_state,
            available_tools,
            business_context,
            SessionSummaryStrategy::Semantic,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_pack_with_summary_strategy(
        &self,
        trace: &AgentRunTrace,
        user_input: &str,
        observation: Option<&AgentObservation>,
        runtime_session_state: Option<&RuntimeSessionStateSnapshot>,
        available_tools: &[String],
        business_context: Option<&BusinessContextSnapshot>,
        session_summary_strategy: SessionSummaryStrategy,
    ) -> ContextPack {
        let mut pack = ContextPack::default();

        pack.push(ContextSection::new(
            ContextSectionKind::Preamble,
            vec!["你正在处理一次 AMClaw agent 运行。请基于下面上下文决定下一步。".to_string()],
        ));

        pack.push(ContextSection::new(
            ContextSectionKind::CurrentIntent,
            vec![
                String::new(),
                "## User Input".to_string(),
                user_input.trim().to_string(),
            ],
        ));

        let mut runtime_lines = vec![
            String::new(),
            "## Runtime Context".to_string(),
            format!("- source_type: {}", trace.source_type),
            format!(
                "- trigger_type: {}",
                trace.trigger_type.as_deref().unwrap_or("(none)")
            ),
            format!(
                "- user_id: {}",
                trace.user_id.as_deref().unwrap_or("(none)")
            ),
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
            format!(
                "- replan_budget: {}/{}",
                trace.controller_state.replan_count, trace.controller_state.max_replans
            ),
            format!("- failure_count: {}", trace.controller_state.failure_count),
            format!(
                "- current_step_index: {}",
                trace
                    .current_step_index
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "(none)".to_string())
            ),
        ];

        if !trace.message_ids.is_empty() {
            runtime_lines.push("- message_ids:".to_string());
            for message_id in &trace.message_ids {
                runtime_lines.push(format!("  - {}", message_id));
            }
        }
        pack.push(ContextSection::new(
            ContextSectionKind::RuntimeContext,
            runtime_lines,
        ));

        if let Some(runtime_session_state) =
            runtime_session_state.filter(|state| !state.is_empty() && !state.is_low_signal())
        {
            pack.push(ContextSection::new(
                ContextSectionKind::SessionState,
                runtime_session_state.to_lines(),
            ));
        }

        if let Some(session_text) = &trace.session_text {
            pack.push(ContextSection::new(
                ContextSectionKind::SessionText,
                build_session_text_section_lines(session_text, session_summary_strategy),
            ));
        }

        if self.include_previous_observations {
            let previous_observations = select_previous_observations(trace, observation);
            if !previous_observations.is_empty() {
                let mut lines = vec![String::new(), "## Previous Observations".to_string()];
                for item in previous_observations {
                    lines.push(format!(
                        "- step={} source={} chars={} summary={}",
                        item.step, item.source, item.content_chars, item.summary
                    ));
                }
                pack.push(ContextSection::new(
                    ContextSectionKind::PreviousObservations,
                    lines,
                ));
            }
        }

        if let Some(observation) = observation {
            let section_lines = build_latest_observation_lines(observation);
            pack.push(ContextSection::new(
                ContextSectionKind::LatestObservation,
                section_lines,
            ));
        }

        if !trace.active_plan_steps.is_empty() {
            let mut plan_lines = vec![
                String::new(),
                "## Active Plan".to_string(),
                format!(
                    "- current_step_index: {}",
                    trace
                        .current_step_index
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "(none)".to_string())
                ),
                format!(
                    "- replan_budget_remaining: {}",
                    trace.controller_state.remaining_replans()
                ),
            ];
            for (idx, step) in trace.active_plan_steps.iter().enumerate() {
                let mut line = format!(
                    "{}. [{}] {} (retry_count={})",
                    idx + 1,
                    step.status.as_str(),
                    step.description,
                    step.retry_count
                );
                if let Some(expected) = &step.expected_observation {
                    line.push_str(&format!(" | expect: {}", expected.summary()));
                }
                plan_lines.push(line);
            }
            if let Some(progress_note) = &trace.last_progress_note {
                plan_lines.push(format!("- progress_note: {}", progress_note));
            }
            pack.push(ContextSection::new(
                ContextSectionKind::RuntimePlan,
                plan_lines,
            ));
        }

        if let Some(business_context) = business_context {
            if let Some(task) = &business_context.current_task {
                let mut task_lines = vec![
                    String::new(),
                    "## Current Task".to_string(),
                    format!("- task_id: {}", task.task_id),
                    format!("- status: {}", task.status),
                    format!("- article_id: {}", task.article_id),
                    format!("- url: {}", task.normalized_url),
                    format!("- retry_count: {}", task.retry_count),
                ];
                if let Some(page_kind) = &task.page_kind {
                    task_lines.push(format!("- page_kind: {}", page_kind));
                }
                if let Some(content_source) = &task.content_source {
                    task_lines.push(format!("- content_source: {}", content_source));
                }
                if let Some(last_error) = &task.last_error {
                    task_lines.push(format!(
                        "- last_error: {}",
                        summarize_for_markdown(last_error, 200)
                    ));
                }
                pack.push(ContextSection::new(
                    ContextSectionKind::CurrentTask,
                    task_lines,
                ));
            }

            if !business_context.recent_tasks.is_empty() {
                let mut recent_lines = vec![String::new(), "## Recent Tasks".to_string()];
                for task in &business_context.recent_tasks {
                    recent_lines.push(format!(
                        "- task_id={} status={} page_kind={} url={}",
                        task.task_id,
                        task.status,
                        task.page_kind.as_deref().unwrap_or("(none)"),
                        task.normalized_url
                    ));
                }
                pack.push(ContextSection::new(
                    ContextSectionKind::RecentTasks,
                    recent_lines,
                ));
            }

            if !business_context.user_memories.is_empty() {
                let mut memory_lines = vec![String::new(), "## User Memories".to_string()];
                for memory in &business_context.user_memories {
                    memory_lines.push(format!(
                        "- {} (priority={}) {}",
                        memory.memory_type.label_prefix(),
                        memory.priority,
                        sanitize_for_prompt(&memory.content)
                    ));
                }
                pack.push(ContextSection::new(
                    ContextSectionKind::UserMemories,
                    memory_lines,
                ));
            }
        }

        let mut tool_lines = vec![String::new(), "## Available Tools".to_string()];
        tool_lines.extend(available_tools.iter().cloned());
        pack.push(ContextSection::new(
            ContextSectionKind::ToolDescriptions,
            tool_lines,
        ));

        pack.push(ContextSection::new(
            ContextSectionKind::ResponseContract,
            vec![
                String::new(),
                "你必须采用最小 ReAct 风格：根据当前上下文决定“继续调一个工具”或“直接结束”。请只输出 JSON，格式为 {\"action\":\"read|write|create|get_task_status|list_recent_tasks|list_manual_tasks|read_article_archive|final\",...,\"expected_fields\":[\"field_a\"],\"minimum_novelty\":\"different_from_last\"}。".to_string(),
            ],
        ));

        pack.apply_total_budget();
        pack
    }
}

/// Build a ContextPack from runtime state (public entry point).
///
/// This is the single-entry API for constructing structured context packs.
/// All prompt assembly should go through this path.
fn build_context_pack(
    trace: &AgentRunTrace,
    user_input: &str,
    observation: Option<&AgentObservation>,
    runtime_session_state: Option<&RuntimeSessionStateSnapshot>,
    available_tools: &[String],
    business_context: Option<&BusinessContextSnapshot>,
    session_summary_strategy: SessionSummaryStrategy,
    include_previous_observations: bool,
) -> ContextPack {
    ContextAssembler {
        include_previous_observations,
    }
    .build_pack_with_summary_strategy(
        trace,
        user_input,
        observation,
        runtime_session_state,
        available_tools,
        business_context,
        session_summary_strategy,
    )
}

/// goal 信号来源，用于 low-signal 判定（不进 prompt，仅运行时标记）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum GoalSignal {
    /// 来自持久化 v2 goal（用户或之前 agent 显式写入）
    PersistentHigh,
    /// 来自持久化 last_user_intent 的 fallback
    PersistentFallback,
    /// 运行时默认模板（无历史状态时的兜底）
    #[default]
    RuntimeDefault,
}

/// 合并持久化数组字段与运行时推导数组，去重并裁剪长度。
///
/// 关键改进：保证 runtime 信号至少保留 `runtime_min_keep` 条，
/// 避免 persistent 项占满预算后 runtime 高价值信号全丢。
fn merge_string_arrays_with_runtime_reserve(
    persistent: Vec<String>,
    runtime: Vec<String>,
    max_total: usize,
    runtime_min_keep: usize,
) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut merged = Vec::new();

    // 第 1 步：去重收集 runtime 项（内部去重）
    let mut runtime_unique = Vec::new();
    for item in runtime {
        let key = item.trim().to_lowercase();
        if key.is_empty() || seen.contains(&key) {
            continue;
        }
        seen.insert(key);
        runtime_unique.push(item);
    }

    // 第 2 步：保底保留 runtime_min_keep 条 runtime 信号
    let runtime_keep = runtime_min_keep.min(runtime_unique.len());
    for item in runtime_unique.drain(..runtime_keep) {
        merged.push(item);
    }

    // 第 3 步：填充 persistent 项（已去重于 runtime）
    for item in persistent {
        let key = item.trim().to_lowercase();
        if key.is_empty() || seen.contains(&key) {
            continue;
        }
        seen.insert(key);
        merged.push(item);
    }

    // 第 4 步：若仍有空间，补充剩余 runtime 项
    for item in runtime_unique {
        if merged.len() >= max_total {
            break;
        }
        merged.push(item);
    }

    // 第 5 步：兜底截断
    merged.truncate(max_total);
    merged
}

fn derive_runtime_session_state(
    trace: &AgentRunTrace,
    user_input: &str,
    observation: Option<&AgentObservation>,
    business_context: Option<&BusinessContextSnapshot>,
) -> RuntimeSessionStateSnapshot {
    let persistent_session_state = trace.user_session_state.as_ref();
    let current_step = trace.active_plan_steps.iter().find(|step| {
        matches!(
            step.status,
            PlanStepStatus::Running | PlanStepStatus::Pending | PlanStepStatus::Failed
        )
    });
    let runtime_done_items = trace
        .active_plan_steps
        .iter()
        .filter(|step| step.status == PlanStepStatus::Done)
        .map(|step| step.description.clone())
        .take(3)
        .collect::<Vec<_>>();

    // goal: 优先从持久化 v2 goal，fallback 到 last_user_intent / 运行时推导
    // 同时记录来源标记，供 is_low_signal 判定
    let (goal, goal_signal) =
        if let Some(g) = persistent_session_state.and_then(|pss| pss.goal.clone()) {
            (Some(g), GoalSignal::PersistentHigh)
        } else if let Some(g) = persistent_session_state.and_then(|pss| {
            pss.last_user_intent.as_ref().map(|intent| {
                format!(
                    "响应当前用户请求（基于历史意图）：{}",
                    summarize_for_markdown(intent, 120)
                )
            })
        }) {
            (Some(g), GoalSignal::PersistentFallback)
        } else if let Some(task) = business_context.and_then(|ctx| ctx.current_task.as_ref()) {
            (
                Some(format!("推进任务 {} 到下一可收敛状态", task.task_id)),
                GoalSignal::PersistentFallback,
            )
        } else if let Some(current_step) = current_step {
            (
                Some(format!("完成当前计划：{}", current_step.description)),
                GoalSignal::PersistentFallback,
            )
        } else {
            (
                Some(format!(
                    "响应当前用户请求：{}",
                    summarize_for_markdown(user_input.trim(), 120)
                )),
                GoalSignal::RuntimeDefault,
            )
        };

    // current_subtask: 优先从持久化 v2，fallback 到 current_task / 运行时推导
    let current_subtask = persistent_session_state
        .and_then(|pss| pss.current_subtask.clone())
        .or_else(|| {
            persistent_session_state.and_then(|pss| {
                pss.current_task
                    .as_ref()
                    .map(|t| format!("当前关注任务: {}", t))
            })
        })
        .or_else(|| current_step.map(|step| step.description.clone()))
        .or_else(|| {
            business_context
                .and_then(|ctx| ctx.current_task.as_ref())
                .map(|task| format!("处理当前任务状态 {}", task.status))
        });

    // constraints: 合并持久化 + 运行时推导，去重，最多 4 条，runtime 至少保底 1 条
    let persistent_constraints = persistent_session_state
        .map(|pss| pss.constraints())
        .unwrap_or_default();
    let mut runtime_constraints = Vec::new();
    if let Some(pss) = persistent_session_state {
        if let Some(ref reason) = pss.blocked_reason {
            runtime_constraints.push(format!("当前阻塞原因（来自历史状态）：{}", reason));
        }
    }
    if trace.controller_state.remaining_replans() == 0 {
        runtime_constraints.push("replan budget 已耗尽，应优先收敛或 ask_user".to_string());
    }
    if trace.controller_state.failure_count > 0 {
        runtime_constraints.push(format!(
            "已有 {} 次失败，避免重复低价值动作",
            trace.controller_state.failure_count
        ));
    }
    if let Some(task) = business_context.and_then(|ctx| ctx.current_task.as_ref()) {
        if task.status == "awaiting_manual_input" {
            runtime_constraints.push("当前任务等待人工补录，不能假装正文已可用".to_string());
        }
    }
    let constraints =
        merge_string_arrays_with_runtime_reserve(persistent_constraints, runtime_constraints, 4, 1);

    // confirmed_facts: 合并持久化 + 运行时推导，去重，最多 5 条，runtime 至少保底 1 条
    let persistent_facts = persistent_session_state
        .map(|pss| pss.confirmed_facts())
        .unwrap_or_default();
    let mut runtime_facts = Vec::new();
    if let Some(task) = business_context.and_then(|ctx| ctx.current_task.as_ref()) {
        runtime_facts.push(format!(
            "current_task={} status={}",
            task.task_id, task.status
        ));
        if let Some(page_kind) = &task.page_kind {
            runtime_facts.push(format!("page_kind={}", page_kind));
        }
    }
    if let Some(observation) = observation {
        runtime_facts.push(format!(
            "latest_observation={} {}",
            observation.source,
            truncate_for_trace(&observation.content, 80)
        ));
    }
    if trace.memory_hit_count > 0 {
        runtime_facts.push(format!("memory_injected={}", trace.memory_hit_count));
    }
    let confirmed_facts =
        merge_string_arrays_with_runtime_reserve(persistent_facts, runtime_facts, 5, 1);

    // next_step: 优先从持久化 v2 next_step，fallback 到现有推导
    let next_step = persistent_session_state
        .and_then(|pss| pss.next_step.clone())
        .or_else(|| current_step.map(|step| step.description.clone()))
        .or_else(|| {
            if observation.is_some() {
                Some("基于 latest observation 判断是继续调工具还是直接结束".to_string())
            } else {
                None
            }
        })
        .or_else(|| {
            business_context
                .and_then(|ctx| ctx.current_task.as_ref())
                .and_then(|task| {
                    if task.status == "awaiting_manual_input" {
                        Some("向用户请求补录内容或引导 retry".to_string())
                    } else {
                        None
                    }
                })
        });

    // done_items: 合并持久化 + 运行时推导，去重，最多 3 条，runtime 不保底
    let persistent_done = persistent_session_state
        .map(|pss| pss.done_items())
        .unwrap_or_default();
    let done_items =
        merge_string_arrays_with_runtime_reserve(persistent_done, runtime_done_items, 3, 0);

    // open_questions: 合并持久化 + 运行时推导，去重，最多 3 条，runtime 至少保底 1 条
    let persistent_questions = persistent_session_state
        .map(|pss| pss.open_questions())
        .unwrap_or_default();
    let mut runtime_questions = Vec::new();
    if let Some(task) = business_context.and_then(|ctx| ctx.current_task.as_ref()) {
        if let Some(last_error) = &task.last_error {
            runtime_questions.push(format!(
                "是否需要处理最近错误：{}",
                summarize_for_markdown(last_error, 120)
            ));
        }
    }
    if let Some(current_step) = current_step {
        if let Some(expected) = &current_step.expected_observation {
            runtime_questions.push(format!(
                "当前 step 期待的 observation 是否已满足：{}",
                expected.summary()
            ));
        }
    }
    let open_questions =
        merge_string_arrays_with_runtime_reserve(persistent_questions, runtime_questions, 3, 1);

    RuntimeSessionStateSnapshot {
        goal,
        current_subtask,
        constraints,
        confirmed_facts,
        done_items,
        next_step,
        open_questions,
        goal_signal,
    }
}

#[derive(Debug)]
pub struct AgentRunResult {
    pub output: String,
    pub run_id: String,
    pub trace_json_path: Option<PathBuf>,
}

/// 空检索器 —— 当没有 task_store_db_path 时的 fallback。
/// 永远返回空结果，零延迟。
#[derive(Debug)]
struct NoOpRetriever;

impl crate::retriever::Retriever for NoOpRetriever {
    fn retrieve(
        &self,
        _query: &crate::retriever::RetrieveQuery,
    ) -> anyhow::Result<crate::retriever::RetrieveResult> {
        Ok(crate::retriever::RetrieveResult::empty("noop"))
    }
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
        )
    }

    fn with_max_steps_and_task_store_db_path_with_compaction_and_retriever_mode(
        workspace_root: impl Into<PathBuf>,
        max_steps: usize,
        task_store_db_path: Option<impl Into<PathBuf>>,
        context_compaction: ContextCompactionConfig,
        retriever_mode: Option<&str>,
        embedding_provider: Option<&str>,
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

        let mode = match retriever_mode {
            Some(text) => RetrieverMode::from_config(text)?,
            None => RetrieverMode::Rule,
        };
        let embedding_provider_name = embedding_provider.unwrap_or("noop");
        let retriever =
            select_retriever(mode, task_store_db_path.as_deref(), embedding_provider_name);

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
        let llm_call = trace.start_llm_call(config, system_prompt, planner_input, &body);
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
    controller_state: RuntimeControllerState,
    current_step_index: Option<usize>,
    step_count: usize,
    workspace_root: String,
    llm_fallback_reason: Option<String>,
    rule_parse_error: Option<String>,
    recovery_action: Option<FailureAction>,
    recovery_result: Option<RecoveryOutcome>,
    recovery_attempts: Vec<RecoveryTrace>,
    decisions: Vec<DecisionTrace>,
    observations: Vec<ObservationTrace>,
    failures: Vec<FailureTrace>,
    active_plan_steps: Vec<RuntimePlanStep>,
    pending_replan_scope: Option<ReplanScope>,
    last_progress_note: Option<String>,
    llm_calls: Vec<LlmCallTrace>,
    tool_calls: Vec<ToolCallTrace>,
    session_state_snapshot: Option<RuntimeSessionStateSnapshot>,
    memory_hit_count: usize,       // 实际注入 prompt 的记忆条数
    memory_retrieved_count: usize, // 从 DB 取出的候选记忆条数
    memory_total_chars: usize,     // 注入记忆的总字符数
    memory_dropped_count: usize,   // 被裁剪掉的记忆条数
    memory_ids: Vec<String>,       // 注入记忆的 ID 列表
    // --- Retriever-level observability ---
    retriever_name: String,                    // 使用的检索器名称
    retrieval_candidate_count: usize,          // 检索器返回的候选条数
    retrieval_hit_count: usize,                // 经裁剪后实际命中的条数
    retrieval_latency_ms: u128,                // 检索耗时（毫秒）
    retrieval_mode: String,                    // 检索模式（rule/hybrid/semantic/shadow）
    retrieval_fallback_reason: Option<String>, // 回退原因
    retrieval_scores_present: bool,            // 是否包含语义分数
    persistent_state_present: bool,
    persistent_state_source: Option<String>,
    persistent_state_updated: bool,
    persistent_state_slot_count: usize,
    persistent_state_preview: Option<String>,
    // --- ContextPack-level observability (C3/C4) ---
    context_pack_present: bool,
    context_pack_section_count: usize,
    context_pack_total_chars: usize,
    context_pack_drop_reasons: Vec<String>,
    #[serde(skip_serializing)]
    user_session_state: Option<UserSessionStateRecord>,
    #[serde(skip_serializing)]
    trace_dir_root: PathBuf,
}

#[derive(Debug, Serialize)]
struct DecisionTrace {
    step: usize,
    current_step_index: Option<usize>,
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
    context_sections: Vec<ContextSectionSnapshot>,
    context_budget_summary: ContextBudgetSummary,
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
    current_step_index: Option<usize>,
    kind: StepFailureKind,
    action: FailureAction,
    replan_scope: Option<ReplanScope>,
    source: String,
    detail: String,
    user_message: Option<String>,
}

#[derive(Debug, Serialize)]
struct RecoveryTrace {
    step: usize,
    current_step_index: Option<usize>,
    failure_kind: StepFailureKind,
    /// 映射前原始 action（failure 自带或映射表默认值）
    original_action: FailureAction,
    /// 实际执行的 action（可能因防循环升级）
    effective_action: FailureAction,
    /// 兼容旧消费方，保留 action 字段（值同 effective_action）
    action: FailureAction,
    outcome: RecoveryOutcome,
    successful: bool,
    /// 本次恢复是否因防循环保护而被升级
    escalated: bool,
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
    memory_hit_count: usize,
    memory_retrieved_count: usize,
    memory_total_chars: usize,
    memory_dropped_count: usize,
    #[serde(default)]
    recovery_attempt_count: usize,
    #[serde(default)]
    recovery_success_count: usize,
    #[serde(default)]
    recovery_action: Option<String>,
    #[serde(default)]
    recovery_result: Option<String>,
    #[serde(default)]
    retriever_name: String,
    #[serde(default)]
    retrieval_candidate_count: usize,
    #[serde(default)]
    retrieval_hit_count: usize,
    #[serde(default)]
    retrieval_latency_ms: u128,
    #[serde(default)]
    retrieval_mode: String,
    #[serde(default)]
    retrieval_fallback_reason: Option<String>,
    #[serde(default)]
    retrieval_scores_present: bool,
    context_pack_present: bool,
    context_pack_section_count: usize,
    context_pack_total_chars: usize,
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
            controller_state: RuntimeControllerState::new(DEFAULT_MAX_STEPS, DEFAULT_MAX_REPLANS),
            current_step_index: None,
            step_count: 0,
            workspace_root: workspace_root.display().to_string(),
            llm_fallback_reason: None,
            rule_parse_error: None,
            recovery_action: None,
            recovery_result: None,
            recovery_attempts: Vec::new(),
            decisions: Vec::new(),
            observations: Vec::new(),
            failures: Vec::new(),
            active_plan_steps: Vec::new(),
            pending_replan_scope: None,
            last_progress_note: None,
            llm_calls: Vec::new(),
            tool_calls: Vec::new(),
            session_state_snapshot: None,
            trace_dir_root: workspace_root.join("data").join("agent_traces"),
            memory_hit_count: 0,
            memory_retrieved_count: 0,
            memory_total_chars: 0,
            memory_dropped_count: 0,
            memory_ids: Vec::new(),
            retriever_name: String::new(),
            retrieval_candidate_count: 0,
            retrieval_hit_count: 0,
            retrieval_latency_ms: 0,
            retrieval_mode: String::new(),
            retrieval_fallback_reason: None,
            retrieval_scores_present: false,
            persistent_state_present: context.user_session_state.is_some(),
            persistent_state_source: if context.user_session_state.is_some() {
                Some("db".to_string())
            } else {
                None
            },
            persistent_state_updated: false,
            persistent_state_slot_count: context
                .user_session_state
                .as_ref()
                .map(|s| s.populated_slot_count())
                .unwrap_or(0),
            persistent_state_preview: context
                .user_session_state
                .as_ref()
                .map(|s| {
                    let mut parts = Vec::new();
                    if let Some(g) = &s.goal {
                        parts.push(format!("goal={}", summarize_for_markdown(g, 40)));
                    }
                    if let Some(st) = &s.current_subtask {
                        parts.push(format!("subtask={}", summarize_for_markdown(st, 40)));
                    }
                    if s.next_step.is_some() {
                        parts.push("has_next_step".to_string());
                    }
                    parts.join(", ")
                })
                .filter(|p| !p.is_empty()),
            context_pack_present: false,
            context_pack_section_count: 0,
            context_pack_total_chars: 0,
            context_pack_drop_reasons: Vec::new(),
            user_session_state: context.user_session_state,
        }
    }

    fn record_decision(&mut self, step: usize, source: &str, planned: &PlannedDecision) {
        if let Some(plan) = &planned.plan {
            let scope = self
                .pending_replan_scope
                .take()
                .unwrap_or(ReplanScope::Full);
            self.apply_plan_update(&plan.steps, scope);
        }
        if let Some(progress_note) = &planned.progress_note {
            self.last_progress_note = Some(progress_note.clone());
        }
        self.decisions.push(DecisionTrace {
            step,
            current_step_index: self.current_step_index,
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
        self.sync_current_step_index();
    }

    fn mark_running_plan_step_done(&mut self) {
        if let Some(step) = self
            .active_plan_steps
            .iter_mut()
            .find(|step| step.status == PlanStepStatus::Running)
        {
            step.status = PlanStepStatus::Done;
        }
        self.sync_current_step_index();
    }

    fn mark_running_plan_step_failed(&mut self) {
        if let Some(step) = self
            .active_plan_steps
            .iter_mut()
            .find(|step| step.status == PlanStepStatus::Running)
        {
            step.status = PlanStepStatus::Failed;
        }
        self.sync_current_step_index();
    }

    fn mark_running_plan_step_retrying(&mut self) -> usize {
        let Some(step) = self
            .active_plan_steps
            .iter_mut()
            .find(|step| step.status == PlanStepStatus::Running)
        else {
            return 0;
        };
        step.retry_count += 1;
        let retry_count = step.retry_count;
        self.sync_current_step_index();
        retry_count
    }

    fn mark_remaining_plan_steps_skipped(&mut self) {
        for step in &mut self.active_plan_steps {
            if step.status == PlanStepStatus::Pending {
                step.status = PlanStepStatus::Skipped;
            }
        }
        self.sync_current_step_index();
    }

    fn has_incomplete_plan_steps(&self) -> bool {
        self.active_plan_steps.iter().any(|step| {
            matches!(
                step.status,
                PlanStepStatus::Pending | PlanStepStatus::Running
            )
        })
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

    fn record_session_state_snapshot(&mut self, snapshot: RuntimeSessionStateSnapshot) {
        self.session_state_snapshot = Some(snapshot);
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
        if failure.action == FailureAction::Replan {
            self.pending_replan_scope = failure.replan_scope.clone();
        }
        self.controller_state.record_failure();
        self.failures.push(FailureTrace {
            step,
            current_step_index: self.current_step_index,
            kind: failure.kind,
            action: failure.action,
            replan_scope: failure.replan_scope.clone(),
            source: failure.source.clone(),
            detail: failure.detail.clone(),
            user_message: failure.user_message.clone(),
        });
    }

    fn record_recovery_attempt(
        &mut self,
        step: usize,
        failure: &FailureDecision,
        outcome: RecoveryOutcome,
        escalated: bool,
        effective_action: FailureAction,
    ) {
        self.recovery_action = Some(effective_action);
        self.recovery_result = Some(outcome);
        self.recovery_attempts.push(RecoveryTrace {
            step,
            current_step_index: self.current_step_index,
            failure_kind: failure.kind,
            original_action: failure.action,
            effective_action,
            action: effective_action,
            successful: outcome == RecoveryOutcome::Continued,
            outcome,
            escalated,
            source: failure.source.clone(),
            detail: failure.detail.clone(),
        });
    }

    fn apply_plan_update(&mut self, steps: &[String], scope: ReplanScope) {
        let new_steps = steps
            .iter()
            .map(|description| RuntimePlanStep {
                description: description.clone(),
                status: PlanStepStatus::Pending,
                expected_observation: None,
                retry_count: 0,
            })
            .collect::<Vec<_>>();

        match scope {
            ReplanScope::Full => {
                self.active_plan_steps = new_steps;
            }
            ReplanScope::RemainingPlan => {
                let mut preserved = self
                    .active_plan_steps
                    .iter()
                    .filter(|step| step.status == PlanStepStatus::Done)
                    .cloned()
                    .collect::<Vec<_>>();
                preserved.extend(new_steps);
                self.active_plan_steps = preserved;
            }
            ReplanScope::CurrentStep => {
                let split_index = self
                    .active_plan_steps
                    .iter()
                    .position(|step| step.status != PlanStepStatus::Done)
                    .unwrap_or(self.active_plan_steps.len());
                let mut rebuilt = self.active_plan_steps[..split_index].to_vec();
                rebuilt.extend(new_steps);
                if split_index < self.active_plan_steps.len() {
                    let preserved_tail = self.active_plan_steps[(split_index + 1)..]
                        .iter()
                        .map(|step| RuntimePlanStep {
                            description: step.description.clone(),
                            status: PlanStepStatus::Pending,
                            expected_observation: None,
                            retry_count: 0,
                        })
                        .collect::<Vec<_>>();
                    rebuilt.extend(preserved_tail);
                }
                self.active_plan_steps = rebuilt;
            }
        }
        self.sync_current_step_index();
    }

    fn sync_current_step_index(&mut self) {
        self.current_step_index = self
            .active_plan_steps
            .iter()
            .position(|step| {
                matches!(
                    step.status,
                    PlanStepStatus::Pending | PlanStepStatus::Running | PlanStepStatus::Failed
                )
            })
            .map(|index| index + 1);
    }

    fn consecutive_failures_for_current_step(
        &self,
        current_step_index: usize,
    ) -> Vec<&FailureTrace> {
        let mut failures = Vec::new();
        for failure in self.failures.iter().rev() {
            if failure.current_step_index != Some(current_step_index) {
                break;
            }
            failures.push(failure);
        }
        failures
    }

    fn configure_controller_limits(&mut self, max_steps: usize, max_replans: usize) {
        self.controller_state
            .configure_limits(max_steps, max_replans);
    }

    fn start_llm_call(
        &mut self,
        config: &LlmConfig,
        system_prompt: &str,
        planner_input: &PlannerInput,
        body: &serde_json::Value,
    ) -> usize {
        let request_body = serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string());
        self.llm_calls.push(LlmCallTrace {
            source: config.source.to_string(),
            model: config.model.clone(),
            base_url: config.base_url.clone(),
            prompt: PromptSnapshot {
                system_prompt: system_prompt.to_string(),
                raw_user_input: planner_input.raw_user_input.clone(),
                user_prompt: planner_input.assembled_user_prompt.clone(),
                context_sections: planner_input.context_sections.clone(),
                context_budget_summary: planner_input.context_budget_summary.clone(),
                context_summary: planner_input.context_summary.clone(),
                system_prompt_chars: system_prompt.chars().count(),
                raw_user_input_chars: planner_input.raw_user_input.chars().count(),
                user_prompt_chars: planner_input.assembled_user_prompt.chars().count(),
                context_summary_chars: planner_input.context_summary.chars().count(),
                request_body_chars: request_body.chars().count(),
                estimated_prompt_chars: system_prompt.chars().count()
                    + planner_input.assembled_user_prompt.chars().count()
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

    fn persist(&self) -> Result<PathBuf> {
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
            memory_hit_count: self.memory_hit_count,
            memory_retrieved_count: self.memory_retrieved_count,
            memory_total_chars: self.memory_total_chars,
            memory_dropped_count: self.memory_dropped_count,
            recovery_attempt_count: self.recovery_attempts.len(),
            recovery_success_count: self
                .recovery_attempts
                .iter()
                .filter(|attempt| attempt.successful)
                .count(),
            recovery_action: self
                .recovery_action
                .as_ref()
                .map(|action| action.as_str().to_string()),
            recovery_result: self
                .recovery_result
                .as_ref()
                .map(|result| result.as_str().to_string()),
            retriever_name: self.retriever_name.clone(),
            retrieval_candidate_count: self.retrieval_candidate_count,
            retrieval_hit_count: self.retrieval_hit_count,
            retrieval_latency_ms: self.retrieval_latency_ms,
            retrieval_mode: self.retrieval_mode.clone(),
            retrieval_fallback_reason: self.retrieval_fallback_reason.clone(),
            retrieval_scores_present: self.retrieval_scores_present,
            context_pack_present: self.context_pack_present,
            context_pack_section_count: self.context_pack_section_count,
            context_pack_total_chars: self.context_pack_total_chars,
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
        Ok(json_path)
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
            format!(
                "- replan_budget: {}/{}",
                self.controller_state.replan_count, self.controller_state.max_replans
            ),
            format!("- failure_count: {}", self.controller_state.failure_count),
            format!("- ask_user_count: {}", self.controller_state.ask_user_count),
            format!("- recovery_attempt_count: {}", self.recovery_attempts.len()),
            format!(
                "- recovery_success_count: {}",
                self.recovery_attempts
                    .iter()
                    .filter(|attempt| attempt.successful)
                    .count()
            ),
            format!(
                "- recovery_action(last): {}",
                self.recovery_action
                    .as_ref()
                    .map(FailureAction::as_str)
                    .unwrap_or("(none)")
            ),
            format!(
                "- recovery_result(last): {}",
                self.recovery_result
                    .as_ref()
                    .map(RecoveryOutcome::as_str)
                    .unwrap_or("(none)")
            ),
            format!("- memory_hit_count: {} (injected)", self.memory_hit_count),
            format!("- memory_retrieved_count: {}", self.memory_retrieved_count),
            format!("- memory_dropped_count: {}", self.memory_dropped_count),
            format!(
                "- memory_total_chars: {} (injected)",
                self.memory_total_chars
            ),
            format!(
                "- retriever: {} (latency={}ms, candidates={}, hits={})",
                if self.retriever_name.is_empty() {
                    "(none)"
                } else {
                    &self.retriever_name
                },
                self.retrieval_latency_ms,
                self.retrieval_candidate_count,
                self.retrieval_hit_count,
            ),
            format!(
                "- retrieval_mode: {}{}",
                if self.retrieval_mode.is_empty() {
                    "(none)"
                } else {
                    &self.retrieval_mode
                },
                if let Some(ref reason) = self.retrieval_fallback_reason {
                    format!(" [fallback: {}]", reason)
                } else {
                    String::new()
                }
            ),
            format!(
                "- retrieval_scores_present: {}",
                self.retrieval_scores_present
            ),
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

        if let Some(session_state_snapshot) = &self.session_state_snapshot {
            append_session_state_lines(&mut lines, session_state_snapshot);
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
                    "- step={} current_step={} source={} type={} summary={}",
                    decision.step,
                    decision
                        .current_step_index
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "(none)".to_string()),
                    decision.source,
                    decision.decision_type,
                    decision.summary
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
            lines.push(format!(
                "- current_step_index: {}",
                self.current_step_index
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "(none)".to_string())
            ));
            for (idx, step) in self.active_plan_steps.iter().enumerate() {
                let mut line = format!(
                    "{}. [{}] {} (retry_count={})",
                    idx + 1,
                    step.status.as_str(),
                    step.description,
                    step.retry_count
                );
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
                    "- context_max_chars: {}",
                    call.prompt.context_budget_summary.max_total_chars
                ));
                lines.push(format!(
                    "- context_final_chars: {}",
                    call.prompt.context_budget_summary.final_total_chars
                ));
                lines.push(format!(
                    "- trimmed_section_count: {}",
                    call.prompt.context_budget_summary.trimmed_section_count
                ));
                lines.push(format!(
                    "- dropped_section_count: {}",
                    call.prompt.context_budget_summary.dropped_section_count
                ));
                lines.push(format!(
                    "- context_section_count: {}",
                    call.prompt.context_sections.len()
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
                lines.push(String::new());
                lines.push("#### Context Sections".to_string());
                lines.push(String::new());
                if call.prompt.context_sections.is_empty() {
                    lines.push("- (none)".to_string());
                } else {
                    for section in &call.prompt.context_sections {
                        lines.push(format!(
                        "- kind={} priority={} included={} trimmed={} lines={} items={} chars={}/{}",
                        section.kind,
                        section.priority,
                        section.included,
                        section.trimmed,
                        section.line_count,
                        section.item_count,
                        section.char_count,
                        section.original_char_count
                    ));
                        if let Some(reason) = &section.trim_reason {
                            lines.push(format!("  - trim_reason: {}", reason.as_str()));
                        }
                        if let Some(reason) = &section.drop_reason {
                            lines.push(format!("  - drop_reason: {}", reason.as_str()));
                        }
                        lines.push("```text".to_string());
                        lines.push(summarize_for_markdown(&section.content, 400));
                        lines.push("```".to_string());
                    }
                }
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
                    "- step={} current_step={} kind={} action={} scope={} source={} detail={}",
                    failure.step,
                    failure
                        .current_step_index
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "(none)".to_string()),
                    failure.kind.as_str(),
                    failure.action.as_str(),
                    failure
                        .replan_scope
                        .as_ref()
                        .map(ReplanScope::as_str)
                        .unwrap_or("(none)"),
                    failure.source,
                    failure.detail
                ));
                if let Some(user_message) = &failure.user_message {
                    lines.push(format!("  - user_message: {}", user_message));
                }
            }
        }

        lines.push(String::new());
        lines.push("## Recovery Attempts".to_string());
        lines.push(String::new());
        if self.recovery_attempts.is_empty() {
            lines.push("- (none)".to_string());
        } else {
            for attempt in &self.recovery_attempts {
                lines.push(format!(
                    "- step={} current_step={} kind={} action={} outcome={} successful={} source={} detail={}",
                    attempt.step,
                    attempt
                        .current_step_index
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "(none)".to_string()),
                    attempt.failure_kind.as_str(),
                    attempt.action.as_str(),
                    attempt.outcome.as_str(),
                    attempt.successful,
                    attempt.source,
                    attempt.detail
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

/// 清理注入 prompt 的用户内容：移除控制字符，截断超长内容
fn sanitize_for_prompt(input: &str) -> String {
    let cleaned: String = input
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    // 单条 memory 不超过 160 字符（与 search_user_memories 预算一致）
    if cleaned.chars().count() > 160 {
        let truncated: String = cleaned.chars().take(157).collect();
        format!("{truncated}...")
    } else {
        cleaned
    }
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

fn project_session_state_to_trace(trace: &mut AgentRunTrace, session_state: &SessionState) {
    if session_state.has_memory_activity() {
        trace.memory_hit_count = session_state.injected_count();
        trace.memory_retrieved_count = session_state.retrieved_count();
        trace.memory_total_chars = session_state.injected_total_chars();
        trace.memory_dropped_count = session_state.dropped.len();
        trace.memory_ids = session_state.injected_ids();
    }
    if session_state.has_retrieval_observability() {
        trace
            .retriever_name
            .clone_from(&session_state.retriever_name);
        trace.retrieval_latency_ms = session_state.retrieval_latency_ms;
        trace.retrieval_candidate_count = session_state.retrieval_candidate_count;
        trace.retrieval_hit_count = session_state.retrieval_hit_count;
        trace
            .retrieval_mode
            .clone_from(&session_state.retrieval_mode);
        trace.retrieval_fallback_reason = session_state.retrieval_fallback_reason.clone();
        trace.retrieval_scores_present = session_state.retrieval_scores_present;
    }
}

/// 将 RetrievedItem 映射为 UserMemoryRecord，供现有 SessionState / prompt 链路零回归使用。
fn retrieved_item_to_user_memory_record(
    item: &crate::retriever::RetrievedItem,
    user_id: &str,
) -> UserMemoryRecord {
    let meta = &item.metadata;
    let parse_i64 = |key: &str| meta.get(key).and_then(|value| value.parse().ok());
    let parse_bool = |key: &str| {
        meta.get(key)
            .map(|value| value.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    };
    let memory_type = meta
        .get("memory_type")
        .or(Some(&item.source_type))
        .and_then(|s| s.parse().ok())
        .unwrap_or(crate::task_store::MemoryType::Auto);
    let priority = meta
        .get("priority")
        .and_then(|s| s.parse().ok())
        .unwrap_or(memory_type.default_priority());
    let status = meta
        .get("status")
        .cloned()
        .unwrap_or_else(|| "active".to_string());
    let use_count = parse_i64("use_count").unwrap_or(0);
    let retrieved_count = parse_i64("retrieved_count").unwrap_or(0);
    let injected_count = parse_i64("injected_count").unwrap_or(0);
    let useful = parse_bool("useful");
    let now = chrono::Utc::now().to_rfc3339();
    let created_at = meta
        .get("created_at")
        .cloned()
        .or_else(|| meta.get("updated_at").cloned())
        .unwrap_or_else(|| now.clone());
    let updated_at = meta
        .get("updated_at")
        .cloned()
        .or_else(|| meta.get("created_at").cloned())
        .unwrap_or_else(|| now.clone());
    UserMemoryRecord {
        id: item.id.clone(),
        user_id: user_id.to_string(),
        content: item.content.clone(),
        memory_type,
        status,
        priority,
        last_used_at: meta.get("last_used_at").cloned(),
        use_count,
        retrieved_count,
        injected_count,
        useful,
        created_at,
        updated_at,
    }
}

fn load_business_context_snapshot(
    task_store_db_path: Option<&std::path::Path>,
    trace: &AgentRunTrace,
    memory_budget: MemoryBudget,
    retriever: &dyn crate::retriever::Retriever,
    apply_feedback: bool,
) -> Result<(Option<BusinessContextSnapshot>, SessionState)> {
    let Some(db_path) = task_store_db_path else {
        return Ok((None, SessionState::default()));
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

    let mut session_state = SessionState::default();
    let user_memories = if let Some(user_id) = &trace.user_id {
        let query = crate::retriever::RetrieveQuery::new(user_id, 15)
            .with_query_text(&trace.user_input)
            .with_hint(
                "has_current_task",
                if current_task.is_some() {
                    "true"
                } else {
                    "false"
                },
            )
            .with_hint("plan_step_count", trace.active_plan_steps.len().to_string())
            .with_hint("source_type", &trace.source_type);

        // query_text 为空时打日志，方便排查 hybrid/semantic 回退原因
        if trace.user_input.trim().is_empty() {
            log_agent_info(
                "retrieve_query_text_empty",
                vec![
                    ("user_id", json!(user_id)),
                    ("source_type", json!(&trace.source_type)),
                    (
                        "reason",
                        json!("user_input is empty, semantic/hybrid may fallback to rule"),
                    ),
                ],
            );
        }

        match retriever.retrieve(&query) {
            Ok(retrieve_result) => {
                let has_current_task = current_task.is_some();
                let plan_step_count = trace.active_plan_steps.len();
                let budget =
                    memory_budget.with_dynamic_adjustment(has_current_task, plan_step_count);
                let retrieved: Vec<UserMemoryRecord> = retrieve_result
                    .candidates
                    .iter()
                    .map(|item| retrieved_item_to_user_memory_record(item, user_id))
                    .collect();
                session_state = SessionState::from_retrieved(retrieved, budget);
                session_state.retriever_name = retrieve_result.retriever_name.clone();
                session_state.retrieval_latency_ms = retrieve_result.latency_ms;
                session_state.retrieval_candidate_count = session_state.retrieved_count();
                session_state.retrieval_hit_count = session_state.injected_count();
                // 从候选 metadata 中提取 retrieval_mode 和 fallback_reason
                let first_mode = retrieve_result
                    .candidates
                    .first()
                    .and_then(|item| item.metadata.get("retrieval_mode"))
                    .cloned()
                    .unwrap_or_default();
                let fallback_reason = retrieve_result
                    .candidates
                    .first()
                    .and_then(|item| item.metadata.get("fallback_reason"))
                    .cloned();
                let scores_present = retrieve_result.candidates.iter().any(|item| {
                    item.metadata.contains_key("semantic_score")
                        || item.metadata.contains_key("final_score")
                });
                session_state.retrieval_mode = first_mode;
                session_state.retrieval_fallback_reason = fallback_reason;
                session_state.retrieval_scores_present = scores_present;
                let mut feedback_state = MemoryFeedbackState::default();
                for memory in &session_state.retrieved {
                    feedback_state.record(&memory.id, FeedbackKind::Retrieved);
                }
                for memory in &session_state.injected {
                    feedback_state.record(&memory.id, FeedbackKind::Injected);
                }
                // 日志投影
                log_agent_info(
                    "agent_memory_lifecycle",
                    vec![
                        ("user_id", json!(user_id)),
                        (
                            "memory_retrieved_count",
                            json!(session_state.retrieved_count()),
                        ),
                        (
                            "memory_injected_count",
                            json!(session_state.injected_count()),
                        ),
                        ("memory_dropped_count", json!(session_state.dropped.len())),
                        (
                            "memory_total_chars",
                            json!(session_state.injected_total_chars()),
                        ),
                        ("memory_ids", json!(session_state.injected_ids())),
                        ("retriever_name", json!(retrieve_result.retriever_name)),
                        (
                            "retrieval_candidate_count",
                            json!(session_state.retrieval_candidate_count),
                        ),
                        (
                            "retrieval_hit_count",
                            json!(session_state.retrieval_hit_count),
                        ),
                        ("retrieval_latency_ms", json!(retrieve_result.latency_ms)),
                    ],
                );
                if apply_feedback {
                    if let Err(err) = store.apply_memory_feedback(&feedback_state) {
                        log_agent_warn(
                            "agent_memory_feedback_apply_failed",
                            vec![
                                ("user_id", json!(user_id)),
                                ("detail", json!(err.to_string())),
                            ],
                        );
                    }
                }
                session_state.injected.clone()
            }
            Err(err) => {
                log_agent_warn(
                    "agent_memory_search_failed",
                    vec![
                        ("user_id", json!(user_id)),
                        ("detail", json!(err.to_string())),
                    ],
                );
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    Ok((
        Some(BusinessContextSnapshot {
            current_task,
            recent_tasks,
            user_memories,
        }),
        session_state,
    ))
}

/// 根据 ObservationKind 类型感知地构建 LatestObservation section 的行。
/// ArchiveContent 大幅压缩（摘要 + 引用），TaskList 保留总数 + 前 N 项，
/// 其余类型保持全文。
fn build_latest_observation_lines(observation: &AgentObservation) -> Vec<String> {
    let section_max = ContextSectionKind::LatestObservation.policy().max_chars;
    let step_line = format!("- step: {}", observation.step);
    let source_line = format!("- source: {}", observation.source);
    let frame_chars = "\n## Latest Observation\n".chars().count()
        + step_line.chars().count()
        + source_line.chars().count()
        + "\n```text\n\n```".chars().count();

    match observation.kind {
        Some(ObservationKind::ArchiveContent) => {
            // 长非结构化正文：大幅压缩，只保留摘要 + 引用提示
            let content_budget = 80;
            let preview = summarize_for_markdown(&observation.content, content_budget);
            let content_hint = format!(
                "{} （长正文已摘要，如需全文可再次调用 {}）",
                preview, observation.source
            );
            vec![
                String::new(),
                "## Latest Observation".to_string(),
                step_line,
                source_line,
                content_hint,
            ]
        }
        Some(ObservationKind::TaskList) => {
            // 结构化列表：保留总数 + 前 3 项核心字段
            let content_budget = section_max.saturating_sub(frame_chars).max(24);
            let content = summarize_for_markdown(&observation.content, content_budget);
            // 尝试提取列表长度提示
            let count_hint = extract_task_list_count_hint(&observation.content);
            vec![
                String::new(),
                "## Latest Observation".to_string(),
                step_line,
                source_line,
                count_hint,
                "```text".to_string(),
                content,
                "```".to_string(),
            ]
        }
        _ => {
            // Short Structured / Text / FileMutation：保持现有行为
            let content_budget = section_max.saturating_sub(frame_chars).max(24);
            let content = summarize_for_markdown(&observation.content, content_budget);
            vec![
                String::new(),
                "## Latest Observation".to_string(),
                step_line,
                source_line,
                "```text".to_string(),
                content,
                "```".to_string(),
            ]
        }
    }
}

/// 从 TaskList JSON 中提取数量提示（尽量提取 count 或数组长度）
fn extract_task_list_count_hint(content: &str) -> String {
    // 优先提取 JSON 数组长度
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(content) {
        if let Some(arr) = val.as_array() {
            return format!("- total_count: {}（仅展示前若干项）", arr.len());
        }
        if let Some(obj) = val.as_object() {
            if let Some(count) = obj.get("count").and_then(|v| v.as_u64()) {
                return format!("- total_count: {}", count);
            }
            if let Some(tasks) = obj.get("tasks").and_then(|v| v.as_array()) {
                return format!("- total_count: {}（仅展示前若干项）", tasks.len());
            }
        }
    }
    // 回退：按行数估算
    let line_count = content.lines().count();
    format!("- approx_line_count: {}", line_count)
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
        format!(
            "replans={}/{}",
            trace.controller_state.replan_count, trace.controller_state.max_replans
        ),
        format!("failures={}", trace.controller_state.failure_count),
        format!(
            "current_step={}",
            trace
                .current_step_index
                .map(|value| value.to_string())
                .unwrap_or_else(|| "(none)".to_string())
        ),
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

fn select_previous_observations<'a>(
    trace: &'a AgentRunTrace,
    observation: Option<&AgentObservation>,
) -> Vec<&'a ObservationTrace> {
    let candidate_len = if observation.is_some() && !trace.observations.is_empty() {
        trace.observations.len().saturating_sub(1)
    } else {
        trace.observations.len()
    };
    let candidates = &trace.observations[..candidate_len];
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut selected = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in candidates.iter().rev() {
        let key = format!("{}::{}", item.source, item.summary);
        if seen.insert(key) {
            selected.push(item);
        }
        if selected.len() >= 3 {
            break;
        }
    }
    selected.reverse();
    selected
}

fn append_session_state_lines(
    lines: &mut Vec<String>,
    runtime_session_state: &RuntimeSessionStateSnapshot,
) {
    lines.push(String::new());
    lines.push("## Session State".to_string());
    if runtime_session_state.is_empty() {
        lines.push("- (empty)".to_string());
    } else {
        lines.extend(runtime_session_state.to_lines().into_iter().skip(2));
    }
}

fn append_context_section_overview(lines: &mut Vec<String>, sections: &[ContextSectionSnapshot]) {
    lines.push(String::new());
    lines.push("## Sections".to_string());
    if sections.is_empty() {
        lines.push("- (none)".to_string());
        return;
    }
    for section in sections {
        lines.push(format!(
            "- kind={} priority={} included={} trimmed={} chars={}/{} orig={}",
            section.kind,
            section.priority,
            section.included,
            section.trimmed,
            section.char_count,
            section.max_chars,
            section.original_char_count
        ));
        if let Some(reason) = &section.trim_reason {
            lines.push(format!("  - trim_reason: {}", reason.as_str()));
        }
        if let Some(reason) = &section.drop_reason {
            lines.push(format!("  - drop_reason: {}", reason.as_str()));
        }
    }
}

fn render_context_preview(
    trace: &AgentRunTrace,
    planner_input: &PlannerInput,
    runtime_session_state: &RuntimeSessionStateSnapshot,
    memory_session_state: &SessionState,
    mode: ContextPreviewMode,
) -> String {
    let mut lines = vec![
        "# Context Preview".to_string(),
        String::new(),
        format!("- source_type: {}", trace.source_type),
        format!(
            "- trigger_type: {}",
            trace.trigger_type.as_deref().unwrap_or("(none)")
        ),
        format!(
            "- user_id: {}",
            trace.user_id.as_deref().unwrap_or("(none)")
        ),
        format!("- message_count: {}", trace.message_count),
        format!("- session_text_chars: {}", trace.session_text_chars),
        format!(
            "- memory: retrieved={} injected={} dropped={} budget={}/{}/{}",
            memory_session_state.retrieved_count(),
            memory_session_state.injected_count(),
            memory_session_state.dropped.len(),
            memory_session_state.budget.max_items,
            memory_session_state.budget.max_total_chars,
            memory_session_state.budget.max_single_chars
        ),
        format!(
            "- context_budget: final={}/{} trimmed={} dropped={}",
            planner_input.context_budget_summary.final_total_chars,
            planner_input.context_budget_summary.max_total_chars,
            planner_input.context_budget_summary.trimmed_section_count,
            planner_input.context_budget_summary.dropped_section_count
        ),
    ];

    append_session_state_lines(&mut lines, runtime_session_state);
    append_context_section_overview(&mut lines, &planner_input.context_sections);

    if matches!(mode, ContextPreviewMode::Verbose) {
        lines.push(String::new());
        lines.push("## Memory Dropped".to_string());
        if memory_session_state.dropped.is_empty() {
            lines.push("- (none)".to_string());
        } else {
            for dropped in &memory_session_state.dropped {
                lines.push(format!(
                    "- id={} reason={} preview={}",
                    dropped.id,
                    dropped.reason.as_str(),
                    dropped.content_preview
                ));
            }
        }

        lines.push(String::new());
        lines.push("## Section Content Preview".to_string());
        for section in &planner_input.context_sections {
            lines.push(String::new());
            lines.push(format!(
                "### {}{}",
                section.kind,
                if section.included { "" } else { " (dropped)" }
            ));
            lines.push("```text".to_string());
            lines.push(summarize_for_markdown(&section.content, 500));
            lines.push("```".to_string());
        }
    }

    lines.push(String::new());
    lines.push("## Prompt Preview".to_string());
    lines.push("```text".to_string());
    lines.push(summarize_for_markdown(
        &planner_input.assembled_user_prompt,
        1200,
    ));
    lines.push("```".to_string());

    lines.join("\n")
}

fn default_expected_observation_for_decision(
    decision: &AgentDecision,
) -> Option<ExpectedObservation> {
    match decision {
        AgentDecision::CallTool(ToolAction::Create { .. })
        | AgentDecision::CallTool(ToolAction::Write { .. }) => Some(ExpectedObservation {
            kind: ObservationKind::FileMutation,
            done_rule: DoneRule::ToolSuccess,
            expected_fields: Vec::new(),
            minimum_novelty: default_minimum_novelty_for_kind(&ObservationKind::FileMutation),
        }),
        AgentDecision::CallTool(ToolAction::Read { .. }) => Some(ExpectedObservation {
            kind: ObservationKind::Text,
            done_rule: DoneRule::NonEmptyOutput,
            expected_fields: Vec::new(),
            minimum_novelty: default_minimum_novelty_for_kind(&ObservationKind::Text),
        }),
        AgentDecision::CallTool(ToolAction::GetTaskStatus { .. }) => Some(ExpectedObservation {
            kind: ObservationKind::TaskStatus,
            done_rule: DoneRule::RequiresJsonField {
                field: "found".to_string(),
            },
            expected_fields: vec!["found".to_string()],
            minimum_novelty: default_minimum_novelty_for_kind(&ObservationKind::TaskStatus),
        }),
        AgentDecision::CallTool(ToolAction::ListRecentTasks { .. })
        | AgentDecision::CallTool(ToolAction::ListManualTasks { .. }) => {
            Some(ExpectedObservation {
                kind: ObservationKind::TaskList,
                done_rule: DoneRule::ToolSuccess,
                expected_fields: vec!["count".to_string(), "tasks".to_string()],
                minimum_novelty: default_minimum_novelty_for_kind(&ObservationKind::TaskList),
            })
        }
        AgentDecision::CallTool(ToolAction::ReadArticleArchive { .. }) => {
            Some(ExpectedObservation {
                kind: ObservationKind::ArchiveContent,
                done_rule: DoneRule::RequiresJsonField {
                    field: "content".to_string(),
                },
                expected_fields: vec![
                    "content".to_string(),
                    "content_chars".to_string(),
                    "output_path".to_string(),
                ],
                minimum_novelty: default_minimum_novelty_for_kind(&ObservationKind::ArchiveContent),
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

    let json_object = match expected.kind {
        ObservationKind::Text | ObservationKind::FileMutation => None,
        ObservationKind::JsonObject
        | ObservationKind::TaskStatus
        | ObservationKind::TaskList
        | ObservationKind::ArchiveContent => Some(parse_observation_json_object(
            observation,
            "期望 JSON observation",
        )?),
    };

    match expected.kind {
        ObservationKind::Text | ObservationKind::FileMutation => {}
        ObservationKind::JsonObject
        | ObservationKind::TaskStatus
        | ObservationKind::TaskList
        | ObservationKind::ArchiveContent => {}
    }

    match &expected.done_rule {
        DoneRule::ToolSuccess => {}
        DoneRule::NonEmptyOutput => {
            if observation.content.trim().is_empty() {
                bail!("期望非空输出，但 observation 为空");
            }
        }
        DoneRule::RequiresJsonField { field } => {
            let object = json_object
                .as_ref()
                .context("done_rule 需要 JSON object observation")?;
            let field_value = object
                .get(field)
                .with_context(|| format!("缺少期望字段: {field}"))?;
            if field_value.is_null() {
                bail!("期望字段 {field} 不能为空");
            }
        }
    }

    if !expected.expected_fields.is_empty() {
        let object = json_object
            .as_ref()
            .context("expected_fields 需要 JSON object observation")?;
        for field in &expected.expected_fields {
            let field_value = object
                .get(field)
                .with_context(|| format!("缺少 expected_fields 字段: {field}"))?;
            if field_value.is_null() {
                bail!("expected_fields 字段 {field} 不能为空");
            }
        }
    }

    Ok(())
}

fn detect_low_value_observation_failure(
    expected: Option<&ExpectedObservation>,
    current: &AgentObservation,
    previous: Option<&AgentObservation>,
) -> Option<FailureDecision> {
    let expected = expected?;
    expected.effective_minimum_novelty()?;
    let previous = previous?;

    if matches!(expected.kind, ObservationKind::FileMutation) {
        return None;
    }

    let current_norm = normalize_observation_payload(&current.content);
    let previous_norm = normalize_observation_payload(&previous.content);
    if current_norm.is_empty() || previous_norm.is_empty() {
        return None;
    }
    if current_norm != previous_norm {
        return None;
    }

    Some(FailureDecision {
        kind: StepFailureKind::LowValueObservation,
        action: FailureAction::Replan,
        replan_scope: Some(ReplanScope::RemainingPlan),
        detail: "observation 没有带来新的信息增量".to_string(),
        source: current.source.clone(),
        user_message: None,
    })
}

fn normalize_observation_payload(input: &str) -> String {
    input
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

fn parse_observation_json_object(
    observation: &AgentObservation,
    context_message: &str,
) -> Result<serde_json::Map<String, Value>> {
    let value: Value = serde_json::from_str(&observation.content)
        .with_context(|| format!("{context_message}，但解析失败: {}", observation.source))?;
    value
        .as_object()
        .cloned()
        .context("期望 JSON object observation，但返回不是 object")
}

fn default_minimum_novelty_for_kind(kind: &ObservationKind) -> Option<MinimumNovelty> {
    match kind {
        ObservationKind::FileMutation => None,
        ObservationKind::Text
        | ObservationKind::JsonObject
        | ObservationKind::TaskStatus
        | ObservationKind::TaskList
        | ObservationKind::ArchiveContent => Some(MinimumNovelty::DifferentFromLast),
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
        replan_scope: Some(ReplanScope::CurrentStep),
        detail: format!("重复动作检测命中: {}", current_summary),
        source: "watchdog:repeated_action".to_string(),
        user_message: None,
    })
}

fn detect_trajectory_drift_failure(
    planned: &PlannedDecision,
    trace: &AgentRunTrace,
) -> Option<FailureDecision> {
    if !matches!(planned.decision, AgentDecision::Final(_)) {
        return None;
    }
    if !trace.has_incomplete_plan_steps() {
        return None;
    }
    Some(FailureDecision {
        kind: StepFailureKind::TrajectoryDrift,
        action: FailureAction::Replan,
        replan_scope: Some(ReplanScope::Full),
        detail: "当前计划尚未完成，却提前进入 Final".to_string(),
        source: "watchdog:trajectory_drift".to_string(),
        user_message: None,
    })
}

fn detect_stalled_trajectory_failure(trace: &AgentRunTrace) -> Option<FailureDecision> {
    let current_step_index = trace.current_step_index?;
    let recent_failures = trace.consecutive_failures_for_current_step(current_step_index);
    if recent_failures.len() < 2 {
        return None;
    }

    let already_full_replanned = recent_failures.iter().any(|failure| {
        failure.replan_scope == Some(ReplanScope::Full)
            || matches!(
                failure.kind,
                StepFailureKind::TrajectoryDrift | StepFailureKind::StalledTrajectory
            )
    });

    let detail = format!(
        "当前计划第 {} 步连续失败/重规划，执行轨迹停滞",
        current_step_index
    );

    if already_full_replanned {
        return Some(FailureDecision {
            kind: StepFailureKind::StalledTrajectory,
            action: FailureAction::AskUser,
            replan_scope: None,
            detail,
            source: "watchdog:stalled_trajectory".to_string(),
            user_message: Some(
                "我在当前计划步骤上连续纠偏后仍没有推进。请补充更明确的目标、任务编号或必要输入后，我再继续。"
                    .to_string(),
            ),
        });
    }

    Some(FailureDecision {
        kind: StepFailureKind::StalledTrajectory,
        action: FailureAction::Replan,
        replan_scope: Some(ReplanScope::Full),
        detail,
        source: "watchdog:stalled_trajectory".to_string(),
        user_message: None,
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
            "user_message": failure.user_message,
        }))
        .unwrap_or_else(|_| "{\"failure_kind\":\"unknown\"}".to_string()),
        kind: Some(ObservationKind::Text),
    }
}

fn classify_tool_execution_failure(source: String, detail: &str) -> FailureDecision {
    let lower = detail.to_ascii_lowercase();
    let kind = if lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("429")
        || lower.contains("governor")
        || lower.contains("too many requests")
    {
        StepFailureKind::Transient
    } else if lower.contains("尚未生成归档")
        || lower.contains("归档 output_path")
        || lower.contains("未找到对应任务")
    {
        StepFailureKind::ManualIntervention
    } else if lower.contains("路径越界") || lower.contains("不能为空") || lower.contains("不支持")
    {
        StepFailureKind::Irrecoverable
    } else if lower.contains("读取文件失败") || lower.contains("未找到") {
        StepFailureKind::Semantic
    } else {
        StepFailureKind::Irrecoverable
    };

    let policy = default_recovery_for_failure(kind);
    let user_message = match kind {
        StepFailureKind::ManualIntervention => Some(
            "当前任务还缺少可直接继续的业务输入。你可以先查任务状态、确认 task_id，或先补正文/等待归档完成后再继续。"
                .to_string(),
        ),
        _ => None,
    };
    let replan_scope = match kind {
        StepFailureKind::Semantic => Some(ReplanScope::RemainingPlan),
        _ => None,
    };

    FailureDecision {
        kind,
        action: policy.action,
        replan_scope,
        detail: detail.to_string(),
        source,
        user_message,
    }
}

impl StepFailureKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Transient => "transient",
            Self::Expectation => "expectation",
            Self::LowValueObservation => "low_value_observation",
            Self::RepeatedAction => "repeated_action",
            Self::BudgetExhausted => "budget_exhausted",
            Self::StalledTrajectory => "stalled_trajectory",
            Self::TrajectoryDrift => "trajectory_drift",
            Self::ManualIntervention => "manual_intervention",
            Self::Semantic => "semantic",
            Self::Irrecoverable => "irrecoverable",
        }
    }
}

impl FailureAction {
    fn as_str(&self) -> &'static str {
        match self {
            Self::RetryStep => "retry_step",
            Self::Replan => "replan",
            Self::AskUser => "ask_user",
            Self::Abort => "abort",
        }
    }
}

impl RecoveryOutcome {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Continued => "continued",
            Self::EscalatedToAskUser => "escalated_to_ask_user",
            Self::Aborted => "aborted",
            Self::Failed => "failed",
        }
    }
}

fn resulting_source_name(action: &ToolAction) -> String {
    format!("tool:{}", action.name())
}

fn observation_kind_for_action(action: &ToolAction) -> Option<ObservationKind> {
    match action {
        ToolAction::Read { .. } => Some(ObservationKind::Text),
        ToolAction::Write { .. } | ToolAction::Create { .. } => Some(ObservationKind::FileMutation),
        ToolAction::GetTaskStatus { .. } => Some(ObservationKind::TaskStatus),
        ToolAction::ListRecentTasks { .. } | ToolAction::ListManualTasks { .. } => {
            Some(ObservationKind::TaskList)
        }
        ToolAction::ReadArticleArchive { .. } => Some(ObservationKind::ArchiveContent),
    }
}

fn build_system_prompt(planning_policy: PlanningPolicy) -> &'static str {
    match planning_policy {
        PlanningPolicy::Reactive => {
            "你是一个工具规划器，采用最小 ReAct 风格工作：先根据上下文判断下一步，再决定是调用一个工具还是直接给出最终结果。每轮最多只调用一个工具。只输出 JSON，不要解释。格式为 {\"action\":\"read|write|create|get_task_status|list_recent_tasks|list_manual_tasks|read_article_archive|final\",\"path\":\"...\",\"content\":\"...\",\"task_id\":\"...\",\"limit\":5,\"answer\":\"...\",\"plan\":[\"步骤1\",\"步骤2\"],\"progress_note\":\"当前做到哪\",\"expected_kind\":\"text|json_object|file_mutation|task_status|task_list|archive_content\",\"done_rule\":\"tool_success|non_empty_output|required_json_field\",\"required_field\":\"field_name\",\"expected_fields\":[\"field_a\",\"field_b\"],\"minimum_novelty\":\"different_from_last\"}。read 只需要 path；write/create 需要 path 与 content；get_task_status 需要 task_id；list_recent_tasks/list_manual_tasks 可选 limit；read_article_archive 需要 task_id；final 需要 answer。plan、progress_note、expected_kind、done_rule、required_field、expected_fields、minimum_novelty 都是可选字段，用于表达当前计划、进度和期望观测。"
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
    expected_fields: Option<Vec<String>>,
    minimum_novelty: Option<String>,
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
            AgentDecision::CallTool(ToolAction::Create { path, content })
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
            AgentDecision::CallTool(ToolAction::ReadArticleArchive { task_id })
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
        plan.expected_fields.as_deref(),
        plan.minimum_novelty.as_deref(),
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
    expected_fields: Option<&[String]>,
    minimum_novelty: Option<&str>,
) -> Result<Option<ExpectedObservation>> {
    let kind = match expected_kind
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
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
    let expected_fields = expected_fields
        .unwrap_or_default()
        .iter()
        .map(|field| field.trim())
        .filter(|field| !field.is_empty())
        .map(|field| field.to_string())
        .collect::<Vec<_>>();
    let minimum_novelty = match minimum_novelty
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some("different_from_last") => Some(MinimumNovelty::DifferentFromLast),
        Some(other) => bail!("minimum_novelty 不支持: {other}"),
        None => None,
    };

    match (kind, done_rule) {
        (None, None) if expected_fields.is_empty() && minimum_novelty.is_none() => Ok(None),
        (None, None) => {
            bail!("expected_fields / minimum_novelty 需要同时提供 expected_kind 与 done_rule")
        }
        (Some(kind), Some(done_rule)) => Ok(Some(ExpectedObservation {
            kind,
            done_rule,
            expected_fields,
            minimum_novelty,
        })),
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
        build_agent_log_payload, build_context_pack, build_context_summary,
        classify_tool_execution_failure, derive_runtime_session_state,
        detect_stalled_trajectory_failure, load_business_context_snapshot, map_llm_plan,
        parse_llm_plan, project_session_state_to_trace, select_previous_observations,
        select_retriever, validate_expected_observation, AgentCore, AgentObservation,
        AgentRunContext, AgentRunTrace, BusinessContextSnapshot, ContextAssembler,
        ContextPreviewMode, DoneRule, DropReason, ExecutionPlan, ExpectedObservation,
        FailureAction, FailureDecision, GoalSignal, LlmPlan, MemoryBudget, MinimumNovelty,
        ObservationKind, PlannedDecision, RecoveryOutcome, ReplanScope, RetrieverMode,
        RuntimeSessionStateSnapshot, StepFailureKind,
    };
    use crate::context_pack::{ContextSectionChangeReason, ContextSectionKind};
    use crate::retriever::rule::RuleRetriever;
    use crate::session_summary::SessionSummaryStrategy;
    use crate::task_store::{MemoryType, TaskStore};
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
        assert!(payload["recovery_attempts"].is_array());
        assert!(payload.get("recovery_action").is_some());
        assert!(payload.get("recovery_result").is_some());

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
        let context =
            AgentRunContext::wechat_chat("user-builder", "commit", vec!["msg-builder".to_string()])
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
        agent
            .scripted_decisions
            .borrow_mut()
            .extend([PlannedDecision::new(super::AgentDecision::CallTool(
                super::ToolAction::Read {
                    path: "missing.txt".to_string(),
                },
            ))
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
        std::fs::create_dir_all(empty_path.parent().expect("空文件路径应存在父目录"))
            .expect("创建空文件目录失败");
        std::fs::write(&empty_path, "").expect("写入空文件失败");
        let agent = AgentCore::with_max_steps_and_task_store_db_path(
            root.clone(),
            3,
            None::<std::path::PathBuf>,
        )
        .expect("初始化 agent 失败");
        agent
            .scripted_decisions
            .borrow_mut()
            .extend([PlannedDecision::new(super::AgentDecision::CallTool(
                super::ToolAction::Read {
                    path: "demo/empty.txt".to_string(),
                },
            ))
            .with_plan(Some(ExecutionPlan {
                steps: vec!["读取非空文件".to_string()],
            }))
            .with_expected_observation(Some(ExpectedObservation {
                kind: ObservationKind::Text,
                done_rule: DoneRule::NonEmptyOutput,
                expected_fields: Vec::new(),
                minimum_novelty: Some(MinimumNovelty::DifferentFromLast),
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
    fn low_value_observation_triggers_replan() {
        let root = temp_workspace();
        let db_path = temp_db_path();
        let agent =
            AgentCore::with_max_steps_and_task_store_db_path(root.clone(), 4, Some(db_path))
                .expect("初始化 agent 失败");
        agent.scripted_decisions.borrow_mut().extend([
            PlannedDecision::new(super::AgentDecision::CallTool(
                super::ToolAction::ListRecentTasks { limit: 5 },
            ))
            .with_plan(Some(ExecutionPlan {
                steps: vec!["查最近任务".to_string(), "查待补录任务".to_string()],
            }))
            .with_expected_observation(Some(ExpectedObservation {
                kind: ObservationKind::TaskList,
                done_rule: DoneRule::ToolSuccess,
                expected_fields: vec!["count".to_string(), "tasks".to_string()],
                minimum_novelty: Some(MinimumNovelty::DifferentFromLast),
            })),
            PlannedDecision::new(super::AgentDecision::CallTool(
                super::ToolAction::ListManualTasks { limit: 5 },
            ))
            .with_expected_observation(Some(ExpectedObservation {
                kind: ObservationKind::TaskList,
                done_rule: DoneRule::ToolSuccess,
                expected_fields: vec!["count".to_string(), "tasks".to_string()],
                minimum_novelty: Some(MinimumNovelty::DifferentFromLast),
            })),
            PlannedDecision::new(super::AgentDecision::Final("replanned".to_string()))
                .with_progress_note(Some("切换后续路径".to_string())),
        ]);

        let result = agent
            .run("检查任务列表")
            .expect("低价值 observation 后应能 replan");
        assert_eq!(result, "replanned");

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

        assert_eq!(payload["failures"][0]["kind"], "low_value_observation");
    }

    #[test]
    fn unfinished_plan_final_triggers_trajectory_drift_replan() {
        let root = temp_workspace();
        let agent = AgentCore::with_max_steps_and_task_store_db_path(
            root.clone(),
            4,
            None::<std::path::PathBuf>,
        )
        .expect("初始化 agent 失败");
        agent.scripted_decisions.borrow_mut().extend([
            PlannedDecision::new(super::AgentDecision::CallTool(super::ToolAction::Create {
                path: "demo/drift.txt".to_string(),
                content: "hello".to_string(),
            }))
            .with_plan(Some(ExecutionPlan {
                steps: vec!["创建文件".to_string(), "读取文件".to_string()],
            })),
            PlannedDecision::new(super::AgentDecision::Final("过早结束".to_string())),
            PlannedDecision::new(super::AgentDecision::CallTool(super::ToolAction::Read {
                path: "demo/drift.txt".to_string(),
            })),
            PlannedDecision::new(super::AgentDecision::Final("重新规划后完成".to_string())),
        ]);

        let result = agent
            .run("执行计划")
            .expect("trajectory drift 后应能 replan");
        assert_eq!(result, "重新规划后完成");

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

        let kinds = payload["failures"]
            .as_array()
            .expect("应存在 failures 数组")
            .iter()
            .map(|value| value["kind"].as_str().unwrap_or(""))
            .collect::<Vec<_>>();
        assert!(kinds.contains(&"trajectory_drift"));
    }

    #[test]
    fn current_step_replan_scope_preserves_done_prefix_and_tail() {
        let workspace = temp_workspace();
        let mut trace = AgentRunTrace::new(&workspace, "计划测试", AgentRunContext::agent_demo());
        trace.record_decision(
            0,
            "scripted",
            &PlannedDecision::new(super::AgentDecision::Final("init".to_string())).with_plan(Some(
                ExecutionPlan {
                    steps: vec!["A".to_string(), "B".to_string(), "C".to_string()],
                },
            )),
        );
        trace.mark_next_plan_step_running(None);
        trace.mark_running_plan_step_done();
        trace.mark_next_plan_step_running(None);
        trace.mark_running_plan_step_failed();
        trace.record_failure(
            1,
            &FailureDecision {
                kind: StepFailureKind::RepeatedAction,
                action: FailureAction::Replan,
                replan_scope: Some(ReplanScope::CurrentStep),
                detail: "repeat".to_string(),
                source: "test".to_string(),
                user_message: None,
            },
        );
        trace.record_decision(
            2,
            "scripted",
            &PlannedDecision::new(super::AgentDecision::Final("noop".to_string())).with_plan(Some(
                ExecutionPlan {
                    steps: vec!["B2".to_string()],
                },
            )),
        );

        let descriptions = trace
            .active_plan_steps
            .iter()
            .map(|step| step.description.as_str())
            .collect::<Vec<_>>();
        assert_eq!(descriptions, vec!["A", "B2", "C"]);
    }

    #[test]
    fn current_step_index_tracks_executor_progress() {
        let workspace = temp_workspace();
        let mut trace = AgentRunTrace::new(&workspace, "计划测试", AgentRunContext::agent_demo());
        trace.record_decision(
            0,
            "scripted",
            &PlannedDecision::new(super::AgentDecision::Final("init".to_string())).with_plan(Some(
                ExecutionPlan {
                    steps: vec!["A".to_string(), "B".to_string()],
                },
            )),
        );

        assert_eq!(trace.current_step_index, Some(1));

        trace.mark_next_plan_step_running(None);
        assert_eq!(trace.current_step_index, Some(1));

        trace.mark_running_plan_step_done();
        assert_eq!(trace.current_step_index, Some(2));

        trace.mark_next_plan_step_running(None);
        trace.mark_running_plan_step_failed();
        assert_eq!(trace.current_step_index, Some(2));
    }

    #[test]
    fn remaining_plan_replan_scope_replaces_remaining_steps() {
        let workspace = temp_workspace();
        let mut trace = AgentRunTrace::new(&workspace, "计划测试", AgentRunContext::agent_demo());
        trace.record_decision(
            0,
            "scripted",
            &PlannedDecision::new(super::AgentDecision::Final("init".to_string())).with_plan(Some(
                ExecutionPlan {
                    steps: vec!["A".to_string(), "B".to_string(), "C".to_string()],
                },
            )),
        );
        trace.mark_next_plan_step_running(None);
        trace.mark_running_plan_step_done();
        trace.record_failure(
            1,
            &FailureDecision {
                kind: StepFailureKind::Semantic,
                action: FailureAction::Replan,
                replan_scope: Some(ReplanScope::RemainingPlan),
                detail: "semantic".to_string(),
                source: "test".to_string(),
                user_message: None,
            },
        );
        trace.record_decision(
            2,
            "scripted",
            &PlannedDecision::new(super::AgentDecision::Final("noop".to_string())).with_plan(Some(
                ExecutionPlan {
                    steps: vec!["X".to_string(), "Y".to_string()],
                },
            )),
        );

        let descriptions = trace
            .active_plan_steps
            .iter()
            .map(|step| step.description.as_str())
            .collect::<Vec<_>>();
        assert_eq!(descriptions, vec!["A", "X", "Y"]);
    }

    #[test]
    fn full_replan_scope_replaces_entire_plan() {
        let workspace = temp_workspace();
        let mut trace = AgentRunTrace::new(&workspace, "计划测试", AgentRunContext::agent_demo());
        trace.record_decision(
            0,
            "scripted",
            &PlannedDecision::new(super::AgentDecision::Final("init".to_string())).with_plan(Some(
                ExecutionPlan {
                    steps: vec!["A".to_string(), "B".to_string()],
                },
            )),
        );
        trace.record_failure(
            1,
            &FailureDecision {
                kind: StepFailureKind::TrajectoryDrift,
                action: FailureAction::Replan,
                replan_scope: Some(ReplanScope::Full),
                detail: "drift".to_string(),
                source: "test".to_string(),
                user_message: None,
            },
        );
        trace.record_decision(
            2,
            "scripted",
            &PlannedDecision::new(super::AgentDecision::Final("noop".to_string())).with_plan(Some(
                ExecutionPlan {
                    steps: vec!["Z".to_string()],
                },
            )),
        );

        let descriptions = trace
            .active_plan_steps
            .iter()
            .map(|step| step.description.as_str())
            .collect::<Vec<_>>();
        assert_eq!(descriptions, vec!["Z"]);
    }

    #[test]
    fn stalled_trajectory_escalates_to_full_replan_then_ask_user() {
        let workspace = temp_workspace();
        let mut trace = AgentRunTrace::new(&workspace, "计划测试", AgentRunContext::agent_demo());
        trace.record_decision(
            0,
            "scripted",
            &PlannedDecision::new(super::AgentDecision::Final("init".to_string())).with_plan(Some(
                ExecutionPlan {
                    steps: vec!["A".to_string(), "B".to_string()],
                },
            )),
        );
        trace.record_failure(
            1,
            &FailureDecision {
                kind: StepFailureKind::Semantic,
                action: FailureAction::Replan,
                replan_scope: Some(ReplanScope::CurrentStep),
                detail: "first".to_string(),
                source: "test".to_string(),
                user_message: None,
            },
        );
        trace.record_failure(
            2,
            &FailureDecision {
                kind: StepFailureKind::LowValueObservation,
                action: FailureAction::Replan,
                replan_scope: Some(ReplanScope::RemainingPlan),
                detail: "second".to_string(),
                source: "test".to_string(),
                user_message: None,
            },
        );

        let first = detect_stalled_trajectory_failure(&trace).expect("应命中 stalled trajectory");
        assert_eq!(first.kind, StepFailureKind::StalledTrajectory);
        assert_eq!(first.action, FailureAction::Replan);
        assert_eq!(first.replan_scope, Some(ReplanScope::Full));

        trace.record_failure(
            3,
            &FailureDecision {
                kind: StepFailureKind::TrajectoryDrift,
                action: FailureAction::Replan,
                replan_scope: Some(ReplanScope::Full),
                detail: "third".to_string(),
                source: "test".to_string(),
                user_message: None,
            },
        );

        let second = detect_stalled_trajectory_failure(&trace).expect("再次停滞时应升级 ask_user");
        assert_eq!(second.kind, StepFailureKind::StalledTrajectory);
        assert_eq!(second.action, FailureAction::AskUser);
        assert!(second.user_message.is_some());
    }

    #[test]
    fn ask_user_failure_action_returns_user_message() {
        let agent = AgentCore::with_max_steps_and_task_store_db_path(
            temp_workspace(),
            3,
            None::<std::path::PathBuf>,
        )
        .expect("初始化 agent 失败");
        let mut trace =
            AgentRunTrace::new(&temp_workspace(), "ask user", AgentRunContext::agent_demo());

        let control = agent
            .handle_recorded_failure(
                1,
                FailureDecision {
                    kind: StepFailureKind::ManualIntervention,
                    action: FailureAction::AskUser,
                    replan_scope: None,
                    detail: "detail".to_string(),
                    source: "test".to_string(),
                    user_message: Some("请补充 task_id".to_string()),
                },
                &mut trace,
            )
            .expect("ask_user 应直接返回");

        match control {
            super::LoopControl::Finish(answer) => assert_eq!(answer, "请补充 task_id"),
            super::LoopControl::Continue(_) => panic!("ask_user 不应继续执行"),
        }
        assert_eq!(trace.controller_state.ask_user_count, 1);
        assert_eq!(trace.recovery_attempts.len(), 1);
        assert_eq!(
            trace.recovery_attempts[0].outcome,
            RecoveryOutcome::EscalatedToAskUser
        );
    }

    #[test]
    fn replan_budget_exhaustion_turns_into_ask_user() {
        let workspace = temp_workspace();
        let agent = AgentCore::with_scripted_decisions(
            workspace.clone(),
            3,
            vec![super::AgentDecision::Final("noop".to_string())],
        )
        .expect("初始化 agent 失败");
        let mut trace = AgentRunTrace::new(&workspace, "budget", AgentRunContext::agent_demo());
        trace.configure_controller_limits(3, 1);

        let first = agent
            .handle_recorded_failure(
                1,
                FailureDecision {
                    kind: StepFailureKind::Semantic,
                    action: FailureAction::Replan,
                    replan_scope: Some(ReplanScope::CurrentStep),
                    detail: "first".to_string(),
                    source: "test".to_string(),
                    user_message: None,
                },
                &mut trace,
            )
            .expect("第一次 replan 应允许");
        assert!(matches!(first, super::LoopControl::Continue(_)));
        assert_eq!(trace.controller_state.replan_count, 1);

        // 第二次用不同 kind（expectation），避免防循环升级先触发，
        // 从而确保能走到 budget exhaustion 路径
        let second = agent
            .handle_recorded_failure(
                2,
                FailureDecision {
                    kind: StepFailureKind::Expectation,
                    action: FailureAction::Replan,
                    replan_scope: Some(ReplanScope::CurrentStep),
                    detail: "second".to_string(),
                    source: "test".to_string(),
                    user_message: None,
                },
                &mut trace,
            )
            .expect("预算耗尽后应 ask_user");
        match second {
            super::LoopControl::Finish(answer) => {
                assert!(answer.contains("多次重规划仍未收敛"));
            }
            super::LoopControl::Continue(_) => panic!("预算耗尽后不应继续 replan"),
        }
        assert_eq!(trace.controller_state.replan_count, 1);
        assert_eq!(trace.controller_state.ask_user_count, 1);
        // 两次 recovery + 一次 budget_exhausted = 3
        assert_eq!(trace.recovery_attempts.len(), 3);
        assert!(trace
            .recovery_attempts
            .iter()
            .any(|attempt| attempt.outcome == RecoveryOutcome::Continued));
        assert!(trace
            .recovery_attempts
            .iter()
            .any(|attempt| { attempt.outcome == RecoveryOutcome::EscalatedToAskUser }));
        assert_eq!(
            trace.failures.last().map(|failure| failure.kind),
            Some(StepFailureKind::BudgetExhausted)
        );
    }

    #[test]
    fn context_assembler_includes_runtime_fields_and_observation() {
        let workspace = temp_workspace();
        let mut trace = AgentRunTrace::new(
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
        trace.record_decision(
            0,
            "scripted",
            &PlannedDecision::new(super::AgentDecision::CallTool(super::ToolAction::Read {
                path: "demo.txt".to_string(),
            }))
            .with_plan(Some(ExecutionPlan {
                steps: vec!["读取 demo.txt".to_string(), "总结内容".to_string()],
            })),
        );
        let observation = AgentObservation::tool_result(
            1,
            "read_file",
            "hello from tool",
            Some(ObservationKind::Text),
        );
        let business_context = BusinessContextSnapshot {
            current_task: None,
            recent_tasks: Vec::new(),
            user_memories: Vec::new(),
        };
        let runtime_session_state =
            derive_runtime_session_state(&trace, "读文件 demo.txt", Some(&observation), None);
        let planner_input = ContextAssembler::default().assemble(
            &trace,
            "读文件 demo.txt",
            Some(&observation),
            Some(&runtime_session_state),
            &[
                "read: 读取工作区内文件，参数: path".to_string(),
                "get_task_status: 查询单个任务状态，参数: task_id".to_string(),
            ],
            Some(&business_context),
        );

        assert!(planner_input
            .assembled_user_prompt
            .contains("## Runtime Context"));
        assert!(planner_input
            .assembled_user_prompt
            .contains("source_type: wechat_chat"));
        assert!(planner_input
            .assembled_user_prompt
            .contains("task_id: task-ctx"));
        assert!(planner_input
            .assembled_user_prompt
            .contains("article_id: article-ctx"));
        assert!(planner_input
            .assembled_user_prompt
            .contains("session merged text"));
        assert!(planner_input
            .assembled_user_prompt
            .contains("replan_budget: 0/3"));
        assert!(planner_input
            .assembled_user_prompt
            .contains("current_step_index: 1"));
        assert!(planner_input
            .assembled_user_prompt
            .contains("## Session State"));
        assert!(planner_input
            .assembled_user_prompt
            .contains("## Latest Observation"));
        assert!(planner_input
            .assembled_user_prompt
            .contains("hello from tool"));
        assert!(planner_input.context_budget_summary.final_total_chars > 0);
        let latest_section = planner_input
            .context_sections
            .iter()
            .find(|section| section.kind == "latest_observation")
            .expect("应包含 latest_observation section");
        assert!(latest_section.line_count >= 6);
        assert!(latest_section.item_count >= 5);
        assert!(latest_section.char_count > 0);
        assert!(latest_section.content.contains("read_file"));
    }

    #[test]
    fn context_pack_exposes_section_metadata() {
        let workspace = temp_workspace();
        let trace = AgentRunTrace::new(
            &workspace,
            "读文件 demo.txt",
            AgentRunContext::wechat_chat("user-context", "commit", vec![])
                .with_session_text("session merged text"),
        );
        let observation = AgentObservation::tool_result(
            1,
            "read_file",
            "hello from tool",
            Some(ObservationKind::Text),
        );
        let runtime_session_state =
            derive_runtime_session_state(&trace, "读文件 demo.txt", Some(&observation), None);
        let pack = ContextAssembler::default().build_pack(
            &trace,
            "读文件 demo.txt",
            Some(&observation),
            Some(&runtime_session_state),
            &["read: 读取工作区内文件，参数: path".to_string()],
            None,
        );

        let latest = pack
            .section(ContextSectionKind::LatestObservation)
            .expect("应包含 latest observation section");
        assert_eq!(latest.kind().as_str(), "latest_observation");
        assert!(latest.item_count() >= 5);
        assert!(latest.char_count() > 0);
        assert!(latest.lines().iter().any(|line| line.contains("read_file")));

        let rendered = pack.render();
        assert!(rendered.contains("## User Input"));
        assert!(rendered.contains("## Session State"));
        assert!(rendered.contains("## Session Text"));
        assert!(rendered.contains("## Available Tools"));
    }

    #[test]
    fn latest_observation_pre_trims_to_section_budget() {
        let workspace = temp_workspace();
        let trace = AgentRunTrace::new(
            &workspace,
            "读文件 demo.txt",
            AgentRunContext::wechat_chat("user-context", "commit", vec![]),
        );
        // Content far exceeds both old 800-limit and new section max (560)
        let long_content = "observation content ".repeat(100);
        let observation = AgentObservation::tool_result(
            1,
            "read_file",
            &long_content,
            Some(ObservationKind::Text),
        );
        let runtime_session_state =
            derive_runtime_session_state(&trace, "读文件 demo.txt", Some(&observation), None);
        let pack = ContextAssembler::default().build_pack(
            &trace,
            "读文件 demo.txt",
            Some(&observation),
            Some(&runtime_session_state),
            &["read: 读取工作区内文件，参数: path".to_string()],
            None,
        );

        let latest = pack
            .section(ContextSectionKind::LatestObservation)
            .expect("应包含 latest observation section");
        let max_chars = ContextSectionKind::LatestObservation.policy().max_chars;
        assert!(
            latest.char_count() <= max_chars,
            "latest_observation char_count {} exceeds max_chars {}",
            latest.char_count(),
            max_chars
        );
        // Pre-trimming means the section enters ContextSection::new already within budget,
        // so section-level trim should NOT fire (no double-truncation).
        assert!(
            !latest.trimmed(),
            "latest_observation should not be trimmed at section level when pre-trimmed"
        );
    }

    #[test]
    fn session_text_section_keeps_full_text_for_short_input() {
        let workspace = temp_workspace();
        let trace = AgentRunTrace::new(
            &workspace,
            "继续处理",
            AgentRunContext::wechat_chat("user-session-short", "commit", vec![])
                .with_session_text("短会话文本"),
        );
        let runtime_session_state = derive_runtime_session_state(&trace, "继续处理", None, None);
        let pack = ContextAssembler::default().build_pack(
            &trace,
            "继续处理",
            None,
            Some(&runtime_session_state),
            &["read: 读取工作区内文件，参数: path".to_string()],
            None,
        );

        let session_section = pack
            .section(ContextSectionKind::SessionText)
            .expect("应包含 session_text section");
        let rendered = session_section.render();
        assert!(rendered.contains("mode: full"));
        assert!(rendered.contains("短会话文本"));
    }

    #[test]
    fn session_text_section_uses_boundary_compaction_for_long_input() {
        let workspace = temp_workspace();
        let session_text = format!(
            "{}{}{}",
            "head-start ".repeat(25),
            "middle-noise ".repeat(50),
            "tail-marker-xyz"
        );
        let trace = AgentRunTrace::new(
            &workspace,
            "继续处理",
            AgentRunContext::wechat_chat("user-session-long", "commit", vec![])
                .with_session_text(session_text),
        );
        let runtime_session_state = derive_runtime_session_state(&trace, "继续处理", None, None);
        let pack = ContextAssembler::default().build_pack(
            &trace,
            "继续处理",
            None,
            Some(&runtime_session_state),
            &["read: 读取工作区内文件，参数: path".to_string()],
            None,
        );

        let session_section = pack
            .section(ContextSectionKind::SessionText)
            .expect("应包含 session_text section");
        let rendered = session_section.render();
        assert!(rendered.contains("mode: boundary_compaction"));
        assert!(rendered.contains("summary_strategy: semantic"));
        assert!(rendered.contains("summary_compact"));
        assert!(rendered.contains("recent_tail"));
        assert!(rendered.contains("tail-marker-xyz"));
    }

    #[test]
    fn session_text_compaction_supports_truncate_strategy() {
        let session_text = format!(
            "{}\n{}\n下一步: 修复关键问题并输出结论。\n风险: 需要补充参数。",
            "背景噪音内容".repeat(50),
            "历史过程信息".repeat(50),
        );
        let semantic_summary = super::summarize_session_text_semantic(
            &session_text,
            super::SESSION_TEXT_SUMMARY_MAX_CHARS,
        );
        let truncate_summary =
            super::summarize_for_markdown(&session_text, super::SESSION_TEXT_SUMMARY_MAX_CHARS);
        let semantic = super::build_session_text_section_lines(
            &session_text,
            super::SessionSummaryStrategy::Semantic,
        )
        .join("\n");
        let truncate = super::build_session_text_section_lines(
            &session_text,
            super::SessionSummaryStrategy::Truncate,
        )
        .join("\n");

        assert!(semantic.contains("summary_strategy: semantic"));
        assert!(truncate.contains("summary_strategy: truncate"));
        assert!(semantic_summary.contains("下一步: 修复关键问题并输出结论。"));
        assert!(!truncate_summary.contains("下一步: 修复关键问题并输出结论。"));
    }

    #[test]
    fn preview_context_respects_agent_config_summary_strategy() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        let config = crate::config::AgentConfig {
            session_summary_strategy: "truncate".to_string(),
            ..Default::default()
        };
        let agent =
            AgentCore::with_task_store_db_path_and_agent_config(workspace, &db_path, &config)
                .expect("初始化失败");

        let session_text = format!(
            "{}\n{}\n下一步: 修复关键问题并输出结论。\n风险: 需要补充参数。",
            "背景噪音内容".repeat(50),
            "历史过程信息".repeat(50),
        );
        let preview = agent
            .preview_context_with_context_mode(
                "帮我总结当前进展",
                AgentRunContext::wechat_chat("user-summary-config", "context_debug", vec![])
                    .with_session_text(session_text),
                ContextPreviewMode::Verbose,
            )
            .expect("应成功生成 preview");

        assert!(preview.contains("summary_strategy: truncate"));
    }

    #[test]
    fn context_pack_records_trim_and_drop_reasons() {
        let workspace = temp_workspace();
        let trace = AgentRunTrace::new(
            &workspace,
            "请帮我处理一个很长的上下文请求",
            AgentRunContext::wechat_chat("user-budget", "commit", vec![])
                .with_session_text("session ".repeat(240)),
        );
        let observation = AgentObservation::tool_result(
            1,
            "read_file",
            &"observation ".repeat(240),
            Some(ObservationKind::Text),
        );
        let runtime_session_state = RuntimeSessionStateSnapshot {
            goal: Some("推进一个需要大量上下文的信息整理任务".to_string()),
            current_subtask: Some("先整理线索，再决定是否继续调工具".to_string()),
            constraints: vec!["避免重复无效动作".to_string(); 4],
            confirmed_facts: vec![
                "已有较长 session_text 和 latest_observation".to_string();
                4
            ],
            done_items: vec!["已完成初步读取".to_string(); 3],
            next_step: Some("优先收敛上下文而不是继续膨胀 prompt".to_string()),
            open_questions: vec!["哪些 section 可以先被丢弃".to_string(); 3],
            goal_signal: GoalSignal::PersistentHigh,
        };
        let mut pack = ContextAssembler::default().build_pack(
            &trace,
            "请帮我处理一个很长的上下文请求",
            Some(&observation),
            Some(&runtime_session_state),
            &[
                format!("read: {}", "tool ".repeat(120)),
                format!("write: {}", "tool ".repeat(120)),
            ],
            None,
        );
        pack.set_max_total_chars(1500);
        pack.apply_total_budget();

        let snapshot = pack.snapshot();
        assert!(snapshot.iter().any(|section| {
            section.trimmed
                && section.trim_reason == Some(ContextSectionChangeReason::SectionBudgetExceeded)
        }));
        assert!(snapshot.iter().any(|section| {
            !section.included
                && section.drop_reason == Some(ContextSectionChangeReason::TotalBudgetExceeded)
        }));
        assert!(pack.budget_summary().final_total_chars <= 1500);
    }

    #[test]
    fn context_pack_includes_previous_observation_summaries() {
        let workspace = temp_workspace();
        let mut trace = AgentRunTrace::new(
            &workspace,
            "继续处理",
            AgentRunContext::wechat_chat("user-obs", "commit", vec![]),
        );
        let older = AgentObservation::tool_result(
            0,
            "read_file",
            "older observation payload",
            Some(ObservationKind::Text),
        );
        let latest = AgentObservation::tool_result(
            1,
            "write_file",
            "latest observation payload",
            Some(ObservationKind::FileMutation),
        );
        trace.record_observation(&older);
        trace.record_observation(&latest);

        let runtime_session_state =
            derive_runtime_session_state(&trace, "继续处理", Some(&latest), None);
        let pack = ContextAssembler {
            include_previous_observations: true,
        }
        .build_pack(
            &trace,
            "继续处理",
            Some(&latest),
            Some(&runtime_session_state),
            &["read: 读取工作区内文件，参数: path".to_string()],
            None,
        );

        let previous = pack
            .section(ContextSectionKind::PreviousObservations)
            .expect("应包含 previous_observations section");
        let rendered = previous.render();
        assert!(rendered.contains("older observation payload"));
        assert!(!rendered.contains("latest observation payload"));
    }

    #[test]
    fn previous_observations_skip_duplicate_summaries() {
        let workspace = temp_workspace();
        let mut trace = AgentRunTrace::new(
            &workspace,
            "继续处理",
            AgentRunContext::wechat_chat("user-obs", "commit", vec![]),
        );
        let repeated = AgentObservation::tool_result(
            0,
            "read_file",
            "same payload",
            Some(ObservationKind::Text),
        );
        let repeated_again = AgentObservation::tool_result(
            1,
            "read_file",
            "same payload",
            Some(ObservationKind::Text),
        );
        let latest = AgentObservation::tool_result(
            2,
            "write_file",
            "different payload",
            Some(ObservationKind::FileMutation),
        );
        trace.record_observation(&repeated);
        trace.record_observation(&repeated_again);
        trace.record_observation(&latest);

        let selected = select_previous_observations(&trace, Some(&latest));
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].summary, "same payload");
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

        let (business, _) = load_business_context_snapshot(
            Some(db_path.as_path()),
            &trace,
            MemoryBudget::default(),
            &RuleRetriever::new(&db_path),
            false,
        )
        .expect("读取业务上下文失败");
        let business = business.expect("应存在业务上下文");

        assert_eq!(
            business
                .current_task
                .as_ref()
                .map(|value| value.task_id.as_str()),
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
        let (business, _) = load_business_context_snapshot(
            Some(db_path.as_path()),
            &trace,
            MemoryBudget::default(),
            &RuleRetriever::new(&db_path),
            false,
        )
        .expect("读取业务上下文失败");
        let business = business.expect("应存在业务上下文");
        let runtime_session_state =
            derive_runtime_session_state(&trace, "帮我看任务", None, Some(&business));

        let planner_input = ContextAssembler::default().assemble(
            &trace,
            "帮我看任务",
            None,
            Some(&runtime_session_state),
            &["get_task_status: 查询单个任务状态，参数: task_id".to_string()],
            Some(&business),
        );

        assert!(planner_input
            .assembled_user_prompt
            .contains("## Current Task"));
        assert!(planner_input
            .assembled_user_prompt
            .contains(&current.task_id));
        assert!(planner_input
            .assembled_user_prompt
            .contains("## Recent Tasks"));
        assert!(planner_input
            .context_sections
            .iter()
            .any(|section| section.kind == "current_task"));
        assert!(planner_input
            .context_sections
            .iter()
            .any(|section| section.kind == "recent_tasks"));
    }

    #[test]
    fn preview_context_renders_budget_and_sections() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        store
            .add_user_memory("user-preview", "我喜欢短摘要")
            .expect("写入 memory 失败");
        let agent = AgentCore::with_task_store_db_path(workspace, &db_path).expect("初始化失败");

        let preview = agent
            .preview_context_with_context(
                "帮我总结一下当前上下文",
                AgentRunContext::wechat_chat("user-preview", "context_debug", vec![])
                    .with_session_text("帮我总结一下当前上下文"),
            )
            .expect("应成功生成 preview");

        assert!(preview.contains("# Context Preview"));
        assert!(preview.contains("## Session State"));
        assert!(preview.contains("## Sections"));
        assert!(preview.contains("context_budget: final="));
        assert!(preview.contains("kind=user_memories"));
        assert!(preview.contains("## Prompt Preview"));
    }

    #[test]
    fn preview_context_verbose_includes_section_content_and_memory_drop_details() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        store
            .add_user_memory("user-preview", "这是一条很长的记忆 ".repeat(40).as_str())
            .expect("写入 memory 失败");
        let agent = AgentCore::with_task_store_db_path(workspace, &db_path).expect("初始化失败");

        let preview = agent
            .preview_context_with_context_mode(
                "帮我总结一下当前上下文",
                AgentRunContext::wechat_chat("user-preview", "context_debug", vec![])
                    .with_session_text("帮我总结一下当前上下文"),
                ContextPreviewMode::Verbose,
            )
            .expect("应成功生成 preview");

        assert!(preview.contains("## Memory Dropped"));
        assert!(preview.contains("single_item_too_long"));
        assert!(preview.contains("## Section Content Preview"));
        // 低价值 SessionState（仅有默认 goal）已被过滤，不再断言 session_state section
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

        let (business, session_state) = load_business_context_snapshot(
            Some(db_path.as_path()),
            &trace,
            MemoryBudget::default(),
            &RuleRetriever::new(&db_path),
            true,
        )
        .expect("读取业务上下文失败");
        let business = business.expect("应存在业务上下文");

        assert_eq!(business.user_memories.len(), 1);
        assert_eq!(business.user_memories[0].content, "我喜欢短摘要");
        assert_eq!(session_state.injected_count(), 1);
        assert_eq!(session_state.injected_ids().len(), 1);
        // 验证 feedback 写回：retrieved/injected 增长，use_count 不受注入影响
        let after = store
            .search_user_memories("user-memory", 15)
            .expect("再次检索失败");
        assert_eq!(after[0].retrieved_count, 1);
        assert_eq!(after[0].injected_count, 1);
        assert_eq!(after[0].use_count, 0);
    }

    #[test]
    fn memory_user_isolation_prevents_cross_user_leak() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        store
            .add_user_memory("user-a", "用户A的偏好")
            .expect("写入 memory 失败");
        store
            .add_user_memory("user-b", "用户B的偏好")
            .expect("写入 memory 失败");
        // user-b 的 trace 不应看到 user-a 的记忆
        let trace = AgentRunTrace::new(
            &workspace,
            "帮我总结",
            AgentRunContext::wechat_chat("user-b", "commit", vec![]),
        );
        let (business, session_state) = load_business_context_snapshot(
            Some(db_path.as_path()),
            &trace,
            MemoryBudget::default(),
            &RuleRetriever::new(&db_path),
            false,
        )
        .expect("读取业务上下文失败");
        let business = business.expect("应存在业务上下文");
        assert_eq!(business.user_memories.len(), 1);
        assert_eq!(business.user_memories[0].content, "用户B的偏好");
        assert_eq!(session_state.injected_count(), 1);
    }

    #[test]
    fn memory_budget_trimming_removes_excess() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        // 写 6 条记忆，预算 max_items=5 应只注入 5 条
        for i in 0..6 {
            store
                .add_user_memory_typed(
                    "user-budget",
                    &format!("记忆内容 {}", i),
                    crate::task_store::MemoryType::Explicit,
                    100,
                )
                .expect("写入 memory 失败");
        }
        let trace = AgentRunTrace::new(
            &workspace,
            "帮我总结",
            AgentRunContext::wechat_chat("user-budget", "commit", vec![]),
        );
        let (business, session_state) = load_business_context_snapshot(
            Some(db_path.as_path()),
            &trace,
            MemoryBudget::default(),
            &RuleRetriever::new(&db_path),
            false,
        )
        .expect("读取业务上下文失败");
        let business = business.expect("应存在业务上下文");
        // injected = 5（max_items），retrieved = 6
        assert_eq!(business.user_memories.len(), 5);
        assert_eq!(session_state.retrieved_count(), 6);
        assert_eq!(session_state.injected_count(), 5);
        assert_eq!(session_state.dropped.len(), 1);
    }

    #[test]
    fn memory_no_user_id_degrades_gracefully() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        store
            .add_user_memory("user-x", "有些记忆")
            .expect("写入 memory 失败");
        // 没有 user_id 的 trace（agent_demo）
        let trace = AgentRunTrace::new(&workspace, "帮我总结", AgentRunContext::agent_demo());
        let (business, session_state) = load_business_context_snapshot(
            Some(db_path.as_path()),
            &trace,
            MemoryBudget::default(),
            &RuleRetriever::new(&db_path),
            false,
        )
        .expect("读取业务上下文失败");
        let business = business.expect("应存在业务上下文");
        assert!(business.user_memories.is_empty());
        assert_eq!(session_state.injected_count(), 0);
    }

    #[test]
    fn session_state_dedup_marks_dropped() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        store
            .add_user_memory("user-dedup", "偏好 短摘要")
            .expect("写入失败");
        store
            .add_user_memory("user-dedup", "偏好  短摘要") // 多空格
            .expect("写入失败");
        let trace = AgentRunTrace::new(
            &workspace,
            "帮我总结",
            AgentRunContext::wechat_chat("user-dedup", "commit", vec![]),
        );
        let (_, session_state) = load_business_context_snapshot(
            Some(db_path.as_path()),
            &trace,
            MemoryBudget::default(),
            &RuleRetriever::new(&db_path),
            false,
        )
        .expect("读取业务上下文失败");
        // 1 injected, 1 dropped (dedup)
        assert_eq!(session_state.injected_count(), 1);
        assert_eq!(session_state.dropped.len(), 1);
        assert_eq!(session_state.dropped[0].reason, DropReason::Deduplicated);
    }

    #[test]
    fn session_state_trim_marks_budget_exceeded() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        for i in 0..7 {
            store
                .add_user_memory("user-trim", &format!("记忆内容 {}", i))
                .expect("写入失败");
        }
        let trace = AgentRunTrace::new(
            &workspace,
            "帮我总结",
            AgentRunContext::wechat_chat("user-trim", "commit", vec![]),
        );
        let (_, session_state) = load_business_context_snapshot(
            Some(db_path.as_path()),
            &trace,
            MemoryBudget::default(),
            &RuleRetriever::new(&db_path),
            false,
        )
        .expect("读取业务上下文失败");
        // max_items=5, 7 条 → 5 injected, 2 dropped (budget exceeded)
        assert_eq!(session_state.injected_count(), 5);
        assert_eq!(session_state.dropped.len(), 2);
        assert_eq!(session_state.dropped[0].reason, DropReason::BudgetExceeded);
        assert_eq!(session_state.dropped[1].reason, DropReason::BudgetExceeded);
    }

    #[test]
    fn session_state_single_item_too_long_marks_dropped() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let long_content: String = "很".repeat(200);
        store
            .add_user_memory("user-long", &long_content)
            .expect("写入失败");
        store
            .add_user_memory("user-long", "短记忆")
            .expect("写入失败");
        let trace = AgentRunTrace::new(
            &workspace,
            "帮我总结",
            AgentRunContext::wechat_chat("user-long", "commit", vec![]),
        );
        let (_, session_state) = load_business_context_snapshot(
            Some(db_path.as_path()),
            &trace,
            MemoryBudget::default(),
            &RuleRetriever::new(&db_path),
            false,
        )
        .expect("读取业务上下文失败");
        // 1 injected (短记忆), 1 dropped (too long)
        assert_eq!(session_state.injected_count(), 1);
        assert_eq!(session_state.dropped.len(), 1);
        assert_eq!(
            session_state.dropped[0].reason,
            DropReason::SingleItemTooLong
        );
    }

    #[test]
    fn session_state_multi_turn_isolation() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        store
            .add_user_memory("user-iso", "用户偏好")
            .expect("写入失败");
        // 第一轮
        let trace1 = AgentRunTrace::new(
            &workspace,
            "帮我总结",
            AgentRunContext::wechat_chat("user-iso", "commit", vec![]),
        );
        let (_, ss1) = load_business_context_snapshot(
            Some(db_path.as_path()),
            &trace1,
            MemoryBudget::default(),
            &RuleRetriever::new(&db_path),
            false,
        )
        .expect("读取业务上下文失败");
        // 第二轮（同一用户，不同 run）
        let trace2 = AgentRunTrace::new(
            &workspace,
            "再帮我总结",
            AgentRunContext::wechat_chat("user-iso", "commit", vec![]),
        );
        let (_, ss2) = load_business_context_snapshot(
            Some(db_path.as_path()),
            &trace2,
            MemoryBudget::default(),
            &RuleRetriever::new(&db_path),
            false,
        )
        .expect("读取业务上下文失败");
        // 两轮独立，互不泄漏
        assert_eq!(ss1.injected_count(), 1);
        assert_eq!(ss2.injected_count(), 1);
        assert!(ss1.dropped.is_empty());
        assert!(ss2.dropped.is_empty());
        // SessionState 不持有可变共享状态
        assert_ne!(ss1.injected_ids()[0], ""); // 只是验证非空
    }

    #[test]
    fn memory_v1_types_are_injected_with_labels() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        store
            .add_user_memory_typed("user-v1", "我喜欢短回复", MemoryType::UserPreference, 80)
            .expect("写入失败");
        store
            .add_user_memory_typed("user-v1", "AMClaw 使用 Rust", MemoryType::ProjectFact, 85)
            .expect("写入失败");
        store
            .add_user_memory_typed("user-v1", "抓取失败时应提示用户", MemoryType::Lesson, 75)
            .expect("写入失败");

        let trace = AgentRunTrace::new(
            &workspace,
            "帮我总结",
            AgentRunContext::wechat_chat("user-v1", "commit", vec![]),
        );
        let (business, _) = load_business_context_snapshot(
            Some(db_path.as_path()),
            &trace,
            MemoryBudget::default(),
            &RuleRetriever::new(&db_path),
            false,
        )
        .expect("读取业务上下文失败");
        let business = business.expect("应存在业务上下文");

        assert_eq!(business.user_memories.len(), 3);
        let labels: Vec<&str> = business
            .user_memories
            .iter()
            .map(|m| m.memory_type.label_prefix())
            .collect();
        assert!(labels.contains(&"[偏好]"));
        assert!(labels.contains(&"[项目]"));
        assert!(labels.contains(&"[经验]"));
    }

    #[test]
    fn memory_budget_trimming_for_typed_memories() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        // 写入 6 条不同类型 memory，max_items=5 应裁剪到 5 条
        for i in 0..6 {
            let (content, mtype, priority) = match i % 3 {
                0 => (format!("偏好 {}", i), MemoryType::UserPreference, 80i64),
                1 => (format!("项目事实 {}", i), MemoryType::ProjectFact, 85i64),
                _ => (format!("经验 {}", i), MemoryType::Lesson, 75i64),
            };
            store
                .add_user_memory_typed("user-trim-v1", &content, mtype, priority)
                .expect("写入失败");
        }

        let trace = AgentRunTrace::new(
            &workspace,
            "帮我总结",
            AgentRunContext::wechat_chat("user-trim-v1", "commit", vec![]),
        );
        let (business, session_state) = load_business_context_snapshot(
            Some(db_path.as_path()),
            &trace,
            MemoryBudget::default(),
            &RuleRetriever::new(&db_path),
            false,
        )
        .expect("读取业务上下文失败");
        let business = business.expect("应存在业务上下文");

        assert_eq!(business.user_memories.len(), 5);
        assert_eq!(session_state.retrieved_count(), 6);
        assert_eq!(session_state.injected_count(), 5);
        assert_eq!(session_state.dropped.len(), 1);
        // project_fact(85) 优先级最高，应被保留
        assert!(business
            .user_memories
            .iter()
            .any(|m| m.memory_type == MemoryType::ProjectFact));
    }

    #[test]
    fn memory_feedback_retrieved_and_injected_are_recorded() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let created = store
            .add_user_memory_typed("user-fb", "这是测试记忆", MemoryType::UserPreference, 80)
            .expect("写入失败");

        let trace = AgentRunTrace::new(
            &workspace,
            "帮我总结",
            AgentRunContext::wechat_chat("user-fb", "commit", vec![]),
        );
        let _ = load_business_context_snapshot(
            Some(db_path.as_path()),
            &trace,
            MemoryBudget::default(),
            &RuleRetriever::new(&db_path),
            true,
        )
        .expect("读取业务上下文失败");

        // 重新打开 DB 验证 feedback 已回写
        let store2 = TaskStore::open(&db_path).expect("重新打开失败");
        let memories = store2.list_user_memories("user-fb", 10).expect("查询失败");
        assert_eq!(memories.len(), 1);
        let mem = &memories[0];
        assert_eq!(mem.id, created.id);
        assert!(
            mem.retrieved_count > 0,
            "retrieved_count 应被累加，当前={}",
            mem.retrieved_count
        );
        assert!(
            mem.injected_count > 0,
            "injected_count 应被累加，当前={}",
            mem.injected_count
        );
    }

    #[test]
    fn suppressed_typed_memory_not_injected() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let created = store
            .add_user_memory_typed("user-sup", "将被抑制的偏好", MemoryType::UserPreference, 80)
            .expect("写入失败");
        store
            .suppress_memory("user-sup", &created.id)
            .expect("抑制失败");

        let trace = AgentRunTrace::new(
            &workspace,
            "帮我总结",
            AgentRunContext::wechat_chat("user-sup", "commit", vec![]),
        );
        let (business, session_state) = load_business_context_snapshot(
            Some(db_path.as_path()),
            &trace,
            MemoryBudget::default(),
            &RuleRetriever::new(&db_path),
            false,
        )
        .expect("读取业务上下文失败");
        let business = business.expect("应存在业务上下文");
        assert!(business.user_memories.is_empty());
        assert_eq!(session_state.injected_count(), 0);
        assert_eq!(session_state.retrieved_count(), 0);
    }

    #[test]
    fn trace_contains_typed_memory_observability_fields() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        store
            .add_user_memory_typed("user-trace", "用户偏好内容", MemoryType::UserPreference, 80)
            .expect("写入失败");
        store
            .add_user_memory_typed("user-trace", "项目事实内容", MemoryType::ProjectFact, 85)
            .expect("写入失败");

        let mut trace = AgentRunTrace::new(
            &workspace,
            "帮我总结",
            AgentRunContext::wechat_chat("user-trace", "commit", vec![]),
        );
        let (_, session_state) = load_business_context_snapshot(
            Some(db_path.as_path()),
            &trace,
            MemoryBudget::default(),
            &RuleRetriever::new(&db_path),
            false,
        )
        .expect("读取业务上下文失败");

        // 模拟 agent_core 中对 trace 的赋值
        if session_state.has_memory_activity() {
            trace.memory_hit_count = session_state.injected_count();
            trace.memory_retrieved_count = session_state.retrieved_count();
            trace.memory_total_chars = session_state.injected_total_chars();
            trace.memory_dropped_count = session_state.dropped.len();
            trace.memory_ids = session_state.injected_ids();
        }

        assert_eq!(trace.memory_hit_count, 2);
        assert_eq!(trace.memory_retrieved_count, 2);
        assert_eq!(trace.memory_dropped_count, 0);
        assert_eq!(trace.memory_ids.len(), 2);
        assert!(trace.memory_total_chars > 0);

        // 验证 trace 渲染包含 memory 字段
        let rendered = trace.to_markdown();
        assert!(rendered.contains("memory_hit_count"));
        assert!(rendered.contains("memory_retrieved_count"));
        assert!(rendered.contains("memory_dropped_count"));
    }

    #[test]
    fn agent_core_uses_retriever_trait_not_store_directly() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        store
            .add_user_memory_typed("user-mock", "mock 记忆", MemoryType::UserPreference, 80)
            .expect("写入失败");

        // Mock retriever：固定返回一条自定义内容
        struct MockRetriever;
        impl crate::retriever::Retriever for MockRetriever {
            fn retrieve(
                &self,
                _query: &crate::retriever::RetrieveQuery,
            ) -> anyhow::Result<crate::retriever::RetrieveResult> {
                let mut metadata = std::collections::BTreeMap::new();
                metadata.insert("memory_type".to_string(), "user_preference".to_string());
                metadata.insert("priority".to_string(), "80".to_string());
                metadata.insert("status".to_string(), "active".to_string());
                Ok(crate::retriever::RetrieveResult {
                    candidates: vec![crate::retriever::RetrievedItem {
                        id: "mock-id".to_string(),
                        content: "mock 检索结果".to_string(),
                        score: Some(0.8),
                        source_type: "user_preference".to_string(),
                        metadata,
                    }],
                    hit_count: 1,
                    dropped_count: 0,
                    latency_ms: 42,
                    retriever_name: "mock_test".to_string(),
                })
            }
        }

        let trace = AgentRunTrace::new(
            &workspace,
            "帮我总结",
            AgentRunContext::wechat_chat("user-mock", "commit", vec![]),
        );
        let (business, session_state) = load_business_context_snapshot(
            Some(db_path.as_path()),
            &trace,
            MemoryBudget::default(),
            &MockRetriever,
            false,
        )
        .expect("读取业务上下文失败");
        let business = business.expect("应存在业务上下文");

        // Mock retriever 的结果被注入
        assert_eq!(business.user_memories.len(), 1);
        assert_eq!(business.user_memories[0].content, "mock 检索结果");
        assert_eq!(session_state.retriever_name, "mock_test");
        assert_eq!(session_state.retrieval_latency_ms, 42);
    }

    #[test]
    fn retrieval_observability_fields_present_in_trace() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        store
            .add_user_memory_typed("user-obs", "观测测试", MemoryType::ProjectFact, 85)
            .expect("写入失败");

        let mut trace = AgentRunTrace::new(
            &workspace,
            "帮我总结",
            AgentRunContext::wechat_chat("user-obs", "commit", vec![]),
        );
        let (_, session_state) = load_business_context_snapshot(
            Some(db_path.as_path()),
            &trace,
            MemoryBudget::default(),
            &RuleRetriever::new(&db_path),
            false,
        )
        .expect("读取业务上下文失败");

        project_session_state_to_trace(&mut trace, &session_state);

        assert_eq!(trace.retriever_name, "rule_v1");
        assert!(trace.retrieval_latency_ms > 0);
        assert_eq!(trace.retrieval_candidate_count, 1);
        assert_eq!(trace.retrieval_hit_count, 1);

        let rendered = trace.to_markdown();
        assert!(
            rendered.contains("rule_v1"),
            "trace markdown 应包含 retriever 名称"
        );
        assert!(
            rendered.contains("latency="),
            "trace markdown 应包含 latency"
        );
    }

    #[test]
    fn legacy_flow_no_regression_with_default_rule_retriever() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        store
            .add_user_memory("user-legacy", "旧流程记忆")
            .expect("写入失败");

        let trace = AgentRunTrace::new(
            &workspace,
            "帮我总结",
            AgentRunContext::wechat_chat("user-legacy", "commit", vec![]),
        );
        let retriever = RuleRetriever::new(&db_path);
        let (business, session_state) = load_business_context_snapshot(
            Some(db_path.as_path()),
            &trace,
            MemoryBudget::default(),
            &retriever,
            false,
        )
        .expect("读取业务上下文失败");
        let business = business.expect("应存在业务上下文");

        // 与修改前完全一致的断言
        assert_eq!(business.user_memories.len(), 1);
        assert_eq!(business.user_memories[0].content, "旧流程记忆");
        assert_eq!(session_state.injected_count(), 1);
        assert_eq!(session_state.retrieved_count(), 1);
        assert_eq!(session_state.dropped.len(), 0);
        assert_eq!(session_state.retriever_name, "rule_v1");
    }

    #[test]
    fn preview_context_does_not_apply_memory_feedback() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let created = store
            .add_user_memory_typed(
                "user-preview-fb",
                "我喜欢短摘要",
                MemoryType::UserPreference,
                80,
            )
            .expect("写入失败");
        // 初始 feedback 计数应为 0
        let before = store
            .list_user_memories("user-preview-fb", 10)
            .expect("查询失败");
        assert_eq!(before[0].retrieved_count, 0);
        assert_eq!(before[0].injected_count, 0);

        let agent = AgentCore::with_max_steps_and_task_store_db_path(&workspace, 5, Some(&db_path))
            .expect("初始化 agent 失败");
        let _preview = agent
            .preview_context_with_context(
                "帮我总结",
                AgentRunContext::wechat_chat("user-preview-fb", "commit", vec![]),
            )
            .expect("preview 应成功");

        // preview 不应写 DB feedback
        let after = store
            .list_user_memories("user-preview-fb", 10)
            .expect("查询失败");
        assert_eq!(after[0].id, created.id);
        assert_eq!(
            after[0].retrieved_count, 0,
            "preview_context 不应修改 retrieved_count"
        );
        assert_eq!(
            after[0].injected_count, 0,
            "preview_context 不应修改 injected_count"
        );
    }

    #[test]
    fn memory_feedback_applied_once_per_run() {
        let workspace = temp_workspace();
        let db_path = temp_db_path();
        // 预先在工作区创建文件，确保 Read 工具成功执行，避免 watchdog 介入
        std::fs::write(workspace.join("demo.txt"), "hello").expect("创建测试文件失败");

        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let _created = store
            .add_user_memory_typed(
                "user-run-fb",
                "我喜欢短摘要",
                MemoryType::UserPreference,
                80,
            )
            .expect("写入失败");

        let agent = AgentCore::with_max_steps_and_task_store_db_path(&workspace, 5, Some(&db_path))
            .expect("初始化 agent 失败");
        // 模拟 3 步 run：2 个 tool + 1 个 final
        agent.scripted_decisions.borrow_mut().extend([
            PlannedDecision::new(super::AgentDecision::CallTool(super::ToolAction::Read {
                path: "demo.txt".to_string(),
            })),
            PlannedDecision::new(super::AgentDecision::CallTool(super::ToolAction::Read {
                path: "demo.txt".to_string(),
            })),
            PlannedDecision::new(super::AgentDecision::Final("done".to_string())),
        ]);

        let result = agent
            .run_with_context(
                "帮我总结",
                AgentRunContext::wechat_chat("user-run-fb", "commit", vec![]),
            )
            .expect("run 应成功");
        assert_eq!(result.output, "done", "run 输出应收敛到 scripted final");

        // 重新打开 DB 验证 feedback 只写了一次
        let store2 = TaskStore::open(&db_path).expect("重新打开失败");
        let memories = store2
            .list_user_memories("user-run-fb", 10)
            .expect("查询失败");
        assert_eq!(memories.len(), 1);
        let mem = &memories[0];
        assert_eq!(
            mem.retrieved_count, 1,
            "3 步 run 应只写一次 retrieved feedback, 当前={}",
            mem.retrieved_count
        );
        assert_eq!(
            mem.injected_count, 1,
            "3 步 run 应只写一次 injected feedback, 当前={}",
            mem.injected_count
        );
    }

    #[test]
    fn invalid_retriever_mode_fails_explicitly() {
        let err = RetrieverMode::from_config("unknown_mode").expect_err("非法 mode 应报错");
        let msg = err.to_string();
        assert!(
            msg.contains("非法 retriever_mode"),
            "错误信息应提示非法 mode, 实际: {msg}"
        );
        assert!(
            msg.contains("rule, semantic, hybrid, shadow"),
            "错误信息应列出合法值, 实际: {msg}"
        );
    }

    #[test]
    fn semantic_mode_falls_back_to_rule_with_fallback_name() {
        let _workspace = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        store
            .add_user_memory_typed(
                "user-semantic-fb",
                "我喜欢短摘要",
                MemoryType::UserPreference,
                80,
            )
            .expect("写入失败");

        let retriever = select_retriever(RetrieverMode::Semantic, Some(&db_path), "noop");
        let query = crate::retriever::RetrieveQuery::new("user-semantic-fb", 10);
        let result = retriever.retrieve(&query).expect("检索应成功");

        // 回退到 rule，但有 fallback 标识
        assert!(
            result.retriever_name.contains("fallback"),
            "semantic 回退时 retriever_name 应包含 fallback, 实际: {}",
            result.retriever_name
        );
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].content, "我喜欢短摘要");
    }

    #[test]
    fn hybrid_mode_returns_hybrid_retriever_with_fallback() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        store
            .add_user_memory_typed(
                "user-hybrid-sel",
                "混合检索测试",
                MemoryType::UserPreference,
                80,
            )
            .expect("写入失败");

        let retriever = select_retriever(RetrieverMode::Hybrid, Some(&db_path), "noop");
        let query =
            crate::retriever::RetrieveQuery::new("user-hybrid-sel", 10).with_query_text("测试");
        let result = retriever.retrieve(&query).expect("检索应成功");

        // Hybrid 使用 NoOpEmbeddingProvider，会 fallback 到 rule
        assert_eq!(result.retriever_name, "hybrid_v1_fallback");
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].content, "混合检索测试");
        // fallback 结果应带 retrieval_mode=hybrid_fallback
        assert_eq!(
            result.candidates[0].metadata.get("retrieval_mode"),
            Some(&"hybrid_fallback".to_string())
        );
    }

    #[test]
    fn shadow_mode_returns_shadow_retriever_with_rule_output() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        store
            .add_user_memory_typed(
                "user-shadow-sel",
                "Shadow 测试",
                MemoryType::UserPreference,
                80,
            )
            .expect("写入失败");

        let retriever = select_retriever(RetrieverMode::Shadow, Some(&db_path), "noop");
        let query =
            crate::retriever::RetrieveQuery::new("user-shadow-sel", 10).with_query_text("测试");
        let result = retriever.retrieve(&query).expect("检索应成功");

        // Shadow 对外始终返回 rule 结果
        assert_eq!(result.retriever_name, "shadow_v1");
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].content, "Shadow 测试");
        // 候选应带 retrieval_mode=shadow
        assert_eq!(
            result.candidates[0].metadata.get("retrieval_mode"),
            Some(&"shadow".to_string())
        );
    }

    #[test]
    fn rule_mode_returns_rule_retriever_directly() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        store
            .add_user_memory_typed(
                "user-rule-sel",
                "规则检索测试",
                MemoryType::UserPreference,
                80,
            )
            .expect("写入失败");

        let retriever = select_retriever(RetrieverMode::Rule, Some(&db_path), "noop");
        let query = crate::retriever::RetrieveQuery::new("user-rule-sel", 10);
        let result = retriever.retrieve(&query).expect("检索应成功");

        assert_eq!(result.retriever_name, "rule_v1");
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].content, "规则检索测试");
    }

    #[test]
    fn context_summary_contains_core_runtime_signals() {
        let workspace = temp_workspace();
        let mut trace = AgentRunTrace::new(
            &workspace,
            "读文件 demo.txt",
            AgentRunContext::wechat_chat("user-summary", "timeout", vec!["msg-9".to_string()])
                .with_task_id("task-summary")
                .with_context_token_present(true),
        );
        trace.record_decision(
            0,
            "scripted",
            &PlannedDecision::new(super::AgentDecision::CallTool(super::ToolAction::Read {
                path: "demo.txt".to_string(),
            }))
            .with_plan(Some(ExecutionPlan {
                steps: vec!["读取 demo.txt".to_string()],
            })),
        );
        let observation = AgentObservation::tool_result(
            2,
            "read_file",
            "summary text",
            Some(ObservationKind::Text),
        );
        let summary = build_context_summary(&trace, Some(&observation));

        assert!(summary.contains("source=wechat_chat"));
        assert!(summary.contains("trigger=timeout"));
        assert!(summary.contains("user=user-summary"));
        assert!(summary.contains("messages=1"));
        assert!(summary.contains("replans=0/3"));
        assert!(summary.contains("failures=0"));
        assert!(summary.contains("current_step=1"));
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
            expected_fields: None,
            minimum_novelty: None,
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

        let archive =
            parse_llm_plan("{\"action\":\"read_article_archive\",\"task_id\":\"task-2\"}")
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
            r#"{"action":"get_task_status","task_id":"task-1","expected_kind":"task_status","done_rule":"required_json_field","required_field":"found","expected_fields":["found","task"],"minimum_novelty":"different_from_last"}"#,
        )
        .expect("带 expected_observation 的 LLM JSON 解析失败");

        assert!(matches!(
            planned.expected_observation,
            Some(ExpectedObservation {
                kind: ObservationKind::TaskStatus,
                done_rule: DoneRule::RequiresJsonField { .. },
                expected_fields,
                minimum_novelty: Some(MinimumNovelty::DifferentFromLast),
            }) if expected_fields == vec!["found".to_string(), "task".to_string()]
        ));
    }

    #[test]
    fn validate_expected_observation_checks_expected_fields() {
        let observation = AgentObservation::tool_result(
            1,
            "tool:get_task_status",
            r#"{"found":true}"#,
            Some(ObservationKind::TaskStatus),
        );

        let err = validate_expected_observation(
            Some(&ExpectedObservation {
                kind: ObservationKind::TaskStatus,
                done_rule: DoneRule::RequiresJsonField {
                    field: "found".to_string(),
                },
                expected_fields: vec!["found".to_string(), "task".to_string()],
                minimum_novelty: Some(MinimumNovelty::DifferentFromLast),
            }),
            &observation,
        )
        .expect_err("缺少 expected_fields 字段时应失败");

        assert!(err.to_string().contains("expected_fields"));
    }

    #[test]
    fn transient_failure_is_classified_as_retry_step() {
        let failure =
            classify_tool_execution_failure("tool:read".to_string(), "operation timed out");
        assert_eq!(failure.kind, StepFailureKind::Transient);
        assert_eq!(failure.action, FailureAction::RetryStep);
    }

    #[test]
    fn trace_context_pack_fields_present_after_run() {
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

        assert_eq!(payload["context_pack_present"], true);
        assert!(payload["context_pack_section_count"].as_u64().unwrap_or(0) > 0);
        assert!(payload["context_pack_total_chars"].as_u64().unwrap_or(0) > 0);
        assert!(payload["context_pack_drop_reasons"].is_array());
    }

    #[test]
    fn trace_context_pack_fields_populated_on_budget_trim() {
        let workspace = temp_workspace();
        let trace = AgentRunTrace::new(
            &workspace,
            "请帮我处理一个很长的上下文请求",
            AgentRunContext::wechat_chat("user-budget", "commit", vec![])
                .with_session_text("session ".repeat(240)),
        );
        let observation = AgentObservation::tool_result(
            1,
            "read_file",
            &"observation ".repeat(240),
            Some(ObservationKind::Text),
        );
        let runtime_session_state = RuntimeSessionStateSnapshot {
            goal: Some("推进一个需要大量上下文的信息整理任务".to_string()),
            current_subtask: Some("先整理线索，再决定是否继续调工具".to_string()),
            constraints: vec!["避免重复无效动作".to_string(); 4],
            confirmed_facts: vec![
                "已有较长 session_text 和 latest_observation".to_string();
                4
            ],
            done_items: vec!["已完成初步读取".to_string(); 3],
            next_step: Some("优先收敛上下文而不是继续膨胀 prompt".to_string()),
            open_questions: vec!["哪些 section 可以先被丢弃".to_string(); 3],
            goal_signal: GoalSignal::PersistentHigh,
        };
        let mut pack = build_context_pack(
            &trace,
            "请帮我处理一个很长的上下文请求",
            Some(&observation),
            Some(&runtime_session_state),
            &[
                format!("read: {}", "tool ".repeat(120)),
                format!("write: {}", "tool ".repeat(120)),
            ],
            None,
            SessionSummaryStrategy::Semantic,
            false,
        );
        pack.set_max_total_chars(1500);
        pack.apply_total_budget();

        assert!(pack.budget_summary().final_total_chars <= 1500);
        assert!(!pack.drop_reasons().is_empty());
    }

    #[test]
    fn session_state_v2_all_slots_injected_into_prompt() {
        let workspace = temp_workspace();
        let mut trace = AgentRunTrace::new(
            &workspace,
            "帮我整理任务",
            AgentRunContext::wechat_chat("user-v2", "commit", vec![]).with_user_session_state(
                Some(crate::task_store::UserSessionStateRecord {
                    user_id: "user-v2".to_string(),
                    goal: Some("整理待办任务".to_string()),
                    current_subtask: Some("读取最近任务".to_string()),
                    next_step: Some("确认是否需要重试".to_string()),
                    constraints_json: Some(r#"["时间有限","优先高优先级"]"#.to_string()),
                    confirmed_facts_json: Some(r#"["有3个pending任务"]"#.to_string()),
                    done_items_json: Some(r#"["已登录"]"#.to_string()),
                    open_questions_json: Some(r#"["是否需要通知用户"]"#.to_string()),
                    ..Default::default()
                }),
            ),
        );

        let runtime_session_state =
            derive_runtime_session_state(&trace, "帮我整理任务", None, None);

        // 7 槽位都应存在
        assert!(runtime_session_state.goal.is_some());
        assert!(runtime_session_state.current_subtask.is_some());
        assert!(!runtime_session_state.constraints.is_empty());
        assert!(!runtime_session_state.confirmed_facts.is_empty());
        assert!(!runtime_session_state.done_items.is_empty());
        assert!(runtime_session_state.next_step.is_some());
        assert!(!runtime_session_state.open_questions.is_empty());
        assert!(!runtime_session_state.is_empty());
        assert!(!runtime_session_state.is_low_signal());

        trace.record_session_state_snapshot(runtime_session_state.clone());

        // 验证 prompt 中确实包含 session state section
        let pack = build_context_pack(
            &trace,
            "帮我整理任务",
            None,
            Some(&runtime_session_state),
            &[],
            None,
            SessionSummaryStrategy::Semantic,
            false,
        );
        let rendered = pack.render();
        assert!(
            rendered.contains("Session State"),
            "prompt 应包含 Session State section"
        );
        assert!(rendered.contains("goal:"), "prompt 应包含 goal");
        assert!(
            rendered.contains("current_subtask:"),
            "prompt 应包含 current_subtask"
        );
    }

    #[test]
    fn session_state_low_signal_only_for_runtime_default_goal() {
        let workspace = temp_workspace();
        // 不设置 persistent goal，让 derive 走 RuntimeDefault 路径
        let trace = AgentRunTrace::new(
            &workspace,
            "你好",
            AgentRunContext::wechat_chat("user-low", "commit", vec![]),
        );

        let runtime_session_state = derive_runtime_session_state(&trace, "你好", None, None);

        assert!(!runtime_session_state.is_empty());
        assert_eq!(
            runtime_session_state.goal_signal,
            GoalSignal::RuntimeDefault,
            "无 persistent state 时应为 RuntimeDefault"
        );
        assert!(runtime_session_state.is_low_signal());

        // prompt 中不应出现 Session State section
        let pack = build_context_pack(
            &trace,
            "你好",
            None,
            Some(&runtime_session_state),
            &[],
            None,
            SessionSummaryStrategy::Semantic,
            false,
        );
        assert!(
            !pack.render().contains("Session State"),
            "低信号 state 不应注入 prompt"
        );
    }

    #[test]
    fn session_state_persistent_goal_not_filtered_even_if_template_like_text() {
        let workspace = temp_workspace();
        // persistent goal 是模板类文本，但因来源是 PersistentHigh，不应被过滤
        let trace = AgentRunTrace::new(
            &workspace,
            "你好",
            AgentRunContext::wechat_chat("user-goal", "commit", vec![]).with_user_session_state(
                Some(crate::task_store::UserSessionStateRecord {
                    user_id: "user-goal".to_string(),
                    goal: Some("响应当前用户请求：你好".to_string()),
                    ..Default::default()
                }),
            ),
        );

        let runtime_session_state = derive_runtime_session_state(&trace, "你好", None, None);

        assert_eq!(
            runtime_session_state.goal_signal,
            GoalSignal::PersistentHigh,
            "persistent goal 应为 PersistentHigh"
        );
        assert!(
            !runtime_session_state.is_low_signal(),
            "PersistentHigh 不应被过滤，即使文本像模板"
        );

        let pack = build_context_pack(
            &trace,
            "你好",
            None,
            Some(&runtime_session_state),
            &[],
            None,
            SessionSummaryStrategy::Semantic,
            false,
        );
        assert!(pack.render().contains("Session State"));
    }

    #[test]
    fn session_state_persistent_fallback_goal_is_not_low_signal() {
        let workspace = temp_workspace();
        // 有 last_user_intent 但无 goal，derive 走 PersistentFallback
        let trace = AgentRunTrace::new(
            &workspace,
            "你好",
            AgentRunContext::wechat_chat("user-fb", "commit", vec![]).with_user_session_state(
                Some(crate::task_store::UserSessionStateRecord {
                    user_id: "user-fb".to_string(),
                    last_user_intent: Some("整理本周待办".to_string()),
                    ..Default::default()
                }),
            ),
        );

        let runtime_session_state = derive_runtime_session_state(&trace, "你好", None, None);

        assert_eq!(
            runtime_session_state.goal_signal,
            GoalSignal::PersistentFallback,
        );
        assert!(!runtime_session_state.is_low_signal());
    }

    #[test]
    fn trace_contains_session_state_observability_fields() {
        let workspace = temp_workspace();
        let trace = AgentRunTrace::new(
            &workspace,
            "测试",
            AgentRunContext::wechat_chat("user-obs", "commit", vec![]).with_user_session_state(
                Some(crate::task_store::UserSessionStateRecord {
                    user_id: "user-obs".to_string(),
                    goal: Some("目标A".to_string()),
                    current_subtask: Some("子任务B".to_string()),
                    next_step: Some("下一步C".to_string()),
                    constraints_json: Some(r#"["约束1"]"#.to_string()),
                    ..Default::default()
                }),
            ),
        );

        let json = serde_json::to_string_pretty(&trace).expect("序列化失败");
        let payload: Value = serde_json::from_str(&json).expect("JSON 应合法");

        assert_eq!(payload["persistent_state_present"], true);
        assert_eq!(payload["persistent_state_source"], "db");
        assert_eq!(payload["persistent_state_updated"], false);
        assert_eq!(
            payload["persistent_state_slot_count"].as_u64().unwrap_or(0),
            4,
            "应有 4 个填充槽位"
        );
        let preview = payload["persistent_state_preview"]
            .as_str()
            .expect("应有 preview");
        assert!(preview.contains("goal="), "preview 应包含 goal");
        assert!(preview.contains("subtask="), "preview 应包含 subtask");
    }

    #[test]
    fn merge_string_arrays_deduplicates_and_caps_length() {
        let persistent = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let runtime = vec!["b".to_string(), "d".to_string(), "e".to_string()];
        let merged = super::merge_string_arrays_with_runtime_reserve(persistent, runtime, 3, 0);
        assert_eq!(merged.len(), 3);
        assert!(merged.contains(&"a".to_string()));
        assert!(merged.contains(&"b".to_string()));
        assert!(merged.contains(&"c".to_string()));
        // d 和 e 因长度限制被截断（无 runtime 保底时 persistent 优先）
    }

    #[test]
    fn merge_string_arrays_is_case_insensitive_dedup() {
        let persistent = vec!["Hello".to_string()];
        let runtime = vec!["hello".to_string(), "HELLO".to_string()];
        let merged = super::merge_string_arrays_with_runtime_reserve(persistent, runtime, 10, 0);
        assert_eq!(merged.len(), 1);
        // runtime 先处理 dedup，保留 runtime 侧的 "hello"
        assert_eq!(merged[0], "hello");
    }

    #[test]
    fn merge_string_arrays_runtime_signal_is_reserved_when_capacity_full() {
        // persistent 填满预算，runtime 有唯一高价值信号，应保底保留
        let persistent = vec![
            "p1".to_string(),
            "p2".to_string(),
            "p3".to_string(),
            "p4".to_string(),
        ];
        let runtime = vec!["r1".to_string()];
        let merged = super::merge_string_arrays_with_runtime_reserve(persistent, runtime, 3, 1);
        // runtime 保底 1 条，再填 persistent
        assert_eq!(merged.len(), 3);
        assert!(
            merged.contains(&"r1".to_string()),
            "runtime 信号 r1 应被保底保留"
        );
        assert!(
            merged.contains(&"p1".to_string()),
            "persistent p1 应在剩余空间中保留"
        );
        assert!(
            merged.contains(&"p2".to_string()),
            "persistent p2 应在剩余空间中保留"
        );
        // p3, p4 因容量限制被截断
    }

    #[test]
    fn merge_string_arrays_dedup_still_works_with_reserve() {
        // runtime 和 persistent 有重复项，去重后 runtime 保底仍应生效
        let persistent = vec!["shared".to_string(), "p1".to_string(), "p2".to_string()];
        let runtime = vec!["shared".to_string(), "r1".to_string()];
        let merged = super::merge_string_arrays_with_runtime_reserve(persistent, runtime, 2, 1);
        assert_eq!(merged.len(), 2);
        // runtime 先 dedup：shared 和 r1 进入 runtime_unique
        // 保底 drain 1 条：shared（runtime 侧首次出现）
        // persistent 去重后填剩余：p1（shared 被 dedup）
        assert!(
            merged.contains(&"shared".to_string()),
            "runtime 侧的 shared 应被保底保留"
        );
        assert!(
            merged.contains(&"p1".to_string()),
            "persistent p1 应在剩余空间保留"
        );
    }

    #[test]
    fn derive_runtime_session_state_merges_persistent_and_runtime() {
        let workspace = temp_workspace();
        let mut trace = AgentRunTrace::new(
            &workspace,
            "测试",
            AgentRunContext::wechat_chat("user-merge", "commit", vec![]).with_user_session_state(
                Some(crate::task_store::UserSessionStateRecord {
                    user_id: "user-merge".to_string(),
                    goal: Some("持久化目标".to_string()),
                    current_subtask: Some("持久化子任务".to_string()),
                    next_step: Some("持久化下一步".to_string()),
                    constraints_json: Some(r#"["持久化约束"]"#.to_string()),
                    confirmed_facts_json: Some(r#"["持久化事实"]"#.to_string()),
                    done_items_json: Some(r#"["持久化完成"]"#.to_string()),
                    open_questions_json: Some(r#"["持久化问题"]"#.to_string()),
                    ..Default::default()
                }),
            ),
        );
        // 添加一个 done step 来测试合并
        trace.active_plan_steps.push(super::RuntimePlanStep {
            description: "运行时完成项".to_string(),
            status: super::PlanStepStatus::Done,
            expected_observation: None,
            retry_count: 0,
        });

        let snapshot = derive_runtime_session_state(&trace, "测试", None, None);

        assert_eq!(snapshot.goal, Some("持久化目标".to_string()));
        assert_eq!(snapshot.current_subtask, Some("持久化子任务".to_string()));
        assert_eq!(snapshot.next_step, Some("持久化下一步".to_string()));
        assert!(
            snapshot.constraints.contains(&"持久化约束".to_string()),
            "constraints 应包含持久化值"
        );
        assert!(
            snapshot.confirmed_facts.contains(&"持久化事实".to_string()),
            "confirmed_facts 应包含持久化值"
        );
        assert!(
            snapshot.done_items.contains(&"持久化完成".to_string()),
            "done_items 应包含持久化值"
        );
        assert!(
            snapshot.open_questions.contains(&"持久化问题".to_string()),
            "open_questions 应包含持久化值"
        );
        // 运行时 done step 也应合并进来
        assert!(
            snapshot.done_items.contains(&"运行时完成项".to_string()),
            "done_items 应包含运行时值"
        );
    }

    #[test]
    fn transient_failure_retry_then_replan() {
        let workspace = temp_workspace();
        let agent = AgentCore::with_scripted_decisions(
            workspace.clone(),
            3,
            vec![super::AgentDecision::Final("noop".to_string())],
        )
        .expect("初始化 agent 失败");
        let mut trace = AgentRunTrace::new(&workspace, "retry", AgentRunContext::agent_demo());
        trace.configure_controller_limits(3, 3);

        // 第一次 Transient -> RetryStep（原始 action）
        let first = agent
            .handle_recorded_failure(
                1,
                FailureDecision {
                    kind: StepFailureKind::Transient,
                    action: FailureAction::RetryStep,
                    replan_scope: None,
                    detail: "timeout".to_string(),
                    source: "test".to_string(),
                    user_message: None,
                },
                &mut trace,
            )
            .expect_err("第一次应为 RetryStep，返回失败");
        assert!(first.to_string().contains("timeout"));
        let first_attempt = trace.recovery_attempts.last().unwrap();
        assert_eq!(first_attempt.original_action, FailureAction::RetryStep);
        assert_eq!(first_attempt.effective_action, FailureAction::RetryStep);
        assert_eq!(first_attempt.action, FailureAction::RetryStep);
        assert!(!first_attempt.escalated);

        // 第二次 Transient -> 因防循环升级为 Replan
        let second = agent
            .handle_recorded_failure(
                2,
                FailureDecision {
                    kind: StepFailureKind::Transient,
                    action: FailureAction::RetryStep,
                    replan_scope: None,
                    detail: "timeout again".to_string(),
                    source: "test".to_string(),
                    user_message: None,
                },
                &mut trace,
            )
            .expect("升级后应成功 Replan");
        assert!(matches!(second, super::LoopControl::Continue(_)));
        let last = trace.recovery_attempts.last().unwrap();
        assert!(last.escalated, "第二次应标记为 escalated");
        assert_eq!(last.original_action, FailureAction::RetryStep);
        assert_eq!(last.effective_action, FailureAction::Replan);
        assert_eq!(last.action, FailureAction::Replan);
        assert_eq!(last.outcome, RecoveryOutcome::Continued);
    }

    #[test]
    fn low_value_observation_replan_then_ask_user() {
        let workspace = temp_workspace();
        let agent = AgentCore::with_scripted_decisions(
            workspace.clone(),
            3,
            vec![super::AgentDecision::Final("noop".to_string())],
        )
        .expect("初始化 agent 失败");
        let mut trace = AgentRunTrace::new(&workspace, "lvo", AgentRunContext::agent_demo());
        trace.configure_controller_limits(3, 3);

        // 第一次 LowValueObservation -> Replan
        let first = agent
            .handle_recorded_failure(
                1,
                FailureDecision {
                    kind: StepFailureKind::LowValueObservation,
                    action: FailureAction::Replan,
                    replan_scope: Some(ReplanScope::RemainingPlan),
                    detail: "no new info".to_string(),
                    source: "test".to_string(),
                    user_message: None,
                },
                &mut trace,
            )
            .expect("第一次 Replan 应成功");
        assert!(matches!(first, super::LoopControl::Continue(_)));
        assert!(!trace.recovery_attempts.last().unwrap().escalated);

        // 第二次 LowValueObservation -> 升级 AskUser
        let second = agent
            .handle_recorded_failure(
                2,
                FailureDecision {
                    kind: StepFailureKind::LowValueObservation,
                    action: FailureAction::Replan,
                    replan_scope: Some(ReplanScope::RemainingPlan),
                    detail: "still no new info".to_string(),
                    source: "test".to_string(),
                    user_message: None,
                },
                &mut trace,
            )
            .expect("升级后应 ask_user");
        assert!(matches!(second, super::LoopControl::Finish(_)));
        let last = trace.recovery_attempts.last().unwrap();
        assert!(last.escalated, "第二次应标记为 escalated");
        assert_eq!(last.outcome, RecoveryOutcome::EscalatedToAskUser);
    }

    #[test]
    fn recovery_loop_guard_prevents_infinite_escalation() {
        let workspace = temp_workspace();
        let agent = AgentCore::with_scripted_decisions(
            workspace.clone(),
            3,
            vec![super::AgentDecision::Final("noop".to_string())],
        )
        .expect("初始化 agent 失败");
        let mut trace = AgentRunTrace::new(&workspace, "loop", AgentRunContext::agent_demo());
        trace.configure_controller_limits(3, 3);

        // 连续触发同一 kind 多次，确保不会无限循环
        for i in 1..=4 {
            let _ = agent.handle_recorded_failure(
                i,
                FailureDecision {
                    kind: StepFailureKind::Transient,
                    action: FailureAction::RetryStep,
                    replan_scope: None,
                    detail: format!("attempt {}", i),
                    source: "test".to_string(),
                    user_message: None,
                },
                &mut trace,
            );
        }

        // Transient 的 max_attempts = 1，所以第 2 次就升级为 Replan
        // 第 3、4 次仍然是 Replan（因为 Replan 的 max_attempts = 1，但 Replan 成功执行，
        // 下一次再遇到 Transient 已经是新 kind 计数... 不，是同 kind 计数继续累加）
        // 实际上第 3 次时 kind_count=3 > max_attempts(1)，仍然升级，但 escalate_to 也是 Replan
        // 第 4 次同理
        let transient_attempts: Vec<_> = trace
            .recovery_attempts
            .iter()
            .filter(|a| a.failure_kind == StepFailureKind::Transient)
            .collect();
        assert_eq!(transient_attempts.len(), 4);
        // 第一次未升级
        assert!(!transient_attempts[0].escalated);
        // 第 2~4 次都被升级（因为 kind_count 一直 > 1）
        assert!(transient_attempts[1].escalated);
        assert!(transient_attempts[2].escalated);
        assert!(transient_attempts[3].escalated);
    }

    #[test]
    fn trace_records_recovery_attempt_action_outcome() {
        let workspace = temp_workspace();
        let agent = AgentCore::with_scripted_decisions(
            workspace.clone(),
            3,
            vec![super::AgentDecision::Final("noop".to_string())],
        )
        .expect("初始化 agent 失败");
        let mut trace = AgentRunTrace::new(&workspace, "trace", AgentRunContext::agent_demo());
        trace.configure_controller_limits(3, 3);

        // 记录一次 recovery
        let _ = agent.handle_recorded_failure(
            1,
            FailureDecision {
                kind: StepFailureKind::Semantic,
                action: FailureAction::Replan,
                replan_scope: Some(ReplanScope::CurrentStep),
                detail: "语义错误".to_string(),
                source: "test".to_string(),
                user_message: None,
            },
            &mut trace,
        );

        assert_eq!(trace.recovery_attempts.len(), 1);
        let attempt = &trace.recovery_attempts[0];
        assert_eq!(attempt.failure_kind, StepFailureKind::Semantic);
        assert_eq!(attempt.action, FailureAction::Replan);
        assert_eq!(attempt.outcome, RecoveryOutcome::Continued);
        assert_eq!(attempt.step, 1);
        assert_eq!(attempt.source, "test");
        assert_eq!(attempt.detail, "语义错误");
        assert!(!attempt.escalated);
        assert!(attempt.successful);
    }
}
