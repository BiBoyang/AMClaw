use super::{
    default_recovery_for_failure, AgentDecision, AgentObservation, AgentRunTrace, DoneRule,
    ExpectedObservation, FailureAction, FailureDecision, MinimumNovelty, ObservationKind,
    PlannedDecision, ReplanScope, StepFailureKind, ToolAction,
};
use anyhow::{bail, Context, Result};
use serde_json::Value;

pub(crate) fn default_expected_observation_for_decision(
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

pub(crate) fn validate_expected_observation(
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

pub(crate) fn detect_low_value_observation_failure(
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

pub(crate) fn detect_repeated_action_failure(
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

pub(crate) fn detect_trajectory_drift_failure(
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

pub(crate) fn detect_stalled_trajectory_failure(trace: &AgentRunTrace) -> Option<FailureDecision> {
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

pub(crate) fn classify_tool_execution_failure(source: String, detail: &str) -> FailureDecision {
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

pub(crate) fn parse_expected_observation(
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

pub(crate) fn default_minimum_novelty_for_kind(kind: &ObservationKind) -> Option<MinimumNovelty> {
    match kind {
        ObservationKind::FileMutation => None,
        ObservationKind::Text
        | ObservationKind::JsonObject
        | ObservationKind::TaskStatus
        | ObservationKind::TaskList
        | ObservationKind::ArchiveContent => Some(MinimumNovelty::DifferentFromLast),
    }
}
