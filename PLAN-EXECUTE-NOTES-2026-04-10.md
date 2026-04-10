# PLAN-EXECUTE-NOTES-2026-04-10

更新于 2026-04-10。

这份记录用于保存一次围绕 `AMClaw` 当前 `Plan & Execute` / `ReAct` 实现现状的问答与判断，重点回答：

1. 当前 `plan` 的失败逻辑是什么
2. 当前最多跑几轮
3. 中间失败后会不会回滚
4. 当前 `replan` 是怎么做的

这份记录只描述 **当前真实实现**，不描述理想化未来方案。

---

## 一、用户问题

用户问题是：

1. `plan` 你是如何做失败逻辑的？
2. 现在要搞几轮？
3. 如果中间的过程失败，回滚了吗？
4. `replan` 是如何做的？

---

## 二、当前实现的直接结论

### 1. 失败逻辑

当前 `Plan & Execute v1` 的失败逻辑是：

- **fail-fast**
- 某一步工具调用失败，整次 agent run 立即结束
- 不会继续自动执行下一步
- 不会自动重做当前 step
- 不会自动重规划整个 plan

对应实现：
- `src/agent_core/mod.rs:417`
- `src/agent_core/mod.rs:435`

也就是说：

- planner 可以给出计划步骤
- runtime 也会记录这些步骤
- 但一旦某个工具执行报错，当前 run 会直接 `Err(err)` 退出

### 2. 当前轮数上限

当前默认最大轮数是：

- `8` 轮

对应实现：
- `src/agent_core/mod.rs:15`
- `src/agent_core/mod.rs:417`

行为规则：

- 每轮先做一次决策
- 如果返回 `Final`，立即结束
- 如果持续返回 `CallTool`，继续进入下一轮
- 超过最大步数仍未收敛，则报错：
  - `达到最大步骤，未能收敛`

所以：

- 不是固定跑 8 轮
- 是“最多 8 轮”
- 由收敛情况提前结束

### 3. 当前是否回滚

当前没有统一的 agent-level rollback。

更准确地说：

- **没有跨步骤统一回滚机制**
- **没有 compensating action**
- **没有 undo step**
- **没有 rollback plan**

当前只有两种“局部保护”：

#### 3.1 底层模块自己的事务
像 `task_store` 的部分写操作，本身会使用 SQLite transaction，
但那只是：

- 单个函数内部的原子性
- 不是整个 agent run 的跨步骤回滚

#### 3.2 只读工具没有副作用
当前多数业务工具是只读的：

- `get_task_status`
- `list_recent_tasks`
- `list_manual_tasks`
- `read_article_archive`

所以这些工具失败时，本身也没有“已写入状态需要回滚”的问题。

#### 3.3 文件工具没有自动撤销
当前文件工具：

- `read`
- `write`
- `create`

如果已经执行成功：
- 文件就已经写入工作区
- 后续步骤失败时，不会自动删除或恢复原状

对应实现：
- `src/tool_registry/mod.rs:55`
- `src/tool_registry/mod.rs:93`

结论：

- 当前没有统一 rollback
- 只有局部事务
- 文件写入不会自动回滚

### 4. 当前 replan 怎么做

当前存在 `replan`，但只是 **最小意义上的隐式 replan**。

它的真实机制是：

- 每一轮都会重新进入 `decide(...)`
- 每一轮都会重新组装 `PlannerInput`
- 每一轮都会重新调用 planner

对应实现：
- `src/agent_core/mod.rs:419`
- `src/agent_core/mod.rs:430`
- `src/agent_core/mod.rs:447`

当前 replan 依赖的输入包括：

- 原始用户输入
- `RunContext`
- 最新 `observation`
- 当前任务摘要
- 最近任务摘要
- 用户记忆
- 可用工具列表
- 当前 active plan
- 上一轮 `progress_note`

对应实现：
- `src/agent_core/mod.rs:177`
- `src/agent_core/mod.rs:202`
- `src/agent_core/mod.rs:250`

但需要强调的是：

当前 replan 还不是一个显式的、独立的 replan 子系统。

它还没有：

- plan diff
- 局部修订剩余步骤
- step 状态迁移
- 失败后自动重试策略
- plan tree
- 重规划预算控制

所以当前更准确的描述是：

- **implicit replan**
- 而不是成熟的 **explicit replanning subsystem**

---

