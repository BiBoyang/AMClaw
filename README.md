# AMClaw

AMClaw 是一个用 Rust 编写的实验性项目，当前主要包含两部分能力：

1. 微信 iLink Bot Demo
2. 一个最小可运行的文件型 Agent 原型

这个项目主要基于我自己的使用习惯来设计，同时也把它当作一个学习 Rust 开发 Agent 的持续实验场。

当前仓库代码版本是 `0.3.0`，请优先以本文档描述的“当前实现”为准。

仓库里已经有更完整的架构设计和实施计划，但当前代码实现仍处于 Demo / 验证阶段。`DESIGN-0.1.0.md` 和 `PLAN.md` 代表后续目标，而不是现状。

## 当前实现

### 微信 Bot

- 扫码登录微信 iLink
- 长轮询 `getupdates` 接收消息
- 缓存每个用户的 `context_token`
- 文本消息进入会话层，支持 `..` / `!!` / 超时合并
- 待提交聊天会话会持久化，进程重启后可恢复最小会话状态
- 支持显式记录用户记忆：`记住 <content>`，并可用 `我的记忆` 查询
- 支持从部分聊天表达中自动提炼最小“主题 / 偏好”记忆
- 入站消息去重，避免重复回复
- 入站原文持久化到 SQLite
- 对链接消息自动入库 `articles` / `tasks`
- 自动消费 `pending` 任务并生成本地归档产物
- 普通 HTTP 网页会尝试最小正文抽取，并记录 `source=http` 与 `page_kind`
- 对公众号链接支持浏览器抓取链路
- 抓取受限时进入 `awaiting_manual_input`，支持人工补正文
- 支持任务状态查询、最近任务查询与任务重试
- `retry` 会同步推进任务并直接返回当前最终状态
- 内置简单回复规则：`hello` / `你好` / `时间` / `帮助`
- 非命令消息默认 Echo 回复
- 真实聊天触发的 Agent run 会写入 `data/agent_traces/<date>/`，并附带 `source_type`、`user_id`、`message_ids` 等上下文
- 支持生成本地日报 Markdown，并可按配置时间自动触发

### Agent Demo

- 支持一个正在演进中的 Agent runtime：
  - 最小多步 `ReAct`
  - 显式 `plan`
  - `progress_note`
  - `step_status`
  - `current_step_index`
  - `expected_observation`（含 `expected_fields` / `minimum_novelty`）
  - 最小 `watchdog`（含 `stalled_trajectory`）
  - 最小 `retry_step / replan / ask_user / abort`
  - 最小 `planner / executor / watchdog` helper 分层
  - 最小 `state/controller` budget（`max_steps` / `replan_budget`）
- 内置文件工具：
  - `read`
  - `write`
  - `create`
- 内置只读业务工具：
  - `get_task_status`
  - `list_recent_tasks`
  - `list_manual_tasks`
  - `read_article_archive`
- 对工具访问路径做工作区边界限制
- 支持多 Provider 规划调用：
  - `DEEPSEEK`
  - `MOONSHOT`
  - `OPENAI`
- 当 LLM 不可用时，可回退到规则解析命令

### Agent Trace

- 每次 Agent run 默认会落盘：
  - JSON trace
  - Markdown trace
  - 每日 `index.jsonl`
  - 每日可读总览 `index.md`
- Demo 入口触发的 trace 会标记 `source_type=agent_demo`
- 真实微信聊天触发的 trace 会标记 `source_type=wechat_chat`，并补充 `trigger_type`、`user_id`、`message_ids`

### 日报 / 调度

- 支持按 `config.toml` 中的 `[scheduler]` 配置每天生成一份本地日报
- 日报会写入 `data/reports/daily-YYYY-MM-DD.md`
- 当前日报聚合的是当天 `archived` 任务
- 若配置 `[scheduler].report_to_user_id`，并且该用户已有可用 `context_token`，程序会尝试按计划自动把日报摘要回传到微信
- 如需手动一次性生成某天日报，可运行：

```bash
cd ~/Desktop/AMClaw
AMCLAW_GENERATE_DAILY_REPORT_FOR=2026-04-10 cargo run
```

