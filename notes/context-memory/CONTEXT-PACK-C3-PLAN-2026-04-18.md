# ContextPack C3 结构化设计冻结文档

> 版本：v0.3.3 ContextPack (C3/C4)
> 日期：2026-04-18
> 状态：设计冻结 — 后续开发不再修改核心口径

---

## 1. 设计目标

将 agent_core 中的上下文组装从"分散字符串拼接"升级为**结构化 ContextPack 单入口渲染**，使：

- 每个 context 来源（runtime、session_state、memory 等）成为显式 section
- 裁剪决策（trim / drop）附带可解释原因
- 所有 prompt 组装可追溯到单一入口
- trace / log / preview 从同一数据结构投影

---

## 2. ContextPack 结构（最小）

```rust
pub struct ContextPack {
    sections: Vec<ContextSection>,
    max_total_chars: usize,
}
```

### 2.1 ContextSection（每段上下文）

```rust
pub struct ContextSection {
    kind: ContextSectionKind,       // section 类型
    lines: Vec<String>,             // 渲染后的行内容
    policy: ContextSectionPolicy,   // 预算与优先级策略
    original_content: String,       // 原始未裁剪内容
    trimmed: bool,                  // 是否被 section 级预算裁剪
    trim_reason: Option<ContextSectionChangeReason>,
    included: bool,                 // 是否被包含在最终 prompt
    drop_reason: Option<ContextSectionChangeReason>,
}
```

### 2.2 Section 类型枚举（ContextSectionKind）

| Kind | 优先级 | 说明 |
|------|--------|------|
| Preamble | 100 | 固定前缀，required |
| CurrentIntent | 100 | 用户输入，required |
| RuntimeContext | 95 | 运行时元信息，required |
| SessionState | 94 | 持久化会话状态投影 |
| SessionText | 55 | 合并后的 session 文本 |
| PreviousObservations | 70 | 历史 observation 摘要 |
| LatestObservation | 92 | 当前 step observation |
| RuntimePlan | 93 | 活跃计划步骤 |
| CurrentTask | 94 | 当前关注任务 |
| RecentTasks | 50 | 最近任务列表 |
| UserMemories | 75 | 注入的用户记忆 |
| ToolDescriptions | 40 | 可用工具描述，required |
| ResponseContract | 100 | 响应格式约束，required |

### 2.3 裁剪元信息（Section Meta）

每 section 的 snapshot 包含：

```rust
pub struct ContextSectionSnapshot {
    kind: String,                   // section 类型名
    priority: u8,                   // 优先级数值
    max_chars: usize,               // section 预算上限
    original_char_count: usize,     // 原始字符数
    line_count: usize,              // 行数
    item_count: usize,              // 非空行数
    char_count: usize,              // 最终字符数
    included: bool,                 // 是否被包含
    trimmed: bool,                  // 是否被 section 预算裁剪
    trim_reason: Option<ContextSectionChangeReason>,
    drop_reason: Option<ContextSectionChangeReason>,
    content: String,                // 内容摘要（用于 trace / preview）
}
```

---

## 3. 统一入口 API

### 3.1 公开函数

```rust
/// 构建 ContextPack（需要 agent_core 内部状态，保留在 agent_core 中）
pub fn build_context_pack(
    trace: &AgentRunTrace,
    user_input: &str,
    observation: Option<&AgentObservation>,
    runtime_session_state: Option<&RuntimeSessionStateSnapshot>,
    available_tools: &[String],
    business_context: Option<&BusinessContextSnapshot>,
    session_summary_strategy: SessionSummaryStrategy,
) -> ContextPack

/// 从 ContextPack 渲染 prompt 字符串（纯函数，无副作用）
pub fn render_prompt_from_context_pack(pack: &ContextPack) -> String
```

### 3.2 ContextPack 方法

