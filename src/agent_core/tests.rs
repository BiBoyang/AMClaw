use super::{
    build_agent_log_payload, build_context_pack, build_context_summary,
    classify_tool_execution_failure, derive_runtime_session_state,
    detect_stalled_trajectory_failure, load_business_context_snapshot, map_llm_plan,
    parse_llm_plan, project_session_state_to_trace, select_retriever,
    validate_expected_observation, AgentCore, AgentObservation, AgentRunContext, AgentRunTrace,
    DoneRule, ExecutionPlan, ExpectedObservation, FailureAction, FailureDecision, GoalSignal,
    LlmPlan, MemoryBudget, MinimumNovelty, ObservationKind, PlannedDecision, RecoveryOutcome,
    ReplanScope, RetrieverMode, RuntimeSessionStateSnapshot, StepFailureKind,
};
use crate::retriever::rule::RuleRetriever;
use crate::session_summary::SessionSummaryStrategy;
use crate::task_store::{MemoryType, TaskStore};
use serde_json::{json, Value};
use uuid::Uuid;

mod context_pack;
mod memory_context;
mod trace_persistence;

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
        .extend([
            PlannedDecision::new(super::AgentDecision::CallTool(super::ToolAction::Read {
                path: "missing.txt".to_string(),
            }))
            .with_plan(Some(ExecutionPlan {
                steps: vec!["读取缺失文件".to_string()],
            })),
        ]);

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
        .extend([
            PlannedDecision::new(super::AgentDecision::CallTool(super::ToolAction::Read {
                path: "demo/empty.txt".to_string(),
            }))
            .with_plan(Some(ExecutionPlan {
                steps: vec!["读取非空文件".to_string()],
            }))
            .with_expected_observation(Some(ExpectedObservation {
                kind: ObservationKind::Text,
                done_rule: DoneRule::NonEmptyOutput,
                expected_fields: Vec::new(),
                minimum_novelty: Some(MinimumNovelty::DifferentFromLast),
            })),
        ]);

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

    let retriever = select_retriever(
        RetrieverMode::Semantic,
        Some(&db_path),
        "noop",
        true,
        &["user-semantic-fb".to_string()],
    );
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

    let retriever = select_retriever(
        RetrieverMode::Hybrid,
        Some(&db_path),
        "noop",
        true,
        &["user-hybrid-sel".to_string()],
    );
    let query = crate::retriever::RetrieveQuery::new("user-hybrid-sel", 10).with_query_text("测试");
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

    let retriever = select_retriever(
        RetrieverMode::Shadow,
        Some(&db_path),
        "noop",
        true,
        &["user-shadow-sel".to_string()],
    );
    let query = crate::retriever::RetrieveQuery::new("user-shadow-sel", 10).with_query_text("测试");
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

    let retriever = select_retriever(RetrieverMode::Rule, Some(&db_path), "noop", false, &[]);
    let query = crate::retriever::RetrieveQuery::new("user-rule-sel", 10);
    let result = retriever.retrieve(&query).expect("检索应成功");

    assert_eq!(result.retriever_name, "rule_v1");
    assert_eq!(result.candidates.len(), 1);
    assert_eq!(result.candidates[0].content, "规则检索测试");
}

// -----------------------------------------------------------------
// Step 3.4: rollout 回退链路测试（agent_core 侧）
// 验收口径 #3：rollout 不放量时稳定回退 rule
// -----------------------------------------------------------------

#[test]
fn rollout_disabled_semantic_fallback_to_rule() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");
    store
        .add_user_memory_typed(
            "user-rollout-disabled",
            "禁用 rollout",
            MemoryType::UserPreference,
            80,
        )
        .expect("写入失败");

    // rollout_enabled=false → 应回退到 rule
    let retriever = select_retriever(
        RetrieverMode::Semantic,
        Some(&db_path),
        "noop",
        false, // disabled
        &[],
    );
    let query =
        crate::retriever::RetrieveQuery::new("user-rollout-disabled", 10).with_query_text("测试");
    let result = retriever.retrieve(&query).expect("检索应成功");

    assert_eq!(result.retriever_name, "rule_v1");
    assert_eq!(
        result.candidates[0].metadata.get("retrieval_mode"),
        Some(&"rollout_fallback_rule".to_string())
    );
    assert_eq!(
        result.candidates[0].metadata.get("rollout_reason"),
        Some(&"rollout_disabled".to_string())
    );
}

#[test]
fn rollout_allowlist_miss_hybrid_fallback_to_rule() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");
    store
        .add_user_memory_typed(
            "user-rollout-miss",
            "不在 allowlist",
            MemoryType::UserPreference,
            80,
        )
        .expect("写入失败");

    // enabled=true 但 allowlist 不匹配 → 回退到 rule
    let retriever = select_retriever(
        RetrieverMode::Hybrid,
        Some(&db_path),
        "noop",
        true, // enabled
        &["other-user".to_string()],
    );
    let query =
        crate::retriever::RetrieveQuery::new("user-rollout-miss", 10).with_query_text("测试");
    let result = retriever.retrieve(&query).expect("检索应成功");

    assert_eq!(result.retriever_name, "rule_v1");
    assert_eq!(
        result.candidates[0].metadata.get("rollout_reason"),
        Some(&"user_not_in_allowlist".to_string())
    );
}

