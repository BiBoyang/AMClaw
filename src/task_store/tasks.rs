use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde_json::json;
use uuid::Uuid;

use super::{
    ArchivedTaskRecord, ClaimableTaskRecord, LinkTaskRecord, MarkTaskArchivedInput,
    PendingTaskRecord, RecentTaskRecord, TaskContentRecord, TaskStatusRecord,
};

impl super::TaskStore {
    pub fn record_link_submission(&mut self, original_url: &str) -> Result<LinkTaskRecord> {
        let normalized_url = super::normalize_url(original_url)?;
        let source_domain = super::source_domain(&normalized_url);
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
        super::log_task_store_info(
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
                ORDER BY datetime(t.updated_at) DESC, datetime(t.created_at) DESC
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

    pub fn list_archived_tasks_in_range(
        &self,
        start: &str,
        end: &str,
        limit: usize,
    ) -> Result<Vec<ArchivedTaskRecord>> {
        let limit = i64::try_from(limit).context("archived task limit 超出范围")?;
        let mut stmt = self
            .conn
            .prepare(
                r#"
                SELECT t.id, t.article_id, a.normalized_url, a.title, a.summary, t.content_source, t.page_kind, t.output_path, t.updated_at
                FROM tasks t
                JOIN articles a ON a.id = t.article_id
                WHERE t.status = 'archived'
                  AND datetime(t.updated_at) >= datetime(?1)
                  AND datetime(t.updated_at) < datetime(?2)
                ORDER BY datetime(t.updated_at) DESC, datetime(t.created_at) DESC
                LIMIT ?3
                "#,
            )
            .context("准备 archived 范围查询失败")?;
        let rows = stmt
            .query_map(params![start, end, limit], |row| {
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
            .context("查询 archived 范围任务失败")?;

        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row.context("读取 archived 范围任务记录失败")?);
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
                    worker_id = NULL,
                    processing_started_at = NULL,
                    lease_until = NULL,
                    updated_at = ?2
                WHERE id = ?1
                  AND status IN ('failed', 'awaiting_manual_input', 'archived')
                "#,
                params![task_id, now],
            )
            .context("更新任务重试状态失败")?;
        if updated == 0 {
            // 任务存在但处于非稳定态（如 processing/pending）
            if let Ok(Some(status)) = tx
                .query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |row| {
                    row.get::<_, String>(0)
                })
                .optional()
            {
                tx.rollback().context("回滚不允许重试任务事务失败")?;
                super::log_task_store_warn(
                    "task_retry_rejected",
                    vec![
                        ("task_id", json!(task_id)),
                        ("current_status", json!(status)),
                        ("reason", json!("task_not_in_terminal_state")),
                    ],
                );
                return Err(super::TaskStoreError::Validation(format!(
                    "任务当前状态为 {status}，不允许重试"
                ))
                .into());
            }
            tx.rollback().context("回滚不存在任务事务失败")?;
            super::log_task_store_warn(
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
        super::log_task_store_info(
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

    /// 按 task_id 查询任意状态的任务（返回基础字段）。
    pub fn get_task_by_id(&self, task_id: &str) -> Result<Option<ClaimableTaskRecord>> {
        self.conn
            .query_row(
                r#"
                SELECT t.id, t.article_id, a.normalized_url, a.original_url
                FROM tasks t
                JOIN articles a ON a.id = t.article_id
                WHERE t.id = ?1
                "#,
                [task_id],
                |row| {
                    Ok(ClaimableTaskRecord {
                        task_id: row.get(0)?,
                        article_id: row.get(1)?,
                        normalized_url: row.get(2)?,
                        original_url: row.get(3)?,
                    })
                },
            )
            .optional()
            .context("查询指定任务失败")
    }

    /// 列出所有可领取的任务：pending，或 lease 已过期的 processing。
    pub fn list_claimable_tasks(&self, limit: usize) -> Result<Vec<ClaimableTaskRecord>> {
        let limit = i64::try_from(limit).context("claimable task limit 超出范围")?;
        let now = Utc::now().to_rfc3339();
        let mut stmt = self
            .conn
            .prepare(
                r#"
                SELECT t.id, t.article_id, a.normalized_url, a.original_url
                FROM tasks t
                JOIN articles a ON a.id = t.article_id
                WHERE t.status = 'pending'
                   OR (t.status = 'processing' AND (t.lease_until IS NULL OR datetime(t.lease_until) < datetime(?1)))
                ORDER BY t.created_at ASC
                LIMIT ?2
                "#,
            )
            .context("准备 claimable 任务查询失败")?;
        let rows = stmt
            .query_map(params![&now, limit], |row| {
                Ok(ClaimableTaskRecord {
                    task_id: row.get(0)?,
                    article_id: row.get(1)?,
                    normalized_url: row.get(2)?,
                    original_url: row.get(3)?,
                })
            })
            .context("查询 claimable 任务失败")?;

        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row.context("读取 claimable 任务记录失败")?);
        }
        Ok(tasks)
    }

    /// 原子领取任务：将 pending 或 lease 过期的 processing 任务更新为 processing。
    /// 成功返回 true，失败（已被其他 worker 领取或不存在）返回 false。
    pub fn claim_task(&mut self, task_id: &str, worker_id: &str, lease_secs: u64) -> Result<bool> {
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let lease_until = now + chrono::Duration::seconds(lease_secs as i64);
        let lease_str = lease_until.to_rfc3339();
        let updated = self
            .conn
            .execute(
                r#"
                UPDATE tasks
                SET status = 'processing',
                    worker_id = ?2,
                    processing_started_at = ?3,
                    lease_until = ?4,
                    updated_at = ?5
                WHERE id = ?1
                  AND (status = 'pending' OR (status = 'processing' AND (lease_until IS NULL OR datetime(lease_until) < datetime(?6))))
                "#,
                params![task_id, worker_id, &now_str, &lease_str, &now_str, &now_str],
            )
            .context("claim 任务失败")?;
        if updated > 0 {
            super::log_task_store_info(
                "task_claimed",
                vec![
                    ("task_id", json!(task_id)),
                    ("worker_id", json!(worker_id)),
                    ("lease_until", json!(lease_str)),
                ],
            );
        }
        Ok(updated > 0)
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
                "UPDATE tasks SET status = 'archived', last_error = NULL, worker_id = NULL, processing_started_at = NULL, lease_until = NULL, output_path = ?2, page_kind = COALESCE(?3, page_kind), snapshot_path = ?4, content_source = COALESCE(?5, content_source), updated_at = ?6 WHERE id = ?1 AND (status = 'processing' OR status = 'awaiting_manual_input')",
                params![task_id, output_path, page_kind, snapshot_path, content_source, now.clone()],
            )
            .context("更新 archived 状态失败")?;
        if updated == 0 {
            tx.rollback().context("回滚不存在任务 archived 事务失败")?;
            super::log_task_store_warn(
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
        super::log_task_store_info(
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
                "UPDATE tasks SET status = 'awaiting_manual_input', last_error = ?2, page_kind = ?3, snapshot_path = ?4, content_source = COALESCE(?5, content_source), output_path = NULL, worker_id = NULL, processing_started_at = NULL, lease_until = NULL, updated_at = ?6 WHERE id = ?1 AND status = 'processing'",
                params![task_id, last_error, page_kind, snapshot_path, content_source, now],
            )
            .context("更新 awaiting_manual_input 状态失败")?;
        if updated > 0 {
            super::log_task_store_warn(
                "task_status_changed",
                vec![
                    ("task_id", json!(task_id)),
                    ("status", json!("awaiting_manual_input")),
                    ("page_kind", json!(page_kind)),
                    ("content_source", json!(content_source)),
                    ("snapshot_path", json!(snapshot_path)),
                    ("error_kind", json!("awaiting_manual_input")),
                    ("detail", json!(super::summarize_text_for_log(last_error, 160))),
                ],
            );
        } else {
            super::log_task_store_warn(
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
                "UPDATE tasks SET status = 'failed', last_error = ?2, page_kind = NULL, output_path = NULL, snapshot_path = NULL, content_source = NULL, worker_id = NULL, processing_started_at = NULL, lease_until = NULL, updated_at = ?3 WHERE id = ?1 AND status = 'processing'",
                params![task_id, last_error, now],
            )
            .context("更新 failed 状态失败")?;
        if updated > 0 {
            super::log_task_store_error(
                "task_status_changed",
                vec![
                    ("task_id", json!(task_id)),
                    ("status", json!("failed")),
                    ("error_kind", json!("task_failed")),
                    ("detail", json!(super::summarize_text_for_log(last_error, 160))),
                ],
            );
        } else {
            super::log_task_store_warn(
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
}
