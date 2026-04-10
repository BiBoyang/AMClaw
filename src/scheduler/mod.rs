use crate::config::AppConfig;
use crate::reporter::{DailyReportOutput, DailyReporter};
use anyhow::{bail, Context, Result};
use chrono::{Timelike, Utc};
use chrono_tz::Tz;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct DailyReportSchedule {
    timezone: Tz,
    hour: u32,
    minute: u32,
    report_to_user_id: String,
}

impl DailyReportSchedule {
    pub fn from_config(config: &AppConfig) -> Result<Option<Self>> {
        if !config.scheduler.enabled {
            return Ok(None);
        }
        let Some(report_to_user_id) = config
            .scheduler
            .report_to_user_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(None);
        };
        let timezone = parse_timezone(&config.agent.timezone)?;
        let (hour, minute) = parse_daily_run_time(&config.scheduler.daily_run_time)?;
        Ok(Some(Self {
            timezone,
            hour,
            minute,
            report_to_user_id: report_to_user_id.to_string(),
        }))
    }

    pub fn report_to_user_id(&self) -> &str {
        &self.report_to_user_id
    }

    pub fn should_run_now(
        &self,
        now_utc: chrono::DateTime<Utc>,
        last_run_day: Option<&str>,
    ) -> Option<String> {
        let now = now_utc.with_timezone(&self.timezone);
        let day = now.format("%Y-%m-%d").to_string();
        if should_run_for_now(&now, self.hour, self.minute, last_run_day) {
            Some(day)
        } else {
            None
        }
    }
}

pub fn spawn_daily_scheduler(
    config: AppConfig,
    running: Arc<AtomicBool>,
) -> Result<Option<JoinHandle<()>>> {
    if !config.scheduler.enabled {
        return Ok(None);
    }

    let timezone = parse_timezone(&config.agent.timezone)?;
    let (hour, minute) = parse_daily_run_time(&config.scheduler.daily_run_time)?;
    let reporter = DailyReporter::from_config(&config)?;
    let handle = thread::Builder::new()
        .name("amclaw-daily-scheduler".to_string())
        .spawn(move || {
            let mut last_run_day: Option<String> = None;
            while running.load(Ordering::Relaxed) {
                let now = Utc::now().with_timezone(&timezone);
                let day = now.format("%Y-%m-%d").to_string();
                if should_run_for_now(&now, hour, minute, last_run_day.as_deref()) {
                    match reporter.generate_for_day(&day) {
                        Ok(output) => {
                            log_scheduler_info(
                                "scheduler_daily_report_generated",
                                vec![
                                    ("day", json!(output.day)),
                                    ("item_count", json!(output.item_count)),
                                    (
                                        "markdown_path",
                                        json!(output.markdown_path.display().to_string()),
                                    ),
                                ],
                            );
                            last_run_day = Some(day);
                        }
                        Err(err) => {
                            log_scheduler_error(
                                "scheduler_daily_report_failed",
                                vec![
                                    ("day", json!(day)),
                                    ("error_kind", json!("scheduler_daily_report_failed")),
                                    ("detail", json!(err.to_string())),
                                ],
                            );
                        }
                    }
                }
                thread::sleep(Duration::from_secs(30));
            }
        })
        .context("启动 daily scheduler 线程失败")?;
    Ok(Some(handle))
}

pub fn generate_daily_report_once(config: &AppConfig, day: &str) -> Result<DailyReportOutput> {
    DailyReporter::from_config(config)?.generate_for_day(day)
}

fn parse_timezone(raw: &str) -> Result<Tz> {
    raw.parse::<Tz>()
        .with_context(|| format!("无效 timezone: {raw}"))
}

fn parse_daily_run_time(raw: &str) -> Result<(u32, u32)> {
    let (hour, minute) = raw
        .trim()
        .split_once(':')
        .context("daily_run_time 格式应为 HH:MM")?;
    let hour: u32 = hour.parse().context("解析调度小时失败")?;
    let minute: u32 = minute.parse().context("解析调度分钟失败")?;
    if hour > 23 || minute > 59 {
        bail!("daily_run_time 超出范围: {raw}");
    }
    Ok((hour, minute))
}

fn should_run_for_now(
    now: &chrono::DateTime<Tz>,
    hour: u32,
    minute: u32,
    last_run_day: Option<&str>,
) -> bool {
    let day = now.format("%Y-%m-%d").to_string();
    if last_run_day == Some(day.as_str()) {
        return false;
    }
    now.hour() > hour || (now.hour() == hour && now.minute() >= minute)
}

fn log_scheduler_info(event: &str, fields: Vec<(&str, Value)>) {
    crate::logging::emit_structured_log("info", event, fields);
}

fn log_scheduler_error(event: &str, fields: Vec<(&str, Value)>) {
    crate::logging::emit_structured_log("error", event, fields);
}

#[cfg(test)]
mod tests {
    use super::{parse_daily_run_time, should_run_for_now, DailyReportSchedule};
    use crate::config::AppConfig;
    use chrono::TimeZone;
    use chrono_tz::Asia::Shanghai;
    use std::fs;
    use uuid::Uuid;

    fn temp_dir() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("amclaw_scheduler_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("创建测试目录失败");
        root
    }

    #[test]
    fn daily_run_time_is_parsed() {
        assert_eq!(parse_daily_run_time("09:30").expect("解析失败"), (9, 30));
    }

    #[test]
    fn invalid_daily_run_time_is_rejected() {
        assert!(parse_daily_run_time("25:00").is_err());
        assert!(parse_daily_run_time("bad").is_err());
    }

    #[test]
    fn should_run_only_after_scheduled_time_and_once_per_day() {
        let now = Shanghai
            .with_ymd_and_hms(2026, 4, 10, 9, 31, 0)
            .single()
            .expect("构造时间失败");
        assert!(should_run_for_now(&now, 9, 30, None));
        assert!(!should_run_for_now(&now, 9, 30, Some("2026-04-10")));
    }

    #[test]
    fn daily_report_schedule_is_built_from_config() {
        let root = temp_dir();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            r#"
[agent]
timezone = "Asia/Shanghai"

[scheduler]
enabled = true
daily_run_time = "09:30"
report_to_user_id = "user-a"
"#,
        )
        .expect("写入配置失败");
        let config = AppConfig::load_or_create(&config_path).expect("加载配置失败");
        let schedule = DailyReportSchedule::from_config(&config)
            .expect("构造 schedule 失败")
            .expect("应存在 schedule");

        let now = chrono::Utc
            .with_ymd_and_hms(2026, 4, 10, 1, 31, 0)
            .single()
            .expect("构造 UTC 时间失败");
        assert_eq!(schedule.report_to_user_id(), "user-a");
        assert_eq!(
            schedule.should_run_now(now, None),
            Some("2026-04-10".to_string())
        );
    }
}
