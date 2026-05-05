use crate::config::AgentConfig;
use crate::context_pack::ContextSectionSnapshot;
use crate::session_summary::{summarize_for_markdown, SessionSummaryStrategy};
use crate::task_store::{
    FeedbackKind, MemoryFeedbackState, MemoryType, RecentTaskRecord, TaskStatusRecord, TaskStore,
    UserMemoryRecord,
};
use serde::Serialize;

#[derive(Debug, Clone, Copy)]
pub(crate) struct ContextCompactionConfig {
    pub(crate) session_summary_strategy: SessionSummaryStrategy,
    pub(crate) include_previous_observations: bool,
    pub(crate) memory_budget: MemoryBudget,
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
    pub(crate) fn from_agent_config(agent_config: &AgentConfig) -> Self {
        Self {
            session_summary_strategy: SessionSummaryStrategy::from_config_text(
                &agent_config.session_summary_strategy,
            ),
            include_previous_observations: agent_config.include_previous_observations,
            memory_budget: MemoryBudget::from_agent_config(agent_config),
        }
    }
}

/// Memory 预算配置
#[derive(Debug, Clone, Copy)]
pub(crate) struct MemoryBudget {
    pub(crate) max_items: usize,
    pub(crate) max_total_chars: usize,
    pub(crate) max_single_chars: usize,
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
    pub(crate) fn from_agent_config(agent_config: &AgentConfig) -> Self {
        Self {
            max_items: agent_config.memory_max_items,
            max_total_chars: agent_config.memory_max_total_chars,
            max_single_chars: agent_config.memory_max_single_chars,
        }
    }

