# AMClaw Context Strategy 设计初稿（2026-04-14）

## 目的

本文件用于把 AMClaw 当前已经具备的：

- `session_router` 会话合并
- `BusinessContextSnapshot`
- `SessionState`
- `user_memories`
- `MemoryWriteState`
- `MemoryFeedbackState`
- `AgentRunTrace`

收敛成一套明确的 **Context Strategy（上下文策略）**。

本文件不是最终定稿，而是一个可讨论的初稿。重点是：

1. 把当前实现到底在做什么说清楚；
2. 把几种常见上下文设计方案并列比较；
3. 给出 AMClaw 更适合的方向；
4. 为后续代码演进提供一个稳定锚点。

---

## 一句话结论（初稿）

AMClaw 下一步不应优先做“更多 memory 功能”，而应先把上下文系统明确为：

> **Session Buffer + Business Snapshot + Ranked Memory Injection + Section Budget**

也就是说：

- 聊天消息先在 session 层做短期合并；
- 每次 agent run 基于当前业务状态构造快照；
- 长期 memory 通过检索排序后进入当前上下文；
- 上下文各 section 按优先级和预算控制，而不是单纯滑动窗口。

初步判断：

- **不建议把“滑动窗口”作为核心策略**
- **建议把“结构化 section + 预算管理 + ranked injection”作为核心策略**

---

## 1. 当前系统基线

截至 2026-04-14，AMClaw 当前上下文相关能力已经包括：

### 1.1 聊天层的短期会话合并

- `session_router` 会把聊天消息按 `pending / continue / commit / timeout` 合并；
- 产出 `merged_text` 与 `message_ids`；
- 这是一种 **session buffer**，不是严格意义上的滑动窗口。

这意味着当前系统已经有：

- “短期会话上下文”
- 但还没有“多轮 token 级上下文窗口”

### 1.2 业务快照

`agent_core` 当前会在每次决策前构造 `BusinessContextSnapshot`，包含：

- `current_task`
- `recent_tasks`
- `user_memories`

也就是说，当前 prompt 并不是直接堆历史对话，而是基于一个结构化业务快照。

### 1.3 本轮 memory 生命周期

`SessionState` 已经明确区分：

- `retrieved`
- `injected`
- `dropped`

并负责：

- 去重
- 单条长度过滤
- 总预算裁剪

这说明 AMClaw 当前对 memory 的注入方式，本质上是：

> **ranked retrieval + bounded injection**

而不是简单“最近记忆直接进入 prompt”。

### 1.4 长期 memory

长期 memory 当前落在 `user_memories`，包含：

- `explicit`
- `auto`
- `priority`
- `status`
- `retrieved_count`
- `injected_count`
- `useful`
- `use_count`
- `last_used_at`

已经具备最小写入治理和反馈排序能力。

### 1.5 trace

`AgentRunTrace` 当前记录：

- 运行输入
- 计划 / 决策
- observation
- tool / llm 调用
- failures
- memory 命中统计

trace 已可用于复盘，但目前仍主要承担“审计 / 调试”职责，而不是 prompt 上下文的一部分。

---

## 2. 为什么现在必须单独设计 Context Strategy

当前如果继续不显式设计 context strategy，会出现以下问题：

### 2.1 上下文来源越来越多，但缺少统一优先级

当前系统里已经有至少这些上下文来源：

- user input
- session merged text
- current task
- recent tasks
- user memories
- current observation
- runtime active plan
- failure state
- tools description
- trace metadata

如果没有统一策略，就会逐渐演变成：

> 每个来源都“有点重要”，但没有明确谁优先、谁可裁掉、谁该压缩。

### 2.2 memory 再强，也不等于使用体验会更好

用户体感更受以下问题影响：

- agent 是否抓住当前重点
- 当前任务是否压过历史噪音
- memory 是否“有用但不喧宾夺主”
- observation 是否太长
- recent tasks 是否挤占空间

这些都属于 context design 问题，而不是 memory table 再加几个字段就能解决。

