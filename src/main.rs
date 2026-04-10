use anyhow::{Context, Result};
use config::AppConfig;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
mod agent_core;
mod chat_adapter;
mod command_router;
mod config;
mod logging;
mod pipeline;
mod reporter;
mod scheduler;
mod session_router;
mod task_store;
mod tool_registry;

fn main() -> Result<()> {
    load_startup_env_files()?;
    let workspace_root = std::env::current_dir().context("获取当前目录失败")?;
    let app_config = AppConfig::load_or_create(workspace_root.join("config.toml"))?;
    let browser = app_config.resolved_browser();

    if let Ok(day) = std::env::var("AMCLAW_GENERATE_DAILY_REPORT_FOR") {
        let output = scheduler::generate_daily_report_once(&app_config, &day)?;
        log_startup_info(
            "daily_report_generated_once",
            vec![
                ("day", json!(output.day)),
                ("item_count", json!(output.item_count)),
                (
                    "markdown_path",
                    json!(output.markdown_path.display().to_string()),
                ),
            ],
        );
        println!("{}", output.summary);
        return Ok(());
    }

    if let Ok(command) = std::env::var("AMCLAW_AGENT_DEMO_COMMAND") {
        let agent =
            agent_core::AgentCore::with_task_store_db_path(workspace_root, app_config.db_path())?;
        let output = agent.run(&command)?;
        log_startup_info(
            "agent_demo_finished",
            vec![
                ("command", json!(command)),
                ("output_chars", json!(output.chars().count())),
            ],
        );
        return Ok(());
    }

    let running = Arc::new(AtomicBool::new(true));
    {
        let running = Arc::clone(&running);
        ctrlc::set_handler(move || {
            log_startup_info("signal_received", vec![("signal", json!("SIGINT"))]);
            running.store(false, Ordering::Relaxed);
        })
        .context("注册 Ctrl-C 处理器失败")?;
    }

    let scheduler_handle =
        scheduler::spawn_daily_scheduler(app_config.clone(), Arc::clone(&running))?;

    if let Err(err) = chat_adapter::run(app_config, browser, Arc::clone(&running)) {
        log_startup_error(
            "startup_failed",
            vec![
                ("error_kind", json!("chat_adapter_run_failed")),
                ("detail", json!(format!("{err:#}"))),
            ],
        );
        std::process::exit(1);
    }
    running.store(false, Ordering::Relaxed);
    if let Some(handle) = scheduler_handle {
        let _ = handle.join();
    }
    Ok(())
}

fn load_startup_env_files() -> Result<()> {
    load_env_file_if_exists(".env.deepseek.local")?;
    load_env_file_if_exists(".env.deepseek")?;
    load_env_file_if_exists(".env.moonshot.local")?;
    load_env_file_if_exists(".env.moonshot")?;
    Ok(())
}

fn load_env_file_if_exists(path: &str) -> Result<()> {
    let file_path = Path::new(path);
    if !file_path.exists() {
        return Ok(());
    }
    let content = fs::read_to_string(file_path)
        .with_context(|| format!("读取配置文件失败: {}", file_path.display()))?;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if std::env::var_os(key).is_some() {
            continue;
        }
        std::env::set_var(key, clean_env_value(value));
    }
    log_startup_info(
        "startup_env_loaded",
        vec![("path", json!(file_path.display().to_string()))],
    );
    Ok(())
}

fn clean_env_value(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_matches('`')
        .trim()
        .to_string()
}

fn log_startup_info(event: &str, fields: Vec<(&str, Value)>) {
    log_startup_event("info", event, fields);
}

fn log_startup_error(event: &str, fields: Vec<(&str, Value)>) {
    log_startup_event("error", event, fields);
}

fn log_startup_event(level: &str, event: &str, fields: Vec<(&str, Value)>) {
    crate::logging::emit_structured_log(level, event, fields);
}
