# LARGE-FILE-SPLIT-PLAN-2026-05-04

> 目标：把 `agent_core/mod.rs`、`task_store/mod.rs`、`chat_adapter/mod.rs` 从“单文件巨石”拆到可维护结构，同时保证行为不变。
> 原则：先机械迁移再逻辑优化；每批次可回滚；每批次有独立校验。
> 风险输入文档：`LARGE-FILE-SPLIT-RISK-REVIEW-2026-05-04.md`

## 0. 基线与约束

### 当前基线

- `src/agent_core/mod.rs`: 8755 行
- `src/task_store/mod.rs`: 5002 行
- `src/chat_adapter/mod.rs`: 3500 行
- `cargo test`: `371 passed`（`src/lib.rs` 单元测试）
- `cargo test -- --list`: 411 test entries（含多个 test target）

### 拆分约束

1. 第一阶段只做“移动代码 + 调整引用”，不改业务逻辑。
2. 每次提交只包含一个批次，避免混合改动。
3. 每个批次改动后必须立即跑对应子集测试。
4. 对外 API 不变，优先通过 `pub(crate) use` 维持调用点稳定。

### 统一校验命令

```bash
cargo fmt --check
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## 1. 目标目录结构

### chat_adapter

```text
src/chat_adapter/
  mod.rs
  types.rs
  ilink_client.rs
  ingest.rs
  command_handlers.rs
  session_flow.rs
  delivery.rs
  helpers.rs
```

### task_store

```text
src/task_store/
  mod.rs
  types.rs
  schema.rs
  sessions.rs
  memory.rs
  embedding_cache.rs
  tasks.rs
  chunk_queue.rs
  url_guard.rs
  logging.rs
```

### agent_core

```text
src/agent_core/
  mod.rs
  types.rs
  retriever_factory.rs
  recovery.rs
  context_assembly.rs
  watchdog.rs
  trace.rs
  llm_client.rs
  command_parse.rs
  logging.rs
