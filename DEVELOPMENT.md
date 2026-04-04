# DEVELOPMENT.md

# AMClaw Development Guide

感谢你继续维护 `AMClaw`。

这个仓库目前更像一个持续演进中的个人 Agent / Bot 实验项目，而不是完全稳定的通用产品。因此，提交改动时请优先保证：

1. 当前可运行链路不被破坏
2. 文档描述与真实实现一致
3. 模块边界继续保持清晰
4. 敏感信息、本地数据和调试产物不进入版本库

## 1. 开始之前

建议先阅读以下文件，按这个顺序理解项目：

1. `README.md`
   - 看“当前实现”、运行方式、已有命令和测试范围
2. `AGENTS.md`
   - 看仓库级目标、约束和模块边界
3. `PLAN.md`
   - 看当前中期路线
4. `NEXT-STEPS.md`
   - 看最近阶段的执行备忘
5. 对应模块下的 `AGENTS.md` / `CLAUDE.md`
   - 看该模块的职责边界与不做事项

如果文档描述与代码行为冲突，处理优先级如下：

1. `README.md` 中“当前实现”
2. 根目录与模块目录下的 `AGENTS.md`
3. 实际代码行为
4. `PLAN.md` / `DESIGN-0.1.0.md` 中的未来规划

## 2. 环境要求

- Rust 2021
- 可访问微信 iLink 接口的网络环境
- 如需启用 LLM 规划，至少配置一个 Provider 的环境变量
- 如需启用公众号浏览器抓取，需在 `config.toml` 中显式开启浏览器配置

启动时会尝试加载这些本地环境文件：

- `.env.deepseek.local`
- `.env.deepseek`
- `.env.moonshot.local`
- `.env.moonshot`

也可以直接通过 shell 设置环境变量，例如：

- `OPENAI_API_KEY`
- `OPENAI_BASE_URL`
- `OPENAI_MODEL`
- `DEEPSEEK_API_KEY`
- `DEEPSEEK_BASE_URL`
- `DEEPSEEK_MODEL`
- `MOONSHOT_API_KEY`
- `MOONSHOT_BASE_URL`
- `MOONSHOT_MODEL`

## 3. 常用开发命令

基础检查：

```bash
cargo check
```

格式检查：

```bash
cargo fmt --check
```

静态检查：

```bash
cargo clippy --all-targets --all-features
```

运行微信 Bot：

```bash
cargo run
```

运行 Agent Demo：

```bash
AMCLAW_AGENT_DEMO_COMMAND='读文件 README.md' cargo run
```

运行测试：

```bash
cargo test
```

## 4. 提交改动的基本原则

### 4.1 优先修根因

尽量解决根本问题，不做只覆盖表面的补丁。

### 4.2 不顺手扩大改动面

除非明确需要，不要顺手重命名、重构或清理无关代码。

### 4.3 保持文档同步

以下变更必须同步更新文档：

- 用户可见命令发生变化
- 任务状态流转发生变化
- 运行方式或配置项发生变化
- 模块职责边界发生变化
- 新增或移除重要能力

最少要检查这些文件是否需要更新：

- `README.md`
- 根目录 `AGENTS.md`
- 对应目录下的 `AGENTS.md`
- 同目录 `CLAUDE.md`

### 4.4 保持 `AGENTS.md` 与 `CLAUDE.md` 一致

`CLAUDE.md` 是给只识别该文件名的助手使用的镜像规则文件。  
如果更新了某目录下的 `AGENTS.md`，通常也应同步更新同目录 `CLAUDE.md`。

## 5. 模块边界提醒

修改前先确认自己是否在正确模块动手：

- `src/chat_adapter`
  - 负责微信 iLink 登录、轮询、消息接入、回复和命令接线
- `src/command_router`
  - 负责文本路由、命令解析、URL 提取
- `src/task_store`
  - 负责 SQLite 持久化、消息去重、文章与任务状态
- `src/pipeline`
  - 负责任务消费、HTTP / 浏览器抓取、归档生成、人工补录接续
- `src/agent_core`
  - 负责最小 Agent loop 与 LLM 调用入口
- `src/tool_registry`
  - 负责工具注册、文件工具与路径边界限制

如果一个改动明显跨越多个模块，优先先确认边界是否真的需要调整，再动代码。

## 6. 提交前检查清单

每次提交前至少确认：

- 能通过 `cargo check`
- 改动范围和目标一致
- 没有硬编码 token、cookie、授权头等敏感信息
- 没有误提交本地配置和运行产物
- 相关文档已经同步
- 如涉及关键链路，已经做过足够验证

建议额外确认：

- `cargo fmt --check`
- `cargo clippy --all-targets --all-features`
- `cargo test`

## 7. 哪些改动需要人工回归

以下改动建议执行完整人工回归：

- 登录、轮询、收发消息链路
- 消息去重或 `context_token` 逻辑
- 主流程入口或关键配置项
- 浏览器抓取、截图、正文归档
- 任务重试、状态查询、人工补录链路

建议回归顺序：

1. `cargo check`
2. `cargo run`
3. 微信扫码登录
4. 发送普通文本，确认可收到自动回复
5. 验证重复消息不会重复回复
6. 发送链接，确认任务入库和状态推进
7. 如影响浏览器抓取，验证公众号链接至少能进入 `archived` 或 `awaiting_manual_input`
8. `Ctrl+C` 退出，确认优雅结束

## 8. 不应提交的内容

默认不提交以下本地产物：

- `.env.*.local`
- `config.toml`
- `data/`
- `target/`
- 浏览器抓取缓存或临时截图
- 含真实 token、cookie、Authorization 的任何文件

如果确实需要提交示例配置，请使用脱敏模板，而不是直接提交真实内容。

## 9. Commit 建议

提交信息尽量直接说明目的，例如：

- `docs: sync repo guidance and assistant docs`
- `feat: add manual task query command`
- `fix: avoid duplicate reply on repeated inbound messages`
- `refactor: simplify task status rendering`

如果改动同时涉及行为变化和文档同步，优先保证描述能体现行为变化本身。

## 10. 如果你是未来的自己

优先做这三件事：

1. 先确认 `README.md` 写的“当前实现”还准不准
2. 再确认 `AGENTS.md` / `CLAUDE.md` 有没有漂移
3. 最后再决定是继续堆功能，还是先补基建
