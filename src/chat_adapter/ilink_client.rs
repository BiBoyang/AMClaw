use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use reqwest::blocking::Client;
use reqwest::header::CONTENT_TYPE;
use reqwest::Method;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::Duration;
use uuid::Uuid;

use super::ingest::extract_messages;
use super::types::GetUpdatesResult;
use super::{
    assert_ok, compact_json, first_non_empty, get_i64, get_str,
    log_chat_info, log_chat_warn, truncate_for_log, BASE_URL,
};

pub(super) struct ILinkClient {
    http: Client,
    base_url: String,
    bot_token: Option<String>,
    ilink_bot_id: String,
    ilink_user_id: String,
    wechat_uin: String,
    channel_version: String,
}

impl ILinkClient {
    pub(super) fn new(channel_version: impl Into<String>) -> Result<Self> {
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

    pub(super) fn login(&mut self, running: &AtomicBool) -> Result<()> {
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

    pub(super) fn get_updates(&self, cursor: &str, timeout: Duration) -> Result<GetUpdatesResult> {
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

    pub(super) fn send_text_message(
        &self,
        to_user_id: &str,
        text: &str,
        context_token: &str,
    ) -> Result<()> {
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
