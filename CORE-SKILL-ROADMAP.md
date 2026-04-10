# CORE-SKILL-ROADMAP

更新于 2026-04-10。

这份文档不描述“当前已经完成什么”，而是描述 `AMClaw` 接下来在 Agent 能力建设上更合适的分层方式，避免继续把运行时底座、业务能力和提示词策略混在一起。

如果已经确认先做 runtime / context / tool use，再看更细的落地顺序，请继续读 `AGENT-RUNTIME-IMPLEMENTATION-PLAN.md`。

当前结论很明确：

- `AMClaw` 适合走 **薄 Kernel + 稳定 Services/Tools + 少量可替换 Skills** 的路线
- 不适合把任务状态、抓取主链路、权限边界这类确定性能力直接做成 skill
- 先把 Agent runtime 做扎实，再逐步引入最小 skill 抽象

## 一、为什么要单独写这份文档

当前仓库里已经有：

- `README.md`：描述“现在能跑什么”
- `PLAN.md`：描述“当前真实状态”
- `NEXT-STEPS.md`：描述“下一轮先做什么”

但我们最近讨论的内容，已经不只是“下一步做哪个功能”，而是：

1. `Agent` 内核到底保留到什么程度
2. 哪些能力应该留在 runtime / service 层
3. 哪些能力才值得抽成 skill

这类问题属于 **架构路线**，不适合继续混进 `PLAN.md` 的“现状描述”里，所以单独拆一份文档。

## 二、建议的四层分工

### 1. Kernel / Runtime

负责 Agent 运行时本身：

- `Agent Loop`
- 上下文组装与裁剪
- `Tool Use / Function Calling`
- `Planning / ReAct` 执行框架
- `Memory` 读写框架
- 状态钩子与运行时回执
- 错误恢复框架
- 安全与权限校验
- 多 Agent 编排基础

这层回答的是：

- “系统有没有这个能力”
- “能力的执行边界在哪里”
- “失败时怎么收口”

### 2. Services / Tools

负责稳定、确定、可测试的执行能力：

- `task_store`
- `pipeline`
- `browser_capture`
- `archive_writer`
- `wechat_send`
- `query_task`
- `fetch_url`
- 文件工具与路径边界

这层回答的是：

- “具体动作怎么做”
- “副作用怎么落地”
- “哪些状态能被写入”

### 3. Skills

负责面向任务目标的策略与工作流约束：

- 适用于什么场景
- 需要哪些上下文片段
- 允许调用哪些 tools / services
- 使用什么 prompt / workflow
- 输出格式长什么样
- 工具失败时怎样 fallback

这层回答的是：

- “这次任务该怎么用 runtime 能力”
- “这类任务应该怎样组织上下文与工具”

### 4. Product Flows

负责最终产品级链路：

- 微信聊天
- 链接归档
- 内容摘要
- 日报生成
- 人工补录协作

这层回答的是：

- “用户最终看见的行为是什么”

## 三、哪些不应该做成 skill

以下能力默认保留在 runtime / service 层，不做成 skill：

### 1. 运行时底座

- `Agent Loop`
- `Tool Use / Function Calling`
- `Planning / ReAct` runtime
- `Memory` 基础设施
- 状态管理
- 错误恢复框架
- 安全与权限边界
- `Multi-Agent` runtime

原因：

- 这些能力属于“Agent 系统是否成立”的地基
- 必须稳定、可测试、可观察、可恢复
- 不适合交给 prompt 或 skill 自由发挥

### 2. 强确定性的业务主链路

- 微信接入与轮询
- 消息去重
- 命令分流
- `task_store` 中的状态写入
- `pipeline` 的抓取、归档和失败落盘
- 浏览器 worker 与路径校验

原因：

- 这些能力副作用明确
- 行为需要强约束
- 出错时不能让 skill 决定系统状态

## 四、哪些适合逐步做成 skill

以下能力更适合做成 skill：

