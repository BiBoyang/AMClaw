use super::super::{MemoryType, TaskStore, UserMemoryRecord};
use super::temp_db_path;
use rusqlite::Connection;

#[test]
fn user_memory_can_be_added_and_listed() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

    let created = store
        .add_user_memory("user-a", "我更喜欢短摘要")
        .expect("写入 user_memory 失败");
    let memories = store
        .list_user_memories("user-a", 10)
        .expect("查询 user_memory 失败");

    assert_eq!(
        memories,
        vec![UserMemoryRecord {
            id: created.id,
            user_id: "user-a".to_string(),
            content: "我更喜欢短摘要".to_string(),
            memory_type: MemoryType::Explicit,
            status: "active".to_string(),
            priority: 100,
            last_used_at: None,
            use_count: 0,
            retrieved_count: 0,
            injected_count: 0,
            useful: false,
            created_at: created.created_at,
            updated_at: created.updated_at,
        }]
    );
}

#[test]
fn user_memory_dedup_check_works() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
    store
        .add_user_memory("user-a", "偏好: 短摘要")
        .expect("写入 user_memory 失败");

    assert!(store
        .has_user_memory("user-a", "偏好: 短摘要")
        .expect("查询 user_memory 去重失败"));
    assert!(!store
        .has_user_memory("user-a", "主题: Rust")
        .expect("查询 user_memory 去重失败"));
}

#[test]
fn user_memory_schema_has_new_fields() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

    let created = store
        .add_user_memory("user-a", "显式记忆")
        .expect("写入 user_memory 失败");
    assert_eq!(created.memory_type, MemoryType::Explicit);
    assert_eq!(created.status, "active");
    assert_eq!(created.priority, 100);
    assert!(created.last_used_at.is_none());
    assert_eq!(created.use_count, 0);
}

#[test]
fn user_memory_typed_auto() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

    let created = store
        .add_user_memory_typed("user-a", "自动提炼主题", MemoryType::Auto, 60)
        .expect("写入 auto memory 失败");
    assert_eq!(created.memory_type, MemoryType::Auto);
    assert_eq!(created.priority, 60);
}

#[test]
fn user_memory_migration_adds_columns() {
    // 模拟老库：手动建只有旧字段的表，然后重新 open 触发 migration
    let db_path = temp_db_path();
    {
        let conn = Connection::open(&db_path).expect("打开数据库失败");
        conn.execute(
            "CREATE TABLE user_memories (id TEXT PRIMARY KEY, user_id TEXT NOT NULL, content TEXT NOT NULL, created_at DATETIME NOT NULL, updated_at DATETIME NOT NULL)",
            [],
        ).expect("建旧表失败");
        conn.execute(
            "INSERT INTO user_memories (id, user_id, content, created_at, updated_at) VALUES ('m1', 'user-x', '旧数据', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [],
        ).expect("插入旧数据失败");
    }
    // 重新 open 触发 migration
    let store = TaskStore::open(&db_path).expect("migration 后打开失败");
    let memories = store.list_user_memories("user-x", 10).expect("查询失败");
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].memory_type, MemoryType::Explicit); // DEFAULT 值
    assert_eq!(memories[0].status, "active");
    assert_eq!(memories[0].priority, 100);
    assert_eq!(memories[0].use_count, 0);
}

#[test]
fn search_memories_sorts_by_priority_and_dedupes() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

    // auto 记忆优先级低
    store
        .add_user_memory_typed("user-a", "自动偏好", MemoryType::Auto, 60)
        .expect("写入 auto 失败");
    // explicit 记忆优先级高
    store
        .add_user_memory("user-a", "显式偏好")
        .expect("写入 explicit 失败");
    // 重复内容（多空格版本，split_whitespace 后与"显式偏好"不同，但与"显式 偏好"相同）
    store
        .add_user_memory("user-a", "显式  偏好")
        .expect("写入重复失败");
    // 真正的重复内容（只有空格差异）
    store
        .add_user_memory("user-a", "显式 偏好")
        .expect("写入真重复失败");

    let results = store.search_user_memories("user-a", 15).expect("检索失败");
    // 去重由 SessionState 负责，task_store 只返回排序后的结果
    // 4 条：显式  偏好(explicit), 显式偏好(explicit), 自动偏好(auto), 显式 偏好(explicit)
    assert_eq!(results.len(), 4);
    assert_eq!(results[0].memory_type, MemoryType::Explicit);
}

