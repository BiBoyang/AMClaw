use serde_json::{json, Value};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::{
    compact_json, log_chat_error, log_chat_info, log_chat_warn, summarize_text_for_log,
    truncate_for_log, WeChatBot, WireMessage, MAX_SEEN_IDS, TRIM_SEEN_IDS_TO,
};

use crate::command_router;

pub(super) fn extract_messages(resp: &Value) -> Vec<WireMessage> {
    let array = resp
        .get("msgs")
        .and_then(Value::as_array)
        .or_else(|| resp.get("messages").and_then(Value::as_array))
        .or_else(|| resp.get("updates").and_then(Value::as_array));

    let mut out = Vec::new();
    if let Some(array) = array {
        for raw in array {
            match serde_json::from_value::<WireMessage>(raw.clone()) {
                Ok(message) => out.push(message),
                Err(err) => log_chat_warn(
                    "message_parse_skipped",
                    vec![
                        ("error_kind", json!("wire_message_parse_failed")),
                        ("detail", json!(err.to_string())),
                        ("raw", json!(truncate_for_log(&compact_json(raw), 200))),
                    ],
                ),
            }
        }
    }
    out
}

pub(super) fn collect_text(msg: &WireMessage) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !msg.text.trim().is_empty() {
        parts.push(msg.text.trim().to_string());
    }
    for item in &msg.item_list {
        if let Some(text) = item
            .text_item
            .as_ref()
            .map(|v| v.text.trim())
            .filter(|v| !v.is_empty())
        {
            parts.push(text.to_string());
        }
    }
    parts.join("")
}

pub(super) fn now_epoch_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

impl WeChatBot {
    pub(super) fn extract_message_id(&self, msg: &WireMessage) -> String {
        if let Some(id) = msg.message_id.as_ref() {
            return id.as_string();
        }
        if let Some(id) = msg.msg_id.as_ref() {
            return id.as_string();
        }
        if !msg.client_id.trim().is_empty() {
            return msg.client_id.clone();
        }
        let sender = if msg.from_user_id.trim().is_empty() {
            "unknown"
        } else {
            msg.from_user_id.trim()
        };
        let ts = msg.create_time_ms.unwrap_or_else(now_epoch_ms);
        format!("{sender}:{ts}")
    }

    pub(super) fn mark_seen(&mut self, id: &str, from_user_id: &str, text: &str) -> bool {
        let is_new = match self
            .task_store
            .record_inbound_message(id, from_user_id, text)
        {
            Ok(inserted) => inserted,
            Err(err) => {
                log_chat_error(
                    "message_dedup_store_failed",
                    vec![
                        ("user_id", json!(from_user_id)),
                        ("message_id", json!(id)),
                        ("error_kind", json!("inbound_message_store_failed")),
                        ("detail", json!(err.to_string())),
                    ],
                );
                true
            }
        };
        if !is_new {
            log_chat_info(
                "message_deduplicated",
                vec![
                    ("user_id", json!(from_user_id)),
                    ("message_id", json!(id)),
                    ("reason", json!("db_existing")),
                ],
            );
            return false;
        }

        let id = id.to_string();
        if self.seen_ids.contains(&id) {
            log_chat_info(
                "message_deduplicated",
                vec![
                    ("user_id", json!(from_user_id)),
                    ("message_id", json!(id)),
                    ("reason", json!("memory_cache")),
                ],
            );
            return false;
        }

        self.seen_ids.insert(id.clone());
        self.seen_order.push_back(id);

        if self.seen_ids.len() > MAX_SEEN_IDS {
            while self.seen_ids.len() > TRIM_SEEN_IDS_TO {
                if let Some(old_id) = self.seen_order.pop_front() {
                    self.seen_ids.remove(&old_id);
                } else {
                    break;
                }
            }
        }

        true
    }

