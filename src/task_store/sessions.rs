use anyhow::{bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde_json::json;

impl super::TaskStore {
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
            super::log_task_store_info(
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
        super::log_task_store_info(
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

    pub fn list_session_states(&self) -> Result<Vec<super::StoredSessionRecord>> {
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
                Ok(super::StoredSessionRecord {
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

    // ---- UserSessionState API ----

    /// 加载用户会话结构化状态（v2，含 7-slot 完整字段）
    pub fn load_user_session_state(
        &self,
        user_id: &str,
    ) -> Result<Option<super::UserSessionStateRecord>> {
        self.conn
            .query_row(
                r#"
                SELECT
                    user_id,
                    last_user_intent,
                    current_task,
                    next_step,
                    blocked_reason,
                    goal,
                    current_subtask,
                    constraints_json,
                    confirmed_facts_json,
                    done_items_json,
                    open_questions_json,
                    updated_at
                FROM user_session_states WHERE user_id = ?1
                "#,
                [user_id],
                |row| {
                    Ok(super::UserSessionStateRecord {
                        user_id: row.get(0)?,
                        last_user_intent: row.get(1)?,
                        current_task: row.get(2)?,
                        next_step: row.get(3)?,
                        blocked_reason: row.get(4)?,
                        goal: row.get(5)?,
                        current_subtask: row.get(6)?,
                        constraints_json: row.get(7)?,
                        confirmed_facts_json: row.get(8)?,
                        done_items_json: row.get(9)?,
                        open_questions_json: row.get(10)?,
                        updated_at: row.get(11)?,
                    })
                },
            )
            .optional()
            .context("加载 user_session_state 失败")
    }

    /// 覆盖写入用户会话结构化状态（v2，含 7-slot 完整字段）
    pub fn upsert_user_session_state(
        &mut self,
        record: &super::UserSessionStateRecord,
    ) -> Result<()> {
        let user_id = record.user_id.trim();
        if user_id.is_empty() {
            bail!("user_id 不能为空");
        }
        self.conn
            .execute(
                r#"
                INSERT INTO user_session_states (
                    user_id, last_user_intent, current_task, next_step, blocked_reason,
                    goal, current_subtask, constraints_json, confirmed_facts_json,
                    done_items_json, open_questions_json, updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                ON CONFLICT(user_id) DO UPDATE SET
                    last_user_intent = excluded.last_user_intent,
                    current_task = excluded.current_task,
                    next_step = excluded.next_step,
                    blocked_reason = excluded.blocked_reason,
                    goal = excluded.goal,
                    current_subtask = excluded.current_subtask,
                    constraints_json = excluded.constraints_json,
                    confirmed_facts_json = excluded.confirmed_facts_json,
                    done_items_json = excluded.done_items_json,
                    open_questions_json = excluded.open_questions_json,
                    updated_at = excluded.updated_at
                "#,
                params![
                    user_id,
                    record.last_user_intent,
                    record.current_task,
                    record.next_step,
                    record.blocked_reason,
                    record.goal,
                    record.current_subtask,
                    record.constraints_json,
                    record.confirmed_facts_json,
                    record.done_items_json,
                    record.open_questions_json,
                    record.updated_at,
                ],
            )
            .context("写入 user_session_state 失败")?;
        Ok(())
    }

    /// 清空用户会话结构化状态
    pub fn clear_user_session_state(&mut self, user_id: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM user_session_states WHERE user_id = ?1",
                [user_id],
            )
            .context("清空 user_session_state 失败")?;
        super::log_task_store_info("session_state_cleared", vec![("user_id", json!(user_id))]);
        Ok(())
    }

    // ---- TTL Cleanup API ----

    /// 清理过期的 context_token（超过 ttl_days 天未更新）。
    pub fn cleanup_expired_context_tokens(&mut self, ttl_days: u64) -> Result<usize> {
        let cutoff = Utc::now() - chrono::Duration::days(ttl_days as i64);
        let cutoff_str = cutoff.to_rfc3339();
        let deleted = self
            .conn
            .execute(
                "DELETE FROM user_context_tokens WHERE updated_at < ?1",
                [&cutoff_str],
            )
            .context("清理过期 context_token 失败")?;
        if deleted > 0 {
            super::log_task_store_info(
                "expired_context_tokens_cleaned",
                vec![("deleted", json!(deleted)), ("ttl_days", json!(ttl_days))],
            );
        }
        Ok(deleted)
    }

    /// 清理过期的 session_state（超过 ttl_days 天未更新）。
    ///
    /// 原子性：两条 DELETE 在同一个事务内执行，避免半清理。
    pub fn cleanup_expired_user_session_states(&mut self, ttl_days: u64) -> Result<usize> {
        let cutoff = Utc::now() - chrono::Duration::days(ttl_days as i64);
        let cutoff_str = cutoff.to_rfc3339();
        let tx = self
            .conn
            .transaction()
            .context("开启 session_state 清理事务失败")?;
        let deleted_sessions = tx
            .execute(
                "DELETE FROM user_sessions WHERE updated_at < ?1",
                [&cutoff_str],
            )
            .context("清理过期 user_sessions 失败")?;
        let deleted_states = tx
            .execute(
                "DELETE FROM user_session_states WHERE updated_at < ?1",
                [&cutoff_str],
            )
            .context("清理过期 user_session_states 失败")?;
        tx.commit().context("提交 session_state 清理事务失败")?;
        let total = deleted_sessions + deleted_states;
        if total > 0 {
            super::log_task_store_info(
                "expired_session_states_cleaned",
                vec![
                    ("deleted_sessions", json!(deleted_sessions)),
                    ("deleted_states", json!(deleted_states)),
                    ("ttl_days", json!(ttl_days)),
                ],
            );
        }
        Ok(total)
    }
}
