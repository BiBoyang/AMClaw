# AMClaw 项目代码审查报告（合并版）

> 来源文件：`AMClaw-代码审查报告-2026-05-04.md` + `AMClaw-代码审查报告-2026-05-04-v2.md`
> 合并时间：2026-05-04
> 合并原则：以 v2（修订版 v3）为主干，补入 v1 中仍有价值且可自洽的未覆盖问题；争议项单列为“待确认”。

---

## 1. 总体判断

项目代码组织清晰，模块分层合理。`pipeline/` 的错误处理、`task_store/` 的 WAL + busy_timeout 配置、核心路径测试覆盖都比较扎实。

本合并版聚焦“可行动问题”：
- **P1：6 项**（优先修复，直接影响正确性/可用性）
- **P2：11 项**（排期修复，影响健壮性/可维护性/运行成本）
- **P3：若干**（改进项）
- **待确认：2 项**（有风险信号，但依赖业务语义或外部接口约束）

---

## 2. 发现列表（按严重性排序）

### P1-1 | `mark_seen` 的 DB 写入失败 Err 分支返回 `true`，导致消息重复处理

- **文件/行号**：`src/chat_adapter/mod.rs:684-702`
- **确认依据**：`record_inbound_message` 返回 `Err` 后仍 `return true`（700行），消息继续进入后续处理链。
- **影响**：DB 未落库但回复已发出时，重启后同一消息可能再次被处理。
- **修复建议**：Err 分支改为 `return false`。

### P1-2 | `mark_task_failed` 无条件清空诊断字段

- **文件/行号**：`src/task_store/mod.rs:2138`
- **确认依据**：SQL 将 `page_kind`、`snapshot_path`、`content_source` 等全部置 NULL。
- **影响**：失败现场丢失，排障成本高。
- **修复建议**：只清理运行态字段（如 `worker_id`、`lease_until`、`output_path`），保留诊断字段。

### P1-3 | `mark_task_awaiting_manual_input` 的 `snapshot_path` 无 COALESCE 保护

- **文件/行号**：`src/task_store/mod.rs:2103`
- **确认依据**：`content_source` 用了 `COALESCE`，`snapshot_path` 没有；调用方传 `None` 时会覆盖旧值。
- **修复建议**：`snapshot_path = COALESCE(?4, snapshot_path)`。

### P1-4 | worker 线程 panic 后任务处理静默停止（channel 断开）

- **文件/行号**：`src/task_executor/mod.rs:31-56`
- **确认依据**：`process_task` panic 会终止 worker；后续 `send` 失败返回 `false`，但没有自动恢复 worker。
- **影响**：系统不崩溃，但任务消费通道失效，外部表现为“任务一直不处理”。
- **修复建议**：worker loop 外包 `catch_unwind`，或增加 panic 后重建 worker 机制。

### P1-5 | `delete_session_state` 失败被静默吞掉

- **文件/行号**：`src/chat_adapter/mod.rs:1170`, `:1178`
- **确认依据**：两处均为 `let _ = self.task_store.delete_session_state(...)`。
- **影响**：stale session 可能被恢复并重复处理。
- **修复建议**：至少打 error 级结构化日志。

### P1-6 | `split_content_only` 在 `content_budget=0` 时无限循环（私有函数边界缺陷）

- **文件/行号**：`src/chat_adapter/mod.rs:2162-2185`
- **确认依据**：`content_budget == 0` 时 `start_byte` 不推进，外层 while 永远满足。
- **现状**：当前唯一调用点有保护，不会在现路径触发。
- **修复建议**：入口守卫 `if content_budget == 0 { return vec![reply.to_string()]; }`。

### P1-7 | `handle_user_memory_write` 对 `memory_id` 进行不安全字节切片 `&id[..8]`

- **文件/行号**：`src/chat_adapter/command_handlers.rs:298`
- **确认依据**：`format!("已提升已有记忆为显式记忆 (id: {})", &id[..8])`，未检查长度与字符边界。
- **影响**：当 `memory_id` 长度小于 8 字节，或第 8 字节处不是合法 UTF-8 字符边界（如多字节非 ASCII 字符）时，会触发 panic。
- **触发条件**：用户发送 `记住 <content>` 触发 `WriteDecision::Promoted` 分支，且被提升的已有记忆 id 为短字符串或非 ASCII 字符（如 `"短"`）。
- **修复建议**：按字符安全切片，如 `id.chars().take(8).collect::<String>()`；若长度不足 8 则直接显示完整 id。

---

### P2-1 | `AgentRunTrace::persist()` 对 `index.jsonl` 采用 read-modify-write，存在并发覆盖

- **文件/行号**：`src/agent_core/mod.rs:3546-3554`
- **确认依据**：先读全量字符串，再内存 append，再整文件写回，无锁。
- **修复建议**：文件锁或 append-only 写法。

### P2-2 | `message_dedup`/`inbound_messages` 主键仅用 `message_id`，未绑定 `from_user_id`

