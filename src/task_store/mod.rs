use anyhow::{bail, Context, Result};
use chrono::Utc;
use reqwest::Url;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkTaskRecord {
    pub article_id: String,
    pub task_id: String,
    pub normalized_url: String,
    pub original_url: String,
    pub created_new: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskStatusRecord {
    pub task_id: String,
    pub article_id: String,
    pub normalized_url: String,
    pub title: Option<String>,
    pub content_source: Option<String>,
    pub page_kind: Option<String>,
    pub status: String,
    pub retry_count: i64,
    pub last_error: Option<String>,
    pub output_path: Option<String>,
    pub snapshot_path: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentTaskRecord {
    pub task_id: String,
    pub status: String,
    pub content_source: Option<String>,
    pub page_kind: Option<String>,
    pub normalized_url: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedTaskRecord {
    pub task_id: String,
    pub article_id: String,
    pub normalized_url: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub content_source: Option<String>,
    pub page_kind: Option<String>,
    pub output_path: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTaskRecord {
    pub task_id: String,
    pub article_id: String,
    pub normalized_url: String,
    pub original_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkTaskArchivedInput<'a> {
    pub output_path: &'a str,
    pub title: Option<&'a str>,
    pub page_kind: Option<&'a str>,
    pub snapshot_path: Option<&'a str>,
    pub content_source: Option<&'a str>,
    pub summary: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskContentRecord {
    pub task_id: String,
    pub article_id: String,
    pub normalized_url: String,
    pub original_url: String,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSessionRecord {
    pub user_id: String,
    pub merged_text: String,
    pub message_ids: Vec<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMemoryRecord {
    pub id: String,
    pub user_id: String,
    pub content: String,
    pub created_at: String,
    pub updated_at: String,
}

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
            log_task_store_info(
                "inbound_message_deduplicated",
                vec![
                    ("message_id", json!(message_id)),
                    ("user_id", json!(from_user_id)),
                    ("status", json!("deduplicated")),
                ],
            );
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
        log_task_store_info(
            "inbound_message_recorded",
            vec![
                ("message_id", json!(message_id)),
                ("user_id", json!(from_user_id)),
                ("status", json!("recorded")),
                ("text_chars", json!(text.chars().count())),
            ],
        );
        Ok(true)
    }

    pub fn upsert_context_token(&mut self, user_id: &str, context_token: &str) -> Result<()> {
        let user_id = user_id.trim();
        let context_token = context_token.trim();
        if user_id.is_empty() || context_token.is_empty() {
            bail!("user_id/context_token 不能为空");
        }
        let now = Utc::now().to_rfc3339();
        self.conn
            .execute(
                r#"
                INSERT INTO user_context_tokens (user_id, context_token, updated_at)
                VALUES (?1, ?2, ?3)
                ON CONFLICT(user_id) DO UPDATE SET
                    context_token = excluded.context_token,
                    updated_at = excluded.updated_at
                "#,
                params![user_id, context_token, now],
            )
            .context("写入 context_token 失败")?;
        Ok(())
    }

    pub fn get_context_token(&self, user_id: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT context_token FROM user_context_tokens WHERE user_id = ?1",
                [user_id],
                |row| row.get(0),
            )
            .optional()
            .context("查询 context_token 失败")
    }

    pub fn upsert_session_state(
        &mut self,
        user_id: &str,
        merged_text: &str,
        message_ids: &[String],
    ) -> Result<()> {
        let user_id = user_id.trim();
        let merged_text = merged_text.trim();
        if user_id.is_empty() || merged_text.is_empty() {
            bail!("user_id/merged_text 不能为空");
        }
        let updated_at = Utc::now().to_rfc3339();
        let message_ids_json =
            serde_json::to_string(message_ids).context("序列化 session message_ids 失败")?;
        self.conn
            .execute(
                r#"
                INSERT INTO user_sessions (user_id, merged_text, message_ids_json, updated_at)
                VALUES (?1, ?2, ?3, ?4)
                ON CONFLICT(user_id) DO UPDATE SET
                    merged_text = excluded.merged_text,
                    message_ids_json = excluded.message_ids_json,
                    updated_at = excluded.updated_at
                "#,
                params![user_id, merged_text, message_ids_json, updated_at],
            )
            .context("写入 session_state 失败")?;
        Ok(())
    }

    pub fn delete_session_state(&mut self, user_id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM user_sessions WHERE user_id = ?1", [user_id])
            .context("删除 session_state 失败")?;
        Ok(())
    }

    pub fn list_session_states(&self) -> Result<Vec<StoredSessionRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                r#"
                SELECT user_id, merged_text, message_ids_json, updated_at
                FROM user_sessions
                ORDER BY updated_at DESC
                "#,
            )
            .context("准备 session_state 查询失败")?;
        let rows = stmt
            .query_map([], |row| {
                let message_ids_json: String = row.get(2)?;
                let message_ids =
                    serde_json::from_str::<Vec<String>>(&message_ids_json).unwrap_or_default();
                Ok(StoredSessionRecord {
                    user_id: row.get(0)?,
                    merged_text: row.get(1)?,
                    message_ids,
                    updated_at: row.get(3)?,
                })
            })
            .context("查询 session_state 失败")?;
        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row.context("读取 session_state 失败")?);
        }
        Ok(sessions)
    }

    pub fn add_user_memory(&mut self, user_id: &str, content: &str) -> Result<UserMemoryRecord> {
        let user_id = user_id.trim();
        let content = content.trim();
        if user_id.is_empty() || content.is_empty() {
            bail!("user_id/content 不能为空");
        }
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        self.conn
            .execute(
                r#"
                INSERT INTO user_memories (id, user_id, content, created_at, updated_at)
                VALUES (?1, ?2, ?3, ?4, ?5)
                "#,
                params![id, user_id, content, now.clone(), now.clone()],
            )
            .context("写入 user_memory 失败")?;
        Ok(UserMemoryRecord {
            id,
            user_id: user_id.to_string(),
            content: content.to_string(),
            created_at: now.clone(),
            updated_at: now,
        })
    }

    pub fn list_user_memories(&self, user_id: &str, limit: usize) -> Result<Vec<UserMemoryRecord>> {
        let limit = i64::try_from(limit).context("memory limit 超出范围")?;
        let mut stmt = self
            .conn
            .prepare(
                r#"
                SELECT id, user_id, content, created_at, updated_at
                FROM user_memories
                WHERE user_id = ?1
                ORDER BY updated_at DESC, created_at DESC
                LIMIT ?2
                "#,
            )
            .context("准备 user_memory 查询失败")?;
        let rows = stmt
            .query_map(params![user_id, limit], |row| {
                Ok(UserMemoryRecord {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    content: row.get(2)?,
                    created_at: row.get(3)?,
                    updated_at: row.get(4)?,
                })
            })
            .context("查询 user_memory 失败")?;
        let mut memories = Vec::new();
        for row in rows {
            memories.push(row.context("读取 user_memory 失败")?);
        }
        Ok(memories)
    }

    pub fn has_user_memory(&self, user_id: &str, content: &str) -> Result<bool> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM user_memories WHERE user_id = ?1 AND content = ?2",
                params![user_id, content],
                |row| row.get(0),
            )
            .context("查询 user_memory 去重失败")?;
        Ok(count > 0)
    }

    pub fn record_link_submission(&mut self, original_url: &str) -> Result<LinkTaskRecord> {
        let normalized_url = normalize_url(original_url)?;
        let source_domain = source_domain(&normalized_url);
        let now = Utc::now().to_rfc3339();
        let tx = self.conn.transaction().context("开启链接写入事务失败")?;

        let existing_article_id: Option<String> = tx
            .query_row(
                "SELECT id FROM articles WHERE normalized_url = ?1",
                [&normalized_url],
                |row| row.get(0),
            )
            .ok();

        let (article_id, created_new) = if let Some(article_id) = existing_article_id {
            tx.execute(
                "UPDATE articles SET updated_at = ?2 WHERE id = ?1",
                params![article_id, now.clone()],
            )
            .context("更新文章时间失败")?;
            (article_id, false)
        } else {
            let article_id = Uuid::new_v4().to_string();
            tx.execute(
                r#"
                INSERT INTO articles (
                    id, normalized_url, original_url, title, source_domain, created_at, updated_at
                ) VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6)
                "#,
                params![
                    article_id,
                    normalized_url,
                    original_url,
                    source_domain,
                    now.clone(),
                    now.clone()
                ],
            )
            .context("写入文章失败")?;
            (article_id, true)
        };

        let existing_task_id: Option<String> = tx
            .query_row(
                "SELECT id FROM tasks WHERE article_id = ?1 ORDER BY created_at ASC LIMIT 1",
                [&article_id],
                |row| row.get(0),
            )
            .ok();

        let task_id = if let Some(task_id) = existing_task_id {
            task_id
        } else {
            let task_id = Uuid::new_v4().to_string();
            tx.execute(
                r#"
                INSERT INTO tasks (
                    id, article_id, status, retry_count, last_error, created_at, updated_at
                ) VALUES (?1, ?2, 'pending', 0, NULL, ?3, ?4)
                "#,
                params![task_id, article_id, now.clone(), now.clone()],
            )
            .context("写入任务失败")?;
            task_id
        };

        tx.commit().context("提交链接写入事务失败")?;
        let record = LinkTaskRecord {
            article_id,
            task_id,
            normalized_url,
            original_url: original_url.to_string(),
            created_new,
        };
        log_task_store_info(
            "task_created",
            vec![
                ("task_id", json!(record.task_id)),
                ("article_id", json!(record.article_id)),
                ("url", json!(record.normalized_url)),
                (
                    "status",
                    json!(if record.created_new {
                        "created"
                    } else {
                        "existing"
                    }),
                ),
                ("created_new", json!(record.created_new)),
            ],
        );
        Ok(record)
    }

    pub fn get_task_status(&self, task_id: &str) -> Result<Option<TaskStatusRecord>> {
        let task = self
            .conn
            .query_row(
                r#"
                SELECT t.id, t.article_id, a.normalized_url, a.title, t.content_source, t.page_kind, t.status, t.retry_count, t.last_error, t.output_path, t.snapshot_path, t.created_at, t.updated_at
                FROM tasks t
                JOIN articles a ON a.id = t.article_id
                WHERE t.id = ?1
                "#,
                [task_id],
                |row| {
                    Ok(TaskStatusRecord {
                        task_id: row.get(0)?,
                        article_id: row.get(1)?,
                        normalized_url: row.get(2)?,
                        title: row.get(3)?,
                        content_source: row.get(4)?,
                        page_kind: row.get(5)?,
                        status: row.get(6)?,
                        retry_count: row.get(7)?,
                        last_error: row.get(8)?,
                        output_path: row.get(9)?,
                        snapshot_path: row.get(10)?,
                        created_at: row.get(11)?,
                        updated_at: row.get(12)?,
                    })
                },
            )
            .optional()
            .context("查询任务状态失败")?;
        Ok(task)
    }

    pub fn get_task_content(&self, task_id: &str) -> Result<Option<TaskContentRecord>> {
        let task = self
            .conn
            .query_row(
                r#"
                SELECT t.id, t.article_id, a.normalized_url, a.original_url, a.title
                FROM tasks t
                JOIN articles a ON a.id = t.article_id
                WHERE t.id = ?1
                "#,
                [task_id],
                |row| {
                    Ok(TaskContentRecord {
                        task_id: row.get(0)?,
                        article_id: row.get(1)?,
                        normalized_url: row.get(2)?,
                        original_url: row.get(3)?,
                        title: row.get(4)?,
                    })
                },
            )
            .optional()
            .context("查询任务上下文失败")?;
        Ok(task)
    }

    pub fn list_recent_tasks(&self, limit: usize) -> Result<Vec<RecentTaskRecord>> {
        let limit = i64::try_from(limit).context("recent task limit 超出范围")?;
        let mut stmt = self
            .conn
            .prepare(
                r#"
                SELECT t.id, t.status, t.content_source, t.page_kind, a.normalized_url, t.updated_at
                FROM tasks t
                JOIN articles a ON a.id = t.article_id
                ORDER BY t.updated_at DESC, t.created_at DESC
                LIMIT ?1
                "#,
            )
            .context("准备最近任务查询失败")?;
        let rows = stmt
            .query_map([limit], |row| {
                Ok(RecentTaskRecord {
                    task_id: row.get(0)?,
                    status: row.get(1)?,
                    content_source: row.get(2)?,
                    page_kind: row.get(3)?,
                    normalized_url: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })
            .context("查询最近任务失败")?;

        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row.context("读取最近任务记录失败")?);
        }
        Ok(tasks)
    }

    pub fn list_manual_tasks(&self, limit: usize) -> Result<Vec<RecentTaskRecord>> {
        let limit = i64::try_from(limit).context("manual task limit 超出范围")?;
        let mut stmt = self
            .conn
            .prepare(
                r#"
                SELECT t.id, t.status, t.content_source, t.page_kind, a.normalized_url, t.updated_at
                FROM tasks t
                JOIN articles a ON a.id = t.article_id
                WHERE t.status = 'awaiting_manual_input'
                ORDER BY t.updated_at DESC, t.created_at DESC
                LIMIT ?1
                "#,
            )
            .context("准备待补录任务查询失败")?;
        let rows = stmt
            .query_map([limit], |row| {
                Ok(RecentTaskRecord {
                    task_id: row.get(0)?,
                    status: row.get(1)?,
                    content_source: row.get(2)?,
                    page_kind: row.get(3)?,
                    normalized_url: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })
            .context("查询待补录任务失败")?;

        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row.context("读取待补录任务记录失败")?);
        }
        Ok(tasks)
    }

    pub fn list_archived_tasks(&self, limit: usize) -> Result<Vec<ArchivedTaskRecord>> {
        let limit = i64::try_from(limit).context("archived task limit 超出范围")?;
        let mut stmt = self
            .conn
            .prepare(
                r#"
                SELECT t.id, t.article_id, a.normalized_url, a.title, a.summary, t.content_source, t.page_kind, t.output_path, t.updated_at
                FROM tasks t
                JOIN articles a ON a.id = t.article_id
                WHERE t.status = 'archived'
                ORDER BY t.updated_at DESC, t.created_at DESC
                LIMIT ?1
                "#,
            )
            .context("准备 archived 任务查询失败")?;
        let rows = stmt
            .query_map([limit], |row| {
                Ok(ArchivedTaskRecord {
                    task_id: row.get(0)?,
                    article_id: row.get(1)?,
                    normalized_url: row.get(2)?,
                    title: row.get(3)?,
                    summary: row.get(4)?,
                    content_source: row.get(5)?,
                    page_kind: row.get(6)?,
                    output_path: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            })
            .context("查询 archived 任务失败")?;

        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row.context("读取 archived 任务记录失败")?);
        }
        Ok(tasks)
    }

    pub fn retry_task(&mut self, task_id: &str) -> Result<Option<TaskStatusRecord>> {
        let now = Utc::now().to_rfc3339();
        let tx = self.conn.transaction().context("开启重试事务失败")?;
        let updated = tx
            .execute(
                r#"
                UPDATE tasks
                SET status = 'pending',
                    retry_count = retry_count + 1,
                    last_error = NULL,
                    page_kind = NULL,
                    output_path = NULL,
                    snapshot_path = NULL,
                    updated_at = ?2
                WHERE id = ?1
                "#,
                params![task_id, now],
            )
            .context("更新任务重试状态失败")?;
        if updated == 0 {
            tx.rollback().context("回滚不存在任务事务失败")?;
            log_task_store_warn(
                "task_retry_requested",
                vec![("task_id", json!(task_id)), ("status", json!("missing"))],
            );
            return Ok(None);
        }
        let task = tx
            .query_row(
                r#"
                SELECT t.id, t.article_id, a.normalized_url, a.title, t.content_source, t.page_kind, t.status, t.retry_count, t.last_error, t.output_path, t.snapshot_path, t.created_at, t.updated_at
                FROM tasks t
                JOIN articles a ON a.id = t.article_id
                WHERE t.id = ?1
                "#,
                [task_id],
                |row| {
                    Ok(TaskStatusRecord {
                        task_id: row.get(0)?,
                        article_id: row.get(1)?,
                        normalized_url: row.get(2)?,
                        title: row.get(3)?,
                        content_source: row.get(4)?,
                        page_kind: row.get(5)?,
                        status: row.get(6)?,
                        retry_count: row.get(7)?,
                        last_error: row.get(8)?,
                        output_path: row.get(9)?,
                        snapshot_path: row.get(10)?,
                        created_at: row.get(11)?,
                        updated_at: row.get(12)?,
                    })
                },
            )
            .context("读取重试后的任务状态失败")?;
        tx.commit().context("提交重试事务失败")?;
        log_task_store_info(
            "task_retry_requested",
            vec![
                ("task_id", json!(task.task_id)),
                ("article_id", json!(task.article_id)),
                ("status", json!(task.status)),
                ("retry_count", json!(task.retry_count)),
            ],
        );
        Ok(Some(task))
    }

    pub fn list_pending_tasks(&self, limit: usize) -> Result<Vec<PendingTaskRecord>> {
        let limit = i64::try_from(limit).context("pending task limit 超出范围")?;
        let mut stmt = self
            .conn
            .prepare(
                r#"
                SELECT t.id, t.article_id, a.normalized_url, a.original_url
                FROM tasks t
                JOIN articles a ON a.id = t.article_id
                WHERE t.status = 'pending'
                ORDER BY t.created_at ASC
                LIMIT ?1
                "#,
            )
            .context("准备 pending 任务查询失败")?;
        let rows = stmt
            .query_map([limit], |row| {
                Ok(PendingTaskRecord {
                    task_id: row.get(0)?,
                    article_id: row.get(1)?,
                    normalized_url: row.get(2)?,
                    original_url: row.get(3)?,
                })
            })
            .context("查询 pending 任务失败")?;

        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row.context("读取 pending 任务记录失败")?);
        }
        Ok(tasks)
    }

    pub fn get_pending_task(&self, task_id: &str) -> Result<Option<PendingTaskRecord>> {
        self.conn
            .query_row(
                r#"
                SELECT t.id, t.article_id, a.normalized_url, a.original_url
                FROM tasks t
                JOIN articles a ON a.id = t.article_id
                WHERE t.id = ?1 AND t.status = 'pending'
                "#,
                [task_id],
                |row| {
                    Ok(PendingTaskRecord {
                        task_id: row.get(0)?,
                        article_id: row.get(1)?,
                        normalized_url: row.get(2)?,
                        original_url: row.get(3)?,
                    })
                },
            )
            .optional()
            .context("查询指定 pending 任务失败")
    }

    pub fn mark_task_archived(
        &mut self,
        task_id: &str,
        input: MarkTaskArchivedInput<'_>,
    ) -> Result<bool> {
        let MarkTaskArchivedInput {
            output_path,
            title,
            page_kind,
            snapshot_path,
            content_source,
            summary,
        } = input;
        let now = Utc::now().to_rfc3339();
        let tx = self.conn.transaction().context("开启 archived 事务失败")?;
        let updated = tx
            .execute(
                "UPDATE tasks SET status = 'archived', last_error = NULL, output_path = ?2, page_kind = COALESCE(?3, page_kind), snapshot_path = ?4, content_source = COALESCE(?5, content_source), updated_at = ?6 WHERE id = ?1",
                params![task_id, output_path, page_kind, snapshot_path, content_source, now.clone()],
            )
            .context("更新 archived 状态失败")?;
        if updated == 0 {
            tx.rollback().context("回滚不存在任务 archived 事务失败")?;
            log_task_store_warn(
                "task_status_changed",
                vec![
                    ("task_id", json!(task_id)),
                    ("status", json!("missing")),
                    ("target_status", json!("archived")),
                ],
            );
            return Ok(false);
        }
        if let Some(title) = title.filter(|v| !v.trim().is_empty()) {
            tx.execute(
                r#"
                UPDATE articles
                SET title = COALESCE(title, ?2), updated_at = ?3
                WHERE id = (SELECT article_id FROM tasks WHERE id = ?1)
                "#,
                params![task_id, title, now.clone()],
            )
            .context("更新文章标题失败")?;
        }
        if let Some(summary) = summary.filter(|v| !v.trim().is_empty()) {
            tx.execute(
                r#"
                UPDATE articles
                SET summary = ?2, updated_at = ?3
                WHERE id = (SELECT article_id FROM tasks WHERE id = ?1)
                "#,
                params![task_id, summary, now.clone()],
            )
            .context("更新文章摘要失败")?;
        }
        tx.commit().context("提交 archived 事务失败")?;
        log_task_store_info(
            "task_status_changed",
            vec![
                ("task_id", json!(task_id)),
                ("status", json!("archived")),
                ("page_kind", json!(page_kind)),
                ("content_source", json!(content_source)),
                ("output_path", json!(output_path)),
                ("snapshot_path", json!(snapshot_path)),
            ],
        );
        Ok(updated > 0)
    }

    pub fn mark_task_awaiting_manual_input(
        &mut self,
        task_id: &str,
        last_error: &str,
        page_kind: &str,
        snapshot_path: Option<&str>,
        content_source: Option<&str>,
    ) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let updated = self
            .conn
            .execute(
                "UPDATE tasks SET status = 'awaiting_manual_input', last_error = ?2, page_kind = ?3, snapshot_path = ?4, content_source = COALESCE(?5, content_source), output_path = NULL, updated_at = ?6 WHERE id = ?1",
                params![task_id, last_error, page_kind, snapshot_path, content_source, now],
            )
            .context("更新 awaiting_manual_input 状态失败")?;
        if updated > 0 {
            log_task_store_warn(
                "task_status_changed",
                vec![
                    ("task_id", json!(task_id)),
                    ("status", json!("awaiting_manual_input")),
                    ("page_kind", json!(page_kind)),
                    ("content_source", json!(content_source)),
                    ("snapshot_path", json!(snapshot_path)),
                    ("error_kind", json!("awaiting_manual_input")),
                    ("detail", json!(summarize_text_for_log(last_error, 160))),
                ],
            );
        } else {
            log_task_store_warn(
                "task_status_changed",
                vec![
                    ("task_id", json!(task_id)),
                    ("status", json!("missing")),
                    ("target_status", json!("awaiting_manual_input")),
                ],
            );
        }
        Ok(updated > 0)
    }

    pub fn mark_task_failed(&mut self, task_id: &str, last_error: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let updated = self
            .conn
            .execute(
                "UPDATE tasks SET status = 'failed', last_error = ?2, page_kind = NULL, output_path = NULL, snapshot_path = NULL, content_source = NULL, updated_at = ?3 WHERE id = ?1",
                params![task_id, last_error, now],
            )
            .context("更新 failed 状态失败")?;
        if updated > 0 {
            log_task_store_error(
                "task_status_changed",
                vec![
                    ("task_id", json!(task_id)),
                    ("status", json!("failed")),
                    ("error_kind", json!("task_failed")),
                    ("detail", json!(summarize_text_for_log(last_error, 160))),
                ],
            );
        } else {
            log_task_store_warn(
                "task_status_changed",
                vec![
                    ("task_id", json!(task_id)),
                    ("status", json!("missing")),
                    ("target_status", json!("failed")),
                ],
            );
        }
        Ok(updated > 0)
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
                content_source TEXT,
                page_kind    TEXT,
                output_path  TEXT,
                snapshot_path TEXT,
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

            CREATE TABLE IF NOT EXISTS user_memories (
                id         TEXT PRIMARY KEY,
                user_id    TEXT NOT NULL,
                content    TEXT NOT NULL,
                created_at DATETIME NOT NULL,
                updated_at DATETIME NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_articles_normalized_url ON articles(normalized_url);
            CREATE INDEX IF NOT EXISTS idx_tasks_article_id ON tasks(article_id);
            CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
            CREATE INDEX IF NOT EXISTS idx_tasks_updated_at ON tasks(updated_at);
            CREATE INDEX IF NOT EXISTS idx_inbound_messages_received_at ON inbound_messages(received_at);
            "#,
            )
            .context("初始化 SQLite 表结构失败")?;
        ensure_column_exists(&self.conn, "tasks", "content_source", "TEXT")?;
        ensure_column_exists(&self.conn, "tasks", "page_kind", "TEXT")?;
        ensure_column_exists(&self.conn, "tasks", "output_path", "TEXT")?;
        ensure_column_exists(&self.conn, "tasks", "snapshot_path", "TEXT")?;
        ensure_column_exists(&self.conn, "articles", "summary", "TEXT")?;
        Ok(())
    }
}

