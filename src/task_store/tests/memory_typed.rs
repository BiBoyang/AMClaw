use super::super::{
    MemoryType, MemoryWriteState, PromoteReason, SkipReason, TaskStore, WriteDecision,
};
use super::temp_db_path;

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