```rust
impl ContextPack {
    pub fn render(&self) -> String;                  // 渲染为 prompt
    pub fn snapshot(&self) -> Vec<ContextSectionSnapshot>;
    pub fn budget_summary(&self) -> ContextBudgetSummary;
    pub fn total_chars(&self) -> usize;
    pub fn section(&self, kind: ContextSectionKind) -> Option<&ContextSection>;
}
```

---

## 4. Drop Reason 枚举（最小集合）

```rust
pub enum ContextSectionChangeReason {
    SectionBudgetExceeded,  // section 自身预算超限
    TotalBudgetExceeded,    // 全局总预算超限，按优先级丢弃
}
```

> 注：memory 级别的 drop reason（Deduplicated / SingleItemTooLong / BudgetExceeded）保留在 `SessionState::dropped` 中，不属于 ContextPack 的 section 级 drop reason。

---

## 5. 兼容策略

### 5.1 无 session_state 时退化

- `RuntimeSessionStateSnapshot::is_empty()` 返回 true 时，不生成 SessionState section
- 所有依赖 session_state 的推导逻辑均使用 `Option` 包装，空值时跳过
- 不改变已有命令路由、工具调用语义

### 5.2 无 business_context 时退化

- `business_context: None` 时，不生成 CurrentTask / RecentTasks / UserMemories section
- 已有 behavior：无 task_store_db_path 时直接返回 `None`

### 5.3 preview_context 兼容

- `preview_context_with_context_mode` 继续使用 `ContextAssembler::assemble_with_summary_strategy`
- 内部仍通过 `build_pack` -> `render` -> `snapshot` 获取全部信息

---

## 6. In Scope / Out of Scope

### In Scope

- [x] ContextPack 数据结构提取为独立模块（`src/context_pack.rs`）
- [x] 统一公开入口 API（`build_context_pack` + `render_prompt_from_context_pack`）
- [x] Trace 增加 pack 级字段（`context_pack_present`, `context_pack_section_count`, `context_pack_total_chars`, `context_pack_drop_reasons`）
- [x] 结构化日志事件（`context_pack_built`, `context_pack_trimmed`）
- [x] 文档收口（设计文档、历程文档、README、session 纪要）
- [x] 最小单测覆盖（pack 构建、无 state 退化、budget 裁剪、渲染标识）

### Out of Scope

- [ ] 向量召回、embedding、新 memory taxonomy
- [ ] 重构 tool/command/router 层
- [ ] 全量异步化与数据库层重写
- [ ] 发布流程（版本/tag）
- [ ] 复杂 benchmark 框架
- [ ] 自动压缩/淘汰策略

---

## 7. 模块边界

```
src/
├── context_pack.rs      ← 新增：纯数据结构 + 通用方法
│   ├── ContextPack
│   ├── ContextSection / ContextSectionKind / ContextSectionPolicy
│   ├── ContextSectionSnapshot / ContextSectionChangeReason / ContextBudgetSummary
│   └── trim_section_lines, DEFAULT_CONTEXT_MAX_TOTAL_CHARS
│
├── agent_core/mod.rs    ← 修改：
│   - 导入 context_pack::* 替代内嵌定义
│   - 保留 ContextAssembler（构建逻辑依赖 agent_core 内部状态）
│   - 导出 build_context_pack, render_prompt_from_context_pack
│   - Trace 新增 pack 级字段
│   - 新增 context_pack_built / context_pack_trimmed 日志
│
└── lib.rs               ← 修改：pub mod context_pack
```

---

## 8. 验收标准

1. `agent_core` 不再主链路散落拼接 context（已通过 ContextAssembler 统一）
2. 所有 prompt 组装可追溯到 ContextPack 单入口（通过 `build_context_pack` -> `render`）
3. 任何一次 run 的 trace 能解释 context 构成与裁剪决策（`context_pack_*` 字段 + `context_sections`）
4. 新字段不破坏现有 trace 读写（向后兼容：旧 trace 缺少新字段时正常解析）
5. 文档能独立讲清这次技术选型与接入过程
