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
| `useful` | 真正帮助本轮决策的记忆 | 否（不自动判定，字段预留） |

### 当前 `memory_hit_count` 的精确含义

**`memory_hit_count` = `injected` 数量**，即实际注入 prompt 的记忆条数。

它不是"从 DB 取出的数量"，也不是"真正有用的数量"。

## 3. `use_count` 字段语义

**`use_count` = 被注入 prompt 的次数**（方案 B）。

- 每次 `mark_memories_used` 被调用时 `use_count += 1`。
- 调用时机：记忆经 `search_user_memories` 检索并注入 prompt 后。
- 它不代表"被检索次数"，也不代表"真正有用次数"。

## 4. 记忆状态（status）

| 值 | 含义 |
|---|---|
| `active` | 正常参与检索与注入 |
| `suppressed` | 软删除，不参与检索与注入 |

## 5. 检索排序规则

```sql
ORDER BY priority DESC,
         COALESCE(last_used_at, updated_at) DESC,
         use_count DESC
```

1. 显式记忆（priority=100）优先于自动记忆（priority=60）
2. 最近使用/更新的优先
3. 高频使用的优先

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
| task_store 函数注释 | `use_count` | 被注入 prompt 次数 |
| chat_adapter 日志 | `user_memory_auto_recorded` | 自动记忆写入成功事件 |
| chat_adapter 日志 | `user_memory_auto_extract_failed` | 自动记忆写入失败事件 |

## 8. 后续扩展预留

以下统计在 Phase 1 不自动判定，但语义定义预留：

- `useful_memory_count`：真正帮助决策的记忆数量（需额外判定机制）
