use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection};
use std::fs;
use std::path::Path;

#[derive(Debug)]
pub struct TaskStore {
    conn: Connection,
}

impl TaskStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建数据库目录失败: {}", parent.display()))?;
        }

        let conn = Connection::open(path)
            .with_context(|| format!("打开 SQLite 数据库失败: {}", path.display()))?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    pub fn record_inbound_message(
        &mut self,
        message_id: &str,
        from_user_id: &str,
        text: &str,
    ) -> Result<bool> {
        let dedup_received_at = Utc::now().to_rfc3339();
        let inbound_received_at = Utc::now().to_rfc3339();
        let tx = self.conn.transaction().context("开启消息写入事务失败")?;
        let inserted = tx.execute(
            r#"
            INSERT OR IGNORE INTO message_dedup (message_id, from_user_id, received_at)
            VALUES (?1, ?2, ?3)
            "#,
            params![message_id, from_user_id, dedup_received_at],
        )?;
        if inserted == 0 {
            tx.rollback().context("回滚重复消息事务失败")?;
            return Ok(false);
        }

        tx.execute(
            r#"
            INSERT INTO inbound_messages (message_id, from_user_id, text, received_at)
            VALUES (?1, ?2, ?3, ?4)
            "#,
            params![message_id, from_user_id, text, inbound_received_at],
        )
        .context("写入入站消息失败")?;
        tx.commit().context("提交消息写入事务失败")?;
        Ok(true)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn
            .execute_batch(
                r#"
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS articles (
                id              TEXT PRIMARY KEY,
                normalized_url  TEXT UNIQUE NOT NULL,
                original_url    TEXT NOT NULL,
                title           TEXT,
                source_domain   TEXT,
                created_at      DATETIME NOT NULL,
                updated_at      DATETIME NOT NULL
            );

            CREATE TABLE IF NOT EXISTS tasks (
                id           TEXT PRIMARY KEY,
                article_id   TEXT NOT NULL REFERENCES articles(id),
                status       TEXT NOT NULL DEFAULT 'pending',
                retry_count  INTEGER NOT NULL DEFAULT 0,
                last_error   TEXT,
                created_at   DATETIME NOT NULL,
                updated_at   DATETIME NOT NULL
            );

            CREATE TABLE IF NOT EXISTS message_dedup (
                message_id    TEXT PRIMARY KEY,
                from_user_id  TEXT NOT NULL,
                received_at   DATETIME NOT NULL
            );

            CREATE TABLE IF NOT EXISTS inbound_messages (
                message_id    TEXT PRIMARY KEY,
                from_user_id  TEXT NOT NULL,
                text          TEXT NOT NULL,
                received_at   DATETIME NOT NULL,
                FOREIGN KEY (message_id) REFERENCES message_dedup(message_id)
            );

            CREATE TABLE IF NOT EXISTS daily_reports (
                date        TEXT PRIMARY KEY,
                report_path TEXT NOT NULL,
                summary     TEXT,
                created_at  DATETIME NOT NULL
            );
            "#,
            )
            .context("初始化 SQLite 表结构失败")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::TaskStore;
    use rusqlite::Connection;
    use std::fs;
    use uuid::Uuid;

    fn temp_db_path() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("amclaw_task_store_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("创建测试目录失败");
        root.join("amclaw.db")
    }

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
}
