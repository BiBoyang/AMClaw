# AMClaw Context / Memory 演进路线图（2026-04-13）

## 目的

本文件用于在 `v0.3.2 Context & Memory Minimal` 的基础上，给出下一阶段 `AMClaw` 的 `context / memory` 系统演进路线，作为后续推进时的统一参考。

目标不是推翻现有实现，而是把当前已经落地的：

- `Agent Trace`
- 最小 `ContextSnapshot`
- 最小 `user_memories`
- 最小 `controller / runtime state`

继续收敛成一套更明确、可演进、可评测的系统。

---

## 一句话结论

下一阶段最合理的路线不是“继续堆 memory 功能”，而是：

> 在保留当前 `Memory v3` 成果的前提下，先把 `session state` 做显式化、把 `context pack` 做结构化、把 `memory` 从“用户记忆”扩成“少量高价值长期记忆”，再用 trace 做评测闭环。

换句话说，后续推进顺序应当是：

1. 先收口现有最小 memory 的语义与观测；
2. 再补完整 `session state`；
3. 再把 `context assembly` 升级为明确的 `context pack`；
4. 再扩 memory 类型；
5. 最后用 trace 驱动评测和下一轮优化。

---

## 当前基线（已具备）

截至 `v0.3.2`，AMClaw 已经具备以下基础：

### 1. Trace 已落地

- Agent run 会落盘 JSON / Markdown / 每日索引；
- 已记录：
  - 运行上下文
  - 决策
  - observation
  - failure
  - tool call
  - LLM call
  - memory 命中统计

### 2. 最小 ContextSnapshot 已落地

当前上下文已能组合：

- `session_text`
- `current_task`
- `recent_tasks`
- `user_memories`
- `context_token_present`
- active plan / controller state / latest observation

### 3. 最小 Memory 已落地

当前 `user_memories` 已支持：

- `memory_type`
- `status`
- `priority`
- `last_used_at`
- `use_count`
- 检索排序
- 预算裁剪
- 命中回写
- 软抑制

### 4. 最小 Runtime State 已落地

当前 runtime 已维护：

- `step_status`
- `current_step_index`
- `replan_budget`
- `failure_count`
- `ask_user_count`
- 最小 `watchdog`

因此，AMClaw 当前并不是“还没开始做 context / memory”，而是已经走到了：

> `Trace + 最小 Context + 最小 Memory + 最小 Controller`

---

## 当前真正缺的不是“更多 memory”，而是 3 个中间层

从当前代码看，下一阶段最缺的是：

### 1. 显式 Session State

现在系统已经有：

- `session_text`
- active plan
- `current_task`
- `recent_tasks`

但还没有一个明确的“当前任务状态板”，例如：

- `goal`
- `current_subtask`
- `constraints`
- `confirmed_facts`
- `done_items`
- `next_step`
- `open_questions`

这会导致系统虽然有很多上下文材料，但“当前要做什么”仍然部分隐含在 prompt 和 trace 里。

### 2. 独立的 Context Pack Schema

当前 prompt 拼装已经不错，但它还主要是“在 `agent_core` 里即时拼接字符串”。

下一步更合理的方向是把“当前一步实际喂给模型的上下文包”抽象成独立概念：

- 哪些部分来自 trace
- 哪些来自 session state
- 哪些来自 memory
- 哪些来自业务状态
- 每部分预算是多少

这样以后才能真正做 context 预算治理、A/B 比较和失败定位。

### 3. 更明确的长期 Memory 类型

当前 memory 基本还是：

- 显式用户记忆
- 自动提炼的主题 / 偏好

这是合理的 MVP，但长期不够。

下一阶段更值得扩的是：

- `user_preference`
- `project_fact`
- `lesson`

而不是直接引入复杂向量库或图谱系统。

---

## 设计原则（后续推进时必须保持）

### 原则 1：先状态，后 memory

如果没有稳定的 `session state`，单纯增加 memory 只会让上下文更脏、更不可控。

### 原则 2：先结构，后检索复杂度

优先把：

- trace 结构
- session state 结构
- context pack 结构
- memory item 结构

定义清楚，而不是优先讨论 embedding / rerank / graph。

### 原则 3：先少而精，再大而全

长期 memory 只保留：

- 高价值
- 可复用
- 相对稳定
- 有证据来源

不要把所有聊天历史都变成 memory。

### 原则 4：一次只加一个机制

每一轮只增加一个明确能力，例如：

- 先加 `session state`
- 或先加 `project_fact`
- 或先加 `session summary`

不要同时上多个 memory 机制，否则无法判断收益来源。

### 原则 5：Trace 是 ground truth

任何后续的：

- context 优化
- memory 扩展
- 失败分析
- 评测闭环

都应以 trace 为事实基础。

---

## 目标结构（建议的系统分层）

下一阶段建议把 AMClaw 的 context / memory 系统明确分成四层：

### 1. Trace Layer

职责：

- 忠实记录 agent 运行现场

保留：

- 输入
- 决策
- observation
- tool calls
- failures
- LLM calls
- prompt snapshot
- memory hit 统计

