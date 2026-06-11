use super::{
    build_task_store_log_payload, FeedbackKind, MarkTaskArchivedInput, MemoryFeedbackState,
    MemoryType, MemoryWriteState, PromoteReason, SkipReason, StoredSessionRecord, TaskStore,
    WriteDecision,
};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::fs;
use uuid::Uuid;

mod memory_basic;
mod memory_governance;
mod messages;
mod task_lifecycle;
mod url_guard;
mod user_session_state;

fn temp_db_path() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("amclaw_task_store_test_{}", Uuid::new_v4()));
    fs::create_dir_all(&root).expect("创建测试目录失败");
    root.join("amclaw.db")
}

#[test]
fn context_token_can_be_persisted_and_loaded() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

    store
        .upsert_context_token("user-a", "ctx-1")
        .expect("写入 token 失败");
    assert_eq!(
        store.get_context_token("user-a").expect("读取 token 失败"),
        Some("ctx-1".to_string())
    );

    store
        .upsert_context_token("user-a", "ctx-2")
        .expect("更新 token 失败");
    assert_eq!(
        store.get_context_token("user-a").expect("读取 token 失败"),
        Some("ctx-2".to_string())
    );
}

#[test]
fn session_state_can_be_persisted_listed_and_deleted() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

    store
        .upsert_session_state(
            "user-a",
            "hello\nworld",
            &["msg-1".to_string(), "msg-2".to_string()],
        )
        .expect("写入 session_state 失败");

    let sessions = store
        .list_session_states()
        .expect("查询 session_state 失败");
    assert_eq!(
        sessions.len(),
        1,
        "应只有一条 session_state，实际: {:?}",
        sessions
    );
    assert_eq!(
        sessions[0],
        StoredSessionRecord {
            user_id: "user-a".to_string(),
            merged_text: "hello\nworld".to_string(),
            message_ids: vec!["msg-1".to_string(), "msg-2".to_string()],
            updated_at: sessions[0].updated_at.clone(),
        }
    );

    store
        .delete_session_state("user-a")
        .expect("删除 session_state 失败");
    assert!(store
        .list_session_states()
        .expect("查询 session_state 失败")
        .is_empty());
}

#[test]
fn cleanup_expired_user_session_states_cleans_both_tables() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

    // 插入一条正常 session_state
    store
        .upsert_session_state("user-old", "旧会话", &["msg-old-1".to_string()])
        .expect("写入失败");
    // 插入一条旧 user_session_state（直接写旧 updated_at）
    store
        .upsert_user_session_state(&crate::task_store::UserSessionStateRecord {
            user_id: "user-old".to_string(),
            last_user_intent: Some("旧意图".to_string()),
            current_task: None,
            next_step: None,
            blocked_reason: None,
            goal: None,
            current_subtask: None,
            constraints_json: None,
            confirmed_facts_json: None,
            done_items_json: None,
            open_questions_json: None,
            updated_at: "2000-01-01T00:00:00Z".to_string(),
        })
        .expect("写入 v2 state 失败");

    // ttl=0 时， cutoff = now，旧记录应被清理
    let cleaned = store
        .cleanup_expired_user_session_states(0)
        .expect("清理失败");
    assert!(cleaned > 0, "应清理至少一条过期记录");

    // 两条表都应为空
    assert!(store.list_session_states().expect("查询失败").is_empty());
    assert!(store
        .load_user_session_state("user-old")
        .expect("加载失败")
        .is_none());
}

#[test]
fn task_store_log_payload_keeps_contract_fields() {
    let payload = build_task_store_log_payload(
        "error",
        "task_status_changed",
        vec![
            ("task_id", json!("task-1")),
            ("status", json!("failed")),
            ("detail", Value::Null),
        ],
    );

    assert_eq!(payload["level"], "error");
    assert_eq!(payload["event"], "task_status_changed");
    assert_eq!(payload["task_id"], "task-1");
    assert_eq!(payload["status"], "failed");
    assert!(payload.get("ts").is_some());
    assert!(payload.get("detail").is_none());
}

