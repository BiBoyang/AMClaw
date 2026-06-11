use super::super::{
    ArchivedTaskRecord, LinkTaskRecord, MarkTaskArchivedInput, PendingTaskRecord, RecentTaskRecord,
    TaskStatusRecord, TaskStore,
};
use super::temp_db_path;
use rusqlite::{params, Connection};

#[test]
fn link_submission_creates_article_and_task() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

    let record = store
        .record_link_submission("https://example.com/path?q=1")
        .expect("写入链接失败");
    drop(store);

    let conn = Connection::open(&db_path).expect("打开数据库失败");
    let article_row: (String, String) = conn
        .query_row(
            "SELECT id, normalized_url FROM articles WHERE id = ?1",
            [record.article_id.clone()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("查询文章失败");
    let task_row: (String, String) = conn
        .query_row(
            "SELECT id, article_id FROM tasks WHERE id = ?1",
            [record.task_id.clone()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("查询任务失败");

    assert_eq!(article_row.0, record.article_id);
    assert_eq!(article_row.1, "https://example.com/path?q=1");
    assert_eq!(task_row.0, record.task_id);
    assert_eq!(task_row.1, record.article_id);
    assert!(record.created_new);
}

#[test]
fn duplicate_link_returns_existing_article_and_task() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

    let first = store
        .record_link_submission("https://example.com")
        .expect("首次写入链接失败");
    let second = store
        .record_link_submission("https://example.com/")
        .expect("重复写入链接失败");
    drop(store);

    let conn = Connection::open(&db_path).expect("打开数据库失败");
    let article_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM articles", [], |row| row.get(0))
        .expect("查询文章数量失败");
    let task_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
        .expect("查询任务数量失败");

    assert_eq!(
        second,
        LinkTaskRecord {
            article_id: first.article_id.clone(),
            task_id: first.task_id.clone(),
            normalized_url: "https://example.com".to_string(),
            original_url: "https://example.com/".to_string(),
            created_new: false,
        }
    );
    assert_eq!(article_count, 1);
    assert_eq!(task_count, 1);
}

#[test]
fn task_status_can_be_queried() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

    let created = store
        .record_link_submission("https://example.com/status")
        .expect("写入链接失败");
    let status = store
        .get_task_status(&created.task_id)
        .expect("查询任务状态失败")
        .expect("应存在任务状态");

    assert_eq!(
        status,
        TaskStatusRecord {
            task_id: created.task_id.clone(),
            article_id: created.article_id.clone(),
            normalized_url: "https://example.com/status".to_string(),
            title: None,
            content_source: None,
            page_kind: None,
            status: "pending".to_string(),
            retry_count: 0,
            last_error: None,
            output_path: None,
            snapshot_path: None,
            created_at: status.created_at.clone(),
            updated_at: status.updated_at.clone(),
        }
    );
}

#[test]
fn querying_missing_task_returns_none() {
    let db_path = temp_db_path();
    let store = TaskStore::open(&db_path).expect("初始化 task store 失败");

    let status = store
        .get_task_status("missing-task")
        .expect("查询不存在任务失败");

    assert_eq!(status, None);
}

#[test]
fn recent_tasks_returns_latest_first() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

    let first = store
        .record_link_submission("https://example.com/one")
        .expect("写入第一条链接失败");
    let second = store
        .record_link_submission("https://example.com/two")
        .expect("写入第二条链接失败");

    let tasks = store.list_recent_tasks(10).expect("查询最近任务失败");

    assert_eq!(
        tasks,
        vec![
            RecentTaskRecord {
                task_id: second.task_id,
                status: "pending".to_string(),
                content_source: None,
                page_kind: None,
                normalized_url: "https://example.com/two".to_string(),
                updated_at: tasks[0].updated_at.clone(),
            },
            RecentTaskRecord {
                task_id: first.task_id,
                status: "pending".to_string(),
                content_source: None,
                page_kind: None,
                normalized_url: "https://example.com/one".to_string(),
                updated_at: tasks[1].updated_at.clone(),
            },
        ]
    );
}

