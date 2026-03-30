# AMClaw

AMClaw 是一个用 Rust 编写的实验性项目，当前主要包含两部分能力：

1. 微信 iLink Bot Demo
2. 一个最小可运行的文件型 Agent 原型

仓库里已经有更完整的架构设计和实施计划，但当前代码实现仍处于 Demo / 验证阶段。请优先以本文档描述的“当前实现”为准，`DESIGN-0.1.0.md` 和 `PLAN.md` 代表后续目标，而不是现状。

## 当前实现

### 微信 Bot

- 扫码登录微信 iLink
- 长轮询 `getupdates` 接收消息
- 缓存每个用户的 `context_token`
- 对文本消息自动回复
- 内置简单回复规则：`hello` / `你好` / `时间` / `帮助`
- 非命令消息默认 Echo 回复
- 消息去重，避免重复回复

### Agent Demo

- 支持一个最小 Agent loop：决策 -> 调工具 -> 返回结果
- 内置文件工具：
  - `read`
  - `write`
  - `create`
- 对工具访问路径做工作区边界限制
- 支持多 Provider 规划调用：
  - `DEEPSEEK`
  - `MOONSHOT`
  - `OPENAI`
- 当 LLM 不可用时，可回退到规则解析命令

## 当前未实现

以下内容在设计文档中有规划，但当前仓库里还没有完整落地：

- `task_store` / SQLite 持久化
- `command_router`
- `pipeline`
- `scheduler`
- `reporter`
- 文章链接抓取、快照、抽取、分类、归档
- 每日汇总报告
- 完整的 `restricted / unrestricted` 模式治理

## 项目结构

当前真正承载运行逻辑的模块主要是：

- `src/main.rs`：启动入口、加载环境变量、切换 Bot / Agent Demo 模式
- `src/chat_adapter`：微信 iLink 登录、轮询、收发消息
- `src/agent_core`：最小 Agent 决策循环与 LLM 规划
- `src/tool_registry`：文件工具执行与路径校验

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

## 运行

### 运行微信 Bot

```bash
cd ~/Desktop/AMClaw
cargo run
```

启动后会打印二维码 URL，扫码完成登录，然后进入长轮询收消息。

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
- 工具路径边界限制
- 最小 Agent loop 行为
- 仓库内 scope 标记文件存在性检查

## 文档说明

- `DESIGN-0.1.0.md`：目标架构与版本设计
- `PLAN.md`：当前实施路线

如果你准备继续开发这个项目，建议先读这两个文件，再看 `src/chat_adapter`、`src/agent_core` 和 `src/tool_registry` 的当前实现。