#[test]
fn search_memories_respects_limit() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

    store
        .add_user_memory("user-a", "短记忆一")
        .expect("写入失败");
    store
        .add_user_memory("user-a", "短记忆二")
        .expect("写入失败");
    store
        .add_user_memory("user-a", "短记忆三")
        .expect("写入失败");

    // limit=2，只返回 2 条
    let results = store.search_user_memories("user-a", 2).expect("检索失败");
    assert_eq!(results.len(), 2);
}

#[test]
fn explicit_memory_sorts_before_auto() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

    // 先写 auto，再写 explicit
    store
        .add_user_memory_typed("user-a", "自动偏好", MemoryType::Auto, 60)
        .expect("写入 auto 失败");
    store
        .add_user_memory("user-a", "显式偏好")
        .expect("写入 explicit 失败");

    let results = store.search_user_memories("user-a", 15).expect("检索失败");
    assert_eq!(results.len(), 2);
    // explicit (priority=100) 应排在 auto (priority=60) 前面
    assert_eq!(results[0].memory_type, MemoryType::Explicit);
    assert_eq!(results[0].priority, 100);
    assert_eq!(results[1].memory_type, MemoryType::Auto);
    assert_eq!(results[1].priority, 60);
}

#[test]
fn search_memories_returns_all_sorted() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

    let long_content: String = "很".repeat(200);
    store
        .add_user_memory("user-a", &long_content)
        .expect("写入失败");
    store.add_user_memory("user-a", "短记忆").expect("写入失败");

    // task_store 只负责检索排序，不做预算裁剪
    let results = store.search_user_memories("user-a", 15).expect("检索失败");
    assert_eq!(results.len(), 2);
    // 两条都返回，trim 由 SessionState 负责
}

#[test]
fn suppress_memory_excludes_from_results() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

    let created = store
        .add_user_memory("user-a", "将被抑制")
        .expect("写入失败");
    store
        .suppress_memory("user-a", &created.id)
        .expect("抑制失败");

    // list 只返回 active
    let listed = store.list_user_memories("user-a", 10).expect("查询失败");
    assert!(listed.is_empty());

    // search 也排除 suppressed
    let searched = store.search_user_memories("user-a", 15).expect("检索失败");
    assert!(searched.is_empty());

    // has_user_memory 也排除 suppressed
    assert!(!store
        .has_user_memory("user-a", "将被抑制")
        .expect("查询失败"));
}

#[test]
fn user_memory_isolation() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

    store
        .add_user_memory("user-a", "A 的记忆")
        .expect("写入失败");
    store
        .add_user_memory("user-b", "B 的记忆")
        .expect("写入失败");

    let a_memories = store.list_user_memories("user-a", 10).expect("查询失败");
    assert_eq!(a_memories.len(), 1);
    assert_eq!(a_memories[0].content, "A 的记忆");

    let b_memories = store.list_user_memories("user-b", 10).expect("查询失败");
    assert_eq!(b_memories.len(), 1);
    assert_eq!(b_memories[0].content, "B 的记忆");
}

#[test]
fn suppress_memory_rejects_other_users_memory() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

    let created = store
        .add_user_memory("user-a", "A 的私有记忆")
        .expect("写入失败");

    let err = store
        .suppress_memory("user-b", &created.id)
        .expect_err("跨用户屏蔽应失败");
    assert!(err.to_string().contains("未找到该记忆"));

    let listed = store.list_user_memories("user-a", 10).expect("查询失败");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.id);
}

#[test]
fn suppress_memory_rejects_unknown_id() {
    let db_path = temp_db_path();
    let store = TaskStore::open(&db_path).expect("初始化 task store 失败");

    let err = store
        .suppress_memory("user-a", "missing-memory-id")
        .expect_err("不存在的 memory id 应失败");
    assert!(err.to_string().contains("未找到该记忆"));
}
