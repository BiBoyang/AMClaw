# 简报：全量日志已进入“可评测阶段”（2026-04-18）

## 一句话结论

AMClaw 的全量日志建设已从“可记录”进入“可评测、可对比、可收尾”的阶段，具备对外写作条件。

---

## 背景

从一开始，项目就确定了“全量日志”方向。经过持续补充，当前日志已经不只是排障材料，而是能稳定产出评测结论的基础设施。

---

## 当前状态（证据）

### 1) 日志体量与覆盖

- `data/agent_traces` 已形成双日期窗口：
  - `2026-04-01`：4 条真实 trace
  - `2026-04-18`：16 条合成 trace
- 总计 20 条样本，能够覆盖成功、fallback、context drop、失败与恢复场景。

### 2) 自动评测已跑通

执行命令：

```bash
cd /Users/boyang/Desktop/AMClaw
cargo run --bin trace_eval
```

报告路径：

`notes/agent-eval/reports/TRACE-EVAL-REPORT.md`

核心指标（当前基线）：
- success_rate：75.0%（15/20）
- fallback_rate：35.0%（7/20）
- context_drop_rate：15.0%（3/20）
- tool_success_rate：82.1%（23/28）
- recovery_success_rate：16.7%（1/6）
- stall_or_drift：1

### 3) 规则与流程已配套

- 指标口径：`EVAL-METRICS-SPEC-2026-04-18.md`
- 失败分类：`EVAL-FAILURE-TAXONOMY-2026-04-18.md`
- 对比规则：`EVAL-COMPARISON-RULES-2026-04-18.md`
- 基线样本：`EVAL-BASELINE-SAMPLES-2026-04-18.md`
- 收尾模板：`sessions/SESSION-TEMPLATE.md`（包含评测摘要段）

---

## 这意味着什么

当前体系的本质是：**日志驱动的评测闭环**。  
它不是单纯日志系统，也不只是错误归因，而是把“日志 -> 指标 -> 判定 -> 收尾”连成了可执行链路。

---

## 风险与边界

1. 真实样本占比仍偏低（4/20）。  
2. 恢复成功率目前还是临时代理口径（需补结构化恢复字段）。  
3. `--compare` 自动对比尚未落地（规则已文档化）。

---

## 下一步（建议）

### P0

1. 为 `trace_eval` 增加 `--compare` 输出 PASS/WARN/FAIL。  
2. 在 trace 增加 `recovery_action` / `recovery_result` 字段。

### P1

3. baseline 扩展到 30 条并提升真实样本占比至 >=50%。

---

## 对外口径建议

“我们先把全量日志做厚，再把日志转化为评测与决策能力；现在已进入可复现、可比较、可复盘的阶段。”

