use crate::config::AppConfig;
use crate::reporter::{DailyReportOutput, DailyReporter, WeeklyReportOutput};
use anyhow::{bail, Context, Result};
use chrono::{Datelike, Timelike, Utc};
use chrono_tz::Tz;
use serde_json::json;
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
    /// 推送目标用户；缺省时调度仍存在（只生成快照），仅推送跳过。
    report_to_user_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WeeklyReportSchedule {
    timezone: Tz,
    weekday_monday_based: u32,
    hour: u32,
    minute: u32,
    /// 推送目标用户；缺省时调度仍存在（只生成快照），仅推送跳过。
    report_to_user_id: Option<String>,
}

impl DailyReportSchedule {
    pub fn from_config(config: &AppConfig) -> Result<Option<Self>> {
        if !config.scheduler.enabled {
            return Ok(None);
        }
        let timezone = parse_timezone(&config.agent.timezone)?;
        let (hour, minute) = parse_daily_run_time(&config.scheduler.daily_run_time)?;
        Ok(Some(Self {
            timezone,
            hour,
            minute,
            report_to_user_id: parse_report_to_user_id(config),
        }))
    }

    pub fn report_to_user_id(&self) -> Option<&str> {
        self.report_to_user_id.as_deref()
    }

    /// 到点判断：每天最多触发一次，到点（含）后返回当天日期（本地时区 YYYY-MM-DD）。
    pub fn should_run_now(
        &self,
        now_utc: chrono::DateTime<Utc>,
        last_run_day: Option<&str>,
    ) -> Option<String> {
        let now = now_utc.with_timezone(&self.timezone);
        let day = now.format("%Y-%m-%d").to_string();
        if last_run_day == Some(day.as_str()) {
            return None;
        }
        if now.hour() > self.hour || (now.hour() == self.hour && now.minute() >= self.minute) {
            Some(day)
        } else {
            None
        }
    }
}

impl WeeklyReportSchedule {
    pub fn from_config(config: &AppConfig) -> Result<Option<Self>> {
        if !config.scheduler.enabled {
            return Ok(None);
        }
        let timezone = parse_timezone(&config.agent.timezone)?;
        let (hour, minute) = parse_daily_run_time(&config.scheduler.daily_run_time)?;
        Ok(Some(Self {
            timezone,
            weekday_monday_based: 1,
            hour,
            minute,
            report_to_user_id: parse_report_to_user_id(config),
        }))
    }

    pub fn report_to_user_id(&self) -> Option<&str> {
        self.report_to_user_id.as_deref()
    }

    /// 到点判断：每周（ISO 周）最多触发一次，目标周日到点（含）后返回周键（YYYY-WW）。
    pub fn should_run_now(
        &self,
        now_utc: chrono::DateTime<Utc>,
        last_run_week: Option<&str>,
    ) -> Option<String> {
        let now = now_utc.with_timezone(&self.timezone);
        let iso = now.iso_week();
        let week = format!("{:04}-{:02}", iso.year(), iso.week());
        if last_run_week == Some(week.as_str()) {
            return None;
        }
        let now_weekday = now.weekday().number_from_monday();
        if now_weekday < self.weekday_monday_based {
            return None;
        }
        if now_weekday > self.weekday_monday_based {
            return Some(week);
        }
        if now.hour() > self.hour || (now.hour() == self.hour && now.minute() >= self.minute) {
            Some(week)
        } else {
            None
        }
    }
}

