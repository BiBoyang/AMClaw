# Context 技术选型与演进历程（持续更新）

> 创建日期：2026-04-17  
> 目标：为后续技术文章沉淀“为什么这样选、如何一步步接入、每一步解决了什么问题”的主线素材。  
> 写法原则：记录里程碑，不写流水账；强调决策与目的，不强依赖实验数据。

---

## 这份文档怎么用

每个阶段固定记录 5 个要点：

1. 背景问题（当时卡在哪）
2. 选型决策（为什么选这个，不选什么）
3. 实现范围（改了哪些模块）
4. 阶段结果（能力边界有什么变化）
5. 暂不做事项（明确克制范围，防止目标漂移）

后续写技术文章时，可直接按本文件章节扩写。

---

## 阶段 0：Context / Memory Minimal 闭环（v0.3.2 基线）

### 1) 背景问题

- 早期上下文主要依赖即时文本拼接，可运行但不稳定，解释成本高。
- 系统需要先有一个“最小可用闭环”，再谈更复杂机制。

### 2) 选型决策

- 先做最小闭环：`session_text + current_task + recent_tasks + user_memories`。
- 先把 Trace、最小 Context、最小 Memory、最小 Controller 接起来。
- 不引入复杂检索与向量机制，优先保证可运行与可回归。

### 3) 实现范围

- `agent_core`：最小上下文拼装、memory 注入、最小控制预算。
- `task_store`：用户记忆读写与最小命中治理。
- `chat_adapter`：聊天入口接线到 agent context。

### 4) 阶段结果

- 项目从“可聊天”升级到“可带最小上下文稳定运行”。
- 后续演进重点从“有没有能力”转为“结构是否清晰、语义是否一致”。

### 5) 暂不做事项

- 不上 embedding / 向量库。
- 不做复杂 memory taxonomy。
- 不做全量异步化重构。

---

## 阶段 1：Memory v3 语义收口（为下一阶段打地基）

### 1) 背景问题

- Memory 已可用，但“检索命中”和“实际注入”的语义与统计口径容易混淆。
- 文档、日志、实现存在逐步漂移风险。

### 2) 选型决策

- 先收口语义，不扩新能力。
- 统一 memory 链路中的关键口径：检索、裁剪、注入、命中回写。

### 3) 实现范围

- `agent_core`：memory 检索结果接入 context 组装，增加预算裁剪与可观测字段。
- `task_store`：命中回写与反馈状态维护。
- Trace / 日志：补齐 memory 相关统计字段，保证可解释。

### 4) 阶段结果

- `Memory v3` 达到“可运行 + 可观测 + 可回归”的状态。
- 为后续 `session_state` 与 `context_pack` 提供了干净起点。

### 5) 暂不做事项

- 不同时叠加多个 memory 新机制。
- 不做“看起来更高级”但难解释收益的扩展。

---

## 阶段 2：Session Summary 策略收口（semantic vs truncate）

### 1) 背景问题

- `session_text` 过长时必须压缩，否则 prompt 噪音与预算波动明显。
- 需要一个可复用策略，而不是临时截断。

### 2) 选型决策

- 抽离独立模块 `session_summary`，把策略从 `agent_core` 中解耦。
- 保留双策略：`semantic`（语义保留）与 `truncate`（确定性截断）。
- 增加离线评测 CLI（`context_eval`）用于快速对比策略行为。

### 3) 实现范围

- `src/session_summary.rs`：统一 summary 逻辑与常量。
- `src/bin/context_eval.rs`：离线评测入口。
- `notes/context-memory/eval_samples.jsonl`：评测样本。
- `README.md`：补充 `context_eval` 使用说明。

### 4) 阶段结果

- Summary 策略从“内嵌逻辑”变为“可复用组件”。
- 具备轻量策略对比能力，后续调优成本明显降低。

### 5) 暂不做事项

- 不做复杂 benchmark 框架。
- 不把评测系统化为重流程（保持轻量、可重复）。

---

## 阶段 3：显式 SessionState 接入（C0/C1/C2）

