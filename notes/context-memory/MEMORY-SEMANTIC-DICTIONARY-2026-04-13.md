# Memory 语义字典（2026-04-13）

本文件定义 AMClaw Memory 系统中所有关键术语的精确含义。后续文档、代码注释、日志字段均以本文件为准。

---

## 1. 写入来源（memory_type）

| 值 | 含义 | priority | 触发方式 |
|---|---|---|---|
| `explicit` | 用户明确要求系统记住的内容 | 100 | "记住 ..." / "记一下 ..." |
| `auto` | 系统从聊天中自动提炼的偏好或主题 | 60 | 匹配偏好/主题前缀的聊天文本 |

- 显式记忆优先级始终高于自动记忆。
- 自动记忆内容会带上前缀标记：`偏好: ...` 或 `主题: ...`。

## 2. 统计阶段

| 术语 | 含义 | 当前是否单独记录 |
|---|---|---|
| `retrieved` | 从 DB 中按条件取出的候选记忆 | 是（= `memory_retrieved_count`） |
| `injected` | 经去重 + 单条长度 + 总预算裁剪后，实际注入 prompt 的记忆 | 是（= `memory_hit_count`） |
| `useful` | 真正帮助本轮决策、被反馈确认为有用的记忆 | 部分是（有字段与写回能力，但尚无自动判定链路） |

### 当前 `memory_hit_count` 的精确含义

**`memory_hit_count` = `injected` 数量**，即实际注入 prompt 的记忆条数。

它不是"从 DB 取出的数量"，也不是"真正有用的数量"。

## 3. `use_count` 字段语义

**`use_count` = 被确认 useful 的次数**。

- 每次 `apply_memory_feedback(...)` 收到 `Useful` feedback 时 `use_count += 1`。
- 同时会把 `useful = true`，并更新 `last_used_at`。
- 它不代表"被检索次数"，也不代表"被注入次数"。

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
| agent_core 结构化日志 | `memory_hit_count` | 注入条数 |
| agent_core 结构化日志 | `memory_total_chars` | 注入总字符数 |
| agent_core 结构化日志 | `memory_ids` | 注入记忆 ID 列表 |
| AgentRunTrace JSON | `memory_hit_count` | 注入条数 |
| AgentRunTrace JSON | `memory_retrieved_count` | DB 取出条数 |
| AgentRunTrace Markdown | `memory_hit_count (injected)` | 注入条数（标注语义） |
| AgentRunTrace Markdown | `memory_retrieved_count` | DB 取出条数 |
| AgentRunTrace Markdown | `memory_total_chars (injected)` | 注入总字符数（标注语义） |
| task_store 函数注释 | `use_count` | 被确认 useful 的次数 |
| chat_adapter 日志 | `user_memory_auto_recorded` | 自动记忆写入成功事件 |
| chat_adapter 日志 | `user_memory_auto_skipped` | 自动记忆写入跳过事件（含 skip_reason） |
| chat_adapter 日志 | `user_memory_explicit_written` | 显式记忆写入成功 |
| chat_adapter 日志 | `user_memory_explicit_skipped` | 显式记忆写入跳过 |
| chat_adapter 日志 | `user_memory_explicit_promoted` | 显式记忆提升已有 auto |

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
| `AutoWouldDowngradeExplicit` | auto 不允许降级已有 explicit |
| `Invalid` | user_id 或内容格式无效 |
| `StorageError` | 持久化写入失败 |

### PromoteReason variants

| 值 | 含义 |
|---|---|
| `ExplicitPromotesAuto` | 新 explicit 提升了已有 auto 为 explicit |

### 写入规则

1. 空/whitespace → Skip(Empty)
2. 超过 500 字符 → Skip(TooLong)
3. normalize 后与已有相同：
   - auto + 已有 explicit → Skip(AutoWouldDowngradeExplicit)
   - explicit + 已有 auto → Promote(ExplicitPromotesAuto)
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
