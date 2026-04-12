# NEXT-STEPS

当前这份文件只记录"接下来最值得做什么"，不重复描述已经完成的能力。当前真实状态请看 `PLAN.md`。

## 本阶段收口（截至 2026-04-12）

以下主线可视为已完成并进入稳定维护：

- Plan-aware ReAct 主链路（含失败语义与最小 watchdog）
- 通用 HTTP 归档最小 summary（规则法）与 `summary` 落库
- `page_kind` 五分类（`error_page/article/index_like/link_post/webpage`）
- reporter / 日报对 `summary` 的展示接入
- 发布流程与文档结构整理（`notes/`、`sessions/`）
- Memory v3：`search_user_memories` 接入 agent_core context 拼装 + 命中回写 + 可观测日志 + 回归测试

结论：v0.3.2 "Context & Memory Minimal" 可以收口。

## v0.3.2 DoD 逐项确认

1. ✅ 显式记忆可命中：`记住 我喜欢短摘要` 后，下一轮问答可体现偏好
2. ✅ 用户隔离有效：A 用户记忆不会注入到 B 用户（回归测试已覆盖）
3. ✅ 长度治理有效：context/memory 注入有预算（5 条 / 500 字符 / 单条 160 字符）
4. ✅ 退化正常：无记忆 / 无 user_id 时系统不报错，行为可回退到当前基线
5. ✅ 可观测：日志有 `memory_hit_count`、`memory_total_chars`、`memory_ids`；Trace 有 `memory_hit_count` / `memory_total_chars`

## 当前主线（v0.3.3）

### 目标

基于已落地的 `Memory v3`，把 AMClaw 的 `context / memory` 从“最小可用”推进到“可稳定演进”。

### 方向

1. 先收口现有 memory 语义与观测
2. 再补显式 `session state`
3. 然后抽结构化 `context pack`
4. 之后再扩少量高价值长期 memory
5. 最后用 trace 驱动评测闭环

统一路线文档：

- `notes/context-memory/CONTEXT-MEMORY-EVOLUTION-ROADMAP-2026-04-13.md`

## v0.3.3 推荐执行顺序

### Phase 1：收口 `Memory v3` 语义

优先要做：

1. 统一自动记忆与显式记忆的写入语义
2. 区分 `retrieved_memory_count` / `injected_memory_count`
3. 明确 `use_count` 的真实含义
4. 保持日志、trace、文档三者口径一致

### Phase 2：补显式 `SessionState`

优先要做：

1. 引入最小状态槽位：
   - `goal`
   - `current_subtask`
   - `constraints`
   - `confirmed_facts`
   - `done_items`
   - `next_step`
   - `open_questions`
2. 让这些槽位进入 trace 与 prompt
3. 保证无状态时仍可退化运行

### Phase 3：抽 `ContextPack`

优先要做：

1. 把“当前喂给模型的内容”抽象成结构体
2. 拆清来源：
   - runtime context
   - session state
   - business context
   - memories
   - latest observation
   - active plan
3. 让 trace 同时保留：
   - 结构化 context pack
   - 最终渲染 prompt

### Phase 4：扩长期 Memory 类型

只优先考虑三类：

1. `user_preference`
2. `project_fact`
3. `lesson`

### Phase 5：建立 Trace 驱动评测闭环

优先要做：

1. 从真实 trace 中抽样
2. 标注失败类型
3. 比较机制变更前后差异
4. 一次只验证一个机制

### 不优先做

- 不先上 embedding / 向量库
- 不先做复杂 memory taxonomy
- 不先做多用户/多任务架构重构
- 不先做 `tokio` 全量迁移或 `sqlx` async 化
- 不回头重写 ReAct / Planning 主框架

## 当前明确不优先做

- 不先上 embedding / 向量库
- 不先做复杂 memory taxonomy
- 不先做多用户/多任务架构重构
- 不先做 `tokio` 全量迁移或 `sqlx` async 化
- 不回头重写 ReAct / Planning 主框架
- 不同时叠加多个 memory 机制
