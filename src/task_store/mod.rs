use anyhow::{bail, Context, Result};
use chrono::Utc;
use reqwest::Url;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use thiserror::Error;
use uuid::Uuid;

/// task_store 模块的错误类型
#[derive(Debug, Error)]
#[allow(dead_code)]
pub enum TaskStoreError {
    #[error("无效 URL: {0}")]
    InvalidUrl(String),
    #[error("不支持内网/本地地址: {0}")]
    PrivateNetworkUrl(String),
    #[error("仅支持 http/https URL: {0}")]
    UnsupportedScheme(String),
    #[error("{0}")]
    Validation(String),
    #[error("数据库错误: {0}")]
    Database(#[from] rusqlite::Error),
}

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
    pub memory_type: String,
    pub status: String,
    pub priority: i64,
    pub last_used_at: Option<String>,
    pub use_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// 内容为空或仅 whitespace
    Empty,
    /// 内容超过单条长度限制
    TooLong,
    /// 自动记忆置信度不足
    TooWeak,
    /// 与已有记忆规范化后重复
    Duplicate,
    /// auto 记忆与已有 explicit 冲突，不允许降级
    AutoWouldDowngradeExplicit,
    /// user_id 或内容格式无效
    Invalid,
    /// 存储写入失败
    StorageError,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "empty_content"),
            Self::TooLong => write!(f, "too_long"),
            Self::TooWeak => write!(f, "too_weak"),
            Self::Duplicate => write!(f, "duplicate"),
            Self::AutoWouldDowngradeExplicit => write!(f, "auto_would_downgrade_explicit"),
            Self::Invalid => write!(f, "invalid"),
            Self::StorageError => write!(f, "storage_error"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromoteReason {
    /// 新 explicit 提升了已有 auto 记忆
    ExplicitPromotesAuto,
}

impl std::fmt::Display for PromoteReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExplicitPromotesAuto => write!(f, "explicit_promotes_auto"),
        }
    }
}

/// 写入决策结果
#[derive(Debug, Clone)]
pub enum WriteDecision {
    /// 新写入成功
    Written(UserMemoryRecord),
    /// 跳过写入
    Skipped {
        content_preview: String,
        reason: SkipReason,
    },
    /// 提升已有 auto 为 explicit（更新已有记录的 type 和 priority）
    Promoted { id: String, reason: PromoteReason },
}

/// 单次 run 的写侧状态
#[derive(Debug, Clone, Default)]
pub struct MemoryWriteState {
    /// 候选数量
    pub candidate_count: usize,
    /// 写入成功
    pub written: Vec<UserMemoryRecord>,
    /// 被跳过
    pub skipped: Vec<(String, SkipReason)>,
    /// 被提升
    pub promoted: Vec<(String, PromoteReason)>,
}

impl MemoryWriteState {
    pub fn written_count(&self) -> usize {
        self.written.len()
    }
    pub fn skipped_count(&self) -> usize {
        self.skipped.len()
    }
    pub fn promoted_count(&self) -> usize {
        self.promoted.len()
    }

    /// 记录一次写入决策
    fn record(&mut self, decision: WriteDecision) {
        match decision {
            WriteDecision::Written(record) => self.written.push(record),
            WriteDecision::Skipped {
                content_preview,
                reason,
            } => self.skipped.push((content_preview, reason)),
            WriteDecision::Promoted { id, reason } => self.promoted.push((id, reason)),
        }
    }
}

