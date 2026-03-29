use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
mod agent_core;
mod chat_adapter;
mod tool_registry;

fn main() -> Result<()> {
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
