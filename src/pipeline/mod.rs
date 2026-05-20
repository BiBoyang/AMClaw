use crate::config::ResolvedBrowserConfig;
use crate::mode_policy::{check_url, AgentMode};
use crate::task_store::{PendingTaskRecord, TaskContentRecord};
use anyhow::{Context, Result};
use chrono::Utc;
use reqwest::blocking::Client;
use reqwest::header::LOCATION;
use reqwest::redirect::Policy;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::json;
#[cfg(test)]
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

pub(crate) mod html_extract;
pub(crate) mod logging;
pub(crate) mod markdown;

pub(crate) use html_extract::{
    extract_html_title, extract_http_archive_body, extract_primary_body, generate_rule_summary,
    preview_text,
};
pub(crate) use logging::*;

#[derive(Debug, Clone)]
pub struct Pipeline {
    root_dir: PathBuf,
    browser: Option<ResolvedBrowserConfig>,
    mode: AgentMode,
    http_client_with_redirect: Client,
    http_client_no_redirect: Client,
    #[cfg(test)]
    http_test_fixtures: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineResult {
    pub output_path: PathBuf,
    pub raw_path: PathBuf,
    pub snapshot_path: Option<PathBuf>,
    pub title: Option<String>,
    pub page_kind: String,
    pub content_source: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpFetchResult {
    html: String,
    final_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExtractedArchiveBody {
    markdown: String,
    page_kind: String,
    section_title: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BrowserCaptureRequest {
    url: String,
    html_path: PathBuf,
    screenshot_path: PathBuf,
    timeout_ms: u64,
    headless: bool,
    mobile_viewport: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BrowserCaptureResponse {
    ok: bool,
    page_kind: String,
    final_url: String,
    title: Option<String>,
    html_path: PathBuf,
    screenshot_path: PathBuf,
    reason: Option<String>,
    logs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BrowserCaptureResult {
    page_kind: String,
    final_url: String,
    title: Option<String>,
    html_path: PathBuf,
    screenshot_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineFailureKind {
    AwaitingManualInput { page_kind: String },
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineTaskError {
    pub kind: PipelineFailureKind,
    pub message: String,
    pub snapshot_path: Option<PathBuf>,
    pub content_source: Option<String>,
}

impl PipelineTaskError {
    fn failed(message: impl Into<String>) -> Self {
        Self {
            kind: PipelineFailureKind::Failed,
            message: message.into(),
            snapshot_path: None,
            content_source: None,
        }
    }

    fn awaiting_manual_input(page_kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: PipelineFailureKind::AwaitingManualInput {
                page_kind: page_kind.into(),
            },
            message: message.into(),
            snapshot_path: None,
            content_source: None,
        }
    }

    fn browser_manual_input(
        page_kind: impl Into<String>,
        message: impl Into<String>,
        snapshot_path: Option<PathBuf>,
    ) -> Self {
        Self {
            kind: PipelineFailureKind::AwaitingManualInput {
                page_kind: page_kind.into(),
            },
            message: message.into(),
            snapshot_path,
            content_source: Some("browser_capture".to_string()),
        }
    }
}

impl fmt::Display for PipelineTaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for PipelineTaskError {}

impl Pipeline {
    pub fn new(
        root_dir: impl Into<PathBuf>,
        browser: Option<ResolvedBrowserConfig>,
        mode: AgentMode,
    ) -> Result<Self> {
        let http_client_with_redirect = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .redirect(Policy::custom(|attempt| {
                if should_reject_http_redirect_target(attempt.url().as_str()).is_some() {
                    return attempt.stop();
                }
                if attempt.previous().len() >= 10 {
                    return attempt.stop();
                }
                attempt.follow()
            }))
            .build()
            .context("创建 pipeline HTTP 客户端失败(redirect enabled)")?;
        let http_client_no_redirect = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .redirect(Policy::none())
            .build()
            .context("创建 pipeline HTTP 客户端失败(redirect disabled)")?;
        Ok(Self {
            root_dir: root_dir.into(),
            browser,
            mode,
            http_client_with_redirect,
            http_client_no_redirect,
            #[cfg(test)]
            http_test_fixtures: HashMap::new(),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_http_fixture(
        mut self,
        url: impl Into<String>,
        html: impl Into<String>,
    ) -> Self {
        self.http_test_fixtures.insert(url.into(), html.into());
        self
    }

    pub fn process_pending_task(
        &self,
        task: &PendingTaskRecord,
    ) -> std::result::Result<PipelineResult, PipelineTaskError> {
        log_pipeline_info(
            "task_processing_started",
            vec![
                ("task_id", json!(task.task_id)),
                ("article_id", json!(task.article_id)),
                ("url", json!(task.normalized_url)),
            ],
        );
        // restricted 运行时门禁：URL 策略
        let url_decision = check_url(self.mode, &task.normalized_url);
        if !url_decision.allowed {
            log_pipeline_error(
                "url_policy_denied",
                vec![
                    ("task_id", json!(task.task_id)),
                    ("url", json!(task.normalized_url)),
                    ("reason", json!(url_decision.reason.clone())),
                ],
            );
            return Err(PipelineTaskError::failed(url_decision.reason));
        }
        if should_prefer_browser_capture(&task.normalized_url) {
            if let Some(browser) = &self.browser {
                log_pipeline_info(
                    "task_fetch_branch_selected",
                    vec![
                        ("task_id", json!(task.task_id)),
                        ("source", json!("browser_capture")),
                        ("status", json!("selected")),
                    ],
                );
                let capture = self.run_browser_capture(browser, task)?;
                return self.archive_browser_capture(task, &capture).map_err(|err| {
                    log_pipeline_error(
                        "task_failed",
                        vec![
                            ("task_id", json!(task.task_id)),
                            ("source", json!("browser_capture")),
                            ("status", json!("failed")),
                            ("error_kind", json!("browser_archive_failed")),
                            ("detail", json!(err.to_string())),
                        ],
                    );
                    PipelineTaskError::failed(format!("归档浏览器抓取结果失败: {err}"))
                });
            }
        }

        log_pipeline_info(
            "task_fetch_branch_selected",
            vec![
                ("task_id", json!(task.task_id)),
                ("source", json!("http")),
                ("status", json!("selected")),
            ],
        );
        let fetched = self.fetch_html(&task.normalized_url)?;
        self.archive_html(task, &fetched).map_err(|err| {
            log_pipeline_error(
                "task_failed",
                vec![
                    ("task_id", json!(task.task_id)),
                    ("source", json!("http")),
                    ("status", json!("failed")),
                    ("error_kind", json!("html_archive_failed")),
                    ("detail", json!(err.to_string())),
                ],
            );
            PipelineTaskError::failed(format!("归档页面失败: {err}"))
        })
    }

    pub fn archive_manual_content(
        &self,
        task: &TaskContentRecord,
        content: &str,
    ) -> Result<PipelineResult> {
        let day = Utc::now().format("%Y-%m-%d").to_string();
        let raw_dir = self.root_dir.join("raw").join(&day);
        let output_dir = self.root_dir.join("processed").join(day);
        fs::create_dir_all(&raw_dir)
            .with_context(|| format!("创建原始目录失败: {}", raw_dir.display()))?;
        fs::create_dir_all(&output_dir)
            .with_context(|| format!("创建归档目录失败: {}", output_dir.display()))?;

        let raw_path = raw_dir.join(format!("{}.manual.txt", task.task_id));
        let output_path = output_dir.join(format!("{}.md", task.task_id));
        fs::write(&raw_path, content)
            .with_context(|| format!("写入人工正文失败: {}", raw_path.display()))?;

        let title = task
            .title
            .clone()
            .or_else(|| extract_manual_title(content))
            .filter(|v| !v.trim().is_empty());
        let body = content.trim();
        let content = format!(
            "# Archived Link\n\n- task_id: {}\n- article_id: {}\n- normalized_url: {}\n- original_url: {}\n- title: {}\n- archived_at: {}\n- source: manual_input\n\n## Content\n\n{}\n",
            task.task_id,
            task.article_id,
            task.normalized_url,
            task.original_url,
            title.clone().unwrap_or_else(|| "(none)".to_string()),
            Utc::now().to_rfc3339(),
            body,
        );
        fs::write(&output_path, content)
            .with_context(|| format!("写入人工归档文件失败: {}", output_path.display()))?;

        let result = PipelineResult {
            output_path,
            raw_path,
            snapshot_path: None,
            title,
            page_kind: "manual_input".to_string(),
            content_source: "manual_input".to_string(),
            summary: None,
        };
        log_pipeline_info(
            "task_archived",
            vec![
                ("task_id", json!(task.task_id)),
                ("source", json!("manual_input")),
                ("status", json!("archived")),
                (
                    "output_path",
                    json!(result.output_path.display().to_string()),
                ),
            ],
        );
        Ok(result)
    }

    fn fetch_html(&self, url: &str) -> std::result::Result<HttpFetchResult, PipelineTaskError> {
        #[cfg(test)]
        if let Some(html) = self.http_test_fixtures.get(url) {
            return Ok(HttpFetchResult {
                html: html.clone(),
                final_url: url.to_string(),
            });
        }

        log_pipeline_info(
            "http_fetch_started",
            vec![("source", json!("http")), ("url", json!(url))],
        );
        let backoff = [
            Duration::from_millis(200),
            Duration::from_millis(500),
            Duration::from_secs(1),
        ];
        let mut last_err = None;

        for attempt in 0..=backoff.len() {
            match self.fetch_html_once(url, attempt) {
                Ok(result) => return Ok(result),
                Err((err, retryable)) => {
                    last_err = Some(err);
                    if !retryable || attempt >= backoff.len() {
                        break;
                    }
                    log_pipeline_warn(
                        "http_fetch_retry_scheduled",
                        vec![
                            ("source", json!("http")),
                            ("url", json!(url)),
                            ("attempt", json!(attempt + 1)),
                            ("retryable", json!(true)),
                            ("backoff_ms", json!(backoff[attempt].as_millis())),
                        ],
                    );
                    sleep(backoff[attempt]);
                }
            }
        }

        let err = last_err.expect("fetch_html 至少应产生一次错误");
        log_pipeline_error(
            "http_fetch_failed",
            vec![
                ("source", json!("http")),
                ("url", json!(url)),
                ("attempts", json!(backoff.len() + 1)),
                ("error_kind", json!("http_request_failed")),
                ("detail", json!(err.to_string())),
            ],
        );
        Err(err)
    }

    /// 单次 HTTP 抓取尝试。
    /// 返回 Ok(result) 或 Err((error, retryable))。
    fn fetch_html_once(
        &self,
        url: &str,
        attempt: usize,
    ) -> std::result::Result<HttpFetchResult, (PipelineTaskError, bool)> {
        let client = if should_disable_http_redirects(url) {
            &self.http_client_no_redirect
        } else {
            &self.http_client_with_redirect
        };
        let response = match client.get(url).send() {
            Ok(resp) => resp,
            Err(err) => {
                let retryable = err.is_timeout() || err.is_connect();
                log_pipeline_warn(
                    "http_fetch_attempt_failed",
                    vec![
                        ("source", json!("http")),
                        ("url", json!(url)),
                        ("attempt", json!(attempt + 1)),
                        ("retryable", json!(retryable)),
                        ("error_kind", json!("http_request_failed")),
                        ("detail", json!(err.to_string())),
                    ],
                );
                return Err((
                    PipelineTaskError::failed(format!("抓取页面失败: {url} ({err})")),
                    retryable,
                ));
            }
        };

        let final_url = response.url().to_string();
        let status = response.status();

        if status.is_redirection() {
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if let Err(err) = detect_wechat_redirect(location) {
                return Err((err, false));
            }
            let (error_kind, error_msg) =
                if let Some(ssrf_kind) = should_reject_http_redirect_target(location) {
                    (
                        ssrf_kind.to_string(),
                        format!(
                            "安全拦截: redirect 指向私有/本地地址 (status={} target={})",
                            status.as_u16(),
                            location
                        ),
                    )
                } else {
                    (
                        "http_redirect_rejected".to_string(),
                        format!("抓取页面失败: HTTP {} {}", status.as_u16(), url),
                    )
                };
            log_pipeline_warn(
                "http_fetch_attempt_failed",
                vec![
                    ("source", json!("http")),
                    ("url", json!(url)),
                    ("attempt", json!(attempt + 1)),
                    ("retryable", json!(false)),
                    ("error_kind", json!(error_kind)),
                    ("redirect_target", json!(location)),
                ],
            );
            return Err((PipelineTaskError::failed(error_msg), false));
        }

        if !status.is_success() {
            let retryable = status.is_server_error();
            let error_kind = if retryable {
                "http_status_server_error"
            } else {
                "http_status_failed"
            };
            log_pipeline_warn(
                "http_fetch_attempt_failed",
                vec![
                    ("source", json!("http")),
                    ("url", json!(url)),
                    ("attempt", json!(attempt + 1)),
                    ("retryable", json!(retryable)),
                    ("status", json!(status.as_u16())),
                    ("error_kind", json!(error_kind)),
                ],
            );
            return Err((
                PipelineTaskError::failed(format!(
                    "抓取页面失败: HTTP {} {}",
                    status.as_u16(),
                    url
                )),
                retryable,
            ));
        }

        let html = match response.text() {
            Ok(body) => body,
            Err(err) => {
                let retryable = err.is_timeout() || err.is_connect();
                log_pipeline_warn(
                    "http_fetch_attempt_failed",
                    vec![
                        ("source", json!("http")),
                        ("url", json!(url)),
                        ("attempt", json!(attempt + 1)),
                        ("retryable", json!(retryable)),
                        ("error_kind", json!("http_read_body_failed")),
                        ("detail", json!(err.to_string())),
                    ],
                );
                return Err((
                    PipelineTaskError::failed(format!("读取页面正文失败: {err}")),
                    retryable,
                ));
            }
        };

        if let Err(err) = validate_fetched_html(url, &final_url, &html) {
            log_pipeline_warn(
                "http_fetch_attempt_failed",
                vec![
                    ("source", json!("http")),
                    ("url", json!(url)),
                    ("attempt", json!(attempt + 1)),
                    ("retryable", json!(false)),
                    ("error_kind", json!("html_validation_failed")),
                    ("detail", json!(err.to_string())),
                ],
            );
            return Err((err, false));
        }

        log_pipeline_info(
            "http_fetch_finished",
            vec![
                ("source", json!("http")),
                ("url", json!(url)),
                ("final_url", json!(final_url)),
                ("status", json!("ok")),
                ("attempt", json!(attempt + 1)),
                ("html_chars", json!(html.chars().count())),
            ],
        );
        Ok(HttpFetchResult { html, final_url })
    }

    fn run_browser_capture(
        &self,
        browser: &ResolvedBrowserConfig,
        task: &PendingTaskRecord,
    ) -> std::result::Result<BrowserCaptureResult, PipelineTaskError> {
        let day = Utc::now().format("%Y-%m-%d").to_string();
        let raw_dir = self.root_dir.join("raw").join(&day);
        let snapshot_dir = self.root_dir.join("snapshots").join(day);
        fs::create_dir_all(&raw_dir)
            .map_err(|err| PipelineTaskError::failed(format!("创建浏览器 raw 目录失败: {err}")))?;
        fs::create_dir_all(&snapshot_dir).map_err(|err| {
            PipelineTaskError::failed(format!("创建浏览器 snapshot 目录失败: {err}"))
        })?;

        let request = BrowserCaptureRequest {
            url: task.normalized_url.clone(),
            html_path: raw_dir.join(format!("{}.browser.html", task.task_id)),
            screenshot_path: snapshot_dir.join(format!("{}.png", task.task_id)),
            timeout_ms: u64::try_from(browser.timeout.as_millis()).unwrap_or(u64::MAX),
            headless: browser.headless,
            mobile_viewport: browser.mobile_viewport,
        };
        log_pipeline_info(
            "browser_worker_started",
            vec![
                ("task_id", json!(task.task_id)),
                ("source", json!("browser_capture")),
                ("url", json!(task.normalized_url)),
                ("timeout_ms", json!(request.timeout_ms)),
            ],
        );

        let mut child = Command::new(&browser.command)
            .arg(&browser.worker_script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| {
                log_pipeline_error(
                    "browser_worker_failed",
                    vec![
                        ("task_id", json!(task.task_id)),
                        ("source", json!("browser_capture")),
                        ("error_kind", json!("browser_worker_unavailable")),
                        ("detail", json!(err.to_string())),
                    ],
                );
                PipelineTaskError::browser_manual_input(
                    "browser_worker_unavailable",
                    format!("启动浏览器 worker 失败: {err}"),
                    existing_file_path(&request.screenshot_path),
                )
            })?;

        if let Some(mut stdin) = child.stdin.take() {
            let payload = serde_json::to_vec(&request).map_err(|err| {
                log_pipeline_error(
                    "browser_worker_failed",
                    vec![
                        ("task_id", json!(task.task_id)),
                        ("source", json!("browser_capture")),
                        ("error_kind", json!("browser_request_serialize_failed")),
                        ("detail", json!(err.to_string())),
                    ],
                );
                PipelineTaskError::failed(format!("序列化浏览器抓取请求失败: {err}"))
            })?;
            stdin.write_all(&payload).map_err(|err| {
                log_pipeline_error(
                    "browser_worker_failed",
                    vec![
                        ("task_id", json!(task.task_id)),
                        ("source", json!("browser_capture")),
                        ("error_kind", json!("browser_worker_io_failed")),
                        ("detail", json!(err.to_string())),
                    ],
                );
                PipelineTaskError::browser_manual_input(
                    "browser_worker_io_failed",
                    format!("写入浏览器抓取请求失败: {err}"),
                    existing_file_path(&request.screenshot_path),
                )
            })?;
            drop(stdin);
        }

        let worker_deadline = Instant::now() + browser.timeout + Duration::from_secs(15);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if Instant::now() >= worker_deadline {
                        let _ = child.kill();
                        let output = child.wait_with_output().map_err(|err| {
                            log_pipeline_error(
                                "browser_worker_failed",
                                vec![
                                    ("task_id", json!(task.task_id)),
                                    ("source", json!("browser_capture")),
                                    ("error_kind", json!("browser_worker_timeout_kill_failed")),
                                    ("detail", json!(err.to_string())),
                                ],
                            );
                            PipelineTaskError::browser_manual_input(
                                "browser_worker_timeout",
                                format!("终止超时浏览器 worker 失败: {err}"),
                                existing_file_path(&request.screenshot_path),
                            )
                        })?;
                        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                        log_pipeline_error(
                            "browser_worker_failed",
                            vec![
                                ("task_id", json!(task.task_id)),
                                ("source", json!("browser_capture")),
                                ("error_kind", json!("browser_worker_timeout")),
                                (
                                    "detail",
                                    json!(format!(
                                        "stdout={} stderr={}",
                                        summarize_output(&stdout),
                                        summarize_output(&stderr)
                                    )),
                                ),
                            ],
                        );
                        return Err(PipelineTaskError::browser_manual_input(
                            "browser_worker_timeout",
                            format!(
                                "浏览器 worker 超时未结束，已终止。stdout={} stderr={}",
                                summarize_output(&stdout),
                                summarize_output(&stderr)
                            ),
                            existing_file_path(&request.screenshot_path),
                        ));
                    }
                    sleep(Duration::from_millis(200));
                }
                Err(err) => {
                    log_pipeline_error(
                        "browser_worker_failed",
                        vec![
                            ("task_id", json!(task.task_id)),
                            ("source", json!("browser_capture")),
                            ("error_kind", json!("browser_worker_status_poll_failed")),
                            ("detail", json!(err.to_string())),
                        ],
                    );
                    return Err(PipelineTaskError::browser_manual_input(
                        "browser_worker_failed",
                        format!("轮询浏览器 worker 状态失败: {err}"),
                        existing_file_path(&request.screenshot_path),
                    ));
                }
            }
        }

        let output = child.wait_with_output().map_err(|err| {
            log_pipeline_error(
                "browser_worker_failed",
                vec![
                    ("task_id", json!(task.task_id)),
                    ("source", json!("browser_capture")),
                    ("error_kind", json!("browser_worker_wait_failed")),
                    ("detail", json!(err.to_string())),
                ],
            );
            PipelineTaskError::browser_manual_input(
                "browser_worker_failed",
                format!("等待浏览器 worker 结束失败: {err}"),
                existing_file_path(&request.screenshot_path),
            )
        })?;
        let stdout = String::from_utf8(output.stdout).map_err(|err| {
            log_pipeline_error(
                "browser_worker_failed",
                vec![
                    ("task_id", json!(task.task_id)),
                    ("source", json!("browser_capture")),
                    ("error_kind", json!("browser_worker_output_invalid_utf8")),
                    ("detail", json!(err.to_string())),
                ],
            );
            PipelineTaskError::browser_manual_input(
                "browser_worker_invalid_output",
                format!("浏览器 worker 输出非 UTF-8: {err}"),
                existing_file_path(&request.screenshot_path),
            )
        })?;
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

        if !output.status.success() && stdout.trim().is_empty() {
            log_pipeline_error(
                "browser_worker_failed",
                vec![
                    ("task_id", json!(task.task_id)),
                    ("source", json!("browser_capture")),
                    ("error_kind", json!("browser_worker_failed")),
                    (
                        "detail",
                        json!(fallback_reason(&stderr, "unknown").to_string()),
                    ),
                ],
            );
            return Err(PipelineTaskError::browser_manual_input(
                "browser_worker_failed",
                format!(
                    "浏览器 worker 执行失败: {}",
                    fallback_reason(&stderr, "unknown")
                ),
                existing_file_path(&request.screenshot_path),
            ));
        }

        let response: BrowserCaptureResponse =
            serde_json::from_str(stdout.trim()).map_err(|err| {
                log_pipeline_error(
                    "browser_worker_failed",
                    vec![
                        ("task_id", json!(task.task_id)),
                        ("source", json!("browser_capture")),
                        ("error_kind", json!("browser_worker_invalid_output")),
                        (
                            "detail",
                            json!(format!(
                                "parse_error={err}; stdout={}; stderr={}",
                                summarize_output(&stdout),
                                summarize_output(&stderr)
                            )),
                        ),
                    ],
                );
                PipelineTaskError::browser_manual_input(
                    "browser_worker_invalid_output",
                    format!(
                        "解析浏览器 worker 返回失败: {err}; stdout={}; stderr={}",
                        summarize_output(&stdout),
                        summarize_output(&stderr)
                    ),
                    existing_file_path(&request.screenshot_path),
                )
            })?;
        validate_browser_capture_paths(&request, &response)?;

        if response.ok {
            log_pipeline_info(
                "browser_worker_finished",
                vec![
                    ("task_id", json!(task.task_id)),
                    ("source", json!("browser_capture")),
                    ("status", json!("ok")),
                    ("page_kind", json!(response.page_kind)),
                    ("final_url", json!(response.final_url)),
                ],
            );
            return Ok(BrowserCaptureResult {
                page_kind: response.page_kind,
                final_url: response.final_url,
                title: response.title,
                html_path: response.html_path,
                screenshot_path: response.screenshot_path,
            });
        }

        log_pipeline_warn(
            "task_awaiting_manual_input",
            vec![
                ("task_id", json!(task.task_id)),
                ("source", json!("browser_capture")),
                ("status", json!("awaiting_manual_input")),
                ("page_kind", json!(response.page_kind)),
                (
                    "detail",
                    json!(format_browser_failure_message(
                        response.reason.as_deref().unwrap_or("浏览器抓取未成功"),
                        &response.logs,
                        &stderr,
                    )),
                ),
            ],
        );
        Err(PipelineTaskError::browser_manual_input(
            response.page_kind,
            format_browser_failure_message(
                response.reason.as_deref().unwrap_or("浏览器抓取未成功"),
                &response.logs,
                &stderr,
            ),
            existing_file_path(&response.screenshot_path),
        ))
    }

    fn archive_browser_capture(
        &self,
        task: &PendingTaskRecord,
        capture: &BrowserCaptureResult,
    ) -> Result<PipelineResult> {
        let html = fs::read_to_string(&capture.html_path).with_context(|| {
            format!("读取浏览器抓取 HTML 失败: {}", capture.html_path.display())
        })?;
        let day = Utc::now().format("%Y-%m-%d").to_string();
        let output_dir = self.root_dir.join("processed").join(day);
        fs::create_dir_all(&output_dir)
            .with_context(|| format!("创建归档目录失败: {}", output_dir.display()))?;
        let output_path = output_dir.join(format!("{}.md", task.task_id));
        let body = extract_primary_body(&html).unwrap_or_else(|| preview_text(&html));
        let content = format!(
            "# Archived Link\n\n- task_id: {}\n- article_id: {}\n- normalized_url: {}\n- original_url: {}\n- final_url: {}\n- title: {}\n- archived_at: {}\n- source: browser_capture\n- page_kind: {}\n- screenshot_path: {}\n\n## Content\n\n{}\n",
            task.task_id,
            task.article_id,
            task.normalized_url,
            task.original_url,
            capture.final_url,
            capture.title.clone().unwrap_or_else(|| "(none)".to_string()),
            Utc::now().to_rfc3339(),
            capture.page_kind,
            capture.screenshot_path.display(),
            body,
        );
        fs::write(&output_path, content)
            .with_context(|| format!("写入浏览器归档文件失败: {}", output_path.display()))?;

        let result = PipelineResult {
            output_path,
            raw_path: capture.html_path.clone(),
            snapshot_path: Some(capture.screenshot_path.clone()),
            title: capture.title.clone(),
            page_kind: capture.page_kind.clone(),
            content_source: "browser_capture".to_string(),
            summary: None,
        };
        log_pipeline_info(
            "task_archived",
            vec![
                ("task_id", json!(task.task_id)),
                ("source", json!("browser_capture")),
                ("status", json!("archived")),
                ("page_kind", json!(capture.page_kind)),
                (
                    "output_path",
                    json!(result.output_path.display().to_string()),
                ),
                (
                    "snapshot_path",
                    json!(capture.screenshot_path.display().to_string()),
                ),
            ],
        );
        Ok(result)
    }

    fn archive_html(
        &self,
        task: &PendingTaskRecord,
        fetched: &HttpFetchResult,
    ) -> Result<PipelineResult> {
        let day = Utc::now().format("%Y-%m-%d").to_string();
        let raw_dir = self.root_dir.join("raw").join(&day);
        let output_dir = self.root_dir.join("processed").join(day);
        fs::create_dir_all(&raw_dir)
            .with_context(|| format!("创建原始目录失败: {}", raw_dir.display()))?;
        fs::create_dir_all(&output_dir)
            .with_context(|| format!("创建归档目录失败: {}", output_dir.display()))?;

        let raw_path = raw_dir.join(format!("{}.html", task.task_id));
        let output_path = output_dir.join(format!("{}.md", task.task_id));
        fs::write(&raw_path, &fetched.html)
            .with_context(|| format!("写入原始文件失败: {}", raw_path.display()))?;

        let title = extract_html_title(&fetched.html);
        let extracted = extract_http_archive_body(&fetched.html);
        let summary = generate_rule_summary(title.as_deref(), &extracted.markdown);
        let summary_section = match &summary {
            Some(s) => format!("\n## Summary\n\n{}\n", s),
            None => String::new(),
        };
        let content = format!(
            "# Archived Link\n\n- task_id: {}\n- article_id: {}\n- normalized_url: {}\n- original_url: {}\n- final_url: {}\n- title: {}\n- archived_at: {}\n- source: http\n- page_kind: {}\n{}## {}\n\n{}\n",
            task.task_id,
            task.article_id,
            task.normalized_url,
            task.original_url,
            fetched.final_url,
            title.clone().unwrap_or_else(|| "(none)".to_string()),
            Utc::now().to_rfc3339(),
            extracted.page_kind,
            summary_section,
            extracted.section_title,
            extracted.markdown,
        );
        fs::write(&output_path, content)
            .with_context(|| format!("写入归档文件失败: {}", output_path.display()))?;

        let result = PipelineResult {
            output_path,
            raw_path,
            snapshot_path: None,
            title,
            page_kind: extracted.page_kind.clone(),
            content_source: "http".to_string(),
            summary,
        };
        log_pipeline_info(
            "task_archived",
            vec![
                ("task_id", json!(task.task_id)),
                ("source", json!("http")),
                ("status", json!("archived")),
                ("page_kind", json!(result.page_kind)),
                (
                    "output_path",
                    json!(result.output_path.display().to_string()),
                ),
            ],
        );
        Ok(result)
    }
}

fn existing_file_path(path: &Path) -> Option<PathBuf> {
    path.exists().then(|| path.to_path_buf())
}

fn summarize_output(output: &str) -> String {
    let cleaned = output.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() {
        "(empty)".to_string()
    } else {
        let truncated: String = cleaned.chars().take(240).collect();
        if cleaned.chars().count() > 240 {
            format!("{truncated}...")
        } else {
            truncated
        }
    }
}

fn fallback_reason<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}

fn format_browser_failure_message(reason: &str, logs: &[String], stderr: &str) -> String {
    let mut parts = vec![reason.trim().to_string()];
    if !logs.is_empty() {
        parts.push(format!("logs={}", summarize_output(&logs.join(" | "))));
    }
    if !stderr.trim().is_empty() {
        parts.push(format!("stderr={}", summarize_output(stderr)));
    }
    parts.join("; ")
}

fn is_wechat_mp_url(url: &str) -> bool {
    Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.domain().map(|domain| domain == "mp.weixin.qq.com"))
        .unwrap_or(false)
}

fn should_prefer_browser_capture(url: &str) -> bool {
    is_wechat_mp_url(url)
}

fn extract_manual_title(content: &str) -> Option<String> {
    content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(80).collect::<String>())
}

fn should_disable_http_redirects(url: &str) -> bool {
    is_wechat_mp_url(url)
}

fn should_reject_http_redirect_target(url: &str) -> Option<&str> {
    if crate::task_store::is_private_url(url) {
        Some("ssrf_redirect_private_target")
    } else {
        None
    }
}

fn validate_browser_capture_paths(
    request: &BrowserCaptureRequest,
    response: &BrowserCaptureResponse,
) -> std::result::Result<(), PipelineTaskError> {
    if response.html_path != request.html_path
        || response.screenshot_path != request.screenshot_path
    {
        return Err(PipelineTaskError::browser_manual_input(
            "browser_worker_invalid_output",
            format!(
                "浏览器 worker 返回了非预期产物路径: html_path={} expected_html_path={} screenshot_path={} expected_screenshot_path={}",
                response.html_path.display(),
                request.html_path.display(),
                response.screenshot_path.display(),
                request.screenshot_path.display()
            ),
            existing_file_path(&request.screenshot_path),
        ));
    }
    Ok(())
}

fn validate_fetched_html(
    _requested_url: &str,
    final_url: &str,
    html: &str,
) -> std::result::Result<(), PipelineTaskError> {
    detect_wechat_error_page(final_url, html)
}

fn detect_wechat_redirect(location: &str) -> std::result::Result<(), PipelineTaskError> {
    let parsed = Url::parse(location)
        .or_else(|_| Url::parse(&format!("https://mp.weixin.qq.com{location}")))
        .ok();
    let Some(url) = parsed else {
        return Ok(());
    };
    if url.domain() != Some("mp.weixin.qq.com") {
        return Ok(());
    }
    let path = url.path();
    if path.contains("wappoc_appmsgcaptcha") {
        return Err(PipelineTaskError::awaiting_manual_input(
            "wechat_captcha",
            "微信公众号页面需要验证码验证",
        ));
    }
    if path.contains("mp/appmsg/show") || path.contains("mp/profile_ext") {
        return Err(PipelineTaskError::awaiting_manual_input(
            "wechat_permission_denied",
            "微信公众号页面跳转到受限页面",
        ));
    }
    Ok(())
}

fn detect_wechat_error_page(url: &str, html: &str) -> std::result::Result<(), PipelineTaskError> {
    let parsed = Url::parse(url).ok();
    if parsed.as_ref().and_then(Url::domain) != Some("mp.weixin.qq.com") {
        return Ok(());
    }

    let title = extract_html_title(html).unwrap_or_default();
    let text = preview_text(html);

    if title.contains("未知错误") || text.contains("未知错误，请稍后再试") {
        return Err(PipelineTaskError::awaiting_manual_input(
            "wechat_error",
            "微信公众号页面返回错误页",
        ));
    }
    if text.contains("你暂无权限查看此页面内容") || text.contains("失效的验证页面")
    {
        return Err(PipelineTaskError::awaiting_manual_input(
            "wechat_permission_denied",
            "微信公众号页面暂无访问权限",
        ));
    }
    if text.contains("微信公众平台安全验证")
        || text.contains("请输入图中的验证码")
        || text.contains("poc_token")
    {
        return Err(PipelineTaskError::awaiting_manual_input(
            "wechat_captcha",
            "微信公众号页面需要验证码验证",
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::html_extract::{
        classify_http_page_kind, extract_html_title, extract_http_archive_body,
        extract_primary_body, generate_rule_summary, is_navigation_like,
    };
    use super::logging::build_pipeline_log_payload;
    use super::markdown::{
        decode_entities_in_text, html_fragment_to_markdown, replace_pre_blocks,
        restore_code_placeholders, strip_html_tags,
    };
    use super::{
        detect_wechat_error_page, detect_wechat_redirect, should_disable_http_redirects,
        should_prefer_browser_capture, should_reject_http_redirect_target,
        validate_browser_capture_paths, validate_fetched_html, AgentMode, BrowserCaptureRequest,
        BrowserCaptureResponse, BrowserCaptureResult, HttpFetchResult, Pipeline,
        PipelineFailureKind,
    };
    use crate::task_store::{PendingTaskRecord, TaskContentRecord};
    use serde_json::{json, Value};
    use std::fs;
    use uuid::Uuid;

    fn temp_dir() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("amclaw_pipeline_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("创建测试目录失败");
        root
    }

    #[test]
    fn processing_pending_task_creates_markdown_file() {
        let root = temp_dir();
        let pipeline =
            Pipeline::new(&root, None, AgentMode::Restricted).expect("初始化 pipeline 失败");
        let task = PendingTaskRecord {
            task_id: "task-1".to_string(),
            article_id: "article-1".to_string(),
            normalized_url: "https://example.com".to_string(),
            original_url: "https://example.com".to_string(),
        };

        let result = pipeline
            .archive_html(
                &task,
                &HttpFetchResult {
                    html:
                        "<html><head><title>Hello World</title></head><body>content</body></html>"
                            .to_string(),
                    final_url: "https://example.com".to_string(),
                },
            )
            .expect("处理 pending 任务失败");
        let content = fs::read_to_string(&result.output_path).expect("读取归档文件失败");
        let raw = fs::read_to_string(&result.raw_path).expect("读取原始文件失败");

        assert!(result.output_path.starts_with(root.join("processed")));
        assert!(result.raw_path.starts_with(root.join("raw")));
        assert!(content.contains("task-1"));
        assert!(content.contains("https://example.com"));
        assert!(content.contains("Hello World"));
        assert!(content.contains("source: http"));
        assert!(content.contains("page_kind: webpage"));
        assert_eq!(result.title, Some("Hello World".to_string()));
        assert_eq!(result.page_kind, "webpage");
        assert_eq!(result.content_source, "http");
        assert!(raw.contains("<title>Hello World</title>"));
    }

    #[test]
    fn http_archive_extracts_generic_article_body() {
        let html = r#"
<html>
  <head>
    <title>通用文章标题</title>
    <meta property="og:type" content="article" />
  </head>
  <body>
    <nav>导航</nav>
    <article>
      <h1>通用文章标题</h1>
      <p>第一段正文，包含足够的信息用于形成可读归档。</p>
      <p><a href="https://example.com/ref">相关阅读</a></p>
      <img src="https://example.com/image.png" />
    </article>
  </body>
</html>
"#;

        let body = extract_http_archive_body(html);

        assert_eq!(body.page_kind, "article");
        assert_eq!(body.section_title, "Content");
        assert!(body.markdown.contains("第一段正文"));
        assert!(body.markdown.contains("相关阅读 (https://example.com/ref)"));
        assert!(body
            .markdown
            .contains("![image](https://example.com/image.png)"));
        assert!(!body.markdown.contains("导航"));
    }

    #[test]
    fn title_extraction_handles_missing_title() {
        assert_eq!(
            extract_html_title("<html><body>no title</body></html>"),
            None
        );
    }

    #[test]
    fn manual_content_creates_archive() {
        let root = temp_dir();
        let pipeline =
            Pipeline::new(&root, None, AgentMode::Restricted).expect("初始化 pipeline 失败");
        let task = TaskContentRecord {
            task_id: "task-manual".to_string(),
            article_id: "article-1".to_string(),
            normalized_url: "https://mp.weixin.qq.com/s/abc".to_string(),
            original_url: "https://mp.weixin.qq.com/s/abc".to_string(),
            title: None,
        };

        let result = pipeline
            .archive_manual_content(&task, "这是人工补录的正文\n第二行")
            .expect("人工补录归档失败");
        let content = fs::read_to_string(&result.output_path).expect("读取归档文件失败");

        assert!(content.contains("source: manual_input"));
        assert!(content.contains("这是人工补录的正文"));
        assert_eq!(result.title, Some("这是人工补录的正文".to_string()));
    }

    #[test]
    fn wechat_error_page_is_rejected() {
        let err = detect_wechat_error_page(
            "https://mp.weixin.qq.com/s/abc",
            "<html><head><title>未知错误</title></head><body>你暂无权限查看此页面内容。</body></html>",
        )
        .expect_err("应识别公众号错误页");

        assert!(matches!(
            err.kind,
            PipelineFailureKind::AwaitingManualInput { .. }
        ));
    }

    #[test]
    fn redirected_wechat_error_page_is_detected_by_final_url() {
        let err = validate_fetched_html(
            "https://short.example/abc",
            "https://mp.weixin.qq.com/s/abc",
            "<html><head><title>未知错误</title></head><body>你暂无权限查看此页面内容。</body></html>",
        )
        .expect_err("应按最终 URL 识别公众号错误页");

        assert!(matches!(
            err.kind,
            PipelineFailureKind::AwaitingManualInput { .. }
        ));
    }

    #[test]
    fn wechat_captcha_redirect_is_rejected() {
        let err = detect_wechat_redirect(
            "https://mp.weixin.qq.com/mp/wappoc_appmsgcaptcha?poc_token=abc&target_url=https%3A%2F%2Fmp.weixin.qq.com%2Fs%2Fxyz",
        )
        .expect_err("应识别公众号验证码跳转");

        assert!(matches!(
            err.kind,
            PipelineFailureKind::AwaitingManualInput { .. }
        ));
    }

    #[test]
    fn non_wechat_urls_allow_http_redirects() {
        assert!(!should_disable_http_redirects(
            "https://example.com/redirect"
        ));
        assert!(!should_disable_http_redirects("https://x.com/t/abc"));
    }

    #[test]
    fn wechat_urls_keep_redirect_guard() {
        assert!(should_disable_http_redirects(
            "https://mp.weixin.qq.com/s/demo"
        ));
        assert!(!should_disable_http_redirects(
            "https://example.com/article"
        ));
    }

    #[test]
    fn http_redirect_to_private_target_is_rejected() {
        let result = should_reject_http_redirect_target("http://169.254.169.254/latest/meta-data/");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "ssrf_redirect_private_target");

        let result = should_reject_http_redirect_target("http://127.0.0.1/admin");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "ssrf_redirect_private_target");

        let result = should_reject_http_redirect_target("http://[::1]/secret");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "ssrf_redirect_private_target");

        assert!(should_reject_http_redirect_target("https://example.com/article").is_none());
    }

    #[test]
    fn wechat_domain_prefers_browser_capture() {
        assert!(should_prefer_browser_capture(
            "https://mp.weixin.qq.com/s/YUvXg9i31QuQN6t-zRTe8g"
        ));
        assert!(!should_prefer_browser_capture(
            "https://example.com/article"
        ));
    }

    #[test]
    fn pipeline_log_payload_keeps_contract_fields() {
        let payload = build_pipeline_log_payload(
            "warn",
            "task_awaiting_manual_input",
            vec![
                ("task_id", json!("task-1")),
                ("source", json!("browser_capture")),
                ("detail", Value::Null),
            ],
        );

        assert_eq!(payload["level"], "warn");
        assert_eq!(payload["event"], "task_awaiting_manual_input");
        assert_eq!(payload["task_id"], "task-1");
        assert_eq!(payload["source"], "browser_capture");
        assert!(payload.get("ts").is_some());
        assert!(payload.get("detail").is_none());
    }

    #[test]
    fn primary_body_extracts_wechat_title_and_content() {
        let html = r#"
<html>
  <body>
    <h1 id="activity-name">公众号标题</h1>
    <div id="js_content">
      <p>第一段内容</p>
      <p>第二段内容</p>
    </div>
  </body>
</html>
"#;

        let body = extract_primary_body(html).expect("应提取出正文");

        assert!(body.contains("## 公众号标题"));
        assert!(body.contains("第一段内容"));
        assert!(body.contains("第二段内容"));
    }

    #[test]
    fn browser_capture_archive_uses_extracted_body() {
        let root = temp_dir();
        let pipeline =
            Pipeline::new(&root, None, AgentMode::Restricted).expect("初始化 pipeline 失败");
        let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let raw_dir = root.join("raw").join(&day);
        let output_dir = root.join("processed").join(day);
        std::fs::create_dir_all(&raw_dir).expect("创建 raw 目录失败");
        std::fs::create_dir_all(&output_dir).expect("创建 processed 目录失败");

        let html_path = raw_dir.join("task-browser.browser.html");
        let screenshot_path = root.join("snapshots").join("shot.png");
        std::fs::create_dir_all(screenshot_path.parent().expect("应有父目录"))
            .expect("创建 screenshot 目录失败");
        std::fs::write(
            &html_path,
            r#"<html><body><h1 id="activity-name">公众号标题</h1><div id="js_content"><p>正文第一段</p></div></body></html>"#,
        )
        .expect("写 HTML 失败");
        std::fs::write(&screenshot_path, "fake").expect("写 screenshot 失败");

        let task = PendingTaskRecord {
            task_id: "task-browser".to_string(),
            article_id: "article-browser".to_string(),
            normalized_url: "https://mp.weixin.qq.com/s/demo".to_string(),
            original_url: "https://mp.weixin.qq.com/s/demo".to_string(),
        };
        let capture = BrowserCaptureResult {
            page_kind: "article".to_string(),
            final_url: "https://mp.weixin.qq.com/s/demo".to_string(),
            title: Some("公众号标题".to_string()),
            html_path,
            screenshot_path,
        };

        let result = pipeline
            .archive_browser_capture(&task, &capture)
            .expect("归档浏览器抓取结果失败");
        let content = std::fs::read_to_string(result.output_path).expect("读取归档文件失败");

        assert!(content.contains("## 公众号标题"));
        assert!(content.contains("正文第一段"));
        assert!(content.contains("source: browser_capture"));
    }

    #[test]
    fn browser_capture_response_paths_must_match_request() {
        let request = BrowserCaptureRequest {
            url: "https://mp.weixin.qq.com/s/demo".to_string(),
            html_path: std::path::PathBuf::from("/tmp/expected.html"),
            screenshot_path: std::path::PathBuf::from("/tmp/expected.png"),
            timeout_ms: 30_000,
            headless: true,
            mobile_viewport: true,
        };
        let response = BrowserCaptureResponse {
            ok: true,
            page_kind: "article".to_string(),
            final_url: "https://mp.weixin.qq.com/s/demo".to_string(),
            title: Some("标题".to_string()),
            html_path: std::path::PathBuf::from("/tmp/other.html"),
            screenshot_path: std::path::PathBuf::from("/tmp/other.png"),
            reason: None,
            logs: Vec::new(),
        };

        let err = validate_browser_capture_paths(&request, &response)
            .expect_err("应拒绝非预期的浏览器产物路径");

        assert!(matches!(
            err.kind,
            PipelineFailureKind::AwaitingManualInput { .. }
        ));
        assert!(err.message.contains("非预期产物路径"));
    }

    #[test]
    fn primary_body_keeps_links_and_images() {
        let html = r#"
<html>
  <body>
    <h1 id="activity-name">公众号标题</h1>
    <div id="js_content">
      <p>正文第一段</p>
      <p><a href="https://example.com/link">相关阅读</a></p>
      <img data-src="https://example.com/image.jpg" />
    </div>
  </body>
</html>
"#;

        let body = extract_primary_body(html).expect("应提取出正文");

        assert!(body.contains("相关阅读 (https://example.com/link)"));
        assert!(body.contains("![image](https://example.com/image.jpg)"));
    }

    #[test]
    fn primary_body_keeps_links_and_images_with_single_quoted_attrs() {
        let html = r#"
<html>
  <body>
    <h1 id='activity-name'>公众号标题</h1>
    <div id='js_content'>
      <p>正文第一段</p>
      <p><a href='https://example.com/link'>相关阅读</a></p>
      <img data-src='https://example.com/image.jpg' />
    </div>
  </body>
</html>
"#;

        let body = extract_primary_body(html).expect("应提取出正文");

        assert!(body.contains("相关阅读 (https://example.com/link)"));
        assert!(body.contains("![image](https://example.com/image.jpg)"));
    }

    #[test]
    fn rule_summary_generates_from_title_and_paragraphs() {
        let markdown = "这是第一段有效正文，包含了足够多的字符来通过过滤条件。\n\n这是第二段有效正文，同样有足够的字符来通过过滤。\n\n这是第三段有效正文。";

        let summary = generate_rule_summary(Some("文章标题"), markdown).expect("应生成摘要");

        assert!(summary.contains("文章标题"));
        assert!(summary.contains("这是第一段有效正文"));
        assert!(summary.contains("这是第二段有效正文"));
    }

    #[test]
    fn rule_summary_skips_short_paragraphs() {
        let markdown = "短\n\n这是第一段有效正文，包含了足够多的字符来通过过滤条件。";

        let summary = generate_rule_summary(None, markdown).expect("应生成摘要");

        assert!(!summary.contains("短"));
        assert!(summary.contains("这是第一段有效正文"));
    }

    #[test]
    fn rule_summary_filters_navigation_text() {
        assert!(is_navigation_like("版权所有 保留一切权利"));
        assert!(is_navigation_like("https://example.com"));
        assert!(!is_navigation_like(
            "这是一段正常的文章正文内容，用来测试过滤逻辑是否正确工作"
        ));
    }

    #[test]
    fn rule_summary_returns_none_for_empty_input() {
        assert_eq!(generate_rule_summary(None, ""), None);
        assert_eq!(generate_rule_summary(None, "短文本"), None);
    }

    #[test]
    fn http_archive_includes_summary_section() {
        let root = temp_dir();
        let pipeline =
            Pipeline::new(&root, None, AgentMode::Restricted).expect("初始化 pipeline 失败");
        let task = PendingTaskRecord {
            task_id: "task-summary".to_string(),
            article_id: "article-1".to_string(),
            normalized_url: "https://example.com/article".to_string(),
            original_url: "https://example.com/article".to_string(),
        };

        let html = r#"<html><head><title>测试文章</title><meta property="og:type" content="article" /></head><body><article><p>这是第一段正文内容，包含了足够多的信息用于形成可读归档和有效摘要。</p><p>这是第二段正文内容，进一步补充了文章的核心观点和论述。</p></article></body></html>"#;

        let result = pipeline
            .archive_html(
                &task,
                &HttpFetchResult {
                    html: html.to_string(),
                    final_url: "https://example.com/article".to_string(),
                },
            )
            .expect("处理失败");

        let content = fs::read_to_string(&result.output_path).expect("读取归档失败");
        assert!(content.contains("## Summary"));
        assert!(content.contains("测试文章"));
        assert!(content.contains("这是第一段正文内容"));
        assert!(result.summary.is_some());
    }

    #[test]
    fn page_kind_error_page_short_with_error_title() {
        let html = "<html><head><title>404 Not Found</title></head><body><p>The page was not found.</p></body></html>";
        let markdown = "The page was not found.";
        assert_eq!(classify_http_page_kind(html, markdown), "error_page");
    }

    #[test]
    fn page_kind_error_page_chinese() {
        let html = "<html><head><title>页面不存在</title></head><body>抱歉，您访问的页面不存在。</body></html>";
        let markdown = "抱歉，您访问的页面不存在。";
        assert_eq!(classify_http_page_kind(html, markdown), "error_page");
    }

    #[test]
    fn page_kind_long_article_about_404_not_classified_as_error() {
        let mut body = String::from("这是一篇关于HTTP 404错误的技术文章。");
        for i in 0..20 {
            body.push_str(&format!("\n\n第{}段正文内容，用于增加文章长度确保超过3000字符的阈值。这段文字会不断重复以填满空间。", i));
        }
        let html = format!(
            "<html><head><title>深入理解HTTP 404错误</title></head><body><article>{}</article></body></html>",
            body
        );
        assert_eq!(classify_http_page_kind(&html, &body), "article");
    }

    #[test]
    fn page_kind_index_like_many_links_little_prose() {
        let mut links = String::new();
        for i in 0..15 {
            links.push_str(&format!("<a href=\"/item/{}\">链接{}</a>\n", i, i));
        }
        let html = format!(
            "<html><head><title>文章列表</title></head><body>{}</body></html>",
            links
        );
        let markdown = "简短描述";
        assert_eq!(classify_http_page_kind(&html, markdown), "index_like");
    }

    #[test]
    fn page_kind_index_like_many_list_items() {
        let mut items = String::new();
        for i in 0..10 {
            items.push_str(&format!("<li><a href=\"/p/{}\">帖子{}</a></li>\n", i, i));
        }
        let html = format!(
            "<html><head><title>最新帖子</title></head><body><ul>{}</ul></body></html>",
            items
        );
        let markdown = "帖子0 帖子1 帖子2";
        assert_eq!(classify_http_page_kind(&html, markdown), "index_like");
    }

    #[test]
    fn page_kind_link_post_short_with_link() {
        let html = "<html><head><title>分享</title></head><body><p>推荐阅读 https://example.com/great-article</p></body></html>";
        let markdown = "推荐阅读 https://example.com/great-article";
        assert_eq!(classify_http_page_kind(html, markdown), "link_post");
    }

    #[test]
    fn page_kind_article_keeps_existing_logic() {
        let html = r#"<html><head><title>技术博客</title><meta property="og:type" content="article" /></head><body><article><p>正文段落一，包含足够多的内容。</p><p>正文段落二，继续补充论点。</p><p>正文段落三，总结全文。</p></article></body></html>"#;
        let markdown = "正文段落一，包含足够多的内容。\n\n正文段落二，继续补充论点。\n\n正文段落三，总结全文。";
        assert_eq!(classify_http_page_kind(html, markdown), "article");
    }

    #[test]
    fn page_kind_error_page_detected_without_primary_body() {
        // No <article>/<main>/body content extraction, but error title should still classify
        let html = "<html><head><title>404 Not Found</title></head><body><div>Error occurred.</div></body></html>";
        let body = extract_http_archive_body(html);
        assert_eq!(body.page_kind, "error_page");
    }

    #[test]
    fn page_kind_webpage_fallback() {
        let html = "<html><head><title>Hello World</title></head><body>content</body></html>";
        let markdown = "content";
        assert_eq!(classify_http_page_kind(html, markdown), "webpage");
    }

    // --- replace_pre_blocks / html_fragment_to_markdown tests ---

    #[test]
    fn strip_html_tags_removes_all_tags() {
        assert_eq!(
            strip_html_tags("<span>hello</span> <b>world</b>"),
            "hello world"
        );
        assert_eq!(strip_html_tags("no tags here"), "no tags here");
    }

    #[test]
    fn decode_entities_converts_common() {
        assert_eq!(decode_entities_in_text("a&amp;b&nbsp;c"), "a&b c");
        assert_eq!(decode_entities_in_text("&lt;code&gt;"), "<code>");
    }

    #[test]
    fn pre_block_converts_to_code_fence() {
        let html = "<div>text before</div><pre><code>fn main() {</code><code>    println!(\"hello\");</code><code>}</code></pre><div>text after</div>";
        let (result, blocks) = replace_pre_blocks(html);
        let restored = restore_code_placeholders(&result, &blocks);
        assert!(
            restored.contains("```\nfn main() {\nprintln!(\"hello\");\n}\n```"),
            "got: {restored}"
        );
    }

    #[test]
    fn pre_block_wechat_code_snippets() {
        // Simulates WeChat article code block HTML with span-based syntax highlighting
        let html = "<pre><code><span class=\"code-snippet__keyword\">void</span>&nbsp;PrebuiltObjC::<span class=\"code-snippet__title\">generateHashTables</span>(RuntimeState&amp; state)</code><code><span>{</span></code><code>&nbsp;&nbsp;<span class=\"code-snippet__comment\">// comment</span></code></pre>";
        let (result, blocks) = replace_pre_blocks(html);
        let restored = restore_code_placeholders(&result, &blocks);
        assert!(
            restored.contains("void PrebuiltObjC::generateHashTables(RuntimeState& state)"),
            "got: {restored}"
        );
        assert!(restored.contains("// comment"), "got: {restored}");
        assert!(restored.contains("```"), "got: {restored}");
    }

    #[test]
    fn html_fragment_to_markdown_preserves_code_block() {
        let html = "<p>See the code:</p><pre><code>let x = 1;</code><code>let y = 2;</code></pre><p>End</p>";
        let md = html_fragment_to_markdown(html);
        assert!(
            md.contains("let x = 1;") && md.contains("let y = 2;"),
            "code content should be present, got: {md}"
        );
        assert!(md.contains("```"), "should contain code fence, got: {md}");
    }
}
