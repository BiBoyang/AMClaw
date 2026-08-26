# Memory 语义字典（2026-04-13）

本文件定义 AMClaw Memory 系统中所有关键术语的精确含义。后续文档、代码注释、日志字段均以本文件为准。

---

## 1. 写入来源（memory_type）

| 值 | 含义 | priority | 触发方式 |
|---|---|---|---|
| `explicit` | 用户明确要求系统记住的内容 | 100 | "记住 ..." / "记一下 ..." |
| `project_fact` | 项目级事实（模块职责、在做主题、约束、边界） | 85 | 匹配主题/在做前缀的聊天文本（自动提炼归位） |
| `user_preference` | 用户偏好（回复风格、输出形式、工作方式） | 80 | 匹配偏好前缀的聊天文本（自动提炼归位） |
| `lesson` | 经验教训（失败模式、有效处理方式） | 75 | agent run 失败收场或升级人工（ask_user）收场时自动沉淀；环境性失败（鉴权/传输）过滤不沉淀 |
| `auto` | 系统自动提炼的未归位内容（兜底类型） | 60 | 历史数据与未归位候选 |

- 显式记忆优先级始终高于自动记忆；完整优先级链：explicit > project_fact > user_preference > lesson > auto。
- 自动提炼内容保留文本前缀标记：`偏好: ...` 或 `主题: ...`，保证与历史 auto 记忆的 dedup/promote 连续性（同一内容再出现时会 promote 既有 auto 记录，而非重复写入）。
- prompt 渲染时由类型标签（`[偏好]` / `[项目]`）承担语义标注，与文本前缀重复时去掉文本前缀，不做双重标注；DB 中内容保持不变。
- lesson 内容由 agent_core 在 run 失败时以 `失败教训: <原因摘要>`、升级人工（ask_user）收场时以 `升级人工: <原因摘要>` 形式写入，走统一 `govern_memory_write` 治理；命中环境性失败特征（鉴权/传输）时不沉淀；写入失败只记日志，不影响主链路。

## 2. 统计阶段

| 术语 | 含义 | 当前是否单独记录 |
|---|---|---|
| `retrieved` | 从 DB 中按条件取出的候选记忆 | 是（= `memory_retrieved_count`） |
| `injected` | 经去重 + 单条长度 + 总预算裁剪后，实际注入 prompt 的记忆 | 是（= `memory_hit_count`） |
| `useful` | 真正帮助本轮决策、被反馈确认为有用的记忆 | 部分是（支持显式确认命令，尚无自动判定链路） |

### 当前 `memory_hit_count` / `memory_injected_count` 的精确含义

**`memory_hit_count` = `memory_injected_count` = `injected` 数量**，即实际注入 prompt 的记忆条数。

它不是"从 DB 取出的数量"，也不是"真正有用的数量"。

两个字段为同一语义的不同名称，同时输出以保障下游兼容。

## 3. `use_count` 字段语义

**`use_count` = 被确认 useful 的次数**。

- 每次 `apply_memory_feedback(...)` 收到 `Useful` feedback 时 `use_count += 1`。
- 同时会把 `useful = true`，并更新 `last_used_at`。
- 它不代表"被检索次数"，也不代表"被注入次数"。
- 当前真实产品触发点：用户显式执行 `有用 <memory_id>` / `标记有用 <memory_id>` / `useful <memory_id>`。

## 4. 记忆状态（status）

| 值 | 含义 |
|---|---|
| `active` | 正常参与检索与注入 |
| `suppressed` | 软删除，不参与检索与注入 |

## 5. 检索排序规则

```sql
ORDER BY priority DESC,
         useful DESC,
         use_count DESC,
         COALESCE(last_used_at, updated_at) DESC,
         id ASC
```

1. 显式记忆（priority=100）优先于自动记忆（priority=60）
2. 被标记 `useful` 的优先
3. `use_count` 更高的优先
4. 最近 useful/更新的优先
5. `id ASC` 作为最终稳定 tie-breaker

## 6. 注入预算

| 参数 | 值 | 含义 |
|---|---|---|
| `MAX_MEMORY_ITEMS` | 5 | 最多注入 5 条记忆 |
| `MAX_MEMORY_TOTAL_CHARS` | 500 | 注入记忆总字符数上限 |
| `MAX_SINGLE_MEMORY_CHARS` | 160 | 单条记忆字符数上限 |

## 7. 日志 / Trace / 文档口径

