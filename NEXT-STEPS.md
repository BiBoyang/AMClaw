# NEXT-STEPS

当前这份文件只记录"接下来最值得做什么"，不重复描述已经完成的能力。当前真实状态请看 `PLAN.md`。

## 本阶段收口（截至 2026-05-08）

以下主线可视为已完成并进入稳定维护：

- Plan-aware ReAct 主链路（含失败语义与最小 watchdog）
- 通用 HTTP 归档最小 summary（规则法）与 `summary` 落库
- `page_kind` 五分类（`error_page/article/index_like/link_post/webpage`）
- reporter / 日报对 `summary` 的展示接入
- 发布流程与文档结构整理（`notes/`、`sessions/`）
- Memory v3：`search_user_memories` 接入 agent_core context 拼装 + 命中回写 + 可观测日志 + 回归测试
- session state 持久化（dirty-merge + trace 记录）
- `ContextPack` 结构化上下文包（section budget + 渲染）
- `trace_eval` gate 结构化输出（`--gate-json` + `STATE_UPDATED_RAW`）
- CI gate 策略升级（soft/hard 模式、`GATE_MODE` 切换、shellcheck 确定性加固）
- 文档同步收口（README / DEVELOPMENT / PLAN / NEXT-STEPS / sessions / agent-eval）

结论：v0.3.2 "Context & Memory Minimal" 已收口；v0.3.3 结构性能力（session state / context pack / gate 策略）已落地。

## v0.3.2 DoD 逐项确认

1. ✅ 显式记忆可命中：`记住 我喜欢短摘要` 后，下一轮问答可体现偏好
2. ✅ 用户隔离有效：A 用户记忆不会注入到 B 用户（回归测试已覆盖）
3. ✅ 长度治理有效：context/memory 注入有预算（5 条 / 500 字符 / 单条 160 字符）
4. ✅ 退化正常：无记忆 / 无 user_id 时系统不报错，行为可回退到当前基线
5. ✅ 可观测：日志有 `memory_hit_count`、`memory_total_chars`、`memory_ids`；Trace 有 `memory_hit_count` / `memory_total_chars`

## 当前主线（v0.3.3）

### 目标

基于已落地的 `Memory v3`，把 AMClaw 的 `context / memory` 从“最小可用”推进到“可稳定演进”。

### 方向

1. 收口现有 memory 语义与观测（Phase 1 剩余）
2. 扩少量高价值长期 memory（Phase 4，当前最优先）
3. 用 trace 驱动评测闭环稳定化（Phase 5 剩余）
4. 把 `state/controller` 从 budget 扩到更完整的策略控制

统一路线文档：

- `notes/context-memory/CONTEXT-MEMORY-EVOLUTION-ROADMAP-2026-04-13.md`

## v0.3.3 推荐执行顺序

### Phase 1：收口 `Memory v3` 语义

优先要做：

1. 统一自动记忆与显式记忆的写入语义
2. 区分 `retrieved_memory_count` / `injected_memory_count`
3. 明确 `use_count` 的真实含义
4. 保持日志、trace、文档三者口径一致

### Phase 2：补显式 `SessionState` ✅

- [x] `RuntimeSessionStateSnapshot` 已引入，含 `goal` / `current_subtask` / `constraints` / `confirmed_facts` / `done_items` / `next_step` / `open_questions`
- [x] 槽位已进入 trace（`session_state_snapshot` / `final_runtime_session_state`）
- [x] 持久化采用 conservative dirty-merge，无状态时可退化运行
- [x] `persistent_state_updated` 指标已接入 compare 与 gate

### Phase 3：抽 `ContextPack` ✅

- [x] `ContextPack` 结构体已落地（`src/context_pack.rs`）
- [x] 来源已拆清：`runtime context` / `session state` / `business context` / `memories` / `latest observation` / `active plan`
- [x] trace 保留结构化 pack（`context_pack_present` / `context_pack_section_count` 等）与最终渲染 prompt
- [x] section budget 与 trim/drop reason 已可观测

### Phase 4：扩长期 Memory 类型

只优先考虑三类：

1. `user_preference`
2. `project_fact`
3. `lesson`

### Phase 5：建立 Trace 驱动评测闭环（部分完成）

- [x] `trace_eval --gate-json` 已提供结构化输出（机器可解析）
- [x] CI soft gate 已改为 JSON 消费（替代脆弱 grep）
- [x] `scripts/trace_soft_gate.sh` 已抽离并带回归测试
- [x] Gate 策略文档与 `GATE_MODE` 切换机制已落地（S19）
- [ ] 从真实 trace 中抽样并标注失败类型（`forgot_known_fact` / `missed_retrieval` / `wrong_retrieval` / `state_drift` / `repeated_work`）
- [ ] 比较机制变更前后差异（需 baseline 完备后启动）

## 当前明确不优先做

- 不先上 embedding / 向量库
- 不先做复杂 memory taxonomy
- 不先做多用户/多任务架构重构
- 不先做 `tokio` 全量迁移或 `sqlx` async 化
- 不回头重写 ReAct / Planning 主框架
- 不同时叠加多个 memory 机制
