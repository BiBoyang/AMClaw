# Trace Eval Gate 规范

## 版本

v1.0 — 2026-04-20

## 目的

`trace_eval --compare` 的结果不仅是报告，还要能作为 CI / 收尾阶段的可执行门禁。
门禁输出精简、退出码可编程，支持 `strict` 模式收紧口径。

## 核心 7 项指标

| 指标 | 方向 | warn_delta | fail_delta | absolute_fail |
|---|---|---|---|---|
| success_rate | 高好 | 3.0pp | 5.0pp | - |
| fallback_rate | 低好 | 3.0pp | 5.0pp | - |
| context_drop_rate | 低好 | 3.0pp | 5.0pp | - |
| state_present_rate | 高好 | 5.0pp | 10.0pp | - |
| memory_injected_rate | 高好 | 5.0pp | 10.0pp | - |
| recovery_success_rate | 高好 | 10.0pp | 20.0pp | < 60% |
| unknown_failure_rate | 低好 | 2.0pp | 5.0pp | > 10% |

## 判定规则

### 单指标判定

- 改善或持平 → PASS
- 退化 >= warn_delta 且 < fail_delta → WARN
- 退化 >= fail_delta → FAIL
- absolute_fail 触发时直接 FAIL（无视 delta）
- 分母为 0 → N/A（不直接 FAIL）

### 综合判定（OVERALL）

1. 基础判定：
   - 任一 core FAIL → 总体 FAIL
   - 无 FAIL 但有 WARN → 总体 WARN
   - core 全 PASS → 总体 PASS

2. N/A 封顶规则：
   - core 中 N/A > 2 个 → 总体最多 WARN（cap_at_warn）

3. 样本保护规则：
   - after.total_runs < 20 → 总体最多 WARN（cap_at_warn）

4. baseline 覆盖降级规则：
   - baseline 覆盖率下降 > 20pp → 总体降一档（PASS→WARN，WARN→FAIL）

5. 硬门槛（绝对值）：
   - unknown_failure_rate > 10% → 总体 FAIL
   - success_rate 降幅 > 5pp → 总体 FAIL

## 退出码

| 总体 | 默认 | --gate-strict |
|---|---|---|
| PASS | 0 | 0 |
| WARN | 0 | 2 |
| FAIL | 1 | 1 |
| N/A | 2 | 2 |

## CLI 用法

```bash
# 标准 compare 模式（生成完整报告）
cargo run --bin trace_eval -- --compare-before notes/reports/BEFORE.md --compare-after notes/reports/AFTER.md

# Gate 模式（精简输出 + 退出码）
cargo run --bin trace_eval -- --compare-before notes/reports/BEFORE.md --compare-after notes/reports/AFTER.md --gate
# 输出示例：
# OVERALL=PASS
# REASONS=全部核心指标 PASS

# Gate strict 模式（WARN 也返回非0）
cargo run --bin trace_eval -- --compare-before BEFORE.md --compare-after AFTER.md --gate-strict
# WARN 时退出码 = 2
```

## 何时用 --gate-strict

- CI 流水线中不允许任何指标退化 → 用 `--gate-strict`
- 收尾阶段允许轻微退化（只拦截 FAIL）→ 用 `--gate`（默认宽松）
- 本地调试或评审阶段 → 不用 gate，看完整报告

## 实现位置

- `src/bin/trace_eval.rs`：`compute_overall()`、`run_compare_mode()`