### 2.3 没有 context 策略，就很难解释行为

未来如果 agent 表现差，最想回答的问题会是：

- 这次模型到底看到了哪些 section？
- 哪个 section 被截断了？
- 哪类信息最占预算？
- 当前任务被哪类历史信息冲淡了？

没有 section/budget 策略，这些都只能靠猜。

---

## 3. Context Strategy 设计目标

AMClaw 的上下文策略建议满足以下目标：

### 3.1 当前任务优先

无论长期 memory 多丰富，系统都要优先解决“当前用户这次想干什么”。

### 3.2 长期记忆有效但克制

memory 应该：

- 在关键时刻提供帮助；
- 但不应压过当前任务；
- 不应重复；
- 不应让 prompt 被偏好类内容淹没。

### 3.3 可解释

系统应该能回答：

- 哪些内容进入了 prompt？
- 哪些被裁掉了？
- 为什么被裁掉？
- 各 section 占了多少预算？

### 3.4 低复杂度起步

v1 不应一上来做复杂 token allocator、向量召回、多级 summary。

建议优先做：

- 显式 section
- chars 级预算
- deterministic 优先级
- trace 可观测

### 3.5 可演进

v1 的策略以后要能自然扩展到：

- observation summary
- session summary
- task summary
- token 级预算
- richer memory types

---

## 4. 需要管理的上下文来源

建议先明确 AMClaw 的 context sources。

| 类别 | 来源 | 生命周期 | 当前是否已存在 |
|---|---|---|---|
| 当前输入 | `user_input` | single run | 是 |
| 会话文本 | `session_text` / `merged_text` | session | 是 |
| 当前任务 | `current_task` | single run / task | 是 |
| 最近任务 | `recent_tasks` | short-term history | 是 |
| 用户记忆 | `user_memories` | cross-session | 是 |
| 当前 observation | 最新 tool result | single step | 是 |
| 历史 observation | 前几步结果 | single run | 部分 |
| 执行计划 | active plan | single run | 是 |
| 失败状态 | failure / retry state | single run | 是 |
| 工具能力 | available tools | static runtime | 是 |
| trace 摘要 | past trace summary | cross-run | 否 |
| session 摘要 | previous session summary | session / cross-session | 否 |

---

## 5. 候选设计方案对比

下面把几种常见的上下文设计方案拆开比较。

### 方案 A：纯滑动窗口（Sliding Window）

#### 核心思想

- 保留最近 N 条消息 / 最近 N token；
- 超出窗口后按时间顺序丢弃最早内容。

#### 优点

- 实现简单；
- 用户对“最近对话被记住”有直觉；
- 对纯聊天机器人比较自然。

#### 缺点

- 按时间裁剪，不按价值裁剪；
- 新噪音会挤掉旧但重要的内容；
- 不适合 task / memory / observation 混合场景；
- 不易解释“为什么这条 memory 没进 prompt”。

#### 对 AMClaw 的适配度

**较低。**

AMClaw 不是纯聊天机器人，而是：

- 有 task 状态
- 有工具 observation
- 有长期 memory
- 有 session 合并

如果只做 sliding window，很容易让最近聊天噪音压过真正重要的 memory / task 状态。

---

### 方案 B：纯摘要压缩（Rolling Summary）

#### 核心思想

- 旧上下文不直接保留；
- 定期压缩成 summary；
- 当前 prompt 只带 latest messages + summary。

#### 优点

- 长对话不会无限膨胀；
- 有利于长期 session 持续。

#### 缺点

- summary 质量高度依赖压缩策略；
- 一旦摘要偏了，错误会长期传播；
- 调试难度大；
- v1 容易过度复杂。

#### 对 AMClaw 的适配度

**中等。**

未来可能需要，但不适合立刻当主策略。

更适合后续用于：

- session summary
- long observation summary
- long task history summary

---

### 方案 C：纯检索注入（Retrieval-Only Context）

#### 核心思想

- 当前 prompt 主要由当前输入 + 检索结果组成；
- 其他历史几乎不显式保留。

