use crate::config::AppConfig;
use crate::task_store::{ArchivedTaskRecord, TaskStore};
use anyhow::{bail, Context, Result};
use chrono::{Datelike, TimeZone, Utc};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeeklyReportOutput {
    pub week: String,
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

    pub fn current_week(&self) -> String {
        let now = Utc::now().with_timezone(&self.timezone);
        format_week_key(now.iso_week().year(), now.iso_week().week())
    }

    pub fn generate_for_day(&self, day: &str) -> Result<DailyReportOutput> {
        let store = TaskStore::open(&self.db_path)?;
        let (start, end) =
            day_range(day, self.timezone).with_context(|| format!("无法计算日期范围: {day}"))?;
        let entries = store.list_archived_tasks_in_range(&start, &end, 500)?;

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

    pub fn generate_weekly_for_week(&self, week: &str) -> Result<WeeklyReportOutput> {
        let week = normalize_week_key(week)?;
        let store = TaskStore::open(&self.db_path)?;
        let (start, end) =
            week_range(&week, self.timezone).with_context(|| format!("无法计算周范围: {week}"))?;
        let entries = store.list_archived_tasks_in_range(&start, &end, 2000)?;

        let reports_dir = self.root_dir.join("reports");
        fs::create_dir_all(&reports_dir)
            .with_context(|| format!("创建报告目录失败: {}", reports_dir.display()))?;
        let markdown_path = reports_dir.join(format!("weekly-{week}.md"));
        let content = render_weekly_report_markdown(&week, &entries);
        fs::write(&markdown_path, content)
            .with_context(|| format!("写入周报失败: {}", markdown_path.display()))?;

        let summary = build_weekly_summary(&week, &entries);
        log_reporter_info(
            "weekly_report_generated",
            vec![
                ("week", json!(week)),
                ("item_count", json!(entries.len())),
                ("markdown_path", json!(markdown_path.display().to_string())),
            ],
        );
        Ok(WeeklyReportOutput {
            week,
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

fn normalize_week_key(raw: &str) -> Result<String> {
    let raw = raw.trim();
    let (year_raw, week_raw) = raw.split_once('-').context("周报 week 格式应为 YYYY-WW")?;
    if year_raw.len() != 4 || week_raw.len() != 2 {
        bail!("周报 week 格式应为 YYYY-WW: {raw}");
    }
    let year: i32 = year_raw.parse().context("解析周报年份失败")?;
    let week: u32 = week_raw.parse().context("解析周报周数失败")?;
    if !(1..=53).contains(&week) {
        bail!("周报周数超出范围: {raw}");
    }
    Ok(format_week_key(year, week))
}

fn format_week_key(year: i32, week: u32) -> String {
    format!("{year:04}-{week:02}")
}

fn day_range(day: &str, tz: Tz) -> Option<(String, String)> {
    let naive = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    let start = tz
        .from_local_datetime(&naive.and_hms_opt(0, 0, 0)?)
        .single()?;
    let end = start + chrono::Duration::days(1);
    Some((start.to_rfc3339(), end.to_rfc3339()))
}

fn week_range(week: &str, tz: Tz) -> Option<(String, String)> {
    let (year_str, week_str) = week.split_once('-')?;
    let year: i32 = year_str.parse().ok()?;
    let week: u32 = week_str.parse().ok()?;
    let monday = chrono::NaiveDate::from_isoywd_opt(year, week, chrono::Weekday::Mon)?;
    let start = tz
        .from_local_datetime(&monday.and_hms_opt(0, 0, 0)?)
        .single()?;
    let end = start + chrono::Duration::days(7);
    Some((start.to_rfc3339(), end.to_rfc3339()))
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

fn build_weekly_summary(week: &str, entries: &[ArchivedTaskRecord]) -> String {
    if entries.is_empty() {
        return format!("周报 {week}\n- 当周没有新的 archived 任务");
    }

    let mut lines = vec![
        format!("周报 {week}"),
        format!("- archived_count: {}", entries.len()),
    ];
    for entry in entries.iter().take(8) {
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

fn render_weekly_report_markdown(week: &str, entries: &[ArchivedTaskRecord]) -> String {
    let mut lines = vec![
        format!("# AMClaw Weekly Report {}", week),
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
    use rusqlite::Connection;
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

    #[test]
    fn weekly_report_is_generated_for_archived_tasks() {
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
            .record_link_submission("https://example.com/weekly-report-item")
            .expect("写入链接失败");
        store
            .mark_task_archived(
                &created.task_id,
                MarkTaskArchivedInput {
                    output_path: "/tmp/weekly-report-item.md",
                    title: Some("Weekly Item"),
                    page_kind: Some("article"),
                    snapshot_path: None,
                    content_source: Some("http"),
                    summary: Some("weekly summary"),
                },
            )
            .expect("更新 archived 状态失败");

        let reporter = DailyReporter::from_config(&config).expect("初始化 reporter 失败");
        let week = reporter.current_week();
        let output = reporter
            .generate_weekly_for_week(&week)
            .expect("生成周报失败");
        let markdown = fs::read_to_string(&output.markdown_path).expect("读取周报失败");

        assert_eq!(output.week, week);
        assert_eq!(output.item_count, 1);
        assert!(markdown.contains("# AMClaw Weekly Report"));
        assert!(markdown.contains("Weekly Item"));
        assert!(output.summary.contains("周报"));
        assert!(output.summary.contains("archived_count: 1"));
    }

    #[test]
    fn daily_report_does_not_miss_entries_when_recent_history_is_large() {
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

        let reporter = DailyReporter::from_config(&config).expect("初始化 reporter 失败");
        let tz = chrono_tz::Asia::Shanghai;
        let today = chrono::Utc::now().with_timezone(&tz);
        let yesterday = (today - chrono::TimeDelta::try_days(1).unwrap())
            .format("%Y-%m-%d")
            .to_string();
        let two_days_ago = (today - chrono::TimeDelta::try_days(2).unwrap())
            .format("%Y-%m-%d")
            .to_string();

        // 502 条"昨天"的数据，把前天的数据挤出全局 LIMIT 500
        let mut yesterday_ids = Vec::new();
        for i in 0..502 {
            let created = store
                .record_link_submission(&format!("https://example.com/yesterday-{i}"))
                .expect("写入链接失败");
            store
                .mark_task_archived(
                    &created.task_id,
                    MarkTaskArchivedInput {
                        output_path: &format!("/tmp/yesterday-{i}.md"),
                        title: Some(&format!("Yesterday Item {i}")),
                        page_kind: Some("article"),
                        snapshot_path: None,
                        content_source: Some("http"),
                        summary: None,
                    },
                )
                .expect("更新 archived 状态失败");
            yesterday_ids.push(created.task_id);
        }

        // 5 条"前天"的数据
        let mut two_days_ago_ids = Vec::new();
        for i in 0..5 {
            let created = store
                .record_link_submission(&format!("https://example.com/twodays-{i}"))
                .expect("写入链接失败");
            store
                .mark_task_archived(
                    &created.task_id,
                    MarkTaskArchivedInput {
                        output_path: &format!("/tmp/twodays-{i}.md"),
                        title: Some(&format!("Two Days Ago Item {i}")),
                        page_kind: Some("article"),
                        snapshot_path: None,
                        content_source: Some("http"),
                        summary: None,
                    },
                )
                .expect("更新 archived 状态失败");
            two_days_ago_ids.push(created.task_id);
        }

        // 用 SQL 修改 updated_at，使数据分布到不同日期
        let conn = Connection::open(&db_path).expect("打开数据库失败");
        let yesterday_start = format!("{}T00:00:00+08:00", yesterday);
        for id in &yesterday_ids {
            conn.execute(
                "UPDATE tasks SET updated_at = ?1 WHERE id = ?2",
                [&yesterday_start, id],
            )
            .expect("更新昨天时间失败");
        }
        let two_days_start = format!("{}T00:00:00+08:00", two_days_ago);
        for id in &two_days_ago_ids {
            conn.execute(
                "UPDATE tasks SET updated_at = ?1 WHERE id = ?2",
                [&two_days_start, id],
            )
            .expect("更新前天时间失败");
        }
        drop(conn);
        drop(store);

        let output = reporter
            .generate_for_day(&two_days_ago)
            .expect("生成前天日报失败");
        assert_eq!(output.item_count, 5, "前天的 5 条数据不应被全局 LIMIT 截断");
    }

    #[test]
    fn utc_storage_with_local_timezone_boundary_is_correct() {
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
        let reporter = DailyReporter::from_config(&config).expect("初始化 reporter 失败");

        // 构造 4 条数据，用 UTC 存储验证本地时区边界
        // 上海 2024-01-15 对应的 UTC 范围是 [2024-01-14T16:00:00Z, 2024-01-15T16:00:00Z)
        let ids: Vec<String> = (0..4)
            .map(|i| {
                let created = store
                    .record_link_submission(&format!("https://example.com/boundary-{i}"))
                    .expect("写入链接失败");
                store
                    .mark_task_archived(
                        &created.task_id,
                        MarkTaskArchivedInput {
                            output_path: &format!("/tmp/boundary-{i}.md"),
                            title: Some(&format!("Boundary {i}")),
                            page_kind: Some("article"),
                            snapshot_path: None,
                            content_source: Some("http"),
                            summary: None,
                        },
                    )
                    .expect("更新 archived 状态失败");
                created.task_id
            })
            .collect();

        let conn = Connection::open(&db_path).expect("打开数据库失败");
        let timestamps = [
            // 15:59:59Z = 前一天上海 23:59:59 → 应排除
            "2024-01-14T15:59:59Z",
            // 16:00:00Z = 当天上海 00:00:00 → 应包含
            "2024-01-14T16:00:00Z",
            // 15:59:59Z = 当天上海 23:59:59 → 应包含
            "2024-01-15T15:59:59Z",
            // 16:00:00Z = 后一天上海 00:00:00 → 应排除
            "2024-01-15T16:00:00Z",
        ];
        for (id, ts) in ids.iter().zip(&timestamps) {
            conn.execute("UPDATE tasks SET updated_at = ?1 WHERE id = ?2", [*ts, id])
                .expect("更新 UTC 时间失败");
        }
        drop(conn);
        drop(store);

        let output = reporter
            .generate_for_day("2024-01-15")
            .expect("生成日报失败");
        assert_eq!(output.item_count, 2, "应恰好命中 UTC 跨日边界内的 2 条数据");
        assert!(
            output.summary.contains("Boundary 1") || output.summary.contains("Boundary 2"),
            "返回的数据应是边界内的两条"
        );
    }
}