## 当前未实现

以下内容在设计文档中有规划，但当前仓库里还没有完整落地：

- 通用网页的高质量正文抽取、分类与摘要
- 更成熟的 `Plan & Execute` executor
- 更强的长期主题记忆与语义检索
- Web 控制台
- 完整的 `restricted / unrestricted` 模式治理

## 项目结构

当前真正承载运行逻辑的模块主要是：

- `src/main.rs`：启动入口、加载环境变量与 `config.toml`
- `src/chat_adapter`：微信 iLink 登录、轮询、会话接线、收发消息
- `src/command_router`：聊天流 / 链接流 / 查询命令分流
- `src/task_store`：SQLite 初始化、入站消息持久化、任务读写
- `src/pipeline`：任务消费、HTTP / 浏览器抓取、归档生成、人工补录归档
- `src/config.rs`：配置加载与路径解析
- `src/session_router.rs`：会话缓冲、超时 flush、最小会话恢复
- `src/agent_core`：Plan-aware ReAct runtime、执行状态、失败分类与最小 watchdog
- `src/tool_registry`：文件工具执行与路径校验
- `src/scheduler`：本地日报的定时触发
- `src/reporter`：本地日报 Markdown 生成

其余模块目录目前主要用于占位和约束未来职责。

## 环境要求

- Rust 2021
- 可用的网络环境，用于访问微信 iLink 接口
- 如需启用 LLM 规划，需要配置至少一个 Provider 的环境变量

### 已自动加载的本地配置文件

启动时会按顺序尝试加载以下文件：

- `.env.deepseek.local`
- `.env.deepseek`
- `.env.moonshot.local`
- `.env.moonshot`

建议先从示例模板复制，再填入真实值：

```bash
cp .env.deepseek.example .env.deepseek
cp .env.moonshot.example .env.moonshot
```

如果你使用 OpenAI，可直接在 shell 环境中设置：

- `OPENAI_API_KEY`
- `OPENAI_BASE_URL`
- `OPENAI_MODEL`

也可设置其他 Provider：

- `DEEPSEEK_API_KEY`
- `DEEPSEEK_BASE_URL`
- `DEEPSEEK_MODEL`
- `MOONSHOT_API_KEY`
- `MOONSHOT_BASE_URL`
- `MOONSHOT_MODEL`

### 本地配置文件

首次启动会自动生成 `config.toml`。默认情况下：

- SQLite 数据库写到 `./data/amclaw.db`
- 会话合并窗口为 5 秒
- 微信 channel version 为 `1.0.0`
- 浏览器抓取默认关闭，需要显式在 `[browser]` 下启用
- `.env*.example` 是可提交模板；真实 `.env` 文件仅用于本地开发，不应提交

## 运行

### 运行微信 Bot

```bash
cd ~/Desktop/AMClaw
cargo run
```

启动后会打印二维码 URL，扫码完成登录，然后进入长轮询收消息。

### 当前支持的微信命令

- 发送普通文本：进入聊天流
- 发送带 `http://` / `https://` 的消息：直接作为链接任务入库
- 发送裸域名链接，例如 `mp.weixin.qq.com` 或 `mp.weixin.qq.com/s/...`：会自动补成 `https://` 后入库
- `收藏 <url>`：显式提交链接任务
- `状态 <task_id>` / `status <task_id>`：查询任务状态
- `最近任务`：查询最近任务列表
- `日报` / `日报 <YYYY-MM-DD>` / `今日整理`：生成并返回当天日报摘要
- `记住 <content>`：写入一条用户记忆
- `我的记忆`：查看当前已保存的用户记忆
- `重试 <task_id>` / `retry <task_id>`：重新处理任务，并直接返回处理后的当前状态
- `补正文 <task_id> :: <content>`：对待人工补录任务手动写入正文
- `待补录任务`：查看当前等待人工补正文的任务
- `帮助` / `help`

### 公众号抓取策略

- `mp.weixin.qq.com` 链接优先走浏览器抓取链路
- 浏览器抓取成功时，会保留：
  - 原始 HTML
  - 全页截图
  - 提取后的标题与正文归档
