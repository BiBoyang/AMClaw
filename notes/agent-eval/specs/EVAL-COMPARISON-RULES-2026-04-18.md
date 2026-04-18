# Agent 评测对比判定规则（2026-04-18）

> 对应阶段：Phase 8 / Step 1.4
> 目的：把 "改动前/后对比" 从主观描述变成机械化判定，任意两份报告可得出统一结论。

---

## 1) 对比对象定义

### 1.1 什么构成一次可对比的 "before/after"

| 要素 | 定义 | 反例（不可对比） |
| --- | --- | --- |
| **基准样本集** | 同一份 `EVAL-BASELINE-SAMPLES-*.md` 中登记的 run_id 集合 | 两份报告使用不同 baseline 文件 |
| **对比维度** | 以下 7 个核心指标 + 3 个 L2 维度 | 只对比非标准字段 |
| **样本交集** | 两份报告中都出现的 baseline run_id 数量 >= 3 | 交集为 0 或 1 |
| **时间窗口** | 推荐同日内或相邻日，跨度 > 7 天需备注 | 跨版本大跨度无解释 |

### 1.2 对比维度清单（标准 7 + L2 3）

**核心 7 指标（必须对比）**：

1. `success_rate` — 成功率
2. `fallback_rate` — LLM fallback 发生率
3. `context_drop_rate` — ContextPack 丢弃率
4. `state_present_rate` — SessionState 携带率
5. `memory_injected_rate` — Memory 注入率
6. `recovery_success_rate` — 恢复成功率（有恢复样本时）
7. `unknown_failure_rate` — 未知失败占比

**L2 扩展 3 维度（可选，但有则必须对比）**：

8. `tool_success_rate` — 工具调用成功率
9. `planning_stall_rate` — 规划停滞率
10. `avg_step_count` — 平均步数偏移

### 1.3 指标取值规则

- 从 `TRACE-EVAL-REPORT.md` 的 **Summary Statistics** 小节读取
- 若某指标在当前报告中缺失，视为 `0` 并在结论中标注 `指标缺失`
- 比率统一按百分比点（pp）计算差值，例如 85% -> 80% = -5.0pp

---

## 2) 阈值定义（PASS / WARN / FAIL）

### 2.1 单指标变动阈值

对每一个对比维度，按以下规则判定：

| 指标 | 变动方向 | PASS（绿色） | WARN（黄色） | FAIL（红色） |
| --- | --- | --- | --- | --- |
| `success_rate` | 越高越好 | >= 前一轮 或 降幅 < 3pp | 降幅 3~5pp | 降幅 > 5pp |
| `fallback_rate` | 越低越好 | <= 前一轮 或 增幅 < 3pp | 增幅 3~5pp | 增幅 > 5pp |
| `context_drop_rate` | 越低越好 | <= 前一轮 或 增幅 < 3pp | 增幅 3~5pp | 增幅 > 5pp |
| `state_present_rate` | 越高越好 | >= 前一轮 或 降幅 < 5pp | 降幅 5~10pp | 降幅 > 10pp |
| `memory_injected_rate` | 越高越好 | >= 前一轮 或 降幅 < 5pp | 降幅 5~10pp | 降幅 > 10pp |
| `recovery_success_rate` | 越高越好 | >= 前一轮 或 降幅 < 10pp | 降幅 10~20pp | 降幅 > 20pp 或 < 60% |
| `unknown_failure_rate` | 越低越好 | <= 前一轮 或 增幅 < 2pp | 增幅 2~5pp | 增幅 > 5pp 或 > 10% |
| `tool_success_rate` | 越高越好 | >= 前一轮 或 降幅 < 3pp | 降幅 3~5pp | 降幅 > 5pp |
| `planning_stall_rate` | 越低越好 | <= 前一轮 或 增幅 < 2pp | 增幅 2~5pp | 增幅 > 5pp |
| `avg_step_count` | 越低越好 | <= 前一轮 或 增幅 < 0.5 步 | 增幅 0.5~1.0 步 | 增幅 > 1.0 步 |

### 2.2 综合结论判定

基于单指标结果，按以下规则得出整体结论：

| 条件 | 综合结论 | 含义 |
| --- | --- | --- |
| 全部核心指标 PASS，且无 FAIL | **PASS** | 改动无 regressions，可合并/发布 |
| 核心指标中 WARN >= 1 但 FAIL = 0 | **WARN** | 有波动需关注，建议 review 但不阻塞 |
| 任一核心指标 FAIL | **FAIL** | 存在明确 regression，必须修复后再合并 |
| `unknown_failure_rate` > 10% | **FAIL** | 即使其他指标正常，未知失败过多也需调查 |
| `success_rate` 降幅 > 5pp | **FAIL** | 成功率下降是硬门槛 |