#[test]
fn summary_is_overwritten_on_rerun() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
    let created = store
        .record_link_submission("https://example.com/summary-rerun")
        .expect("写入链接失败");

    store
        .claim_task(&created.task_id, "test-worker", 300)
        .expect("claim 失败");
    store
        .mark_task_archived(
            &created.task_id,
            MarkTaskArchivedInput {
                output_path: "/tmp/summary-rerun.md",
                title: Some("Summary Rerun"),
                page_kind: Some("article"),
                snapshot_path: None,
                content_source: Some("http"),
                summary: Some("初始摘要"),
            },
        )
        .expect("首次 archived 失败");

    // Simulate retry: reset then re-archive with better summary
    let conn = Connection::open(&db_path).expect("打开数据库失败");
    conn.execute(
        "UPDATE tasks SET status = 'pending', output_path = NULL, worker_id = NULL, processing_started_at = NULL, lease_until = NULL WHERE id = ?1",
        [created.task_id.as_str()],
    )
    .expect("重置任务状态失败");
    drop(conn);

    store
        .claim_task(&created.task_id, "test-worker", 300)
        .expect("claim 失败");
    store
        .mark_task_archived(
            &created.task_id,
            MarkTaskArchivedInput {
                output_path: "/tmp/summary-rerun-v2.md",
                title: Some("Summary Rerun"),
                page_kind: Some("article"),
                snapshot_path: None,
                content_source: Some("http"),
                summary: Some("更精确的LLM摘要"),
            },
        )
        .expect("二次 archived 失败");

    let archived = store.list_archived_tasks(10).expect("查询失败");
    assert_eq!(archived[0].summary, Some("更精确的LLM摘要".to_string()));
}

// ——— Phase 4: Memory Feedback 测试 ———

#[test]
fn feedback_retrieved_updates_retrieved_count() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");
    let mut ws = MemoryWriteState::default();
    let decision =
        store.govern_memory_write("user-a", "测试记忆", MemoryType::Explicit, 100, &mut ws);
    let memory_id = match decision {
        WriteDecision::Written(r) => r.id,
        _ => panic!("应写入"),
    };
    let mut fb = MemoryFeedbackState::default();
    fb.record(&memory_id, FeedbackKind::Retrieved);
    store.apply_memory_feedback(&fb).expect("feedback 写回失败");
    let memories = store.list_user_memories("user-a", 10).expect("查询失败");
    assert_eq!(memories[0].retrieved_count, 1);
}

#[test]
fn feedback_injected_updates_injected_count() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");
    let mut ws = MemoryWriteState::default();
    let decision =
        store.govern_memory_write("user-a", "测试记忆", MemoryType::Explicit, 100, &mut ws);
    let memory_id = match decision {
        WriteDecision::Written(r) => r.id,
        _ => panic!("应写入"),
    };
    let mut fb = MemoryFeedbackState::default();
    fb.record(&memory_id, FeedbackKind::Injected);
    store.apply_memory_feedback(&fb).expect("feedback 写回失败");
    let memories = store.list_user_memories("user-a", 10).expect("查询失败");
    assert_eq!(memories[0].injected_count, 1);
}

#[test]
fn feedback_useful_updates_use_count_and_useful_and_last_used_at() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");
    let mut ws = MemoryWriteState::default();
    let decision =
        store.govern_memory_write("user-a", "测试记忆", MemoryType::Explicit, 100, &mut ws);
    let memory_id = match decision {
        WriteDecision::Written(r) => r.id,
        _ => panic!("应写入"),
    };
    assert!(store.list_user_memories("user-a", 10).expect("查询失败")[0]
        .last_used_at
        .is_none());
    let mut fb = MemoryFeedbackState::default();
    fb.record(&memory_id, FeedbackKind::Useful);
    store.apply_memory_feedback(&fb).expect("feedback 写回失败");
    let mem = &store.list_user_memories("user-a", 10).expect("查询失败")[0];
    assert_eq!(mem.use_count, 1);
    assert!(mem.useful);
    assert!(mem.last_used_at.is_some());
}

#[test]
fn confirm_memory_useful_enforces_user_ownership() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");
    let mut ws = MemoryWriteState::default();
    let decision =
        store.govern_memory_write("user-a", "测试记忆", MemoryType::Explicit, 100, &mut ws);
    let memory_id = match decision {
        WriteDecision::Written(r) => r.id,
        _ => panic!("应写入"),
    };

    let err = store
        .confirm_memory_useful("user-b", &memory_id)
        .expect_err("应拒绝其他用户标记有用");
    assert!(err.to_string().contains("无权标记有用"));

    store
        .confirm_memory_useful("user-a", &memory_id)
        .expect("同用户应可标记有用");
    let mem = &store.list_user_memories("user-a", 10).expect("查询失败")[0];
    assert!(mem.useful);
    assert_eq!(mem.use_count, 1);
}

