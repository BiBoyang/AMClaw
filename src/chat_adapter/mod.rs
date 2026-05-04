mod command_handlers;
mod delivery;
mod helpers;
mod ilink_client;
mod ingest;
mod session_flow;
mod types;
use self::helpers::{
    assert_ok, compact_json, first_non_empty, get_i64, get_str, is_agent_command,
    is_llm_auth_error, is_poll_timeout_error, log_chat_error, log_chat_info, log_chat_warn,
    sanitize_report_markdown_for_wechat, summarize_text_for_log, truncate_for_log, value_to_string,
};

use self::ilink_client::ILinkClient;
use self::types::WireMessage;

use crate::agent_core::AgentCore;
use crate::config::{AppConfig, ResolvedBrowserConfig};
use crate::pipeline::Pipeline;
use crate::reporter::DailyReporter;
use crate::scheduler::{DailyReportSchedule, WeeklyReportSchedule};
use crate::session_router::SessionRouter;
use crate::task_store::TaskStore;
use anyhow::{Context, Result};
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;
const BASE_URL: &str = "https://ilinkai.weixin.qq.com";
const MAX_SEEN_IDS: usize = 1000;
const TRIM_SEEN_IDS_TO: usize = 500;
const DEFAULT_GET_UPDATES_TIMEOUT: Duration = Duration::from_secs(70);
const MIN_GET_UPDATES_TIMEOUT: Duration = Duration::from_millis(200);

/// 微信单条消息最大字符数（超过则自动分段）
const WECHAT_REPLY_CHUNK_MAX_CHARS: usize = 1200;
/// 触发“处理中”回执的最小输入长度
const PROCESSING_ACK_MIN_INPUT_CHARS: usize = 180;
/// 长任务处理中回执文案
const PROCESSING_ACK_TEXT: &str = "收到，处理中，稍后给你完整回复。";

pub fn run(
    config: AppConfig,
    browser: Option<ResolvedBrowserConfig>,
    running: Arc<AtomicBool>,
) -> Result<()> {
    let mut bot = WeChatBot::new(config, browser, running)?;
    bot.start()
}

struct WeChatBot {
    agent_core: AgentCore,
    client: ILinkClient,
    pipeline: Pipeline,
    reporter: DailyReporter,
    task_store: TaskStore,
    task_executor: crate::task_executor::TaskExecutor,
    context_token_map: HashMap<String, String>,
    context_token_ttl_days: u64,
    session_state_ttl_days: u64,
    daily_report_schedule: Option<DailyReportSchedule>,
    last_daily_report_push_day: Option<String>,
    weekly_report_schedule: Option<WeeklyReportSchedule>,
    last_weekly_report_push_week: Option<String>,
    cursor: String,
    seen_ids: HashSet<String>,
    seen_order: VecDeque<String>,
    session_router: SessionRouter,
    running: Arc<AtomicBool>,
}

impl WeChatBot {
    fn new(
        config: AppConfig,
        browser: Option<ResolvedBrowserConfig>,
        running: Arc<AtomicBool>,
    ) -> Result<Self> {
        let workspace_root = std::env::current_dir().context("获取工作目录失败")?;
        let db_path = config.db_path();
        let root_dir = config.resolved_root_dir();
        let pipeline = Pipeline::new(
            &root_dir,
            browser,
            crate::mode_policy::AgentMode::from_config(&config.agent.mode),
        )?;
        let task_executor =
            crate::task_executor::TaskExecutor::start(pipeline.clone(), db_path.clone());
        let mut bot = Self {
            agent_core: AgentCore::with_task_store_db_path_and_agent_config(
                workspace_root,
                db_path.clone(),
                &config.agent,
            )?,
            client: ILinkClient::new(config.wechat.channel_version.clone())?,
            pipeline,
            reporter: DailyReporter::from_config(&config)?,
            task_store: TaskStore::open(&db_path)?,
            task_executor,
            context_token_map: HashMap::new(),
            context_token_ttl_days: config.agent.context_token_ttl_days,
            session_state_ttl_days: config.agent.session_state_ttl_days,
            daily_report_schedule: DailyReportSchedule::from_config(&config)?,
            last_daily_report_push_day: None,
            weekly_report_schedule: WeeklyReportSchedule::from_config(&config)?,
            last_weekly_report_push_week: None,
            cursor: String::new(),
            seen_ids: HashSet::new(),
            seen_order: VecDeque::new(),
            session_router: SessionRouter::new(config.session_merge_timeout()),
            running,
        };
        bot.restore_persisted_sessions()?;
        bot.run_ttl_cleanup();
        Ok(bot)
    }

    fn start(&mut self) -> Result<()> {
        log_chat_info("bot_starting", vec![]);
        self.client.login(&self.running)?;
        log_chat_info("bot_polling_started", vec![]);
        self.poll_loop();
        Ok(())
    }

    fn run_ttl_cleanup(&mut self) {
        if let Err(err) = self
            .task_store
            .cleanup_expired_context_tokens(self.context_token_ttl_days)
        {
            log_chat_error(
                "ttl_cleanup_failed",
                vec![
                    ("target", json!("context_tokens")),
                    ("detail", json!(err.to_string())),
                ],
            );
        }
        if let Err(err) = self
            .task_store
            .cleanup_expired_user_session_states(self.session_state_ttl_days)
        {
            log_chat_error(
                "ttl_cleanup_failed",
                vec![
                    ("target", json!("session_states")),
                    ("detail", json!(err.to_string())),
                ],
            );
        }
    }