## 三、当前 `plan` 的实际作用是什么

当前 LLM 已可输出：

- `plan: string[]`
- `progress_note: string`

对应实现：
- `src/agent_core/mod.rs:129`
- `src/agent_core/mod.rs:1834`

这些字段当前的作用主要是：

1. 写入 trace
2. 在 markdown trace 中可见
3. 在下一轮 planner prompt 中可见
4. 让 planner 具备最小“计划意识”

对应实现：
- trace 记录计划：`src/agent_core/mod.rs:1059`
- markdown 展示计划：`src/agent_core/mod.rs:1331`
- active plan 注入下一轮上下文：`src/agent_core/mod.rs:177`

但当前还没有：

- step index 的强执行约束
- 每个 step 的状态管理（pending/running/done/failed）
- executor 严格按 plan 驱动
- plan validation

所以当前的 `plan` 更像：

- **planner 输出的显式工作说明**

而不是：

- **runtime 严格约束执行的计划状态机**

---

## 四、当前系统到底属于什么阶段

现在的 `AMClaw` 在 `Plan & Execute` 这块，应该这样定义：

### 不是

- 完整成熟的 `Plan & Execute`
- 强状态机驱动的执行器
- 具备重规划 / 回滚 / 补偿机制的执行框架

### 而是

- **ReAct + 显式计划字段**
- 或者说：
- **Plan-aware ReAct v1**

这意味着：

- 已经有最小计划意识
- 已经有最小进度表达
- 已经能把计划显示出来并带到下一轮
- 但还没有形成真正独立的计划执行子系统

---

## 五、当前最明显的缺口

如果继续把 `Plan & Execute` 往下做，当前最明显缺口有：

1. `step state machine`
   - 目前没有 `pending/running/done/failed/skipped`
2. 失败后处理策略
   - 目前没有“重做当前 step / 重做整份 plan”的区分
3. rollback / compensation
   - 目前没有统一回滚与补偿动作
4. plan validation
   - 当前 plan 是可显示的，不是强约束的
5. executor 层
   - 目前没有独立 `planner + executor` 双层结构

---

## 六、这轮问答后的建议讨论方向

如果后续继续深入研究 `Plan & Execute`，最值得讨论的 4 个点是：

1. 计划是否要从 `string[]` 升级成结构化对象
   - `goal`
   - `steps`
   - `expected_output`
   - `done_condition`
2. 是否需要正式的 step 状态机
3. 失败后该“重做当前 step”还是“整体 replan”
4. rollback 应做到哪一层
   - 只做数据库事务
   - 还是做 agent-level compensation

---

## 七、一句话总结

当前 `AMClaw` 的 `Plan & Execute`：

- **有 plan**
- **有 progress**
- **有多轮 replan**
- **但失败时仍是 fail-fast**
- **没有统一 rollback**
- **还不是成熟计划执行系统**

最准确的定义是：

- **Plan-aware ReAct v1**

---

## 八、关于 `Thought` / `Reasoning` 是否可信的补充讨论

### 用户追问

用户进一步提出了一个很关键的问题：

- `Thought` 看起来很完整，理由也说得通，但未必真的是在驱动后面的 `Action`
- 很多时候更像是：
  - 模型先有动作倾向
  - 再回头补一段解释
  - 让整个轨迹看起来更连贯
- 很多 agent 框架后来都对 `Thought` 持负面或保守态度：
  - `Thought` 可以展示事件流
  - 可以进入上下文
  - 但不信任它作为执行依据

用户的问题本质是：

- 后续继续做 `Plan & Execute` 时，要不要明确防止系统把 `Thought` 当成可信执行依据？

### 回答结论

结论非常明确：

- **要，而且必须明确写成设计原则。**

更直接地说：

- `Thought` / `Reasoning` / `progress_note`
  - 可以展示
  - 可以记录
  - 可以进入下一轮上下文
  - 但**不能直接驱动执行判定**

真正应该驱动执行的应该是：

1. 结构化 action
2. 可验证 observation
3. executor state
4. 业务状态机
5. 明确的 step status

### 为什么要这样做

因为 `Thought` 很容易出现下面这种情况：

- 模型先决定了一个动作
- 再补一段看起来合理的解释
- 让轨迹更像“我思考过”

这意味着：

- `Thought` 很可能只是 **事后解释文本**
- 而不是真正可靠的执行控制信号