这一层继续沿用当前 `AgentRunTrace`，不推翻。

### 2. Session State Layer

职责：

- 维护当前任务推进所需的结构化状态

建议增加的最小槽位：

- `goal`
- `current_subtask`
- `constraints`
- `confirmed_facts`
- `assumptions`
- `done_items`
- `next_step`
- `open_questions`
- `important_artifacts`

这一层是下一阶段最值得优先补齐的部分。

### 3. Context Pack Layer

职责：

- 把“当前真正送给模型的内容”结构化表达出来

建议固定槽位：

- `system_instructions`
- `user_input`
- `runtime_state`
- `session_state`
- `business_context`
- `retrieved_memories`
- `latest_observation`
- `active_plan`
- `tool_budget / step_budget summary`

这层的本质是预算分配，而不是简单拼接。

### 4. Memory Layer

职责：

- 沉淀跨步骤 / 跨 session 仍有价值的内容

建议长期只保留三类：

- `user_preference`
- `project_fact`
- `lesson`

后续才考虑是否需要进一步细分。

---

## 演进阶段计划

下面按“先做什么、为什么先做、做到什么算完成”给出推荐顺序。

---

## Phase 1：收口现有 Memory v3 语义

### 目标

把当前已经落地的 memory 消费链路做语义收口，避免设计稿和实现逐渐漂移。

执行任务清单见：

- `notes/context-memory/PHASE-1-MEMORY-V3-SEMANTICS-TASKLIST-2026-04-13.md`

### 为什么先做

因为现在已经有最小 memory，但仍有几个细节不完全一致：

1. 自动提炼的 memory 在设计上应当是：
   - `memory_type='auto'`
   - `priority=60`
2. 但当前聊天入口自动提炼后，实际仍走显式记忆写入路径；
3. 当前 `mark_memories_used` 更接近“被检索到”而不是“真正被证明有用”。

如果这些基础语义不先收口，后面继续扩展只会越来越混。

### 本阶段建议动作

1. 收口自动记忆写入语义：
   - 自动提炼统一走 typed API；
   - 显式记忆与自动记忆保持不同优先级。
2. 明确区分三个统计概念：
   - `retrieved_memory_count`
   - `injected_memory_count`
   - `useful_memory_count`（先可不自动判断，但字段预留）
3. 明确 `use_count` 的语义：
   - 当前先定义为“被注入次数”或“被采用次数”二选一；
   - 不允许实现和命名长期不一致。
4. 把 `ContextSnapshot` / `MemoryStats` 的字段命名与文档统一。

### DoD

1. 自动记忆不再伪装成显式记忆；
2. Trace / 日志 / store API 对 memory 命中语义一致；
3. 文档与实现中的字段含义统一；
4. 无行为回归。

---

## Phase 2：引入显式 Session State（最高优先级）

### 目标

把“当前任务进行到哪里”从隐式状态升级成显式结构。

### 为什么先做

因为真实 agent 系统里，很多失败不是“没有长期记忆”，而是：

- 不知道当前目标是什么；
- 不知道哪些事实已经确认；
- 不知道下一步该干什么；
- 不知道哪些问题还没解决；
- 不知道哪些约束不能违反。

这类问题都属于 `session state` 缺失，不属于长期 memory 缺失。

### 建议数据结构

新增最小 `SessionState`：

- `goal`
- `current_subtask`
- `constraints`
- `confirmed_facts`
- `assumptions`
- `done_items`
- `next_step`
- `open_questions`
- `important_artifacts`
- `updated_at`

### 建议落点

- 主组装在 `src/agent_core/`
- 输入材料来自：
  - `AgentRunContext`
  - `BusinessContextSnapshot`
  - active plan / latest observation
  - 必要时来自 `chat_adapter` 的 session 合并结果

### 初期实现建议

第一步不要做持久化复杂化，先支持：

- 运行时内结构化维护
- 写入 trace
- 注入 prompt

等结构稳定后，再评估是否需要单独持久化。

### DoD

1. prompt 中不再只有 `session_text`，而是带有显式 `session state` 槽位；
2. trace 可看到该 run 的 `session state snapshot`；
3. 重复劳动、任务漂移、违背约束的问题有可见下降；
4. 无 `session state` 时仍可退化运行。

---

## Phase 3：把 Context Assembly 升级为 Context Pack

### 目标

把当前“字符串拼 prompt”的方式升级成“结构化上下文包再渲染 prompt”。

### 为什么要做

因为 context 系统的核心不是“拼得更长”，而是：

- 有哪些输入槽位；
- 每个槽位预算多少；
- 哪些信息优先级更高；
- 哪些失败是因为没带进去；
- 哪些失败是因为带了噪音。

没有独立 `ContextPack`，这些问题很难系统化分析。

### 建议结构

`ContextPack` 最小建议字段：

- `runtime_context`
- `session_state`
- `current_task`
- `recent_tasks`
- `memories`
- `latest_observation`
- `active_plan`
- `budget_summary`
- `assembly_notes`

### 建议实现方式

