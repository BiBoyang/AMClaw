use crate::config::ResolvedBrowserConfig;
use crate::task_store::{PendingTaskRecord, TaskContentRecord};
use anyhow::{Context, Result};
use chrono::Utc;
use reqwest::blocking::Client;
use reqwest::header::LOCATION;
use reqwest::redirect::Policy;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

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
}

impl PipelineTaskError {
    fn failed(message: impl Into<String>) -> Self {
        Self {
            kind: PipelineFailureKind::Failed,
            message: message.into(),
        }
    }

    fn awaiting_manual_input(page_kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: PipelineFailureKind::AwaitingManualInput {
                page_kind: page_kind.into(),
            },
            message: message.into(),
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
        if should_prefer_browser_capture(&task.normalized_url) {
            if let Some(browser) = &self.browser {
                let capture = self.run_browser_capture(browser, task)?;
                return self.archive_browser_capture(task, &capture).map_err(|err| {
                    PipelineTaskError::failed(format!("归档浏览器抓取结果失败: {err}"))
                });
            }
        }

        let html = self.fetch_html(&task.normalized_url)?;
        self.archive_html(task, &html)
            .map_err(|err| PipelineTaskError::failed(format!("归档页面失败: {err}")))
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

        Ok(PipelineResult {
            output_path,
            raw_path,
            snapshot_path: None,
            title,
        })
    }

    fn fetch_html(&self, url: &str) -> std::result::Result<String, PipelineTaskError> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .redirect(Policy::none())
            .build()
            .map_err(|err| {
                PipelineTaskError::failed(format!("创建 pipeline HTTP 客户端失败: {err}"))
            })?;
        let response = client
            .get(url)
            .send()
            .map_err(|err| PipelineTaskError::failed(format!("抓取页面失败: {url} ({err})")))?;
        let status = response.status();
        if status.is_redirection() {
            if let Some(location) = response
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
            {
                detect_wechat_redirect(location)?;
            }
            return Err(PipelineTaskError::failed(format!(
                "抓取页面失败: HTTP {} {}",
                status.as_u16(),
                url
            )));
        }
        if !status.is_success() {
            return Err(PipelineTaskError::failed(format!(
                "抓取页面失败: HTTP {} {}",
                status.as_u16(),
                url
            )));
        }
        let html = response
            .text()
            .map_err(|err| PipelineTaskError::failed(format!("读取页面正文失败: {err}")))?;
        detect_wechat_error_page(url, &html)?;
        Ok(html)
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

        let mut child = Command::new(&browser.command)
            .arg(&browser.worker_script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| {
                PipelineTaskError::awaiting_manual_input(
                    "browser_worker_unavailable",
                    format!("启动浏览器 worker 失败: {err}"),
                )
            })?;

        if let Some(stdin) = child.stdin.as_mut() {
            let payload = serde_json::to_vec(&request).map_err(|err| {
                PipelineTaskError::failed(format!("序列化浏览器抓取请求失败: {err}"))
            })?;
            stdin.write_all(&payload).map_err(|err| {
                PipelineTaskError::failed(format!("写入浏览器抓取请求失败: {err}"))
            })?;
        }

        let output = child.wait_with_output().map_err(|err| {
            PipelineTaskError::failed(format!("等待浏览器 worker 结束失败: {err}"))
        })?;
        let stdout = String::from_utf8(output.stdout).map_err(|err| {
            PipelineTaskError::failed(format!("浏览器 worker 输出非 UTF-8: {err}"))
        })?;
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

        if !output.status.success() && stdout.trim().is_empty() {
            return Err(PipelineTaskError::awaiting_manual_input(
                "browser_worker_failed",
                format!(
                    "浏览器 worker 执行失败: {}",
                    if stderr.is_empty() {
                        "unknown"
                    } else {
                        &stderr
                    }
                ),
            ));
        }

