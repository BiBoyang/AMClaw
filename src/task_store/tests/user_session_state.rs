use super::super::{TaskStore, UserSessionStateRecord};
use super::temp_db_path;

#[test]
fn user_session_state_empty_load_returns_none() {
    let db_path = temp_db_path();
    let store = TaskStore::open(&db_path).expect("初始化失败");
    let result = store.load_user_session_state("user-a").expect("加载失败");
    assert!(result.is_none());
}

#[test]
fn user_session_state_first_write_and_read_back() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");

    let record = UserSessionStateRecord {
        user_id: "user-a".to_string(),
        last_user_intent: Some("查询任务状态".to_string()),
        current_task: Some("task-123".to_string()),
        next_step: Some("等待用户确认".to_string()),
        blocked_reason: None,
        updated_at: "2026-04-17T10:00:00Z".to_string(),
        ..Default::default()
    };
    store.upsert_user_session_state(&record).expect("写入失败");

    let loaded = store
        .load_user_session_state("user-a")
        .expect("加载失败")
        .expect("应存在记录");
    assert_eq!(loaded.user_id, "user-a");
    assert_eq!(loaded.last_user_intent, Some("查询任务状态".to_string()));
    assert_eq!(loaded.current_task, Some("task-123".to_string()));
    assert_eq!(loaded.next_step, Some("等待用户确认".to_string()));
    assert_eq!(loaded.blocked_reason, None);
}

#[test]
fn user_session_state_overwrite_updates_fields() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");

    let first = UserSessionStateRecord {
        user_id: "user-a".to_string(),
        last_user_intent: Some("旧意图".to_string()),
        current_task: Some("task-1".to_string()),
        next_step: Some("步骤1".to_string()),
        blocked_reason: None,
        updated_at: "2026-04-17T10:00:00Z".to_string(),
        ..Default::default()
    };
    store.upsert_user_session_state(&first).expect("写入失败");

    let second = UserSessionStateRecord {
        user_id: "user-a".to_string(),
        last_user_intent: Some("新意图".to_string()),
        current_task: Some("task-2".to_string()),
        next_step: Some("步骤2".to_string()),
        blocked_reason: Some("等待人工输入".to_string()),
        updated_at: "2026-04-17T11:00:00Z".to_string(),
        ..Default::default()
    };
    store.upsert_user_session_state(&second).expect("更新失败");

    let loaded = store
        .load_user_session_state("user-a")
        .expect("加载失败")
        .expect("应存在记录");
    assert_eq!(loaded.last_user_intent, Some("新意图".to_string()));
    assert_eq!(loaded.current_task, Some("task-2".to_string()));
    assert_eq!(loaded.next_step, Some("步骤2".to_string()));
    assert_eq!(loaded.blocked_reason, Some("等待人工输入".to_string()));
    assert_eq!(loaded.updated_at, "2026-04-17T11:00:00Z".to_string());
}

#[test]
fn user_session_state_user_isolation() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");

    let record_a = UserSessionStateRecord {
        user_id: "user-a".to_string(),
        last_user_intent: Some("A的意图".to_string()),
        current_task: None,
        next_step: None,
        blocked_reason: None,
        updated_at: "2026-04-17T10:00:00Z".to_string(),
        ..Default::default()
    };
    let record_b = UserSessionStateRecord {
        user_id: "user-b".to_string(),
        last_user_intent: Some("B的意图".to_string()),
        current_task: None,
        next_step: None,
        blocked_reason: None,
        updated_at: "2026-04-17T10:00:00Z".to_string(),
        ..Default::default()
    };
    store
        .upsert_user_session_state(&record_a)
        .expect("写入A失败");
    store
        .upsert_user_session_state(&record_b)
        .expect("写入B失败");

    let loaded_a = store
        .load_user_session_state("user-a")
        .expect("加载失败")
        .expect("应存在");
    let loaded_b = store
        .load_user_session_state("user-b")
        .expect("加载失败")
        .expect("应存在");
    assert_eq!(loaded_a.last_user_intent, Some("A的意图".to_string()));
    assert_eq!(loaded_b.last_user_intent, Some("B的意图".to_string()));
}

#[test]
fn user_session_state_clear_removes_record() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");

    let record = UserSessionStateRecord {
        user_id: "user-a".to_string(),
        last_user_intent: Some("意图".to_string()),
        current_task: None,
        next_step: None,
        blocked_reason: None,
        updated_at: "2026-04-17T10:00:00Z".to_string(),
        ..Default::default()
    };
    store.upsert_user_session_state(&record).expect("写入失败");
    assert!(store.load_user_session_state("user-a").unwrap().is_some());

    store.clear_user_session_state("user-a").expect("清空失败");
    assert!(store.load_user_session_state("user-a").unwrap().is_none());
}

#[test]
fn user_session_state_upsert_empty_user_id_fails() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");

    let record = UserSessionStateRecord {
        user_id: "   ".to_string(),
        last_user_intent: None,
        current_task: None,
        next_step: None,
        blocked_reason: None,
        updated_at: "2026-04-17T10:00:00Z".to_string(),
        ..Default::default()
    };
    let err = store
        .upsert_user_session_state(&record)
        .expect_err("应失败");
    assert!(err.to_string().contains("user_id"));
}

