use anyhow::{Context, Result};
use rusqlite::Connection;

impl super::TaskStore {
    pub(super) fn init_schema(&self) -> Result<()> {
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
                content_source TEXT,
                page_kind    TEXT,
                output_path  TEXT,
                snapshot_path TEXT,
                worker_id    TEXT,
                processing_started_at DATETIME,
                lease_until  DATETIME,
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

            CREATE TABLE IF NOT EXISTS user_context_tokens (
                user_id       TEXT PRIMARY KEY,
                context_token TEXT NOT NULL,
                updated_at    DATETIME NOT NULL
            );

            CREATE TABLE IF NOT EXISTS user_sessions (
                user_id          TEXT PRIMARY KEY,
                merged_text      TEXT NOT NULL,
                message_ids_json TEXT NOT NULL,
                updated_at       DATETIME NOT NULL
            );

            CREATE TABLE IF NOT EXISTS user_session_states (
                user_id          TEXT PRIMARY KEY,
                last_user_intent TEXT,
                current_task     TEXT,
                next_step        TEXT,
                blocked_reason   TEXT,
                goal             TEXT,
                current_subtask  TEXT,
                constraints_json TEXT,
                confirmed_facts_json TEXT,
                done_items_json  TEXT,
                open_questions_json TEXT,
                updated_at       DATETIME NOT NULL
            );

            CREATE TABLE IF NOT EXISTS user_memories (
                id          TEXT PRIMARY KEY,
                user_id     TEXT NOT NULL,
                content     TEXT NOT NULL,
                memory_type TEXT NOT NULL DEFAULT 'explicit',
                status      TEXT NOT NULL DEFAULT 'active',
                priority    INTEGER NOT NULL DEFAULT 100,
                last_used_at DATETIME,
                use_count   INTEGER NOT NULL DEFAULT 0,
                created_at  DATETIME NOT NULL,
                updated_at  DATETIME NOT NULL
            );

            CREATE TABLE IF NOT EXISTS embedding_cache (
                text_hash   TEXT NOT NULL,
                model_name  TEXT NOT NULL,
                vector_json TEXT NOT NULL,
                dimension   INTEGER NOT NULL,
                created_at  DATETIME NOT NULL,
                PRIMARY KEY (text_hash, model_name)
            );

            CREATE INDEX IF NOT EXISTS idx_articles_normalized_url ON articles(normalized_url);
            CREATE INDEX IF NOT EXISTS idx_tasks_article_id ON tasks(article_id);
            CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
            CREATE INDEX IF NOT EXISTS idx_tasks_updated_at ON tasks(updated_at);
            CREATE INDEX IF NOT EXISTS idx_tasks_status_lease ON tasks(status, lease_until);
            CREATE INDEX IF NOT EXISTS idx_inbound_messages_received_at ON inbound_messages(received_at);

            CREATE TABLE IF NOT EXISTS outbound_pending_chunks (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id       TEXT NOT NULL,
                context_token TEXT NOT NULL,
                chunk_text    TEXT NOT NULL,
                chunk_index   INTEGER NOT NULL,
                chunk_total   INTEGER NOT NULL,
                created_at    DATETIME NOT NULL
            );
            "#,
            )
            .context("初始化 SQLite 表结构失败")?;
        ensure_column_exists(&self.conn, "tasks", "content_source", "TEXT")?;
        ensure_column_exists(&self.conn, "tasks", "page_kind", "TEXT")?;
        ensure_column_exists(&self.conn, "tasks", "output_path", "TEXT")?;
        ensure_column_exists(&self.conn, "tasks", "snapshot_path", "TEXT")?;
        ensure_column_exists(&self.conn, "tasks", "worker_id", "TEXT")?;
        ensure_column_exists(&self.conn, "tasks", "processing_started_at", "DATETIME")?;
        ensure_column_exists(&self.conn, "tasks", "lease_until", "DATETIME")?;
        ensure_column_exists(&self.conn, "articles", "summary", "TEXT")?;
        ensure_column_exists(
            &self.conn,
            "user_memories",
            "memory_type",
            "TEXT NOT NULL DEFAULT 'explicit'",
        )?;
        ensure_column_exists(
            &self.conn,
            "user_memories",
            "status",
            "TEXT NOT NULL DEFAULT 'active'",
        )?;
        ensure_column_exists(
            &self.conn,
            "user_memories",
            "priority",
            "INTEGER NOT NULL DEFAULT 100",
        )?;
        ensure_column_exists(&self.conn, "user_memories", "last_used_at", "DATETIME")?;
        ensure_column_exists(
            &self.conn,
            "user_memories",
            "use_count",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column_exists(
            &self.conn,
            "user_memories",
            "retrieved_count",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column_exists(
            &self.conn,
            "user_memories",
            "injected_count",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column_exists(
            &self.conn,
            "user_memories",
            "useful",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        // v2 session state 字段迁移
        ensure_column_exists(&self.conn, "user_session_states", "goal", "TEXT")?;
        ensure_column_exists(&self.conn, "user_session_states", "current_subtask", "TEXT")?;
        ensure_column_exists(
            &self.conn,
            "user_session_states",
            "constraints_json",
            "TEXT",
        )?;
        ensure_column_exists(
            &self.conn,
            "user_session_states",
            "confirmed_facts_json",
            "TEXT",
        )?;
        ensure_column_exists(&self.conn, "user_session_states", "done_items_json", "TEXT")?;
        ensure_column_exists(
            &self.conn,
            "user_session_states",
            "open_questions_json",
            "TEXT",
        )?;
        // v3 outbound_pending_chunks 表创建（兼容旧库）
        self.conn
            .execute(
                r#"
                CREATE TABLE IF NOT EXISTS outbound_pending_chunks (
                    id            INTEGER PRIMARY KEY AUTOINCREMENT,
                    user_id       TEXT NOT NULL,
                    context_token TEXT NOT NULL,
                    chunk_text    TEXT NOT NULL,
                    chunk_index   INTEGER NOT NULL,
                    chunk_total   INTEGER NOT NULL,
                    created_at    DATETIME NOT NULL
                )
                "#,
                [],
            )
            .context("创建 outbound_pending_chunks 表失败")?;
        Ok(())
    }
}

pub(super) fn ensure_column_exists(
    conn: &Connection,
    table: &str,
    column: &str,
    column_def: &str,
) -> Result<()> {
    let pragma = format!("PRAGMA table_info({table})");
    let mut stmt = conn
        .prepare(&pragma)
        .with_context(|| format!("准备表结构检查失败: {table}"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .with_context(|| format!("读取表结构失败: {table}"))?;

    for row in rows {
        if row.context("读取列名失败")? == column {
            return Ok(());
        }
    }

    let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {column_def}");
    conn.execute(&sql, [])
        .or_else(|e| {
            let msg = e.to_string().to_lowercase();
            if msg.contains("duplicate column name") {
                Ok(0)
            } else {
                Err(e)
            }
        })
        .with_context(|| format!("补充列失败: {table}.{column}"))?;
    Ok(())
}
