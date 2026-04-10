use crate::command_router::RouteIntent;
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    Noop,
    FlushNow {
        user_id: String,
        merged_text: String,
        message_ids: Vec<String>,
        reason: FlushReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushReason {
    Commit,
    Timeout,
}

impl FlushReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Timeout => "timeout",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlushItem {
    pub user_id: String,
    pub merged_text: String,
    pub message_ids: Vec<String>,
    pub reason: FlushReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSnapshot {
    pub user_id: String,
    pub merged_text: String,
    pub message_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SessionRouter {
    sessions: HashMap<String, UserSession>,
    merge_timeout: Duration,
}

#[derive(Debug, Clone)]
struct UserSession {
    buffer: Vec<String>,
    message_ids: Vec<String>,
    last_update: Instant,
}

impl SessionRouter {
    pub fn new(merge_timeout: Duration) -> Self {
        Self {
            sessions: HashMap::new(),
            merge_timeout,
        }
    }

    #[cfg(test)]
    pub fn on_intent(&mut self, user_id: &str, intent: RouteIntent, now: Instant) -> SessionEvent {
        self.on_intent_with_message(user_id, intent, None, now)
    }

    pub fn on_intent_with_message(
        &mut self,
        user_id: &str,
        intent: RouteIntent,
        message_id: Option<String>,
        now: Instant,
    ) -> SessionEvent {
        if user_id.trim().is_empty() {
            return SessionEvent::Noop;
        }

        match intent {
            RouteIntent::Ignore
            | RouteIntent::LinkSubmission { .. }
            | RouteIntent::ManualContentSubmission { .. }
            | RouteIntent::ManualTasksQuery
            | RouteIntent::UserMemoryWrite { .. }
            | RouteIntent::UserMemoriesQuery
            | RouteIntent::DailyReportQuery { .. }
            | RouteIntent::TaskStatusQuery { .. }
            | RouteIntent::TaskRetryRequest { .. }
            | RouteIntent::RecentTasksQuery => SessionEvent::Noop,
            RouteIntent::ChatContinue { text } | RouteIntent::ChatPending { text } => {
                self.push_text(user_id, text, message_id, now);
                SessionEvent::Noop
            }
            RouteIntent::ChatCommit { text } => {
                self.push_text(user_id, text, message_id, now);
                let (merged_text, message_ids) = self.take_merged_text(user_id);
                SessionEvent::FlushNow {
                    user_id: user_id.to_string(),
                    merged_text,
                    message_ids,
                    reason: FlushReason::Commit,
                }
            }
        }
    }

    pub fn flush_expired(&mut self, now: Instant) -> Vec<FlushItem> {
        let expired_users: Vec<String> = self
            .sessions
            .iter()
            .filter_map(|(user_id, session)| {
                let elapsed = now
                    .checked_duration_since(session.last_update)
                    .unwrap_or_default();
                if elapsed >= self.merge_timeout {
                    Some(user_id.clone())
                } else {
                    None
                }
            })
            .collect();

        expired_users
            .into_iter()
            .map(|user_id| {
                let (merged_text, message_ids) = self.take_merged_text(&user_id);
                FlushItem {
                    user_id,
                    merged_text,
                    message_ids,
                    reason: FlushReason::Timeout,
                }
            })
            .collect()
    }

    pub fn next_flush_delay(&self, now: Instant) -> Option<Duration> {
        self.sessions
            .values()
            .map(|session| {
                let elapsed = now
                    .checked_duration_since(session.last_update)
                    .unwrap_or_default();
                self.merge_timeout.saturating_sub(elapsed)
            })
            .min()
    }

    pub fn snapshot(&self, user_id: &str) -> Option<SessionSnapshot> {
        self.sessions.get(user_id).map(|session| SessionSnapshot {
            user_id: user_id.to_string(),
            merged_text: session.buffer.join("\n"),
            message_ids: session.message_ids.clone(),
        })
    }

    pub fn restore_session(
        &mut self,
        user_id: &str,
        merged_text: &str,
        message_ids: Vec<String>,
        now: Instant,
    ) {
        if user_id.trim().is_empty() || merged_text.trim().is_empty() {
            return;
        }
        self.sessions.insert(
            user_id.to_string(),
            UserSession {
                buffer: merged_text.lines().map(|line| line.to_string()).collect(),
                message_ids,
                last_update: now,
            },
        );
    }

    fn push_text(&mut self, user_id: &str, text: String, message_id: Option<String>, now: Instant) {
        let session = self
            .sessions
            .entry(user_id.to_string())
            .or_insert_with(|| UserSession {
                buffer: Vec::new(),
                message_ids: Vec::new(),
                last_update: now,
            });
        session.buffer.push(text);
        if let Some(message_id) = message_id.filter(|value| !value.trim().is_empty()) {
            session.message_ids.push(message_id);
        }
        session.last_update = now;
    }

    fn take_merged_text(&mut self, user_id: &str) -> (String, Vec<String>) {
        self.sessions
            .remove(user_id)
            .map(|session| (session.buffer.join("\n"), session.message_ids))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::{FlushItem, FlushReason, SessionEvent, SessionRouter, SessionSnapshot};
    use crate::command_router::RouteIntent;
    use std::time::{Duration, Instant};

    #[test]
    fn pending_messages_do_not_flush_immediately() {
        let now = Instant::now();
        let mut router = SessionRouter::new(Duration::from_secs(5));

        let event = router.on_intent(
            "user-a",
            RouteIntent::ChatPending {
                text: "hello".to_string(),
            },
            now,
        );

        assert_eq!(event, SessionEvent::Noop);
    }

    #[test]
    fn commit_flushes_buffered_messages() {
        let now = Instant::now();
        let mut router = SessionRouter::new(Duration::from_secs(5));

        router.on_intent(
            "user-a",
            RouteIntent::ChatPending {
                text: "hello".to_string(),
            },
            now,
        );

        let event = router.on_intent(
            "user-a",
            RouteIntent::ChatCommit {
                text: "world".to_string(),
            },
            now + Duration::from_secs(1),
        );

        assert_eq!(
            event,
            SessionEvent::FlushNow {
                user_id: "user-a".to_string(),
                merged_text: "hello\nworld".to_string(),
                message_ids: Vec::new(),
                reason: FlushReason::Commit,
            }
        );
    }

    #[test]
    fn continue_does_not_flush_immediately() {
        let now = Instant::now();
        let mut router = SessionRouter::new(Duration::from_secs(5));

        let event = router.on_intent(
            "user-a",
            RouteIntent::ChatContinue {
                text: "hello".to_string(),
            },
            now,
        );

        assert_eq!(event, SessionEvent::Noop);
    }

    #[test]
    fn expired_session_is_flushed() {
        let now = Instant::now();
        let mut router = SessionRouter::new(Duration::from_secs(5));

        router.on_intent(
            "user-a",
            RouteIntent::ChatPending {
                text: "hello".to_string(),
            },
            now,
        );

        assert_eq!(
            router.flush_expired(now + Duration::from_secs(5)),
            vec![FlushItem {
                user_id: "user-a".to_string(),
                merged_text: "hello".to_string(),
                message_ids: Vec::new(),
                reason: FlushReason::Timeout,
            }]
        );
    }

    #[test]
    fn users_are_isolated() {
        let now = Instant::now();
        let mut router = SessionRouter::new(Duration::from_secs(5));

        router.on_intent(
            "user-a",
            RouteIntent::ChatPending {
                text: "hello".to_string(),
            },
            now,
        );
        router.on_intent(
            "user-b",
            RouteIntent::ChatPending {
                text: "world".to_string(),
            },
            now + Duration::from_secs(1),
        );

        assert_eq!(
            router.flush_expired(now + Duration::from_secs(5)),
            vec![FlushItem {
                user_id: "user-a".to_string(),
                merged_text: "hello".to_string(),
                message_ids: Vec::new(),
                reason: FlushReason::Timeout,
            }]
        );
    }

    #[test]
    fn flushed_session_is_removed() {
        let now = Instant::now();
        let mut router = SessionRouter::new(Duration::from_secs(5));

        router.on_intent(
            "user-a",
            RouteIntent::ChatPending {
                text: "hello".to_string(),
            },
            now,
        );

        let first = router.flush_expired(now + Duration::from_secs(6));
        let second = router.flush_expired(now + Duration::from_secs(7));

        assert_eq!(
            first,
            vec![FlushItem {
                user_id: "user-a".to_string(),
                merged_text: "hello".to_string(),
                message_ids: Vec::new(),
                reason: FlushReason::Timeout,
            }]
        );
        assert!(second.is_empty());
    }

    #[test]
    fn message_ids_are_preserved_when_flushing() {
        let now = Instant::now();
        let mut router = SessionRouter::new(Duration::from_secs(5));

        router.on_intent_with_message(
            "user-a",
            RouteIntent::ChatPending {
                text: "hello".to_string(),
            },
            Some("msg-1".to_string()),
            now,
        );

        let event = router.on_intent_with_message(
            "user-a",
            RouteIntent::ChatCommit {
                text: "world".to_string(),
            },
            Some("msg-2".to_string()),
            now + Duration::from_secs(1),
        );

        assert_eq!(
            event,
            SessionEvent::FlushNow {
                user_id: "user-a".to_string(),
                merged_text: "hello\nworld".to_string(),
                message_ids: vec!["msg-1".to_string(), "msg-2".to_string()],
                reason: FlushReason::Commit,
            }
        );
    }

    #[test]
    fn next_flush_delay_tracks_soonest_session() {
        let now = Instant::now();
        let mut router = SessionRouter::new(Duration::from_secs(5));

        router.on_intent(
            "user-a",
            RouteIntent::ChatPending {
                text: "hello".to_string(),
            },
            now,
        );
        router.on_intent(
            "user-b",
            RouteIntent::ChatPending {
                text: "world".to_string(),
            },
            now + Duration::from_secs(2),
        );

        assert_eq!(
            router.next_flush_delay(now + Duration::from_secs(3)),
            Some(Duration::from_secs(2))
        );
    }

    #[test]
    fn session_snapshot_reflects_buffered_state() {
        let now = Instant::now();
        let mut router = SessionRouter::new(Duration::from_secs(5));
        router.on_intent_with_message(
            "user-a",
            RouteIntent::ChatPending {
                text: "hello".to_string(),
            },
            Some("msg-1".to_string()),
            now,
        );
        router.on_intent_with_message(
            "user-a",
            RouteIntent::ChatContinue {
                text: "world".to_string(),
            },
            Some("msg-2".to_string()),
            now + Duration::from_secs(1),
        );

        assert_eq!(
            router.snapshot("user-a"),
            Some(SessionSnapshot {
                user_id: "user-a".to_string(),
                merged_text: "hello\nworld".to_string(),
                message_ids: vec!["msg-1".to_string(), "msg-2".to_string()],
            })
        );
    }

    #[test]
    fn restore_session_allows_timeout_flush() {
        let now = Instant::now();
        let mut router = SessionRouter::new(Duration::from_secs(5));
        router.restore_session(
            "user-a",
            "restored\nsession",
            vec!["msg-a".to_string(), "msg-b".to_string()],
            now,
        );

        assert_eq!(
            router.flush_expired(now + Duration::from_secs(5)),
            vec![FlushItem {
                user_id: "user-a".to_string(),
                merged_text: "restored\nsession".to_string(),
                message_ids: vec!["msg-a".to_string(), "msg-b".to_string()],
                reason: FlushReason::Timeout,
            }]
        );
    }
}