#[test]
fn retry_task_resets_status_and_clears_error() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
    let created = store
        .record_link_submission("https://example.com/retry")
        .expect("写入链接失败");

    let conn = Connection::open(&db_path).expect("打开数据库失败");
    conn.execute(
        "UPDATE tasks SET status = 'failed', retry_count = 2, last_error = 'boom' WHERE id = ?1",
        [created.task_id.as_str()],
    )
    .expect("准备失败任务状态失败");
    drop(conn);

    let retried = store
        .retry_task(&created.task_id)
        .expect("重试任务失败")
        .expect("应存在任务");

    assert_eq!(retried.status, "pending");
    assert_eq!(retried.normalized_url, "https://example.com/retry");
    assert_eq!(retried.content_source, None);
    assert_eq!(retried.page_kind, None);
    assert_eq!(retried.retry_count, 3);
    assert_eq!(retried.last_error, None);
    assert_eq!(retried.output_path, None);
    assert_eq!(retried.snapshot_path, None);
}

#[test]
fn retry_missing_task_returns_none() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

    let retried = store
        .retry_task("missing-task")
        .expect("重试不存在任务失败");

    assert_eq!(retried, None);
}

#[test]
fn retry_processing_task_returns_validation_error() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
    let created = store
        .record_link_submission("https://example.com/retry-processing")
        .expect("写入链接失败");

    // 先领取，进入 processing 状态
    assert!(
        store
            .claim_task(&created.task_id, "worker-a", 300)
            .expect("claim 失败"),
        "pending 任务应可被领取"
    );

    let err = store
        .retry_task(&created.task_id)
        .expect_err("processing 状态下重试应失败");
    let message = err.to_string();
    assert!(
        message.contains("不允许重试"),
        "错误信息应提示不允许重试，实际: {message}"
    );
    assert!(
        message.contains("processing"),
        "错误信息应包含当前状态 processing，实际: {message}"
    );
}

#[test]
fn expired_lease_task_can_be_reclaimed() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
    let created = store
        .record_link_submission("https://example.com/reclaim")
        .expect("写入链接失败");

    // 首次领取成功，第二次应失败（lease 尚未过期）
    assert!(store
        .claim_task(&created.task_id, "worker-a", 300)
        .expect("首次 claim 失败"));
    assert!(
        !store
            .claim_task(&created.task_id, "worker-b", 300)
            .expect("二次 claim 查询失败"),
        "lease 未过期时不应被再次领取"
    );

    // 人工制造过期 lease，再次领取应成功
    let conn = Connection::open(&db_path).expect("打开数据库失败");
    conn.execute(
        "UPDATE tasks SET lease_until = ?2 WHERE id = ?1",
        params![created.task_id.as_str(), "2000-01-01T00:00:00+00:00"],
    )
    .expect("回写过期 lease 失败");
    drop(conn);

    assert!(
        store
            .claim_task(&created.task_id, "worker-b", 300)
            .expect("过期后 claim 失败"),
        "lease 过期后应可被重新领取"
    );

    let conn = Connection::open(&db_path).expect("打开数据库失败");
    let worker_id: Option<String> = conn
        .query_row(
            "SELECT worker_id FROM tasks WHERE id = ?1",
            [created.task_id.as_str()],
            |row| row.get(0),
        )
        .expect("读取 worker_id 失败");
    assert_eq!(worker_id, Some("worker-b".to_string()));
}