#[test]
fn explicit_still_sorts_before_auto() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");
    // auto with high use_count
    let mut ws = MemoryWriteState::default();
    let auto_id =
        match store.govern_memory_write("user-a", "主题: Rust", MemoryType::Auto, 60, &mut ws) {
            WriteDecision::Written(r) => r.id,
            _ => panic!("应写入"),
        };
    // 给 auto 大量 feedback
    let mut fb = MemoryFeedbackState::default();
    for _ in 0..10 {
        fb.record(&auto_id, FeedbackKind::Useful);
    }
    store.apply_memory_feedback(&fb).expect("feedback 失败");
    // 写入 explicit
    let mut ws2 = MemoryWriteState::default();
    let _ = store.govern_memory_write("user-a", "显式偏好", MemoryType::Explicit, 100, &mut ws2);
    let results = store.search_user_memories("user-a", 15).expect("检索失败");
    // explicit 仍然排第一
    assert_eq!(results[0].memory_type, MemoryType::Explicit);
    assert_eq!(results[0].priority, 100);
    assert_eq!(results[1].memory_type, MemoryType::Auto);
}

#[test]
fn useful_auto_sorts_before_non_useful_auto() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");
    let mut ws = MemoryWriteState::default();
    let useful_id =
        match store.govern_memory_write("user-a", "主题: Rust", MemoryType::Auto, 60, &mut ws) {
            WriteDecision::Written(r) => r.id,
            _ => panic!("应写入"),
        };
    let mut ws2 = MemoryWriteState::default();
    let _ = store.govern_memory_write("user-a", "主题: Python", MemoryType::Auto, 60, &mut ws2);
    // 给第一个 useful feedback
    let mut fb = MemoryFeedbackState::default();
    fb.record(&useful_id, FeedbackKind::Useful);
    store.apply_memory_feedback(&fb).expect("feedback 失败");
    let results = store.search_user_memories("user-a", 15).expect("检索失败");
    assert!(results[0].useful);
    assert!(!results[1].useful);
    assert_eq!(results[0].content, "主题: Rust");
}

#[test]
fn higher_use_count_sorts_first() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");
    let mut ws1 = MemoryWriteState::default();
    let id_high =
        match store.govern_memory_write("user-a", "主题: Rust", MemoryType::Auto, 60, &mut ws1) {
            WriteDecision::Written(r) => r.id,
            _ => panic!("应写入"),
        };
    let mut ws2 = MemoryWriteState::default();
    let _ = store.govern_memory_write("user-a", "主题: Go", MemoryType::Auto, 60, &mut ws2);
    // 给 Rust 5 次 useful
    let mut fb = MemoryFeedbackState::default();
    for _ in 0..5 {
        fb.record(&id_high, FeedbackKind::Useful);
    }
    store.apply_memory_feedback(&fb).expect("feedback 失败");
    let results = store.search_user_memories("user-a", 15).expect("检索失败");
    assert_eq!(results[0].content, "主题: Rust");
    assert!(results[0].use_count > results[1].use_count);
}

#[test]
fn last_used_at_affects_sorting() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");
    let mut ws1 = MemoryWriteState::default();
    let id_old = match store.govern_memory_write("user-a", "旧记忆", MemoryType::Auto, 60, &mut ws1)
    {
        WriteDecision::Written(r) => r.id,
        _ => panic!("应写入"),
    };
    let mut ws2 = MemoryWriteState::default();
    let id_new = match store.govern_memory_write("user-a", "新记忆", MemoryType::Auto, 60, &mut ws2)
    {
        WriteDecision::Written(r) => r.id,
        _ => panic!("应写入"),
    };
    // 只给"新记忆" useful feedback → 更新 last_used_at
    let mut fb = MemoryFeedbackState::default();
    fb.record(&id_new, FeedbackKind::Useful);
    store.apply_memory_feedback(&fb).expect("feedback 失败");
    let results = store.search_user_memories("user-a", 15).expect("检索失败");
    // 新记忆（useful=true, use_count=1）排在旧记忆（useful=false）前面
    assert_eq!(results[0].content, "新记忆");
    // 旧记忆没有 last_used_at
    assert!(store
        .list_user_memories("user-a", 10)
        .expect("查询失败")
        .iter()
        .find(|m| m.id == id_old)
        .unwrap()
        .last_used_at
        .is_none());
}

