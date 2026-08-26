use super::super::{
    classify_env_failure, load_business_context_snapshot, persist_ask_user_lesson,
    persist_failure_lesson, project_session_state_to_trace, AgentCore, AgentRunContext,
    AgentRunTrace, DropReason, FailureAction, FailureDecision, LoopControl, MemoryBudget,
    SessionState, StepFailureKind,
};
use super::{temp_db_path, temp_workspace};
use crate::retriever::rule::RuleRetriever;
use crate::task_store::{MemoryType, TaskStore, UserMemoryRecord};

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
        trace.memory_injected_count = session_state.injected_count();
        trace.memory_retrieved_count = session_state.retrieved_count();
        trace.memory_total_chars = session_state.injected_total_chars();
        trace.memory_injected_total_chars = session_state.injected_total_chars();
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

/// Phase 4：agent run 失败收场时应沉淀一条 Lesson 记忆。
#[test]
fn failure_lesson_is_persisted_as_lesson_memory() {
    let db_path = temp_db_path();
    persist_failure_lesson(
        Some(db_path.as_path()),
        Some("user-fail"),
        "工具执行失败: 文件读取失败",
    );

    let store = TaskStore::open(&db_path).expect("打开 task store 失败");
    let memories = store
        .list_user_memories("user-fail", 10)
        .expect("查询 user_memory 失败");
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].memory_type, MemoryType::Lesson);
    assert_eq!(memories[0].priority, 75);
    assert!(memories[0].content.starts_with("失败教训: "));
    assert!(memories[0].content.contains("文件读取失败"));
}

/// Phase 4：相同失败原因重复沉淀时应被 dedup，不产生第二条记忆。
#[test]
fn failure_lesson_dedups_repeated_failure() {
    let db_path = temp_db_path();
    persist_failure_lesson(
        Some(db_path.as_path()),
        Some("user-fail"),
        "达到最大步骤，未能收敛",
    );
    persist_failure_lesson(
        Some(db_path.as_path()),
        Some("user-fail"),
        "达到最大步骤，未能收敛",
    );

    let store = TaskStore::open(&db_path).expect("打开 task store 失败");
    let memories = store
        .list_user_memories("user-fail", 10)
        .expect("查询 user_memory 失败");
    assert_eq!(memories.len(), 1, "重复失败教训应被去重");
}

/// Phase 4：缺少 db_path / user_id 或错误内容为空时应静默跳过，不 panic、不落库。
#[test]
fn failure_lesson_skips_when_context_missing() {
    let db_path = temp_db_path();
    // 无 db_path
    persist_failure_lesson(None, Some("user-fail"), "some error");
    // 无 user_id
    persist_failure_lesson(Some(db_path.as_path()), None, "some error");
    // 空 user_id / 空错误内容
    persist_failure_lesson(Some(db_path.as_path()), Some("  "), "some error");
    persist_failure_lesson(Some(db_path.as_path()), Some("user-fail"), "   ");

    let store = TaskStore::open(&db_path).expect("打开 task store 失败");
    let memories = store
        .list_user_memories("user-fail", 10)
        .expect("查询 user_memory 失败");
    assert!(memories.is_empty(), "上下文缺失时不应写入任何记忆");
}

/// Phase 4：环境性失败分类应对齐 failure taxonomy 的 L1 口径。
#[test]
fn env_failure_classification_matches_taxonomy_l1() {
    // 鉴权类（小写匹配）
    assert_eq!(
        classify_env_failure("LLM 调用失败: HTTP 401 Authentication Fails"),
        Some("llm_auth_error")
    );
    assert_eq!(
        classify_env_failure("request failed: Unauthorized"),
        Some("llm_auth_error")
    );
    assert_eq!(
        classify_env_failure("error: invalid API key"),
        Some("llm_auth_error")
    );
    // 传输类
    assert_eq!(
        classify_env_failure("模型请求失败: connection timeout"),
        Some("llm_transport_error")
    );
    assert_eq!(
        classify_env_failure("error sending request for url"),
        Some("llm_transport_error")
    );
    assert_eq!(
        classify_env_failure("dns error: failed to lookup address"),
        Some("llm_transport_error")
    );
    // auth 优先于 transport（taxonomy 固定映射顺序）
    assert_eq!(
        classify_env_failure("HTTP 401 after connection retry"),
        Some("llm_auth_error")
    );
    // 普通任务错误不命中
    assert_eq!(classify_env_failure("工具执行失败: 文件读取失败"), None);
    assert_eq!(classify_env_failure("达到最大步骤，未能收敛"), None);
}

/// Phase 4：鉴权类环境失败不应沉淀 lesson（记 memory_lesson_env_filtered 日志）。
#[test]
fn auth_failure_lesson_is_filtered() {
    let db_path = temp_db_path();
    persist_failure_lesson(
        Some(db_path.as_path()),
        Some("user-env"),
        "LLM 调用失败: HTTP 401 Authentication Fails: invalid api key",
    );

    let store = TaskStore::open(&db_path).expect("打开 task store 失败");
    let memories = store
        .list_user_memories("user-env", 10)
        .expect("查询 user_memory 失败");
    assert!(memories.is_empty(), "鉴权类环境失败不应沉淀 lesson");
}

