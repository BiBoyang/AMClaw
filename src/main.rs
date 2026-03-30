use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
mod agent_core;
mod chat_adapter;
mod tool_registry;

fn main() -> Result<()> {
    load_startup_env_files()?;
    if let Ok(command) = std::env::var("AMCLAW_AGENT_DEMO_COMMAND") {
        let workspace_root = std::env::current_dir().context("获取当前目录失败")?;
        let agent = agent_core::AgentCore::new(workspace_root)?;
        let output = agent.run(&command)?;
        println!("[AgentDemo] {output}");
        return Ok(());
    }

    let running = Arc::new(AtomicBool::new(true));
    {
        let running = Arc::clone(&running);
        ctrlc::set_handler(move || {
            println!("\n[退出] 收到 SIGINT，正在退出...");
            running.store(false, Ordering::Relaxed);
        })
        .context("注册 Ctrl-C 处理器失败")?;
    }

    if let Err(err) = chat_adapter::run(running) {
        eprintln!("[启动失败] {err:#}");
        std::process::exit(1);
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
    eprintln!("[启动] 已加载配置文件: {}", file_path.display());
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
