use super::super::{
    MemoryType, MemoryWriteState, PromoteReason, SkipReason, TaskStore, WriteDecision,
};
use super::temp_db_path;

#[test]
fn govern_dedup_checks_all_active_memories_not_top_50() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

    // 写入 50 条高优先级 explicit 记忆，把后续 auto 记忆挤出 top 50
    for i in 0..50 {
        store
            .add_user_memory("user-dedup-all", &format!("高优先级记忆 {i}"))
            .expect("写入失败");
    }
    // 写入 1 条低优先级 auto 记忆（priority=60 < 100）
    let mut ws1 = MemoryWriteState::default();
    let decision = store.govern_memory_write(
        "user-dedup-all",
        "将被提升的偏好",
        MemoryType::Auto,
        60,
        &mut ws1,
    );
    let auto_id = match decision {
        WriteDecision::Written(r) => r.id,
        _ => panic!("应写入 auto memory"),
    };

    // 再写入 50 条高优先级 explicit 记忆，确保 auto 记忆在检索时远在 50 名之后
    for i in 0..50 {
        store
            .add_user_memory("user-dedup-all", &format!("高优先级记忆 {i}"))
            .expect("写入失败");
    }

    // 现在用 explicit 写入同一内容；若 dedup 仅查 top 50 会误判为新写入，
    // 全量查应正确识别为重复并 promote。
    let mut ws2 = MemoryWriteState::default();
    let decision = store.govern_memory_write(
        "user-dedup-all",
        "将被提升的偏好",
        MemoryType::Explicit,
        100,
        &mut ws2,
    );
    match decision {
        WriteDecision::Promoted { id, .. } => assert_eq!(id, auto_id),
        other => panic!("应 Promote 原有 auto memory: {:?}", other),
    }

    // 验证只存在一条该内容的记忆
    let memories = store
        .list_user_memories("user-dedup-all", 200)
        .expect("查询失败");
    assert_eq!(
        memories
            .iter()
            .filter(|m| m.content == "将被提升的偏好")
            .count(),
        1
    );
}

// ——— Phase 3: Memory Write Governance 测试 ———

#[test]
fn govern_writes_new_explicit_memory() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");
    let mut ws = MemoryWriteState::default();
    let decision =
        store.govern_memory_write("user-a", "我喜欢短摘要", MemoryType::Explicit, 100, &mut ws);
    match decision {
        WriteDecision::Written(r) => {
            assert_eq!(r.memory_type, MemoryType::Explicit);
            assert_eq!(r.priority, 100);
        }
        _ => panic!("应写入: {:?}", decision),
    }
    assert_eq!(ws.written_count(), 1);
    assert_eq!(ws.skipped_count(), 0);
    assert_eq!(ws.candidate_count, 1);
}

#[test]
fn govern_writes_new_auto_memory() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");
    let mut ws = MemoryWriteState::default();
    let decision =
        store.govern_memory_write("user-a", "偏好: 短摘要", MemoryType::Auto, 60, &mut ws);
    match decision {
        WriteDecision::Written(r) => {
            assert_eq!(r.memory_type, MemoryType::Auto);
            assert_eq!(r.priority, 60);
        }
        _ => panic!("应写入: {:?}", decision),
    }
}

#[test]
fn govern_skips_empty_content() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");
    let mut ws = MemoryWriteState::default();
    let decision = store.govern_memory_write("user-a", "   ", MemoryType::Explicit, 100, &mut ws);
    match decision {
        WriteDecision::Skipped {
            reason: SkipReason::Empty,
            ..
        } => {}
        _ => panic!("应跳过空内容: {:?}", decision),
    }
    assert_eq!(ws.skipped_count(), 1);
}

#[test]
fn govern_skips_too_long_content() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");
    let long: String = "很".repeat(501);
    let mut ws = MemoryWriteState::default();
    let decision = store.govern_memory_write("user-a", &long, MemoryType::Explicit, 100, &mut ws);
    match decision {
        WriteDecision::Skipped {
            reason: SkipReason::TooLong,
            ..
        } => {}
        _ => panic!("应跳过超长: {:?}", decision),
    }
}

#[test]
fn govern_skips_duplicate_same_type() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");
    let mut ws1 = MemoryWriteState::default();
    let _ = store.govern_memory_write("user-a", "偏好: 短摘要", MemoryType::Auto, 60, &mut ws1);
    let mut ws2 = MemoryWriteState::default();
    let decision =
        store.govern_memory_write("user-a", "偏好: 短摘要", MemoryType::Auto, 60, &mut ws2);
    match decision {
        WriteDecision::Skipped {
            reason: SkipReason::Duplicate,
            ..
        } => {}
        _ => panic!("应跳过重复: {:?}", decision),
    }
}