fn ensure_column_exists(
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
        .with_context(|| format!("补充列失败: {table}.{column}"))?;
    Ok(())
}

fn log_task_store_info(event: &str, fields: Vec<(&str, Value)>) {
    log_task_store_event("info", event, fields);
}

fn log_task_store_warn(event: &str, fields: Vec<(&str, Value)>) {
    log_task_store_event("warn", event, fields);
}

fn log_task_store_error(event: &str, fields: Vec<(&str, Value)>) {
    log_task_store_event("error", event, fields);
}

fn log_task_store_event(level: &str, event: &str, fields: Vec<(&str, Value)>) {
    crate::logging::emit_structured_log(level, event, fields);
}

#[cfg(test)]
fn build_task_store_log_payload(level: &str, event: &str, fields: Vec<(&str, Value)>) -> Value {
    crate::logging::build_structured_log_payload(level, event, fields)
}

fn summarize_text_for_log(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut output: String = input.chars().take(max_chars).collect();
    output.push_str("...");
    output
}

fn normalize_url(input: &str) -> Result<String> {
    let mut url = Url::parse(input).with_context(|| format!("无效 URL: {input}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("仅支持 http/https URL: {input}");
    }
    url.set_fragment(None);
    strip_tracking_query_pairs(&mut url);
    let mut normalized = url.to_string();
    if url.path() == "/" && url.query().is_none() && normalized.ends_with('/') {
        normalized.pop();
    }
    Ok(normalized)
}

fn strip_tracking_query_pairs(url: &mut Url) {
    let tracking_keys: HashSet<&str> = [
        "fbclid", "gclid", "mc_cid", "mc_eid", "mkt_tok", "spm", "si",
    ]
    .into_iter()
    .collect();

    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| {
            let key = key.as_ref();
            !key.starts_with("utm_") && !tracking_keys.contains(key)
        })
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();

    url.set_query(None);
    if pairs.is_empty() {
        return;
    }

    let mut query_pairs = url.query_pairs_mut();
    for (key, value) in pairs {
        query_pairs.append_pair(&key, &value);
    }
}