1. 先在 Rust 里用结构体表达；
2. 再由单独函数把 `ContextPack` 渲染成 prompt；
3. trace 中同时记录：
   - 原始 `ContextPack`
   - 渲染后的 prompt snapshot

### DoD

1. context 组装逻辑不再完全散落在字符串拼接里；
2. 每个 prompt section 有明确来源；
3. trace 能区分“带了什么”和“最后渲染成什么”；
4. 后续 budget 调整可以独立测试。

---

## Phase 4：扩 memory 类型，但仍保持极简

### 目标

把 memory 从“仅用户记忆”扩成三类高价值长期信息。

### 建议新增类型

#### 1. `user_preference`

例如：

- 回复风格偏好
- 输出形式偏好
- 工作方式偏好

#### 2. `project_fact`

例如：

- 模块职责
- 稳定约束
- 关键边界
- 项目级不变量

#### 3. `lesson`

例如：

- 哪类失败经常出现
- 哪种处理方式对当前项目有效
- 哪种排查路径更稳

### 为什么只扩这三类

因为它们：

- 最容易界定价值；
- 最容易跨 session 复用；
- 最不容易快速腐烂；
- 最适合先做规则化提炼。

### 明确不做

本阶段仍然不优先做：

- embedding / 向量库
- 图谱 memory
- 自动写入所有历史
- 复杂 taxonomy

### DoD

1. memory 不再只有 `explicit / auto` 两种来源视角；
2. 至少能表达 `user_preference / project_fact / lesson` 三类价值视角；
3. 新类型检索不污染原有系统；
4. prompt 注入仍受预算限制。

---

## Phase 5：让 Trace 驱动评测闭环

### 目标

从“有 trace”升级为“trace 真能指导下一轮 context / memory 演进”。

### 为什么要做

没有评测，memory 设计很容易回到“感觉上更高级”。

后续优化应当回答这几个具体问题：

- 是不是减少了重复劳动？
- 是不是减少了状态漂移？
- 是不是更少遗漏用户约束？
- 是不是真正提高了任务连续性？
- 是不是只是 prompt 更长了？

### 建议失败分类

先按这几类做人工标注：

- `forgot_known_fact`
- `missed_retrieval`
- `wrong_retrieval`
- `overcompressed_summary`
- `state_drift`
- `repeated_work`

### 建议第一阶段评测方式

1. 从 `data/agent_traces/` 挑 20~50 个真实 run；
2. 人工标注主要失败类型；
3. 统计：
   - memory hit
   - retrieved / injected 数量
   - 失败类型分布
   - 返工情况
4. 每轮只对一个机制做前后比较。

### DoD

1. 至少形成一版 failure taxonomy；
2. 至少能手工分析一批真实 trace；
3. 后续 memory 改动不再只靠主观感觉；
4. 评测结果能反馈回文档和实现。

---

## 推荐模块分工

### `src/chat_adapter/`

负责：

- 提供聊天入口的 session 合并结果；
- 负责显式/自动 memory 写入入口语义；
- 不负责复杂检索与上下文拼装策略。

### `src/task_store/`

负责：

- memory 持久化；
- memory 类型 / 状态 /命中信息；
- 如后续需要，可承接更稳定的 session summary / lesson 持久化。

### `src/agent_core/`

负责：

- `SessionState`
- `ContextPack`
- business context snapshot
- prompt 渲染
- trace 扩展
- 运行时 state/controller 演进

### `data/agent_traces/`

负责：

- 作为事实来源和评测样本；
- 不直接当长期 memory 仓库使用。

---

## 明确不建议的路线

以下路线当前不建议优先走：

### 1. 直接上向量库

原因：

- 现在最主要的问题不是“没有语义召回能力”，而是 session / context 结构还不够明确。

### 2. 先做复杂 memory taxonomy

原因：

- 类型分太细会让写入和维护成本暴涨；
- 当前系统规模还不需要。

### 3. 先做大规模自动提炼

原因：

- 没有评测闭环之前，大规模自动写入更容易污染 memory。

### 4. 先重写 runtime

原因：

- 当前 runtime 已有可用骨架；
- 更合理的做法是在现有骨架上逐步补 `session state` 与 `context pack`。

---

## 推荐执行顺序（最终版）

后续推进建议严格按以下顺序进行：

1. **Phase 1：收口 Memory v3 语义**
2. **Phase 2：补显式 Session State**
3. **Phase 3：抽 Context Pack**
4. **Phase 4：扩展长期 Memory 类型**
5. **Phase 5：建立 Trace 驱动评测闭环**

如果资源有限，最优先只做前三步：

1. 收口现有 memory 语义；
2. 引入显式 `session state`；
3. 抽象 `context pack`；

这三步做完后，AMClaw 的 `context / memory` 系统就会从“最小可用”进入“可稳定演进”阶段。

---

## 最后压成一句话

AMClaw 下一阶段不该追求“更复杂的 memory”，而应追求：

> 用显式 `session state` 管当前任务，用结构化 `context pack` 管模型输入，用少量高价值 `memory` 管长期复用，再用 `trace` 驱动每一轮演进。

这条路线最符合 AMClaw 当前代码现状，也最有利于后续持续推进而不返工。
