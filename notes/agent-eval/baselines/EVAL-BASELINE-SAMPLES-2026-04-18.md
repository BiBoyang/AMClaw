# Agent 评测样本基线集（2026-04-18）

> 对应阶段：Phase 8 / Step 1.2
> 目标：建立固定样本池，支持每次改动的 before/after 对比评测。

---

## 1) 样本集目标

本阶段目标样本量：**20 条**（最少）。

分层覆盖：

1. 成功且无明显问题：5 条
2. 成功但发生 fallback：6 条（含原有 4 条）
3. 成功但 context drop 明显：3 条
4. 失败样本（含恢复成功）：6 条

> 说明：v1 基线共 20 条，其中 4 条为真实 trace（2026-04-01），16 条为合成 trace（2026-04-18）。

---

## 2) 固定字段模板

每条样本固定记录以下字段（后续不变）：

- `run_id`
- `trace_file`
- `date`
- `source_type`
- `success`
- `llm_fallback`
- `context_drop`
- `state_present`
- `memory_injected`
- `primary_failure_type`
- `recovery_result`
- `notes`

---

## 3) 当前基线（v1：20条）

### 3.1 成功且无明显问题（5条）

| run_id | trace_file | date | success | llm_fallback | context_drop | state_present | memory_injected | primary_failure_type | recovery_result | notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `a1111111-1111-1111-1111-111111111111` | `data/agent_traces/2026-04-18/run_20260418T080000_a1111111-1111-1111-1111-111111111111.json` | `2026-04-18` | true | false | false | true | true | (none) | N/A | 有 memory 注入 + 有 state，正常完成 |
| `a2222222-2222-2222-2222-222222222222` | `data/agent_traces/2026-04-18/run_20260418T080100_a2222222-2222-2222-2222-222222222222.json` | `2026-04-18` | true | false | false | false | false | (none) | N/A | 简单问候，规则直接回复 |
| `a3333333-3333-3333-3333-333333333333` | `data/agent_traces/2026-04-18/run_20260418T080200_a3333333-3333-3333-3333-333333333333.json` | `2026-04-18` | true | false | false | false | true | (none) | N/A | 有 memory 注入，无 state，多步工具 |
| `a4444444-4444-4444-4444-444444444444` | `data/agent_traces/2026-04-18/run_20260418T080300_a4444444-4444-4444-4444-444444444444.json` | `2026-04-18` | true | false | false | true | false | (none) | N/A | 有 state，无 memory，LLM 调用成功 |
| `a5555555-5555-5555-5555-555555555555` | `data/agent_traces/2026-04-18/run_20260418T080400_a5555555-5555-5555-5555-555555555555.json` | `2026-04-18` | true | false | false | true | true | (none) | N/A | 多工具链成功，有 memory 有 state |

### 3.2 成功但发生 fallback（6条）

| run_id | trace_file | date | success | llm_fallback | context_drop | state_present | memory_injected | primary_failure_type | recovery_result | notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `9633a8ee-6479-45ba-94a0-ac3831af5727` | `data/agent_traces/2026-04-01/run_20260401T000945_9633a8ee-6479-45ba-94a0-ac3831af5727.json` | `2026-04-01` | true | true | false | false | false | `llm_transport_error` | success | LLM 请求失败后 fallback 成功 |
| `9aa8cdec-ed66-4731-94a4-638e57ab2976` | `data/agent_traces/2026-04-01/run_20260401T001648_9aa8cdec-ed66-4731-94a4-638e57ab2976.json` | `2026-04-01` | true | true | false | false | false | `llm_transport_error` | success | 含 URL 级网络报错，最终成功 |
| `21cced62-914e-4453-b2dc-352d363c67e7` | `data/agent_traces/2026-04-01/run_20260401T004009_21cced62-914e-4453-b2dc-352d363c67e7.json` | `2026-04-01` | true | true | false | false | false | `llm_transport_error` | success | fallback 成功收敛 |
| `ece3aad2-a621-4619-ab3b-93abba3ea3c2` | `data/agent_traces/2026-04-01/run_20260401T005133_ece3aad2-a621-4619-ab3b-93abba3ea3c2.json` | `2026-04-01` | true | true | false | false | false | `llm_transport_error` | success | fallback 成功，输出正常 |
| `b1111111-1111-1111-1111-111111111111` | `data/agent_traces/2026-04-18/run_20260418T080500_b1111111-1111-1111-1111-111111111111.json` | `2026-04-18` | true | true | false | false | false | `llm_transport_error` | success | 模型超时回退到规则解析，成功 |
| `b2222222-2222-2222-2222-222222222222` | `data/agent_traces/2026-04-18/run_20260418T080600_b2222222-2222-2222-2222-222222222222.json` | `2026-04-18` | true | true | false | false | false | `llm_auth_error` | success | 内容审核触发回退，最终成功 |

### 3.3 成功但 context drop 明显（3条）