#[test]
fn sorting_is_deterministic() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");
    // 写 5 条相同 type/priority 的 auto 记忆
    for i in 0..5 {
        let mut ws = MemoryWriteState::default();
        let _ = store.govern_memory_write(
            "user-a",
            &format!("记忆 {}", i),
            MemoryType::Auto,
            60,
            &mut ws,
        );
    }
    // 多次检索，结果必须一致
    let r1 = store.search_user_memories("user-a", 15).expect("检索失败");
    let r2 = store.search_user_memories("user-a", 15).expect("检索失败");
    assert_eq!(r1.len(), r2.len());
    for (a, b) in r1.iter().zip(r2.iter()) {
        assert_eq!(a.id, b.id);
    }
}

#[test]
fn feedback_state_is_single_source() {
    // 验证 feedback 统计来自 MemoryFeedbackState，不各自重复计算
    let mut fb = MemoryFeedbackState::default();
    fb.record("m1", FeedbackKind::Retrieved);
    fb.record("m1", FeedbackKind::Injected);
    fb.record("m1", FeedbackKind::Useful);
    fb.record("m2", FeedbackKind::Retrieved);
    assert_eq!(fb.retrieved_count("m1"), 1);
    assert_eq!(fb.injected_count("m1"), 1);
    assert_eq!(fb.useful_count("m1"), 1);
    assert_eq!(fb.retrieved_count("m2"), 1);
    assert_eq!(fb.injected_count("m2"), 0);
    assert!(fb.has_feedback());
}

// ——— Phase 4: 新 Memory 类型测试 ———

#[test]
fn user_preference_can_be_written_and_retrieved() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");

    let created = store
        .add_user_memory_typed("user-a", "我喜欢短摘要", MemoryType::UserPreference, 80)
        .expect("写入 user_preference 失败");

    assert_eq!(created.memory_type, MemoryType::UserPreference);
    assert_eq!(created.priority, 80);

    let memories = store.list_user_memories("user-a", 10).expect("查询失败");
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].memory_type, MemoryType::UserPreference);
}

#[test]
fn project_fact_can_be_written_and_retrieved() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");

    let created = store
        .add_user_memory_typed(
            "user-a",
            "AMClaw 使用 Rust 开发",
            MemoryType::ProjectFact,
            85,
        )
        .expect("写入 project_fact 失败");

    assert_eq!(created.memory_type, MemoryType::ProjectFact);
    assert_eq!(created.priority, 85);

    let memories = store.list_user_memories("user-a", 10).expect("查询失败");
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].memory_type, MemoryType::ProjectFact);
}

#[test]
fn lesson_can_be_written_and_retrieved() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");

    let created = store
        .add_user_memory_typed(
            "user-a",
            "链接抓取失败时应提示用户手动补录",
            MemoryType::Lesson,
            75,
        )
        .expect("写入 lesson 失败");

    assert_eq!(created.memory_type, MemoryType::Lesson);
    assert_eq!(created.priority, 75);

    let memories = store.list_user_memories("user-a", 10).expect("查询失败");
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].memory_type, MemoryType::Lesson);
}

#[test]
fn new_memory_types_sort_by_priority() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");

    store
        .add_user_memory_typed("user-a", "lesson", MemoryType::Lesson, 75)
        .expect("写入失败");
    store
        .add_user_memory_typed("user-a", "auto", MemoryType::Auto, 60)
        .expect("写入失败");
    store
        .add_user_memory_typed("user-a", "explicit", MemoryType::Explicit, 100)
        .expect("写入失败");
    store
        .add_user_memory_typed("user-a", "project_fact", MemoryType::ProjectFact, 85)
        .expect("写入失败");
    store
        .add_user_memory_typed("user-a", "user_preference", MemoryType::UserPreference, 80)
        .expect("写入失败");

    let results = store.search_user_memories("user-a", 15).expect("检索失败");
    assert_eq!(results.len(), 5);
    // explicit(100) > project_fact(85) > user_preference(80) > lesson(75) > auto(60)
    assert_eq!(results[0].memory_type, MemoryType::Explicit);
    assert_eq!(results[1].memory_type, MemoryType::ProjectFact);
    assert_eq!(results[2].memory_type, MemoryType::UserPreference);
    assert_eq!(results[3].memory_type, MemoryType::Lesson);
    assert_eq!(results[4].memory_type, MemoryType::Auto);
}

