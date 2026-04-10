# NEXT-STEPS

当前这份文件只记录“接下来最值得做什么”，不重复描述已经完成的能力。当前真实状态请看 `PLAN.md`。

## 当前优先级

关于为什么当前阶段先做 runtime / context / tool use，而不是先全面做 skill 化，请先看 `CORE-SKILL-ROADMAP.md`。

Agent Trace 当前已具备：

- 真实聊天链路上下文透传
- `source_type`
- `trigger_type`
- `user_id`
- `message_ids`
- 每日 `index.jsonl`
- 每日可读总览 `index.md`

下一步优先往下推进：

### 1. Agent runtime 基础能力

当前更适合优先推进：

- 更真实的多步 `Agent Loop`
- `RunContext` 扩展
- 上下文组装与裁剪
- `Tool Use / Function Calling`
- 最小 `Planning / ReAct` 闭环

当前结论是：

- 这些能力属于 runtime / kernel，不属于 skill
- skill 抽象应该后置，在 runtime 有了最小闭环后再接
- 第一批 skill 只考虑低风险的策略型能力，例如 `chat.reply`、`article.summarize`
- 当前已补最小会话恢复，下一步更值得继续推进持久记忆而不是重复做会话缓存
- Memory v1/v2 已补显式记忆与最小自动提炼，下一步可以评估长期主题记忆与语义检索

### 2. 系统级日志与错误结构

- `chat_adapter`、`pipeline`、`task_store` 已有第一版结构化日志
- `chat_adapter` 的登录、轮询和任务消费旧式输出已基本收口
- `main`、`config`、`agent_core` 也已补第一版结构化事件
- 下一步继续处理仓库里剩余零散调试输出，或进一步统一 logger 抽象
- 继续统一 `error_kind` 和事件命名，减少漂移
- 让登录、任务消费、浏览器抓取、人工补录的错误更容易检索与归因

### 3. 内容处理继续深化

- 提升通用网页正文抽取质量
- 在公众号之外逐步补分类与摘要链路
- 让 pipeline 状态比现在更明确
- 对“普通短链最终跳到公众号”的场景，当前先保持观测，不立即自动升级到 browser capture
- 等积累更多真实样本后，再决定是否引入二阶段抓取策略

### 4. 汇总与调度

- 本地 Markdown 日报与最小定时触发已补齐
- 最小微信摘要回传已补齐（依赖 `report_to_user_id` 与已持久化 `context_token`）
- 再考虑汇总失败补偿与更多周期任务

## 当前明确暂不做

- `tokio` 全量迁移
- `sqlx` async 化
- 模式策略的大改版
- 多聊天应用接入层重构

## 下一轮建议顺序

1. 先做 `Agent Loop`、上下文与 `Tool Use`
2. 再继续统一系统级结构化日志
3. 再补通用网页抽取、分类、摘要
4. 最后再接最小 skill 抽象与少量 skills

## 开工标准

下一轮继续开发前，至少保证：

- 现状文档与仓库行为一致
- 真实 Bot 登录与消息链路仍可用
- 公众号浏览器抓取链路不回退
- Agent Trace 当前 JSON / Markdown / `index.jsonl` 生成逻辑不回退
- `retry`、`状态`、`最近任务`、`待补录任务` 这些命令的行为与文档一致
- skill 化不应破坏确定性命令与任务状态链路