/// Phase 4：传输类环境失败不应沉淀 lesson（记 memory_lesson_env_filtered 日志）。
#[test]
fn transport_failure_lesson_is_filtered() {
    let db_path = temp_db_path();
    persist_failure_lesson(
        Some(db_path.as_path()),
        Some("user-env"),
        "模型请求失败: connection timeout",
    );

    let store = TaskStore::open(&db_path).expect("打开 task store 失败");
    let memories = store
        .list_user_memories("user-env", 10)
        .expect("查询 user_memory 失败");
    assert!(memories.is_empty(), "传输类环境失败不应沉淀 lesson");
}

/// Phase 4：AskUser 升级收场应沉淀一条"升级人工" Lesson 记忆。
#[test]
fn ask_user_escalation_persists_lesson() {
    let db_path = temp_db_path();
    let workspace = temp_workspace();
    let agent = AgentCore::with_max_steps_and_task_store_db_path(
        workspace.clone(),
        3,
        Some(db_path.clone()),
    )
    .expect("初始化 agent 失败");
    let mut trace = AgentRunTrace::new(
        &workspace,
        "ask user",
        AgentRunContext::wechat_chat("user-ask", "commit", vec![]),
    );

    let control = agent
        .handle_recorded_failure(
            1,
            FailureDecision {
                kind: StepFailureKind::ManualIntervention,
                action: FailureAction::AskUser,
                replan_scope: None,
                detail: "缺少任务编号，无法继续".to_string(),
                source: "test".to_string(),
                user_message: Some("请补充 task_id".to_string()),
            },
            &mut trace,
        )
        .expect("ask_user 应直接返回");
    assert!(matches!(control, LoopControl::Finish(_)));

    let store = TaskStore::open(&db_path).expect("打开 task store 失败");
    let memories = store
        .list_user_memories("user-ask", 10)
        .expect("查询 user_memory 失败");
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].memory_type, MemoryType::Lesson);
    assert!(memories[0].content.starts_with("升级人工: "));
    assert!(memories[0].content.contains("缺少任务编号"));
}

/// Phase 4：重复升级人工应被 dedup，不产生第二条记忆。
#[test]
fn ask_user_escalation_dedups_repeated() {
    let db_path = temp_db_path();
    persist_ask_user_lesson(
        Some(db_path.as_path()),
        Some("user-ask"),
        "缺少任务编号，无法继续",
    );
    persist_ask_user_lesson(
        Some(db_path.as_path()),
        Some("user-ask"),
        "缺少任务编号，无法继续",
    );

    let store = TaskStore::open(&db_path).expect("打开 task store 失败");
    let memories = store
        .list_user_memories("user-ask", 10)
        .expect("查询 user_memory 失败");
    assert_eq!(memories.len(), 1, "重复升级人工应被去重");
}

/// Phase 4：鉴权失败导致的 ask_user 升级不应沉淀 lesson（环境过滤同样生效）。
#[test]
fn ask_user_escalation_filters_env_auth() {
    let db_path = temp_db_path();
    persist_ask_user_lesson(
        Some(db_path.as_path()),
        Some("user-ask"),
        "LLM 调用失败: HTTP 401 Authentication Fails",
    );

    let store = TaskStore::open(&db_path).expect("打开 task store 失败");
    let memories = store
        .list_user_memories("user-ask", 10)
        .expect("查询 user_memory 失败");
    assert!(memories.is_empty(), "鉴权失败导致的升级不应沉淀 lesson");
}

/// project_session_state_to_trace 应把 dropped 明细（id/preview/reason）投影进 trace。
#[test]
fn project_session_state_to_trace_projects_dropped_details() {
    let workspace = temp_workspace();
    let mut trace = AgentRunTrace::new(&workspace, "帮我总结", AgentRunContext::agent_demo());

    let mut session_state = SessionState::default();
    session_state.retrieved.push(UserMemoryRecord {
        id: "mem-keep-1".to_string(),
        user_id: "user-x".to_string(),
        content: "保留的记忆".to_string(),
        memory_type: MemoryType::UserPreference,
        status: "active".to_string(),
        priority: 80,
        last_used_at: None,
        use_count: 0,
        retrieved_count: 0,
        injected_count: 0,
        useful: false,
        created_at: "2026-08-26T00:00:00Z".to_string(),
        updated_at: "2026-08-26T00:00:00Z".to_string(),
    });
    session_state
        .dropped
        .push(super::super::context_assembly::DroppedMemory {
            id: "mem-drop-1".to_string(),
            content_preview: "被预算裁掉的记忆预览".to_string(),
            reason: DropReason::BudgetExceeded,
        });

    project_session_state_to_trace(&mut trace, &session_state);

    assert_eq!(trace.memory_dropped_count, 1);
    assert_eq!(trace.memory_dropped.len(), 1);
    assert_eq!(trace.memory_dropped[0].id, "mem-drop-1");
    assert_eq!(trace.memory_dropped[0].drop_reason, "budget_exceeded");
    assert_eq!(
        trace.memory_dropped[0].content_preview,
        "被预算裁掉的记忆预览"
    );
}
