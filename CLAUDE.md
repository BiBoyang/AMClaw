# AGENTS.md

## 作用

本文件是仓库级通用约束，描述项目目标、目录边界与统一开发规则。  
细分模块规则写在对应子目录下的 `AGENTS.md`。

## 项目目标（0.1.0）

`AMClaw` 是 Rust 版微信 iLink Bot 学习项目，当前目标是先打通可运行闭环：

1. 微信扫码登录
2. 长轮询接收消息
3. 解析文本并回消息
4. 支持 Ctrl+C 优雅退出

详细范围以 `DESIGN-0.1.0.md` 为准。

## 全局约束

1. 默认走受限能力，不引入任意系统控制能力。
2. 禁止在代码中硬编码 token、cookie、授权头等敏感信息。
3. 改动协议字段时，保留足够调试日志便于排查。
4. 任何写文件能力后续必须遵守固定目录与路径校验策略。

## 目录索引

1. `src/main.rs`
   - 进程入口与信号处理
   - 调用微信运行入口
2. `src/wechat/`
   - 现有实现：登录、轮询、消息收发
3. `src/wechat_gateway/`
   - 架构目标：微信接入层（替代/演进当前 wechat 模块）
4. `src/command_router/`
   - 架构目标：消息命令解析与 URL 提取
5. `src/task_store/`
   - 架构目标：任务与文章持久化
6. `src/scheduler/`
   - 架构目标：定时任务触发
7. `src/pipeline/`
   - 架构目标：快照、抽取、分类、归档流程
8. `src/agent_core/`
   - 架构目标：LLM 编排与调用入口
9. `src/tool_registry/`
   - 架构目标：工具注册与能力边界
10. `src/mode_policy/`
    - 架构目标：restricted/unrestricted 策略判定
11. `src/reporter/`
    - 架构目标：日报生成与回执摘要

## 开发与校验

每次代码改动后至少执行：

```bash
cargo check
```

提交前建议执行：

```bash
cargo fmt --check
cargo clippy --all-targets --all-features
```

运行项目：

```bash
cargo run
```