#[test]
fn pending_tasks_can_be_listed_and_archived() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
    let created = store
        .record_link_submission("https://example.com/pending")
        .expect("写入链接失败");

    let pending = store.list_pending_tasks(10).expect("查询 pending 失败");
    assert_eq!(
        pending,
        vec![PendingTaskRecord {
            task_id: created.task_id.clone(),
            article_id: created.article_id.clone(),
            normalized_url: "https://example.com/pending".to_string(),
            original_url: "https://example.com/pending".to_string(),
        }]
    );

    store
        .claim_task(&created.task_id, "test-worker", 300)
        .expect("claim 失败");

    assert!(store
        .mark_task_archived(
            &created.task_id,
            MarkTaskArchivedInput {
                output_path: "/tmp/example.md",
                title: Some("Example Title"),
                page_kind: Some("article"),
                snapshot_path: Some("/tmp/example.png"),
                content_source: Some("browser_capture"),
                summary: None,
            },
        )
        .expect("更新 archived 状态失败"));

    let pending_after = store.list_pending_tasks(10).expect("查询 pending 失败");
    let status = store
        .get_task_status(&created.task_id)
        .expect("查询状态失败")
        .expect("应存在任务");

    assert!(pending_after.is_empty());
    assert_eq!(status.status, "archived");
    assert_eq!(status.content_source, Some("browser_capture".to_string()));
    assert_eq!(status.page_kind, Some("article".to_string()));
    assert_eq!(status.output_path, Some("/tmp/example.md".to_string()));
    assert_eq!(status.snapshot_path, Some("/tmp/example.png".to_string()));
    assert_eq!(status.title, Some("Example Title".to_string()));
}

#[test]
fn task_can_be_marked_failed() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
    let created = store
        .record_link_submission("https://example.com/fail")
        .expect("写入链接失败");

    store
        .claim_task(&created.task_id, "test-worker", 300)
        .expect("claim 失败");

    assert!(store
        .mark_task_failed(&created.task_id, "network fail")
        .expect("更新 failed 状态失败"));

    let status = store
        .get_task_status(&created.task_id)
        .expect("查询状态失败")
        .expect("应存在任务");

    assert_eq!(status.status, "failed");
    assert_eq!(status.content_source, None);
    assert_eq!(status.last_error, Some("network fail".to_string()));
}

#[test]
fn task_can_be_marked_awaiting_manual_input() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
    let created = store
        .record_link_submission("https://mp.weixin.qq.com/s/manual")
        .expect("写入链接失败");

    store
        .claim_task(&created.task_id, "test-worker", 300)
        .expect("claim 失败");

    assert!(store
        .mark_task_awaiting_manual_input(
            &created.task_id,
            "微信公众号页面需要验证码验证",
            "wechat_captcha",
            None,
            Some("browser_capture"),
        )
        .expect("更新 awaiting_manual_input 状态失败"));

    let status = store
        .get_task_status(&created.task_id)
        .expect("查询状态失败")
        .expect("应存在任务");

    assert_eq!(status.status, "awaiting_manual_input");
    assert_eq!(status.content_source, Some("browser_capture".to_string()));
    assert_eq!(status.page_kind, Some("wechat_captcha".to_string()));
    assert_eq!(
        status.last_error,
        Some("微信公众号页面需要验证码验证".to_string())
    );
}

#[test]
fn manual_tasks_can_be_listed() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
    let created = store
        .record_link_submission("https://mp.weixin.qq.com/s/manual-list")
        .expect("写入链接失败");

    store
        .claim_task(&created.task_id, "test-worker", 300)
        .expect("claim 失败");
    store
        .mark_task_awaiting_manual_input(
            &created.task_id,
            "微信公众号页面需要验证码验证",
            "wechat_captcha",
            None,
            Some("browser_capture"),
        )
        .expect("更新 awaiting_manual_input 状态失败");

    let tasks = store.list_manual_tasks(10).expect("查询待补录任务失败");

    assert_eq!(
        tasks,
        vec![RecentTaskRecord {
            task_id: created.task_id,
            status: "awaiting_manual_input".to_string(),
            content_source: Some("browser_capture".to_string()),
            page_kind: Some("wechat_captcha".to_string()),
            normalized_url: "https://mp.weixin.qq.com/s/manual-list".to_string(),
            updated_at: tasks[0].updated_at.clone(),
        }]
    );
}