/// 最大单条内容长度（写入时校验）
const MAX_MEMORY_WRITE_CHARS: usize = 500;

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

    /// 写入显式用户记忆（用户明确要求"记住"）
    /// memory_type="explicit", priority=100
    pub fn add_user_memory(&mut self, user_id: &str, content: &str) -> Result<UserMemoryRecord> {
        self.add_user_memory_typed(user_id, content, "explicit", 100)
    }

    /// 写入带类型和优先级的用户记忆
    /// - explicit: 用户显式要求记住，priority=100
    /// - auto: 系统自动提炼，priority=60
    pub fn add_user_memory_typed(
        &mut self,
        user_id: &str,
        content: &str,
        memory_type: &str,
        priority: i64,
    ) -> Result<UserMemoryRecord> {
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
                INSERT INTO user_memories (id, user_id, content, memory_type, status, priority, last_used_at, use_count, created_at, updated_at)
                VALUES (?1, ?2, ?3, ?4, 'active', ?5, NULL, 0, ?6, ?7)
                "#,
                params![id, user_id, content, memory_type, priority, now.clone(), now.clone()],
            )
            .context("写入 user_memory 失败")?;
        Ok(UserMemoryRecord {
            id,
            user_id: user_id.to_string(),
            content: content.to_string(),
            memory_type: memory_type.to_string(),
            status: "active".to_string(),
            priority,
            last_used_at: None,
            use_count: 0,
            created_at: now.clone(),
            updated_at: now,
        })
    }

    /// 统一写入治理入口
    ///
    /// 执行：validate → dedup → promote/skip → persist
    /// 返回 WriteDecision，调用方不直接决定是否写入。
    pub fn govern_memory_write(
        &mut self,
        user_id: &str,
        content: &str,
        memory_type: &str,
        priority: i64,
        write_state: &mut MemoryWriteState,
    ) -> WriteDecision {
        write_state.candidate_count += 1;
        let content = content.trim();
        let content_preview = if content.chars().count() > 20 {
            let truncated: String = content.chars().take(20).collect();
            format!("{}...", truncated)
        } else {
            content.to_string()
        };

        // 1. Validate: 空/whitespace
        if content.is_empty() {
            let decision = WriteDecision::Skipped {
                content_preview,
                reason: SkipReason::Empty,
            };
            write_state.record(decision.clone());
            return decision;
        }

        // 2. Validate: 超长
        if content.chars().count() > MAX_MEMORY_WRITE_CHARS {
            let decision = WriteDecision::Skipped {
                content_preview,
                reason: SkipReason::TooLong,
            };
            write_state.record(decision.clone());
            return decision;
        }

        // 3. Validate: user_id
        if user_id.trim().is_empty() {
            let decision = WriteDecision::Skipped {
                content_preview,
                reason: SkipReason::Invalid,
            };
            write_state.record(decision.clone());
            return decision;
        }

        // 4. Dedup: 检查已有记忆（normalize 后比较）
        let normalized: String = content.split_whitespace().collect::<Vec<_>>().join(" ");
        let existing = self.search_user_memories(user_id, 50);

        match existing {
            Ok(memories) => {
                for mem in &memories {
                    let existing_normalized: String =
                        mem.content.split_whitespace().collect::<Vec<_>>().join(" ");
                    if existing_normalized != normalized {
                        continue;
                    }
                    // normalize 后相同
                    if memory_type == "auto" && mem.memory_type == "explicit" {
                        // auto 不允许降级 explicit
                        let decision = WriteDecision::Skipped {
                            content_preview,
                            reason: SkipReason::AutoWouldDowngradeExplicit,
                        };
                        write_state.record(decision.clone());
                        return decision;
                    }
                    if memory_type == "explicit" && mem.memory_type == "auto" {
                        // explicit 提升 auto
                        if let Err(e) = self.promote_memory_to_explicit(&mem.id) {
                            let decision = WriteDecision::Skipped {
                                content_preview,
                                reason: SkipReason::StorageError,
                            };
                            write_state.record(decision.clone());
                            log_task_store_warn(
                                "memory_promote_failed",
                                vec![
                                    ("error_kind", json!("promote_failed")),
                                    ("detail", json!(e.to_string())),
                                ],
                            );
                            return decision;
                        }
                        let decision = WriteDecision::Promoted {
                            id: mem.id.clone(),
                            reason: PromoteReason::ExplicitPromotesAuto,
                        };
                        write_state.record(decision.clone());
                        return decision;
                    }
                    // 同类型重复
                    let decision = WriteDecision::Skipped {
                        content_preview,
                        reason: SkipReason::Duplicate,
                    };
                    write_state.record(decision.clone());
                    return decision;
                }
            }
            Err(err) => {
                log_task_store_warn(
                    "memory_govern_dedup_lookup_failed",
                    vec![
                        ("error_kind", json!("dedup_lookup_failed")),
                        ("detail", json!(err.to_string())),
                    ],
                );
                // 查询失败时 conservative：允许写入
            }
        }

        // 5. 新写入
        match self.add_user_memory_typed(user_id, content, memory_type, priority) {
            Ok(record) => {
                let decision = WriteDecision::Written(record);
                write_state.record(decision.clone());
                decision
            }
            Err(_) => {
                let decision = WriteDecision::Skipped {
                    content_preview,
                    reason: SkipReason::StorageError,
                };
                write_state.record(decision.clone());
                decision
            }
        }
    }

    /// 将已有 auto 记忆提升为 explicit（更新 type + priority）
    fn promote_memory_to_explicit(&self, memory_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn
            .execute(
                "UPDATE user_memories SET memory_type = 'explicit', priority = 100, updated_at = ?1 WHERE id = ?2",
                params![now, memory_id],
            )
            .context("提升 memory 为 explicit 失败")?;
        Ok(())
    }

    pub fn list_user_memories(&self, user_id: &str, limit: usize) -> Result<Vec<UserMemoryRecord>> {
        let limit = i64::try_from(limit).context("memory limit 超出范围")?;
        let mut stmt = self
            .conn
            .prepare(
                r#"
                SELECT id, user_id, content, memory_type, status, priority, last_used_at, use_count, created_at, updated_at
                FROM user_memories
                WHERE user_id = ?1 AND status = 'active'
                ORDER BY priority DESC, COALESCE(last_used_at, updated_at) DESC, use_count DESC
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
                    memory_type: row.get(3)?,
                    status: row.get(4)?,
                    priority: row.get(5)?,
                    last_used_at: row.get(6)?,
                    use_count: row.get(7)?,
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
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
                "SELECT COUNT(*) FROM user_memories WHERE user_id = ?1 AND content = ?2 AND status = 'active'",
                params![user_id, content],
                |row| row.get(0),
            )
            .context("查询 user_memory 去重失败")?;
        Ok(count > 0)
    }

    /// 检索 active 记忆（排序后返回，不含裁剪逻辑）
    /// 裁剪（去重 + 预算）由上层 SessionState 负责
    pub fn search_user_memories(
        &self,
        user_id: &str,
        limit: usize,
    ) -> Result<Vec<UserMemoryRecord>> {
        let limit = i64::try_from(limit).context("memory limit 超出范围")?;
        let mut stmt = self
            .conn
            .prepare(
                r#"
                SELECT id, user_id, content, memory_type, status, priority, last_used_at, use_count, created_at, updated_at
                FROM user_memories
                WHERE user_id = ?1 AND status = 'active'
                ORDER BY priority DESC, COALESCE(last_used_at, updated_at) DESC, use_count DESC
                LIMIT ?2
                "#,
            )
            .context("准备 user_memory 检索失败")?;
        let rows = stmt
            .query_map(params![user_id, limit], |row| {
                Ok(UserMemoryRecord {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    content: row.get(2)?,
                    memory_type: row.get(3)?,
                    status: row.get(4)?,
                    priority: row.get(5)?,
                    last_used_at: row.get(6)?,
                    use_count: row.get(7)?,
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                })
            })
            .context("检索 user_memory 失败")?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("读取 user_memory 失败")?);
        }
        Ok(results)
    }

    /// 命中回写：use_count += 1（被注入 prompt 次数），last_used_at = now
    pub fn mark_memory_used(&self, memory_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn
            .execute(
                "UPDATE user_memories SET use_count = use_count + 1, last_used_at = ?1 WHERE id = ?2",
                params![now, memory_id],
            )
            .context("更新 memory 命中计数失败")?;
        Ok(())
    }

    /// 批量命中回写
    pub fn mark_memories_used(&self, memory_ids: &[String]) -> Result<()> {
        for id in memory_ids {
            self.mark_memory_used(id)?;
        }
        Ok(())
    }

    /// 软删除：将 status 设为 'suppressed'
    pub fn suppress_memory(&self, user_id: &str, memory_id: &str) -> Result<()> {
        let affected = self
            .conn
            .execute(
                "UPDATE user_memories SET status = 'suppressed' WHERE id = ?1 AND user_id = ?2 AND status = 'active'",
                params![memory_id, user_id],
            )
            .context("抑制 memory 失败")?;
        if affected == 0 {
            bail!("未找到该记忆，或无权屏蔽: {memory_id}");
        }
        Ok(())
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
    let mut url = Url::parse(input).map_err(|_| TaskStoreError::InvalidUrl(input.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(TaskStoreError::UnsupportedScheme(input.to_string()).into());
    }
    // 安全：拒绝内网/本地地址，防止 SSRF
    if let Some(host) = url.host_str() {
        if is_private_host(host) {
            return Err(TaskStoreError::PrivateNetworkUrl(input.to_string()).into());
        }
    }
    url.set_fragment(None);
    strip_tracking_query_pairs(&mut url);
    let mut normalized = url.to_string();
    if url.path() == "/" && url.query().is_none() && normalized.ends_with('/') {
        normalized.pop();
    }
    Ok(normalized)
}

/// 公开：检查 URL 是否指向私有/本地/元数据地址
pub fn is_private_url(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    parsed.host_str().is_some_and(is_private_host)
}

/// 检查 host 是否属于私有/本地/元数据地址
fn is_private_host(host: &str) -> bool {
    // IPv6 地址在 URL 中带方括号 [::1]，需去掉
    let trimmed = host.trim_start_matches('[').trim_end_matches(']');
    let lower = trimmed.to_ascii_lowercase();
    // 特殊域名
    if lower == "localhost"
        || lower == "0.0.0.0"
        || lower.ends_with(".local")
        || lower.ends_with(".internal")
        || lower.ends_with(".localhost")
    {
        return true;
    }
    // IPv6 私有/本地地址
    if lower.starts_with("::1")
        || lower.starts_with("fc")
        || lower.starts_with("fd")
        || lower.starts_with("fe80:")
        || lower.starts_with("fe::")
        || lower.starts_with("::ffff:")
    {
        return true;
    }
    // 尝试解析为 IPv4 各进制表示
    if let Some(octets) = parse_ipv4_octets(&lower) {
        return is_private_ipv4(&octets);
    }
    false
}

/// 尝试从 host 字符串解析出 4 个 u8 作为 IPv4 八位组。
/// 支持十进制、八进制(0前缀)、十六进制(0x前缀)及混合表示。
fn parse_ipv4_octets(host: &str) -> Option<[u8; 4]> {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut octets = [0u8; 4];
    for (i, part) in parts.iter().enumerate() {
        octets[i] = parse_ip_segment(part)?;
    }
    Some(octets)
}

/// 解析单个 IP 段：支持十进制、八进制(0前缀)、十六进制(0x前缀)
fn parse_ip_segment(s: &str) -> Option<u8> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u8::from_str_radix(hex, 16).ok()
    } else if s.len() > 1 && s.starts_with('0') {
        // 八进制：0777 等；但 "0" 本身是十进制 0
        u8::from_str_radix(s, 8).ok()
    } else {
        s.parse::<u8>().ok()
    }
}

