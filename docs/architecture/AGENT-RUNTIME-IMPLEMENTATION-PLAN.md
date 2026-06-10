# AGENT-RUNTIME-IMPLEMENTATION-PLAN

更新于 2026-04-10。

这份文档是 `CORE-SKILL-ROADMAP.md` 的执行层补充，用来回答：

1. 如果下一步先做 `Agent Loop`、上下文和 `Tool Use`，具体该怎么动手
2. 哪些文件应该先改，哪些文件暂时不要碰
3. 每一阶段的完成标准是什么

当前原则保持不变：

- 先把 runtime / kernel 做扎实
- skill 抽象后置
- 不为了“看起来像 agent”而破坏现有确定性主链路

当前进度补充：

- 阶段 0、1、2 已完成
- 阶段 3 的最小 ReAct loop 已完成第一版
- 阶段 4 已完成第一批只读业务工具：
  - `get_task_status`
  - `list_recent_tasks`
  - `list_manual_tasks`
  - `read_article_archive`
- `Context v2` 已完成最小版：
  - planner 可看到当前任务摘要
  - planner 可看到最近任务摘要
- 本轮实现记录与技术选型原因见 `notes/runtime-evolution/AGENT-RUNTIME-01-REACT-FOUNDATION-2026-04-10.md`

## 一、当前起点

当前仓库里的 `agent_core` 已经具备最小原型，但还属于单回合能力：

- 有最小 loop
- 有 LLM / rule 双规划入口
- 有文件工具调用
- 有 trace 落盘

当前主要限制是：

1. 实际只支持“一次规划 + 一次工具 + 直接结束”
2. 上下文主要用于 trace，而不是用于推理
3. 工具只有文件工具，缺少语义更清晰的业务工具层
4. `Planning / ReAct` 还没有形成真正多步闭环

## 二、当前明确不做

这一轮实现计划里，先不做：

- skill marketplace
- manifest / DSL
- 热加载
- 全量 memory 系统
- `Multi-Agent`
- 把 `状态` / `重试` / `补正文` 改成 skill
- 改写 `chat_adapter` / `task_store` / `pipeline` 的主职责

## 三、阶段化实施顺序

### 阶段 0：清理 `agent_core` 的职责边界

目标：

- 让 `agent_core` 不再继续堆业务分支
- 先把“运行时骨架”概念明确下来

建议动作：

1. 在 `src/agent_core/` 内部明确拆出以下结构：
   - `RunContext`
   - `AgentObservation`
   - `AgentDecision`
   - `PlanningPolicy`
2. 保留现有 trace 结构，但不要再把更多业务字段直接塞进 `run(...)` 参数
3. 把当前“首轮规划”和“拿到工具结果后直接 final”的逻辑改成更清晰的阶段状态

完成标准：

- `agent_core` 文件内的核心概念命名稳定
- 现有 trace 测试不回退
- 行为仍与当前仓库兼容

### 阶段 1：补齐 `RunContext`

目标：

- 让 runtime 拿到“这次运行的最小事实上下文”
- 但不直接引入复杂 memory 或 skill 系统

建议字段：

- `source_type`
- `trigger_type`
- `user_id`
- `message_ids`
- `task_id`
- `article_id`
- `session_text`
- `context_token_present`

说明：

- 不要求这些字段一次全部接通
- 先把结构设计好，再按真实入口逐步接线

优先改动文件：

- `src/agent_core/mod.rs`
- `src/chat_adapter/mod.rs`

完成标准：

- `run_with_context(...)` 的上下文对象不再只服务于 trace
- 聊天入口可以稳定透传最小运行上下文
- 新增对应单元测试

### 阶段 2：加入 `ContextAssembler`

目标：

- 让 runtime 可以按规则组装 prompt 上下文
- 把“上下文读取”和“上下文使用”分开

建议最小接口：

1. 输入：
   - `RunContext`
   - 当前用户输入
   - 可选观察结果 `observation`
2. 输出：
   - 提供给 planner 的结构化上下文文本
   - 或结构化 JSON 片段

第一版只建议接这几类上下文：

- 当前用户输入
- 上游消息 ID 列表
- 来源类型
- 触发方式
- 用户 ID
- 最近一次工具 observation

这一阶段先不要做：

- 长期 memory 检索
- 历史消息大规模召回
- 动态 ranking / retrieval

优先改动文件：

- `src/agent_core/mod.rs`
- 如有必要，再新增 `src/agent_core/context.rs`

完成标准：

- LLM planner 不再只收到裸 `user_input`
- observation 可以回灌到下一轮规划
- trace 里可看出上下文组装后的结果摘要