#### 优点

- 聚焦；
- 不容易被聊天噪音污染；
- 容易控制预算。

#### 缺点

- 对 session 连续性支持弱；
- 对当前任务状态表达不足；
- 如果 retrieval 不准，上下文就会断层。

#### 对 AMClaw 的适配度

**中等偏低。**

AMClaw 需要的不只是 user memory，还包括：

- current task
- recent tasks
- observation
- active plan

所以只做 retrieval-only 不够。

---

### 方案 D：快照式上下文（Snapshot Context）

#### 核心思想

- 每次 agent 决策前重新构造“当前业务快照”；
- 快照里包含当前任务、最近任务、关键状态、memory 等。

#### 优点

- 非常适合 agent / task 系统；
- 可解释；
- 对每次 run 都有稳定结构；
- 比滑动窗口更适合结构化业务。

#### 缺点

- 如果没有 budget 和 priority，快照也会膨胀；
- 仍需要明确 section 之间如何竞争上下文空间。

#### 对 AMClaw 的适配度

**很高。**

AMClaw 当前已经在做这一方向，只是还没有把它上升为明确策略。

---

### 方案 E：Section Budget + Ranked Injection（推荐方向）

#### 核心思想

- 先把上下文分成明确 section；
- 每个 section 有优先级和预算；
- 长期 memory 通过 ranked injection 进入对应 section；
- observation / recent tasks / current task 各自有预算；
- 超预算时按 section 内策略裁剪，而不是全局乱截。

#### 优点

- 兼顾结构化和可控性；
- 容易解释；
- 易于 trace；
- 与当前 `SessionState + user_memories` 架构天然契合；
- 适合后续迭代 token allocator。

#### 缺点

- 比当前即时拼字符串更复杂；
- 需要先设计 section schema；
- 预算划分不合理时，也会影响效果。

#### 对 AMClaw 的适配度

**最高。**

这是最适合 AMClaw 现阶段的方案。

---

## 6. 对比结论

| 方案 | 简单性 | 可解释性 | 适合 Agent | 适合长对话 | 适合当前 AMClaw |
|---|---:|---:|---:|---:|---:|
| Sliding Window | 高 | 低 | 低 | 中 | 低 |
| Rolling Summary | 中 | 中低 | 中 | 高 | 中 |
| Retrieval-Only | 中 | 中 | 中 | 低 | 中低 |
| Snapshot Context | 中高 | 高 | 高 | 中 | 高 |
| Section Budget + Ranked Injection | 中 | 很高 | 很高 | 中高 | 很高 |

初稿判断：

> AMClaw 不应以 `Sliding Window` 为主策略，而应以 `Snapshot Context + Section Budget + Ranked Memory Injection` 为主策略。

---

## 7. 建议的 AMClaw Context Strategy v1

### 7.1 策略名称（建议）

> **Context Strategy V1 = Session Buffer + Business Snapshot + Ranked Memory Injection + Section Budget**

### 7.2 v1 基本原则

#### 原则 1：当前任务优先于历史信息

- 当前用户输入永远优先；
- current task 优先于 recent tasks；
- current observation 优先于 older observation；
- explicit memory 优先于 auto memory。

#### 原则 2：长期记忆不直接等于上下文

- memory 是长期池；
- 只有被检索排序选中的部分，才进入本轮上下文；
- 注入前仍需经过 `SessionState` 裁剪。

#### 原则 3：每类上下文都是 section，而不是“一锅粥”

建议至少拆分：

1. `Current User Request`
2. `Current Task`
3. `Latest Observation`
4. `Active Plan / Failure State`
5. `User Memories`
6. `Recent Tasks`
7. `Tool Descriptions`

#### 原则 4：预算按 section 管理

不是全局无差别地拼接后再截断，而是：

- 先给 section 预算；
- section 内部再做裁剪；
- 被裁掉的内容要可解释。

#### 原则 5：trace 记录“上下文是如何被组装的”

未来应该能记录：