        let response: BrowserCaptureResponse =
            serde_json::from_str(stdout.trim()).map_err(|err| {
                PipelineTaskError::failed(format!("解析浏览器 worker 返回失败: {err}"))
            })?;

        if response.ok {
            return Ok(BrowserCaptureResult {
                page_kind: response.page_kind,
                final_url: response.final_url,
                title: response.title,
                html_path: response.html_path,
                screenshot_path: response.screenshot_path,
            });
        }

        Err(PipelineTaskError::awaiting_manual_input(
            response.page_kind,
            response
                .reason
                .unwrap_or_else(|| "浏览器抓取未成功".to_string()),
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

        Ok(PipelineResult {
            output_path,
            raw_path: capture.html_path.clone(),
            snapshot_path: Some(capture.screenshot_path.clone()),
            title: capture.title.clone(),
        })
    }

    fn archive_html(&self, task: &PendingTaskRecord, html: &str) -> Result<PipelineResult> {
        let day = Utc::now().format("%Y-%m-%d").to_string();
        let raw_dir = self.root_dir.join("raw").join(&day);
        let output_dir = self.root_dir.join("processed").join(day);
        fs::create_dir_all(&raw_dir)
            .with_context(|| format!("创建原始目录失败: {}", raw_dir.display()))?;
        fs::create_dir_all(&output_dir)
            .with_context(|| format!("创建归档目录失败: {}", output_dir.display()))?;

        let raw_path = raw_dir.join(format!("{}.html", task.task_id));
        let output_path = output_dir.join(format!("{}.md", task.task_id));
        fs::write(&raw_path, html)
            .with_context(|| format!("写入原始文件失败: {}", raw_path.display()))?;

        let title = extract_html_title(html);
        let content = format!(
            "# Archived Link\n\n- task_id: {}\n- article_id: {}\n- normalized_url: {}\n- original_url: {}\n- title: {}\n- archived_at: {}\n\n## Preview\n\n{}\n",
            task.task_id,
            task.article_id,
            task.normalized_url,
            task.original_url,
            title.clone().unwrap_or_else(|| "(none)".to_string()),
            Utc::now().to_rfc3339(),
            preview_text(html),
        );
        fs::write(&output_path, content)
            .with_context(|| format!("写入归档文件失败: {}", output_path.display()))?;

        Ok(PipelineResult {
            output_path,
            raw_path,
            snapshot_path: None,
            title,
        })
    }
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

fn should_prefer_browser_capture(url: &str) -> bool {
    Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.domain().map(|domain| domain == "mp.weixin.qq.com"))
        .unwrap_or(false)
}

fn extract_manual_title(content: &str) -> Option<String> {
    content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(80).collect::<String>())
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
        detect_wechat_error_page, detect_wechat_redirect, extract_html_title, extract_primary_body,
        should_prefer_browser_capture, BrowserCaptureResult, Pipeline, PipelineFailureKind,
    };
    use crate::task_store::{PendingTaskRecord, TaskContentRecord};
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
                "<html><head><title>Hello World</title></head><body>content</body></html>",
            )
            .expect("处理 pending 任务失败");
        let content = fs::read_to_string(&result.output_path).expect("读取归档文件失败");
        let raw = fs::read_to_string(&result.raw_path).expect("读取原始文件失败");

        assert!(result.output_path.starts_with(root.join("processed")));
        assert!(result.raw_path.starts_with(root.join("raw")));
        assert!(content.contains("task-1"));
        assert!(content.contains("https://example.com"));
        assert!(content.contains("Hello World"));
        assert_eq!(result.title, Some("Hello World".to_string()));
        assert!(raw.contains("<title>Hello World</title>"));
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
    fn wechat_domain_prefers_browser_capture() {
        assert!(should_prefer_browser_capture(
            "https://mp.weixin.qq.com/s/YUvXg9i31QuQN6t-zRTe8g"
        ));
        assert!(!should_prefer_browser_capture(
            "https://example.com/article"
        ));
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
