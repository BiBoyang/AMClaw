# Agent Eval Notes

本目录用于存放 `Agent 评测` 与 `全量日志资产化` 相关文档。

## 目录结构

- `reports/`
  - `TRACE-EVAL-REPORT.md`：`trace_eval` 自动生成的评测报告（滚动更新）。
- `specs/`
  - `EVAL-METRICS-SPEC-2026-04-18.md`：指标口径规范。
  - `EVAL-FAILURE-TAXONOMY-2026-04-18.md`：失败归因字典。
  - `EVAL-COMPARISON-RULES-2026-04-18.md`：PASS/WARN/FAIL 对比判定规则。
- `baselines/`
  - `EVAL-BASELINE-SAMPLES-2026-04-18.md`：固定 baseline 样本集。
- `plans/`
  - `PHASE-8-EVAL-PLAN-2026-04-18.md`：评测阶段执行计划。
  - `PHASE-8-EVAL-SUMMARY-2026-04-18.md`：阶段总结。
  - `PHASE-NEXT-8-7-PLAN-2026-04-18.md`：下一阶段计划。
- `briefs/`
  - `BRIEF-FULL-LOG-EVAL-CLOSURE-2026-04-18.md`：对外简报。
  - `LOG-ASSETS-FOR-ARTICLE-2026-04-18.md`：写作用日志资产清单。
  - `PHASE-8-EVAL-RETRO-TECH-2026-04-18.md`：技术复盘长文。

## 常用命令

```bash
cd ~/Desktop/AMClaw
cargo run --bin trace_eval
```

默认输出：
- 报告：`notes/agent-eval/reports/TRACE-EVAL-REPORT.md`
- baseline：`notes/agent-eval/baselines/EVAL-BASELINE-SAMPLES-2026-04-18.md`

