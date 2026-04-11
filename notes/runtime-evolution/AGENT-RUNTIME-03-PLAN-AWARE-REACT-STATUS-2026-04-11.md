# AGENT-RUNTIME-03-PLAN-AWARE-REACT-STATUS-2026-04-11

更新于 2026-04-11。

这份文档专门记录 `AMClaw` 当前在 `ReAct` 与 `Plan & Execute` 方向上：

1. 已经完成了哪些设计与实现
2. 当前系统准确处于什么阶段
3. 下一步准备做哪些设计

这不是理想化规划文档，而是面向当前真实代码状态的工作记录。

---

## 一、当前总判断

当前 `AMClaw` 在 Agent 执行层面，已经不是单纯的“会调工具的 demo”，但也还不是成熟的 `Plan-and-Execute` 系统。

当前更准确的定义是：

- **Plan-aware ReAct v3**
- 具备：
  - 最小多步 loop
  - 显式 `plan`
  - `progress_note`
  - `step_status`
  - `expected_observation`
  - 最小 `watchdog v1`

但还不具备：

- 完整的 step state machine runtime 策略层
- `retry_policy`
- `replan_scope`
- `expected_observation` 的 richer schema
- trajectory / drift 检测
- planner / executor 双层结构
- rollback / compensation

---

## 二、已经完成的设计与实现

### 1. 最小多步 ReAct loop

已经完成：

- 支持多轮 `decide -> act -> observe -> continue/final`
- observation 会进入下一轮上下文
- planner 每轮都可以重新参与决策

当前实现要点：

- 每轮先进入 `decide(...)`
- 返回：
  - `CallTool`
  - `Final`
- 工具执行成功后，生成 `AgentObservation`
- observation 再进入下一轮 planner 输入

当前限制：

- 每轮最多一个工具调用
- 不支持并行工具
- 不支持 planner/executor 双层拆分

---

### 2. 显式 `plan` 与 `progress_note`

已经完成：

- LLM 可输出：
  - `plan: string[]`
  - `progress_note: string`
- runtime 会保存这些字段
- trace 会展示这些字段
- 下一轮上下文会看到当前 `Active Plan`

当前作用：

- 让系统拥有最小“计划意识”
- 让 trace 和调试更清晰
- 让 planner 下一轮知道当前计划草案和进度

当前限制：

- `plan` 还只是显式计划文本
- 还不是强约束执行计划机

---

### 3. `step_status`

已经完成：

- 每个计划步骤拥有状态：
  - `pending`
  - `running`
  - `done`
  - `failed`
  - `skipped`

当前状态推进规则：

1. 新 plan 出现时：
   - 所有步骤初始化为 `pending`
2. 某一步准备执行工具时：
   - 第一个 `pending` -> `running`
3. 工具成功且满足完成条件：
   - `running` -> `done`
4. 工具失败或校验失败：
   - `running` -> `failed`
5. 最终成功收口时：
   - 剩余 `pending` -> `skipped`

关键意义：

- step 状态开始由 runtime 事实驱动
- 不再只是靠 `progress_note` 暗示进度

当前限制：

- 没有 `current_step_index`
- 没有 step 级 retry 计数
- 没有 step 级历史

---

### 4. `expected_observation` / `done_rule`

已经完成：

- planner 可显式输出最小期望观测字段：
  - `expected_kind`
  - `done_rule`
  - `required_field`

当前支持的 `ObservationKind`：

- `text`
- `json_object`
- `file_mutation`
- `task_status`
- `task_list`
- `archive_content`

当前支持的 `DoneRule`：

- `tool_success`
- `non_empty_output`
- `required_json_field`

当前默认映射（runtime 自动补）包括：

- `create` / `write` -> `tool_success`
- `read` -> `non_empty_output`
- `get_task_status` -> `required_json_field("found")`
- `list_recent_tasks` / `list_manual_tasks` -> `tool_success`
- `read_article_archive` -> `required_json_field("content")`

关键意义：

- 现在“工具成功”不再自动等于“step done”
- step 完成开始依赖可验证 observation

当前限制：

- 规则仍然很轻量
- 还没有 richer `done_condition`
- 还没有 `failure_condition`
- 还没有 observation value / novelty 判断

---

### 5. `watchdog v1`

已经完成的最小 failure diagnosis：

#### `expectation`

触发条件：
- 工具执行成功
- 但 observation 不满足 `done_rule`

当前动作：
- 记录 failure
- 当前 step 标记为 `failed`
- 如果可 replan，则转为 failure observation 进入下一轮
- 否则直接 abort

#### `repeated_action`