所以如果系统把 `Thought` 当作真理，会有几个风险：

1. 以为 planner 真在按它说的方式执行
2. 实际上它只是生成了一段 narrative
3. 最终让 runtime 的状态被“解释性文本”污染

### 后续应该采用的分层原则

后续继续做 `Plan & Execute` 时，建议把系统信息分成三层：

#### 1. 可执行控制层

这些可以驱动执行：

- `action`
- `tool_name`
- `tool_args`
- `current_step_index`
- `step_status`
- `expected_observation`
- `done_condition`

特点：

- 结构化
- 可校验
- 可比对
- 可追踪

#### 2. 可验证事实层

这些可以作为运行时判断依据：

- 工具返回结果
- 数据库状态
- 文件是否存在
- 任务状态
- 归档是否成功
- `context_token` 是否存在

特点：

- runtime 能真实观测
- 可用于 replan / retry / finish 判断

#### 3. 解释 / 叙述层

这就是：

- `Thought`
- `Reasoning`
- `progress_note`
- `planner_note`

这些内容：

- 可以帮助人类理解轨迹
- 可以帮助模型维持连续性
- 但应该视为：
  - `advisory`
  - `non-authoritative`
  - `untrusted narrative`

### 应该落地成哪些明确规则

建议后续把这几条规则写进 `Plan & Execute` 设计里：

#### 规则 A：`Thought` / `progress_note` 不能直接决定执行结果

例如不能做：

- 因为 note 里说“step 1 已完成”，就直接把 step 标记为 `done`
- 因为 note 里说“文件应该已经生成”，就跳过验证

#### 规则 B：step 完成必须由 observation 或状态确认

例如：

- `read` 成功 -> 有工具返回
- `write` 成功 -> 文件存在 / 工具成功返回
- `get_task_status` 成功 -> 有结构化状态对象
- 日报生成成功 -> 产物路径存在

也就是说：

- **step 是否完成必须绑定可验证 observation**
- **不能绑定 thought**

#### 规则 C：replan 只能主要基于事实层，不基于 thought 自嗨

replan 应该看：

- 上一步 observation
- 当前任务状态
- 当前 plan 剩余步骤
- 错误类型
- retry 次数

而不是主要看：

- 模型上一轮怎么解释自己
- 模型上一轮自称“我觉得已经完成了”

#### 规则 D：`Thought` 只作为软上下文，不作为硬依据

允许：

- 下一轮 planner 看到 `progress_note`
- 看到 plan 草案
- 看到解释性 reasoning

但必须明确：

- 这些只是辅助文本
- 不能替代真实状态

### 对当前实现的评价

结合当前 `AMClaw` 现状，可以做出一个比较正面的判断：

- 现在系统还没有把 `Thought` 变成强控制逻辑
- 目前的 `plan` / `progress_note` 主要还是：
  1. trace 可见
  2. prompt 可见
  3. 计划感增强

这其实是健康的。

也就是说：

- 当前实现还没有犯“把 thought 当成真理”的大错误
- 但如果下一步继续做 `Plan & Execute v2`，就必须把这个边界正式写清楚

### 建议的工程化原则（一句话版）

后续建议明确采用下面这句话作为设计原则：

- **Thought 可以看，可以记，可以传，但不能直接信。**

更工程化一点说：

- **执行要信结构化动作和可验证状态，不要信模型生成的解释性文本。**

### 后续最值得继续推进的两个点

如果继续往下做 `Plan & Execute v2`，最应该优先推进的是：

#### 1. `step state machine`

引入：

- `pending`
- `running`
- `done`
- `failed`
- `skipped`

并规定：

- `done` 只能由 observation / state 校验后更新
- 不能由 `progress_note` 更新

#### 2. `expected_observation`

让 planner 不只输出：

- 下一步调用哪个工具

还输出：

- 我预期会拿到什么类型的 observation
- 成功长什么样
- 失败长什么样

这样后续 runtime 才能真正做：

- step 完成判断
- failure handling
- replan 触发
- 更可信的 executor

### 这轮补充讨论的一句话总结

当前继续推进 `Plan & Execute` 时，必须明确：

- `Thought` 只是解释性文本
- 可以展示、可以传递、可以作为软上下文
- 但**不能直接驱动执行判定**

后续真正应当驱动执行的，是：

- 结构化 action
- 可验证 observation
- step status
- 业务状态
