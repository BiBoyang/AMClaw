# AGENT-RUNTIME-04-PARADIGM-EVOLUTION-2026-04-11

更新于 2026-04-11。

这份文档专门记录 `AMClaw` 当前在 Agent 设计范式上的思考、判断与解释。它不是单纯的实现记录，也不是抽象概念介绍，而是围绕一个核心问题展开：

> `AMClaw` 现在到底是在做纯 `ReAct`，还是在往 `Planning / Plan-and-Execute` 走？
> 如果不是纯 `ReAct`，那当前的混合态应该怎么理解？
> 之后设计范式的推进方向又应该是什么？

这份文档有意写得更完整一些，既服务项目内部记录，也尽量保留足够完整的论证脉络，方便后续整理成技术博文。

---

## 一、核心结论

当前 `AMClaw` 已经**不是纯 `ReAct` 系统**。

更准确地说，`AMClaw` 现在正在形成一种：

- **ReAct + Planning-aware execution + runtime/watchdog control**

的混合范式。

它当前不是：

- 纯 `ReAct`
- 纯 `Plan-and-Execute`
- 纯 prompt 驱动的 agent loop

而是一个正在演化中的中间状态。这个状态并不是“偏了”，而是我认为**走对了**。

一句话定义当前范式：

> `AMClaw` 当前是一个 **Plan-aware ReAct runtime**，并且正在逐步过渡到 **Planner 统筹、Executor 执行、Watchdog 托底** 的混合执行系统。

---

## 二、为什么说它已经不是纯 ReAct

如果只看最经典、最原始的 `ReAct`，它的核心模式通常是：

1. 模型根据当前上下文思考一步
2. 产出一个 `Action`
3. 调用工具
4. 拿到 `Observation`
5. 再进入下一轮思考
6. 最后输出 `Final`

它的关键特征是：

- 强在线反应
- 强局部闭环
- 弱全局计划约束
- planner 和 executor 常常混在一个循环里

### 纯 ReAct 的典型特点

如果一个系统是真正偏纯的 `ReAct`，它往往具有这些特征：

- 没有显式 `plan`
- 没有显式 step 状态机
- 工具成功通常就被视作“这一步完成”
- 很少有清晰的 failure taxonomy
- 很少有单独的 watchdog / runtime control 层
- 很少区分“当前 step 执行”和“整体计划维护”

### AMClaw 已经超出的部分

`AMClaw` 现在已经具备这些东西：

- 显式 `plan`
- `progress_note`
- `step_status`
- `expected_observation`
- `done_rule`
- `StepFailureKind`
- `FailureAction`
- `ReplanScope`
- 最小 `watchdog`
- 最小 `trajectory_drift`

这些东西一旦出现，系统的本质就已经变了。

它不再只是：

- “模型边想边调工具”

而是开始出现：

- 计划结构
- 执行状态
- 完成条件
- 失败分类
- 重规划范围
- 执行控制层

所以说它已经不是纯 `ReAct`，是一个很直接的判断，不是修辞上的夸大。

---

## 三、但它也还不是纯 Planning / Plan-and-Execute

虽然现在已经不再是纯 `ReAct`，但 `AMClaw` 也还不能被称为完整成熟的 `Plan-and-Execute`。

### 为什么还不是纯 Plan-and-Execute

因为典型意义上的 `Plan-and-Execute`，通常意味着：

1. planner 先生成一个相对完整的计划
2. executor 按计划推进执行
3. 执行时主要由状态机和执行器负责前进
4. 中途失败时再由 planner 负责重规划

也就是说，它往往至少会有比较清晰的两层：

- planner
- executor

而当前 `AMClaw` 还没有真正把这两层拆开。

### 当前还缺哪些 Planning 层特征

当前 `AMClaw` 还没有：

- 显式 `goal`
- 独立的 `current_step_index`
- 完整的 step contract
- `success_condition / failure_condition`
- planner / executor 分层
- planner 维护剩余计划的完整机制
- dependency graph
- 计划级预算与策略控制
- rollback / compensation 体系

所以当前不能说它已经是完整 `Plan-and-Execute`，而更像是：

- `ReAct` 主体仍然存在
- `Planning` 成分已经开始进入
- 但还在“半结构化阶段”