- 普通聊天回复
- 网页内容摘要
- 网页内容分类
- 任务失败解释
- 日报 / digest 生成
- 面向不同内容来源的策略选择

原因：

- 这些能力更像“策略”和“生成”
- 提示词、流程和输出格式会持续迭代
- 允许因场景不同而采用不同工作流

## 五、把原来的十步计划重新归类

原始计划：

1. `Agent Loop`
2. 上下文工程
3. `Tool Use / Function Calling`
4. `Planning` 与 `ReAct`
5. `Memory` 设计
6. 状态管理
7. 错误恢复与反馈闭环
8. `Agent` 评测
9. 安全与权限边界
10. `Multi-Agent` 设计

新的解释应该是：

### 1. Core / Runtime 路线

1. `Agent Loop`
2. 上下文引擎
3. `Tool Use / Function Calling`
4. `Planning / ReAct` runtime
5. `Memory` engine
6. 状态管理
7. 错误恢复框架
8. 评测框架
9. 安全与权限边界
10. `Multi-Agent` runtime

### 2. Skill 路线

等上面几项至少有最小闭环后，再做：

1. 定义最小 `Skill` 抽象
2. 让 skill 声明需要哪些上下文
3. 让 skill 声明允许哪些 tools / services
4. 让 skill 定义 prompt / output contract
5. 先落地第一批 skill：
   - `chat.reply`
   - `article.summarize`
   - `article.classify`

## 六、当前最合适的落地顺序

### 阶段 A：先把 runtime 做像真的 Agent

优先实现：

1. 更真实的多步 `Agent Loop`
2. `RunContext` 扩展
3. `ContextAssembler`
4. 多步 `Tool Use / Observation / Final`
5. 最小 `Planning / ReAct` 闭环

这一阶段的目标不是“skill 化”，而是先让 Agent 内核具备足够清晰的运行机制。

### 阶段 B：再引入最小 skill 抽象

只做最小需要的字段，例如：

- `name`
- `description`
- `required_context`
- `allowed_tools`
- `system_prompt`

不急着做：

- 复杂 manifest
- marketplace
- 热加载
- 大规模配置化

### 阶段 C：只接少量低风险 skills

第一批优先：

1. `chat.reply`
2. `article.summarize`
3. `task.explain`

暂不 skill 化：

- `状态 <task_id>`
- `重试 <task_id>`
- `待补录任务`
- `补正文 <task_id> :: <content>`
- 抓取主流程状态迁移

## 七、对当前仓库模块的映射建议

### 应继续保留为 core / service 的模块

- `src/chat_adapter/`
- `src/command_router/`
- `src/session_router.rs`
- `src/task_store/`
- `src/pipeline/`
- `src/tool_registry/`
- `src/config.rs`
- `src/logging.rs`

### 应逐步收敛为 runtime kernel 的模块

- `src/agent_core/`

目标不是继续堆业务分支，而是变成：

- `RunContext`
- `ContextAssembler`
- `Tool Loop`
- `Planning Runtime`
- 最小 `Skill` 入口

### 后续可以新增的模块

- `src/skill_registry/`
- `src/skills/`

## 八、当前明确不做的事

在 runtime 基础能力还没形成闭环前，暂不做：

- 大而全的 skill 系统
- 把所有命令改成 skill
- 让 skill 直接访问底层存储实现
- 先上复杂 manifest / DSL
- 先做通用 marketplace 式扩展体系

## 九、这份文档的使用方式

后续判断一个能力应该放哪层时，优先用下面三句话判断：

1. 如果不用 LLM，这个能力也必须稳定存在吗？
   - 是：更像 core / service
2. 如果换一个 prompt 或策略，这个能力的行为应该允许变化吗？
   - 是：更像 skill
3. 如果这块出错，会不会直接把系统状态写坏？
   - 会：不要优先做成 skill

一句话结论：

- **状态和执行留在系统里**
- **策略和生成放进 skill 里**