- 每个 section 的 chars
- 每个 section 的 items count
- 哪些 items 被丢弃
- 丢弃原因

---

## 8. 建议的 v1 上下文 section

这是一个建议版，不是最终定稿。

### 8.1 必保留 section

#### A. Current User Request

- 当前用户这次的输入
- 不做裁剪或只做极小裁剪

#### B. Current Task

- 当前 task_id
- 当前状态
- last_error / retry_count / page_kind / content_source（按需要）

#### C. Latest Observation

- 最新一步工具输出的摘要
- v1 只保留 latest，不保留完整历史 observation

### 8.2 高优先级 section

#### D. Active Plan / Failure State

- 当前还没完成的计划步骤
- 最近失败的原因
- 当前 step 的 expected observation

#### E. User Memories

- 来自 `search_user_memories(...)`
- 先排序，再经 `SessionState` 裁剪后注入

### 8.3 次优先级 section

#### F. Recent Tasks

- 最近几个任务
- 只保留短摘要，不保留冗长字段

#### G. Tool Descriptions

- 当前已有
- 后续可再做紧凑化

---

## 9. Observation Hygiene v1（正式策略）

本节基于当前讨论达成的三条共识：

1. v1 默认只保留 **latest observation** 进入 prompt；
2. failure / retry / replan 状态不混在普通 observation 里，而是进入独立 `FailureState` section；
3. 长文件 / 长正文 / 长抓取结果默认走“摘要 + 引用”，不全文注入。

### 9.1 核心原则

#### 原则 1：Observation 不是对话历史

Observation 不应被当成普通聊天消息线性堆进 prompt。

原因：

- observation 体积大；
- 结构差异大；
- 噪音比例高；
- 旧 observation 的价值衰减很快。

因此，observation 必须独立于 conversation history 管理。

#### 原则 2：默认只保留 Latest Observation

每轮 agent 决策时，默认只让模型看到：

- 最新的一条 observation；
- 或最新的一组“同一动作结果”的 observation。

Older observations 默认只保留在 trace，不直接进入 prompt。

#### 原则 3：Failure 不属于普通 Observation

失败类内容必须单独抽出来，进入独立的 `FailureState`。

因为它的职责不是“告诉模型看到什么数据”，而是：

- 告诉模型上一步为什么失败；
- 当前有哪些约束；
- 哪些路径刚刚被证明无效。

#### 原则 4：长 observation 走“摘要 + 引用”

对于长正文、长文件、长列表、长抓取结果：

- 不直接全文注入；
- 只注入 summary / preview / path / id / status / 必要元数据；
- 如果模型还需要更详细内容，应再次调用工具获取，而不是默认吃全量。

### 9.2 Observation 类型划分

| 类型 | 例子 | v1 策略 |
|---|---|---|
| Short Structured Observation | task status、小工具结果、retry result | 可直接进入 `LatestObservation` |
| Long Structured Observation | recent tasks、manual tasks、搜索结果列表 | 保留总数、前 N 项、每项核心字段 |
| Long Unstructured Observation | article archive、网页正文、大段 Markdown / HTML、大文件全文 | 默认 `summary + reference + preview` |
| Failure Observation | expectation mismatch、low-value observation、tool failed、replan trigger | 进入 `FailureState`，不混入普通 observation |

### 9.3 ContextPack section 设计

#### `LatestObservation`

建议字段：

- `source`
- `kind`
- `summary`
- `raw_chars`
- `included_chars`
- `truncated`
- `reference_path` 或 `reference_id`（如果有）

用途：

- 让模型知道“刚刚发生了什么”；
- 不让它背负完整历史 output。

#### `FailureState`

建议字段：

- `failure_kind`
- `source`
- `detail`
- `suggested_constraint`
- `user_visible_message`（如有）

用途：

- 告诉模型哪条路径刚刚失败；
- 解释为什么失败；
- 避免下一步重复同样错误。

#### `ObservationMeta`（可选，不一定进 prompt）

建议字段：

