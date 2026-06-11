use super::super::{
    FeedbackKind, MemoryFeedbackState, MemoryType, MemoryWriteState, TaskStore, WriteDecision,
};
use super::temp_db_path;

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
