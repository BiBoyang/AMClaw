# Trace Evaluation Comparison Report

- **对比窗口**: before=2026-04-18 after=2026-04-18
- **样本数**: before=20 after=20
- **baseline 覆盖**: before=100.0% after=100.0%
- **综合判定**: PASS

## 核心指标（7项）

| # | 指标 | before | after | 变动 | 判定 | 说明 |
| ---: | --- | ---: | ---: | ---: | --- | --- |
| 1 | success_rate | 75.0% | 75.0% | 0.0pp | PASS | 无退化 |
| 2 | fallback_rate | 35.0% | 35.0% | 0.0pp | PASS | 无退化 |
| 3 | context_drop_rate | 15.0% | 15.0% | 0.0pp | PASS | 无退化 |
| 4 | state_present_rate | 15.0% | 15.0% | 0.0pp | PASS | 无退化 |
| 5 | memory_injected_rate | 20.0% | 20.0% | 0.0pp | PASS | 无退化 |
| 6 | recovery_success_rate | 16.7% | 16.7% | 0.0pp | PASS | 无退化 |
| 7 | unknown_failure_rate | 5.0% | 5.0% | 0.0pp | PASS | 无退化 |

**核心指标统计**: PASS=7 WARN=0 FAIL=0

## L2 扩展指标（3项）

| # | 指标 | before | after | 变动 | 判定 | 说明 |
| ---: | --- | ---: | ---: | ---: | --- | --- |
| 8 | tool_success_rate | 82.1% | 82.1% | 0.0pp | PASS | 无退化 |
| 9 | planning_stall_rate | 5.0% | 5.0% | 0.0pp | PASS | 无退化 |
| 10 | avg_step_count | 3.2 | 3.2 | 0.0步 | PASS | 无退化 |

## 判定依据

- 全部核心指标 PASS

## 后续动作建议

- 综合结论 PASS：改动无 regressions，可合并/发布
- 建议：继续观察后续 trace

---
*本对比报告由 trace_eval --compare 自动生成。*