#[test]
fn archived_tasks_can_be_listed() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
    let created = store
        .record_link_submission("https://example.com/archived-list")
        .expect("写入链接失败");

    store
        .claim_task(&created.task_id, "test-worker", 300)
        .expect("claim 失败");

    assert!(store
        .mark_task_archived(
            &created.task_id,
            MarkTaskArchivedInput {
                output_path: "/tmp/archived-list.md",
                title: Some("Archived List Title"),
                page_kind: Some("article"),
                snapshot_path: None,
                content_source: Some("http"),
                summary: None,
            },
        )
        .expect("更新 archived 状态失败"));

    let tasks = store.list_archived_tasks(10).expect("查询 archived 失败");
    assert_eq!(
        tasks,
        vec![ArchivedTaskRecord {
            task_id: created.task_id,
            article_id: created.article_id,
            normalized_url: "https://example.com/archived-list".to_string(),
            title: Some("Archived List Title".to_string()),
            summary: None,
            content_source: Some("http".to_string()),
            page_kind: Some("article".to_string()),
            output_path: Some("/tmp/archived-list.md".to_string()),
            updated_at: tasks[0].updated_at.clone(),
        }]
    );
}

#[test]
fn mark_task_awaiting_manual_input_preserves_snapshot_when_none() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
    let created = store
        .record_link_submission("https://example.com/snapshot-test")
        .expect("写入链接失败");
    store
        .claim_task(&created.task_id, "test-worker", 300)
        .expect("claim 失败");

    // 先手动设置 snapshot_path
    let raw_conn = Connection::open(&db_path).expect("打开 raw 连接失败");
    raw_conn
        .execute(
            "UPDATE tasks SET snapshot_path = 'original_snapshot' WHERE id = ?1",
            [&created.task_id],
        )
        .expect("设置 snapshot_path 失败");
    drop(raw_conn);

    // 传入 None，应保留原有 snapshot_path
    assert!(store
        .mark_task_awaiting_manual_input(
            &created.task_id,
            "需要人工确认",
            "article",
            None,
            Some("browser_capture"),
        )
        .expect("更新 awaiting_manual_input 状态失败"));

    let status = store
        .get_task_status(&created.task_id)
        .expect("查询状态失败")
        .expect("应存在任务");
    assert_eq!(status.snapshot_path, Some("original_snapshot".to_string()));
    assert_eq!(status.content_source, Some("browser_capture".to_string()));
}

#[test]
fn mark_task_failed_preserves_diagnostic_fields() {
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
    let created = store
        .record_link_submission("https://example.com/fail-diag")
        .expect("写入链接失败");
    store
        .claim_task(&created.task_id, "test-worker", 300)
        .expect("claim 失败");

    // 先设置诊断字段
    let raw_conn = Connection::open(&db_path).expect("打开 raw 连接失败");
    raw_conn
        .execute(
            "UPDATE tasks SET page_kind = 'wechat_article', snapshot_path = 'snap.html', content_source = 'browser_capture' WHERE id = ?1",
            [&created.task_id],
        )
        .expect("设置诊断字段失败");
    drop(raw_conn);

    assert!(store
        .mark_task_failed(&created.task_id, "network timeout")
        .expect("更新 failed 状态失败"));

    let status = store
        .get_task_status(&created.task_id)
        .expect("查询状态失败")
        .expect("应存在任务");
    assert_eq!(status.status, "failed");
    assert_eq!(status.last_error, Some("network timeout".to_string()));
    assert_eq!(status.page_kind, Some("wechat_article".to_string()));
    assert_eq!(status.snapshot_path, Some("snap.html".to_string()));
    assert_eq!(status.content_source, Some("browser_capture".to_string()));
    assert_eq!(status.output_path, None);
}
