use crate::task_store::{PendingTaskRecord, TaskContentRecord};
use anyhow::{Context, Result};
use chrono::Utc;
use reqwest::blocking::Client;
use reqwest::header::LOCATION;
use reqwest::redirect::Policy;
use reqwest::Url;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Pipeline {
    root_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineResult {
    pub output_path: PathBuf,
    pub raw_path: PathBuf,
    pub title: Option<String>,
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
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
        }
    }

    pub fn process_pending_task(
        &self,
        task: &PendingTaskRecord,
    ) -> std::result::Result<PipelineResult, PipelineTaskError> {
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
        detect_wechat_error_page, detect_wechat_redirect, extract_html_title, Pipeline,
        PipelineFailureKind,
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
        let pipeline = Pipeline::new(&root);
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
        let pipeline = Pipeline::new(&root);
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
}
