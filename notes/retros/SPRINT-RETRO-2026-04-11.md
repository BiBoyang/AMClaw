# SPRINT-RETRO-2026-04-11

更新于 2026-04-11。

这份文档记录今天围绕 `AMClaw` 的一整轮推进情况，重点不是列出所有零散改动，而是回答四件事：

1. 今天到底推进了什么
2. 当前系统已经到什么阶段
3. 哪些关键判断在今天被坐实了
4. 下一轮应该从哪里继续，而不是重复今天已经做过的事

---

## 一、今天的总目标

今天的主线不是新增一个孤立 feature，而是把 `AMClaw` 的 Agent 执行层，从：

- 最小 ReAct loop
- 最小工具调用
- 最小上下文

继续推进成：

- 带显式计划的执行系统
- 带状态推进的 runtime
- 带失败语义与重规划范围的混合执行系统

换句话说，今天主要做的是：

- **执行系统层升级**

而不是简单地再堆几个用户命令。

---

## 二、今天具体完成了什么

### 1. `step_status` 落地

今天补齐并稳定了计划步骤状态：

- `pending`
- `running`
- `done`
- `failed`
- `skipped`

并且这些状态不再只是概念，而是由 runtime 根据执行结果推进。

这意味着：

- 计划不再只是 `plan: string[]`
- 系统开始真正拥有“当前执行到哪一步”的基础语义

### 2. `expected_observation` / `done_rule` 落地

今天把“工具成功 ≠ step 完成”这件事真正做成了系统行为。

当前支持：

- `expected_kind`
- `done_rule`
- `required_field`

并配有默认映射，例如：

- `read` -> `non_empty_output`
- `get_task_status` -> `required_json_field(found)`
- `read_article_archive` -> `required_json_field(content)`

这一步非常关键，因为它意味着：

- 现在 step 的完成开始依赖 observation 是否符合预期
- 而不是单纯看工具有没有报错

### 3. `StepFailureKind` 与 `FailureAction` 进入最小可用状态

当前 failure taxonomy 已经包含：

- `transient`
- `expectation`
- `low_value_observation`
- `repeated_action`
- `trajectory_drift`
- `semantic`
- `irrecoverable`

当前 failure action 已经包含：

- `retry_step`
- `replan`
- `abort`

这意味着：

- 系统已经不再把“失败”视为单一事件
- 而开始按照类型采取不同动作

### 4. `retry_step` 落地

今天正式把 `transient` 失败接到：

- `retry_step`

并且当前系统已经具备：

- 每个 step 最多自动重试一次

这说明失败处理已经从：

- “失败就结束 / 失败就重规划”

推进到：

- “先判断是否只是一次临时执行失败”

### 5. `replan_scope` 落地

今天把 `replan` 进一步结构化成三种范围：

- `current_step`
- `remaining_plan`
- `full`

这一步的意义在于：

- 系统开始明确“重规划到底重画多大范围”
- 不再只有一个模糊的 `replan`

这为后续 executor 分层提供了非常重要的基础。

### 6. `watchdog v1` 继续前进

今天把 watchdog 又往前推了一步。

当前已经有：

- `repeated_action`
- `low_value_observation`
- `trajectory_drift`

尤其是：

- `low_value_observation` 对应了“有返回，不等于有进展”
- `trajectory_drift` 对应了“计划未完成却提前 Final”

这已经开始贴近我们关于 `ReAct` 失控问题的系统性治理了。

### 7. 文档体系继续完善

今天还补了一份关键文档：

- `../runtime-evolution/AGENT-RUNTIME-04-PARADIGM-EVOLUTION-2026-04-11.md`

它专门回答：

- 当前 `AMClaw` 已经不是纯 `ReAct`
- 也还不是完整 `Plan-and-Execute`
- 它更像一种：
  - `ReAct + Planning-aware execution + runtime/watchdog control`
  的混合范式

这让今天的代码推进和设计思考终于能被统一放到一个范式叙事里看。

---

## 三、今天最重要的设计判断

### 1. `AMClaw` 现在已经不是纯 ReAct

