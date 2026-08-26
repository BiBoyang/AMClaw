use super::super::{
    load_business_context_snapshot, project_session_state_to_trace, select_retriever, AgentCore,
    AgentRunContext, AgentRunTrace, MemoryBudget, PlannedDecision, RetrieverMode,
};
use super::{temp_db_path, temp_workspace};
use crate::retriever::rule::RuleRetriever;
use crate::task_store::{MemoryType, TaskStore};

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
        PlannedDecision::new(super::super::AgentDecision::CallTool(
            super::super::ToolAction::Read {
                path: "demo.txt".to_string(),
            },
        )),
        PlannedDecision::new(super::super::AgentDecision::CallTool(
            super::super::ToolAction::Read {
                path: "demo.txt".to_string(),
            },
        )),
        PlannedDecision::new(super::super::AgentDecision::Final("done".to_string())),
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