#[test]
fn user_session_state_all_optional_fields_can_be_none() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");

    let record = UserSessionStateRecord {
        user_id: "user-a".to_string(),
        last_user_intent: None,
        current_task: None,
        next_step: None,
        blocked_reason: None,
        updated_at: "2026-04-17T10:00:00Z".to_string(),
        ..Default::default()
    };
    store.upsert_user_session_state(&record).expect("写入失败");

    let loaded = store
        .load_user_session_state("user-a")
        .expect("加载失败")
        .expect("应存在");
    assert!(loaded.last_user_intent.is_none());
    assert!(loaded.current_task.is_none());
    assert!(loaded.next_step.is_none());
    assert!(loaded.blocked_reason.is_none());
}

#[test]
fn user_session_state_survives_reopen() {
    let db_path = temp_db_path();
    {
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        let record = UserSessionStateRecord {
            user_id: "user-a".to_string(),
            last_user_intent: Some("持久化测试".to_string()),
            current_task: Some("task-xyz".to_string()),
            next_step: None,
            blocked_reason: Some("blocked".to_string()),
            updated_at: "2026-04-17T12:00:00Z".to_string(),
            ..Default::default()
        };
        store.upsert_user_session_state(&record).expect("写入失败");
    }

    let store = TaskStore::open(&db_path).expect("重新打开失败");
    let loaded = store
        .load_user_session_state("user-a")
        .expect("加载失败")
        .expect("应存在");
    assert_eq!(loaded.last_user_intent, Some("持久化测试".to_string()));
    assert_eq!(loaded.current_task, Some("task-xyz".to_string()));
    assert_eq!(loaded.blocked_reason, Some("blocked".to_string()));
}

#[test]
fn user_session_state_v2_fields_round_trip() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化失败");

    let mut record = UserSessionStateRecord {
        user_id: "user-a".to_string(),
        last_user_intent: Some("测试意图".to_string()),
        current_task: Some("task-v2".to_string()),
        next_step: Some("下一步".to_string()),
        blocked_reason: None,
        goal: Some("完成目标".to_string()),
        current_subtask: Some("当前子任务".to_string()),
        constraints_json: Some(r#"["约束1","约束2"]"#.to_string()),
        confirmed_facts_json: Some(r#"["事实A","事实B"]"#.to_string()),
        done_items_json: Some(r#"["完成1"]"#.to_string()),
        open_questions_json: Some(r#"["问题1","问题2"]"#.to_string()),
        updated_at: "2026-04-17T10:00:00Z".to_string(),
    };
    store.upsert_user_session_state(&record).expect("写入失败");

    let loaded = store
        .load_user_session_state("user-a")
        .expect("加载失败")
        .expect("应存在");
    assert_eq!(loaded.goal, Some("完成目标".to_string()));
    assert_eq!(loaded.current_subtask, Some("当前子任务".to_string()));
    assert_eq!(loaded.constraints(), vec!["约束1", "约束2"]);
    assert_eq!(loaded.confirmed_facts(), vec!["事实A", "事实B"]);
    assert_eq!(loaded.done_items(), vec!["完成1"]);
    assert_eq!(loaded.open_questions(), vec!["问题1", "问题2"]);
    assert_eq!(loaded.populated_slot_count(), 7);
    assert!(!loaded.is_v2_empty());

    // 测试 set_ 方法
    record.set_constraints(vec!["新约束".to_string()]);
    record.set_confirmed_facts(vec![]);
    record.set_done_items(vec!["完成A".to_string(), "完成B".to_string()]);
    record.set_open_questions(vec![]);
    store.upsert_user_session_state(&record).expect("更新失败");

    let loaded2 = store
        .load_user_session_state("user-a")
        .expect("加载失败")
        .expect("应存在");
    assert_eq!(loaded2.constraints(), vec!["新约束"]);
    assert!(loaded2.confirmed_facts_json.is_none());
    assert_eq!(loaded2.done_items(), vec!["完成A", "完成B"]);
    assert!(loaded2.open_questions_json.is_none());
}

#[test]
fn user_session_state_v2_migration_on_existing_db() {
    // 模拟旧 DB（无 v2 字段），重新打开应自动迁移
    let db_path = temp_db_path();
    {
        let conn = rusqlite::Connection::open(&db_path).expect("打开失败");
        conn.execute(
            "CREATE TABLE user_session_states (
                user_id TEXT PRIMARY KEY,
                last_user_intent TEXT,
                current_task TEXT,
                next_step TEXT,
                blocked_reason TEXT,
                updated_at DATETIME NOT NULL
            )",
            [],
        )
        .expect("建旧表失败");
        conn.execute(
            "INSERT INTO user_session_states (user_id, last_user_intent, updated_at)
             VALUES ('user-a', '旧意图', '2026-04-01T00:00:00Z')",
            [],
        )
        .expect("插入旧数据失败");
    }

    let store = TaskStore::open(&db_path).expect("重新打开失败");
    let loaded = store
        .load_user_session_state("user-a")
        .expect("加载失败")
        .expect("应存在");
    assert_eq!(loaded.last_user_intent, Some("旧意图".to_string()));
    assert_eq!(loaded.goal, None);
    assert_eq!(loaded.constraints_json, None);
    assert!(loaded.is_v2_empty());

    // 写入 v2 数据应成功
    let mut store = TaskStore::open(&db_path).expect("重新打开失败");
    let mut record = loaded.clone();
    record.goal = Some("新目标".to_string());
    record.set_constraints(vec!["约束".to_string()]);
    store
        .upsert_user_session_state(&record)
        .expect("v2 写入失败");

    let loaded2 = store
        .load_user_session_state("user-a")
        .expect("加载失败")
        .expect("应存在");
    assert_eq!(loaded2.goal, Some("新目标".to_string()));
    assert_eq!(loaded2.constraints(), vec!["约束"]);
}
