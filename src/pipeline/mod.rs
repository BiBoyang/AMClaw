use crate::config::ResolvedBrowserConfig;
use crate::task_store::{PendingTaskRecord, TaskContentRecord};
use anyhow::{Context, Result};
use chrono::Utc;
use reqwest::blocking::Client;
use reqwest::header::LOCATION;
use reqwest::redirect::Policy;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct Pipeline {
    root_dir: PathBuf,
    browser: Option<ResolvedBrowserConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineResult {
    pub output_path: PathBuf,
    pub raw_path: PathBuf,
    pub snapshot_path: Option<PathBuf>,
    pub title: Option<String>,
    pub page_kind: String,
    pub content_source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpFetchResult {
    html: String,
    final_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExtractedArchiveBody {
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
    pub fn new(root_dir: impl Into<PathBuf>, browser: Option<ResolvedBrowserConfig>) -> Self {
        Self {
            root_dir: root_dir.into(),
            browser,
        }
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
        log_pipeline_info(
            "http_fetch_started",
            vec![("source", json!("http")), ("url", json!(url))],
        );
        let redirect_policy = if should_disable_http_redirects(url) {
            Policy::none()
        } else {
            Policy::limited(10)
        };
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .redirect(redirect_policy)
            .build()
            .map_err(|err| {
                log_pipeline_error(
                    "http_fetch_failed",
                    vec![
                        ("source", json!("http")),
                        ("url", json!(url)),
                        ("error_kind", json!("http_client_build_failed")),
                        ("detail", json!(err.to_string())),
                    ],
                );
                PipelineTaskError::failed(format!("创建 pipeline HTTP 客户端失败: {err}"))
            })?;
        let response = client.get(url).send().map_err(|err| {
            log_pipeline_error(
                "http_fetch_failed",
                vec![
                    ("source", json!("http")),
                    ("url", json!(url)),
                    ("error_kind", json!("http_request_failed")),
                    ("detail", json!(err.to_string())),
                ],
            );
            PipelineTaskError::failed(format!("抓取页面失败: {url} ({err})"))
        })?;
        let final_url = response.url().to_string();
        let status = response.status();
        if status.is_redirection() {
            if let Some(location) = response
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
            {
                detect_wechat_redirect(location)?;
            }
            log_pipeline_error(
                "http_fetch_failed",
                vec![
                    ("source", json!("http")),
                    ("url", json!(url)),
                    ("status", json!(status.as_u16())),
                    ("error_kind", json!("http_redirect_rejected")),
                ],
            );
            return Err(PipelineTaskError::failed(format!(
                "抓取页面失败: HTTP {} {}",
                status.as_u16(),
                url
            )));
        }
        if !status.is_success() {
            log_pipeline_error(
                "http_fetch_failed",
                vec![
                    ("source", json!("http")),
                    ("url", json!(url)),
                    ("status", json!(status.as_u16())),
                    ("error_kind", json!("http_status_failed")),
                ],
            );
            return Err(PipelineTaskError::failed(format!(
                "抓取页面失败: HTTP {} {}",
                status.as_u16(),
                url
            )));
        }
        let html = response.text().map_err(|err| {
            log_pipeline_error(
                "http_fetch_failed",
                vec![
                    ("source", json!("http")),
                    ("url", json!(url)),
                    ("error_kind", json!("http_read_body_failed")),
                    ("detail", json!(err.to_string())),
                ],
            );
            PipelineTaskError::failed(format!("读取页面正文失败: {err}"))
        })?;
        validate_fetched_html(url, &final_url, &html)?;
        log_pipeline_info(
            "http_fetch_finished",
            vec![
                ("source", json!("http")),
                ("url", json!(url)),
                ("final_url", json!(final_url)),
                ("status", json!("ok")),
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
        let content = format!(
            "# Archived Link\n\n- task_id: {}\n- article_id: {}\n- normalized_url: {}\n- original_url: {}\n- final_url: {}\n- title: {}\n- archived_at: {}\n- source: http\n- page_kind: {}\n\n## {}\n\n{}\n",
            task.task_id,
            task.article_id,
            task.normalized_url,
            task.original_url,
            fetched.final_url,
            title.clone().unwrap_or_else(|| "(none)".to_string()),
            Utc::now().to_rfc3339(),
            extracted.page_kind,
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

fn log_pipeline_info(event: &str, fields: Vec<(&str, Value)>) {
    log_pipeline_event("info", event, fields);
}

fn log_pipeline_warn(event: &str, fields: Vec<(&str, Value)>) {
    log_pipeline_event("warn", event, fields);
}

fn log_pipeline_error(event: &str, fields: Vec<(&str, Value)>) {
    log_pipeline_event("error", event, fields);
}

fn log_pipeline_event(level: &str, event: &str, fields: Vec<(&str, Value)>) {
    crate::logging::emit_structured_log(level, event, fields);
}

#[cfg(test)]
fn build_pipeline_log_payload(level: &str, event: &str, fields: Vec<(&str, Value)>) -> Value {
    crate::logging::build_structured_log_payload(level, event, fields)
}

fn extract_html_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title>")?;
    let end = lower[start + 7..].find("</title>")?;
    let raw = &html[start + 7..start + 7 + end];
    let title = raw.replace('\n', " ").replace('\r', " ");
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

fn preview_text(html: &str) -> String {
    let text = html
        .replace('\n', " ")
        .replace('\r', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut preview: String = text.chars().take(240).collect();
    if text.chars().count() > 240 {
        preview.push_str("...");
    }
    preview
}

fn extract_primary_body(html: &str) -> Option<String> {
    if let Some(wechat_body) = extract_wechat_primary_body(html) {
        return Some(wechat_body);
    }

    extract_generic_primary_body(html)
}

fn extract_wechat_primary_body(html: &str) -> Option<String> {
    let activity_title = extract_element_text_by_id(html, "activity-name");
    let body = extract_element_inner_html_by_id(html, "js_content")
        .map(|fragment| html_fragment_to_markdown(&fragment))
        .filter(|text| !text.trim().is_empty());

    match (activity_title, body) {
        (Some(title), Some(body)) => Some(format!("## {}\n\n{}", title.trim(), body.trim())),
        (None, Some(body)) => Some(body.trim().to_string()),
        (Some(title), None) => Some(format!("## {}", title.trim())),
        (None, None) => None,
    }
}

fn extract_generic_primary_body(html: &str) -> Option<String> {
    for tag in ["article", "main"] {
        if let Some(body) = extract_element_inner_html_by_tag(html, tag)
            .map(|fragment| html_fragment_to_markdown(&fragment))
            .map(|text| text.trim().to_string())
            .filter(|text| is_meaningful_extracted_body(text))
        {
            return Some(body);
        }
    }

    extract_element_inner_html_by_tag(html, "body")
        .map(|fragment| html_fragment_to_markdown(&fragment))
        .map(|text| text.trim().to_string())
        .filter(|text| is_meaningful_extracted_body(text))
}

fn extract_element_text_by_id(html: &str, element_id: &str) -> Option<String> {
    extract_element_inner_html_by_id(html, element_id)
        .map(|fragment| html_fragment_to_markdown(&fragment))
}

fn extract_element_inner_html_by_id(html: &str, element_id: &str) -> Option<String> {
    let id_patterns = [format!("id=\"{element_id}\""), format!("id='{element_id}'")];
    let mut start_idx = None;
    for pattern in id_patterns {
        if let Some(found) = html.find(&pattern) {
            start_idx = Some(found);
            break;
        }
    }
    let start_idx = start_idx?;
    let tag_open_start = html[..start_idx].rfind('<')?;
    let tag_open_end = html[start_idx..].find('>')? + start_idx;
    let tag_name = html[tag_open_start + 1..]
        .split_whitespace()
        .next()?
        .trim_start_matches('/')
        .trim_end_matches('>');
    let close_tag = format!("</{tag_name}>");
    let content_start = tag_open_end + 1;
    let content_end = html[content_start..].find(&close_tag)? + content_start;
    html.get(content_start..content_end).map(ToOwned::to_owned)
}

fn extract_element_inner_html_by_tag(html: &str, tag_name: &str) -> Option<String> {
    let open_pattern = format!("<{tag_name}");
    let start_idx = html.to_ascii_lowercase().find(&open_pattern)?;
    let tag_open_end = html[start_idx..].find('>')? + start_idx;
    let close_tag = format!("</{tag_name}>");
    let content_start = tag_open_end + 1;
    let content_end = html[content_start..]
        .to_ascii_lowercase()
        .find(&close_tag)?
        + content_start;
    html.get(content_start..content_end).map(ToOwned::to_owned)
}

fn extract_http_archive_body(html: &str) -> ExtractedArchiveBody {
    if let Some(markdown) = extract_primary_body(html) {
        let page_kind = classify_http_page_kind(html, &markdown);
        return ExtractedArchiveBody {
            markdown,
            page_kind,
            section_title: "Content",
        };
    }

    ExtractedArchiveBody {
        markdown: preview_text(html),
        page_kind: "webpage".to_string(),
        section_title: "Preview",
    }
}

fn classify_http_page_kind(html: &str, markdown: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let paragraph_count = markdown.matches("\n\n").count() + 1;
    let body_chars = markdown.chars().count();
    let looks_like_article = lower.contains("<article")
        || lower.contains("property=\"og:type\" content=\"article")
        || lower.contains("property='og:type' content='article")
        || lower.contains("name=\"twitter:card\" content=\"summary_large_image")
        || paragraph_count >= 3
        || body_chars >= 400;

    if looks_like_article {
        "article".to_string()
    } else {
        "webpage".to_string()
    }
}

fn is_meaningful_extracted_body(text: &str) -> bool {
    let non_empty_lines = text.lines().filter(|line| !line.trim().is_empty()).count();
    let chars = text.chars().count();
    chars >= 80 || non_empty_lines >= 3
}

fn html_fragment_to_markdown(fragment: &str) -> String {
    let fragment = replace_anchor_blocks(fragment);
    let fragment = replace_img_tags(&fragment);
    normalize_fragment_text(&fragment)
}

fn normalize_fragment_text(fragment: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    let mut in_entity = false;
    let mut entity = String::new();

    for ch in fragment.chars() {
        if in_tag {
            if ch == '>' {
                in_tag = false;
                out.push('\n');
            }
            continue;
        }
        if in_entity {
            if ch == ';' {
                out.push_str(decode_html_entity(&entity));
                entity.clear();
                in_entity = false;
            } else {
                entity.push(ch);
            }
            continue;
        }
        match ch {
            '<' => in_tag = true,
            '&' => in_entity = true,
            _ => out.push(ch),
        }
    }

    out.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn replace_anchor_blocks(fragment: &str) -> String {
    let mut out = String::new();
    let mut cursor = 0;

    while let Some(rel_start) = fragment[cursor..].find("<a") {
        let start = cursor + rel_start;
        out.push_str(&fragment[cursor..start]);

        let Some(rel_tag_end) = fragment[start..].find('>') else {
            out.push_str(&fragment[start..]);
            return out;
        };
        let tag_end = start + rel_tag_end;
        let tag = &fragment[start..=tag_end];
        let href = extract_attribute_value(tag, "href");
        let Some(rel_close) = fragment[tag_end + 1..].find("</a>") else {
            out.push_str(&fragment[start..]);
            return out;
        };
        let close = tag_end + 1 + rel_close;
        let inner = &fragment[tag_end + 1..close];
        let text = normalize_fragment_text(inner);

        if let Some(href) = href.filter(|href| !href.trim().is_empty()) {
            if text.is_empty() {
                out.push_str(&format!("\n\n{href}\n\n"));
            } else {
                out.push_str(&format!("\n\n{text} ({href})\n\n"));
            }
        } else {
            out.push_str(inner);
        }

        cursor = close + 4;
    }

    out.push_str(&fragment[cursor..]);
    out
}

fn replace_img_tags(fragment: &str) -> String {
    let mut out = String::new();
    let mut cursor = 0;

    while let Some(rel_start) = fragment[cursor..].find("<img") {
        let start = cursor + rel_start;
        out.push_str(&fragment[cursor..start]);

        let Some(rel_end) = fragment[start..].find('>') else {
            out.push_str(&fragment[start..]);
            return out;
        };
        let end = start + rel_end;
        let tag = &fragment[start..=end];
        let src = extract_attribute_value(tag, "data-src")
            .or_else(|| extract_attribute_value(tag, "src"))
            .unwrap_or_default();

        if !src.trim().is_empty() {
            out.push_str(&format!("\n\n![image]({src})\n\n"));
        }

        cursor = end + 1;
    }

    out.push_str(&fragment[cursor..]);
    out
}

fn extract_attribute_value(tag: &str, attr: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let pattern = format!("{attr}={quote}");
        let start = tag.find(&pattern)? + pattern.len();
        let end = tag[start..].find(quote)? + start;
        return Some(tag[start..end].to_string());
    }
    None
}

fn existing_file_path(path: &PathBuf) -> Option<PathBuf> {
    path.exists().then(|| path.clone())
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

fn decode_html_entity(entity: &str) -> &str {
    match entity {
        "nbsp" => " ",
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "#39" => "'",
        _ => "",
    }
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
    use super::{
        build_pipeline_log_payload, detect_wechat_error_page, detect_wechat_redirect,
        extract_html_title, extract_http_archive_body, extract_primary_body,
        should_disable_http_redirects, should_prefer_browser_capture,
        validate_browser_capture_paths, validate_fetched_html, BrowserCaptureRequest,
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
        let pipeline = Pipeline::new(&root, None);
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
        let pipeline = Pipeline::new(&root, None);
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
        let pipeline = Pipeline::new(&root, None);
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
}