这就是为什么我会更倾向于叫它：

- **Plan-aware ReAct**

而不是直接说：

- `AMClaw` 已经做完了 `Plan-and-Execute`

---

## 四、现在的 AMClaw 更准确地属于什么范式

如果一定要给当前范式一个比较精确的名字，我觉得至少有三个候选：

### 候选一：Plan-aware ReAct

这是我目前最推荐的叫法。

原因：

- 仍然保留了在线闭环的 `ReAct` 核心
- 同时已经加入显式计划和执行约束
- 不会误导别人以为它已经完成了完整 `Plan-and-Execute`

### 候选二：ReAct with execution semantics

这个名字更强调：

- 不是纯 prompt 式 `ReAct`
- 而是开始带执行语义了

这个叫法也对，但不如第一个直观。

### 候选三：Hybrid ReAct / Plan-and-Execute runtime

这个名字更适合写在技术博文或对外描述里。

因为它直接告诉别人：

- 这不是纯 `ReAct`
- 也不是纯 `Plan-and-Execute`
- 而是一个混合系统

如果写在文章里，我会更偏向：

> `AMClaw` 当前正在从 `ReAct` 演进到一个由 Planning 约束、由 runtime/watchdog 托底的混合执行系统。

---

## 五、从系统演进角度看，现在处在哪一阶段

我更喜欢把这条演进路线拆成几个阶段，而不是只用“做没做完”来判断。

### 阶段 1：最小 ReAct

系统形态：

- 想一步
- 做一步
- 看一步
- 再想一步

特点：

- 工具调用是核心
- 没有计划对象
- 没有执行状态机

### 阶段 2：显式计划进入系统

系统开始出现：

- `plan`
- `progress_note`

特点：

- 计划不再只存在于模型“脑内”
- 至少在 trace 和上下文里是显式存在的

### 阶段 3：执行语义开始形成

系统开始出现：

- `step_status`
- `expected_observation`
- `done_rule`

特点：

- step 不再只靠“工具成功”推进
- runtime 开始接管“完成条件”

### 阶段 4：控制层开始出现

系统开始出现：

- `StepFailureKind`
- `FailureAction`
- `ReplanScope`
- `watchdog`
- 最小 `trajectory_drift`

特点：

- 不再只是 planner 说了算
- runtime / watchdog 开始成为独立控制力量

### 当前所处阶段

如果按这个分法，`AMClaw` 现在已经处在：

- **阶段 4：带控制层的混合执行系统**

也就是说：

- 还没长成成熟 executor framework
- 但已经明显不再是“只会调工具的 ReAct”

---

## 六、为什么这种“混合态”不是问题，反而是合理结果

很多人在讨论 Agent 范式时，容易陷入一种二选一：

- 要么做纯 `ReAct`
- 要么做纯 `Plan-and-Execute`

但工程上真实情况往往不是这样。

### 为什么纯 ReAct 会不够

因为纯 `ReAct` 很容易遇到：

- 参数重复型失稳
- 结果失效型失稳
- 方向漂移型失稳
- observation 污染上下文
- thought 看起来合理但不能被信任

一旦系统进入真实世界，这些问题就会逼你补：

- 状态
- 约束
- 失败分类
- 执行控制

这些一补上，系统自然就不再是纯 `ReAct` 了。

### 为什么纯 Planning 现在也还不合适

因为当前 `AMClaw` 里仍然有很多任务是高不确定性的：

- 查任务 / 查归档 / 查记忆时，仍要边走边看
- observation 很可能改变下一步策略
- 业务上下文还在演化，不适合一开始就把所有路径写死

所以如果现在直接硬切到纯 `Plan-and-Execute`：

- planner 复杂度会陡增
- executor 也会变重
- 你反而会失去 `ReAct` 的局部灵活性

### 所以混合是自然结果

最合理的结果反而是：

- 整体保持 `ReAct` 的闭环能力
- 再逐步引入：
  - 计划
  - 状态
  - 完成条件
  - 失败控制

这就是为什么我会说：

- `AMClaw` 现在不是“偏了”
- 而是在**自然地长向一个更成熟的混合范式**

---

## 七、当前这套混合范式，可以怎么分层理解

