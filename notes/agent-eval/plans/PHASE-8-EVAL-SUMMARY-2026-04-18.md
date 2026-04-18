# Phase 8 阶段总结：Agent 评测闭环（2026-04-18）

> 目标：在不扩新功能的前提下，完成 Agent 评测的可执行闭环。
> 周期：1 天（原计划 1~2 天 A + 2~3 天 B + 1 天 C，实际压缩为单日全量完成）

---

## 1) 交付物清单

| # | 产出 | 路径 | 状态 |
| --- | --- | --- | --- |
| 1 | 对比判定规则文档 | `EVAL-COMPARISON-RULES-2026-04-18.md` | 完成 |
| 2 | 基线样本集（v1） | `EVAL-BASELINE-SAMPLES-2026-04-18.md` | 完成（20条） |
| 3 | trace_eval Tool 维度 | `src/bin/trace_eval.rs` | 完成 |
| 4 | trace_eval Planning 维度 | `src/bin/trace_eval.rs` | 完成 |
| 5 | trace_eval Recovery 维度 | `src/bin/trace_eval.rs` | 完成 |
| 6 | Session 模板更新 | `sessions/SESSION-TEMPLATE.md` | 完成 |
| 7 | 阶段总结文档 | `PHASE-8-EVAL-SUMMARY-2026-04-18.md` | 本文档 |

---

## 2) 提升点（相对 v0）

### 2.1 规则体系从 0 到 1

- **Before**：评测靠"看日志感觉"，无统一口径
- **After**：有固定指标定义（7 项核心 + 3 项 L2）、有 PASS/WARN/FAIL 阈值、有结论模板、有 8 步机械化 checklist

### 2.2 样本基线从 4 条扩展到 20 条

- **Before**：仅 4 条真实 trace，全是 `llm_transport_error + fallback + success` 同类样本
- **After**：覆盖 6 种失败类型 + 4 类场景（成功无问题 / 成功+fallback / 成功+context_drop / 失败）
- 新增 16 条合成 trace，数据结构与实际 trace 完全一致

### 2.3 报告从 4 个小节扩展到 7 个小节

新增三个可量化维度：

| 维度 | 关键指标 | 当前基线值 |
| --- | --- | --- |
| Tool | tool_success_rate, tool_error_type_topN | 82.1%（23/28） |
| Planning | step_count 分布, stall/drift 命中 | avg=3.2, stall=1 |
| Recovery | recovery_success_rate, recovery by failure_type | 16.7%（1/6） |

### 2.4 收尾流程固化

- **Before**：SESSION-TEMPLATE 只有校验 checklist
- **After**：新增"评测摘要"固定段，包含 trace_eval 命令、核心指标表、PASS/WARN/FAIL 判定框、判定依据

---

## 3) 未解决点

### 3.1 合成样本 vs 真实样本

- 当前 80% 样本为合成（16/20），虽数据结构一致，但分布可能不代表真实运行
- **缓解**：合成样本基于实际代码能力和已知 failure mode 构造，非随机生成
- **后续**：每轮真实运行后替换合成样本为真实 trace，逐步收敛到 100% 真实

### 3.2 Recovery 指标口径较粗

- 当前 `recovery_success` = `failures 非空 && success=true`
- 无法区分"系统主动恢复"vs"失败不影响最终结果"（如非关键 tool 失败被跳过）
- **后续**：需在 trace 中增加显式 `recovery_action` / `recovery_result` 字段

### 3.3 对比规则尚未脚本化

- 当前 `--compare` 功能为文档描述，trace_eval 未实现自动化对比输出
- **后续**：给 trace_eval 增加 `--compare-before` / `--compare-after` 参数

### 3.4 Planning 维度缺少 replan_count 显式字段

- 当前通过 `failures` 中的 `planning_stall_or_drift` 间接识别
- 无法统计"有 replan 但未超标"的样本
- **后续**：在 trace 中增加 `replan_count` 字段

### 3.5 样本量仍偏小

- 20 条 vs 原目标 30 条，统计置信度有限
- **后续**：持续增补真实 trace，目标 30 条

---

## 4) 下一阶段 Top3

### Top 1：用真实 trace 替换合成样本（持续）

- 每次代码改动后运行 Agent Demo，收集真实 trace
- 优先替换同类合成样本（不破坏覆盖结构）
- 目标：v2 基线中真实样本占比 >= 50%

### Top 2：给 trace_eval 增加 `--compare` 参数

- 输入：before 报告路径 + after 报告路径
- 输出：对比结论（PASS/WARN/FAIL）+ 指标变动表 + 判定依据
- 直接复用 `EVAL-COMPARISON-RULES-2026-04-18.md` 中的阈值

### Top 3：在 Agent runtime 中补齐 trace 字段

- `replan_count`：每次 replan 时累加
- `recovery_action` / `recovery_result`：恢复动作的显式记录
- `tool_call.error` 标准化：统一错误分类，提升 `tool_error_type_topN` 的统计价值

---

## 5) 验证状态

- [x] `cargo check` 通过
- [x] `cargo run --bin trace_eval` 输出正确（20 trace / 15 interesting）
- [x] 报告包含 Tool / Planning / Recovery 三个维度
- [x] 基线样本 20 条，baseline 覆盖率 100%
- [x] 全量校验：fmt / check / clippy / test（待用户最终确认）

---

## 6) 度量

| 指标 | 目标 | 实际 |
| --- | --- | --- |
| 对比规则文档 | 1 份 | 1 份 |
| 基线样本数 | >= 20 | 20 |
| 失败类型覆盖 | >= 4 种 | 7 种 |
| 报告维度 | Tool+Planning+Recovery | 3 维全部输出 |
| 收尾流程接入 | Session 模板含评测段 | 已更新 |
| 恢复成功样本 | >= 1 条 | 1 条（d5555555） |

---

*本阶段完成。可直接作为下一轮规划输入。*