### 阶段 3：把单次工具调用升级成多步 loop

目标：

- 从“单工具回合”升级到“Plan -> Act -> Observe -> Repeat -> Final”

建议动作：

1. 保留 `max_steps`
2. 每一步允许 planner 返回：
   - `CallTool`
   - `Final`
3. 工具成功后，把结果存成 `AgentObservation`
4. 下一轮规划时，把 observation 加入 planner 上下文
5. 避免默认“有工具结果就直接结束”

关键约束：

- 第一版仍然只允许一次一个工具调用
- 不做并行工具调用
- 不做复杂计划树

优先改动文件：

- `src/agent_core/mod.rs`

完成标准：

- 至少支持：
  - `read -> final`
  - `create -> read -> final`
  - `query-like tool -> final`
- `max_steps` 超限时能稳定失败并记录 trace

当前状态：

- 已完成第一版
- 当前已支持最小多步 loop 骨架与 scripted 多步测试
- 当前仍保留“planner 不可用时 observation -> final”的保守收口

### 阶段 4：引入“语义更清晰”的业务工具

目标：

- 让模型调用的不是底层存储细节，而是清晰的业务动作

第一批候选工具：

- `get_task_status`
- `list_recent_tasks`
- `list_manual_tasks`
- `read_article_archive`
- `fetch_url`

说明：

- 这些工具不要求一开始全部对 LLM 开放
- 先挑只读、低风险、无破坏性的工具

这一阶段应避免：

- 直接开放“写任务状态”的工具
- 让模型自由更新数据库记录

优先改动文件：

- `src/tool_registry/mod.rs`
- 可能新增更细的工具定义文件
- 视情况接 `src/task_store/`

完成标准：

- 新工具有清晰输入输出
- 工具边界有单元测试
- 不破坏现有 deterministic 命令链路

当前状态：

- 第一批只读业务工具已接入
- 当前业务工具仍保持只读，不开放任务状态写入
- 下一步可继续评估是否需要 `read_article_archive`

### 阶段 5：最小 `Planning / ReAct` 闭环

目标：

- 明确 runtime 的推理工作流，而不是继续堆条件分支

建议策略：

1. planner 接收：
   - system prompt
   - 用户输入
   - 上下文摘要
   - 最近 observation
   - 可用工具列表
2. planner 输出：
   - `tool_call`
   - `final`
3. runtime 负责执行并回填

第一版不追求：

- 通用 planning tree
- self-reflection
- 多代理协作

完成标准：

- 代码里能清楚区分：
  - planning
  - tool execution
  - observation
  - finalization
- 测试覆盖至少一个两步以上回合

### 阶段 6：再接最小 skill 抽象

只有当前面几步形成最小闭环后，才开始：

1. 定义 `Skill`
2. 让 skill 声明：
   - `required_context`
   - `allowed_tools`
   - `system_prompt`
3. 第一批先接低风险 skill：
   - `chat.reply`
   - `article.summarize`

完成标准：

- skill 只是策略层，不接管状态机
- 不影响现有确定性命令与任务链路

## 四、建议的文件改动顺序

### 第一批优先改

1. `src/agent_core/mod.rs`
2. `src/chat_adapter/mod.rs`
3. `src/tool_registry/mod.rs`

### 第二批按需要再改

1. `src/task_store/mod.rs`
2. `README.md`
3. `PLAN.md`
4. `NEXT-STEPS.md`

### 当前尽量不碰

1. `src/pipeline/` 主流程
2. `src/session_router.rs` 语义
3. 微信协议细节

## 五、每一轮开发都应验证什么

每一阶段至少保证：

1. `cargo check`
2. `cargo test`
3. Agent trace 不回退
4. 微信聊天主链路不被 runtime 重构破坏
5. 任务状态命令仍然走确定性路径

如果本轮改动涉及：

- `chat_adapter`
- `context_token`
- 主消息接线

则还应按仓库约定执行一次人工回归。

## 六、第一轮最小开工建议

如果下一轮真的开始写代码，建议只做下面三件事：

1. 给 `agent_core` 引入明确的 `AgentObservation`
2. 让 planner 在第二轮也能看到 observation
3. 把“有工具结果就直接 final”改成“交回 planner 再判断”

为什么先做这三件事：

- 改动集中
- 风险可控
- 最能验证 runtime 是否正在从“单回合原型”走向“可扩展内核”
- 还不会过早引入 skill 抽象负担

一句话总结：

- **先把 runtime 做真**
- **再把 skill 接上去**
