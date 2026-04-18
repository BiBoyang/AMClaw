# 全量日志写作资产清单（2026-04-18）

> 用途：为“全量日志 -> 评测闭环”主题文章提供可复现证据。  
> 范围：`/Users/boyang/Desktop/AMClaw`。

---

## 1) 原始日志资产（Raw）

### A. Trace JSON（核心证据）

- 目录：`data/agent_traces/2026-04-01/`
  - 实际 JSON：4 条（真实 trace）
- 目录：`data/agent_traces/2026-04-18/`
  - 实际 JSON：16 条（合成 trace）
- 全量 JSON 合计：20 条

说明：这批 JSON 是“全量日志”的主证据，文章里可作为“数据底座”引用。

### B. 索引与辅助文件

- `data/agent_traces/2026-04-01/index.jsonl`：按行索引（用于回溯 run）。
- `data/agent_traces/2026-04-01/*.md`：部分 run 的侧写文本（可作为案例描述素材）。

---

## 2) 评测与归因资产（Evaluation）

- `notes/agent-eval/reports/TRACE-EVAL-REPORT.md`：自动评测报告（核心统计输出）。
- `notes/agent-eval/specs/EVAL-METRICS-SPEC-2026-04-18.md`：指标口径规范。
- `notes/agent-eval/specs/EVAL-FAILURE-TAXONOMY-2026-04-18.md`：失败归因字典。
- `notes/agent-eval/baselines/EVAL-BASELINE-SAMPLES-2026-04-18.md`：固定基线样本（20 条）。
- `notes/agent-eval/specs/EVAL-COMPARISON-RULES-2026-04-18.md`：PASS/WARN/FAIL 判定规则。

说明：这组文档共同证明“不是只记录日志，而是把日志转成可对比评测资产”。

---

## 3) 流程沉淀资产（Process）

- `sessions/SESSION-TEMPLATE.md`：收尾模板（含评测摘要固定段）。
- `notes/agent-eval/plans/PHASE-8-EVAL-SUMMARY-2026-04-18.md`：阶段总结。
- `notes/agent-eval/briefs/PHASE-8-EVAL-RETRO-TECH-2026-04-18.md`：技术复盘长文。

说明：这组文件用于证明“评测动作已进入团队流程，而不是一次性动作”。

---

## 4) 当前可直接引用的数字（来自报告）

来源：`notes/agent-eval/reports/TRACE-EVAL-REPORT.md`

- `total`: 20
- `success_rate`: 75.0%（15/20）
- `fallback_rate`: 35.0%（7/20）
- `context_drop_rate`: 15.0%（3/20）
- `tool_success_rate`: 82.1%（23/28）
- `recovery_success_rate`: 16.7%（1/6）
- `stall_or_drift hits`: 1
- `baseline hits`: 20/20（100%）

---

## 5) 文章取证建议（按证据力度）

### 必用（P0）

1. `data/agent_traces/*/*.json`（证明“全量日志持续积累”）
2. `notes/agent-eval/reports/TRACE-EVAL-REPORT.md`（证明“可量化输出”）
3. `notes/agent-eval/specs/EVAL-COMPARISON-RULES-2026-04-18.md`（证明“可决策门禁”）

### 建议用（P1）

4. `notes/agent-eval/specs/EVAL-FAILURE-TAXONOMY-2026-04-18.md`（证明“错误归因标准化”）
5. `sessions/SESSION-TEMPLATE.md`（证明“进入收尾流程”）

### 可选（P2）

6. `notes/agent-eval/briefs/PHASE-8-EVAL-RETRO-TECH-2026-04-18.md`（长文展开）

---

## 6) 最小复现实验命令

```bash
cd /Users/boyang/Desktop/AMClaw
cargo run --bin trace_eval
```

输出报告路径：

`notes/agent-eval/reports/TRACE-EVAL-REPORT.md`

---

## 7) 对外表述口径（统一一句话）

“我们先做了全量日志沉淀，再把日志资产转换为可评测、可比较、可收尾的工程闭环。”