- `latest_only_policy = true`
- `older_observation_count`
- `dropped_observation_count`
- `observation_truncation_happened`

用途：

- 进入 trace；
- 不一定进入 prompt；
- 用于调试和评测。

### 9.4 v1 预算策略

`LatestObservation` 初始建议预算：

- 目标预算：`800 ~ 1200 chars`
- 超出时截断或转为摘要 + 引用

建议规则：

| 规则 | 条件 | 处理方式 |
|---|---|---|
| O1 | `raw_chars <= 800` | 原样放入 `LatestObservation` |
| O2 | `800 < raw_chars <= 2000` | 保留结构化头部，截断正文，标记 `truncated = true` |
| O3 | `raw_chars > 2000` | 进入 `summary + reference` 模式，不直接放全文 |

### 9.5 “摘要 + 引用”格式建议

长 observation 建议统一渲染为：

```text
Latest Observation
- source: read_article_archive
- summary: 该文章主要讨论微信公众号错误页识别与浏览器抓取回退策略
- preview: 标题为《...》，正文前两段主要说明...
- reference: output/articles/2026-04-14/xxx.md
- raw_chars: 12874
- included_mode: summary+reference
```

优点：

- 模型知道它看到的是“压缩表示”；
- 知道信息来源；
- 知道必要时可以进一步访问；
- 不会被全文污染。

### 9.6 Older observations 策略

正式规则：

> **Older observations default to trace-only.**

也就是说：

- older observation 默认进入 `AgentRunTrace` / markdown trace；
- older observation 默认不进入 prompt；
- 只有未来实现 `ObservationSummary` 后，older observations 才允许重新进入上下文。

### 9.7 FailureState 策略

以下情况应进入 `FailureState`：

- tool execution failed；
- expected observation mismatch；
- low-value observation；
- repeated action failure；
- stalled trajectory；
- retry / replan budget exhaustion。

建议渲染格式：

```text
Failure State
- kind: expected_observation_failed
- source: read_file
- detail: 缺少 required_field=content
- implication: 不应重复调用同样的 read_file 路径而不改变参数
```

### 9.8 Trace 记录建议

未来 trace 应记录：

- `latest_observation_chars`
- `latest_observation_truncated`
- `older_observation_count`
- `observation_drop_policy`
- `failure_state_present`

这样才能回答：

- 是 observation 太长吗？
- 是否每次都被截断？
- older observation 是否总被丢弃？
- 当前行为差是不是因为 failure state 没保留？

---

## 10. 建议的 v1 预算（初稿）

以下预算先用 chars，不先上 token。

| Section | 建议预算 | 说明 |
|---|---:|---|
| Current User Request | 必保留 | 不应被 memory 挤掉 |
| Current Task | 600-1000 chars | 以当前 task 状态为主 |
| Latest Observation | 800-1200 chars | 最新一步比历史更重要；长内容走摘要 + 引用 |
| Active Plan / Failure State | 400-800 chars | 保留最小行动约束 |
| User Memories | 延续当前 5 条 / 500 chars | 先复用已落地预算 |
| Recent Tasks | 3 条 / 300-500 chars | 只放最必要信息 |
| Tool Descriptions | 现状先保留 | 后续再压缩 |

注意：

- v1 的关键不是预算值绝对正确；
- 而是先建立“section 有预算”这个设计。

---

## 11. 当前不建议优先做的事

### 11.1 不建议先做完整滑动窗口

原因：

- 时间顺序不等于价值顺序；
- 容易让聊天噪音压过业务上下文；
- 与 AMClaw 当前 memory / task 架构不匹配。

### 11.2 不建议先做自动 summary 驱动主链路

原因：

- 复杂度高；
- 容易引入压缩误差；
- 当前还没有 section budget 做基础承接。

### 11.3 不建议先做“所有 observation 都进 prompt”

原因：

- tool output 很容易膨胀；
- 旧 observation 对当前 step 的价值通常迅速下降；
- 更适合 latest observation + trace 保存。

---