我建议把 `AMClaw` 当前 Agent 设计分成三层来理解。

### 第一层：局部反应层（ReAct）

职责：

- 每轮根据 observation 决定下一步
- 保留在线反馈能力
- 允许局部探索与即时调整

这是当前系统还保留最强的一层。

### 第二层：计划约束层（Planning-aware）

职责：

- 提供 `plan`
- 提供 `progress_note`
- 提供 `expected_observation`
- 提供 `done_rule`
- 提供 `step_status`

这层的作用是：

- 让系统不再完全靠临场反应
- 开始有“我正在执行一份计划”的意识

### 第三层：执行控制层（runtime / watchdog）

职责：

- 失败分类
- retry / replan / abort
- replan scope
- repeated action 检测
- low-value observation 检测
- trajectory drift 检测

这层其实已经不属于经典意义上的 `ReAct` 了。
它更像：

- executor
- runtime controller
- watchdog

也正是这一层，让系统开始真正具有“执行系统”的味道。

---

## 八、当前已经完成的 Planning 成分到底有哪些

如果只问：

> `Planning` 到底做了多少？

我会这样回答：

### 已经完成的 Planning 成分

1. 显式 `plan`
2. `progress_note`
3. `step_status`
4. `expected_observation`
5. `done_rule`
6. `FailureAction`
7. `ReplanScope`
8. 最小 watchdog

这些说明：

- `Planning` 已经进入系统
- 而且不再只是 prompt 层概念
- 开始被 runtime 消费

### 还没完成的 Planning 成分

1. `goal`
2. `current_step_index`
3. richer step contract
4. `success_condition / failure_condition`
5. planner / executor 分层
6. 依赖关系表达
7. 计划级预算控制
8. rollback / compensation

这说明：

- `Planning` 已经开始了
- 但还没彻底长成上层控制框架

所以最准确的说法不是：

- “还没做 Planning”

而是：

- **已经做了 Planning 的一部分，但还处在半结构化阶段**

---

## 九、接下来范式推进的方向是什么

如果只谈“范式推进”，而不谈某个具体 feature，我会建议按下面的方向往前走。

### 方向 A：继续把执行语义做稳

这是最近一步，最现实，也最值得做。

重点包括：

- `current_step_index`
- richer `expected_observation`
- 更稳的 `StepFailureKind / FailureAction`
- 更强的 `replan_scope`
- 更强的 `observation value / drift`

目标：

- 让系统从“有 plan 感”走向“有执行器语义”

### 方向 B：开始拆 planner / executor / watchdog

这是下一大步。

目标是形成三层：

- planner：生成计划与动作
- executor：执行当前 step
- watchdog：诊断失控与偏航

一旦做到这一步，系统就会从：

- “一个 loop 里什么都做”

进化成：

- **分层执行系统**

### 方向 C：让 Planning 变成更上位的范式

到更远的阶段，系统才会真正变成：

- Planning 管整体
- ReAct 管局部不确定探索
- runtime/watchdog 管控制与纠偏

也就是说：

- 现在是 `ReAct + Planning`
- 以后会走向：
- **Planning-led hybrid system**

---

## 十、一个适合写进技术博文的总结说法

如果以后你要把这个思路写成技术文章，我觉得可以用下面这段话来概括：

> `AMClaw` 当前的 Agent 执行模型已经不是纯 `ReAct`。它保留了 `ReAct` 在局部探索与在线反馈上的优势，同时逐步引入显式计划、执行状态、完成条件、失败分类与重规划范围，使得系统从“会调工具的循环”演进成一个由 `ReAct`、`Planning` 与 `runtime/watchdog` 混合构成的执行范式。短期内它仍然是 `Plan-aware ReAct`，长期则更可能走向由 Planner 统筹、Executor 执行、Watchdog 托底的混合系统。`

---

## 十一、一句话结论

当前 `AMClaw` 的设计范式不是：

- 纯 `ReAct`
- 纯 `Plan-and-Execute`

而是：

- **ReAct + Planning-aware execution + runtime/watchdog control**

并且它的下一步推进方向是：

- 从“带计划意识的 ReAct”，逐步演进到“由 Planner 统筹、由 Executor 执行、由 Watchdog 托底的混合执行系统”。