### 2.3 特殊场景覆盖

**场景 A：新增样本导致分母变化**
- 规则：若 after 的 total_runs 比 before 增加 > 50%，单指标阈值放宽 2pp
- 原因：新样本可能引入新场景，短期波动可接受

**场景 B：某指标分母为 0**
- 例：before 无 recovery 样本，after 有 1 次 recovery 成功
- 规则：标记为 `N/A -> 有数据`，不纳入综合判定，单独备注

**场景 C：baseline 覆盖率下降**
- 规则：若 baseline_hit_rate 下降 > 20%，综合结论降级一档（PASS -> WARN，WARN -> FAIL）
- 原因：对比基础不可靠

---

## 3) 结论模板（可直接贴 session）

### 3.1 最小结论模板

```markdown
## 评测结论

- **对比窗口**: before={DATE_A} after={DATE_B}
- **样本数**: before={N} after={M}
- **baseline 覆盖**: before={X}% after={Y}%
- **综合判定**: {PASS / WARN / FAIL}

### 指标变动明细

| 指标 | before | after | 变动 | 判定 |
| --- | ---: | ---: | ---: | --- |
| success_rate | {val}% | {val}% | {delta}pp | {PASS/WARN/FAIL} |
| fallback_rate | {val}% | {val}% | {delta}pp | {PASS/WARN/FAIL} |
| context_drop_rate | {val}% | {val}% | {delta}pp | {PASS/WARN/FAIL} |
| ... | ... | ... | ... | ... |

### 判定依据

{根据哪条规则得出综合结论，简要说明}

### 后续动作

- {若 PASS：继续观察 / 若 WARN：关注 XXX 指标 / 若 FAIL：修复 XXX 后重跑}
```

### 3.2 完整结论模板（含 L2 维度）

```markdown
## 评测结论（完整版）

- **对比窗口**: before={DATE_A} after={DATE_B}
- **样本数**: before={N} after={M}（baseline 交集={K}）
- **综合判定**: {PASS / WARN / FAIL}
- **判定依据**: {引用 2.2 中的具体条件}

### 核心指标（7项）

| # | 指标 | before | after | 变动 | 判定 | 说明 |
| ---: | --- | ---: | ---: | ---: | --- | --- |
| 1 | success_rate | | | | | |
| 2 | fallback_rate | | | | | |
| 3 | context_drop_rate | | | | | |
| 4 | state_present_rate | | | | | |
| 5 | memory_injected_rate | | | | | |
| 6 | recovery_success_rate | | | | | |
| 7 | unknown_failure_rate | | | | | |

**核心指标统计**: PASS={n} WARN={n} FAIL={n}

### L2 扩展指标（3项）

| # | 指标 | before | after | 变动 | 判定 | 说明 |
| ---: | --- | ---: | ---: | ---: | --- | --- |
| 8 | tool_success_rate | | | | | |
| 9 | planning_stall_rate | | | | | |
| 10 | avg_step_count | | | | | |

### 失败类型变化

| failure_type | before_count | after_count | 变化 | 关注 |
| --- | ---: | ---: | ---: | --- |
| {type} | | | | |

### 需要人工 review 的 trace

| run_id | 原因 | 建议动作 |
| --- | --- | --- |
| | | |

### 后续动作

1. {具体行动项}
2. {具体行动项}
```

---

## 4) 机械化判定流程（checklist）

执行对比时按以下顺序检查：

- [ ] 1. 确认两份报告使用同一份 baseline 文件
- [ ] 2. 确认 baseline 交集 >= 3 条
- [ ] 3. 提取 7 个核心指标 before/after 值
- [ ] 4. 对每项指标应用 2.1 阈值，得出 PASS/WARN/FAIL
- [ ] 5. 统计核心指标中 FAIL 数量
- [ ] 6. 检查特殊场景 A/B/C 是否触发
- [ ] 7. 按 2.2 综合规则得出整体结论
- [ ] 8. 填充结论模板并输出

> 以上 8 步全部可脚本化。理想状态：trace_eval 支持 `--compare` 参数直接输出结论。

---

## 5) 与现有文档的衔接

| 文档 | 角色 |
| --- | --- |
| `EVAL-METRICS-SPEC-2026-04-18.md` | 指标定义（本文档引用其指标口径） |
| `EVAL-FAILURE-TAXONOMY-2026-04-18.md` | 失败分类（本文档引用其类型做变化对比） |
| `EVAL-BASELINE-SAMPLES-2026-04-18.md` | 样本基线（本文档要求对比使用同一份） |
| `TRACE-EVAL-REPORT.md` | 报告实例（本文档的输入数据源） |

---

## 6) 版本记录

- v1.0（2026-04-18）：初始规则，覆盖核心 7 指标 + L2 3 维度 + 3 种特殊场景