    pub(super) fn handle_message(&mut self, msg: WireMessage) {
        let wire = msg.message.as_deref().unwrap_or(&msg);

        if let Some(message_type) = wire.message_type {
            if message_type != 1 {
                return;
            }
        }

        let from_user_id = wire.from_user_id.trim();
        if from_user_id.is_empty() {
            return;
        }

        let context_token = wire.context_token.trim();
        if !context_token.is_empty() {
            self.context_token_map
                .insert(from_user_id.to_string(), context_token.to_string());
            if let Err(err) = self
                .task_store
                .upsert_context_token(from_user_id, context_token)
            {
                log_chat_warn(
                    "context_token_persist_failed",
                    vec![
                        ("user_id", json!(from_user_id)),
                        ("error_kind", json!("context_token_persist_failed")),
                        ("detail", json!(err.to_string())),
                    ],
                );
            }
        }

        let text = collect_text(wire);
        if text.is_empty() {
            return;
        }

        let msg_id = self.extract_message_id(wire);
        log_chat_info(
            "message_received",
            vec![
                ("user_id", json!(from_user_id)),
                ("message_id", json!(msg_id)),
                ("text_chars", json!(text.chars().count())),
                ("text_preview", json!(summarize_text_for_log(&text, 120))),
            ],
        );
        if !self.mark_seen(&msg_id, from_user_id, &text) {
            return;
        }

        log_chat_info(
            "message_accepted",
            vec![
                ("user_id", json!(from_user_id)),
                ("message_id", json!(msg_id)),
                ("status", json!("accepted")),
            ],
        );
        let session_message_id = if msg_id.trim().is_empty() {
            None
        } else {
            Some(msg_id)
        };
        let intent = command_router::route_text(&text);
        match intent {
            command_router::RouteIntent::ManualContentSubmission { task_id, content } => {
                self.handle_manual_content_submission(from_user_id, &task_id, &content);
            }
            command_router::RouteIntent::ManualTasksQuery => {
                self.handle_manual_tasks_query(from_user_id);
            }
            command_router::RouteIntent::TaskRetryRequest { task_id } => {
                self.handle_task_retry(from_user_id, &task_id);
            }
            command_router::RouteIntent::RecentTasksQuery => {
                self.handle_recent_tasks_query(from_user_id);
            }
            command_router::RouteIntent::UserMemoriesQuery => {
                self.handle_user_memories_query(from_user_id);
            }
            command_router::RouteIntent::ContextDebugQuery { text, verbose } => {
                self.handle_context_debug_query(from_user_id, text.as_deref(), verbose);
            }
            command_router::RouteIntent::UserMemoryWrite { content } => {
                self.handle_user_memory_write(from_user_id, &content);
            }
            command_router::RouteIntent::UserMemoryUseful { memory_id } => {
                self.handle_user_memory_useful(from_user_id, &memory_id);
            }
            command_router::RouteIntent::UserMemorySuppress { memory_id } => {
                self.handle_user_memory_suppress(from_user_id, &memory_id);
            }
            command_router::RouteIntent::DailyReportQuery { day } => {
                self.handle_daily_report_query(from_user_id, day.as_deref());
            }
            command_router::RouteIntent::WeeklyReportQuery { week } => {
                self.handle_weekly_report_query(from_user_id, week.as_deref());
            }
            command_router::RouteIntent::TaskStatusQuery { task_id } => {
                self.handle_task_status_query(from_user_id, &task_id);
            }
            command_router::RouteIntent::LinkSubmission { urls } => {
                self.handle_link_submission(from_user_id, urls);
            }
            other => {
                self.maybe_persist_auto_memory(from_user_id, &other);
                let should_persist_session = matches!(
                    other,
                    command_router::RouteIntent::ChatContinue { .. }
                        | command_router::RouteIntent::ChatPending { .. }
                );
                let event = self.session_router.on_intent_with_message(
                    from_user_id,
                    other,
                    session_message_id,
                    Instant::now(),
                );
                if should_persist_session {
                    self.persist_session_snapshot(from_user_id);
                }
                self.handle_session_event(event);
            }
        }
    }
}
