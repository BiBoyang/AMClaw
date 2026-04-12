# State Management 与 Context/Memory 设计纪要（2026-04-12）

## 目的

本纪要用于沉淀“从状态管理定义到 v0.3.2 最小设计”的讨论结果，作为下一步开发的直接输入。

## 一句话结论

`状态管理` 的目标不是增加复杂度，而是让 Agent 始终知道：

1. 现在走到哪一步；
2. 下一步该做什么；
3. 哪些信息该用、哪些不该用。

如果没有状态管理，`context/memory` 会失控，表现为：命中不稳定、旧信息污染、失败不可回退、评测不可比。

## 我们讨论后确认的范围（Now 阶段）

Now 阶段优先能力：

- `6. 状态管理`
- `2. 上下文工程`
- `5. Memory 设计`
- `7. 错误恢复与反馈闭环`
- `9. 安全与权限边界`

执行顺序（含依赖）：

1. `6 状态管理`
2. `2 上下文工程`
3. `5 Memory 设计`
4. `7 错误恢复`
5. `9 安全边界`（贯穿全程，阶段收口时做门禁）

## 状态管理到底在管理什么

结合 AMClaw 当前代码，状态管理对象分 4 类：

1. **Task 状态**
   - `pending / archived / failed / awaiting_manual_input`
2. **Session 状态**
   - 会话合并内容、消息集合、提交与清理时机
3. **Memory 状态**
   - 记忆类型、活跃性、优先级、命中历史、生命周期
4. **Controller 状态**
   - step 进度、失败次数、replan 预算、ask_user 次数

## 为什么必须先做状态管理

### 目标收益

- 连续性：重启后行为一致
- 可控性：避免无限循环与记忆污染
- 可解释性：能追溯“为什么这样决策”
- 可恢复性：失败可回退，不阻断主链路
- 可评测性：输入稳定，结果可比较

### 不做会出现的问题

- memory 注入不可预测，回复风格漂移
- 自动提炼噪声累积，prompt 膨胀
- 失败后无法区分“检索失败”还是“注入失败”
- 相同问题多次运行结果不一致

## v0.3.2 最小可落地设计（MVP）

### 1) ContextSnapshot（先定义结构，再消费）

建议最小结构：

- `user_id: Option<String>`
- `session_text: Option<String>`（截断后）
- `current_task: Option<TaskLite>`
- `recent_tasks: Vec<TaskLite>`（最多 3）
- `memories: Vec<MemoryLite>`（预算内）
- `context_token_present: bool`

`TaskLite` 最小字段：

- `task_id`
- `status`
- `page_kind`
- `content_source`
- `normalized_url`
- `last_error`（可选、截断）

`MemoryLite` 最小字段：

- `id`
- `content`
- `memory_type`
- `priority`
- `last_used_at`
- `use_count`

### 2) `user_memories` 最小状态字段（DB 自动迁移）

在现有 `user_memories` 基础上补齐：

- `memory_type TEXT NOT NULL DEFAULT 'explicit'`
- `status TEXT NOT NULL DEFAULT 'active'`（`active|suppressed`）
- `priority INTEGER NOT NULL DEFAULT 100`
- `last_used_at DATETIME`（nullable）
- `use_count INTEGER NOT NULL DEFAULT 0`

### 3) 检索与注入规则（先规则法，不上向量库）

- 过滤：`status='active'`
- 排序：
  1. `priority DESC`
  2. `COALESCE(last_used_at, updated_at) DESC`
  3. `use_count DESC`
- 去重：规范化 content 后去重（trim + 多空格压缩）
- 注入预算建议：
  - `MAX_MEMORY_ITEMS = 5`
  - `MAX_MEMORY_TOTAL_CHARS = 500`
  - `MAX_SINGLE_MEMORY_CHARS = 160`

### 4) 命中回写（反馈闭环最小动作）

当 memory 被注入 prompt：

- `use_count = use_count + 1`
- `last_used_at = now`

### 5) 写入策略（最小）

- 显式命令 `记住 ...`
  - `memory_type='explicit'`
  - `priority=100`
- 自动提炼
  - `memory_type='auto'`
  - `priority=60`
- 暂不硬删除噪声，先支持 `status='suppressed'`

## 当前明确不做（避免过度设计）

- 不上 embedding / 向量库
- 不做复杂 memory taxonomy
- 不做跨 Agent 共享记忆协议
- 不做大规模 runtime 重构

## 开发拆分建议

### PR1（状态底座）

- `task_store`：schema + store API + migration + tests
- 不改 planner prompt 结构（只准备数据能力）

### PR2（消费接入）

- `agent_core`：按新状态检索/注入 memory
- 增加命中日志与回写逻辑

## DoD（本阶段验收）

1. 显式记忆命中可验证；
2. 用户隔离不串用；
3. 注入预算生效；
4. 检索/注入失败可回退；
5. 日志可观测（命中条数、注入长度、来源）。
