use crate::session_router::SessionEvent;
use anyhow::Result;
use chrono::Utc;
use serde_json::json;
use std::time::Instant;
use super::{log_chat_info, log_chat_warn};

impl super::WeChatBot {
    pub(super) fn persist_session_snapshot(&mut self, user_id: &str) {
        let Some(snapshot) = self.session_router.snapshot(user_id) else {
            return;
        };
        if let Err(err) = self.task_store.upsert_session_state(
            &snapshot.user_id,
            &snapshot.merged_text,
            &snapshot.message_ids,
        ) {
            log_chat_warn(
                "session_persist_failed",
                vec![
                    ("user_id", json!(user_id)),
                    ("error_kind", json!("session_persist_failed")),
                    ("detail", json!(err.to_string())),
                ],
            );
        }
    }

    pub(super) fn restore_persisted_sessions(&mut self) -> Result<()> {
        let sessions = self.task_store.list_session_states()?;
        let now = Instant::now();
        for session in sessions {
            self.session_router.restore_session(
                &session.user_id,
                &session.merged_text,
                session.message_ids,
                now,
            );
        }
        Ok(())
    }

    pub(super) fn handle_session_event(&mut self, event: SessionEvent) {
        if let SessionEvent::FlushNow {
            user_id,
            merged_text,
            message_ids,
            reason,
        } = event
        {
            let _ = self.task_store.delete_session_state(&user_id);
            self.update_session_state_intent(&user_id, &merged_text);
            self.send_generated_reply(&user_id, &merged_text, &message_ids, reason);
        }
    }

    pub(super) fn flush_expired_sessions(&mut self) {
        for item in self.session_router.flush_expired(Instant::now()) {
            let _ = self.task_store.delete_session_state(&item.user_id);
            self.update_session_state_intent(&item.user_id, &item.merged_text);
            self.send_generated_reply(
                &item.user_id,
                &item.merged_text,
                &item.message_ids,
                item.reason,
            );
        }
    }

    /// C2: 在 session flush 时更新 session state（v2，保守更新策略）
    ///
    /// 更新规则（宁缺毋滥）：
    /// - goal: 来自最近用户意图（截断到 120 字符）
    /// - current_subtask: 来自当前意图或保留已有值
    /// - next_step: 保留已有值（由 agent 运行时推导更新）
    /// - 数组槽位（constraints/confirmed_facts/done_items/open_questions）：
    ///   只在有明确来源时更新，不强行猜测
    pub(super) fn update_session_state_intent(&mut self, user_id: &str, merged_text: &str) {
        let now = Utc::now().to_rfc3339();
        let intent_preview = if merged_text.chars().count() > 120 {
            let truncated: String = merged_text.chars().take(120).collect();
            format!("{}...", truncated)
        } else {
            merged_text.to_string()
        };

        let mut record = match self.task_store.load_user_session_state(user_id) {
            Ok(Some(existing)) => existing,
            Ok(None) => crate::task_store::UserSessionStateRecord {
                user_id: user_id.to_string(),
                last_user_intent: None,
                current_task: None,
                next_step: None,
                blocked_reason: None,
                goal: None,
                current_subtask: None,
                constraints_json: None,
                confirmed_facts_json: None,
                done_items_json: None,
                open_questions_json: None,
                updated_at: now.clone(),
            },
            Err(err) => {
                log_chat_warn(
                    "session_state_intent_update_failed",
                    vec![
                        ("user_id", json!(user_id)),
                        ("error_kind", json!("session_state_load_failed")),
                        ("detail", json!(err.to_string())),
                    ],
                );
                return;
            }
        };

        // 保守更新：只更新有明确来源的字段
        record.last_user_intent = Some(intent_preview.clone());
        record.goal = Some(format!("响应当前用户请求：{}", intent_preview));
        // current_subtask: 若已有则保留，否则设为用户意图
        if record.current_subtask.is_none() {
            record.current_subtask = Some(intent_preview.clone());
        }
        record.updated_at = now;

        if let Err(err) = self.task_store.upsert_user_session_state(&record) {
            log_chat_warn(
                "session_state_intent_update_failed",
                vec![
                    ("user_id", json!(user_id)),
                    ("error_kind", json!("session_state_upsert_failed")),
                    ("detail", json!(err.to_string())),
                ],
            );
        } else {
            log_chat_info(
                "session_state_upserted",
                vec![
                    ("user_id", json!(user_id)),
                    ("intent_preview", json!(intent_preview)),
                    ("v2_slots_populated", json!(record.populated_slot_count())),
                ],
            );
        }
    }
}
