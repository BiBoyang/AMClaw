use super::super::{
    build_context_pack, derive_runtime_session_state, AgentCore, AgentRunContext, AgentRunTrace,
    GoalSignal,
};
use super::temp_workspace;
use crate::session_summary::SessionSummaryStrategy;
use serde_json::Value;
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
    let merged = super::super::merge_string_arrays_with_runtime_reserve(persistent, runtime, 3, 0);
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
    let merged = super::super::merge_string_arrays_with_runtime_reserve(persistent, runtime, 10, 0);
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
    let merged = super::super::merge_string_arrays_with_runtime_reserve(persistent, runtime, 3, 1);
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
    let merged = super::super::merge_string_arrays_with_runtime_reserve(persistent, runtime, 2, 1);
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
    trace.active_plan_steps.push(super::super::RuntimePlanStep {
        description: "运行时完成项".to_string(),
        status: super::super::PlanStepStatus::Done,
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
fn run_with_context_returns_runtime_session_state() {
    let root = temp_workspace();
    let agent = AgentCore::with_scripted_decisions(
        root.clone(),
        5,
        vec![
            super::super::AgentDecision::CallTool(super::super::ToolAction::Create {
                path: "demo/state.txt".to_string(),
                content: "hello state".to_string(),
            }),
            super::super::AgentDecision::Final("done".to_string()),
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
