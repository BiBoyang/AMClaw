# NEXT-STEPS

当前这份文件只记录“接下来最值得做什么”，不重复描述已经完成的能力。当前真实状态请看 `PLAN.md`。

## 本阶段收口（截至 2026-04-12）

以下主线可视为已完成并进入稳定维护：

- Plan-aware ReAct 主链路（含失败语义与最小 watchdog）
- 通用 HTTP 归档最小 summary（规则法）与 `summary` 落库
- `page_kind` 五分类（`error_page/article/index_like/link_post/webpage`）
- reporter / 日报对 `summary` 的展示接入
- 发布流程与文档结构整理（`notes/`、`sessions/`）

结论：上一轮“ReAct / Planning 基础建设”可以收口，下一轮切入 `Context & Memory`。

## 当前主线（v0.3.2）

### 目标

做出 `Context & Memory Minimal`：

- Agent 每次规划/回复都能稳定拿到“当前上下文 + 用户长期记忆”
- 记忆可写、可查、可控（不过度注入、不污染 prompt）
- 行为可观测（能看到命中与注入情况）

### DoD（完成标准）

1. 显式记忆可命中：`记住 我喜欢短摘要` 后，下一轮问答可体现偏好。
2. 用户隔离有效：A 用户记忆不会注入到 B 用户。
3. 长度治理有效：context/memory 注入有预算，不会挤爆 prompt。
4. 退化正常：无记忆/低命中时系统不报错，行为可回退到当前基线。
5. 可观测：日志能看到 `memory_hit_count`、注入长度、命中来源。

## 下一步执行顺序（直接开工）

### M1. 统一 Context Snapshot 入口

在 `agent_core` 增加统一上下文拼装入口（例如 `build_context_snapshot(...)`）：

- 输入：当前 user、session、最近任务、最近归档摘要
- 输出：结构化 snapshot（供 planner/executor 使用）
- 要求：单入口、可测试、可截断

### M2. Memory 检索最小策略（先不用向量库）

在 `task_store` / `agent_core` 落最小可用检索：

- 显式记忆优先于自动提炼记忆
- 关键词 + 时间衰减 + `top_k`
- 去重（同义重复内容不重复注入）

### M3. Prompt 注入与预算治理

- 在 planner 输入中注入 context + memory
- 配置注入预算（例如最大字符数 / 最大条数）
- 超限时按优先级裁剪（显式 > 自动，近期 > 远期）

### M4. 观测与回归

- 增加结构化日志字段：
  - `context_chars`
  - `memory_hit_count`
  - `memory_ids`（或可追踪标识）
- 增加最小回归测试（命中、隔离、裁剪、无记忆退化）

## 当前明确不优先做

- 不先上 embedding / 向量库
- 不先做复杂 memory taxonomy
- 不先做多用户/多任务架构重构
- 不先做 `tokio` 全量迁移或 `sqlx` async 化
- 不回头重写 ReAct / Planning 主框架

## 开工检查清单

下轮开发前，至少保证：

- `README.md`、`AGENTS.md`、`CLAUDE.md` 与当前行为一致
- `cargo check` 可通过
- 关键链路不回退：登录、收消息、任务状态、归档、日报
- `sessions/SESSION-YYYY-MM-DD.md` 已创建并记录本轮目标