    fn poll_loop(&mut self) {
        while self.running.load(Ordering::Relaxed) {
            self.resend_pending_chunks();
            self.flush_expired_sessions();
            self.process_pending_tasks();
            self.process_scheduled_daily_report_push();
            self.process_scheduled_weekly_report_push();

            let poll_timeout = self.next_poll_timeout();
            match self.client.get_updates(&self.cursor, poll_timeout) {
                Ok(result) => {
                    if !result.cursor.is_empty() && result.cursor != self.cursor {
                        self.cursor = result.cursor;
                    }
                    for msg in result.messages {
                        self.handle_message(msg);
                    }
                    self.process_pending_tasks();
                }
                Err(err) => {
                    if is_poll_timeout_error(&err) {
                        continue;
                    }
                    log_chat_error(
                        "poll_failed",
                        vec![
                            ("error_kind", json!("get_updates_failed")),
                            ("detail", json!(err.to_string())),
                        ],
                    );
                    log_chat_info("poll_retry_scheduled", vec![("retry_after_secs", json!(5))]);
                    sleep(Duration::from_secs(5));
                }
            }
        }
    }
}

#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
fn build_chat_log_payload(level: &str, event: &str, fields: Vec<(&str, Value)>) -> Value {
    crate::logging::build_structured_log_payload(level, event, fields)
}

#[cfg(test)]
mod tests {
    use super::{ILinkClient, WeChatBot, WireMessage};
    use crate::agent_core::AgentCore;
    use crate::config::ResolvedBrowserConfig;
    use crate::pipeline::Pipeline;
    use crate::reporter::DailyReporter;
    use crate::session_router::{FlushReason, SessionRouter, SessionSnapshot};
    use crate::task_store::{MarkTaskArchivedInput, TaskStore};
    use rusqlite::Connection;
    use serde_json::json;
    use serde_json::Value;
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::time::Duration;
    use uuid::Uuid;

    fn temp_dir() -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("amclaw_chat_adapter_test_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("创建测试目录失败");
        root
    }

    fn temp_db_path() -> std::path::PathBuf {
        temp_dir().join("amclaw.db")
    }

    fn build_test_bot_with_pipeline(
        db_path: &Path,
        workspace_root: PathBuf,
        pipeline: Pipeline,
    ) -> WeChatBot {
        let reporter_root = temp_dir();
        let timezone = "Asia/Shanghai".parse().expect("解析测试 timezone 失败");
        let mut bot = WeChatBot {
            agent_core: AgentCore::with_task_store_db_path(workspace_root, db_path.to_path_buf())
                .expect("初始化 agent 失败"),
            client: ILinkClient::new("1.0.0").expect("初始化 iLink 客户端失败"),
            pipeline: pipeline.clone(),
            reporter: DailyReporter::new(reporter_root, db_path.to_path_buf(), timezone),
            task_store: TaskStore::open(db_path).expect("初始化 task store 失败"),
            task_executor: crate::task_executor::TaskExecutor::start(
                pipeline,
                db_path.to_path_buf(),
            ),
            context_token_map: HashMap::new(),
            context_token_ttl_days: 30,
            session_state_ttl_days: 30,
            daily_report_schedule: None,
            last_daily_report_push_day: None,
            weekly_report_schedule: None,
            last_weekly_report_push_week: None,
            cursor: String::new(),
            seen_ids: HashSet::new(),
            seen_order: VecDeque::new(),
            session_router: SessionRouter::new(Duration::from_secs(5)),
            running: Arc::new(AtomicBool::new(true)),
        };
        bot.restore_persisted_sessions()
            .expect("恢复测试 session 失败");
        bot
    }

    fn build_test_bot(db_path: &Path, workspace_root: PathBuf) -> WeChatBot {
        let pipeline = Pipeline::new(
            temp_dir(),
            None::<ResolvedBrowserConfig>,
            crate::mode_policy::AgentMode::Restricted,
        )
        .expect("初始化 pipeline 失败");
        build_test_bot_with_pipeline(db_path, workspace_root, pipeline)
    }

    fn test_bot(db_path: &Path) -> WeChatBot {
        let workspace_root = temp_dir();
        build_test_bot(db_path, workspace_root)
    }