pub fn spawn_daily_scheduler(
    config: AppConfig,
    running: Arc<AtomicBool>,
) -> Result<Option<JoinHandle<()>>> {
    // 生成触发与 chat_adapter 推送复用同一份调度判定（*ReportSchedule::should_run_now），
    // 避免两份到点逻辑漂移。scheduler.enabled=false 时两边一致不启动。
    let Some(daily_schedule) = DailyReportSchedule::from_config(&config)? else {
        return Ok(None);
    };
    let Some(weekly_schedule) = WeeklyReportSchedule::from_config(&config)? else {
        return Ok(None);
    };
    let reporter = DailyReporter::from_config(&config)?;
    let handle = thread::Builder::new()
        .name("amclaw-daily-scheduler".to_string())
        .spawn(move || {
            let mut last_run_day: Option<String> = None;
            let mut last_run_week: Option<String> = None;
            while running.load(Ordering::Relaxed) {
                let now = Utc::now();
                if let Some(day) = daily_schedule.should_run_now(now, last_run_day.as_deref()) {
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

                if let Some(week) = weekly_schedule.should_run_now(now, last_run_week.as_deref()) {
                    match reporter.generate_weekly_for_week(&week) {
                        Ok(output) => {
                            log_scheduler_info(
                                "scheduler_weekly_report_generated",
                                vec![
                                    ("week", json!(output.week)),
                                    ("item_count", json!(output.item_count)),
                                    (
                                        "markdown_path",
                                        json!(output.markdown_path.display().to_string()),
                                    ),
                                ],
                            );
                            last_run_week = Some(week);
                        }
                        Err(err) => {
                            log_scheduler_error(
                                "scheduler_weekly_report_failed",
                                vec![
                                    ("week", json!(week)),
                                    ("error_kind", json!("scheduler_weekly_report_failed")),
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

/// 启动 scheduler watchdog 线程，定期检查 scheduler 线程是否异常终止。
/// 若 scheduler 在 `running` 仍为 true 时结束，则记录结构化 error 日志并返回 true。
/// 返回的 JoinHandle 可通过 `.join()` 获取检测结果（true = 异常终止）。
pub fn spawn_scheduler_watchdog(
    handle: JoinHandle<()>,
    running: Arc<AtomicBool>,
) -> JoinHandle<bool> {
    spawn_scheduler_watchdog_with_interval(handle, running, Duration::from_secs(5))
}

/// 带自定义检查间隔的 watchdog，主要用于测试缩短等待时间。
pub fn spawn_scheduler_watchdog_with_interval(
    handle: JoinHandle<()>,
    running: Arc<AtomicBool>,
    check_interval: Duration,
) -> JoinHandle<bool> {
    thread::spawn(move || {
        while running.load(Ordering::Relaxed) {
            if handle.is_finished() {
                let _ = handle.join();
                log_scheduler_error(
                    "scheduler_health_check_failed",
                    vec![
                        ("component", json!("scheduler")),
                        ("status", json!("thread_terminated")),
                    ],
                );
                return true;
            }
            thread::sleep(check_interval);
        }
        let _ = handle.join();
        false
    })
}

pub fn generate_daily_report_once(config: &AppConfig, day: &str) -> Result<DailyReportOutput> {
    DailyReporter::from_config(config)?.generate_for_day(day)
}

pub fn generate_weekly_report_once(config: &AppConfig, week: &str) -> Result<WeeklyReportOutput> {
    DailyReporter::from_config(config)?.generate_weekly_for_week(week)
}

fn parse_timezone(raw: &str) -> Result<Tz> {
    raw.parse::<Tz>()
        .with_context(|| format!("无效 timezone: {raw}"))
}

/// 解析推送目标用户：空白视为未配置（调度照常生成快照，仅推送跳过）。
fn parse_report_to_user_id(config: &AppConfig) -> Option<String> {
    config
        .scheduler
        .report_to_user_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
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

crate::define_module_loggers!(info = log_scheduler_info, error = log_scheduler_error);

#[cfg(test)]
mod tests {
    use super::{
        parse_daily_run_time, spawn_scheduler_watchdog_with_interval, DailyReportSchedule,
        WeeklyReportSchedule,
    };
    use crate::config::AppConfig;
    use chrono::TimeZone;
    use chrono_tz::Asia::Shanghai;
    use std::fs;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;
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
        let schedule = DailyReportSchedule {
            timezone: Shanghai,
            hour: 9,
            minute: 30,
            report_to_user_id: None,
        };
        // 上海 2026-04-10 09:31 = UTC 01:31，已过 09:30 触发点
        let now = chrono::Utc
            .with_ymd_and_hms(2026, 4, 10, 1, 31, 0)
            .single()
            .expect("构造 UTC 时间失败");
        assert_eq!(
            schedule.should_run_now(now, None),
            Some("2026-04-10".to_string())
        );
        assert_eq!(schedule.should_run_now(now, Some("2026-04-10")), None);

        // 上海 2026-04-10 09:29 = UTC 01:29，未到触发点
        let early = chrono::Utc
            .with_ymd_and_hms(2026, 4, 10, 1, 29, 0)
            .single()
            .expect("构造 UTC 时间失败");
        assert_eq!(schedule.should_run_now(early, None), None);
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
        assert_eq!(schedule.report_to_user_id(), Some("user-a"));
        assert_eq!(
            schedule.should_run_now(now, None),
            Some("2026-04-10".to_string())
        );
    }

    #[test]
    fn schedule_is_built_without_push_target() {
        // 未配置 report_to_user_id 时调度仍存在（保障生成触发不依赖推送配置），仅推送目标为空
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
"#,
        )
        .expect("写入配置失败");
        let config = AppConfig::load_or_create(&config_path).expect("加载配置失败");
        let daily = DailyReportSchedule::from_config(&config)
            .expect("构造 schedule 失败")
            .expect("无推送目标时 schedule 仍应存在");
        assert_eq!(daily.report_to_user_id(), None);
        let weekly = WeeklyReportSchedule::from_config(&config)
            .expect("构造 weekly schedule 失败")
            .expect("无推送目标时 weekly schedule 仍应存在");
        assert_eq!(weekly.report_to_user_id(), None);
    }

    #[test]
    fn schedule_is_none_when_scheduler_disabled() {
        let root = temp_dir();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            r#"
[agent]
timezone = "Asia/Shanghai"

[scheduler]
enabled = false
daily_run_time = "09:30"
report_to_user_id = "user-a"
"#,
        )
        .expect("写入配置失败");
        let config = AppConfig::load_or_create(&config_path).expect("加载配置失败");
        assert!(DailyReportSchedule::from_config(&config)
            .expect("构造 schedule 失败")
            .is_none());
        assert!(WeeklyReportSchedule::from_config(&config)
            .expect("构造 weekly schedule 失败")
            .is_none());
    }

    #[test]
    fn should_run_weekly_after_target_weekday_and_once_per_week() {
        let schedule = WeeklyReportSchedule {
            timezone: Shanghai,
            weekday_monday_based: 1,
            hour: 9,
            minute: 30,
            report_to_user_id: None,
        };
        // 上海 2026-04-13（周一，ISO 2026-16）09:31 = UTC 01:31，已过触发点
        let now = chrono::Utc
            .with_ymd_and_hms(2026, 4, 13, 1, 31, 0)
            .single()
            .expect("构造 UTC 时间失败");
        assert_eq!(
            schedule.should_run_now(now, None),
            Some("2026-16".to_string())
        );
        assert_eq!(schedule.should_run_now(now, Some("2026-16")), None);

        // 上海 2026-04-13（周一）09:29 = UTC 01:29，目标周日当天未到点不触发
        let early = chrono::Utc
            .with_ymd_and_hms(2026, 4, 13, 1, 29, 0)
            .single()
            .expect("构造 UTC 时间失败");
        assert_eq!(schedule.should_run_now(early, None), None);

        // 上海 2026-04-14（周二）09:31 = UTC 01:31，目标周日之后触发
        let tuesday = chrono::Utc
            .with_ymd_and_hms(2026, 4, 14, 1, 31, 0)
            .single()
            .expect("构造 UTC 时间失败");
        assert_eq!(
            schedule.should_run_now(tuesday, None),
            Some("2026-16".to_string())
        );
    }

    #[test]
    fn weekly_report_schedule_is_built_from_config() {
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
        let schedule = WeeklyReportSchedule::from_config(&config)
            .expect("构造 weekly schedule 失败")
            .expect("应存在 weekly schedule");

        let now = chrono::Utc
            .with_ymd_and_hms(2026, 4, 13, 1, 31, 0)
            .single()
            .expect("构造 UTC 时间失败");
        assert_eq!(schedule.report_to_user_id(), Some("user-a"));
        assert_eq!(
            schedule.should_run_now(now, None),
            Some("2026-16".to_string())
        );
    }

    #[test]
    fn scheduler_watchdog_detects_panic() {
        let running = Arc::new(AtomicBool::new(true));
        let doomed = thread::spawn(|| panic!("injected scheduler panic"));
        let watchdog = spawn_scheduler_watchdog_with_interval(
            doomed,
            Arc::clone(&running),
            Duration::from_millis(10),
        );
        let detected = watchdog.join().expect("watchdog 不应 panic");
        assert!(detected, "watchdog 应检测到 scheduler 异常终止");
    }

    #[test]
    fn scheduler_watchdog_ignores_normal_shutdown() {
        let running = Arc::new(AtomicBool::new(true));
        let r = Arc::clone(&running);
        let normal = thread::spawn(move || {
            while r.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(10));
            }
        });

        let watchdog = spawn_scheduler_watchdog_with_interval(
            normal,
            Arc::clone(&running),
            Duration::from_millis(10),
        );

        // 模拟主流程正常退出：先设置 running=false，再等待 watchdog
        thread::sleep(Duration::from_millis(30));
        running.store(false, Ordering::Relaxed);

        let detected = watchdog.join().expect("watchdog 不应 panic");
        assert!(
            !detected,
            "正常结束时 running 已为 false，不应被判定为异常终止"
        );
    }
}
