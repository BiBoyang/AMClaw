use super::super::TaskStore;
use super::temp_db_path;
use rusqlite::Connection;
use std::sync::Arc;
use std::thread;

#[test]
fn schema_is_created() {
    let db_path = temp_db_path();
    TaskStore::open(&db_path).expect("初始化 task store 失败");

    let conn = Connection::open(&db_path).expect("打开数据库失败");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'inbound_messages'",
            [],
            |row| row.get(0),
        )
        .expect("查询表结构失败");

    assert_eq!(count, 1);
}

#[test]
fn duplicate_message_is_ignored_even_after_reopen() {
    let db_path = temp_db_path();

    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
    assert!(store
        .record_inbound_message("msg-1", "user-a", "hello")
        .expect("首次写入失败"));
    assert!(!store
        .record_inbound_message("msg-1", "user-a", "hello")
        .expect("重复写入失败"));
    drop(store);

    let mut reopened = TaskStore::open(&db_path).expect("重新打开 task store 失败");
    assert!(!reopened
        .record_inbound_message("msg-1", "user-a", "hello")
        .expect("重启后重复写入失败"));
}

#[test]
fn inbound_message_text_is_persisted() {
    let db_path = temp_db_path();

    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
    store
        .record_inbound_message("msg-2", "user-b", "https://example.com hello")
        .expect("写入入站消息失败");
    drop(store);

    let conn = Connection::open(&db_path).expect("打开数据库失败");
    let row: (String, String, String) = conn
        .query_row(
            "SELECT message_id, from_user_id, text FROM inbound_messages WHERE message_id = 'msg-2'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("查询入站消息失败");

    assert_eq!(
        row,
        (
            "msg-2".to_string(),
            "user-b".to_string(),
            "https://example.com hello".to_string(),
        )
    );
}

#[test]
fn duplicate_message_does_not_create_second_inbound_row() {
    let db_path = temp_db_path();

    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
    assert!(store
        .record_inbound_message("msg-3", "user-c", "first")
        .expect("首次写入失败"));
    assert!(!store
        .record_inbound_message("msg-3", "user-c", "second")
        .expect("重复写入失败"));
    drop(store);

    let conn = Connection::open(&db_path).expect("打开数据库失败");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM inbound_messages WHERE message_id = 'msg-3'",
            [],
            |row| row.get(0),
        )
        .expect("查询入站消息数量失败");
    let text: String = conn
        .query_row(
            "SELECT text FROM inbound_messages WHERE message_id = 'msg-3'",
            [],
            |row| row.get(0),
        )
        .expect("查询入站消息文本失败");

    assert_eq!(count, 1);
    assert_eq!(text, "first");
}

#[test]
fn concurrent_writes_do_not_panic_on_busy() {
    let db_path = temp_db_path();
    let db_path = Arc::new(db_path);
    let threads: Vec<_> = (0..4)
        .map(|tid| {
            let path = Arc::clone(&db_path);
            thread::spawn(move || {
                let mut store = TaskStore::open(&*path).expect("并发线程初始化 task store 失败");
                for i in 0..10 {
                    let msg_id = format!("msg-t{tid}-i{i}");
                    store
                        .record_inbound_message(&msg_id, "user-a", "hello")
                        .expect("并发写入不应 panic 或返回 BUSY");
                }
            })
        })
        .collect();

    for t in threads {
        t.join().expect("并发线程不应 panic");
    }

    let conn = Connection::open(&*db_path).expect("验证读取失败");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM message_dedup", [], |row| row.get(0))
        .expect("计数失败");
    assert_eq!(count, 40, "40 条独立消息应全部写入");
}
