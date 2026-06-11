use super::super::{
    FeedbackKind, MemoryFeedbackState, MemoryType, MemoryWriteState, TaskStore, WriteDecision,
};
use super::temp_db_path;

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