```

## 2. 执行顺序（必须按顺序）

1. Phase A: `chat_adapter`（中等复杂，外部依赖集中，最适合先练手）
2. Phase B: `task_store`（高耦合 DB 模块，按领域拆）
3. Phase C: `agent_core`（最大模块，最后拆）

## 3. Phase A - chat_adapter 拆分清单

### A0. 建立门面与子模块骨架

1. 在 `src/chat_adapter/` 新建目标子文件（空文件）。
2. `mod.rs` 增加 `mod xxx;` 声明。
3. 暂时在 `mod.rs` 里 `use` 子模块，确保编译通过。

校验：

```bash
cargo check
cargo test chat_adapter::tests::chat_log_payload_keeps_contract_fields
```

### A1. 迁移基础类型与协议结构

迁移目标：

- `TextItem`, `MessageItem`, `FlexibleId`, `WireMessage`, `GetUpdatesResult`
- `FlexibleId::as_string`

来源：

- `src/chat_adapter/mod.rs:48-120`

目标文件：

- `src/chat_adapter/types.rs`

校验：

```bash
cargo check
cargo test chat_adapter::tests::handle_message_persists_inbound_text
```

### A2. 迁移 ILinkClient

迁移目标：

- `ILinkClient` struct 与其 `impl`
- `new`, `build_url`, `request`, `request_with_timeout`
- `get_qrcode`, `get_qrcode_status`, `fetch_login_qrcode`, `login`, `get_updates`, `send_text_message`

来源：

- `src/chat_adapter/mod.rs:121-374`

目标文件：

- `src/chat_adapter/ilink_client.rs`

校验：

```bash
cargo check
cargo test chat_adapter::tests::persisted_session_is_restored_on_bot_startup
```

### A3. 迁移消息入口与去重链路

迁移目标：

- `handle_message`
- `extract_message_id`
- `mark_seen`
- `extract_messages`
- `collect_text`
- `now_epoch_ms`

来源：

- `src/chat_adapter/mod.rs:538-742`
- `src/chat_adapter/mod.rs:2205-2255`

目标文件：

- `src/chat_adapter/ingest.rs`

校验：

```bash
cargo check
cargo test chat_adapter::tests::duplicate_message_is_ignored_by_handle_message
cargo test chat_adapter::tests::duplicate_link_messages_do_not_create_second_article_or_task
```

### A4. 迁移命令处理器

迁移目标：

- `generate_reply`
- `handle_link_submission`
- `handle_task_status_query`
- `handle_recent_tasks_query`
- `handle_context_debug_query`
- `handle_daily_report_query`
- `handle_weekly_report_query`
- `handle_user_memory_write`
- `handle_user_memory_suppress`
- `handle_user_memory_useful`
- `handle_user_memories_query`
- `build_daily_report_query_reply`
- `build_weekly_report_query_reply`
- `maybe_persist_auto_memory`
- `handle_task_retry`
- `handle_manual_tasks_query`
- `handle_manual_content_submission`
- `build_link_submission_reply`
- `build_task_status_reply`
- `build_recent_tasks_reply`
- `build_manual_tasks_reply`
- `build_user_memories_reply`
- `build_manual_archive_rejected_reply`
- `extract_auto_memory_candidate`

来源：

- `src/chat_adapter/mod.rs:746-1131`
- `src/chat_adapter/mod.rs:1870-2052`

目标文件：

- `src/chat_adapter/command_handlers.rs`

校验：

```bash
cargo check
cargo test chat_adapter::tests::status_query_after_link_keeps_single_task
cargo test chat_adapter::tests::manual_content_submission_archives_task
cargo test chat_adapter::tests::user_memory_commands_write_and_read_back
```

### A5. 迁移会话生命周期

迁移目标：

- `handle_session_event`
- `flush_expired_sessions`
- `update_session_state_intent`
- `persist_session_snapshot`
- `restore_persisted_sessions`

来源：

- `src/chat_adapter/mod.rs:1162-1264`
- `src/chat_adapter/mod.rs:1768-1801`

目标文件：

- `src/chat_adapter/session_flow.rs`

校验：

```bash
cargo check
cargo test chat_adapter::tests::pending_chat_session_is_persisted
cargo test chat_adapter::tests::session_state_is_written_on_flush
cargo test chat_adapter::tests::session_state_is_loaded_and_injected_into_agent_context
```

### A6. 迁移发送与调度路径

迁移目标：

- `next_poll_timeout`
- `send_generated_reply`
- `send_reply_text`
- `resend_pending_chunks`
- `process_scheduled_daily_report_push`
- `process_scheduled_weekly_report_push`
- `process_pending_tasks`
- `split_reply_into_chunks`
- `split_content_only`
- `should_send_processing_ack`

来源：

- `src/chat_adapter/mod.rs:1265-1766`
- `src/chat_adapter/mod.rs:1802`
- `src/chat_adapter/mod.rs:2119-2190`

目标文件：

- `src/chat_adapter/delivery.rs`

校验：

```bash
cargo check
cargo test chat_adapter::tests::pending_link_task_is_consumed
cargo test chat_adapter::tests::retry_command_processes_task_immediately
cargo test chat_adapter::tests::send_generated_reply_writes_trace_with_chat_context
```

### A7. 迁移通用 helper 与日志函数

迁移目标：

- `is_agent_command`, `is_llm_auth_error`, `sanitize_report_markdown_for_wechat`, `is_poll_timeout_error`
- `assert_ok`, `get_i64`, `get_str`, `first_non_empty`, `compact_json`, `value_to_string`
- `log_chat_info`, `log_chat_warn`, `log_chat_error`, `log_chat_event`
- `truncate_for_log`, `summarize_text_for_log`

来源：

- `src/chat_adapter/mod.rs:1833-2118`
- `src/chat_adapter/mod.rs:2068-2106`

目标文件：

- `src/chat_adapter/helpers.rs`

校验：

```bash
cargo fmt --check
cargo check
cargo test chat_adapter::tests::
```

## 4. Phase B - task_store 拆分清单

### B0. 建立骨架与 `TaskStore` 门面

1. 新建 `src/task_store/*.rs` 子模块。
2. `mod.rs` 保留 `TaskStore` struct 定义，其他逐步迁走。
3. 每批次新增一个 `impl TaskStore` 分片文件。

校验：

```bash
cargo check
cargo test task_store::tests::schema_is_created
```

### B1. 迁移 types 与枚举

迁移目标：

- `TaskStoreError`
- 所有 `*Record` 类型
- `MemoryType`, `UserSessionStateRecord`, `WriteDecision`, `SkipReason`, `PromoteReason`
- `MemoryWriteState`, `MemoryFeedbackState`, `FeedbackKind`

来源：

- `src/task_store/mod.rs:13-516`

目标文件：

- `src/task_store/types.rs`

校验：

```bash
cargo check
cargo test task_store::tests::memory_type_user_isolation
```

### B2. 迁移 schema 与迁移逻辑

迁移目标：

- `init_schema`
- `ensure_column_exists`

来源：

- `src/task_store/mod.rs:2165-2551`

目标文件：

- `src/task_store/schema.rs`

校验：

```bash
cargo check
cargo test task_store::tests::schema_is_created
cargo test task_store::tests::user_session_state_v2_migration_on_existing_db
```

### B3. 迁移 session / token 相关

迁移目标：

- `record_inbound_message`
- `upsert_context_token`
- `get_context_token`
- `upsert_session_state`
- `delete_session_state`
- `list_session_states`
- `load_user_session_state`
- `upsert_user_session_state`
- `clear_user_session_state`
- `cleanup_expired_context_tokens`
- `cleanup_expired_user_session_states`

来源：

- `src/task_store/mod.rs:606-848`
- `src/task_store/mod.rs:2467-2516`

目标文件：

- `src/task_store/sessions.rs`

校验：

```bash
cargo check
cargo test task_store::tests::duplicate_message_is_ignored_even_after_reopen
cargo test task_store::tests::session_state_can_be_persisted_listed_and_deleted
```

### B4. 迁移 memory 相关

迁移目标：

- `is_memory_noise`
- `add_user_memory*`
- `govern_memory_write`
- `promote_memory`
- `list_user_memories`
- `has_user_memory`
- `search_user_memories`
- `apply_memory_feedback`
- `confirm_memory_useful`
- `suppress_memory`

来源：

- `src/task_store/mod.rs:517-1277`

目标文件：

- `src/task_store/memory.rs`

校验：

```bash
cargo check
cargo test task_store::tests::govern_write_state_counters_accurate
cargo test task_store::tests::new_memory_types_sort_by_priority
cargo test task_store::tests::suppress_memory_rejects_other_users_memory
```

### B5. 迁移 embedding cache 相关

迁移目标：

- `text_hash`
- `get_embedding`
- `get_embeddings_batch`
- `put_embedding`
- `put_embeddings_batch`
- `clear_embedding_cache`
- `embedding_cache_stats`

来源：

- `src/task_store/mod.rs:1278-1468`

目标文件：

- `src/task_store/embedding_cache.rs`

校验：

```bash
cargo check
cargo test task_store::tests::schema_is_created
cargo test task_store::tests::user_memory_migration_adds_columns
```

### B6. 迁移 task 状态机与归档

迁移目标：

- `record_link_submission`
- `get_task_status`
- `get_task_content`
- `list_recent_tasks`
- `list_manual_tasks`
- `list_archived_tasks`
- `list_archived_tasks_in_range`
- `retry_task`
- `list_pending_tasks`
- `get_pending_task`
- `get_task_by_id`
- `list_claimable_tasks`
- `claim_task`
- `mark_task_archived`
- `mark_task_awaiting_manual_input`
- `mark_task_failed`

来源：

- `src/task_store/mod.rs:1470-2164`

目标文件：

- `src/task_store/tasks.rs`

校验：

```bash
cargo check
cargo test task_store::tests::link_submission_creates_article_and_task
cargo test task_store::tests::retry_task_resets_status_and_clears_error
cargo test task_store::tests::expired_lease_task_can_be_reclaimed
```

### B7. 迁移 pending chunk、URL guard、logging

迁移目标：

- `insert_pending_chunks`
- `list_pending_chunks`
- `delete_pending_chunk`
- `normalize_url`, `is_private_url`, `is_private_host*`, `parse_ipv4_address` 等 URL guard 函数
- `log_task_store_info/warn/error/event`
- `summarize_text_for_log`
- `source_domain`, `strip_tracking_query_pairs`

来源：

- `src/task_store/mod.rs:2384-2788`
- `src/task_store/mod.rs:2553-2788`

目标文件：

- `src/task_store/chunk_queue.rs`
- `src/task_store/url_guard.rs`
- `src/task_store/logging.rs`

校验：

```bash
cargo check
cargo test task_store::tests::private_network_urls_are_rejected
cargo test task_store::tests::domain_resolving_to_private_ip_is_blocked
cargo test task_store::tests::pending_tasks_can_be_listed_and_archived
```

## 5. Phase C - agent_core 拆分清单

> 说明：`agent_core` 最大，先拆“纯函数和工具层”，最后拆控制流，风险最低。

### C0. 建立模块骨架

1. 新建 `src/agent_core/*.rs` 子文件。
2. `mod.rs` 只先加 `mod ...;`，暂不迁移。

校验：

```bash
cargo check
cargo test agent_core::tests::loop_create_then_read
```

### C1. 迁移 retriever 工厂

迁移目标：

- `RetrieverMode`
- `select_retriever`
- `NoOpRetriever`（及其 trait impl）

来源：

- `src/agent_core/mod.rs:40-227`
- `src/agent_core/mod.rs:1645` 附近 `Retriever` impl

目标文件：

- `src/agent_core/retriever_factory.rs`

校验：

```bash
cargo check
cargo test agent_core::tests::rule_mode_returns_rule_retriever_directly
cargo test agent_core::tests::hybrid_mode_returns_hybrid_retriever_with_fallback
```

### C2. 迁移恢复策略与控制器

迁移目标：

- `RecoveryPolicy`
- `default_recovery_for_failure`
- `ControllerState` 与其 methods
- `ReplanScope`, `FailureAction`, `StepFailureKind`, `RecoveryOutcome` 的 `as_str`
- `handle_recorded_failure` 依赖的辅助结构

来源：

- `src/agent_core/mod.rs:519-763`
- `src/agent_core/mod.rs:2125-2244`
- `src/agent_core/mod.rs:5090-5127`

目标文件：

- `src/agent_core/recovery.rs`

校验：

```bash
cargo check
cargo test agent_core::tests::transient_failure_retry_then_replan
cargo test agent_core::tests::replan_budget_exhaustion_turns_into_ask_user
```

### C3. 迁移 context 组装链路

迁移目标：

- `ContextCompactionConfig`
- `MemoryBudget` 动态调整
- `SessionState` 与 `from_retrieved`
- `ContextAssembler` 相关 methods
- `build_context_pack`
- `merge_string_arrays_with_runtime_reserve`
- `derive_runtime_session_state`
- `load_business_context_snapshot`
- `project_session_state_to_trace`
- `build_context_summary`
- `select_previous_observations`
- `append_session_state_lines`
- `append_context_section_overview`
- `render_context_preview`

来源：

- `src/agent_core/mod.rs:229-1550`
- `src/agent_core/mod.rs:4227-4469`
- `src/agent_core/mod.rs:4555-4667`

目标文件：

- `src/agent_core/context_assembly.rs`

校验：

```bash
cargo check
cargo test agent_core::tests::context_pack_records_trim_and_drop_reasons
cargo test agent_core::tests::context_assembler_includes_business_context_sections
cargo test agent_core::tests::preview_context_verbose_includes_section_content_and_memory_drop_details
```

### C4. 迁移 watchdog 与观测校验

迁移目标：

- `validate_expected_observation`
- `detect_low_value_observation_failure`
- `detect_repeated_action_failure`
- `detect_trajectory_drift_failure`
- `detect_stalled_trajectory_failure`
- `failure_to_observation`
- `classify_tool_execution_failure`
- `default_expected_observation_for_decision`
- `parse_expected_observation`

来源：

- `src/agent_core/mod.rs:4752-5088`
- `src/agent_core/mod.rs:5477-5541`

目标文件：

- `src/agent_core/watchdog.rs`

校验：

```bash
cargo check
cargo test agent_core::tests::low_value_observation_triggers_replan
cargo test agent_core::tests::stalled_trajectory_escalates_to_full_replan_then_ask_user
```

### C5. 迁移 trace 模型与落盘

迁移目标：

- `AgentRunTrace` struct 与 methods
- `persist`
- `write_daily_index_markdown`
- `to_markdown`
- `render_daily_index_markdown`
- `truncate_for_trace`
- trace 相关展示辅助函数（`display_trace_time` 等）

来源：

- `src/agent_core/mod.rs:2971-4218`

目标文件：

- `src/agent_core/trace.rs`

校验：

```bash
cargo check
cargo test agent_core::tests::agent_run_writes_trace_file
cargo test agent_core::tests::daily_index_markdown_is_generated
```

### C6. 迁移 LLM client 与配置解析

迁移目标：

- `LlmClient` 与 `plan/plan_with_config`
- `load_llm_config`
- `normalize_base_url`
- `clean_env`
- `get_env`
- `key_tail`
- `is_llm_auth_error`
- OpenAI response structs

来源：

- `src/agent_core/mod.rs:2490-2705`
- `src/agent_core/mod.rs:5218-5302`

目标文件：

- `src/agent_core/llm_client.rs`

校验：

```bash
cargo check
cargo test agent_core::tests::llm_plan_json_is_supported
cargo test agent_core::tests::llm_plan_markdown_json_is_supported
```

### C7. 迁移命令解析与 LLM plan 映射

迁移目标：

- `parse_user_command`
- `normalize_user_command`
- `parse_llm_plan`
- `map_llm_plan`
- `extract_json_object`
- `split_path_and_content`
- `ToolAction`/`AgentDecision` 展示辅助（`name`, `target`, `summary`）

来源：

- `src/agent_core/mod.rs:5320-5562`
- `src/agent_core/mod.rs:5153-5216`

目标文件：

- `src/agent_core/command_parse.rs`

校验：

```bash
cargo check
cargo test agent_core::tests::invalid_command_returns_error
cargo test agent_core::tests::map_llm_plan_requires_path_for_read
```

### C8. 精简 `mod.rs` 为编排层

保留在 `mod.rs` 的内容：

- `AgentCore` 构造函数
- `run` / `run_with_context`
- `decide`
- `execute_planned_decision`
- `execute_tool_action`
- `watchdog_review`（如果未迁走）

要求：

1. `mod.rs` 最终目标 <= 1500 行
2. 只保留编排，不保留大量纯函数

校验：

```bash
cargo fmt --check
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test agent_core::tests::
```

## 6. 每阶段验收标准

### A 阶段完成标准

1. `chat_adapter/mod.rs` 降到 <= 1200 行
2. `chat_adapter::tests::` 全绿
3. 关键流程测试全绿：去重、会话 flush、分片发送、任务补录

### B 阶段完成标准

1. `task_store/mod.rs` 降到 <= 1000 行
2. `task_store::tests::` 全绿
3. URL guard、task 状态流、memory 治理测试全绿

### C 阶段完成标准

1. `agent_core/mod.rs` 降到 <= 1500 行
2. `agent_core::tests::` 全绿
3. trace 持久化、watchdog、LLM 计划解析测试全绿

## 7. 最终收口检查

```bash
cargo fmt --check
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
wc -l src/agent_core/mod.rs src/task_store/mod.rs src/chat_adapter/mod.rs
```

完成定义：

1. 全量检查通过
2. 三个巨型文件已降到目标行数
3. 无行为回归（以既有测试 + 手工 smoke 为准）

## 8. 推荐提交粒度

1. `refactor(chat_adapter): split transport / ingest / delivery / sessions / handlers`
2. `refactor(task_store): split schema / sessions / memory / tasks / url_guard / queue`
3. `refactor(agent_core): split trace / context / recovery / llm / parse / watchdog`

每个提交都要满足：独立可编译、独立可测试、可单独回滚。
