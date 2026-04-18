# Agent 评测指标口径规范（2026-04-18）

> 对应计划：`PHASE-8-EVAL-PLAN-2026-04-18.md`  
> 目的：统一评测指标定义，保证报告可解释、可对比。

---

## 1) 总体规则

1. 统计对象：单次 `run_id`（一条 trace 视为一个样本）。
2. 统计窗口：按日期目录（`data/agent_traces/YYYY-MM-DD`）。
3. 比率统一：
   - 分母默认 `total_runs`
   - 结果保留 1 位小数（百分比）
4. 缺失字段策略：
   - 字段缺失视为 `false` 或 `0`
   - 同时在报告中记录 `missing_fields`

---

## 2) 核心指标定义（Step 1）

### 2.1 `total_runs`

- 定义：统计窗口内 trace 总数
- 字段来源：trace 文件数量

### 2.2 `success_rate`

- 定义：`success=true` 的样本占比
- 公式：`success_count / total_runs`
- 字段来源：`success`

### 2.3 `fallback_rate`

- 定义：发生 LLM fallback 的样本占比
- 公式：`fallback_count / total_runs`
- 字段来源：`llm_fallback_reason`（非空即算 fallback）

### 2.4 `context_drop_rate`

- 定义：发生 ContextPack 丢弃的样本占比
- 公式：`context_drop_count / total_runs`
- 字段来源：`context_pack_drop_reasons`（非空数组）

### 2.5 `state_present_rate`

- 定义：携带持久化 SessionState 的样本占比
- 公式：`state_present_count / total_runs`
- 字段来源：`persistent_state_present`

### 2.6 `memory_injected_rate`

- 定义：存在 memory 注入的样本占比
- 公式：`memory_injected_count / total_runs`
- 字段来源：`memory_hit_count > 0`（或 `memory_injected_count > 0`）

### 2.7 `recovery_success_rate`（若有恢复字段）

- 定义：发生恢复动作后成功收敛的占比
- 公式：`recovery_success_count / recovery_attempt_count`
- 字段来源：
  - 第一优先：显式恢复字段（后续补齐）
  - 临时口径：`failures` 非空且最终 `success=true` 视为一次恢复成功

---

## 3) 报告呈现约定

1. 顶部概览：时间窗口、样本数、核心指标表
2. 分类统计：失败类型分布（见后续 failure taxonomy）
3. 明细样本：interesting traces（最多 N 条）
4. 尾部结论：
   - 本次主要变化点
   - 与上次对比（若存在）

---

## 4) 指标质量门槛（建议）

> 以下是阶段门槛建议，可按运行稳定性调整。

1. `success_rate` 不低于前一轮 -3%
2. `context_drop_rate` 不高于前一轮 +5%
3. `fallback_rate` 不高于前一轮 +5%
4. `recovery_success_rate` 不低于 60%（有恢复样本时）

---

## 5) 下一步衔接

Step 2 将补充：

- 失败分类字典（`EVAL-FAILURE-TAXONOMY-2026-04-18.md`）
- 指标与 failure 类型的映射规则