这是今天最重要的判断之一，而且我认为已经不只是“口头总结”，而是代码层面成立。

因为当前系统已经有：

- 显式 `plan`
- `progress_note`
- `step_status`
- `expected_observation`
- `done_rule`
- `StepFailureKind`
- `FailureAction`
- `ReplanScope`
- `watchdog`

这些东西一旦都出现，系统的本质就已经从：

- “边想边调工具”

变成了：

- “带计划意识、执行语义和控制逻辑的混合执行系统”

### 2. `Thought` 不应被信任为执行依据

这也是今天很核心的一条。

当前系统继续沿着这个原则推进：

- `progress_note` 可以展示
- `plan` 可以展示
- 但 step 是否完成不再依赖这些文字
- 依赖的是：
  - 结构化 action
  - observation
  - `expected_observation`
  - `step_status`
  - failure diagnosis

这说明系统已经开始从“语言解释驱动”转向“事实状态驱动”。

### 3. 失败后不能只剩一个动作

今天把 `retry_step / replan / abort` 分出来以后，整个执行系统的感觉明显不一样了。

这其实意味着：

- 系统开始拥有最小执行策略层
- 而不是只是一个会调工具的循环

这个变化我认为非常重要。

---

## 四、当前系统准确处于什么阶段

如果要给当前执行系统一个比较准确的阶段名称，我会这样定义：

### `Plan-aware ReAct v4.5`

它现在已经具备：

- 多轮 ReAct loop
- 显式 plan
- progress note
- step status
- expected observation
- done rule
- failure taxonomy
- retry / replan / abort
- replan scope
- watchdog v1

但它还不具备：

- `current_step_index`
- richer `expected_observation` schema
- 更成熟的 failure policy
- planner / executor / watchdog 分层
- rollback / compensation
- 更强的 trajectory drift 检测

所以当前状态已经超过“最小 Agent demo”，但还没有到成熟 `Plan-and-Execute` 框架。

---

## 五、今天哪些事情没有继续做，是刻意的

今天其实完全可以继续往下做很多东西，比如：

- richer `expected_observation`
- `current_step_index`
- 更强 `trajectory_drift`
- executor 分层
- rollback / compensation

但今天没有继续往下推，是刻意的。

原因很简单：

- 到当前这一步，执行系统主线已经形成了一条很完整的阶段链
- 如果继续不停往下加，反而会让今天这轮的结构性成果被淹掉

今天最适合收工的点，就是：

- 先把这个阶段完整落下来
- 再在下一轮继续

我认为这是正确的节奏。

---

## 六、今天的结果有多稳

今天最后我跑过：

- `cargo check`
- `cargo test`

当前结果：

- **`134 passed`**

这意味着：

- 这不是一轮“想法很多但代码不稳”的推进
- 而是一轮：
  - 设计推进
  - 代码落地
  - 测试托底
  同时成立的推进

---

## 七、下一轮最自然的延续方向

如果明天继续，我建议不要横向发散，而是沿着当前执行系统主线继续往前推。

### 第一优先

- `current_step_index`
- richer `expected_observation`

原因：

- 这是继续把“执行语义”做稳的最自然下一步
- 也是从当前 `Plan-aware ReAct` 向更完整 `Plan-and-Execute` 过渡的关键桥梁

### 第二优先

- 更强的 `trajectory_drift`
- 更细的 `Observation Value / Novelty Check`

原因：

- 这会更贴近“方向漂移型”失稳治理
- 也更贴近对 `ReAct` 的系统性修正

### 第三优先

- planner / executor / watchdog 分层草案

原因：

- 这会真正推动范式从：
  - `Plan-aware ReAct`
  走向：
  - `Planner-led hybrid system`

---

## 八、一句话总结

今天真正做成的，不是“多了几个功能”，而是：

> **把 `AMClaw` 从一个会调工具的 Bot，推进成了一个开始拥有执行状态、完成条件、失败语义和重规划范围的混合执行系统。**

而且这个系统现在已经能被测试、能被复盘、也开始能被治理。
