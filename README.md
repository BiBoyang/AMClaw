# AMClaw

Rust 实现的微信 iLink Bot Demo。
当前版本（0.1.0）为实验项目。

## 功能

- 扫码登录微信 iLink
- 长轮询 `getupdates` 接收消息
- 缓存每个用户的 `context_token`
- 收到文本后自动回复（`hello/你好/时间/帮助` + Echo）
- 消息去重，避免重复回复

## 运行

```bash
cd ~/Desktop/AMClaw
cargo run
```