触发条件：
- 非首轮
- 有 observation
- 当前仍然重复上一次同类动作摘要

当前动作：
- 记录 failure
- 当前 step 标记为 `failed`
- 如果可 replan，则进入下一轮
- 否则直接 abort

当前还保留的 failure kind 但尚未系统扩展：
- `semantic`
- `irrecoverable`

当前 `FailureAction` 只支持：
- `replan`
- `abort`

关键意义：

- runtime 已经开始具备最小“看门狗”能力
- 不再只是盲目信任 planner 一直往下走

当前限制：

- 还没有 `retry_step`
- 还没有 failure budget
- 还没有 trajectory-level drift detection
- 还没有 observation value / novelty 检测

---

### 6. 设计原则：不信 `Thought`，只信结构化动作和可验证状态

这一条虽然不是单独模块，但已经明确成为当前实现方向：

- `Thought` / `progress_note`
  - 可以展示
  - 可以记录
  - 可以进入上下文
  - 但不能直接作为执行依据

真正驱动执行的是：

- 结构化 `action`
- 可验证 `observation`
- `step_status`
- 业务状态
- failure diagnosis

这一原则已经通过下面两步开始落地：

1. `step_status` 由 runtime 推动，不由 note 推动
2. `done` 判定依赖 `expected_observation`

---

## 三、当前系统还没有完成的部分

下面这些是当前 `ReAct / Plan & Execute` 仍然缺失的：

### 1. `current_step_index`

当前没有显式的“当前正在执行第几个 plan step”的独立字段。

### 2. richer `expected_observation`

当前还没有：
- `failure_condition`
- `success_condition`
- `done_condition` 组合逻辑
- `expected_schema`
- `value_novelty`

### 3. `retry_policy`

当前还没有：
- `retry_step`
- `max_retry_per_step`
- `transient failure` 与 `semantic failure` 的区分

### 4. `replan_scope`

当前一旦进入 replan，还是比较粗糙的：
- 还没有区分“局部 replan”与“整体 replan”

### 5. trajectory / goal drift detection

当前没有：
- 目标漂移检测
- 多轮信息增量停滞检测
- 历史 Action / Observation 的更高级 watchdog 判断

### 6. planner / executor 分层

当前还是：
- 单一 loop 内完成 planner 与 execution

还没有拆成更清晰的：
- planner
- executor
- watchdog
- state controller

### 7. rollback / compensation

当前没有统一 rollback：
- 文件写入不会自动撤销
- 失败时不会自动补偿前序副作用

---

## 四、下一步准备做的设计

下面这些是后续最值得推进的部分，按建议优先级排序。

### A. `StepFailureKind` 扩展

当前已经有最小分类，但后续应明确扩展成更稳定的 failure taxonomy，例如：

- `transient`
- `expectation`
- `repeated_action`
- `semantic`
- `irrecoverable`

目标：
- 让 retry / replan / abort 不再混在一起

### B. `FailureAction` 扩展

当前只有：
- `replan`
- `abort`

后续建议引入：
- `retry_step`
- `fallback_tool`
- `ask_user`

目标：
- failure handling 不再只剩“重规划或结束”两种粗动作

### C. `Observation Value / Novelty Check`

这是当前最值得继续做的一项。

目标：
- 不是只判断 observation 是否“合法”
- 还要判断 observation 是否“有增量 / 有推进价值”

因为：
- 有返回，不等于有进展
- 这正是 `结果失效型` 问题的核心

### D. `current_step_index` + executor state

目标：
- 把 plan 从“有状态的文本”继续推进到“更像真正的执行器状态”

### E. `expected_observation` richer schema

后续可以从当前 B-lite 版本升级到更强的结构：

- `kind`
- `done_rule`
- `failure_condition`
- `expected_fields`
- `minimum_novelty`

### F. `replan_scope`

目标：
- 区分：
  - 当前 step 重规划
  - 剩余 plan 重规划
  - 整体重规划

### G. planner / executor / watchdog 分层

这是长期方向。

当前更理想的演化方向是：

- planner 负责生成计划与动作
- executor 负责执行当前 step
- watchdog 负责诊断失控征兆
- state/controller 负责推进状态与预算

---

## 五、当前最准确的一句话定义

截至 2026-04-11，`AMClaw` 当前在 `ReAct / Plan & Execute` 上最准确的定义是：

- **带 `step_status`、`expected_observation` 与最小 watchdog 的 Plan-aware ReAct v3**

它已经超出“会调工具”的层面，
但还没有到成熟 `Plan-and-Execute` 执行框架的程度。
