# AGENTS.md

@scope:global:v1

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
2. `src/chat_adapter/`
   - 现有实现：登录、轮询、消息收发
3. `src/chat_gateway/`
   - 架构目标：多聊天应用接入层（替代/演进当前 chat_adapter 模块）
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

## 大改动全流程回归（必须）

当出现以下任一情况，视为“大改动”，必须执行一次完整人工回归后再合并：

1. 改动 `src/chat_adapter/` 或 `src/chat_gateway/` 的登录、轮询、收发逻辑。
2. 改动消息解析、去重、`context_token` 相关逻辑。
3. 改动主流程入口（`src/main.rs`）或关键配置字段。
4. 改动可能影响端到端链路稳定性的依赖与网络调用代码。

执行步骤（按顺序）：

1. 执行基础静态校验：
   ```bash
   cargo check
   cargo fmt --check
   cargo clippy --all-targets --all-features
   ```
2. 启动程序并走登录流程：
   ```bash
   cargo run
   ```
3. 看到终端输出二维码 URL，使用微信扫码并确认登录。
4. 验证登录成功日志（含 bot_id / user_id），并进入“开始接收消息，长轮询中”状态。
5. 从微信发送一条文本消息（如“你好”），验证程序收到消息并自动回复。
6. 再发送同一条消息或触发重复投递场景，验证不会重复回复。
7. 使用 `Ctrl+C` 结束进程，验证出现优雅退出日志且进程正常结束。

通过标准：

1. 登录、收消息、回复、去重、优雅退出 5 个环节全部通过。
2. 终端无 panic、无未处理错误退出。