#[test]
fn govern_user_preference_promotes_auto() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");

    let mut ws1 = MemoryWriteState::default();
    let _ = store.govern_memory_write("user-a", "偏好: 短摘要", MemoryType::Auto, 60, &mut ws1);

    let mut ws2 = MemoryWriteState::default();
    let decision = store.govern_memory_write(
        "user-a",
        "偏好: 短摘要",
        MemoryType::UserPreference,
        80,
        &mut ws2,
    );

    match decision {
        WriteDecision::Promoted {
            reason:
                PromoteReason::TypePromotesLower {
                    from: MemoryType::UserPreference,
                    to: MemoryType::Auto,
                },
            ..
        } => {}
        _ => panic!("user_preference 应提升 auto: {:?}", decision),
    }

    let memories = store.list_user_memories("user-a", 10).expect("查询失败");
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].memory_type, MemoryType::UserPreference);
    assert_eq!(memories[0].priority, 80);
}

#[test]
fn govern_project_fact_cannot_downgrade_explicit() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");

    let mut ws1 = MemoryWriteState::default();
    let _ = store.govern_memory_write(
        "user-a",
        "约束: 不用 unsafe",
        MemoryType::Explicit,
        100,
        &mut ws1,
    );

    let mut ws2 = MemoryWriteState::default();
    let decision = store.govern_memory_write(
        "user-a",
        "约束: 不用 unsafe",
        MemoryType::ProjectFact,
        85,
        &mut ws2,
    );

    match decision {
        WriteDecision::Skipped {
            reason: SkipReason::LowerPriorityWouldDowngradeHigher,
            ..
        } => {}
        _ => panic!("project_fact 不应覆盖 explicit: {:?}", decision),
    }
}

#[test]
fn govern_lesson_skips_duplicate_project_fact() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");

    let mut ws1 = MemoryWriteState::default();
    let _ = store.govern_memory_write(
        "user-a",
        "经验: 先 cargo check",
        MemoryType::ProjectFact,
        85,
        &mut ws1,
    );

    let mut ws2 = MemoryWriteState::default();
    let decision = store.govern_memory_write(
        "user-a",
        "经验: 先 cargo check",
        MemoryType::Lesson,
        75,
        &mut ws2,
    );

    // project_fact(85) > lesson(75)，所以 lesson 不能覆盖 project_fact
    match decision {
        WriteDecision::Skipped {
            reason: SkipReason::LowerPriorityWouldDowngradeHigher,
            ..
        } => {}
        _ => panic!("lesson 不应覆盖 project_fact: {:?}", decision),
    }
}

#[test]
fn govern_explicit_promotes_lesson() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");

    let mut ws1 = MemoryWriteState::default();
    let _ = store.govern_memory_write("user-a", "重要信息", MemoryType::Lesson, 75, &mut ws1);

    let mut ws2 = MemoryWriteState::default();
    let decision =
        store.govern_memory_write("user-a", "重要信息", MemoryType::Explicit, 100, &mut ws2);

    match decision {
        WriteDecision::Promoted {
            reason:
                PromoteReason::TypePromotesLower {
                    from: MemoryType::Explicit,
                    to: MemoryType::Lesson,
                },
            ..
        } => {}
        _ => panic!("explicit 应提升 lesson: {:?}", decision),
    }
}

#[test]
fn memory_type_label_prefixes_are_correct() {
    assert_eq!(MemoryType::Explicit.label_prefix(), "[记忆]");
    assert_eq!(MemoryType::Auto.label_prefix(), "[记忆]");
    assert_eq!(MemoryType::UserPreference.label_prefix(), "[偏好]");
    assert_eq!(MemoryType::ProjectFact.label_prefix(), "[项目]");
    assert_eq!(MemoryType::Lesson.label_prefix(), "[经验]");
}

#[test]
fn memory_type_user_isolation() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");

    store
        .add_user_memory_typed("user-a", "A 的偏好", MemoryType::UserPreference, 80)
        .expect("写入失败");
    store
        .add_user_memory_typed("user-a", "A 的项目事实", MemoryType::ProjectFact, 85)
        .expect("写入失败");
    store
        .add_user_memory_typed("user-b", "B 的经验", MemoryType::Lesson, 75)
        .expect("写入失败");

    let a_memories = store.list_user_memories("user-a", 10).expect("查询失败");
    assert_eq!(a_memories.len(), 2);
    assert!(a_memories.iter().all(|m| m.user_id == "user-a"));

    let b_memories = store.list_user_memories("user-b", 10).expect("查询失败");
    assert_eq!(b_memories.len(), 1);
    assert_eq!(b_memories[0].memory_type, MemoryType::Lesson);
    assert_eq!(b_memories[0].content, "B 的经验");
}
