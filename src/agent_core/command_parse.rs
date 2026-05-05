use super::{
    default_expected_observation_for_decision, parse_expected_observation, AgentDecision,
    ExecutionPlan, PlannedDecision,
};
use crate::tool_registry::ToolAction;
use anyhow::{bail, Context, Result};
use serde::Deserialize;

impl AgentDecision {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::CallTool(_) => "call_tool",
            Self::Final(_) => "final",
        }
    }

    pub(crate) fn summary(&self) -> String {
        match self {
            Self::CallTool(action) => format!("tool={} target={}", action.name(), action.target()),
            Self::Final(answer) => super::trace::truncate_for_trace(answer, 240),
        }
    }
}

impl ToolAction {
    pub(crate) fn name(&self) -> &'static str {
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

    pub(crate) fn path(&self) -> Option<&str> {
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

    pub(crate) fn content(&self) -> Option<&str> {
        match self {
            Self::Read { .. } => None,
            Self::Write { content, .. } | Self::Create { content, .. } => Some(content.as_str()),
            Self::GetTaskStatus { .. }
            | Self::ListRecentTasks { .. }
            | Self::ListManualTasks { .. }
            | Self::ReadArticleArchive { .. } => None,
        }
    }

    pub(crate) fn target(&self) -> String {
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

#[derive(Debug, Deserialize)]
pub(crate) struct LlmPlan {
    pub(crate) action: String,
    pub(crate) path: Option<String>,
    pub(crate) content: Option<String>,
    pub(crate) answer: Option<String>,
    pub(crate) task_id: Option<String>,
    pub(crate) limit: Option<usize>,
    pub(crate) plan: Option<Vec<String>>,
    pub(crate) progress_note: Option<String>,
    pub(crate) expected_kind: Option<String>,
    pub(crate) done_rule: Option<String>,
    pub(crate) required_field: Option<String>,
    pub(crate) expected_fields: Option<Vec<String>>,
    pub(crate) minimum_novelty: Option<String>,
}

pub(crate) fn parse_user_command(input: &str) -> Result<AgentDecision> {
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

pub(crate) fn parse_llm_plan(raw: &str) -> Result<PlannedDecision> {
    let normalized = extract_json_object(raw).unwrap_or(raw);
    let plan: LlmPlan = serde_json::from_str(normalized)
        .with_context(|| format!("LLM 输出不是合法 JSON: {raw}"))?;
    map_llm_plan(plan)
}

pub(crate) fn map_llm_plan(plan: LlmPlan) -> Result<PlannedDecision> {
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