- 截图前会主动触发公众号长文懒加载图片，并等待图片渲染后再截
- 如果页面被验证码、权限或错误页拦截：
  - 原始链接仍然保留
  - 任务进入 `awaiting_manual_input`
  - 可用 `补正文 <task_id> :: <content>` 继续归档
- 真实 Bot 中已经完成过一次公众号链接端到端抓取验证

### 普通网页抓取策略

- 非公众号链接默认走 HTTP 抓取链路
- HTTP 抓取成功时，会保留原始 HTML，并生成本地 Markdown 归档
- 归档时会优先尝试从 `<article>` / `<main>` / `<body>` 提取正文；提取不到时退回短预览
- 归档产物会记录 `source=http` 与最小 `page_kind` 判断，目前仅区分 `article` / `webpage`

### 运行 Agent Demo

```bash
cd ~/Desktop/AMClaw
AMCLAW_AGENT_DEMO_COMMAND='读文件 README.md' cargo run
```

支持的规则命令格式：

- `读文件 <path>`
- `创建文件 <path> :: <content>`
- `写文件 <path> :: <content>`
- `read <path>`
- `create <path> :: <content>`
- `write <path> :: <content>`

也支持带前缀的自然语言包装：

- `帮我运行：读文件 README.md`
- `请帮我运行: 创建文件 demo/a.txt :: hello`

## 测试

```bash
cd ~/Desktop/AMClaw
cargo test
```

当前测试主要覆盖：

- Agent 命令解析
- 聊天流 `..` / `!!` / 超时合并
- 最小会话恢复
- 用户记忆写入与查询
- 链接流路由与 URL 提取
- SQLite 表结构、消息去重、入站消息持久化
- `articles` / `tasks` 入库、状态查询、最近任务、任务重试
- 人工补正文闭环
- 公众号错误页 / 验证页识别
- 浏览器抓取归档的正文提取
- 待人工补录任务的查询与恢复
- 日报查询命令
- 工具路径边界限制
- 最小 Agent loop 行为
- 日报生成与调度时间解析
- 仓库内 scope 标记文件存在性检查

## 协作与文档约定

- `README.md`：面向人类读者，描述“当前能跑什么、怎么跑、怎么验证”。
- `AGENTS.md`：仓库或模块级开发约束；修改模块职责或边界时，要同步更新对应目录的 `AGENTS.md`。
- `CLAUDE.md`：给只识别该文件名的助手使用；内容应与同目录 `AGENTS.md` 保持一致，避免指令漂移。
- 影响用户可见行为、命令、任务状态或运行方式的改动，必须同步更新 `README.md`。
- 日常改动后至少执行 `cargo check`；提交前建议再跑 `cargo fmt --check` 和 `cargo clippy --all-targets --all-features`。
- 本地环境文件、运行配置和数据库默认不提交：如 `.env`、`.env.*`、`config.toml`、`data/`；示例模板 `*.example` 除外。

## 文档说明

- `DESIGN-0.1.0.md`：目标架构与版本设计
- `PLAN.md`：当前实施路线
- `NEXT-STEPS.md`：当前阶段执行备忘
- `CORE-SKILL-ROADMAP.md`：`core/runtime`、`service/tool` 与 `skill` 的分层路线
- `AGENT-RUNTIME-IMPLEMENTATION-PLAN.md`：`agent_core`、上下文与 `Tool Use` 的分阶段实施计划
- `AGENT-RUNTIME-01-REACT-FOUNDATION-2026-04-10.md`：最小 ReAct runtime 的实现记录与技术选型原因
- `AMCLAW-FEATURE-ROADMAP.md`：未来功能路线与优先级、分层归属建议
- `DEVELOPMENT.md`：后续开发、维护与提交流程说明
- `LOGGING.md`：当前结构化日志字段、事件名与扩展约定

如果你准备继续开发这个项目，建议先读这三个文件，再看 `src/chat_adapter`、`src/command_router`、`src/task_store` 和 `src/agent_core` 的当前实现。