#[test]
fn rollout_allowlist_hit_shadow_uses_primary() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");
    store
        .add_user_memory_typed(
            "user-rollout-hit",
            "命中 allowlist",
            MemoryType::UserPreference,
            80,
        )
        .expect("写入失败");

    // enabled=true + allowlist 命中 → 走 shadow primary（对外仍是 rule 结果）
    let retriever = select_retriever(
        RetrieverMode::Shadow,
        Some(&db_path),
        "noop",
        true, // enabled
        &["user-rollout-hit".to_string()],
    );
    let query =
        crate::retriever::RetrieveQuery::new("user-rollout-hit", 10).with_query_text("测试");
    let result = retriever.retrieve(&query).expect("检索应成功");

    // Shadow primary 对外返回 rule 内容，但 metadata 带 rollout_allowed
    assert_eq!(result.candidates.len(), 1);
    assert_eq!(result.candidates[0].content, "命中 allowlist");
    assert_eq!(
        result.candidates[0].metadata.get("rollout_allowed"),
        Some(&"true".to_string())
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
        &PlannedDecision::new(super::AgentDecision::CallTool(super::ToolAction::Read {
            path: "demo.txt".to_string(),
        }))
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

#[test]
fn llm_plan_json_is_supported() {
    let decision =
        parse_llm_plan("{\"action\":\"read\",\"path\":\"demo/a.txt\"}").expect("LLM JSON 解析失败");
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
    let failure = classify_tool_execution_failure("tool:read".to_string(), "operation timed out");
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
    let payload: Value =
        serde_json::from_str(&std::fs::read_to_string(&trace_path).expect("读取 trace 文件失败"))
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
        confirmed_facts: vec!["已有较长 session_text 和 latest_observation".to_string(); 4],
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
        AgentRunContext::wechat_chat("user-v2", "commit", vec![]).with_user_session_state(Some(
            crate::task_store::UserSessionStateRecord {
                user_id: "user-v2".to_string(),
                goal: Some("整理待办任务".to_string()),
                current_subtask: Some("读取最近任务".to_string()),
                next_step: Some("确认是否需要重试".to_string()),
                constraints_json: Some(r#"["时间有限","优先高优先级"]"#.to_string()),
                confirmed_facts_json: Some(r#"["有3个pending任务"]"#.to_string()),
                done_items_json: Some(r#"["已登录"]"#.to_string()),
                open_questions_json: Some(r#"["是否需要通知用户"]"#.to_string()),
                ..Default::default()
            },
        )),
    );

    let runtime_session_state = derive_runtime_session_state(&trace, "帮我整理任务", None, None);

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
        AgentRunContext::wechat_chat("user-goal", "commit", vec![]).with_user_session_state(Some(
            crate::task_store::UserSessionStateRecord {
                user_id: "user-goal".to_string(),
                goal: Some("响应当前用户请求：你好".to_string()),
                ..Default::default()
            },
        )),
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
        AgentRunContext::wechat_chat("user-fb", "commit", vec![]).with_user_session_state(Some(
            crate::task_store::UserSessionStateRecord {
                user_id: "user-fb".to_string(),
                last_user_intent: Some("整理本周待办".to_string()),
                ..Default::default()
            },
        )),
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
        AgentRunContext::wechat_chat("user-obs", "commit", vec![]).with_user_session_state(Some(
            crate::task_store::UserSessionStateRecord {
                user_id: "user-obs".to_string(),
                goal: Some("目标A".to_string()),
                current_subtask: Some("子任务B".to_string()),
                next_step: Some("下一步C".to_string()),
                constraints_json: Some(r#"["约束1"]"#.to_string()),
                ..Default::default()
            },
        )),
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
        AgentRunContext::wechat_chat("user-merge", "commit", vec![]).with_user_session_state(Some(
            crate::task_store::UserSessionStateRecord {
                user_id: "user-merge".to_string(),
                goal: Some("持久化目标".to_string()),
                current_subtask: Some("持久化子任务".to_string()),
                next_step: Some("持久化下一步".to_string()),
                constraints_json: Some(r#"["持久化约束"]"#.to_string()),
                confirmed_facts_json: Some(r#"["持久化事实"]"#.to_string()),
                done_items_json: Some(r#"["持久化完成"]"#.to_string()),
                open_questions_json: Some(r#"["持久化问题"]"#.to_string()),
                ..Default::default()
            },
        )),
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

#[test]
fn run_with_context_returns_runtime_session_state() {
    let root = temp_workspace();
    let agent = AgentCore::with_scripted_decisions(
        root.clone(),
        5,
        vec![
            super::AgentDecision::CallTool(super::ToolAction::Create {
                path: "demo/state.txt".to_string(),
                content: "hello state".to_string(),
            }),
            super::AgentDecision::Final("done".to_string()),
        ],
    )
    .expect("初始化 agent 失败");

    let result = agent
        .run_with_context("创建文件", AgentRunContext::agent_demo())
        .expect("应成功");

    assert!(
        result.runtime_session_state.is_some(),
        "应返回 runtime_session_state"
    );
    let state = result.runtime_session_state.unwrap();
    assert!(state.goal.is_some());
    assert!(!state.is_empty());
}