| run_id | trace_file | date | success | llm_fallback | context_drop | state_present | memory_injected | primary_failure_type | recovery_result | notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `c1111111-1111-1111-1111-111111111111` | `data/agent_traces/2026-04-18/run_20260418T080700_c1111111-1111-1111-1111-111111111111.json` | `2026-04-18` | true | false | true | false | true | `context_overtrim` | N/A | memory budget 不足，3 条 memory 被丢弃 |
| `c2222222-2222-2222-2222-222222222222` | `data/agent_traces/2026-04-18/run_20260418T080800_c2222222-2222-2222-2222-222222222222.json` | `2026-04-18` | true | false | true | false | false | `context_overtrim` | N/A | state section 被 trim，history tail 丢失 |
| `c3333333-3333-3333-3333-333333333333` | `data/agent_traces/2026-04-18/run_20260418T080900_c3333333-3333-3333-3333-333333333333.json` | `2026-04-18` | true | false | true | false | false | `context_overtrim` | N/A | history 过长导致裁剪，移除 3 轮旧对话 |

### 3.4 失败样本（6条）

| run_id | trace_file | date | success | llm_fallback | context_drop | state_present | memory_injected | primary_failure_type | recovery_result | notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `d1111111-1111-1111-1111-111111111111` | `data/agent_traces/2026-04-18/run_20260418T081000_d1111111-1111-1111-1111-111111111111.json` | `2026-04-18` | false | false | false | false | false | `tool_call_error` | failed | 路径超出工作区边界，无恢复 |
| `d2222222-2222-2222-2222-222222222222` | `data/agent_traces/2026-04-18/run_20260418T081100_d2222222-2222-2222-2222-222222222222.json` | `2026-04-18` | false | false | false | false | false | `llm_auth_error` | failed | API key 401，无可用 fallback |
| `d3333333-3333-3333-3333-333333333333` | `data/agent_traces/2026-04-18/run_20260418T081200_d3333333-3333-3333-3333-333333333333.json` | `2026-04-18` | false | false | false | false | false | `planning_stall_or_drift` | failed | replan 超过最大次数(5)，陷入循环 |
| `d4444444-4444-4444-4444-444444444444` | `data/agent_traces/2026-04-18/run_20260418T081300_d4444444-4444-4444-4444-444444444444.json` | `2026-04-18` | false | true | false | false | false | `fallback_exhausted` | failed | 主模型 + fallback 模型均失败 |
| `d5555555-5555-5555-5555-555555555555` | `data/agent_traces/2026-04-18/run_20260418T081400_d5555555-5555-5555-5555-555555555555.json` | `2026-04-18` | true | false | false | false | false | `tool_call_error` | success | 路径越界后自动修正，恢复成功 |
| `d6666666-6666-6666-6666-666666666666` | `data/agent_traces/2026-04-18/run_20260418T081500_d6666666-6666-6666-6666-666666666666.json` | `2026-04-18` | false | false | false | false | false | `unknown_failure` | failed | worker panic，未知错误 |

---

## 4) 覆盖统计

| 类别 | 目标 | 实际 | 状态 |
| --- | ---: | ---: | --- |
| 成功且无问题 | 5 | 5 | 完成 |
| 成功 + fallback | 6 | 6 | 完成 |
| 成功 + context_drop | 3 | 3 | 完成 |
| 失败样本 | 6 | 6 | 完成 |
| **总计** | **20** | **20** | **完成** |

### 失败类型覆盖

| failure_type | 样本数 | 来源 |
| --- | ---: | --- |
| `llm_transport_error` | 5 | 真实 4 + 合成 1 |
| `llm_auth_error` | 2 | 合成 2 |
| `tool_call_error` | 2 | 合成 2 |
| `context_overtrim` | 3 | 合成 3 |
| `planning_stall_or_drift` | 1 | 合成 1 |
| `fallback_exhausted` | 1 | 合成 1 |
| `unknown_failure` | 1 | 合成 1 |

---

## 5) 增补规则（后续执行）

每次新增样本时遵守：

1. 优先补齐缺失类别，不重复采集同类样本。
2. 每次至少新增 5 条，直到达到 30 条。
3. 若新增字段（如 `context_pack_drop_reasons`），仅追加列，不改旧列语义。
4. 若样本归类变更，保留原分类并新增 `reclassified_as` 备注字段。

---

## 6) 与评测脚本的衔接

- 使用 `src/bin/trace_eval.rs` 生成日报：
  - `notes/agent-eval/reports/TRACE-EVAL-REPORT.md`
- 本文件用于固定"对比样本池"，日报用于"全量趋势观察"。

两者角色：

1. **Baseline Samples**：稳定对比（small but fixed）
2. **Trace Eval Report**：全量监控（broad and rolling）

---

## 7) 完成标准（DoD）

- [x] 样本数 >= 20
- [x] 四类场景都有覆盖
- [x] 每条样本具备固定字段模板
- [x] 可支持 before/after 对比输出
- [x] 失败类型覆盖 >= 6 种
- [x] 至少 1 条恢复成功样本
