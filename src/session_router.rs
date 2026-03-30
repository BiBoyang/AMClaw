use crate::command_router::RouteIntent;
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    Noop,
    FlushNow {
        user_id: String,
        merged_text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlushItem {
    pub user_id: String,
    pub merged_text: String,
}

#[derive(Debug, Clone)]
pub struct SessionRouter {
    sessions: HashMap<String, UserSession>,
    merge_timeout: Duration,
}

#[derive(Debug, Clone)]
struct UserSession {
    buffer: Vec<String>,
    last_update: Instant,
}

impl SessionRouter {
    pub fn new(merge_timeout: Duration) -> Self {
        Self {
            sessions: HashMap::new(),
            merge_timeout,
        }
    }

    pub fn on_intent(&mut self, user_id: &str, intent: RouteIntent, now: Instant) -> SessionEvent {
        if user_id.trim().is_empty() {
            return SessionEvent::Noop;
        }

        match intent {
            RouteIntent::Ignore
            | RouteIntent::LinkSubmission { .. }
            | RouteIntent::TaskStatusQuery { .. }
            | RouteIntent::TaskRetryRequest { .. }
            | RouteIntent::RecentTasksQuery => SessionEvent::Noop,
            RouteIntent::ChatContinue { text } | RouteIntent::ChatPending { text } => {
                self.push_text(user_id, text, now);
                SessionEvent::Noop
            }
            RouteIntent::ChatCommit { text } => {
                self.push_text(user_id, text, now);
                let merged_text = self.take_merged_text(user_id);
                SessionEvent::FlushNow {
                    user_id: user_id.to_string(),
                    merged_text,
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
            .map(|user_id| FlushItem {
                merged_text: self.take_merged_text(&user_id),
                user_id,
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

    fn push_text(&mut self, user_id: &str, text: String, now: Instant) {
        let session = self
            .sessions
            .entry(user_id.to_string())
            .or_insert_with(|| UserSession {
                buffer: Vec::new(),
                last_update: now,
            });
        session.buffer.push(text);
        session.last_update = now;
    }

    fn take_merged_text(&mut self, user_id: &str) -> String {
        self.sessions
            .remove(user_id)
            .map(|session| session.buffer.join("\n"))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::{FlushItem, SessionEvent, SessionRouter};
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
            }]
        );
        assert!(second.is_empty());
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
}
