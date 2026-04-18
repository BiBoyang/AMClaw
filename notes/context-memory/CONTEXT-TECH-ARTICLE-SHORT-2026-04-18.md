# 为什么 AMClaw 先做 SessionState，再做 ContextPack

> 短文版（发布稿草案）  
> 目标长度：800~1200 字

很多 Agent 项目在做 Context 时，会第一时间想到“更强召回”或“更复杂记忆”。  
AMClaw 这一轮的选择恰好相反：**先把结构收口，再谈能力扩展**。

## 我们遇到的真实问题

早期系统其实“能跑”：

- 会话文本可以进入 prompt
- 任务信息可以注入
- 用户记忆可以检索

但真正的痛点是：**难解释**。  
当回复不稳定时，我们很难回答：

1. 本轮 prompt 到底由哪些来源组成？
2. 哪些内容被裁剪？为什么？
3. 是状态问题、记忆问题，还是预算策略问题？

这意味着系统虽然可用，但不够可维护。

## 为什么先做 SessionState

没有显式状态时，系统长期依赖“从当前输入猜意图”。  
跨轮次场景里，`当前任务`、`阻塞原因`、`下一步` 都容易漂移。

所以 AMClaw 先补了最小持久化状态：

- `last_user_intent`
- `current_task`
- `next_step`
- `blocked_reason`
- `updated_at`

重点不在“字段多”，而在“状态有位置”。  
它让系统先能稳定回答“用户现在处于什么阶段”。

## 为什么接着做 ContextPack

即使有了 SessionState，如果上下文仍然散落拼接，解释成本还是高。  
所以第二步是引入 `ContextPack`，把组装入口统一为：

`build_context_pack -> render_prompt_from_context_pack`

同时把 pack 级可观测写进 trace：

- `context_pack_present`
- `context_pack_section_count`
- `context_pack_total_chars`
- `context_pack_drop_reasons`

并输出结构化日志事件：

- `context_pack_built`
- `context_pack_trimmed`

这一步的价值是：  
系统不只是“生成了 prompt”，而是“生成了可解释的 prompt”。

## 我们刻意不做什么

这一阶段没有上：

- embedding / 向量召回
- 复杂 memory taxonomy
- 全量异步化重构

原因很简单：如果基础口径还没收口，复杂机制只会放大噪声，让收益来源不可归因。

## 结果是什么

到目前为止，AMClaw 的 Context 系统从“最小可用”进入了“可持续迭代”阶段：

1. 状态可表达（SessionState）
2. 组装可统一（ContextPack）
3. 裁剪可追溯（trace/log）
4. 改动可回归（测试与评测脚本）

一句话总结：

> 先把系统变得可解释，再让它变得更强。

下一步我们不会立刻堆新能力，而是先做稳定化：  
参数治理、固定回归场景、Trace 日报对比，让每次优化都能被验证。
