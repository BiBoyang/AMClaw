use super::super::{
    build_agent_log_payload, build_context_summary, detect_stalled_trajectory_failure, AgentCore,
    AgentObservation, AgentRunContext, AgentRunTrace, DoneRule, ExecutionPlan, ExpectedObservation,
    FailureAction, FailureDecision, MinimumNovelty, ObservationKind, PlannedDecision,
    RecoveryOutcome, ReplanScope, StepFailureKind,
};
use super::{temp_db_path, temp_workspace};
use serde_json::{json, Value};
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
            super::super::AgentDecision::CallTool(super::super::ToolAction::Create {
                path: "demo/loop.txt".to_string(),
                content: "hello multi step".to_string(),
            }),
            super::super::AgentDecision::CallTool(super::super::ToolAction::Read {
                path: "demo/loop.txt".to_string(),
            }),
            super::super::AgentDecision::Final("done".to_string()),
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
    let payload: Value =
        serde_json::from_str(&std::fs::read_to_string(trace_path).expect("读取 trace 文件失败"))
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
        PlannedDecision::new(super::super::AgentDecision::CallTool(
            super::super::ToolAction::Create {
                path: "demo/plan.txt".to_string(),
                content: "hello".to_string(),
            },
        ))
        .with_plan(Some(ExecutionPlan {
            steps: vec!["创建文件".to_string(), "读取文件".to_string()],
        }))
        .with_progress_note(Some("执行第一步".to_string())),
        PlannedDecision::new(super::super::AgentDecision::CallTool(
            super::super::ToolAction::Read {
                path: "demo/plan.txt".to_string(),
            },
        ))
        .with_progress_note(Some("执行第二步".to_string())),
        PlannedDecision::new(super::super::AgentDecision::Final("done".to_string()))
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
    let payload: Value =
        serde_json::from_str(&std::fs::read_to_string(trace_path).expect("读取 trace 文件失败"))
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
        .extend([PlannedDecision::new(super::super::AgentDecision::CallTool(
            super::super::ToolAction::Read {
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
    let payload: Value =
        serde_json::from_str(&std::fs::read_to_string(trace_path).expect("读取 trace 文件失败"))
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
        .extend([PlannedDecision::new(super::super::AgentDecision::CallTool(
            super::super::ToolAction::Read {
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
    let payload: Value =
        serde_json::from_str(&std::fs::read_to_string(trace_path).expect("读取 trace 文件失败"))
            .expect("trace JSON 应合法");

    assert_eq!(payload["active_plan_steps"][0]["status"], "failed");
}

#[test]
fn low_value_observation_triggers_replan() {
    let root = temp_workspace();
    let db_path = temp_db_path();
    let agent = AgentCore::with_max_steps_and_task_store_db_path(root.clone(), 4, Some(db_path))
        .expect("初始化 agent 失败");
    agent.scripted_decisions.borrow_mut().extend([
        PlannedDecision::new(super::super::AgentDecision::CallTool(
            super::super::ToolAction::ListRecentTasks { limit: 5 },
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
        PlannedDecision::new(super::super::AgentDecision::CallTool(
            super::super::ToolAction::ListManualTasks { limit: 5 },
        ))
        .with_expected_observation(Some(ExpectedObservation {
            kind: ObservationKind::TaskList,
            done_rule: DoneRule::ToolSuccess,
            expected_fields: vec!["count".to_string(), "tasks".to_string()],
            minimum_novelty: Some(MinimumNovelty::DifferentFromLast),
        })),
        PlannedDecision::new(super::super::AgentDecision::Final("replanned".to_string()))
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
    let payload: Value =
        serde_json::from_str(&std::fs::read_to_string(trace_path).expect("读取 trace 文件失败"))
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
        PlannedDecision::new(super::super::AgentDecision::CallTool(
            super::super::ToolAction::Create {
                path: "demo/drift.txt".to_string(),
                content: "hello".to_string(),
            },
        ))
        .with_plan(Some(ExecutionPlan {
            steps: vec!["创建文件".to_string(), "读取文件".to_string()],
        })),
        PlannedDecision::new(super::super::AgentDecision::Final("过早结束".to_string())),
        PlannedDecision::new(super::super::AgentDecision::CallTool(
            super::super::ToolAction::Read {
                path: "demo/drift.txt".to_string(),
            },
        )),
        PlannedDecision::new(super::super::AgentDecision::Final(
            "重新规划后完成".to_string(),
        )),
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
    let payload: Value =
        serde_json::from_str(&std::fs::read_to_string(trace_path).expect("读取 trace 文件失败"))
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
        &PlannedDecision::new(super::super::AgentDecision::Final("init".to_string())).with_plan(
            Some(ExecutionPlan {
                steps: vec!["A".to_string(), "B".to_string(), "C".to_string()],
            }),
        ),
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
        &PlannedDecision::new(super::super::AgentDecision::Final("noop".to_string())).with_plan(
            Some(ExecutionPlan {
                steps: vec!["B2".to_string()],
            }),
        ),
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
        &PlannedDecision::new(super::super::AgentDecision::Final("init".to_string())).with_plan(
            Some(ExecutionPlan {
                steps: vec!["A".to_string(), "B".to_string()],
            }),
        ),
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
        &PlannedDecision::new(super::super::AgentDecision::Final("init".to_string())).with_plan(
            Some(ExecutionPlan {
                steps: vec!["A".to_string(), "B".to_string(), "C".to_string()],
            }),
        ),
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
        &PlannedDecision::new(super::super::AgentDecision::Final("noop".to_string())).with_plan(
            Some(ExecutionPlan {
                steps: vec!["X".to_string(), "Y".to_string()],
            }),
        ),
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
        &PlannedDecision::new(super::super::AgentDecision::Final("init".to_string())).with_plan(
            Some(ExecutionPlan {
                steps: vec!["A".to_string(), "B".to_string()],
            }),
        ),
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
        &PlannedDecision::new(super::super::AgentDecision::Final("noop".to_string())).with_plan(
            Some(ExecutionPlan {
                steps: vec!["Z".to_string()],
            }),
        ),
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
        &PlannedDecision::new(super::super::AgentDecision::Final("init".to_string())).with_plan(
            Some(ExecutionPlan {
                steps: vec!["A".to_string(), "B".to_string()],
            }),
        ),
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
        super::super::LoopControl::Finish(answer) => assert_eq!(answer, "请补充 task_id"),
        super::super::LoopControl::Continue(_) => panic!("ask_user 不应继续执行"),
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
        vec![super::super::AgentDecision::Final("noop".to_string())],
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
    assert!(matches!(first, super::super::LoopControl::Continue(_)));
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
        super::super::LoopControl::Finish(answer) => {
            assert!(answer.contains("多次重规划仍未收敛"));
        }
        super::super::LoopControl::Continue(_) => panic!("预算耗尽后不应继续 replan"),
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
        &PlannedDecision::new(super::super::AgentDecision::CallTool(
            super::super::ToolAction::Read {
                path: "demo.txt".to_string(),
            },
        ))
        .with_plan(Some(ExecutionPlan {
            steps: vec!["读取 demo.txt".to_string()],
        })),
    );
    let observation =
        AgentObservation::tool_result(2, "read_file", "summary text", Some(ObservationKind::Text));
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
