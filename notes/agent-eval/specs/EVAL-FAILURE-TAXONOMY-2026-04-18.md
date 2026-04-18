# Agent 评测失败分类字典（2026-04-18）

> 对应阶段：Phase 8（Agent 评测）  
> 目标：把“失败”从模糊描述变成可统计、可对比、可恢复的标准分类。

---

## 1) 分类原则

1. **先主因后现象**：优先按“根因类别”归类，不按表面报错文本分散统计。
2. **单条样本可多标签**：允许一个 run 同时命中多个 failure 标签（例如 transport + fallback）。
3. **统一映射顺序**：同一条 trace 按固定优先级映射，避免同样样本在不同批次归类不一致。
4. **先覆盖高频**：本版先覆盖高频类型，未知类型先收敛到 `unknown_failure` 并记录原始线索。

---

## 2) 一级分类（L1 + L2）

### A. `llm_auth_error`
- 含义：模型调用鉴权失败（如 401）。
- 线索：
  - `llm_calls[].error` 含鉴权关键字
  - 或 fallback reason 指向 auth failure
- 建议恢复动作：`ask_user`（提示检查 provider 配置）。

### B. `llm_transport_error`
- 含义：模型调用网络/传输失败（超时、连接失败、DNS 等）。
- 线索：
  - `llm_calls[].error` 含 transport/timeout/send request 等关键字
- 建议恢复动作：`retry_step`（带次数上限）-> `fallback`。

### C. `tool_call_error`
- 含义：工具调用失败（路径、参数、执行错误）。
- 线索：
  - `tool_calls[].success = false`
  - 或 `failures` 包含 tool 执行失败语义
- 建议恢复动作：`replan` 或 `ask_user`（补参数）。

### D. `context_overtrim`
- 含义：上下文裁剪过度导致关键信息缺失。
- 线索：
  - `context_pack_drop_reasons` 非空且出现 budget 相关原因
  - 同时 `step_count` 增高、反复 ask/replan（作为辅助信号）
- 建议恢复动作：提高相关 section 预算或优先级；补充 state/memory 关键段固定保留。

### E. `memory_conflict`
- 含义：记忆冲突/降级导致信息不一致或被错误覆盖。
- 线索：
  - governance 相关 skip/promote 事件异常集中
  - memory 注入结果与预期类型优先级不符（结合样本对照）
- 建议恢复动作：检查 `MemoryType` 优先级链与冲突规则。

### F. `session_state_missing_or_stale`
- 含义：SessionState 缺失、过期或与当前会话意图不一致。
- 线索：
  - `persistent_state_present=false`（在应有状态场景）
  - 或状态存在但行为明显退化（结合样本标签）
- 建议恢复动作：检查状态加载/回写链路，补刷新策略。

### G. `planning_stall_or_drift`
- 含义：规划循环停滞、重规划过多、轨迹漂移。
- 线索：
  - failure type 包含 `stalled_trajectory` / `trajectory_drift`
  - 或 `replan_count` 异常偏高
- 建议恢复动作：收紧 replan 预算，增强 done_rule/expected_observation。

### H. `done_rule_validation_fail`
- 含义：工具成功但收敛判定失败，导致无法 finalize。
- 线索：
  - failure type 包含 done_rule 校验失败语义
- 建议恢复动作：调整 done_rule 或补充 required observation 字段。

### I. `fallback_exhausted`
- 含义：主链路失败后 fallback 仍未收敛。
- 线索：
  - 出现 fallback reason，最终 `success=false`
- 建议恢复动作：`ask_user` + 明确下一步人工输入要求。

### J. `unknown_failure`
- 含义：未命中以上分类的失败。
- 线索：
  - `success=false` 且无可映射 failure/type/error 线索
- 建议恢复动作：进入人工分类池，下一轮补字典。

---

## 3) 固定映射优先级（从上到下匹配）

1. `llm_auth_error`
2. `llm_transport_error`
3. `tool_call_error`
4. `done_rule_validation_fail`
5. `planning_stall_or_drift`
6. `context_overtrim`
7. `memory_conflict`
8. `session_state_missing_or_stale`
9. `fallback_exhausted`
10. `unknown_failure`

> 说明：同条样本可附加辅助标签，但主标签按此顺序取首个命中项。

---

## 4) 报告输出字段建议

每条样本至少输出：

- `run_id`
- `success`
- `primary_failure_type`
- `secondary_failure_types`（可空）
- `recovery_action`
- `recovery_result`
- `raw_signals`（简版线索，如 error 摘要 / drop reasons）

---

## 5) 质量门槛（建议）

1. `unknown_failure` 占比 < 10%
2. Top3 failure 类型占比可解释（都有明确修复路径）
3. 每类失败至少映射一个默认恢复动作

---

## 6) 下一步衔接

- Step 1.2：基于本字典构建样本基线集  
  产出：`EVAL-BASELINE-SAMPLES-2026-04-18.md`