    /// 轻量动态上调：有 current_task 或计划步较多时上调 20%
    pub(crate) fn with_dynamic_adjustment(
        self,
        has_current_task: bool,
        plan_step_count: usize,
    ) -> Self {
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

/// goal 信号来源，用于 low-signal 判定（不进 prompt，仅运行时标记）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum GoalSignal {
    /// 来自持久化 v2 goal（用户或之前 agent 显式写入）
    PersistentHigh,
    /// 来自持久化 last_user_intent 的 fallback
    PersistentFallback,
    /// 运行时默认模板（无历史状态时的兜底）
    #[default]
    RuntimeDefault,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub(crate) struct RuntimeSessionStateSnapshot {
    pub(crate) goal: Option<String>,
    pub(crate) current_subtask: Option<String>,
    pub(crate) constraints: Vec<String>,
    pub(crate) confirmed_facts: Vec<String>,
    pub(crate) done_items: Vec<String>,
    pub(crate) next_step: Option<String>,
    pub(crate) open_questions: Vec<String>,
    /// goal 信号来源标记，仅运行时用于 low-signal 判定
    #[serde(skip)]
    pub(crate) goal_signal: GoalSignal,
}

impl RuntimeSessionStateSnapshot {
    pub(crate) fn is_empty(&self) -> bool {
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
    pub(crate) fn is_low_signal(&self) -> bool {
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

    pub(crate) fn to_lines(&self) -> Vec<String> {
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
/// 合并持久化数组字段与运行时推导数组，去重并裁剪长度。
///
/// 关键改进：保证 runtime 信号至少保留 `runtime_min_keep` 条，
/// 避免 persistent 项占满预算后 runtime 高价值信号全丢。
pub(crate) fn merge_string_arrays_with_runtime_reserve(
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

/// 单次 agent run 的 memory 生命周期状态
///
/// 设计原则：
/// - 只管"本次请求生命周期"，不管长期存储
/// - retrieved → injected / dropped 的完整链路
/// - trace / log / markdown 都从这里投影，不各自维护
#[derive(Debug, Clone, Default)]
pub(crate) struct SessionState {
    /// 注入预算
    pub(crate) budget: MemoryBudget,
    /// 从 DB 检索出的候选记忆（已排序、未裁剪）
    pub(crate) retrieved: Vec<UserMemoryRecord>,
    /// 经裁剪后实际注入 prompt 的记忆
    pub(crate) injected: Vec<UserMemoryRecord>,
    /// 被裁剪掉的记忆及原因
    pub(crate) dropped: Vec<DroppedMemory>,
    /// 使用的检索器名称（用于 trace / A/B 对比）
    pub(crate) retriever_name: String,
    /// 检索耗时（毫秒）
    pub(crate) retrieval_latency_ms: u128,
    /// 检索器原始候选条数
    pub(crate) retrieval_candidate_count: usize,
    /// 预算裁剪后命中（注入）条数
    pub(crate) retrieval_hit_count: usize,
    /// 检索模式（rule / hybrid / semantic / shadow）
    pub(crate) retrieval_mode: String,
    /// 检索回退原因（如 embedding 失败、query_text 为空）
    pub(crate) retrieval_fallback_reason: Option<String>,
    /// 候选结果是否包含语义分数（hybrid/semantic 时为 true）
    pub(crate) retrieval_scores_present: bool,
}

/// 被裁剪掉的记忆及其原因
#[derive(Debug, Clone)]
pub(crate) struct DroppedMemory {
    pub(crate) id: String,
    pub(crate) content_preview: String,
    pub(crate) reason: DropReason,
}

/// 裁剪原因
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DropReason {
    /// 规范化后与更高优先级的记忆重复
    Deduplicated,
    /// 单条字符数超过 max_single_chars
    SingleItemTooLong,
    /// 总字符数或条数超过预算
    BudgetExceeded,
}

impl DropReason {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Deduplicated => "deduplicated",
            Self::SingleItemTooLong => "single_item_too_long",
            Self::BudgetExceeded => "budget_exceeded",
        }
    }
}

impl SessionState {
    /// 从检索结果构建 SessionState，执行去重 + 预算裁剪
    ///
    /// 裁剪逻辑（从 task_store 上移到此处）：
    /// 1. 规范化去重（trim + 多空格压缩）
    /// 2. 单条超长跳过
    /// 3. 总预算检查
    pub(crate) fn from_retrieved(retrieved: Vec<UserMemoryRecord>, budget: MemoryBudget) -> Self {
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
    pub(crate) fn retrieved_count(&self) -> usize {
        self.retrieved.len()
    }

    /// 实际注入 prompt 的记忆条数
    pub(crate) fn injected_count(&self) -> usize {
        self.injected.len()
    }

    /// 注入记忆的总字符数
    pub(crate) fn injected_total_chars(&self) -> usize {
        self.injected
            .iter()
            .map(|m| m.content.chars().count())
            .sum()
    }

    /// 注入记忆的 ID 列表
    pub(crate) fn injected_ids(&self) -> Vec<String> {
        self.injected.iter().map(|m| m.id.clone()).collect()
    }

    /// 是否有任何记忆活动（检索到或注入）
    pub(crate) fn has_memory_activity(&self) -> bool {
        !self.retrieved.is_empty()
    }

    /// 是否记录了 retriever 级可观测信息
    pub(crate) fn has_retrieval_observability(&self) -> bool {
        !self.retriever_name.is_empty()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct BusinessContextSnapshot {
    pub(crate) current_task: Option<TaskStatusRecord>,
    pub(crate) recent_tasks: Vec<RecentTaskRecord>,
    pub(crate) user_memories: Vec<UserMemoryRecord>,
}

/// 将 RetrievedItem 映射为 UserMemoryRecord，供现有 SessionState / prompt 链路零回归使用。
pub(crate) fn retrieved_item_to_user_memory_record(
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
        .unwrap_or(MemoryType::Auto);
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

pub(crate) fn derive_runtime_session_state(
    trace: &super::AgentRunTrace,
    user_input: &str,
    observation: Option<&super::AgentObservation>,
    business_context: Option<&BusinessContextSnapshot>,
) -> RuntimeSessionStateSnapshot {
    let persistent_session_state = trace.user_session_state.as_ref();
    let current_step = trace.active_plan_steps.iter().find(|step| {
        matches!(
            step.status,
            super::PlanStepStatus::Running
                | super::PlanStepStatus::Pending
                | super::PlanStepStatus::Failed
        )
    });
    let runtime_done_items = trace
        .active_plan_steps
        .iter()
        .filter(|step| step.status == super::PlanStepStatus::Done)
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
            super::trace::truncate_for_trace(&observation.content, 80)
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

use anyhow::Result;
use serde_json::json;

pub(crate) fn load_business_context_snapshot(
    task_store_db_path: Option<&std::path::Path>,
    trace: &super::AgentRunTrace,
    memory_budget: MemoryBudget,
    retriever: &dyn crate::retriever::Retriever,
    apply_feedback: bool,
) -> Result<(Option<BusinessContextSnapshot>, SessionState)> {
    let Some(db_path) = task_store_db_path else {
        return Ok((None, SessionState::default()));
    };

    let mut store = TaskStore::open(db_path)?;
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
            super::log_agent_info(
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
                super::log_agent_info(
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
                        // 兼容字段：memory_hit_count 与 memory_injected_count 同值
                        ("memory_hit_count", json!(session_state.injected_count())),
                        ("memory_dropped_count", json!(session_state.dropped.len())),
                        (
                            "memory_total_chars",
                            json!(session_state.injected_total_chars()),
                        ),
                        // 兼容字段：memory_injected_total_chars 与 memory_total_chars 同值
                        (
                            "memory_injected_total_chars",
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
                        super::log_agent_warn(
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
                super::log_agent_warn(
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

pub(crate) fn append_session_state_lines(
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

pub(crate) fn append_context_section_overview(
    lines: &mut Vec<String>,
    sections: &[ContextSectionSnapshot],
) {
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

/// 清理注入 prompt 的用户内容：移除控制字符，截断超长内容
pub(crate) fn sanitize_for_prompt(input: &str) -> String {
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

/// 根据 ObservationKind 类型感知地构建 LatestObservation section 的行。
/// ArchiveContent 大幅压缩（摘要 + 引用），TaskList 保留总数 + 前 N 项，
/// 其余类型保持全文。
pub(crate) fn build_latest_observation_lines(observation: &super::AgentObservation) -> Vec<String> {
    let section_max = crate::context_pack::ContextSectionKind::LatestObservation
        .policy()
        .max_chars;
    let step_line = format!("- step: {}", observation.step);
    let source_line = format!("- source: {}", observation.source);
    let frame_chars = "\n## Latest Observation\n".chars().count()
        + step_line.chars().count()
        + source_line.chars().count()
        + "```text\n\n```".chars().count();

    match observation.kind {
        Some(super::ObservationKind::ArchiveContent) => {
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
        Some(super::ObservationKind::TaskList) => {
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
pub(crate) fn extract_task_list_count_hint(content: &str) -> String {
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

pub(crate) fn build_context_summary(
    trace: &super::AgentRunTrace,
    observation: Option<&super::AgentObservation>,
) -> String {
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
            super::trace::truncate_for_trace(&observation.content, 80)
        ));
    }

    parts.join(", ")
}

pub(crate) fn select_previous_observations<'a>(
    trace: &'a super::AgentRunTrace,
    observation: Option<&super::AgentObservation>,
) -> Vec<&'a super::ObservationTrace> {
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

use crate::context_pack::{ContextPack, ContextSection, ContextSectionKind};
use crate::session_summary::build_session_text_section_lines;

#[derive(Debug, Default)]
pub(crate) struct ContextAssembler {
    pub(crate) include_previous_observations: bool,
}

impl ContextAssembler {
    #[cfg(test)]
    pub(super) fn assemble(
        &self,
        trace: &super::AgentRunTrace,
        user_input: &str,
        observation: Option<&super::AgentObservation>,
        runtime_session_state: Option<&RuntimeSessionStateSnapshot>,
        available_tools: &[String],
        business_context: Option<&BusinessContextSnapshot>,
    ) -> super::PlannerInput {
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
    pub(crate) fn assemble_with_summary_strategy(
        &self,
        trace: &super::AgentRunTrace,
        user_input: &str,
        observation: Option<&super::AgentObservation>,
        runtime_session_state: Option<&RuntimeSessionStateSnapshot>,
        available_tools: &[String],
        business_context: Option<&BusinessContextSnapshot>,
        session_summary_strategy: SessionSummaryStrategy,
    ) -> super::PlannerInput {
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

        super::PlannerInput {
            raw_user_input: user_input.to_string(),
            assembled_user_prompt,
            context_sections,
            context_budget_summary,
            context_summary,
        }
    }

    #[cfg(test)]
    pub(super) fn build_pack(
        &self,
        trace: &super::AgentRunTrace,
        user_input: &str,
        observation: Option<&super::AgentObservation>,
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
    pub(crate) fn build_pack_with_summary_strategy(
        &self,
        trace: &super::AgentRunTrace,
        user_input: &str,
        observation: Option<&super::AgentObservation>,
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
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_context_pack(
    trace: &super::AgentRunTrace,
    user_input: &str,
    observation: Option<&super::AgentObservation>,
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

pub(crate) fn project_session_state_to_trace(
    trace: &mut super::AgentRunTrace,
    session_state: &SessionState,
) {
    if session_state.has_memory_activity() {
        trace.memory_hit_count = session_state.injected_count();
        trace.memory_injected_count = session_state.injected_count();
        trace.memory_retrieved_count = session_state.retrieved_count();
        trace.memory_total_chars = session_state.injected_total_chars();
        trace.memory_injected_total_chars = session_state.injected_total_chars();
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

pub(crate) fn render_context_preview(
    trace: &super::AgentRunTrace,
    planner_input: &super::PlannerInput,
    runtime_session_state: &RuntimeSessionStateSnapshot,
    memory_session_state: &SessionState,
    mode: super::ContextPreviewMode,
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

    if matches!(mode, super::ContextPreviewMode::Verbose) {
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