fn source_domain(normalized_url: &str) -> Option<String> {
    Url::parse(normalized_url)
        .ok()
        .and_then(|url| url.host_str().map(ToOwned::to_owned))
}

#[cfg(test)]
mod tests {
    use super::{
        build_task_store_log_payload, ArchivedTaskRecord, LinkTaskRecord, MarkTaskArchivedInput,
        PendingTaskRecord, RecentTaskRecord, StoredSessionRecord, TaskStatusRecord, TaskStore,
        UserMemoryRecord,
    };
    use rusqlite::Connection;
    use serde_json::{json, Value};
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
    fn tracking_query_params_are_removed_during_normalization() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        let first = store
            .record_link_submission("https://example.com/page?utm_source=x&gclid=1&id=42")
            .expect("首次写入链接失败");
        let second = store
            .record_link_submission("https://example.com/page?id=42&utm_medium=email")
            .expect("重复写入链接失败");

        assert_eq!(first.normalized_url, "https://example.com/page?id=42");
        assert_eq!(second.normalized_url, "https://example.com/page?id=42");
        assert!(!second.created_new);
    }

    #[test]
    fn non_http_scheme_is_rejected_during_link_submission() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        let err = store
            .record_link_submission("file:///tmp/demo.html")
            .expect_err("应拒绝非 http/https 协议");

        assert!(err.to_string().contains("仅支持 http/https URL"));
    }

    #[test]
    fn javascript_scheme_is_rejected_during_link_submission() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        let err = store
            .record_link_submission("javascript:alert(1)")
            .expect_err("应拒绝 javascript 协议");

        assert!(err.to_string().contains("仅支持 http/https URL"));
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
    fn context_token_can_be_persisted_and_loaded() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        store
            .upsert_context_token("user-a", "ctx-1")
            .expect("写入 token 失败");
        assert_eq!(
            store.get_context_token("user-a").expect("读取 token 失败"),
            Some("ctx-1".to_string())
        );

        store
            .upsert_context_token("user-a", "ctx-2")
            .expect("更新 token 失败");
        assert_eq!(
            store.get_context_token("user-a").expect("读取 token 失败"),
            Some("ctx-2".to_string())
        );
    }

    #[test]
    fn session_state_can_be_persisted_listed_and_deleted() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        store
            .upsert_session_state(
                "user-a",
                "hello\nworld",
                &["msg-1".to_string(), "msg-2".to_string()],
            )
            .expect("写入 session_state 失败");

        let sessions = store
            .list_session_states()
            .expect("查询 session_state 失败");
        assert_eq!(
            sessions.len(),
            1,
            "应只有一条 session_state，实际: {:?}",
            sessions
        );
        assert_eq!(
            sessions[0],
            StoredSessionRecord {
                user_id: "user-a".to_string(),
                merged_text: "hello\nworld".to_string(),
                message_ids: vec!["msg-1".to_string(), "msg-2".to_string()],
                updated_at: sessions[0].updated_at.clone(),
            }
        );

        store
            .delete_session_state("user-a")
            .expect("删除 session_state 失败");
        assert!(store
            .list_session_states()
            .expect("查询 session_state 失败")
            .is_empty());
    }

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
    fn task_store_log_payload_keeps_contract_fields() {
        let payload = build_task_store_log_payload(
            "error",
            "task_status_changed",
            vec![
                ("task_id", json!("task-1")),
                ("status", json!("failed")),
                ("detail", Value::Null),
            ],
        );

        assert_eq!(payload["level"], "error");
        assert_eq!(payload["event"], "task_status_changed");
        assert_eq!(payload["task_id"], "task-1");
        assert_eq!(payload["status"], "failed");
        assert!(payload.get("ts").is_some());
        assert!(payload.get("detail").is_none());
    }

    #[test]
    fn summary_is_overwritten_on_rerun() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let created = store
            .record_link_submission("https://example.com/summary-rerun")
            .expect("写入链接失败");

        store
            .mark_task_archived(
                &created.task_id,
                MarkTaskArchivedInput {
                    output_path: "/tmp/summary-rerun.md",
                    title: Some("Summary Rerun"),
                    page_kind: Some("article"),
                    snapshot_path: None,
                    content_source: Some("http"),
                    summary: Some("初始摘要"),
                },
            )
            .expect("首次 archived 失败");

        // Simulate retry: reset then re-archive with better summary
        let conn = Connection::open(&db_path).expect("打开数据库失败");
        conn.execute(
            "UPDATE tasks SET status = 'pending', output_path = NULL WHERE id = ?1",
            [created.task_id.as_str()],
        )
        .expect("重置任务状态失败");
        drop(conn);

        store
            .mark_task_archived(
                &created.task_id,
                MarkTaskArchivedInput {
                    output_path: "/tmp/summary-rerun-v2.md",
                    title: Some("Summary Rerun"),
                    page_kind: Some("article"),
                    snapshot_path: None,
                    content_source: Some("http"),
                    summary: Some("更精确的LLM摘要"),
                },
            )
            .expect("二次 archived 失败");

        let archived = store.list_archived_tasks(10).expect("查询失败");
        assert_eq!(archived[0].summary, Some("更精确的LLM摘要".to_string()));
    }
}