#[test]
fn govern_auto_does_not_downgrade_explicit() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");
    // 先写 explicit
    let mut ws1 = MemoryWriteState::default();
    let _ = store.govern_memory_write(
        "user-a",
        "偏好: 短摘要",
        MemoryType::Explicit,
        100,
        &mut ws1,
    );
    // 再尝试 auto 同内容
    let mut ws2 = MemoryWriteState::default();
    let decision =
        store.govern_memory_write("user-a", "偏好: 短摘要", MemoryType::Auto, 60, &mut ws2);
    match decision {
        WriteDecision::Skipped {
            reason: SkipReason::LowerPriorityWouldDowngradeHigher,
            ..
        } => {}
        _ => panic!("auto 不应降级 explicit: {:?}", decision),
    }
    // 验证原有 explicit 未被改变
    let memories = store.list_user_memories("user-a", 10).expect("查询失败");
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].memory_type, MemoryType::Explicit);
}

#[test]
fn govern_explicit_promotes_auto() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");
    // 先写 auto
    let mut ws1 = MemoryWriteState::default();
    let _ = store.govern_memory_write("user-a", "偏好: 短摘要", MemoryType::Auto, 60, &mut ws1);
    // 再写 explicit 同内容
    let mut ws2 = MemoryWriteState::default();
    let decision = store.govern_memory_write(
        "user-a",
        "偏好: 短摘要",
        MemoryType::Explicit,
        100,
        &mut ws2,
    );
    match decision {
        WriteDecision::Promoted {
            reason:
                PromoteReason::TypePromotesLower {
                    from: MemoryType::Explicit,
                    to: MemoryType::Auto,
                },
            ..
        } => {}
        _ => panic!("explicit 应提升 auto: {:?}", decision),
    }
    // 验证已提升
    let memories = store.list_user_memories("user-a", 10).expect("查询失败");
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].memory_type, MemoryType::Explicit);
    assert_eq!(memories[0].priority, 100);
}

#[test]
fn govern_write_state_counters_accurate() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");
    let mut ws = MemoryWriteState::default();

    // 3 个候选
    let _ = store.govern_memory_write("user-a", "记忆一", MemoryType::Explicit, 100, &mut ws);
    let _ = store.govern_memory_write("user-a", "", MemoryType::Explicit, 100, &mut ws); // empty → skip
    let _ = store.govern_memory_write("user-a", "记忆一", MemoryType::Auto, 60, &mut ws); // dup → skip

    assert_eq!(ws.candidate_count, 3);
    assert_eq!(ws.written_count(), 1);
    assert_eq!(ws.skipped_count(), 2);
}

#[test]
fn govern_write_state_no_cross_user_leak() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");
    let mut ws_a = MemoryWriteState::default();
    let _ = store.govern_memory_write("user-a", "偏好: 红", MemoryType::Auto, 60, &mut ws_a);
    // user-b 的相同内容不应受 user-a 影响
    let mut ws_b = MemoryWriteState::default();
    let decision = store.govern_memory_write("user-b", "偏好: 红", MemoryType::Auto, 60, &mut ws_b);
    match decision {
        WriteDecision::Written(_) => {}
        _ => panic!("user-b 应能写入: {:?}", decision),
    }
    // 各自独立
    assert_eq!(ws_a.written_count(), 1);
    assert_eq!(ws_b.written_count(), 1);
}

#[test]
fn memory_write_threshold_skips_noise() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");
    let mut ws = MemoryWriteState::default();

    // 过短内容被跳过
    let d1 = store.govern_memory_write("user-a", "好的", MemoryType::UserPreference, 80, &mut ws);
    assert!(
        matches!(
            d1,
            WriteDecision::Skipped {
                reason: SkipReason::Noise,
                ..
            }
        ),
        "黑名单短句应被跳过: {:?}",
        d1
    );

    // 另一个黑名单
    let d2 = store.govern_memory_write("user-a", "OK", MemoryType::UserPreference, 80, &mut ws);
    assert!(
        matches!(
            d2,
            WriteDecision::Skipped {
                reason: SkipReason::Noise,
                ..
            }
        ),
        "ok 应被跳过: {:?}",
        d2
    );

    // 少于 6 字符被跳过
    let d3 = store.govern_memory_write("user-a", "短", MemoryType::UserPreference, 80, &mut ws);
    assert!(
        matches!(
            d3,
            WriteDecision::Skipped {
                reason: SkipReason::Noise,
                ..
            }
        ),
        "过短内容应被跳过: {:?}",
        d3
    );

    // 正常内容可通过
    let d4 = store.govern_memory_write(
        "user-a",
        "我喜欢在晚上看技术文章",
        MemoryType::UserPreference,
        80,
        &mut ws,
    );
    assert!(
        matches!(d4, WriteDecision::Written(_)),
        "正常内容应写入: {:?}",
        d4
    );

    // 查询确认只写入了正常内容
    let memories = store.list_user_memories("user-a", 10).expect("查询失败");
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].content, "我喜欢在晚上看技术文章");
}
