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
mod tests;
