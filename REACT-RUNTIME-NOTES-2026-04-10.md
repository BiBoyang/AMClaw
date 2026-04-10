# REACT-RUNTIME-NOTES-2026-04-10

更新于 2026-04-10。

这份记录用于保存本轮 `agent_core` 最小 ReAct 改造的实际落地结果，并明确说明本次技术选型的原因，方便后续继续沿着同一条 runtime 路线迭代。

## 一、本轮完成了什么

本轮目标不是做“大而全 planner”，而是把 `AMClaw` 当前的 Agent runtime 从：

- 单次规划
- 单次工具调用
- observation 默认直接结束

推进到：

- 最小多步回合
- observation 可回灌到下一轮规划
- planner prompt 不再只看到裸 `user_input`
- trace 能看清上下文、prompt 与 observation

当前已经落地的点包括：

1. `RunContext` 扩展
   - 增加了 `task_id`
   - 增加了 `article_id`
   - 增加了 `session_text`
   - 增加了 `context_token_present`
2. `AgentObservation` 骨架
   - 工具结果不再只是临时字符串
   - 先转成 runtime 可复用的 observation
3. `ContextAssembler`
   - planner 不再只拿到裸输入
   - 现在会看到来源、触发方式、用户、消息、session text 和最新 observation
   - 现在也能看到当前任务摘要与最近任务摘要
4. 最小 ReAct 提示词
   - 明确告诉模型这是“每轮最多调用一个工具”的反应式回合
5. 多步 loop 骨架
   - 已支持测试环境下的 `create -> read -> final`
   - 证明 runtime 已可承载多步决策

## 二、本轮技术选型与原因

### 1. 先做“最小 ReAct”，不做“大 Planning”

本轮选择的是：

- 先做 **最小 ReAct runtime**
- 不先做复杂任务分解系统
- 不先做 plan tree
- 不先做 self-reflection

原因：

1. 当前仓库的工具集合还很少，主要还是文件工具，过早引入复杂 planner 会显著增加代码复杂度，但短期收益不大。
2. 当前更大的瓶颈不是“不会做大计划”，而是“工具结果还不能自然进入下一轮决策”。
3. 最小 ReAct 可以直接增强现有 loop，而不需要重写整个 `agent_core`。

结论：

- 当前阶段优先解决“回合能不能持续”，而不是“计划能不能很宏大”。

### 2. 先保留单工具一步一决策，不做并行工具调用

本轮选择的是：

- 每轮 planner 最多只决定一个工具调用
- 不做多工具并发
- 不做工具批量计划

原因：

1. 现在的 runtime 还在形成基础闭环，过早引入并行工具会让 trace、错误处理和状态收口复杂度明显上升。
2. `AMClaw` 当前最需要的是稳定可复盘，而不是最大吞吐。
3. 单工具一步一决策更符合当前最小 ReAct 的目标，也更容易验证 observation 的价值。

结论：

- 先把“一步一工具”的循环做稳，再考虑更复杂的调用模型。

### 3. 先把上下文组装放在 `ContextAssembler`，不让 planner 直接读取底层状态

本轮选择的是：

- planner 不直接访问 `task_store`、`pipeline` 或其他底层实现
- 统一通过 `ContextAssembler` 组装上下文

原因：

1. 这样能把“上下文读取”与“上下文使用”分离，便于后续替换或扩展。
2. 后面如果引入 skill，这层接口天然可以复用。
3. 这能避免 prompt 逐步绑定底层实现细节，降低未来重构成本。

结论：

- `ContextAssembler` 是 runtime 层抽象，不是 skill 抽象，但它会成为未来 skill 的基础设施。

### 4. 先在聊天入口透传最小事实字段，不急着上 memory

本轮选择的是：

- 先透传 `source_type`、`trigger_type`、`user_id`、`message_ids`
- 再补 `session_text`
- 再补 `context_token_present`
- 暂不引入长期 memory 检索

原因：

1. 当前先解决“这次运行的最小事实上下文”更重要。
2. memory 系统的设计边界远比最小 ReAct 大，过早接入会让本轮目标发散。
3. 真实聊天上下文已经能带来明显收益，不需要等 memory 完成后再开始。

结论：

- 当前是 `RunContext v1`，不是 memory 系统。

### 5. 先保留保守 fallback，不强推 planner 接管所有后续步骤

本轮选择的是：

- 第二轮之后已经允许继续走 planner
- 但如果 planner 不可用或失败，仍保留 observation -> final 的保守收口

原因：

1. 当前生产环境仍以稳定性优先，不能为了“像 ReAct”而把现有链路一下改炸。
2. 这能在不损失现有可用性的前提下，把 runtime 向多步方向推进。
3. 这也为下一轮“接业务只读工具”留下了渐进空间。

结论：

- 当前是“最小 ReAct + 保守降级”，不是完全放开的自主 agent。

## 三、为什么这轮不先做 skills

本轮没有先做 skill，原因不是 skill 不重要，而是：

1. 如果 runtime 还不够清晰，skill 只会把问题往更高层包装，不能真正解决问题。
2. ReAct、上下文和 tool use 的边界要先在 runtime 层稳定下来，skill 才有明确的承载面。
3. 当前最值得先验证的是：Agent runtime 能不能自然支持多步，而不是“skill manifest 长什么样”。

结论：

- 本轮先让 runtime 变真，再让 skill 变薄。

## 四、本轮后的系统状态

当前可以这样理解 `AMClaw` 的 Agent runtime：

- 还不是完整通用 planner
- 但已经不再只是“单次工具 demo”
- 已经具备：
  - 最小多步 loop 骨架
  - observation 回灌
  - 上下文组装
  - 更完整的 prompt trace

所以当前状态可描述为：

- **最小可扩展 ReAct runtime**

## 五、下一步建议

本轮完成后，下一步最自然的方向是：

1. 接入第一批只读业务工具
   - `get_task_status`
   - `list_recent_tasks`
   - `list_manual_tasks`
   - `read_article_archive`
2. 继续观察这些工具是否还需要更细的上下文裁剪
3. 再决定是否把第二轮后的 planner 优先级进一步提高

当前暂不建议立即做：

- 复杂 skill 系统
- memory 检索系统
- 并行工具调用
- 多代理编排

一句话总结：

- **先把 runtime 做真**
- **再把业务工具接进来**
- **然后才轮到更像产品层的 skill**
