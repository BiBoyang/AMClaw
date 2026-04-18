# SessionState 显式化实施计划（C0/C1/C2）

> 日期：2026-04-17
> 范围：仅 SessionState 显式化 + 接线
> 版本：v0.3.3 的子集

---

## C0 | 范围冻结（通过）

### 本轮唯一目标
SessionState 显式化 + 持久化 API + chat/session 到 agent 接线。

### In Scope
- [x] `UserSessionStateRecord` 结构定义（user_id / last_user_intent / current_task / next_step / blocked_reason / updated_at）
- [x] `user_session_states` 表 + 迁移（可重复执行、向后兼容）
- [x] task_store 最小 API：`load_user_session_state` / `upsert_user_session_state` / `clear_user_session_state`
- [x] 单测覆盖：空状态读取、首次写入、覆盖更新、用户隔离、异常输入降级
- [x] chat_adapter 在 agent 运行前加载 session_state 并注入 AgentRunContext
- [x] chat_adapter 在 session flush 时更新 `last_user_intent`
- [x] chat_adapter 在 agent 运行后回写 `updated_at`
- [x] agent_core 将持久化 session_state 合并入 `RuntimeSessionStateSnapshot`
- [x] trace/log 增加最小观测：`state_present` / `state_source` / `state_updated`
- [x] 降级：状态读写失败不阻断主流程，仅记日志

### Out of Scope
- [ ] 向量召回 / embedding
- [ ] memory taxonomy 扩展
- [ ] 新工具能力
- [ ] 复杂评测框架
- [ ] 从 agent 内部推导并回写 deep state（current_task / next_step / blocked_reason 的深度语义推导）
- [ ] 自动化状态压缩/清理策略

### 基线记录
- 当前 `context_eval` 报告路径：`notes/context-memory/SESSION-SUMMARY-EVAL-2026-04-17.md`
- 典型 trace 目录：`data/agent_traces/2026-04-17/`

### 命名约定
为避免与现有代码冲突，采用以下命名：

| 概念 | 已有名称 | 本次新增名称 |
|------|---------|-------------|
| 聊天缓冲持久化 | `StoredSessionRecord` / `user_sessions` 表 | 不变 |
| 用户会话状态（新增） | — | `UserSessionStateRecord` / `user_session_states` 表 |
| 运行时内部状态 | `RuntimeSessionStateSnapshot` | 不变，接收持久化状态后合并 |
| Memory 检索状态 | `SessionState`（agent_core 内） | 不变 |

---

## C1 | SessionState 结构与存储

### 数据模型

```rust
pub struct UserSessionStateRecord {
    pub user_id: String,
    pub last_user_intent: Option<String>,
    pub current_task: Option<String>,
    pub next_step: Option<String>,
    pub blocked_reason: Option<String>,
    pub updated_at: String,
}
```

### 数据库表

```sql
CREATE TABLE IF NOT EXISTS user_session_states (
    user_id          TEXT PRIMARY KEY,
    last_user_intent TEXT,
    current_task     TEXT,
    next_step        TEXT,
    blocked_reason   TEXT,
    updated_at       DATETIME NOT NULL
);
```

### 生命周期
1. **创建**：首次遇到用户时延迟创建（upsert）
2. **读取**：`load_user_session_state(user_id)` → `Option<UserSessionStateRecord>`
3. **覆盖更新**：`upsert_user_session_state(&record)` 全字段覆盖
4. **清空**：`clear_user_session_state(user_id)` 删除该用户记录

### API 风格
延续 task_store 现有命名风格：
- `load_user_session_state`（非 `get_*`，与现有 `load_business_context_snapshot` 对齐）
- `upsert_user_session_state`（与现有 `upsert_context_token` 对齐）
- `clear_user_session_state`（与现有 `delete_session_state` 对齐但操作不同表）

---

## C2 | 状态接线

### 加载点
`chat_adapter::send_generated_reply` 中，在构建 `AgentRunContext` 前：
```rust
let session_state = self.task_store.load_user_session_state(user_id).ok().flatten();
```

### 注入点
`AgentRunContext` 新增 `with_user_session_state` builder 方法，传入 `Option<UserSessionStateRecord>`。

`agent_core::derive_runtime_session_state` 中：
- 若存在持久化 session_state，将其字段合并到 `RuntimeSessionStateSnapshot`
- `last_user_intent` → 影响 goal 推导
- `blocked_reason` → 加入 constraints
- `current_task` / `next_step` → 影响 current_subtask / next_step 推导

### 更新点
1. **Session flush 时更新 intent**：
   `chat_adapter::handle_session_event` / `flush_expired_sessions` 中，根据 `RouteIntent` 提取 `last_user_intent` 并 upsert。

2. **Agent 完成后回写**：
   `send_generated_reply` 中 agent 返回后，更新 `updated_at`（保持最小实现，不深推导 next_step/block）。

### 降级
所有状态读写失败时：
- 不阻断主流程
- 记录 `warn` 级别结构化日志（`event=session_state_*_failed`）
- Agent 无状态注入时行为与现在完全一致

### 观测字段
Trace 新增：
- `persistent_state_present: bool`
- `persistent_state_source: Option<String>`（"db" / "none"）
- `persistent_state_updated: bool`

Log 新增事件：
- `session_state_loaded` / `session_state_load_failed`
- `session_state_upserted` / `session_state_upsert_failed`
- `session_state_cleared`

---

## 文件改动清单（预期）

### C1 文件
- `src/task_store/mod.rs` — 新增结构体、表、API、单测
- `src/task_store/CLAUDE.md` — 更新职责说明

### C2 文件
- `src/agent_core/mod.rs` — AgentRunContext 扩展、derive_runtime_session_state 合并逻辑、trace 字段
- `src/chat_adapter/mod.rs` — 加载、更新、回写接线
- `src/chat_adapter/CLAUDE.md` — 更新职责说明

---

## 通过标准

1. `cargo check` 通过
2. `cargo test` 全部通过（含新增单测）
3. 有状态链路：trace 中可见 `persistent_state_present=true`
4. 无状态链路：trace 中 `persistent_state_present=false`，行为与当前一致
5. 状态读写失败时：主流程不中断，日志中有降级记录