## 12. 建议的演进顺序

### Phase C1：设计落地（现在）

目标：

- 把 context strategy 文档定下来；
- 明确 section、优先级、预算、边界。

产物：

- 本文件

### Phase C2：Context Section Schema

目标：

- 把当前 `ContextAssembler` 从即时拼字符串升级为 section 化结构；
- 例如引入：
  - `ContextSectionKind`
  - `ContextSection`
  - `ContextPack`

### Phase C3：Section Budget & Trace

目标：

- 每个 section 记录 chars / item_count；
- 记录 dropped / trimmed reasons；
- trace 可以回答“哪个 section 占了多少预算”。

### Phase C4：Observation Strategy

目标：

- latest observation 单独成 section；
- older observation 暂不注入或仅保留摘要；
- 为 future summary 铺路。

### Phase C5：Session Summary（可选）

目标：

- 当微信 session 极长时，压缩早期会话；
- 不是全局强制，而是有条件启用。

---

## 13. Claude Code 机制参考（新增）

本节基于两类材料：

1. 本地研究笔记：`/Users/boyang/Desktop/Boyang_Obs/Context & Memory/Claude Code 上下文管理研究.md`
2. 张汉东笔记：`https://zhanghandong.github.io/harness-engineering-from-cc-to-ai-coding/part3/ch09.html`

需要注意：

- Claude Code 相关源码材料来自公开暴露的 snapshot / 社区分析，不是 Anthropic 官方仓库；
- 因此本节只把它当作“机制参考”，不把每个细节视为可无条件照搬的事实标准；
- 对 AMClaw 来说，最有价值的是抽象出来的 context 设计模式，而不是具体阈值或 API 假设。

### 13.1 Claude Code 不是纯滑动窗口

从研究笔记看，Claude Code 的上下文策略更接近混合架构：

```text
recent history tail
+ notes / plan / task state
+ tool-output compaction
+ summary compaction
+ selective reinjection
+ subagent context splitting
```

如果必须给一个标签，它更像：

> **notes + compaction 为核心，再叠加 multi-agent context splitting 的混合策略**

这和 AMClaw 当前判断一致：

- 不应把 sliding window 作为主策略；
- 应把 context 拆成多层；
- 对 observation / tool outputs 做单独治理；
- 对长期 memory 做 ranked injection，而不是全量注入。

### 13.2 Claude Code 的关键机制抽象

研究笔记里提炼出的 Claude Code 抽象模型，可以转译成以下模式：

| 模式 | 含义 | 对 AMClaw 的启发 |
|---|---|---|
| Stable Prefix | 固定指令、系统提示、工具 schema、项目/用户规则尽量稳定 | AMClaw 应把 instructions / tools / policy 与业务上下文分层 |
| Working Set | 当前正在做事需要的最近轨迹：最近消息、最近文件、当前 plan | AMClaw 应显式建 `ContextPack`，而不是把所有东西拼成一个字符串 |
| Observation Hygiene | 工具输出小的直接进，大的落盘/摘要/引用 | AMClaw 后续处理网页抓取、文章正文、工具输出时必须单独治理 |
| Distilled Memory | 长期记忆不是完整 transcript，而是少量高价值信息 | AMClaw 当前 `user_memories + SessionState` 方向正确 |
| Boundary Compaction | 边界 + 摘要 + 保留尾部 + 恢复关键工件 | AMClaw 后续做 summary 时应避免简单“从头截断” |
| Delegation as Context Partitioning | subagent 不只是并行，也是上下文分治 | AMClaw 未来若做多 agent，应先区分 fork vs fresh worker |
| Partial Transparency | 提供 `/context` 类视图，但内部仍有隐藏策略 | AMClaw 应尽量比 Claude Code 更可解释，记录 section budget 与 dropped reason |

### 13.3 Claude Code 压缩机制给 AMClaw 的启发

张汉东笔记对 Claude Code 自动压缩机制的启发主要是：

