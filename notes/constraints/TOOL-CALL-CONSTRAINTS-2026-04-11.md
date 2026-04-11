# TOOL-CALL-CONSTRAINTS-2026-04-11

更新于 2026-04-11。

这份文档记录 `AMClaw` 当前 `tool use / tool call` 的真实限制，方便后续继续扩展工具体系、skill 体系和执行策略时参考。

当前一句话结论：

- `AMClaw` 现在是一个 **强约束、低自由度、偏只读、单轮单工具、带可验证完成条件** 的最小工具系统。

---

## 一、工具集合是白名单，不是开放式工具市场

当前模型只能调用 `ToolRegistry` 里显式注册的工具。

当前已有工具大致分两类：

### 1. 文件工具

- `read`
- `write`
- `create`

### 2. 业务只读工具

- `get_task_status`
- `list_recent_tasks`
- `list_manual_tasks`
- `read_article_archive`

当前不支持：

- 任意 shell
- 任意 HTTP 请求
- 任意浏览器操作
- 任意数据库写状态
- 任意系统文件访问
- 任意外部 API 调用

结论：

- 当前 tool use 是白名单式能力，不是开放式能力。

---

## 二、每轮最多一个工具调用

当前 planner 每轮只能返回：

- 一个 `CallTool`
- 或一个 `Final`

当前不支持：

- 一轮多个工具调用
- 并行工具调用
- 批量 action
- tool call graph

当前模式是：

```text
Reason -> Call one tool -> Observe -> Next round
```

而不是：

```text
Reason -> Call many tools -> Aggregate -> Continue
```

结论：

- 当前 tool execution 是单轮单工具、串行推进。

---

## 三、文件工具有工作区路径边界

文件工具不是任意读写系统文件。

当前约束：

- 所有文件路径必须落在 `workspace_root` 内
- 禁止通过 `../..` 访问工作区外路径
- 禁止访问任意用户文件或系统文件

示例：

- 允许：读写项目工作区内文件
- 禁止：读 `/etc/hosts`
- 禁止：通过相对路径逃逸工作区

结论：

- 文件工具是受限能力，不是全盘文件系统访问。

---

## 四、业务工具当前基本只读

当前业务工具主要用于观察系统状态，而不是修改系统状态。

当前只读业务工具包括：

- `get_task_status`
- `list_recent_tasks`
- `list_manual_tasks`
- `read_article_archive`

当前没有开放给 agent 的业务写工具：

- 修改任务状态
- 重试任务
- 删除任务
- 改配置
- 主动发微信消息
- 触发抓取流程

结论：

- 当前业务工具重点是“观察”，不是“行动”。

---

## 五、工具调用参数是固定 schema

LLM 不能随便编一个工具名或参数就被执行。

当前 action schema 固定，例如：

- `read` 需要 `path`
- `write` 需要 `path` + `content`
- `create` 需要 `path` + `content`
- `get_task_status` 需要 `task_id`
- `list_recent_tasks` 可选 `limit`
- `list_manual_tasks` 可选 `limit`
- `read_article_archive` 需要 `task_id`

如果 action 不支持，或者必要字段缺失：

- 解析失败
- 不进入工具执行层

结论：

- 模型只能在固定 action schema 内活动。

---

## 六、工具成功不等于 step 完成

当前系统已经加入 `expected_observation` / `done_rule`。

这意味着：

```text
工具调用成功 != step 一定完成
```

当前流程是：

```text
Action -> Tool Success -> Observation -> Done Validation -> Step Done / Step Failed
```

当前支持的 `DoneRule`：

- `tool_success`
- `non_empty_output`
- `required_json_field`

示例：

- `read` 成功但读到空文本，可能仍然失败
- `get_task_status` 成功但缺少 `found` 字段，仍然失败
- `read_article_archive` 成功但缺少 `content` 字段，仍然失败

结论：

- 当前 tool use 已经开始进入“可验证完成条件”阶段。

---

## 七、失败处理仍是最小版

当前已有最小 failure diagnosis：

### 当前已有 `StepFailureKind`

- `expectation`
- `repeated_action`
- `semantic`
- `irrecoverable`

### 当前已有 `FailureAction`

- `replan`
- `abort`

当前还没有：

- `retry_step`
- `fallback_tool`
- `ask_user`
- failure budget
- retry budget
- rollback / compensation

结论：

- 当前失败处理已经不只是纯 fail-fast，但仍然很基础。

---

## 八、当前没有完整 retry 机制

现在没有正式的：

- transient failure 分类
- step 级 retry 次数
- retry backoff
- retry budget

所以当前还不能说具备完整失败恢复能力。

结论：

- 当前具备最小 replan / abort 分流，但尚未具备正式 retry system。

---

## 九、当前没有统一 rollback / compensation

当前没有跨步骤统一回滚机制。

例如：

- `write` 成功后，后续 step 失败，文件不会自动恢复
- `create` 成功后，后续 step 失败，文件不会自动删除

底层某些模块内部可能有 SQLite transaction，
但那只是单个函数内部原子性，不是 agent run 级 rollback。

结论：

- 当前没有 agent-level rollback。

---

## 十、当前没有 skill-level 工具权限

虽然后续有 skill 化路线，但当前还没有正式做到：

- 某个 skill 只能调用某些工具
- 某个 skill 禁止调用某类工具
- 某个 skill 需要额外授权

当前工具权限主要来自：

- 全局工具白名单
- 文件路径边界
- 业务工具只读策略

结论：

- 当前还不是 skill-scoped tool permission system。

---

## 十一、当前没有工具预算 / 成本预算

当前还没有正式：

- 每类工具调用次数上限
- 单次 run 工具调用预算
- 高成本工具配额
- 工具冷却期

当前限制更多来自执行模型：

- 单轮单工具
- 最大 step 数
- 白名单能力边界

结论：

- tool budget 还没形成独立机制。

---

## 十二、当前约束分层总结

当前 `AMClaw` 的 tool use 限制可以分成五层：

### 1. 能力白名单

只能调用系统注册过的工具。

### 2. 执行模型限制

每轮最多一个工具，串行执行。

### 3. 安全边界限制

文件工具限制在工作区内，业务工具当前偏只读。

### 4. 完成条件限制

工具成功后，还必须满足 `expected_observation` / `done_rule`。

### 5. 失败处理限制

当前只有最小 `replan / abort`，没有完整 retry / rollback。

---

## 十三、后续扩展建议

如果后续继续扩展 tool use，应优先考虑：

1. 补正式 `retry_step`
2. 补 `transient` failure 分类
3. 补工具风险等级
4. 补 skill-level tool permission
5. 补 tool budget
6. 补 rollback / compensation 策略
7. 补 observation value / novelty check

---

## 十四、一句话总结

当前 `AMClaw` 的 tool use / tool call 不是开放式 agent 工具系统，而是：

- **受限白名单**
- **单轮单工具**
- **路径受控**
- **业务偏只读**
- **带最小完成校验**
- **带最小失败诊断**

这是一个偏安全、偏保守、便于继续扩展的最小工具执行框架。