| 场景 | 字段名 | 含义 |
|---|---|---|
| agent_core 结构化日志 | `memory_retrieved_count` | DB 取出条数 |
| agent_core 结构化日志 | `memory_hit_count` | 注入条数（= `memory_injected_count`） |
| agent_core 结构化日志 | `memory_injected_count` | 注入条数（= `memory_hit_count`，兼容字段） |
| agent_core 结构化日志 | `memory_total_chars` | 注入总字符数（= `memory_injected_total_chars`） |
| agent_core 结构化日志 | `memory_injected_total_chars` | 注入总字符数（= `memory_total_chars`，兼容字段） |
| agent_core 结构化日志 | `memory_ids` | 注入记忆 ID 列表 |
| AgentRunTrace JSON | `memory_hit_count` | 注入条数 |
| AgentRunTrace JSON | `memory_injected_count` | 注入条数（兼容字段） |
| AgentRunTrace JSON | `memory_retrieved_count` | DB 取出条数 |
| AgentRunTrace JSON | `memory_total_chars` | 注入总字符数 |
| AgentRunTrace JSON | `memory_injected_total_chars` | 注入总字符数（兼容字段） |
| AgentRunTrace JSON | `memory_dropped` | 被裁剪记忆明细（id/preview/reason） |
| AgentRunTrace Markdown | `memory_hit_count (injected)` | 注入条数（标注语义） |
| AgentRunTrace Markdown | `memory_retrieved_count` | DB 取出条数 |
| AgentRunTrace Markdown | `memory_total_chars (injected)` | 注入总字符数（标注语义） |
| task_store 函数注释 | `use_count` | 被确认 useful 的次数 |
| chat_adapter 日志 | `user_memory_auto_recorded` | 自动提炼记忆写入成功事件（含 memory_type 字段，Phase 4 起为 user_preference/project_fact） |
| chat_adapter 日志 | `user_memory_auto_skipped` | 自动提炼记忆写入跳过事件（含 skip_reason、memory_type） |
| chat_adapter 日志 | `user_memory_auto_promoted` | 归位后的类型提升历史 auto 记忆（Phase 4 新增） |
| chat_adapter 日志 | `user_memory_explicit_written` | 显式记忆写入成功 |
| chat_adapter 日志 | `user_memory_explicit_skipped` | 显式记忆写入跳过 |
| chat_adapter 日志 | `user_memory_explicit_promoted` | 显式记忆提升已有 auto |
| agent_core 结构化日志 | `memory_lesson_recorded` | 失败收场时 lesson 记忆写入成功 |
| agent_core 结构化日志 | `memory_lesson_env_filtered` | 环境性失败（鉴权/传输）被过滤、不沉淀 lesson（含 category 字段） |
| agent_core 结构化日志 | `memory_lesson_skipped` | lesson 写入被治理跳过（含 skip_reason） |
| agent_core 结构化日志 | `memory_lesson_promoted` | lesson 提升已有低优先级记忆 |
| agent_core 结构化日志 | `memory_lesson_persist_failed` | lesson 写入链路自身失败（只记日志，不影响主链路） |

## 8. 写侧治理术语（Phase 3）

### 写入管线

```
candidate → validate → dedup → promote/skip → persist → trace/log
```

### WriteDecision variants

| 值 | 含义 |
|---|---|
| `Written(record)` | 新写入成功 |
| `Skipped { reason }` | 跳过写入，reason 解释原因 |
| `Promoted { id, reason }` | 提升已有记录（更新 type/priority） |

### SkipReason variants

| 值 | 含义 |
|---|---|
| `Empty` | 内容为空或仅 whitespace |
| `TooLong` | 内容超过 500 字符 |
| `TooWeak` | 自动记忆置信度不足（预留） |
| `Duplicate` | 与已有同类型记忆规范化后重复 |
| `LowerPriorityWouldDowngradeHigher` | 低优先级类型不能覆盖高优先级类型 |
| `Invalid` | user_id 或内容格式无效 |
| `StorageError` | 持久化写入失败 |

### PromoteReason variants

| 值 | 含义 |
|---|---|
| `TypePromotesLower` | 高优先级类型提升了已有低优先级类型记忆 |

### 写入规则

1. 空/whitespace → Skip(Empty)
2. 超过 500 字符 → Skip(TooLong)
3. normalize 后与已有相同：
   - auto + 已有 explicit → Skip(LowerPriorityWouldDowngradeHigher)
   - explicit + 已有 auto → Promote(TypePromotesLower)
   - 同类型 → Skip(Duplicate)
4. 确实不同 → WriteNew

### MemoryWriteState 字段

| 字段 | 含义 |
|---|---|
| `candidate_count` | 本轮候选写入数量 |
| `written` | 成功写入的记录列表 |
| `skipped` | 跳过列表（content_preview + SkipReason） |
| `promoted` | 提升列表（id + PromoteReason） |

## 9. 后续扩展预留

以下统计在 Phase 1 不自动判定，但语义定义预留：

- `useful_memory_count`：真正帮助决策的记忆数量（需额外判定机制）
