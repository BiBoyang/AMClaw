use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
mod wechat;

fn main() -> Result<()> {
    let running = Arc::new(AtomicBool::new(true));
    {
        let running = Arc::clone(&running);
        ctrlc::set_handler(move || {
            println!("\n[退出] 收到 SIGINT，正在退出...");
            running.store(false, Ordering::Relaxed);
        })
        .context("注册 Ctrl-C 处理器失败")?;
    }

    if let Err(err) = wechat::run(running) {
        eprintln!("[启动失败] {err:#}");
        std::process::exit(1);
    }
    Ok(())
}