    fn message_row(
        db_path: &std::path::Path,
        message_id: &str,
    ) -> Option<(String, String, String)> {
        let conn = Connection::open(db_path).expect("打开数据库失败");
        conn.query_row(
            "SELECT message_id, from_user_id, text FROM inbound_messages WHERE message_id = ?1",
            [message_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .ok()
    }

    fn message_count(db_path: &std::path::Path, message_id: &str) -> i64 {
        let conn = Connection::open(db_path).expect("打开数据库失败");
        conn.query_row(
            "SELECT COUNT(*) FROM inbound_messages WHERE message_id = ?1",
            [message_id],
            |row| row.get(0),
        )
        .expect("查询消息数量失败")
    }

    fn article_count(db_path: &std::path::Path) -> i64 {
        let conn = Connection::open(db_path).expect("打开数据库失败");
        conn.query_row("SELECT COUNT(*) FROM articles", [], |row| row.get(0))
            .expect("查询文章数量失败")
    }

    fn task_count(db_path: &std::path::Path) -> i64 {
        let conn = Connection::open(db_path).expect("打开数据库失败");
        conn.query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
            .expect("查询任务数量失败")
    }

    fn first_task_id(db_path: &std::path::Path) -> String {
        let conn = Connection::open(db_path).expect("打开数据库失败");
        conn.query_row("SELECT id FROM tasks LIMIT 1", [], |row| row.get(0))
            .expect("应存在任务")
    }

    fn task_row(db_path: &std::path::Path, task_id: &str) -> Option<(String, i64, Option<String>)> {
        let conn = Connection::open(db_path).expect("打开数据库失败");
        conn.query_row(
            "SELECT status, retry_count, last_error FROM tasks WHERE id = ?1",
            [task_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .ok()
    }

    type TaskStatusDetails = (String, Option<String>, Option<String>, Option<String>);

    fn task_status_details(db_path: &std::path::Path, task_id: &str) -> Option<TaskStatusDetails> {
        let conn = Connection::open(db_path).expect("打开数据库失败");
        conn.query_row(
            "SELECT status, page_kind, output_path, last_error FROM tasks WHERE id = ?1",
            [task_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .ok()
    }

    fn first_article_and_task(db_path: &std::path::Path) -> Option<(String, String, String)> {
        let conn = Connection::open(db_path).expect("打开数据库失败");
        conn.query_row(
            r#"
            SELECT a.normalized_url, a.original_url, t.status
            FROM articles a
            JOIN tasks t ON t.article_id = a.id
            ORDER BY a.created_at ASC
            LIMIT 1
            "#,
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .ok()
    }

    #[test]
    fn handle_message_persists_inbound_text() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            context_token: "ctx-1".to_string(),
            text: "hello world".to_string(),
            message_id: Some(super::types::FlexibleId::Str("msg-1".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        assert_eq!(
            message_row(&db_path, "msg-1"),
            Some((
                "msg-1".to_string(),
                "user-a".to_string(),
                "hello world".to_string(),
            ))
        );
    }

    #[test]
    fn duplicate_message_is_ignored_by_handle_message() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        let message = WireMessage {
            from_user_id: "user-a".to_string(),
            context_token: "ctx-1".to_string(),
            text: "hello world".to_string(),
            message_id: Some(super::types::FlexibleId::Str("msg-2".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        };

        bot.handle_message(message.clone());
        bot.handle_message(message);

        assert_eq!(message_count(&db_path, "msg-2"), 1);
    }

    #[test]
    fn empty_text_is_not_persisted() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            context_token: "ctx-1".to_string(),
            text: "   ".to_string(),
            message_id: Some(super::types::FlexibleId::Str("msg-3".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        assert_eq!(message_count(&db_path, "msg-3"), 0);
    }

    #[test]
    fn link_message_creates_article_and_task() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "收藏 https://example.com/path?q=1".to_string(),
            message_id: Some(super::types::FlexibleId::Str("msg-4".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        assert_eq!(article_count(&db_path), 1);
        assert_eq!(task_count(&db_path), 1);
        assert_eq!(
            first_article_and_task(&db_path),
            Some((
                "https://example.com/path?q=1".to_string(),
                "https://example.com/path?q=1".to_string(),
                "pending".to_string(),
            ))
        );
    }

    #[test]
    fn duplicate_link_messages_do_not_create_second_article_or_task() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "https://example.com".to_string(),
            message_id: Some(super::types::FlexibleId::Str("msg-5".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });
        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "https://example.com/".to_string(),
            message_id: Some(super::types::FlexibleId::Str("msg-6".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        assert_eq!(article_count(&db_path), 1);
        assert_eq!(task_count(&db_path), 1);
    }

    #[test]
    fn status_query_does_not_create_article_or_task() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "状态 task-unknown".to_string(),
            message_id: Some(super::types::FlexibleId::Str("msg-7".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        assert_eq!(article_count(&db_path), 0);
        assert_eq!(task_count(&db_path), 0);
        assert_eq!(message_count(&db_path, "msg-7"), 1);
    }

    #[test]
    fn status_query_after_link_keeps_single_task() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "https://example.com/status-check".to_string(),
            message_id: Some(super::types::FlexibleId::Str("msg-8".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let conn = Connection::open(&db_path).expect("打开数据库失败");
        let task_id: String = conn
            .query_row("SELECT id FROM tasks LIMIT 1", [], |row| row.get(0))
            .expect("应存在任务");
        drop(conn);

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: format!("状态 {task_id}"),
            message_id: Some(super::types::FlexibleId::Str("msg-9".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        assert_eq!(article_count(&db_path), 1);
        assert_eq!(task_count(&db_path), 1);
        assert_eq!(message_count(&db_path, "msg-9"), 1);
    }

    #[test]
    fn recent_tasks_query_does_not_create_new_tasks() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "https://example.com/recent".to_string(),
            message_id: Some(super::types::FlexibleId::Str("msg-10".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });
        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "最近任务".to_string(),
            message_id: Some(super::types::FlexibleId::Str("msg-11".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        assert_eq!(article_count(&db_path), 1);
        assert_eq!(task_count(&db_path), 1);
        assert_eq!(message_count(&db_path, "msg-11"), 1);
    }

    #[test]
    fn retry_command_processes_task_immediately() {
        let db_path = temp_db_path();
        let fixture_url = "https://example.com/retry-chat-fixture";
        let fixture_html =
            "<html><head><title>Fixture Page</title></head><body>fixture content</body></html>";
        let pipeline = Pipeline::new(
            temp_dir(),
            None::<ResolvedBrowserConfig>,
            crate::mode_policy::AgentMode::Restricted,
        )
        .expect("初始化 fixture pipeline 失败")
        .with_http_fixture(fixture_url, fixture_html);
        let mut bot = build_test_bot_with_pipeline(&db_path, temp_dir(), pipeline);

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: fixture_url.to_string(),
            message_id: Some(super::types::FlexibleId::Str("msg-12".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let task_id = first_task_id(&db_path);
        let conn = Connection::open(&db_path).expect("打开数据库失败");
        conn.execute(
            "UPDATE tasks SET status = 'failed', retry_count = 1, last_error = 'boom' WHERE id = ?1",
            [task_id.as_str()],
        )
        .expect("准备失败任务失败");
        drop(conn);

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: format!("重试 {task_id}"),
            message_id: Some(super::types::FlexibleId::Str("msg-13".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });
        assert!(
            bot.task_executor.flush(),
            "task_executor flush 超时，任务未在预期时间内 drain 完"
        );

        let row = task_row(&db_path, &task_id).expect("应存在任务");
        assert_eq!(row.1, 2);
        assert_ne!(row.0, "pending");
        assert_eq!(message_count(&db_path, "msg-13"), 1);
    }

    #[test]
    fn pending_link_task_is_consumed() {
        let db_path = temp_db_path();
        let fixture_url = "https://example.com/archive-me";
        let fixture_html =
            "<html><head><title>Fixture Page</title></head><body>fixture content</body></html>";
        let pipeline = Pipeline::new(
            temp_dir(),
            None::<ResolvedBrowserConfig>,
            crate::mode_policy::AgentMode::Restricted,
        )
        .expect("初始化 fixture pipeline 失败")
        .with_http_fixture(fixture_url, fixture_html);
        let mut bot = build_test_bot_with_pipeline(&db_path, temp_dir(), pipeline);

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: fixture_url.to_string(),
            message_id: Some(super::types::FlexibleId::Str("msg-14".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let task_id = first_task_id(&db_path);
        bot.process_pending_tasks();
        assert!(
            bot.task_executor.flush(),
            "task_executor flush 超时，任务未在预期时间内 drain 完"
        );

        let status = task_row(&db_path, &task_id).map(|row| row.0);
        assert!(matches!(
            status.as_deref(),
            Some("archived") | Some("failed")
        ));
    }

    #[test]
    fn task_status_can_reflect_awaiting_manual_input() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "https://mp.weixin.qq.com/s/YUvXg9i31QuQN6t-zRTe8g".to_string(),
            message_id: Some(super::types::FlexibleId::Str("msg-15".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let task_id = first_task_id(&db_path);
        bot.task_store
            .claim_task(&task_id, "test-worker", 300)
            .expect("claim 失败");
        bot.task_store
            .mark_task_awaiting_manual_input(
                &task_id,
                "微信公众号页面需要验证码验证",
                "wechat_captcha",
                None,
                Some("browser_capture"),
            )
            .expect("更新 awaiting_manual_input 状态失败");

        let details = task_status_details(&db_path, &task_id).expect("应存在任务状态");
        assert_eq!(details.0, "awaiting_manual_input".to_string());
        assert_eq!(details.1, Some("wechat_captcha".to_string()));
        assert!(details.3.is_some());
    }

    #[test]
    fn manual_content_submission_archives_task() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "https://mp.weixin.qq.com/s/YUvXg9i31QuQN6t-zRTe8g".to_string(),
            message_id: Some(super::types::FlexibleId::Str("msg-16".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let task_id = first_task_id(&db_path);
        bot.task_store
            .claim_task(&task_id, "test-worker", 300)
            .expect("claim 失败");
        bot.task_store
            .mark_task_awaiting_manual_input(
                &task_id,
                "微信公众号页面需要验证码验证",
                "wechat_captcha",
                None,
                Some("browser_capture"),
            )
            .expect("更新 awaiting_manual_input 状态失败");

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: format!("补正文 {task_id} :: 这是人工补录的公众号正文"),
            message_id: Some(super::types::FlexibleId::Str("msg-17".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let details = task_status_details(&db_path, &task_id).expect("应存在任务状态");
        assert_eq!(details.0, "archived".to_string());
        assert_eq!(details.1, Some("manual_input".to_string()));
        assert!(details.2.is_some());
    }

    #[test]
    fn manual_archive_rejected_reply_includes_current_status() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "https://example.com/not-manual".to_string(),
            message_id: Some(super::types::FlexibleId::Str(
                "msg-manual-reject".to_string(),
            )),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let task_id = first_task_id(&db_path);
        let reply =
            super::command_handlers::build_manual_archive_rejected_reply(&bot.task_store, &task_id);
        assert!(
            reply.contains("任务当前状态为 pending"),
            "应包含当前状态提示，实际: {reply}"
        );
        assert!(
            reply.contains("不允许人工归档"),
            "应提示状态不允许人工归档，实际: {reply}"
        );
    }

    #[test]
    fn manual_tasks_query_does_not_create_new_tasks() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "https://mp.weixin.qq.com/s/YUvXg9i31QuQN6t-zRTe8g".to_string(),
            message_id: Some(super::types::FlexibleId::Str("msg-18".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let task_id = first_task_id(&db_path);
        bot.task_store
            .mark_task_awaiting_manual_input(
                &task_id,
                "微信公众号页面需要验证码验证",
                "wechat_captcha",
                None,
                Some("browser_capture"),
            )
            .expect("更新 awaiting_manual_input 状态失败");

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "待补录任务".to_string(),
            message_id: Some(super::types::FlexibleId::Str("msg-19".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        assert_eq!(task_count(&db_path), 1);
        assert_eq!(message_count(&db_path, "msg-19"), 1);
    }

    #[test]
    fn send_generated_reply_writes_trace_with_chat_context() {
        let db_path = temp_db_path();
        let workspace_root = temp_dir();
        let mut bot = build_test_bot(&db_path, workspace_root.clone());

        bot.send_generated_reply(
            "user-a",
            "读文件 missing.txt",
            &[String::from("msg-trace-1"), String::from("msg-trace-2")],
            FlushReason::Commit,
        );

        let trace_root = workspace_root.join("data").join("agent_traces");
        let day_dir = std::fs::read_dir(&trace_root)
            .expect("应存在 trace 根目录")
            .next()
            .expect("应存在日期目录")
            .expect("读取日期目录失败")
            .path();
        let trace_path = std::fs::read_dir(day_dir)
            .expect("应存在 trace 文件")
            .filter_map(|entry| entry.ok().map(|value| value.path()))
            .find(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
            .expect("应存在至少一个 json trace 文件");
        let payload: Value = serde_json::from_str(
            &std::fs::read_to_string(trace_path).expect("读取 trace 文件失败"),
        )
        .expect("trace JSON 应合法");

        assert_eq!(payload["source_type"], "wechat_chat");
        assert_eq!(payload["trigger_type"], "commit");
        assert_eq!(payload["user_id"], "user-a");
        assert_eq!(payload["message_count"], 2);
        assert_eq!(payload["session_text"], "读文件 missing.txt");
        assert_eq!(payload["context_token_present"], false);
        assert_eq!(payload["message_ids"][0], "msg-trace-1");
        assert_eq!(payload["message_ids"][1], "msg-trace-2");
    }

    #[test]
    fn daily_report_query_builds_reply() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);
        let created = bot
            .task_store
            .record_link_submission("https://example.com/daily-query")
            .expect("写入任务失败");
        bot.task_store
            .claim_task(&created.task_id, "test-worker", 300)
            .expect("claim 失败");
        bot.task_store
            .mark_task_archived(
                &created.task_id,
                MarkTaskArchivedInput {
                    output_path: "/tmp/daily-query.md",
                    title: Some("Daily Query Title"),
                    page_kind: Some("article"),
                    snapshot_path: None,
                    content_source: Some("http"),
                    summary: None,
                },
            )
            .expect("更新 archived 状态失败");

        let day = bot.reporter.current_day();
        let reply = bot.build_daily_report_query_reply(Some(&day));

        assert!(reply.contains("Daily Report") || reply.contains("日报"));
        assert!(reply.contains("archived_count: 1"));
        // 不应暴露服务器路径
        assert!(!reply.contains("markdown_path:"));
        assert!(!reply.contains("output_path:"));
    }

    #[test]
    fn weekly_report_query_builds_reply() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);
        let created = bot
            .task_store
            .record_link_submission("https://example.com/weekly-query")
            .expect("写入任务失败");
        bot.task_store
            .claim_task(&created.task_id, "test-worker", 300)
            .expect("claim 失败");
        bot.task_store
            .mark_task_archived(
                &created.task_id,
                MarkTaskArchivedInput {
                    output_path: "/tmp/weekly-query.md",
                    title: Some("Weekly Query Title"),
                    page_kind: Some("article"),
                    snapshot_path: None,
                    content_source: Some("http"),
                    summary: Some("weekly summary"),
                },
            )
            .expect("更新 archived 状态失败");

        let week = bot.reporter.current_week();
        let reply = bot.build_weekly_report_query_reply(Some(&week));

        assert!(reply.contains("Weekly Report") || reply.contains("周报"));
        assert!(reply.contains("archived_count: 1"));
        assert!(!reply.contains("markdown_path:"));
        assert!(!reply.contains("output_path:"));
    }

    #[test]
    fn pending_chat_session_is_persisted() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            context_token: "ctx-1".to_string(),
            text: "先记一条会话".to_string(),
            message_id: Some(super::types::FlexibleId::Str("msg-session-1".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let sessions = bot
            .task_store
            .list_session_states()
            .expect("查询 session_state 失败");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].user_id, "user-a");
        assert_eq!(sessions[0].merged_text, "先记一条会话");
        assert_eq!(sessions[0].message_ids, vec!["msg-session-1".to_string()]);
    }

    #[test]
    fn context_debug_query_keeps_pending_session_intact() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "先记一条会话".to_string(),
            message_id: Some(super::types::FlexibleId::Str("msg-context-1".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "/context 再补一条".to_string(),
            message_id: Some(super::types::FlexibleId::Str("msg-context-2".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        assert_eq!(article_count(&db_path), 0);
        assert_eq!(task_count(&db_path), 0);
        assert_eq!(
            bot.session_router.snapshot("user-a"),
            Some(SessionSnapshot {
                user_id: "user-a".to_string(),
                merged_text: "先记一条会话".to_string(),
                message_ids: vec!["msg-context-1".to_string()],
            })
        );
    }

    #[test]
    fn persisted_session_is_restored_on_bot_startup() {
        let db_path = temp_db_path();
        {
            let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
            store
                .upsert_session_state("user-a", "恢复前的会话", &["msg-restore-1".to_string()])
                .expect("写入 session_state 失败");
        }

        let bot = test_bot(&db_path);
        assert_eq!(
            bot.session_router.snapshot("user-a"),
            Some(SessionSnapshot {
                user_id: "user-a".to_string(),
                merged_text: "恢复前的会话".to_string(),
                message_ids: vec!["msg-restore-1".to_string()],
            })
        );
    }

    #[test]
    fn user_memory_commands_write_and_read_back() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);
        bot.context_token_map
            .insert("user-a".to_string(), "ctx-1".to_string());

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "记住 我喜欢短摘要".to_string(),
            message_id: Some(super::types::FlexibleId::Str("msg-memory-1".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let memories = bot
            .task_store
            .list_user_memories("user-a", 10)
            .expect("查询 user_memory 失败");
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].content, "我喜欢短摘要");

        let reply = super::command_handlers::build_user_memories_reply(&memories);
        assert!(reply.contains("我的记忆"));
        assert!(reply.contains("我喜欢短摘要"));
        assert!(reply.contains("id:"));
        assert!(reply.contains(&memories[0].id));
    }

    #[test]
    fn user_memory_suppress_command_removes_from_list() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);
        bot.context_token_map
            .insert("user-a".to_string(), "ctx-1".to_string());

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "记住 将被遗忘的记忆".to_string(),
            message_id: Some(super::types::FlexibleId::Str("msg-suppress-1".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let memories = bot
            .task_store
            .list_user_memories("user-a", 10)
            .expect("查询失败");
        assert_eq!(memories.len(), 1);
        let memory_id = memories[0].id.clone();

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: format!("忘记 {memory_id}"),
            message_id: Some(super::types::FlexibleId::Str("msg-suppress-2".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let memories_after = bot
            .task_store
            .list_user_memories("user-a", 10)
            .expect("查询失败");
        assert!(memories_after.is_empty());
    }

    #[test]
    fn user_memory_suppress_command_cannot_remove_other_users_memory() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);
        bot.context_token_map
            .insert("user-a".to_string(), "ctx-1".to_string());
        bot.context_token_map
            .insert("user-b".to_string(), "ctx-2".to_string());

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "记住 仅 user-a 可屏蔽".to_string(),
            message_id: Some(super::types::FlexibleId::Str(
                "msg-suppress-cross-1".to_string(),
            )),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let memories = bot
            .task_store
            .list_user_memories("user-a", 10)
            .expect("查询失败");
        assert_eq!(memories.len(), 1);
        let memory_id = memories[0].id.clone();

        bot.handle_message(WireMessage {
            from_user_id: "user-b".to_string(),
            text: format!("忘记 {memory_id}"),
            message_id: Some(super::types::FlexibleId::Str(
                "msg-suppress-cross-2".to_string(),
            )),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let memories_after = bot
            .task_store
            .list_user_memories("user-a", 10)
            .expect("查询失败");
        assert_eq!(memories_after.len(), 1);
        assert_eq!(memories_after[0].id, memory_id);
    }

    #[test]
    fn user_memory_useful_command_marks_memory_useful() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);
        bot.context_token_map
            .insert("user-a".to_string(), "ctx-1".to_string());

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "记住 我喜欢短摘要".to_string(),
            message_id: Some(super::types::FlexibleId::Str(
                "msg-memory-useful-1".to_string(),
            )),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let memories = bot
            .task_store
            .list_user_memories("user-a", 10)
            .expect("查询失败");
        let memory_id = memories[0].id.clone();

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: format!("有用 {memory_id}"),
            message_id: Some(super::types::FlexibleId::Str(
                "msg-memory-useful-2".to_string(),
            )),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let after = bot
            .task_store
            .list_user_memories("user-a", 10)
            .expect("查询失败");
        assert!(after[0].useful);
        assert_eq!(after[0].use_count, 1);
    }

    #[test]
    fn user_memory_useful_command_cannot_mark_other_users_memory() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);
        bot.context_token_map
            .insert("user-a".to_string(), "ctx-1".to_string());
        bot.context_token_map
            .insert("user-b".to_string(), "ctx-2".to_string());

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "记住 仅 user-a 可标记有用".to_string(),
            message_id: Some(super::types::FlexibleId::Str(
                "msg-memory-useful-cross-1".to_string(),
            )),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let memories = bot
            .task_store
            .list_user_memories("user-a", 10)
            .expect("查询失败");
        let memory_id = memories[0].id.clone();

        bot.handle_message(WireMessage {
            from_user_id: "user-b".to_string(),
            text: format!("有用 {memory_id}"),
            message_id: Some(super::types::FlexibleId::Str(
                "msg-memory-useful-cross-2".to_string(),
            )),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let after = bot
            .task_store
            .list_user_memories("user-a", 10)
            .expect("查询失败");
        assert!(!after[0].useful);
        assert_eq!(after[0].use_count, 0);
    }

    #[test]
    fn auto_memory_is_extracted_from_chat_text() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "我在研究 Rust Agent".to_string(),
            message_id: Some(super::types::FlexibleId::Str(
                "msg-auto-memory-1".to_string(),
            )),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let memories = bot
            .task_store
            .list_user_memories("user-a", 10)
            .expect("查询 user_memory 失败");
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].content, "主题: Rust Agent");

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "我在研究 Rust Agent".to_string(),
            message_id: Some(super::types::FlexibleId::Str(
                "msg-auto-memory-2".to_string(),
            )),
            message_type: Some(1),
            ..WireMessage::default()
        });
        let deduped = bot
            .task_store
            .list_user_memories("user-a", 10)
            .expect("查询 user_memory 失败");
        assert_eq!(deduped.len(), 1);
    }

    #[test]
    fn chat_log_payload_keeps_contract_fields() {
        let payload = super::build_chat_log_payload(
            "info",
            "message_received",
            vec![
                ("user_id", json!("user-a")),
                ("message_id", json!("msg-1")),
                ("detail", Value::Null),
            ],
        );

        assert_eq!(payload["level"], "info");
        assert_eq!(payload["event"], "message_received");
        assert_eq!(payload["user_id"], "user-a");
        assert_eq!(payload["message_id"], "msg-1");
        assert!(payload.get("ts").is_some());
        assert!(payload.get("detail").is_none());
    }

    // ——— UserSessionState 接线测试 ———

    #[test]
    fn session_state_is_written_on_flush() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        // 直接调用 update_session_state_intent 验证写入逻辑
        bot.update_session_state_intent("user-a", "你好，帮我查状态");

        let state = bot
            .task_store
            .load_user_session_state("user-a")
            .expect("加载失败")
            .expect("应存在记录");
        assert_eq!(state.user_id, "user-a");
        assert_eq!(state.last_user_intent, Some("你好，帮我查状态".to_string()));
    }

    #[test]
    fn session_state_is_loaded_and_injected_into_agent_context() {
        let db_path = temp_db_path();
        let workspace_root = temp_dir();
        let mut bot = build_test_bot(&db_path, workspace_root.clone());

        // 预写入一条 session_state
        let record = crate::task_store::UserSessionStateRecord {
            user_id: "user-a".to_string(),
            last_user_intent: Some("预先存在的意图".to_string()),
            current_task: Some("task-pre".to_string()),
            next_step: Some("等待回复".to_string()),
            blocked_reason: None,
            updated_at: "2026-04-17T10:00:00Z".to_string(),
            ..Default::default()
        };
        bot.task_store
            .upsert_user_session_state(&record)
            .expect("预写入失败");

        // 触发 send_generated_reply，它会加载 session_state 并注入 context
        // 使用 agent 能成功执行的命令（创建文件），确保 run_with_context 成功返回 run_id
        bot.send_generated_reply(
            "user-a",
            "创建文件 test_session_state.txt :: hello",
            &["msg-ss-2".to_string()],
            FlushReason::Commit,
        );

        // 验证 trace 中记录了 persistent_state_present
        let trace_root = workspace_root.join("data").join("agent_traces");
        let day_dir = std::fs::read_dir(&trace_root)
            .expect("应存在 trace 根目录")
            .next()
            .expect("应存在日期目录")
            .expect("读取日期目录失败")
            .path();
        let trace_path = std::fs::read_dir(day_dir)
            .expect("应存在 trace 文件")
            .filter_map(|entry| entry.ok().map(|value| value.path()))
            .find(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
            .expect("应存在至少一个 json trace 文件");
        let payload: Value = serde_json::from_str(
            &std::fs::read_to_string(trace_path).expect("读取 trace 文件失败"),
        )
        .expect("trace JSON 应合法");

        assert_eq!(payload["persistent_state_present"], true);
        assert_eq!(payload["persistent_state_source"], "db");
        assert_eq!(payload["persistent_state_updated"], true); // send_generated_reply 成功更新后 patch
    }

    #[test]
    fn session_state_updated_at_is_refreshed_after_agent_run() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        // 预写入旧状态
        let old = crate::task_store::UserSessionStateRecord {
            user_id: "user-a".to_string(),
            last_user_intent: Some("旧意图".to_string()),
            current_task: None,
            next_step: None,
            blocked_reason: None,
            updated_at: "2026-04-01T00:00:00Z".to_string(),
            ..Default::default()
        };
        bot.task_store
            .upsert_user_session_state(&old)
            .expect("预写入失败");

        // 直接触发 send_generated_reply，它会加载已有 state 并回写 updated_at
        bot.send_generated_reply(
            "user-a",
            "读文件 README.md",
            &["msg-ss-3".to_string()],
            FlushReason::Commit,
        );

        let state = bot
            .task_store
            .load_user_session_state("user-a")
            .expect("加载失败")
            .expect("应存在");
        // updated_at 应被刷新（不是旧值）
        assert_ne!(state.updated_at, "2026-04-01T00:00:00Z");
    }

    #[test]
    fn session_state_user_isolation_in_chat_adapter() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        // 直接调用 update_session_state_intent 验证用户隔离
        bot.update_session_state_intent("user-a", "A 的消息");
        bot.update_session_state_intent("user-b", "B 的消息");

        let state_a = bot
            .task_store
            .load_user_session_state("user-a")
            .expect("加载失败")
            .expect("应存在");
        let state_b = bot
            .task_store
            .load_user_session_state("user-b")
            .expect("加载失败")
            .expect("应存在");
        assert_eq!(state_a.last_user_intent, Some("A 的消息".to_string()));
        assert_eq!(state_b.last_user_intent, Some("B 的消息".to_string()));
    }

    #[test]
    fn session_state_v2_fields_written_on_flush() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        bot.update_session_state_intent("user-a", "帮我整理本周待办");

        let state = bot
            .task_store
            .load_user_session_state("user-a")
            .expect("加载失败")
            .expect("应存在");
        assert_eq!(state.last_user_intent, Some("帮我整理本周待办".to_string()));
        assert_eq!(
            state.goal,
            Some("响应当前用户请求：帮我整理本周待办".to_string())
        );
        assert_eq!(state.current_subtask, Some("帮我整理本周待办".to_string()));
        assert!(!state.is_v2_empty() || state.goal.is_some());
    }

    #[test]
    fn context_debug_does_not_corrupt_persistent_state() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        // 预写入一条带 v2 字段的 session state
        let record = crate::task_store::UserSessionStateRecord {
            user_id: "user-a".to_string(),
            goal: Some("整理任务".to_string()),
            current_subtask: Some("读取任务".to_string()),
            next_step: Some("确认状态".to_string()),
            constraints_json: Some(r#"["时间有限"]"#.to_string()),
            confirmed_facts_json: Some(r#"["有3个pending"]"#.to_string()),
            ..Default::default()
        };
        bot.task_store
            .upsert_user_session_state(&record)
            .expect("预写入失败");

        // 通过 handle_message 触发 /context 查询
        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "/context 测试".to_string(),
            message_id: Some(super::types::FlexibleId::Str("msg-cd-1".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        // 验证持久化 state 未被破坏
        let after = bot
            .task_store
            .load_user_session_state("user-a")
            .expect("加载失败")
            .expect("应存在");
        assert_eq!(after.goal, Some("整理任务".to_string()));
        assert_eq!(after.current_subtask, Some("读取任务".to_string()));
        assert_eq!(after.next_step, Some("确认状态".to_string()));
        assert_eq!(after.constraints(), vec!["时间有限"]);
        assert_eq!(after.confirmed_facts(), vec!["有3个pending"]);
    }

    #[test]
    fn no_session_state_does_not_panic() {
        let db_path = temp_db_path();
        let bot = test_bot(&db_path);

        // 无持久化 state 时调用 send_generated_reply 不应 panic
        // 使用一个不需要 agent 运行的命令来避免实际调用 LLM
        // 这里只验证 load_user_session_state 返回 None 时不会崩溃
        let state = bot
            .task_store
            .load_user_session_state("user-a")
            .expect("查询不应失败");
        assert!(state.is_none());
    }

    // ===== 分段发送与处理中回执测试 =====

    #[test]
    fn split_reply_into_chunks_short_text_single_chunk() {
        let reply = "短文本，直接单条发送。";
        let chunks = super::delivery::split_reply_into_chunks(reply, 1200);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], reply);
    }

    #[test]
    fn split_reply_into_chunks_long_text_multi_chunks() {
        // 构造超过 max_chars 的长文本
        let reply = "一段很长的测试内容。".repeat(100);
        let chunks = super::delivery::split_reply_into_chunks(&reply, 120);
        assert!(chunks.len() > 1, "应切分为多段");
        // 每段都应包含（i/n）前缀
        for (i, chunk) in chunks.iter().enumerate() {
            let expected_prefix = format!("（{}/{}）", i + 1, chunks.len());
            assert!(
                chunk.starts_with(&expected_prefix),
                "第 {} 段应以前缀 {} 开头,实际为: {}",
                i,
                expected_prefix,
                chunk
            );
        }
        // 每段字符数不超过 max_chars
        for chunk in &chunks {
            assert!(
                chunk.chars().count() <= 120,
                "每段不应超过 120 字符: {}",
                chunk
            );
        }
    }

    #[test]
    fn split_reply_into_chunks_preserves_full_content_when_joined() {
        let reply = "测试内容".repeat(80); // 320 字符
        let chunks = super::delivery::split_reply_into_chunks(&reply, 120);
        assert!(chunks.len() > 1);
        // 去掉每段的（i/n）前缀，拼接后应与原文一致
        let reassembled: String = chunks
            .iter()
            .map(|c| {
                if let Some(pos) = c.find('）') {
                    c[pos..].chars().skip(1).collect::<String>()
                } else {
                    c.to_string()
                }
            })
            .collect();
        assert_eq!(reassembled, reply, "去掉前缀后拼接应与原文完全一致");
    }

    #[test]
    fn should_send_processing_ack_threshold_behavior() {
        // 短输入：不应触发
        assert!(!super::delivery::should_send_processing_ack("你好"));
        assert!(!super::delivery::should_send_processing_ack("短消息"));
        assert!(!super::delivery::should_send_processing_ack("   "));
        // 刚好达到阈值：应触发
        let at_threshold = "a".repeat(super::PROCESSING_ACK_MIN_INPUT_CHARS);
        assert!(super::delivery::should_send_processing_ack(&at_threshold));
        // 超过阈值：应触发
        let above_threshold = "b".repeat(super::PROCESSING_ACK_MIN_INPUT_CHARS + 1);
        assert!(super::delivery::should_send_processing_ack(
            &above_threshold
        ));
        // 刚好低于阈值：不应触发
        let below_threshold = "c".repeat(super::PROCESSING_ACK_MIN_INPUT_CHARS - 1);
        assert!(!super::delivery::should_send_processing_ack(
            &below_threshold
        ));
    }

    /// 回归测试：Promoted 分支对短/非 ASCII memory_id 不再 panic，
    /// 且记忆类型可正确被提升为 explicit。
    #[test]
    fn user_memory_promoted_short_non_ascii_id_safe_preview() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);
        bot.context_token_map
            .insert("user-a".to_string(), "ctx-1".to_string());

        // 1. 先写入一条 auto memory
        let mut ws = crate::task_store::MemoryWriteState::default();
        let decision = bot.task_store.govern_memory_write(
            "user-a",
            "我喜欢短摘要",
            crate::task_store::MemoryType::Auto,
            60,
            &mut ws,
        );
        let original_id = match decision {
            crate::task_store::WriteDecision::Written(r) => r.id,
            other => panic!("应写入 auto memory: {:?}", other),
        };

        // 2. 用 rusqlite::Connection 将该 memory 的 id 更新为短非 ASCII（例如 "短"）
        let conn = Connection::open(&db_path).expect("打开数据库失败");
        let affected = conn
            .execute(
                "UPDATE user_memories SET id = ?1 WHERE id = ?2",
                ["短", &original_id],
            )
            .expect("UPDATE 失败");
        assert_eq!(affected, 1, "应更新 1 行");
        drop(conn);

        // 3. 发送 "记住 <同内容>" 触发 WriteDecision::Promoted 分支，应不 panic。
        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "记住 我喜欢短摘要".to_string(),
            message_id: Some(super::types::FlexibleId::Str(
                "msg-promote-safe-1".to_string(),
            )),
            message_type: Some(1),
            ..WireMessage::default()
        });

        // 验证记忆已被提升为 explicit
        let memories = bot
            .task_store
            .list_user_memories("user-a", 10)
            .expect("查询失败");
        assert_eq!(memories.len(), 1);
        assert_eq!(
            memories[0].memory_type,
            crate::task_store::MemoryType::Explicit
        );
    }

    // ——— B2 回归测试 ———

    #[test]
    fn split_reply_into_chunks_zero_budget_does_not_loop() {
        // max_chars 过小导致 content_budget 为 0 时，不应死循环或 panic，
        // 且退化为无前缀切分后每段仍不超过 max_chars。
        let max_chars = 1usize;
        let chunks = super::delivery::split_reply_into_chunks("测试内容", max_chars);
        assert!(!chunks.is_empty());
        assert!(chunks.iter().any(|c| c.contains("测试内容")
            || c.contains("测")
            || c.contains("试")
            || c.contains("内")
            || c.contains("容")));
        for chunk in &chunks {
            assert!(
                chunk.chars().count() <= max_chars,
                "每段不应超过 max_chars={}: {}",
                max_chars,
                chunk
            );
        }
    }

    #[test]
    fn flush_expired_sessions_deletes_persisted_state() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        // 1. 直接写入一条持久化 session_state
        bot.task_store
            .upsert_session_state("user-expired", "过期会话", &["msg-exp-1".to_string()])
            .expect("写入失败");
        assert_eq!(
            bot.task_store
                .list_session_states()
                .expect("查询失败")
                .len(),
            1
        );

        // 2. 在 session_router 恢复一条已过期会话（timeout=0 即立即过期）
        bot.session_router =
            crate::session_router::SessionRouter::new(std::time::Duration::from_secs(0));
        bot.session_router.restore_session(
            "user-expired",
            "过期会话",
            vec!["msg-exp-1".to_string()],
            std::time::Instant::now(),
        );

        // 3. flush_expired_sessions 应删除持久化 state
        bot.flush_expired_sessions();
        assert!(
            bot.task_store
                .list_session_states()
                .expect("查询失败")
                .is_empty(),
            "flush_expired_sessions 应删除持久化 session_state"
        );
    }

    #[test]
    fn mark_seen_store_error_is_fail_closed() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        // 破坏数据库：删除 message_dedup 表，使 record_inbound_message 失败
        let raw_conn = Connection::open(&db_path).expect("打开 raw 连接失败");
        raw_conn
            .execute("DROP TABLE message_dedup", [])
            .expect("删除表失败");
        drop(raw_conn);

        // mark_seen 在 DB 失败时应返回 false（fail-closed）
        let result = bot.mark_seen("msg-1", "user-a", "hello");
        assert!(
            !result,
            "mark_seen 在 store 失败时应返回 false，避免重复处理"
        );
    }
}
