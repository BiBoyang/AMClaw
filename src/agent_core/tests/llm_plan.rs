use super::super::{
    map_llm_plan, parse_llm_plan, validate_expected_observation, AgentObservation, DoneRule,
    ExpectedObservation, LlmPlan, MinimumNovelty, ObservationKind,
};
#[test]
fn llm_plan_json_is_supported() {
    let decision =
        parse_llm_plan("{\"action\":\"read\",\"path\":\"demo/a.txt\"}").expect("LLM JSON 解析失败");
    assert!(matches!(
        decision.decision,
        super::super::AgentDecision::CallTool(super::super::ToolAction::Read { .. })
    ));
}

#[test]
fn llm_plan_markdown_json_is_supported() {
    let raw = "```json\n{\"action\":\"final\",\"answer\":\"ok\"}\n```";
    let decision = parse_llm_plan(raw).expect("Markdown JSON 解析失败");
    assert!(matches!(
        decision.decision,
        super::super::AgentDecision::Final(_)
    ));
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
        super::super::AgentDecision::CallTool(super::super::ToolAction::GetTaskStatus { .. })
    ));

    let recent = parse_llm_plan("{\"action\":\"list_recent_tasks\",\"limit\":3}")
        .expect("最近任务工具 JSON 解析失败");
    assert!(matches!(
        recent.decision,
        super::super::AgentDecision::CallTool(super::super::ToolAction::ListRecentTasks {
            limit: 3
        })
    ));

    let archive = parse_llm_plan("{\"action\":\"read_article_archive\",\"task_id\":\"task-2\"}")
        .expect("归档工具 JSON 解析失败");
    assert!(matches!(
        archive.decision,
        super::super::AgentDecision::CallTool(super::super::ToolAction::ReadArticleArchive { .. })
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
        super::super::AgentDecision::CallTool(super::super::ToolAction::GetTaskStatus { .. })
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
