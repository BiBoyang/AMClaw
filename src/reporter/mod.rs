use crate::config::AppConfig;
use crate::task_store::{ArchivedTaskRecord, TaskStore};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyReportOutput {
    pub day: String,
    pub markdown_path: PathBuf,
    pub item_count: usize,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct DailyReporter {
    root_dir: PathBuf,
    db_path: PathBuf,
    timezone: Tz,
}

impl DailyReporter {
    pub fn new(root_dir: PathBuf, db_path: PathBuf, timezone: Tz) -> Self {
        Self {
            root_dir,
            db_path,
            timezone,
        }
    }

    pub fn from_config(config: &AppConfig) -> Result<Self> {
        Ok(Self::new(
            config.resolved_root_dir(),
            config.db_path(),
            parse_timezone(&config.agent.timezone)?,
        ))
    }

    pub fn current_day(&self) -> String {
        Utc::now()
            .with_timezone(&self.timezone)
            .format("%Y-%m-%d")
            .to_string()
    }

    pub fn generate_for_day(&self, day: &str) -> Result<DailyReportOutput> {
        let store = TaskStore::open(&self.db_path)?;
        let archived = store.list_archived_tasks(500)?;
        let entries = archived
            .into_iter()
            .filter(|record| {
                report_day_for_timestamp(&record.updated_at, self.timezone).as_deref() == Some(day)
            })
            .collect::<Vec<_>>();

        let reports_dir = self.root_dir.join("reports");
        fs::create_dir_all(&reports_dir)
            .with_context(|| format!("创建报告目录失败: {}", reports_dir.display()))?;
        let markdown_path = reports_dir.join(format!("daily-{day}.md"));
        let content = render_daily_report_markdown(day, &entries);
        fs::write(&markdown_path, content)
            .with_context(|| format!("写入日报失败: {}", markdown_path.display()))?;

        let summary = build_daily_summary(day, &entries);
        log_reporter_info(
            "daily_report_generated",
            vec![
                ("day", json!(day)),
                ("item_count", json!(entries.len())),
                ("markdown_path", json!(markdown_path.display().to_string())),
            ],
        );
        Ok(DailyReportOutput {
            day: day.to_string(),
            markdown_path,
            item_count: entries.len(),
            summary,
        })
    }
}

fn parse_timezone(raw: &str) -> Result<Tz> {
    raw.parse::<Tz>()
        .with_context(|| format!("无效 timezone: {raw}"))
}

fn report_day_for_timestamp(timestamp: &str, timezone: Tz) -> Option<String> {
    let parsed = DateTime::parse_from_rfc3339(timestamp).ok()?;
    Some(
        parsed
            .with_timezone(&timezone)
            .format("%Y-%m-%d")
            .to_string(),
    )
}

fn build_daily_summary(day: &str, entries: &[ArchivedTaskRecord]) -> String {
    if entries.is_empty() {
        return format!("日报 {day}\n- 当天没有新的 archived 任务");
    }

    let mut lines = vec![
        format!("日报 {day}"),
        format!("- archived_count: {}", entries.len()),
    ];
    for entry in entries.iter().take(5) {
        let label = entry.title.as_deref().unwrap_or(&entry.normalized_url);
        if let Some(summary) = &entry.summary {
            lines.push(format!("- {label} | {}", flatten_summary(summary)));
        } else {
            lines.push(format!("- {} | {label}", entry.task_id));
        }
    }
    lines.join("\n")
}

fn render_daily_report_markdown(day: &str, entries: &[ArchivedTaskRecord]) -> String {
    let mut lines = vec![
        format!("# AMClaw Daily Report {}", day),
        String::new(),
        format!("- archived_count: {}", entries.len()),
        String::new(),
        "## Archived Tasks".to_string(),
        String::new(),
    ];

    if entries.is_empty() {
        lines.push("- (none)".to_string());
        lines.push(String::new());
        return lines.join("\n");
    }

    for entry in entries {
        lines.push(format!(
            "### {}",
            entry.title.as_deref().unwrap_or(&entry.task_id)
        ));
        lines.push(String::new());
        lines.push(format!("- task_id: {}", entry.task_id));
        lines.push(format!("- article_id: {}", entry.article_id));
        lines.push(format!("- url: {}", entry.normalized_url));
        lines.push(format!(
            "- content_source: {}",
            entry.content_source.as_deref().unwrap_or("(none)")
        ));
        lines.push(format!(
            "- page_kind: {}",
            entry.page_kind.as_deref().unwrap_or("(none)")
        ));
        if let Some(summary) = &entry.summary {
            lines.push(format!("- summary: {}", flatten_summary(summary)));
        }
        lines.push(format!(
            "- output_path: {}",
            entry.output_path.as_deref().unwrap_or("(none)")
        ));
        lines.push(format!("- updated_at: {}", entry.updated_at));
        lines.push(String::new());
    }

    lines.join("\n")
}

fn log_reporter_info(event: &str, fields: Vec<(&str, Value)>) {
    crate::logging::emit_structured_log("info", event, fields);
}

fn flatten_summary(summary: &str) -> String {
    summary.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::DailyReporter;
    use crate::config::AppConfig;
    use crate::task_store::{MarkTaskArchivedInput, TaskStore};
    use std::fs;
    use uuid::Uuid;

    fn temp_root() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("amclaw_reporter_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("创建测试目录失败");
        root
    }

    #[test]
    fn daily_report_is_generated_for_archived_tasks() {
        let root = temp_root();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            r#"
[storage]
root_dir = "./data"

[agent]
timezone = "Asia/Shanghai"
"#,
        )
        .expect("写入配置失败");
        let config = AppConfig::load_or_create(&config_path).expect("加载配置失败");
        let db_path = config.db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let created = store
            .record_link_submission("https://example.com/report-item")
            .expect("写入链接失败");
        store
            .mark_task_archived(
                &created.task_id,
                MarkTaskArchivedInput {
                    output_path: &root
                        .join("data")
                        .join("processed")
                        .join("report-item.md")
                        .display()
                        .to_string(),
                    title: Some("Report Item Title"),
                    page_kind: Some("article"),
                    snapshot_path: None,
                    content_source: Some("http"),
                    summary: None,
                },
            )
            .expect("更新 archived 状态失败");

        let reporter = DailyReporter::from_config(&config).expect("初始化 reporter 失败");
        let day = chrono::Utc::now()
            .with_timezone(&chrono_tz::Asia::Shanghai)
            .format("%Y-%m-%d")
            .to_string();
        let output = reporter.generate_for_day(&day).expect("生成日报失败");
        let markdown = fs::read_to_string(&output.markdown_path).expect("读取日报失败");

        assert_eq!(output.day, day);
        assert_eq!(output.item_count, 1);
        assert!(markdown.contains("# AMClaw Daily Report"));
        assert!(markdown.contains("Report Item Title"));
        assert!(output.summary.contains("archived_count: 1"));
    }

    #[test]
    fn daily_report_includes_summary_when_available() {
        let root = temp_root();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            r#"
[storage]
root_dir = "./data"

[agent]
timezone = "Asia/Shanghai"
"#,
        )
        .expect("写入配置失败");
        let config = AppConfig::load_or_create(&config_path).expect("加载配置失败");
        let db_path = config.db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        let with_summary = store
            .record_link_submission("https://example.com/with-summary")
            .expect("写入链接失败");
        store
            .mark_task_archived(
                &with_summary.task_id,
                MarkTaskArchivedInput {
                    output_path: "/tmp/with-summary.md",
                    title: Some("Summary Article"),
                    page_kind: Some("article"),
                    snapshot_path: None,
                    content_source: Some("http"),
                    summary: Some(
                        "这是一篇关于Rust的文章\n介绍了async运行时的设计\n适合初学者入门",
                    ),
                },
            )
            .expect("更新 archived 状态失败");

        let without_summary = store
            .record_link_submission("https://example.com/without-summary")
            .expect("写入链接失败");
        store
            .mark_task_archived(
                &without_summary.task_id,
                MarkTaskArchivedInput {
                    output_path: "/tmp/without-summary.md",
                    title: Some("No Summary Article"),
                    page_kind: Some("webpage"),
                    snapshot_path: None,
                    content_source: Some("http"),
                    summary: None,
                },
            )
            .expect("更新 archived 状态失败");

        let reporter = DailyReporter::from_config(&config).expect("初始化 reporter 失败");
        let day = chrono::Utc::now()
            .with_timezone(&chrono_tz::Asia::Shanghai)
            .format("%Y-%m-%d")
            .to_string();
        let output = reporter.generate_for_day(&day).expect("生成日报失败");
        let markdown = fs::read_to_string(&output.markdown_path).expect("读取日报失败");

        assert_eq!(output.item_count, 2);
        assert!(markdown
            .contains("summary: 这是一篇关于Rust的文章 介绍了async运行时的设计 适合初学者入门"));
        assert!(output.summary.contains(
            "Summary Article | 这是一篇关于Rust的文章 介绍了async运行时的设计 适合初学者入门"
        ));
        assert!(output.summary.contains("No Summary Article"));
        assert!(!markdown.contains("summary: None"));
    }
}