> 状态：进行中（C0/C1/C2 已形成实现与校验，持续收口中）

### 1) 背景问题

- 当前上下文能跑，但“用户正在做什么”仍偏隐式。
- 缺少持久化会话状态时，跨轮次目标与阻塞信息难稳定继承。

### 2) 选型决策

- 先做最小 `UserSessionStateRecord`，字段只保留高价值核心槽位。
- 先在 `chat_adapter -> agent_core` 主链路接线，不引入深度状态推导。
- 读写失败一律降级，不阻断主流程。

### 3) 实现范围

- `task_store`：新增 `user_session_states` 表及 `load/upsert/clear` API。
- `chat_adapter`：flush 更新 `last_user_intent`；agent 运行前加载；运行后最小回写 `updated_at`。
- `agent_core`：`AgentRunContext` 挂载持久化状态；合并到运行时 `RuntimeSessionStateSnapshot`；trace 增加 `persistent_state_*` 字段。

### 4) 阶段结果

- 有状态时可继承历史意图/阻塞信息；无状态时保持现有行为基线。
- SessionState 从“临时运行信息”升级为“可持久化、可追踪”的一层。

### 5) 暂不做事项

- 不从 agent 内部自动推导并回写深层业务状态。
- 不引入自动压缩/淘汰策略。
- 不扩展到复杂状态机。

---

---

## 阶段 4：ContextPack 结构化（C3/C4）

### 1) 背景问题

- 上下文已能稳定运行，但组装逻辑散落在 `agent_core` 内部，模块边界模糊。
- “哪些 section 进了 prompt、哪些被裁了、为什么” 的口径需要显式化才能可解释。
- trace 中缺少 pack 级字段，无法一眼看出一次 run 的上下文构成。

### 2) 选型决策

- 不增加新的上下文来源，而是把已有来源（runtime、session_state、memory、task 等）显式化为 `ContextSection`。
- 提取独立模块 `context_pack`，与 `agent_core` 解耦：pack 管”结构 + 渲染 + 预算”，agent_core 管”根据运行时状态填充内容”。
- 统一入口：`build_context_pack(...)` -> `render_prompt_from_context_pack(...)`，保证所有 prompt 组装可追溯。
- Trace 补齐 `context_pack_*` 字段 + 日志事件 `context_pack_built` / `context_pack_trimmed`。

### 3) 实现范围

- 新增 `src/context_pack.rs`：
  - `ContextPack` / `ContextSection` / `ContextSectionKind` / `ContextSectionPolicy`
  - `ContextSectionSnapshot` / `ContextSectionChangeReason` / `ContextBudgetSummary`
  - `trim_section_lines` / `render_prompt_from_context_pack`
- 修改 `src/agent_core/mod.rs`：
  - 移除内嵌的 context pack 定义，改为从 `context_pack` 导入
  - 保留 `ContextAssembler`（构建逻辑依赖 agent_core 内部状态）
  - `decide` 主链路改用 `build_context_pack` 单入口
  - `AgentRunTrace` / `AgentTraceIndexEntry` 新增 pack 级字段
  - 新增 `context_pack_built` / `context_pack_trimmed` 结构化日志
- 新增单测：`trace_context_pack_fields_present_after_run`、`trace_context_pack_fields_populated_on_budget_trim`

### 4) 阶段结果

- Context 组装从”内嵌实现”升级为”独立模块 + 单入口 API”。
- 任何一次 run 的 trace 都能解释 context 构成与裁剪决策。
- 为后续扩展新的 section 类型提供了干净的插入点。

### 5) 暂不做事项

- 不上 embedding / 向量库。
- 不做新的 memory taxonomy。
- 不做全量异步化或数据库层重写。

---

## 后续追加模板

可复制以下模板追加新阶段：

```md
## 阶段 X：<阶段名>

### 1) 背景问题
- ...

### 2) 选型决策
- ...

### 3) 实现范围
- ...

### 4) 阶段结果
- ...

### 5) 暂不做事项
- ...
```
