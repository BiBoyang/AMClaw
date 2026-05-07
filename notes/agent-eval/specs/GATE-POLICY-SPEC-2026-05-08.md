# Trace Eval Gate 策略规范

## 版本

v1.0 — 2026-05-08

## 目的

把 `trace_eval` 门禁的"是否阻断合并"从隐式约定升级为显式策略，做到：

- 条件清楚：什么情况下用什么模式
- 开关清楚：如何切换模式
- 回滚路径清楚：出问题怎么退回来

本规范不修改指标口径与评分规则，仅定义门禁的**执行策略**（何时告警、何时阻断）。
指标口径见 `EVAL-GATE-SPEC-2026-04-20.md`。

## Gate 模式定义

### Soft Gate（软门禁）

- **行为**：CI 输出 warning，上传报告 artifact，**不阻断合并**。
- **适用阶段**：策略初期验证、基线不稳定、团队对误报成本尚未充分评估。
- **退出码处理**：步骤脚本最终以 `exit 0` 结束，实现上可不依赖 `continue-on-error`，由脚本内部分支统一控制。

### Hard Gate（硬门禁）

- **行为**：按规则阻断合并（`FAIL` → workflow 失败）。
- **适用阶段**：策略稳定、误报率低、团队确认门槛可接受。
- **退出码处理**：`trace_eval` 的退出码直接决定 workflow 成败；`FAIL`（exit 1）和 `N/A`（exit 2）均阻断；`WARN` 不阻断但需在 PR 描述中说明原因。

### 模式对比

| 总体判定 | Soft Gate | Hard Gate |
|---|---|---|
| PASS | 通过，无告警 | 通过 |
| WARN | 通过，输出 warning | 通过，需在 PR 描述中说明原因与观察计划（人工治理要求，CI 不强制校验） |
| FAIL | 通过，输出 warning | **阻断合并** |
| N/A | 通过，输出 warning | **阻断合并**（样本不足或数据缺失，不可判定即不可放行） |

## 升级触发条件（Soft → Hard）

必须**同时满足**以下全部条件，方可升级：

1. **连续稳定性**：最近连续 **5 次** PR 的 gate 结果无 FAIL、无 N/A，且 WARN 次数 ≤ 1。
2. **误报率阈值**：最近 10 次 gate 结果中，因"指标口径/基础设施抖动"导致的 WARN 占比 ≤ 20%（即 ≤ 2 次）。
3. **基线样本完备度**：当前 baseline 覆盖的 trace 样本数 ≥ 20，且最近一次 baseline 更新距今 ≤ 30 天。
4. **团队确认**：维护者（当前为单人）书面确认（session 记录或 PR 描述）误报成本可接受、门槛可理解。

> **记录方式**：升级决策必须落在 session 记录或策略文档的"历史状态"章节，不可口头约定。

## 回退条件（Hard → Soft）

满足以下**任一条件**，立即回退到 Soft Gate：

1. **误报升高**：连续 2 次 PR 出现非预期 FAIL（即指标实际未退化，但因基础设施/基线漂移导致误判）。
2. **样本失真**：baseline 覆盖率下降 > 20pp，或 after 样本数 < 20 导致 N/A 频发。
3. **基础设施异常**：CI 环境变化（如 shellcheck 版本漂移、`jq` 行为变更）导致 gate 链路偶发失败。
4. **主动降级**：维护者判断当前阶段不适合 Hard Gate，主动发起回退。

> **回退时效**：触发回退条件后，**同一次 PR 内**即应切回 Soft Gate，不等待下次合并。

## 策略切换机制

### CI 层（workflow）

在 `.github/workflows/trace-eval-compare.yml` 中通过环境变量控制：

```yaml
env:
  GATE_MODE: soft   # 可选：soft / hard
```

Soft Gate 步骤根据 `GATE_MODE` 决定：

- **`soft`**：步骤脚本最终以 `exit 0` 结束，workflow 不失败。实现上可不依赖 `continue-on-error`，由脚本内部分支统一控制。
- **`hard`**：步骤脚本在执行 `trace_soft_gate.sh` 后，**必须以 `eval_gate.sh` 的原始退出码结束**（例如 `exit "$GATE_EXIT"`），确保 `FAIL`/`N/A` 能真正阻断 workflow。

Hard 模式步骤脚本示例：

```bash
set +e
GATE_JSON=1 ./scripts/eval_gate.sh > trace-gate.json
GATE_EXIT=$?
set -e
./scripts/trace_soft_gate.sh trace-gate.json "$GATE_EXIT" || true  # summary/warning 不阻断
exit "$GATE_EXIT"  # 传播原始退出码，实现真正的 Hard Gate
```

### 本地层

本地复现命令不受模式影响，始终输出实际判定结果。开发者可通过以下命令查看当前策略：

```bash
cat notes/agent-eval/specs/GATE-POLICY-SPEC-2026-05-08.md | grep -A5 "当前状态"
```

## 当前状态

- **生效模式**：Soft Gate
- **生效日期**：2026-04-20 起
- **已满足升级条件**：
  - [ ] 连续 5 次 PR 稳定
  - [ ] 误报率 ≤ 20%
  - [ ] 基线样本 ≥ 20
  - [ ] 团队确认
- **下次评估日期**：待定（建议在完成 5 次稳定 PR 后触发评估）

## 历史状态

| 日期 | 事件 | 触发人 |
|---|---|---|
| 2026-04-20 | Soft Gate 生效（初始策略） | — |

## 相关文档

- `EVAL-GATE-SPEC-2026-04-20.md`：指标口径、判定规则、CLI 用法
- `EVAL-COMPARISON-RULES-2026-04-18.md`：PASS/WARN/FAIL 对比判定规则
- `DEVELOPMENT.md` 第 11 节：开发侧门禁流程约定