/// 判断 IPv4 八位组是否属于私有/保留地址
fn is_private_ipv4(octets: &[u8; 4]) -> bool {
    match octets[0] {
        0 => true,                                      // 0.0.0.0/8
        10 => true,                                     // 10.0.0.0/8
        100 if (64..=127).contains(&octets[1]) => true, // 100.64.0.0/10 (CGN)
        127 => true,                                    // 127.0.0.0/8
        169 if octets[1] == 254 => true,                // 169.254.0.0/16 (link-local / 云元数据)
        172 if (16..=31).contains(&octets[1]) => true,  // 172.16.0.0/12
        192 => match octets[1] {
            0 if octets[2] == 0 => true, // 192.0.0.0/24
            168 => true,                 // 192.168.0.0/16
            _ => false,
        },
        198 if octets[1] == 51 && octets[2] == 100 => true, // 198.51.100.0/24 (文档)
        203 if octets[1] == 0 && octets[2] == 113 => true,  // 203.0.113.0/24 (文档)
        224..=239 => true,                                  // 组播
        240..=255 => true,                                  // 保留
        _ => false,
    }
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
        MemoryWriteState, PendingTaskRecord, PromoteReason, RecentTaskRecord, SkipReason,
        StoredSessionRecord, TaskStatusRecord, TaskStore, UserMemoryRecord, WriteDecision,
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
    fn private_network_urls_are_rejected() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let private_urls = vec![
            // 经典私有段
            "http://127.0.0.1/secret",
            "http://localhost/admin",
            "http://192.168.1.1/router",
            "http://10.0.0.1/internal",
            "http://172.16.0.1/corp",
            "http://172.31.255.255/corp",
            // 云元数据 / link-local
            "http://169.254.169.254/metadata",
            "http://169.254.0.1/whatever",
            // IPv6
            "http://[::1]/secret",
            "http://[fc00::1]/internal",
            "http://[fe80::1]/link",
            // 非十进制表示
            "http://0x7f000001/secret",
            "http://0177.0.0.1/secret",
            "http://2130706433/secret",
            // 特殊域名
            "http://myapp.localhost/",
            "http://myapp.local/",
        ];
        for url in private_urls {
            let err = store
                .record_link_submission(url)
                .expect_err(&format!("应拒绝内网 URL: {url}"));
            assert!(
                err.to_string().contains("内网"),
                "错误信息应包含'内网': {} => {}",
                url,
                err
            );
        }
        // 公网 URL 应正常通过
        store
            .record_link_submission("https://example.com/public")
            .expect("公网 URL 应正常通过");
    }

    #[test]
    fn is_private_url_detects_all_known_patterns() {
        assert!(super::is_private_url(
            "http://169.254.169.254/latest/meta-data/"
        ));
        assert!(super::is_private_url("http://100.64.0.1/cgn"));
        assert!(super::is_private_url("http://0x7f000001/ping"));
        assert!(super::is_private_url("http://0177.0.0.1/ping"));
        assert!(super::is_private_url("http://[::1]/ping"));
        assert!(super::is_private_url("http://[fc00::1]/ping"));
        // 公网不应命中
        assert!(!super::is_private_url("https://example.com/page"));
        assert!(!super::is_private_url("https://1.1.1.1/dns"));
        assert!(!super::is_private_url("https://8.8.8.8/dns"));
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
                memory_type: "explicit".to_string(),
                status: "active".to_string(),
                priority: 100,
                last_used_at: None,
                use_count: 0,
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
        assert_eq!(created.memory_type, "explicit");
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
            .add_user_memory_typed("user-a", "自动提炼主题", "auto", 60)
            .expect("写入 auto memory 失败");
        assert_eq!(created.memory_type, "auto");
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
        assert_eq!(memories[0].memory_type, "explicit"); // DEFAULT 值
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
            .add_user_memory_typed("user-a", "自动偏好", "auto", 60)
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
        assert_eq!(results[0].memory_type, "explicit");
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
            .add_user_memory_typed("user-a", "自动偏好", "auto", 60)
            .expect("写入 auto 失败");
        store
            .add_user_memory("user-a", "显式偏好")
            .expect("写入 explicit 失败");

        let results = store.search_user_memories("user-a", 15).expect("检索失败");
        assert_eq!(results.len(), 2);
        // explicit (priority=100) 应排在 auto (priority=60) 前面
        assert_eq!(results[0].memory_type, "explicit");
        assert_eq!(results[0].priority, 100);
        assert_eq!(results[1].memory_type, "auto");
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
    fn mark_memory_used_updates_count_and_time() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        let created = store
            .add_user_memory("user-a", "测试命中")
            .expect("写入失败");
        assert_eq!(created.use_count, 0);
        assert!(created.last_used_at.is_none());

        store.mark_memory_used(&created.id).expect("命中回写失败");

        let memories = store.list_user_memories("user-a", 10).expect("查询失败");
        assert_eq!(memories[0].use_count, 1);
        assert!(memories[0].last_used_at.is_some());
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

    // ——— Phase 3: Memory Write Governance 测试 ———

    #[test]
    fn govern_writes_new_explicit_memory() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        let mut ws = MemoryWriteState::default();
        let decision =
            store.govern_memory_write("user-a", "我喜欢短摘要", "explicit", 100, &mut ws);
        match decision {
            WriteDecision::Written(r) => {
                assert_eq!(r.memory_type, "explicit");
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
        let decision = store.govern_memory_write("user-a", "偏好: 短摘要", "auto", 60, &mut ws);
        match decision {
            WriteDecision::Written(r) => {
                assert_eq!(r.memory_type, "auto");
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
        let decision = store.govern_memory_write("user-a", "   ", "explicit", 100, &mut ws);
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
        let decision = store.govern_memory_write("user-a", &long, "explicit", 100, &mut ws);
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
        let _ = store.govern_memory_write("user-a", "偏好: 短摘要", "auto", 60, &mut ws1);
        let mut ws2 = MemoryWriteState::default();
        let decision = store.govern_memory_write("user-a", "偏好: 短摘要", "auto", 60, &mut ws2);
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
        let _ = store.govern_memory_write("user-a", "偏好: 短摘要", "explicit", 100, &mut ws1);
        // 再尝试 auto 同内容
        let mut ws2 = MemoryWriteState::default();
        let decision = store.govern_memory_write("user-a", "偏好: 短摘要", "auto", 60, &mut ws2);
        match decision {
            WriteDecision::Skipped {
                reason: SkipReason::AutoWouldDowngradeExplicit,
                ..
            } => {}
            _ => panic!("auto 不应降级 explicit: {:?}", decision),
        }
        // 验证原有 explicit 未被改变
        let memories = store.list_user_memories("user-a", 10).expect("查询失败");
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].memory_type, "explicit");
    }

    #[test]
    fn govern_explicit_promotes_auto() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        // 先写 auto
        let mut ws1 = MemoryWriteState::default();
        let _ = store.govern_memory_write("user-a", "偏好: 短摘要", "auto", 60, &mut ws1);
        // 再写 explicit 同内容
        let mut ws2 = MemoryWriteState::default();
        let decision =
            store.govern_memory_write("user-a", "偏好: 短摘要", "explicit", 100, &mut ws2);
        match decision {
            WriteDecision::Promoted {
                reason: PromoteReason::ExplicitPromotesAuto,
                ..
            } => {}
            _ => panic!("explicit 应提升 auto: {:?}", decision),
        }
        // 验证已提升
        let memories = store.list_user_memories("user-a", 10).expect("查询失败");
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].memory_type, "explicit");
        assert_eq!(memories[0].priority, 100);
    }

    #[test]
    fn govern_write_state_counters_accurate() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        let mut ws = MemoryWriteState::default();

        // 3 个候选
        let _ = store.govern_memory_write("user-a", "记忆一", "explicit", 100, &mut ws);
        let _ = store.govern_memory_write("user-a", "", "explicit", 100, &mut ws); // empty → skip
        let _ = store.govern_memory_write("user-a", "记忆一", "auto", 60, &mut ws); // dup → skip

        assert_eq!(ws.candidate_count, 3);
        assert_eq!(ws.written_count(), 1);
        assert_eq!(ws.skipped_count(), 2);
    }

    #[test]
    fn govern_write_state_no_cross_user_leak() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        let mut ws_a = MemoryWriteState::default();
        let _ = store.govern_memory_write("user-a", "偏好: 红", "auto", 60, &mut ws_a);
        // user-b 的相同内容不应受 user-a 影响
        let mut ws_b = MemoryWriteState::default();
        let decision = store.govern_memory_write("user-b", "偏好: 红", "auto", 60, &mut ws_b);
        match decision {
            WriteDecision::Written(_) => {}
            _ => panic!("user-b 应能写入: {:?}", decision),
        }
        // 各自独立
        assert_eq!(ws_a.written_count(), 1);
        assert_eq!(ws_b.written_count(), 1);
    }
}
