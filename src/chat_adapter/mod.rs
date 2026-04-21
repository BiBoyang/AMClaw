use crate::agent_core::{AgentCore, AgentRunContext};
use crate::command_router;
use crate::config::{AppConfig, ResolvedBrowserConfig};
use crate::pipeline::Pipeline;
use crate::reporter::DailyReporter;
use crate::scheduler::{DailyReportSchedule, WeeklyReportSchedule};
use crate::session_router::{FlushReason, SessionEvent, SessionRouter};
use crate::task_store::{MarkTaskArchivedInput, TaskStore};
use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use chrono::Utc;
use chrono_tz::Asia::Shanghai;
use reqwest::blocking::Client;
use reqwest::header::CONTENT_TYPE;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

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

#[derive(Debug, Clone, Deserialize, Default)]
struct TextItem {
    #[serde(default)]
    text: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct MessageItem {
    #[serde(default)]
    text_item: Option<TextItem>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum FlexibleId {
    Str(String),
    Int(i64),
    Float(f64),
    Obj(Value),
}

impl FlexibleId {
    fn as_string(&self) -> String {
        match self {
            Self::Str(v) => v.clone(),
            Self::Int(v) => v.to_string(),
            Self::Float(v) => {
                if v.fract() == 0.0 {
                    (*v as i64).to_string()
                } else {
                    v.to_string()
                }
            }
            Self::Obj(v) => {
                if let Some(value) = value_to_string(v) {
                    value
                } else {
                    compact_json(v)
                }
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
struct WireMessage {
    #[serde(default)]
    message: Option<Box<WireMessage>>,
    #[serde(default)]
    from_user_id: String,
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    create_time_ms: Option<i64>,
    #[serde(default)]
    message_type: Option<i64>,
    #[serde(default)]
    context_token: String,
    #[serde(default)]
    item_list: Vec<MessageItem>,
    #[serde(default)]
    text: String,
    #[serde(default)]
    message_id: Option<FlexibleId>,
    #[serde(default)]
    msg_id: Option<FlexibleId>,
}

struct GetUpdatesResult {
    messages: Vec<WireMessage>,
    cursor: String,
}

struct ILinkClient {
    http: Client,
    base_url: String,
    bot_token: Option<String>,
    ilink_bot_id: String,
    ilink_user_id: String,
    wechat_uin: String,
    channel_version: String,
}

impl ILinkClient {
    fn new(channel_version: impl Into<String>) -> Result<Self> {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(70))
            .build()
            .context("创建 HTTP 客户端失败")?;

        let uuid = Uuid::new_v4();
        let bytes = uuid.as_bytes();
        let wechat_uin = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);

        Ok(Self {
            http,
            base_url: BASE_URL.to_string(),
            bot_token: None,
            ilink_bot_id: String::new(),
            ilink_user_id: String::new(),
            wechat_uin: BASE64_STANDARD.encode(wechat_uin.to_string()),
            channel_version: channel_version.into(),
        })
    }

    fn build_url(&self, path: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        let normalized_path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        format!("{base}{normalized_path}")
    }

    fn request(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        self.request_with_timeout(method, path, body, None)
    }

    fn request_with_timeout(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        timeout: Option<Duration>,
    ) -> Result<Value> {
        let method_name = method.as_str().to_string();
        let mut req = self
            .http
            .request(method, self.build_url(path))
            .header(CONTENT_TYPE, "application/json")
            .header("iLink-App-ClientVersion", "1")
            .header("X-WECHAT-UIN", &self.wechat_uin);

        if let Some(timeout) = timeout {
            req = req.timeout(timeout);
        }

        if let Some(token) = self.bot_token.as_deref().filter(|v| !v.trim().is_empty()) {
            req = req
                .header("Authorization", format!("Bearer {token}"))
                .header("AuthorizationType", "ilink_bot_token");
        }

        if let Some(payload) = body {
            req = req.json(&payload);
        }

        let resp = req
            .send()
            .with_context(|| format!("请求失败: {method_name} {path}"))?;
        let status = resp.status();
        let text = resp
            .text()
            .with_context(|| format!("读取响应失败: {method_name} {path}"))?;
        let data: Value = serde_json::from_str(&text).with_context(|| {
            format!(
                "[HTTP {}] {} {} 返回非JSON: {}",
                status.as_u16(),
                method_name,
                path,
                truncate_for_log(&text, 300)
            )
        })?;

        if !status.is_success() {
            bail!(
                "[HTTP {}] {} {} 失败: {}",
                status.as_u16(),
                method_name,
                path,
                truncate_for_log(&compact_json(&data), 500)
            );
        }

        Ok(data)
    }

    fn get_qrcode(&self) -> Result<Value> {
        self.request(Method::GET, "/ilink/bot/get_bot_qrcode?bot_type=3", None)
    }

    fn get_qrcode_status(&self, qrcode_id: &str) -> Result<Value> {
        self.request(
            Method::GET,
            &format!("/ilink/bot/get_qrcode_status?qrcode={qrcode_id}"),
            None,
        )
    }

    fn fetch_login_qrcode(&self) -> Result<String> {
        log_chat_info("login_qrcode_requested", vec![]);
        let qr_resp = self.get_qrcode()?;
        assert_ok(&qr_resp, "获取二维码")?;

        let qrcode_id =
            first_non_empty([get_str(&qr_resp, "qrcode"), get_str(&qr_resp, "qrcode_id")])
                .ok_or_else(|| anyhow::anyhow!("二维码ID为空: {}", compact_json(&qr_resp)))?;

        let qrcode_url = first_non_empty([
            get_str(&qr_resp, "qrcode_img_content"),
            get_str(&qr_resp, "qrcode_url"),
            get_str(&qr_resp, "url"),
        ]);

        if let Some(url) = qrcode_url {
            log_chat_info(
                "login_qrcode_ready",
                vec![("qrcode_id", json!(qrcode_id)), ("qrcode_url", json!(url))],
            );
        } else {
            log_chat_warn(
                "login_qrcode_missing_url",
                vec![("qrcode_id", json!(qrcode_id))],
            );
        }

        Ok(qrcode_id)
    }

    fn login(&mut self, running: &AtomicBool) -> Result<()> {
        let mut qrcode_id = self.fetch_login_qrcode()?;

        log_chat_info(
            "login_waiting_for_scan",
            vec![("qrcode_id", json!(qrcode_id))],
        );
        while running.load(Ordering::Relaxed) {
            sleep(Duration::from_secs(2));
            let status_resp = self.get_qrcode_status(&qrcode_id)?;
            let ret = get_i64(&status_resp, "ret").unwrap_or(0);
            let status = get_str(&status_resp, "status").unwrap_or_default();

            if ret == 0 && status == "confirmed" {
                self.bot_token = get_str(&status_resp, "bot_token");
                self.ilink_bot_id = get_str(&status_resp, "ilink_bot_id").unwrap_or_default();
                self.ilink_user_id = get_str(&status_resp, "ilink_user_id").unwrap_or_default();
                if let Some(base_url) = get_str(&status_resp, "baseurl") {
                    if !base_url.trim().is_empty() {
                        self.base_url = base_url;
                    }
                }

                log_chat_info(
                    "login_confirmed",
                    vec![
                        ("qrcode_id", json!(qrcode_id)),
                        ("status", json!(status)),
                        ("bot_id", json!(self.ilink_bot_id)),
                        ("user_id", json!(self.ilink_user_id)),
                    ],
                );
                return Ok(());
            }

            if ret == 1 || (ret == 0 && matches!(status.as_str(), "wait" | "scanned")) {
                continue;
            }

            if ret == 0 && status == "expired" {
                log_chat_warn(
                    "login_qrcode_expired",
                    vec![("qrcode_id", json!(qrcode_id)), ("status", json!(status))],
                );
                qrcode_id = self.fetch_login_qrcode()?;
                log_chat_info(
                    "login_waiting_for_scan",
                    vec![("qrcode_id", json!(qrcode_id))],
                );
                continue;
            }

            log_chat_warn(
                "login_status_unknown",
                vec![
                    ("qrcode_id", json!(qrcode_id)),
                    ("status", json!(status)),
                    ("ret", json!(ret)),
                    (
                        "detail",
                        json!(truncate_for_log(&compact_json(&status_resp), 240)),
                    ),
                ],
            );
        }

        log_chat_warn("login_aborted", vec![("qrcode_id", json!(qrcode_id))]);
        bail!("登录被中止")
    }

    fn get_updates(&self, cursor: &str, timeout: Duration) -> Result<GetUpdatesResult> {
        let body = json!({
            "get_updates_buf": cursor,
            "base_info": {
                "channel_version": self.channel_version
            }
        });
        let resp = self.request_with_timeout(
            Method::POST,
            "/ilink/bot/getupdates",
            Some(body),
            Some(timeout),
        )?;
        assert_ok(&resp, "getupdates")?;

        log_chat_info(
            "poll_updates_received",
            vec![("detail", json!(truncate_for_log(&compact_json(&resp), 500)))],
        );

        let new_cursor = first_non_empty([
            get_str(&resp, "get_updates_buf"),
            get_str(&resp, "cursor"),
            get_str(&resp, "sync_buf"),
            Some(cursor.to_string()),
        ])
        .unwrap_or_default();

        let messages = extract_messages(&resp);
        Ok(GetUpdatesResult {
            messages,
            cursor: new_cursor,
        })
    }

    fn send_text_message(&self, to_user_id: &str, text: &str, context_token: &str) -> Result<()> {
        let body = json!({
            "msg": {
                "from_user_id": "",
                "to_user_id": to_user_id,
                "client_id": format!("amclaw:{}", Uuid::new_v4()),
                "message_type": 2,
                "message_state": 2,
                "context_token": context_token,
                "item_list": [
                    {
                        "type": 1,
                        "text_item": { "text": text }
                    }
                ]
            },
            "base_info": {
                "channel_version": self.channel_version
            }
        });

        let resp = self.request(Method::POST, "/ilink/bot/sendmessage", Some(body))?;
        assert_ok(&resp, "sendmessage")
    }
}

struct WeChatBot {
    agent_core: AgentCore,
    client: ILinkClient,
    pipeline: Pipeline,
    reporter: DailyReporter,
    task_store: TaskStore,
    context_token_map: HashMap<String, String>,
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
        let mut bot = Self {
            agent_core: AgentCore::with_task_store_db_path_and_agent_config(
                workspace_root,
                db_path.clone(),
                &config.agent,
            )?,
            client: ILinkClient::new(config.wechat.channel_version.clone())?,
            pipeline: Pipeline::new(root_dir, browser)?,
            reporter: DailyReporter::from_config(&config)?,
            task_store: TaskStore::open(&db_path)?,
            context_token_map: HashMap::new(),
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
        Ok(bot)
    }

    fn start(&mut self) -> Result<()> {
        log_chat_info("bot_starting", vec![]);
        self.client.login(&self.running)?;
        log_chat_info("bot_polling_started", vec![]);
        self.poll_loop();
        Ok(())
    }

    fn poll_loop(&mut self) {
        while self.running.load(Ordering::Relaxed) {
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

    fn handle_message(&mut self, msg: WireMessage) {
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

    fn extract_message_id(&self, msg: &WireMessage) -> String {
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

    fn mark_seen(&mut self, id: &str, from_user_id: &str, text: &str) -> bool {
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

    /// 返回 (reply_text, optional_run_id)
    /// run_id 仅在 agent_core 成功执行时存在，用于后续 trace 补更新
    fn generate_reply(
        &self,
        user_id: &str,
        user_text: &str,
        message_ids: &[String],
        reason: FlushReason,
        trace_context: AgentRunContext,
    ) -> (String, Option<String>, Option<std::path::PathBuf>) {
        match self.agent_core.run_with_context(user_text, trace_context) {
            Ok(result) => {
                return (result.output, Some(result.run_id), result.trace_json_path);
            }
            Err(err) => {
                let err_text = err.to_string();
                if is_agent_command(user_text) {
                    log_chat_error(
                        "agent_reply_failed",
                        vec![
                            ("user_id", json!(user_id)),
                            ("trigger", json!(reason.as_str())),
                            ("message_ids", json!(message_ids)),
                            ("message_count", json!(message_ids.len())),
                            ("error_kind", json!("agent_command_failed")),
                            ("detail", json!(err_text.clone())),
                        ],
                    );
                    return (format!("执行失败: {err_text}"), None, None);
                }
                if is_llm_auth_error(&err_text) {
                    log_chat_warn(
                        "agent_reply_fallback",
                        vec![
                            ("user_id", json!(user_id)),
                            ("trigger", json!(reason.as_str())),
                            ("message_ids", json!(message_ids)),
                            ("message_count", json!(message_ids.len())),
                            ("error_kind", json!("llm_auth_failed")),
                            ("detail", json!(err_text.clone())),
                        ],
                    );
                    return (
                        "LLM 鉴权失败（401），请检查 MOONSHOT_* / DEEPSEEK_* / OPENAI_* 配置"
                            .to_string(),
                        None,
                        None,
                    );
                }
                log_chat_warn(
                    "agent_reply_fallback",
                    vec![
                        ("user_id", json!(user_id)),
                        ("trigger", json!(reason.as_str())),
                        ("message_ids", json!(message_ids)),
                        ("message_count", json!(message_ids.len())),
                        ("error_kind", json!("agent_run_failed")),
                        ("detail", json!(err_text)),
                    ],
                );
            }
        }
        if user_text == "hello" || user_text == "你好" {
            return (
                "你好！我是 iLink Bot Demo（Rust版），有什么可以帮你的？".to_string(),
                None,
                None,
            );
        }
        if user_text == "时间" || user_text == "几点了" {
            let now = Utc::now().with_timezone(&Shanghai);
            return (
                format!("现在是 {}", now.format("%Y-%m-%d %H:%M:%S")),
                None,
                None,
            );
        }
        if user_text == "帮助" || user_text == "help" {
            return (
                "可用命令:\n- hello / 你好\n- 时间\n- 帮助 / help\n- 发送链接或 收藏 <url>\n- 状态 <task_id>\n- 最近任务\n- 日报 [YYYY-MM-DD] / 今日整理\n- 周报 [YYYY-WW]\n- 记住 <content>\n- 我的记忆\n- 有用 <memory_id>\n- 重试 <task_id>\n- /context [text]\n- /context verbose [text]\n- 其他文字我会 echo 回复"
                    .to_string(),
                None,
                None,
            );
        }
        (format!("Echo: {user_text}"), None, None)
    }

    fn handle_link_submission(&mut self, user_id: &str, urls: Vec<String>) {
        let mut records = Vec::new();
        let mut failures = Vec::new();

        for url in urls {
            match self.task_store.record_link_submission(&url) {
                Ok(record) => records.push(record),
                Err(err) => failures.push(format!("{url} => {err}")),
            }
        }

        let reply = build_link_submission_reply(&records, &failures);
        self.send_reply_text(user_id, &reply);
    }

    fn handle_task_status_query(&self, user_id: &str, task_id: &str) {
        let reply = match self.task_store.get_task_status(task_id) {
            Ok(Some(status)) => build_task_status_reply(&status),
            Ok(None) => format!("未找到对应任务: {task_id}"),
            Err(err) => format!("查询任务状态失败: {err}"),
        };
        self.send_reply_text(user_id, &reply);
    }

    fn handle_recent_tasks_query(&self, user_id: &str) {
        let reply = match self.task_store.list_recent_tasks(5) {
            Ok(tasks) => build_recent_tasks_reply(&tasks),
            Err(err) => format!("查询最近任务失败: {err}"),
        };
        self.send_reply_text(user_id, &reply);
    }

    fn handle_context_debug_query(&self, user_id: &str, extra_text: Option<&str>, verbose: bool) {
        let pending = self.session_router.snapshot(user_id);
        let mut parts = Vec::new();
        let mut message_ids = Vec::new();
        if let Some(snapshot) = &pending {
            if !snapshot.merged_text.trim().is_empty() {
                parts.push(snapshot.merged_text.trim().to_string());
            }
            message_ids = snapshot.message_ids.clone();
        }
        if let Some(extra_text) = extra_text.filter(|value| !value.trim().is_empty()) {
            parts.push(extra_text.trim().to_string());
        }

        if parts.is_empty() {
            self.send_reply_text(
                user_id,
                "当前没有待提交会话。可直接发送 `/context 你的问题` 预览一次上下文装配。",
            );
            return;
        }

        let merged_text = parts.join("\n");
        let mode = if verbose {
            crate::agent_core::ContextPreviewMode::Verbose
        } else {
            crate::agent_core::ContextPreviewMode::Summary
        };

        // 加载持久化 session state 供预览（只读，不破坏已持久化 state）
        let session_state = self
            .task_store
            .load_user_session_state(user_id)
            .ok()
            .flatten();
        let context = AgentRunContext::wechat_chat(user_id, "context_debug", message_ids)
            .with_session_text(&merged_text)
            .with_context_token_present(self.context_token_map.contains_key(user_id))
            .with_user_session_state(session_state);

        let reply = match if matches!(mode, crate::agent_core::ContextPreviewMode::Summary) {
            self.agent_core
                .preview_context_with_context(&merged_text, context)
        } else {
            self.agent_core
                .preview_context_with_context_mode(&merged_text, context, mode)
        } {
            Ok(reply) => reply,
            Err(err) => format!("生成 context preview 失败: {err}"),
        };
        self.send_reply_text(user_id, &reply);
    }

    fn handle_daily_report_query(&self, user_id: &str, day: Option<&str>) {
        let reply = self.build_daily_report_query_reply(day);
        self.send_reply_text(user_id, &reply);
    }

    fn handle_weekly_report_query(&self, user_id: &str, week: Option<&str>) {
        let reply = self.build_weekly_report_query_reply(week);
        self.send_reply_text(user_id, &reply);
    }

    fn handle_user_memory_write(&mut self, user_id: &str, content: &str) {
        let mut write_state = crate::task_store::MemoryWriteState::default();
        let decision = self.task_store.govern_memory_write(
            user_id,
            content,
            crate::task_store::MemoryType::Explicit,
            100,
            &mut write_state,
        );
        let reply = match &decision {
            crate::task_store::WriteDecision::Written(record) => {
                log_chat_info(
                    "user_memory_explicit_written",
                    vec![
                        ("user_id", json!(user_id)),
                        ("memory_id", json!(record.id)),
                        (
                            "content_preview",
                            json!(summarize_text_for_log(content, 120)),
                        ),
                    ],
                );
                format!("已记住\n- {}", content.trim())
            }
            crate::task_store::WriteDecision::Skipped { reason, .. } => {
                log_chat_info(
                    "user_memory_explicit_skipped",
                    vec![
                        ("user_id", json!(user_id)),
                        ("skip_reason", json!(reason.to_string())),
                    ],
                );
                format!("未能记住: {}", reason)
            }
            crate::task_store::WriteDecision::Promoted { id, reason } => {
                log_chat_info(
                    "user_memory_explicit_promoted",
                    vec![
                        ("user_id", json!(user_id)),
                        ("memory_id", json!(id)),
                        ("promote_reason", json!(reason.to_string())),
                    ],
                );
                format!("已提升已有记忆为显式记忆 (id: {})", &id[..8])
            }
        };
        self.send_reply_text(user_id, &reply);
    }

    fn handle_user_memory_suppress(&self, user_id: &str, memory_id: &str) {
        let reply = match self.task_store.suppress_memory(user_id, memory_id) {
            Ok(()) => format!("已屏蔽记忆: {memory_id}"),
            Err(err) => format!("屏蔽记忆失败: {err}"),
        };
        self.send_reply_text(user_id, &reply);
    }

    fn handle_user_memory_useful(&self, user_id: &str, memory_id: &str) {
        let reply = match self.task_store.confirm_memory_useful(user_id, memory_id) {
            Ok(()) => format!("已标记记忆有用: {memory_id}"),
            Err(err) => format!("标记记忆有用失败: {err}"),
        };
        self.send_reply_text(user_id, &reply);
    }

    fn handle_user_memories_query(&self, user_id: &str) {
        let reply = match self.task_store.list_user_memories(user_id, 10) {
            Ok(memories) => build_user_memories_reply(&memories),
            Err(err) => format!("查询记忆失败: {err}"),
        };
        self.send_reply_text(user_id, &reply);
    }

    fn build_daily_report_query_reply(&self, day: Option<&str>) -> String {
        let day = day
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| self.reporter.current_day());
        match self.reporter.generate_for_day(&day) {
            Ok(report) => {
                // 如果 summary 太短（空任务场景），尝试补上 markdown 文件中的详细内容
                let detailed = std::fs::read_to_string(&report.markdown_path)
                    .ok()
                    .map(|content| sanitize_report_markdown_for_wechat(&content));
                if let Some(content) = detailed {
                    // 微信单条消息限制约 4096 字符，截断到安全长度
                    if content.chars().count() > 3800 {
                        let truncated: String = content.chars().take(3800).collect();
                        format!(
                            "{truncated}\n\n...(已截断，共 {item_count} 条)",
                            item_count = report.item_count
                        )
                    } else {
                        content
                    }
                } else {
                    report.summary
                }
            }
            Err(err) => format!("生成日报失败: {err}"),
        }
    }

    fn build_weekly_report_query_reply(&self, week: Option<&str>) -> String {
        let week = week
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| self.reporter.current_week());
        match self.reporter.generate_weekly_for_week(&week) {
            Ok(report) => {
                let detailed = std::fs::read_to_string(&report.markdown_path)
                    .ok()
                    .map(|content| sanitize_report_markdown_for_wechat(&content));
                if let Some(content) = detailed {
                    if content.chars().count() > 3800 {
                        let truncated: String = content.chars().take(3800).collect();
                        format!(
                            "{truncated}\n\n...(已截断，共 {item_count} 条)",
                            item_count = report.item_count
                        )
                    } else {
                        content
                    }
                } else {
                    report.summary
                }
            }
            Err(err) => format!("生成周报失败: {err}"),
        }
    }

    fn maybe_persist_auto_memory(&mut self, user_id: &str, intent: &command_router::RouteIntent) {
        let text = match intent {
            command_router::RouteIntent::ChatContinue { text }
            | command_router::RouteIntent::ChatCommit { text }
            | command_router::RouteIntent::ChatPending { text } => text.as_str(),
            _ => return,
        };
        let Some(memory) = extract_auto_memory_candidate(text) else {
            return;
        };
        let mut write_state = crate::task_store::MemoryWriteState::default();
        let decision = self.task_store.govern_memory_write(
            user_id,
            &memory,
            crate::task_store::MemoryType::Auto,
            60,
            &mut write_state,
        );
        match &decision {
            crate::task_store::WriteDecision::Written(_) => {
                log_chat_info(
                    "user_memory_auto_recorded",
                    vec![
                        ("user_id", json!(user_id)),
                        (
                            "memory_preview",
                            json!(summarize_text_for_log(&memory, 120)),
                        ),
                    ],
                );
            }
            crate::task_store::WriteDecision::Skipped { reason, .. } => {
                log_chat_info(
                    "user_memory_auto_skipped",
                    vec![
                        ("user_id", json!(user_id)),
                        ("skip_reason", json!(reason.to_string())),
                    ],
                );
            }
            crate::task_store::WriteDecision::Promoted { .. } => {
                // auto 不会 promote（只有 explicit 能 promote auto）
            }
        }
    }

    fn handle_task_retry(&mut self, user_id: &str, task_id: &str) {
        let reply = match self.task_store.retry_task(task_id) {
            Ok(Some(_status)) => match self.process_pending_task_by_id(task_id) {
                Ok(Some(final_status)) => build_task_retry_reply(&final_status),
                Ok(None) => format!("未找到对应任务: {task_id}"),
                Err(err) => format!("任务已重置为 pending，但重试处理失败: {err}"),
            },
            Ok(None) => format!("未找到对应任务: {task_id}"),
            Err(err) => format!("重试任务失败: {err}"),
        };
        self.send_reply_text(user_id, &reply);
    }

    fn handle_manual_tasks_query(&self, user_id: &str) {
        let reply = match self.task_store.list_manual_tasks(5) {
            Ok(tasks) => build_manual_tasks_reply(&tasks),
            Err(err) => format!("查询待补录任务失败: {err}"),
        };
        self.send_reply_text(user_id, &reply);
    }

    fn handle_manual_content_submission(&mut self, user_id: &str, task_id: &str, content: &str) {
        let reply = match self.task_store.get_task_content(task_id) {
            Ok(Some(task)) => match self.pipeline.archive_manual_content(&task, content) {
                Ok(result) => {
                    let output_path = result.output_path.to_string_lossy().to_string();
                    match self.task_store.mark_task_archived(
                        task_id,
                        MarkTaskArchivedInput {
                            output_path: &output_path,
                            title: result.title.as_deref(),
                            page_kind: Some("manual_input"),
                            snapshot_path: None,
                            content_source: Some("manual_input"),
                            summary: None,
                        },
                    ) {
                        Ok(true) => format!(
                            "已写入人工补正文\ntask_id: {task_id}\noutput_path: {output_path}"
                        ),
                        Ok(false) => format!("未找到对应任务: {task_id}"),
                        Err(err) => format!("更新任务状态失败: {err}"),
                    }
                }
                Err(err) => format!("人工补录归档失败: {err}"),
            },
            Ok(None) => format!("未找到对应任务: {task_id}"),
            Err(err) => format!("查询任务上下文失败: {err}"),
        };
        self.send_reply_text(user_id, &reply);
    }

    fn handle_session_event(&mut self, event: SessionEvent) {
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

    fn flush_expired_sessions(&mut self) {
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
    fn update_session_state_intent(&mut self, user_id: &str, merged_text: &str) {
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

    fn next_poll_timeout(&self) -> Duration {
        self.session_router
            .next_flush_delay(Instant::now())
            .map(|delay| {
                delay
                    .max(MIN_GET_UPDATES_TIMEOUT)
                    .min(DEFAULT_GET_UPDATES_TIMEOUT)
            })
            .unwrap_or(DEFAULT_GET_UPDATES_TIMEOUT)
    }

    fn send_generated_reply(
        &mut self,
        user_id: &str,
        merged_text: &str,
        message_ids: &[String],
        reason: FlushReason,
    ) {
        if merged_text.trim().is_empty() {
            return;
        }

        log_chat_info(
            "session_flushed",
            vec![
                ("user_id", json!(user_id)),
                ("trigger", json!(reason.as_str())),
                ("message_ids", json!(message_ids)),
                ("message_count", json!(message_ids.len())),
                ("text_chars", json!(merged_text.chars().count())),
                (
                    "text_preview",
                    json!(summarize_text_for_log(merged_text, 160)),
                ),
            ],
        );

        log_chat_info(
            "agent_reply_started",
            vec![
                ("user_id", json!(user_id)),
                ("trigger", json!(reason.as_str())),
                ("message_ids", json!(message_ids)),
                ("message_count", json!(message_ids.len())),
            ],
        );

        // 长输入先发“处理中”回执，避免用户空等
        if should_send_processing_ack(merged_text) {
            self.send_reply_text(user_id, PROCESSING_ACK_TEXT);
        }

        // C2: 加载持久化 SessionState
        let session_state = match self.task_store.load_user_session_state(user_id) {
            Ok(state) => {
                log_chat_info(
                    "session_state_loaded",
                    vec![
                        ("user_id", json!(user_id)),
                        ("state_present", json!(state.is_some())),
                        (
                            "state_source",
                            json!(if state.is_some() { "db" } else { "none" }),
                        ),
                    ],
                );
                state
            }
            Err(err) => {
                log_chat_warn(
                    "session_state_load_failed",
                    vec![
                        ("user_id", json!(user_id)),
                        ("error_kind", json!("session_state_load_failed")),
                        ("detail", json!(err.to_string())),
                    ],
                );
                None
            }
        };

        let context = AgentRunContext::wechat_chat(user_id, reason.as_str(), message_ids.to_vec())
            .with_session_text(merged_text)
            .with_context_token_present(self.context_token_map.contains_key(user_id))
            .with_user_session_state(session_state);

        let (reply, run_id, trace_json_path) =
            self.generate_reply(user_id, merged_text, message_ids, reason, context);

        // C2: agent 完成后刷新 updated_at（最小回写，不推导深状态）
        let mut state_updated = false;
        match self.task_store.load_user_session_state(user_id) {
            Ok(Some(state)) => {
                let mut updated = state.clone();
                updated.updated_at = Utc::now().to_rfc3339();
                if let Err(err) = self.task_store.upsert_user_session_state(&updated) {
                    log_chat_warn(
                        "session_state_upsert_failed",
                        vec![
                            ("user_id", json!(user_id)),
                            ("error_kind", json!("session_state_upsert_failed")),
                            ("detail", json!(err.to_string())),
                        ],
                    );
                } else {
                    state_updated = true;
                    log_chat_info(
                        "session_state_refreshed",
                        vec![
                            ("user_id", json!(user_id)),
                            ("state_updated", json!(true)),
                            ("v2_slots_populated", json!(updated.populated_slot_count())),
                        ],
                    );
                }
            }
            Ok(None) => {
                log_chat_info(
                    "session_state_noop",
                    vec![
                        ("user_id", json!(user_id)),
                        ("reason", json!("no_persistent_state")),
                    ],
                );
            }
            Err(err) => {
                log_chat_warn(
                    "session_state_upsert_read_failed",
                    vec![
                        ("user_id", json!(user_id)),
                        ("error_kind", json!("session_state_load_failed")),
                        ("detail", json!(err.to_string())),
                    ],
                );
            }
        }

        // 补更新 trace 中的 persistent_state_updated（按路径 patch，不再扫描目录）
        if state_updated {
            if let Some(path) = trace_json_path {
                if let Err(err) = self
                    .agent_core
                    .patch_trace_persistent_state_updated(&path, true)
                {
                    log_chat_warn(
                        "trace_patch_state_updated_failed",
                        vec![
                            ("user_id", json!(user_id)),
                            ("run_id", json!(&run_id)),
                            ("error_kind", json!("trace_patch_failed")),
                            ("detail", json!(err.to_string())),
                        ],
                    );
                }
            }
        }

        log_chat_info(
            "agent_reply_finished",
            vec![
                ("user_id", json!(user_id)),
                ("trigger", json!(reason.as_str())),
                ("message_ids", json!(message_ids)),
                ("message_count", json!(message_ids.len())),
                ("reply_chars", json!(reply.chars().count())),
                ("reply_preview", json!(summarize_text_for_log(&reply, 160))),
            ],
        );
        self.send_reply_text(user_id, &reply);
    }

    fn send_reply_text(&self, user_id: &str, reply: &str) {
        let token = self
            .context_token_map
            .get(user_id)
            .cloned()
            .or_else(|| self.task_store.get_context_token(user_id).ok().flatten());
        let Some(token) = token else {
            log_chat_warn(
                "reply_skipped_no_context_token",
                vec![
                    ("user_id", json!(user_id)),
                    ("reply_chars", json!(reply.chars().count())),
                    ("reply_preview", json!(summarize_text_for_log(reply, 120))),
                ],
            );
            return;
        };

        let chunks = split_reply_into_chunks(reply, WECHAT_REPLY_CHUNK_MAX_CHARS);
        let chunk_total = chunks.len();
        let mut sent_chunk_count = 0usize;
        let mut all_sent = true;
        for (idx, chunk) in chunks.iter().enumerate() {
            match self.client.send_text_message(user_id, chunk, &token) {
                Ok(()) => {
                    sent_chunk_count += 1;
                    log_chat_info(
                        "reply_chunk_sent",
                        vec![
                            ("user_id", json!(user_id)),
                            ("chunk_index", json!(idx + 1)),
                            ("chunk_total", json!(chunk_total)),
                            ("chunk_chars", json!(chunk.chars().count())),
                        ],
                    )
                }
                Err(err) => {
                    all_sent = false;
                    log_chat_error(
                        "reply_send_failed",
                        vec![
                            ("user_id", json!(user_id)),
                            ("chunk_index", json!(idx + 1)),
                            ("chunk_total", json!(chunk_total)),
                            ("error_kind", json!("wechat_send_failed")),
                            ("detail", json!(err.to_string())),
                        ],
                    );
                    // 一旦某段发送失败，停止后续段，避免乱序
                    break;
                }
            }
        }

        if all_sent {
            log_chat_info(
                "reply_sent",
                vec![
                    ("user_id", json!(user_id)),
                    ("reply_chars", json!(reply.chars().count())),
                    ("chunk_total", json!(chunk_total)),
                    ("sent_chunk_count", json!(sent_chunk_count)),
                    ("all_sent", json!(true)),
                    ("reply_preview", json!(summarize_text_for_log(reply, 120))),
                ],
            );
        } else {
            log_chat_warn(
                "reply_partially_sent",
                vec![
                    ("user_id", json!(user_id)),
                    ("reply_chars", json!(reply.chars().count())),
                    ("chunk_total", json!(chunk_total)),
                    ("sent_chunk_count", json!(sent_chunk_count)),
                    ("all_sent", json!(false)),
                    ("reply_preview", json!(summarize_text_for_log(reply, 120))),
                ],
            );
        }
    }

    fn process_scheduled_daily_report_push(&mut self) {
        let Some(schedule) = &self.daily_report_schedule else {
            return;
        };
        let Some(day) =
            schedule.should_run_now(Utc::now(), self.last_daily_report_push_day.as_deref())
        else {
            return;
        };
        let reply = self.build_daily_report_query_reply(Some(&day));
        let target_user_id = schedule.report_to_user_id().to_string();
        let token = self
            .context_token_map
            .get(&target_user_id)
            .cloned()
            .or_else(|| {
                self.task_store
                    .get_context_token(&target_user_id)
                    .ok()
                    .flatten()
            });
        let Some(token) = token else {
            log_chat_warn(
                "scheduler_daily_report_skipped",
                vec![
                    ("day", json!(day)),
                    ("user_id", json!(target_user_id)),
                    ("error_kind", json!("missing_context_token")),
                ],
            );
            return;
        };

        // 日报正文可能较长，复用分段发送
        let chunks = split_reply_into_chunks(&reply, WECHAT_REPLY_CHUNK_MAX_CHARS);
        let chunk_total = chunks.len();
        let mut all_ok = true;
        for (idx, chunk) in chunks.iter().enumerate() {
            match self
                .client
                .send_text_message(&target_user_id, chunk, &token)
            {
                Ok(()) => log_chat_info(
                    "scheduler_daily_report_chunk_sent",
                    vec![
                        ("day", json!(&day)),
                        ("user_id", json!(&target_user_id)),
                        ("chunk_index", json!(idx + 1)),
                        ("chunk_total", json!(chunk_total)),
                        ("chunk_chars", json!(chunk.chars().count())),
                    ],
                ),
                Err(err) => {
                    all_ok = false;
                    log_chat_error(
                        "scheduler_daily_report_send_failed",
                        vec![
                            ("day", json!(&day)),
                            ("user_id", json!(&target_user_id)),
                            ("chunk_index", json!(idx + 1)),
                            ("chunk_total", json!(chunk_total)),
                            ("error_kind", json!("scheduler_daily_report_send_failed")),
                            ("detail", json!(err.to_string())),
                        ],
                    );
                    break;
                }
            }
        }
        if all_ok {
            self.last_daily_report_push_day = Some(day.clone());
            log_chat_info(
                "scheduler_daily_report_sent",
                vec![
                    ("day", json!(day)),
                    ("user_id", json!(target_user_id)),
                    ("reply_chars", json!(reply.chars().count())),
                    ("chunk_total", json!(chunk_total)),
                ],
            );
        }
    }

    fn process_scheduled_weekly_report_push(&mut self) {
        let Some(schedule) = &self.weekly_report_schedule else {
            return;
        };
        let Some(week) =
            schedule.should_run_now(Utc::now(), self.last_weekly_report_push_week.as_deref())
        else {
            return;
        };
        let reply = self.build_weekly_report_query_reply(Some(&week));
        let target_user_id = schedule.report_to_user_id().to_string();
        let token = self
            .context_token_map
            .get(&target_user_id)
            .cloned()
            .or_else(|| {
                self.task_store
                    .get_context_token(&target_user_id)
                    .ok()
                    .flatten()
            });
        let Some(token) = token else {
            log_chat_warn(
                "scheduler_weekly_report_skipped",
                vec![
                    ("week", json!(week)),
                    ("user_id", json!(target_user_id)),
                    ("error_kind", json!("missing_context_token")),
                ],
            );
            return;
        };

        let chunks = split_reply_into_chunks(&reply, WECHAT_REPLY_CHUNK_MAX_CHARS);
        let chunk_total = chunks.len();
        let mut all_ok = true;
        for (idx, chunk) in chunks.iter().enumerate() {
            match self
                .client
                .send_text_message(&target_user_id, chunk, &token)
            {
                Ok(()) => log_chat_info(
                    "scheduler_weekly_report_chunk_sent",
                    vec![
                        ("week", json!(&week)),
                        ("user_id", json!(&target_user_id)),
                        ("chunk_index", json!(idx + 1)),
                        ("chunk_total", json!(chunk_total)),
                        ("chunk_chars", json!(chunk.chars().count())),
                    ],
                ),
                Err(err) => {
                    all_ok = false;
                    log_chat_error(
                        "scheduler_weekly_report_send_failed",
                        vec![
                            ("week", json!(&week)),
                            ("user_id", json!(&target_user_id)),
                            ("chunk_index", json!(idx + 1)),
                            ("chunk_total", json!(chunk_total)),
                            ("error_kind", json!("scheduler_weekly_report_send_failed")),
                            ("detail", json!(err.to_string())),
                        ],
                    );
                    break;
                }
            }
        }
        if all_ok {
            self.last_weekly_report_push_week = Some(week.clone());
            log_chat_info(
                "scheduler_weekly_report_sent",
                vec![
                    ("week", json!(week)),
                    ("user_id", json!(target_user_id)),
                    ("reply_chars", json!(reply.chars().count())),
                    ("chunk_total", json!(chunk_total)),
                ],
            );
        }
    }

    fn persist_session_snapshot(&mut self, user_id: &str) {
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

    fn restore_persisted_sessions(&mut self) -> Result<()> {
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

    fn process_pending_tasks(&mut self) {
        let pending = match self.task_store.list_pending_tasks(5) {
            Ok(tasks) => tasks,
            Err(err) => {
                log_chat_error(
                    "pending_tasks_query_failed",
                    vec![
                        ("error_kind", json!("pending_tasks_query_failed")),
                        ("detail", json!(err.to_string())),
                    ],
                );
                return;
            }
        };

        for task in pending {
            if let Err(err) = self.process_single_pending_task(&task) {
                log_chat_error(
                    "pending_task_process_failed",
                    vec![
                        ("task_id", json!(task.task_id)),
                        ("error_kind", json!("pending_task_process_failed")),
                        ("detail", json!(err.to_string())),
                    ],
                );
            }
        }
    }

    fn process_pending_task_by_id(
        &mut self,
        task_id: &str,
    ) -> Result<Option<crate::task_store::TaskStatusRecord>> {
        let Some(task) = self.task_store.get_pending_task(task_id)? else {
            return self.task_store.get_task_status(task_id);
        };
        self.process_single_pending_task(&task)?;
        self.task_store.get_task_status(task_id)
    }

    fn process_single_pending_task(
        &mut self,
        task: &crate::task_store::PendingTaskRecord,
    ) -> Result<()> {
        match self.pipeline.process_pending_task(task) {
            Ok(result) => {
                let output_path = result.output_path.to_string_lossy().to_string();
                let snapshot_path = result
                    .snapshot_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().to_string());
                self.task_store
                    .mark_task_archived(
                        &task.task_id,
                        MarkTaskArchivedInput {
                            output_path: &output_path,
                            title: result.title.as_deref(),
                            page_kind: Some(&result.page_kind),
                            snapshot_path: snapshot_path.as_deref(),
                            content_source: Some(&result.content_source),
                            summary: result.summary.as_deref(),
                        },
                    )
                    .with_context(|| format!("更新 archived 失败 task_id={}", task.task_id))?;
                log_chat_info(
                    "pending_task_archived",
                    vec![
                        ("task_id", json!(task.task_id)),
                        ("status", json!("archived")),
                        (
                            "output_path",
                            json!(result.output_path.display().to_string()),
                        ),
                    ],
                );
            }
            Err(err) => match err.kind {
                crate::pipeline::PipelineFailureKind::AwaitingManualInput { page_kind } => {
                    let snapshot_path = err
                        .snapshot_path
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string());
                    let content_source = err.content_source.clone();
                    self.task_store
                        .mark_task_awaiting_manual_input(
                            &task.task_id,
                            &err.message,
                            &page_kind,
                            snapshot_path.as_deref(),
                            content_source.as_deref(),
                        )
                        .with_context(|| {
                            format!("更新 awaiting_manual_input 失败 task_id={}", task.task_id)
                        })?;
                    log_chat_warn(
                        "pending_task_awaiting_manual_input",
                        vec![
                            ("task_id", json!(task.task_id)),
                            ("status", json!("awaiting_manual_input")),
                            ("page_kind", json!(page_kind)),
                            ("detail", json!(err.message)),
                        ],
                    );
                }
                crate::pipeline::PipelineFailureKind::Failed => {
                    self.task_store
                        .mark_task_failed(&task.task_id, &err.message)
                        .with_context(|| format!("更新 failed 失败 task_id={}", task.task_id))?;
                    log_chat_error(
                        "pending_task_failed",
                        vec![
                            ("task_id", json!(task.task_id)),
                            ("status", json!("failed")),
                            ("error_kind", json!("pipeline_task_failed")),
                            ("detail", json!(err.message)),
                        ],
                    );
                }
            },
        }

        Ok(())
    }
}

fn is_agent_command(text: &str) -> bool {
    let raw = text.trim();
    raw.starts_with("读文件 ")
        || raw.starts_with("创建文件 ")
        || raw.starts_with("写文件 ")
        || raw.starts_with("read ")
        || raw.starts_with("create ")
        || raw.starts_with("write ")
        || raw.starts_with("帮我运行：")
        || raw.starts_with("帮我运行:")
        || raw.starts_with("请帮我运行：")
        || raw.starts_with("请帮我运行:")
}

fn is_llm_auth_error(err: &str) -> bool {
    err.contains("HTTP 401") || err.contains("Authentication Fails")
}

fn sanitize_report_markdown_for_wechat(content: &str) -> String {
    content
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with("- output_path:")
                && !trimmed.starts_with("- snapshot_path:")
                && !trimmed.starts_with("- markdown_path:")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_poll_timeout_error(err: &anyhow::Error) -> bool {
    err.chain()
        .filter_map(|cause| cause.downcast_ref::<reqwest::Error>())
        .any(reqwest::Error::is_timeout)
}

fn build_link_submission_reply(
    records: &[crate::task_store::LinkTaskRecord],
    failures: &[String],
) -> String {
    let mut lines = Vec::new();
    if !records.is_empty() {
        if records.len() == 1 && failures.is_empty() {
            let record = &records[0];
            let status = if record.created_new {
                "已收录链接"
            } else {
                "链接已存在"
            };
            lines.push(status.to_string());
            lines.push(format!("url: {}", record.normalized_url));
            lines.push(format!("task_id: {}", record.task_id));
        } else {
            lines.push("链接处理结果:".to_string());
            for record in records {
                let status = if record.created_new {
                    "新建"
                } else {
                    "已存在"
                };
                lines.push(format!(
                    "- {status} {} task_id={}",
                    record.normalized_url, record.task_id
                ));
            }
        }
    }
    for failure in failures {
        lines.push(format!("- 失败 {failure}"));
    }
    if lines.is_empty() {
        return "没有可入库的链接".to_string();
    }
    lines.join("\n")
}

fn build_task_status_reply(status: &crate::task_store::TaskStatusRecord) -> String {
    let mut lines = vec![
        "任务状态".to_string(),
        format!("task_id: {}", status.task_id),
        format!("url: {}", status.normalized_url),
        format!(
            "source: {}",
            status.content_source.as_deref().unwrap_or("unknown")
        ),
        format!(
            "page_kind: {}",
            status.page_kind.as_deref().unwrap_or("unknown")
        ),
        format!("status: {}", status.status),
        format!("retry_count: {}", status.retry_count),
        format!("created_at: {}", status.created_at),
        format!("updated_at: {}", status.updated_at),
    ];
    if let Some(title) = &status.title {
        if !title.trim().is_empty() {
            lines.push(format!("title: {title}"));
        }
    }
    if let Some(output_path) = &status.output_path {
        if !output_path.trim().is_empty() {
            lines.push(format!("output_path: {output_path}"));
        }
    }
    if let Some(snapshot_path) = &status.snapshot_path {
        if !snapshot_path.trim().is_empty() {
            lines.push(format!("snapshot_path: {snapshot_path}"));
        }
    }
    if let Some(last_error) = &status.last_error {
        if !last_error.trim().is_empty() {
            lines.push(format!("last_error: {last_error}"));
        }
    }
    if status.status == "awaiting_manual_input" {
        lines.push(format!(
            "action_required: 请使用 补正文 {} :: <content>",
            status.task_id
        ));
    }
    lines.join("\n")
}

fn build_recent_tasks_reply(tasks: &[crate::task_store::RecentTaskRecord]) -> String {
    if tasks.is_empty() {
        return "最近没有任务".to_string();
    }

    let mut lines = vec!["最近任务:".to_string()];
    for task in tasks {
        lines.push(format!(
            "- {} {} source={} page_kind={} task_id={}",
            task.status,
            task.normalized_url,
            task.content_source.as_deref().unwrap_or("unknown"),
            task.page_kind.as_deref().unwrap_or("unknown"),
            task.task_id
        ));
    }
    lines.join("\n")
}

fn build_manual_tasks_reply(tasks: &[crate::task_store::RecentTaskRecord]) -> String {
    if tasks.is_empty() {
        return "当前没有待补录任务".to_string();
    }

    let mut lines = vec!["待补录任务:".to_string()];
    for task in tasks {
        lines.push(format!(
            "- {} {} source={} page_kind={} task_id={}",
            task.status,
            task.normalized_url,
            task.content_source.as_deref().unwrap_or("unknown"),
            task.page_kind.as_deref().unwrap_or("unknown"),
            task.task_id
        ));
    }
    lines.join("\n")
}

fn build_task_retry_reply(status: &crate::task_store::TaskStatusRecord) -> String {
    format!("任务已重试\n{}", build_task_status_reply(status))
}

fn build_user_memories_reply(memories: &[crate::task_store::UserMemoryRecord]) -> String {
    if memories.is_empty() {
        return "当前还没有保存的记忆".to_string();
    }

    let mut lines = vec!["我的记忆:".to_string()];
    for memory in memories {
        lines.push(format!("- id: {} | {}", memory.id, memory.content));
    }
    lines.join("\n")
}

fn extract_auto_memory_candidate(input: &str) -> Option<String> {
    let text = input.trim();
    if text.is_empty() {
        return None;
    }

    for prefix in ["我更喜欢", "我喜欢", "我偏好", "I prefer ", "I like "] {
        if let Some(rest) = text.strip_prefix(prefix) {
            let value = rest.trim();
            if !value.is_empty() {
                return Some(format!("偏好: {value}"));
            }
        }
    }

    for prefix in [
        "我关注",
        "我在研究",
        "我最近在看",
        "我想了解",
        "我在做",
        "I am researching ",
        "I'm researching ",
        "I want to learn ",
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            let value = rest.trim();
            if !value.is_empty() {
                return Some(format!("主题: {value}"));
            }
        }
    }

    None
}

fn assert_ok(resp: &Value, action: &str) -> Result<()> {
    let ret = get_i64(resp, "ret").unwrap_or(0);
    let errcode = get_i64(resp, "errcode").unwrap_or(0);
    if ret != 0 || errcode != 0 {
        let errmsg = get_str(resp, "errmsg").unwrap_or_default();
        bail!("{action} 失败: ret={ret} errcode={errcode} errmsg={errmsg}");
    }
    Ok(())
}

fn get_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

fn get_str(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
}

fn first_non_empty<const N: usize>(items: [Option<String>; N]) -> Option<String> {
    items.into_iter().flatten().find(|v| !v.trim().is_empty())
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<json-serialize-error>".to_string())
}

fn log_chat_info(event: &str, fields: Vec<(&str, Value)>) {
    log_chat_event("info", event, fields);
}

fn log_chat_warn(event: &str, fields: Vec<(&str, Value)>) {
    log_chat_event("warn", event, fields);
}

fn log_chat_error(event: &str, fields: Vec<(&str, Value)>) {
    log_chat_event("error", event, fields);
}

fn log_chat_event(level: &str, event: &str, fields: Vec<(&str, Value)>) {
    crate::logging::emit_structured_log(level, event, fields);
}

#[cfg(test)]
fn build_chat_log_payload(level: &str, event: &str, fields: Vec<(&str, Value)>) -> Value {
    crate::logging::build_structured_log_payload(level, event, fields)
}

fn truncate_for_log(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut out: String = input.chars().take(max_chars).collect();
    out.push_str("...");
    out
}

fn summarize_text_for_log(input: &str, max_chars: usize) -> String {
    truncate_for_log(&input.replace('\n', "\\n"), max_chars)
}

/// 将长回复按 max_chars 安全切分为多段，短文本返回单段。
/// 多段时每段加前缀 "（i/n）"。
fn split_reply_into_chunks(reply: &str, max_chars: usize) -> Vec<String> {
    let total_chars = reply.chars().count();
    if total_chars <= max_chars {
        return vec![reply.to_string()];
    }

    // 递归收敛：先按 max_chars 切内容，得到总段数 n；
    // 再用实际前缀长度（i/n 前缀）重新切分，直到稳定。
    let mut prev_count = 0usize;
    let mut segments: Vec<String> = Vec::new();

    for _ in 0..5 {
        // 保守预算：max_chars 减去最长前缀的字符数（最后一段前缀最长）
        let longest_prefix = if segments.is_empty() {
            "（1/2）".chars().count()
        } else {
            format!("（{}/{}）", segments.len(), segments.len())
                .chars()
                .count()
        };
        let content_budget = max_chars.saturating_sub(longest_prefix);
        if content_budget == 0 {
            break;
        }

        segments = split_content_only(reply, content_budget);
        if segments.len() == prev_count {
            break; // 已收敛
        }
        prev_count = segments.len();
    }

    let total = segments.len();
    segments
        .into_iter()
        .enumerate()
        .map(|(i, content)| format!("（{}/{}）{}", i + 1, total, content))
        .collect()
}

/// 仅按 content_budget 切分内容，不添加前缀。返回每段内容的 Vec<String>。
fn split_content_only(reply: &str, content_budget: usize) -> Vec<String> {
    if reply.is_empty() {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut char_indices = reply.char_indices().peekable();
    let mut start_byte = 0usize;

    while char_indices.peek().is_some() {
        let mut chars_count = 0usize;
        let mut end_byte = reply.len();
        while let Some((byte_idx, _)) = char_indices.peek() {
            if chars_count >= content_budget {
                end_byte = *byte_idx;
                break;
            }
            chars_count += 1;
            char_indices.next();
        }
        result.push(reply[start_byte..end_byte].to_string());
        start_byte = end_byte;
    }
    result
}

/// 判断是否应该发送“处理中”回执：trim 后长度达到阈值才回执。
fn should_send_processing_ack(user_text: &str) -> bool {
    user_text.trim().chars().count() >= PROCESSING_ACK_MIN_INPUT_CHARS
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(v) => Some(v.clone()),
        Value::Number(v) => Some(v.to_string()),
        Value::Object(map) => map
            .get("id")
            .or_else(|| map.get("str"))
            .or_else(|| map.get("value"))
            .and_then(value_to_string),
        _ => None,
    }
}

fn extract_messages(resp: &Value) -> Vec<WireMessage> {
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

fn collect_text(msg: &WireMessage) -> String {
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

fn now_epoch_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
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

    fn build_test_bot(db_path: &Path, workspace_root: PathBuf) -> WeChatBot {
        let reporter_root = temp_dir();
        let timezone = "Asia/Shanghai".parse().expect("解析测试 timezone 失败");
        let mut bot = WeChatBot {
            agent_core: AgentCore::with_task_store_db_path(workspace_root, db_path.to_path_buf())
                .expect("初始化 agent 失败"),
            client: ILinkClient::new("1.0.0").expect("初始化 iLink 客户端失败"),
            pipeline: Pipeline::new(temp_dir(), None::<ResolvedBrowserConfig>)
                .expect("初始化 pipeline 失败"),
            reporter: DailyReporter::new(reporter_root, db_path.to_path_buf(), timezone),
            task_store: TaskStore::open(db_path).expect("初始化 task store 失败"),
            context_token_map: HashMap::new(),
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
            message_id: Some(super::FlexibleId::Str("msg-1".to_string())),
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
            message_id: Some(super::FlexibleId::Str("msg-2".to_string())),
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
            message_id: Some(super::FlexibleId::Str("msg-3".to_string())),
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
            message_id: Some(super::FlexibleId::Str("msg-4".to_string())),
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
            message_id: Some(super::FlexibleId::Str("msg-5".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });
        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "https://example.com/".to_string(),
            message_id: Some(super::FlexibleId::Str("msg-6".to_string())),
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
            message_id: Some(super::FlexibleId::Str("msg-7".to_string())),
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
            message_id: Some(super::FlexibleId::Str("msg-8".to_string())),
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
            message_id: Some(super::FlexibleId::Str("msg-9".to_string())),
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
            message_id: Some(super::FlexibleId::Str("msg-10".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });
        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "最近任务".to_string(),
            message_id: Some(super::FlexibleId::Str("msg-11".to_string())),
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
        let mut bot = test_bot(&db_path);

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "https://example.com/retry-chat".to_string(),
            message_id: Some(super::FlexibleId::Str("msg-12".to_string())),
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
            message_id: Some(super::FlexibleId::Str("msg-13".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let row = task_row(&db_path, &task_id).expect("应存在任务");
        assert_eq!(row.1, 2);
        assert_ne!(row.0, "pending");
        assert_eq!(message_count(&db_path, "msg-13"), 1);
    }

    #[test]
    fn pending_link_task_is_consumed() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "https://example.com/archive-me".to_string(),
            message_id: Some(super::FlexibleId::Str("msg-14".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let task_id = first_task_id(&db_path);
        bot.process_pending_tasks();

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
            message_id: Some(super::FlexibleId::Str("msg-15".to_string())),
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
            message_id: Some(super::FlexibleId::Str("msg-16".to_string())),
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
            text: format!("补正文 {task_id} :: 这是人工补录的公众号正文"),
            message_id: Some(super::FlexibleId::Str("msg-17".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let details = task_status_details(&db_path, &task_id).expect("应存在任务状态");
        assert_eq!(details.0, "archived".to_string());
        assert_eq!(details.1, Some("manual_input".to_string()));
        assert!(details.2.is_some());
    }

    #[test]
    fn manual_tasks_query_does_not_create_new_tasks() {
        let db_path = temp_db_path();
        let mut bot = test_bot(&db_path);

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "https://mp.weixin.qq.com/s/YUvXg9i31QuQN6t-zRTe8g".to_string(),
            message_id: Some(super::FlexibleId::Str("msg-18".to_string())),
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
            message_id: Some(super::FlexibleId::Str("msg-19".to_string())),
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
            message_id: Some(super::FlexibleId::Str("msg-session-1".to_string())),
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
            message_id: Some(super::FlexibleId::Str("msg-context-1".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        bot.handle_message(WireMessage {
            from_user_id: "user-a".to_string(),
            text: "/context 再补一条".to_string(),
            message_id: Some(super::FlexibleId::Str("msg-context-2".to_string())),
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
            message_id: Some(super::FlexibleId::Str("msg-memory-1".to_string())),
            message_type: Some(1),
            ..WireMessage::default()
        });

        let memories = bot
            .task_store
            .list_user_memories("user-a", 10)
            .expect("查询 user_memory 失败");
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].content, "我喜欢短摘要");

        let reply = super::build_user_memories_reply(&memories);
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
            message_id: Some(super::FlexibleId::Str("msg-suppress-1".to_string())),
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
            message_id: Some(super::FlexibleId::Str("msg-suppress-2".to_string())),
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
            message_id: Some(super::FlexibleId::Str("msg-suppress-cross-1".to_string())),
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
            message_id: Some(super::FlexibleId::Str("msg-suppress-cross-2".to_string())),
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
            message_id: Some(super::FlexibleId::Str("msg-memory-useful-1".to_string())),
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
            message_id: Some(super::FlexibleId::Str("msg-memory-useful-2".to_string())),
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
            message_id: Some(super::FlexibleId::Str(
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
            message_id: Some(super::FlexibleId::Str(
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
            message_id: Some(super::FlexibleId::Str("msg-auto-memory-1".to_string())),
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
            message_id: Some(super::FlexibleId::Str("msg-auto-memory-2".to_string())),
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
            message_id: Some(super::FlexibleId::Str("msg-cd-1".to_string())),
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
        let chunks = super::split_reply_into_chunks(reply, 1200);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], reply);
    }

    #[test]
    fn split_reply_into_chunks_long_text_multi_chunks() {
        // 构造超过 max_chars 的长文本
        let reply = "一段很长的测试内容。".repeat(100);
        let chunks = super::split_reply_into_chunks(&reply, 120);
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
        let chunks = super::split_reply_into_chunks(&reply, 120);
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
        assert!(!super::should_send_processing_ack("你好"));
        assert!(!super::should_send_processing_ack("短消息"));
        assert!(!super::should_send_processing_ack("   "));
        // 刚好达到阈值：应触发
        let at_threshold = "a".repeat(super::PROCESSING_ACK_MIN_INPUT_CHARS);
        assert!(super::should_send_processing_ack(&at_threshold));
        // 超过阈值：应触发
        let above_threshold = "b".repeat(super::PROCESSING_ACK_MIN_INPUT_CHARS + 1);
        assert!(super::should_send_processing_ack(&above_threshold));
        // 刚好低于阈值：不应触发
        let below_threshold = "c".repeat(super::PROCESSING_ACK_MIN_INPUT_CHARS - 1);
        assert!(!super::should_send_processing_ack(&below_threshold));
    }
}