1. 自动压缩不应等上下文完全爆掉才触发；
2. 压缩应预留 summary 输出区和安全 buffer；
3. 压缩应有 circuit breaker，避免失败后无限重试；
4. 压缩 prompt 应是结构化模板，而不是一句“总结一下”；
5. 压缩后需要重注入关键状态，避免“压缩后失忆”；
6. tool outputs / observations 是最需要治理的上下文来源之一。

对 AMClaw 来说，最值得借鉴的不是具体阈值，而是这个流程：

```text
检测预算压力
  ↓
先治理 tool outputs / observation
  ↓
再做结构化 summary compaction
  ↓
保留 recent tail
  ↓
重注入关键状态
  ↓
记录 compact boundary / dropped reason / recovery state
```

### 13.4 AMClaw 不应直接照搬的部分

不建议直接照搬：

- Anthropic-specific prompt cache / cache edits 假设；
- 大量 feature-flag 驱动的隐藏 heuristics；
- 用户不可见的内部-only summary / attachment 机制；
- 过度复杂的 subagent context cache 设计；
- 对 coding agent 强相关的 file/repo context 策略。

原因：

- AMClaw 是微信 bot / task / article / memory 系统，不是纯 coding agent；
- 当前更需要可解释、可调参、可回归的 context 策略；
- 隐藏 heuristics 会让后续调试体验变差。

---

## 14. AMClaw Context Compression Strategy（新增）

在加入 Claude Code 参考后，AMClaw 的上下文策略建议补一条独立主线：

> **先 section budget，再 compression；先治理 observation，再治理 session summary。**

### 14.1 v1：先不做复杂自动压缩

v1 仍建议优先做：

- `ContextSection`
- `ContextPack`
- section chars / item_count
- memory `retrieved / injected / dropped`
- observation 单独 section

不建议 v1 立即做完整自动摘要压缩。

原因：

- 目前 AMClaw 的 prompt 还没有 section 级统计；
- 没有统计就做压缩，容易压错对象；
- 先 section 化，才能知道到底是哪类上下文最容易膨胀。

### 14.2 v2：Observation Hygiene

v2 最值得优先处理的是 observation / tool output。

建议策略：

- latest observation 单独成 section；
- 长 observation 先截断或摘要；
- older observations 默认留在 trace，不直接进入 prompt；
- 对大产物保留路径 / preview / summary，而不是全文注入。

这一步与 AMClaw 的文章抓取、正文归档、tool output 场景高度相关。

### 14.3 v3：Boundary Compaction

等 section 统计和 observation hygiene 稳定后，再做 boundary compaction。

建议结构：

```text
compact_boundary
+ compact_summary
+ preserved_recent_tail
+ reinjected_current_task
+ reinjected_plan_state
+ reinjected_useful_memories
```

压缩摘要至少应保留：

- 用户原始意图；
- 当前任务状态；
- 已完成事项；
- 未完成事项；
- 关键文件 / 产物路径；
- 关键失败与修复尝试；
- 用户明确约束；
- 下一步建议。

### 14.4 v4：Session Summary

Session summary 不建议作为当前第一优先级。

触发条件可以后置到：

- `merged_text` 太长；
- 用户多轮会话跨越多个 flush；
- 当前 task 尚未结束但聊天内容明显膨胀；
- trace 表明大量 recent chat 没有长期价值。

### 14.5 压缩失败策略

未来如果引入自动压缩，应设计最小 circuit breaker：

- 单次压缩失败：保留原始 recent tail，降级为截断；
- 连续失败 N 次：本 run 不再自动压缩；
- trace 记录压缩失败原因；
- 不应无限重试 summary。

---

## 15. 当前开放问题（需要讨论）

这是这份初稿最重要的部分。

### Q1：Current Task 和 Recent Tasks 的边界怎么划？

- `current_task` 是否只保留当前 task_id 对应状态？
- `recent_tasks` 是否只给标题级摘要？

### Q2：Latest Observation 的预算应有多大？

- observation 很关键，但也可能非常长；
- 是否应该优先“摘要 latest observation”，而不是直接全文注入？

