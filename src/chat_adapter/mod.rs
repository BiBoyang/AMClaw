use crate::agent_core::AgentCore;
use crate::command_router;
use crate::config::AppConfig;
use crate::session_router::{SessionEvent, SessionRouter};
use crate::task_store::TaskStore;
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

pub fn run(config: AppConfig, running: Arc<AtomicBool>) -> Result<()> {
    let mut bot = WeChatBot::new(config, running)?;
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

    fn login(&mut self, running: &AtomicBool) -> Result<()> {
        println!("[登录] 正在获取二维码...");
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
            println!("\n请用微信扫描以下二维码登录:");
            println!("  {url}\n");
        } else {
            println!("[登录] 二维码URL为空，qrcodeId: {qrcode_id}");
        }

        println!("[登录] 等待扫码...");
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

                println!("[登录] 登录成功!");
                println!("  bot_id: {}", self.ilink_bot_id);
                println!("  user_id: {}", self.ilink_user_id);
                return Ok(());
            }

            if ret == 1 || (ret == 0 && status == "wait") {
                continue;
            }

            println!("[登录] 未知状态: {}", compact_json(&status_resp));
        }

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

        println!(
            "[调试] getupdates 返回: {}",
            truncate_for_log(&compact_json(&resp), 500)
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
    task_store: TaskStore,
    context_token_map: HashMap<String, String>,
    cursor: String,
    seen_ids: HashSet<String>,
    seen_order: VecDeque<String>,
    session_router: SessionRouter,
    running: Arc<AtomicBool>,
}

impl WeChatBot {
    fn new(config: AppConfig, running: Arc<AtomicBool>) -> Result<Self> {
        let workspace_root = std::env::current_dir().context("获取工作目录失败")?;
        let db_path = config.db_path();
        Ok(Self {
            agent_core: AgentCore::new(workspace_root)?,
            client: ILinkClient::new(config.wechat.channel_version.clone())?,
            task_store: TaskStore::open(&db_path)?,
            context_token_map: HashMap::new(),
            cursor: String::new(),
            seen_ids: HashSet::new(),
            seen_order: VecDeque::new(),
            session_router: SessionRouter::new(config.session_merge_timeout()),
            running,
        })
    }

    fn start(&mut self) -> Result<()> {
        println!("=== 微信 iLink Bot Demo (Rust) ===\n");
        self.client.login(&self.running)?;
        println!("\n[Bot] 开始接收消息，长轮询中...\n");
        self.poll_loop();
        Ok(())
    }

    fn poll_loop(&mut self) {
        while self.running.load(Ordering::Relaxed) {
            self.flush_expired_sessions();

            let poll_timeout = self.next_poll_timeout();
            match self.client.get_updates(&self.cursor, poll_timeout) {
                Ok(result) => {
                    if !result.cursor.is_empty() && result.cursor != self.cursor {
                        self.cursor = result.cursor;
                    }
                    for msg in result.messages {
                        self.handle_message(msg);
                    }
                }
                Err(err) => {
                    if is_poll_timeout_error(&err) {
                        continue;
                    }
                    eprintln!("[轮询] 错误: {err}");
                    println!("[轮询] 5秒后重试...");
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
        }

        let text = collect_text(wire);
        if text.is_empty() {
            return;
        }

        let msg_id = self.extract_message_id(wire);
        if !self.mark_seen(&msg_id, from_user_id, &text) {
            return;
        }

        println!("[收到消息] {from_user_id}: {text}");
        let intent = command_router::route_text(&text);
        let event = self
            .session_router
            .on_intent(from_user_id, intent, Instant::now());
        self.handle_session_event(event);
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
                eprintln!("[去重] 数据库写入失败: {err}");
                true
            }
        };
        if !is_new {
            return false;
        }

        let id = id.to_string();
        if self.seen_ids.contains(&id) {
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

    fn generate_reply(&self, user_text: &str) -> String {
        match self.agent_core.run(user_text) {
            Ok(result) => return result,
            Err(err) => {
                let err_text = err.to_string();
                if is_agent_command(user_text) {
                    return format!("执行失败: {err_text}");
                }
                if is_llm_auth_error(&err_text) {
                    return "LLM 鉴权失败（401），请检查 MOONSHOT_* / DEEPSEEK_* / OPENAI_* 配置"
                        .to_string();
                }
                eprintln!("[Bot] agent_fallback reason={}", err_text);
            }
        }
        if user_text == "hello" || user_text == "你好" {
            return "你好！我是 iLink Bot Demo（Rust版），有什么可以帮你的？".to_string();
        }
        if user_text == "时间" || user_text == "几点了" {
            let now = Utc::now().with_timezone(&Shanghai);
            return format!("现在是 {}", now.format("%Y-%m-%d %H:%M:%S"));
        }
        if user_text == "帮助" || user_text == "help" {
            return "可用命令:\n- hello / 你好\n- 时间\n- 帮助 / help\n- 其他文字我会 echo 回复"
                .to_string();
        }
        format!("Echo: {user_text}")
    }

    fn handle_session_event(&mut self, event: SessionEvent) {
        if let SessionEvent::FlushNow {
            user_id,
            merged_text,
        } = event
        {
            self.send_generated_reply(&user_id, &merged_text);
        }
    }

    fn flush_expired_sessions(&mut self) {
        for item in self.session_router.flush_expired(Instant::now()) {
            self.send_generated_reply(&item.user_id, &item.merged_text);
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

    fn send_generated_reply(&mut self, user_id: &str, merged_text: &str) {
        if merged_text.trim().is_empty() {
            return;
        }

        let display_text = merged_text.replace('\n', "\\n");
        println!("[会话提交] {user_id}: {display_text}");

        let reply = self.generate_reply(merged_text);
        let Some(token) = self.context_token_map.get(user_id).cloned() else {
            println!("[警告] 无 {user_id} 的 context_token，跳过回复");
            return;
        };

        match self.client.send_text_message(user_id, &reply, &token) {
            Ok(()) => println!("[已回复] {user_id}: {reply}"),
            Err(err) => eprintln!("[发送失败] {err}"),
        }
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

fn is_poll_timeout_error(err: &anyhow::Error) -> bool {
    err.chain()
        .filter_map(|cause| cause.downcast_ref::<reqwest::Error>())
        .any(reqwest::Error::is_timeout)
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

fn truncate_for_log(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut out: String = input.chars().take(max_chars).collect();
    out.push_str("...");
    out
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
                Err(err) => eprintln!(
                    "[调试] 跳过无法解析的消息: {}; raw={}",
                    err,
                    truncate_for_log(&compact_json(raw), 200)
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
    use crate::session_router::SessionRouter;
    use crate::task_store::TaskStore;
    use rusqlite::Connection;
    use std::collections::{HashMap, HashSet, VecDeque};
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

    fn test_bot(db_path: &std::path::Path) -> WeChatBot {
        let workspace_root = temp_dir();
        WeChatBot {
            agent_core: AgentCore::new(workspace_root).expect("初始化 agent 失败"),
            client: ILinkClient::new("1.0.0").expect("初始化 iLink 客户端失败"),
            task_store: TaskStore::open(db_path).expect("初始化 task store 失败"),
            context_token_map: HashMap::new(),
            cursor: String::new(),
            seen_ids: HashSet::new(),
            seen_order: VecDeque::new(),
            session_router: SessionRouter::new(Duration::from_secs(5)),
            running: Arc::new(AtomicBool::new(true)),
        }
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
}