- **文件/行号**：`src/task_store/mod.rs:2198-2209`
- **确认依据**：两表均以 `message_id` 作为主键。
- **影响**：若上游 `message_id` 出现跨会话冲突，可能误去重。
- **修复建议**：主键改为复合键（如 `(message_id, from_user_id)`）或改造去重逻辑。

### P2-3 | `govern_memory_write` 去重只检索前 50 条记忆

- **文件/行号**：`src/task_store/mod.rs:978`
- **确认依据**：`self.search_user_memories(user_id, 50)`。
- **修复建议**：定向查询 `WHERE normalized_content = ?`，避免 LIMIT 造成漏判。

### P2-4 | `apply_memory_feedback` 多个 UPDATE 无事务

- **文件/行号**：`src/task_store/mod.rs:1208-1235`
- **确认依据**：循环内多次 `execute`，无显式事务边界。
- **修复建议**：包在单事务内。

### P2-5 | `cleanup_expired_user_session_states` 两条 DELETE 无事务

- **文件/行号**：`src/task_store/mod.rs:2487-2516`
- **确认依据**：两次独立删除，存在半清理。
- **修复建议**：包在单事务内。

### P2-6 | `CachedEmbeddingProvider` 每次缓存读写都打开新 SQLite 连接

- **文件/行号**：`src/retriever/cached_embedding.rs:53-78`
- **确认依据**：各方法内重复 `TaskStore::open(&self.db_path)`。
- **影响**：高并发/batch 下连接与 schema 初始化开销偏大。
- **修复建议**：连接池（如 `r2d2-sqlite`）或单线程 DB actor；避免直接缓存 `rusqlite::Connection`（`!Sync`）。

### P2-7 | `process::exit(1)` 跳过 scheduler 线程 join

- **文件/行号**：`src/main.rs:63-71`
- **确认依据**：错误路径直接退出，不执行 73-76 行收尾。
- **修复建议**：退出前发 shutdown 并 join。

### P2-8 | `tool_registry::resolve_path` normalize 与 canonicalize 分步执行（TOCTOU 窗口）

- **文件/行号**：`src/tool_registry/mod.rs:237-252`, `:263-281`
- **确认依据**：先字符串规范化，后文件系统解析。
- **修复建议**：先 canonicalize 后做 workspace 边界检查。

### P2-9 | scheduler 线程 panic 不可观测、无恢复

- **文件/行号**：`src/scheduler/mod.rs:135-204`, `src/main.rs:74-76`
- **确认依据**：主线程仅在进程退出时 join，运行期不监控。
- **修复建议**：运行期健康检查（`is_finished`）+ 失败重启或 `catch_unwind`。

### P2-10 | CI：`cargo check` 放在 `cargo test` 后

- **文件/行号**：`.github/workflows/trace-eval-compare.yml:27-30`
- **影响**：失去快速失败价值。
- **修复建议**：前置或删除。

### P2-11 | CI：`check-changes` 在 push/PR 事件上存在无效执行

- **文件/行号**：`.github/workflows/nightly-tests.yml:13-38`
- **确认依据**：下游 `test` 的条件分支使其在非 schedule 事件价值有限。
- **修复建议**：给 `check-changes` job 增加 `if: github.event_name == 'schedule'`。

---

## 3. P3 改进项（择要）

- `src/retriever/shadow.rs:101`：后台线程 `JoinHandle` 未持有，panic 不可见。
- `src/retriever/hybrid.rs:163-196`：cosine similarity 可能重复计算。
- `src/reporter/mod.rs:66,98`：硬编码 LIMIT 可能静默截断。
- `src/logging.rs:27-32`：多线程 `println!`/`eprintln!` 可能导致 JSON 行交叠。
- `src/command_router/mod.rs:246-279`：裸域名归一化规则可能误判输入。

---

## 4. 待确认项

### T-1 | `poll_loop` 先更新 cursor 再处理消息，是否存在真实“丢消息”风险

- **文件/行号**：`src/chat_adapter/mod.rs:510-518`
- **争议点**：风险大小依赖 `getupdates` 接口语义（空 cursor 起点、服务端游标行为、崩溃恢复模型）。
- **建议**：补一组故障注入测试（处理中断/重启）后再决定是否升为 P2。

### T-2 | `TaskExecutor` 无界 channel 的实际风险级别

- **文件/行号**：`src/task_executor/mod.rs:26`
- **争议点**：理论上可积压，但需要压测确认是否能达到不可接受内存增长。
- **建议**：用压测数据决定是否改为 `sync_channel` + 背压。

---

## 5. 建议修复顺序

1. `mark_seen` Err 分支改 `false`。
2. `mark_task_failed` / `mark_task_awaiting_manual_input` 字段保留策略修复。
3. TaskExecutor panic 恢复与最小健康监控。
4. `split_content_only` 入口守卫。
5. `persist(index.jsonl)` 改为锁/追加模式。
6. `task_store` 两类事务边界补齐（feedback、session cleanup）。
7. CI 两处顺序/触发条件优化。