### Q3：User Memories 是否要继续固定 5 条 / 500 chars？

- 这可能对当前实现足够；
- 但未来如果 current task / observation 更复杂，memory 预算可能要动态调整。

### Q4：是否需要单独的 `Session Summary`，还是当前 `merged_text` 暂时够用？

- 如果大多数用户交互仍是短回合，可能暂时不需要；
- 如果会话越来越长，就需要 session summary。

### Q5：是否要把 `trace summary` 变成上下文来源？

- 当前 trace 主要用于 audit/debug；
- 是否要把“上次失败原因的摘要”作为下轮上下文的一部分？

### Q6：context 预算是否需要 chars → tokens 升级？

- v1 chars 已经够实用；
- 但未来如果模型切换，tokens 更合理。

---

### Q7：AMClaw 是否需要 `/context` 或 context debug 命令？

Claude Code 的 `/context` 提供了一定可解释性。AMClaw 是否需要类似能力？

可能形式：

- 微信命令：`上下文`
- CLI/debug 输出：本轮各 section chars、items、dropped_count
- trace markdown：自动附带 `ContextPack` section 表

### Q8：压缩机制应该先做 observation，还是先做 session summary？

初稿倾向：

- 先 observation hygiene；
- 后 session summary。

原因：

- tool / observation 输出更容易膨胀；
- session buffer 当前还相对简单；
- trace 已经能承接完整历史。

---

## 16. 当前建议（供讨论）

如果今天只做一个方向性决定，我的建议是：

### 决定 A：不把滑动窗口作为主策略

原因：

- 不符合 AMClaw 当前结构化业务 agent 的形态；
- 容易让“最近但不重要”的内容污染上下文。

### 决定 B：把 `Snapshot + Ranked Injection` 明确为主策略

原因：

- 它已经是当前实现的实际方向；
- 只是现在还没被正式命名和 section 化。

### 决定 C：下一步先做 section schema，不急着做 summary

原因：

- section 化是后续预算管理、trace 解释、summary 压缩的基础；
- 没有 section schema，summary 也会很乱。

### 决定 D：把 Observation Hygiene 提升到 summary 之前

原因：

- Claude Code 机制显示 tool outputs 是上下文污染的核心来源之一；
- AMClaw 后续也会面对文章正文、抓取结果、tool output 膨胀；
- latest observation 应单独预算，older observations 默认留在 trace。

### 决定 E：压缩策略采用 boundary compaction，而不是简单截断

原因：

- 简单截断会丢失当前任务关键状态；
- boundary compaction 可以保留 compact summary + recent tail；
- 压缩后应重注入 current task / plan / useful memories。

---

## 17. 初稿版结论

AMClaw 当前最适合的上下文设计方向不是：

- 纯滑动窗口
- 纯摘要
- 纯检索上下文

而是：

> **以 Business Snapshot 为骨架，以 Ranked Memory Injection 为长期信息入口，以 Section Budget 为上下文控制机制。**

换句话说：

- `SessionState` 继续负责 memory 注入裁剪；
- `BusinessContextSnapshot` 继续负责业务快照；
- 下一步应引入 **section 化的 ContextPack**；
- 各 section 显式管理预算、优先级和 dropped reason；
- 滑动窗口和 summary 可以后续作为补充机制，而不是主策略；
- observation hygiene 应优先于完整 session summary；
- 如果后续需要压缩，应采用 boundary compaction，而不是简单头部截断。

---

## 18. 下一步建议

讨论完成后，建议的第一个代码实现动作不是“大改 prompt”，而是：

1. 先定义 `ContextSectionKind / ContextSection / ContextPack`
2. 把当前 `ContextAssembler` 改为先组 section，再输出字符串
3. 给 section 记录 chars 和 item_count
4. 在 trace 中记录各 section 占用

这样一来，AMClaw 的上下文系统就会从：

> “能工作的一段 prompt 拼装逻辑”

进化成：

> “可设计、可解释、可评测的上下文系统”
