# LOGGING.md

# AMClaw Logging Conventions

这份文档记录 `AMClaw` 当前已经落地的系统级结构化日志约定，用来回答三件事：

1. 现在日志大概长什么样
2. 事件名和字段该怎么收
3. 后续继续补日志时，应该遵守什么边界

当前文档描述的是 **第一版最小结构化日志约定**，重点是：

- 先让消息链路、任务链路、Agent run 能按字段追踪
- 先让关键失败事件有稳定命名
- 先避免不同模块各写各的自由格式

不追求一开始就做成完整 tracing 体系。

## 1. 当前覆盖范围

目前第一版结构化日志已经覆盖：

- `src/chat_adapter`
- `src/pipeline`
- `src/task_store`

尚未系统化收口的输出仍然可能存在于：

- 个别调试输出
- 其他暂未覆盖的旧打印点

因此当前状态可以理解为：

- **主链路已基本结构化**
- **全仓库尚未完全统一**

## 2. 日志形态

当前三处模块都采用统一思路：

- 每条日志输出一个 JSON object
- 至少包含：
  - `ts`
  - `level`
  - `event`
- 其余上下文字段按事件补充

示例：

```json
{
  "ts": "2026-04-06T14:32:10+08:00",
  "level": "info",
  "event": "message_received",
  "user_id": "user-a",
  "message_id": "msg-1",
  "text_chars": 24
}
```

## 3. 第一版字段约定

### 3.1 必备字段

以下字段每条结构化日志都应存在：

- `ts`
- `level`
- `event`

含义：

- `ts`：Asia/Shanghai 时区的 RFC3339 时间戳
- `level`：`info` / `warn` / `error`
- `event`：稳定事件名

### 3.2 常见关联字段

以下字段不是每条都必须有，但凡上下文里有，优先带：

- `user_id`
- `message_id`
- `message_ids`
- `message_count`
- `task_id`
- `article_id`
- `run_id`
- `source`
- `trigger`
- `status`
- `error_kind`

### 3.3 detail / preview 类字段

允许使用：

- `detail`
- `text_preview`
- `reply_preview`

但这类字段只作为补充说明，不应替代结构字段本身。

例如：

- `task_id` 不能只写进 `detail`
- `message_id` 不能只写进 `detail`
- `error_kind` 不要退化成自然语言句子

## 4. 命名原则

### 4.1 event 命名

事件名统一使用：

- 小写
- 下划线分隔
- 动作语义稳定

例如：

- `message_received`
- `session_flushed`
- `task_status_changed`
- `browser_worker_failed`

避免：

- 同一类事件多个别名
- 同时出现中英文混用
- 用自然语言整句当事件名

### 4.2 error_kind 命名

错误类型也统一使用：

- 小写
- 下划线分隔
- 尽量稳定

例如：

- `wechat_send_failed`
- `browser_worker_timeout`
- `http_request_failed`
- `task_failed`

如果只是人类补充说明，写到 `detail`，不要把 `error_kind` 写成一整句。

## 5. 当前已落地事件

以下列表描述的是 **截至当前代码状态已经落地的事件名**。

### 5.1 `src/chat_adapter`

- `bot_starting`
- `bot_polling_started`
- `login_qrcode_requested`
- `login_qrcode_ready`
- `login_qrcode_missing_url`
- `login_waiting_for_scan`
- `login_confirmed`
- `login_qrcode_expired`
- `login_status_unknown`
- `login_aborted`
- `poll_failed`
- `poll_retry_scheduled`
- `poll_updates_received`
- `message_received`
- `message_accepted`
- `message_deduplicated`
- `message_dedup_store_failed`
- `message_parse_skipped`
- `session_flushed`
- `agent_reply_started`
- `agent_reply_finished`
- `agent_reply_failed`
- `agent_reply_fallback`
- `reply_sent`
- `reply_send_failed`
- `reply_skipped_no_context_token`
- `pending_tasks_query_failed`
- `pending_task_process_failed`
- `pending_task_archived`
- `pending_task_awaiting_manual_input`
- `pending_task_failed`

### 5.2 `src/agent_core`

- `agent_trace_persist_failed`
- `agent_planner_selected`
- `agent_planner_fallback`
- `agent_llm_config_enabled`
- `agent_llm_multi_provider_enabled`
- `agent_llm_fallback_success`
- `agent_llm_auth_failed`
- `agent_llm_retry`

### 5.3 `src/pipeline`

- `task_processing_started`
- `task_fetch_branch_selected`
- `http_fetch_started`
- `http_fetch_finished`
- `http_fetch_failed`
- `browser_worker_started`
- `browser_worker_finished`
- `browser_worker_failed`
- `task_archived`
- `task_awaiting_manual_input`
- `task_failed`

### 5.4 `src/task_store`

- `inbound_message_recorded`
- `inbound_message_deduplicated`
- `task_created`
- `task_retry_requested`
- `task_status_changed`

### 5.5 `src/main.rs` / `src/config.rs`

- `startup_env_loaded`
- `agent_demo_finished`
- `signal_received`
- `startup_failed`
- `config_default_created`

## 6. 模块边界提醒

结构化日志不改变模块职责。

### `src/chat_adapter`

适合记录：

- 消息进入
- 去重结果
- 会话 flush
- 回复发送结果

不应在这里承担：

- 任务抓取内部细节
- 持久化状态机逻辑

### `src/pipeline`

适合记录：

- 任务开始处理
- 分支选择
- HTTP / 浏览器抓取
- 归档成功 / 失败
- 转人工补录

不应在这里承担：

- 微信协议层日志
- 聊天命令解析日志

### `src/task_store`

适合记录：

- 消息落库
- 任务创建
- retry
- 状态变化

不应在这里承担：

- 网络抓取
- 微信发送
- 上层业务编排

## 7. 后续继续补日志时的规则

### 7.1 优先复用已有事件名

如果已有事件名已经能表达当前动作，就不要新起一个相近名字。

例如：

- 状态变化优先继续用 `task_status_changed`
- 失败优先在 `error_kind` 上扩展

### 7.2 先补结构字段，再补说明文字

优先补：

- `task_id`
- `message_id`
- `user_id`
- `status`
- `error_kind`

最后才补 `detail`。

### 7.3 不要为了日志顺手重构业务

日志改动尽量和业务改动解耦。  
如果本次目标只是“收口日志”，就尽量不要顺手调整流程或重命名无关代码。

### 7.4 允许模块内 helper 暂时重复

当前 `chat_adapter`、`pipeline`、`task_store` 都各自维护一份很薄的 helper。  
第一阶段允许这种重复，优先保证：

- 事件名稳定
- 字段稳定
- 主链路可追踪

等约定稳定后，再考虑是否提炼成共享 logger。

## 8. 当前测试约定

当前已经补了最小日志契约测试，目标不是抓终端输出，而是保证 payload 结构不漂。

覆盖点：

- `src/chat_adapter/mod.rs`
- `src/pipeline/mod.rs`
- `src/task_store/mod.rs`

这些测试至少保证：

- `ts` / `level` / `event` 存在
- 关键字段能保留
- `null` 字段不会被错误输出

后续继续补日志时，如果新增新的 payload builder 或明显改变字段结构，建议同步补一个对应测试。

## 9. 当前最重要的判断

现在这套日志约定的目标，不是“日志更漂亮”，而是：

- 让消息链路可追踪
- 让任务链路可追踪
- 让 Agent Trace 能和系统日志对齐
- 让失败更容易归因

一句话总结：

**先把事件名和字段收住，比先把 logger 抽象做漂亮更重要。**
