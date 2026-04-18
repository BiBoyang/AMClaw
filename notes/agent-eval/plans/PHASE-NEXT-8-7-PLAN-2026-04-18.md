# 下一阶段主线：8（Agent 评测）+ 7（错误恢复闭环）两周执行计划

> 周期：2 周  
> 目标：把 AMClaw 从“上下文可解释”推进到“问题可定位、失败可恢复、改动可验证”

---

## 0. 选型结论

本阶段主线选择：

1. `8. Agent 评测`
2. `7. 错误恢复与反馈闭环`

不作为主线（本阶段不优先）：

- 继续扩 Context 功能
- 继续扩 Memory 类型
- Multi-Agent 设计

---

## 1. 两周目标（可量化）

1. 形成最小评测集（至少 20 条 trace 样本）
2. 形成失败分类清单（至少 6 类）
3. 每类失败有恢复动作（retry/replan/ask_user/fallback）
4. 形成一份“每日评测摘要”可读报告

---

## 2. Week 1：评测基线搭建（偏 8）

### Day 1：样本基线

- 从 `data/agent_traces/<date>` 抽取样本（成功/失败/降级混合）
- 生成统一样本表：run_id、source_type、失败类型、恢复结果
- 产出：`notes/agent-eval/baselines/EVAL-BASELINE-SAMPLES-2026-04-XX.md`

### Day 2：指标口径冻结

- 固定最小指标：
  - success_rate
  - fallback_rate
  - context_drop_rate
  - recovery_success_rate
- 产出：`notes/context-memory/EVAL-METRICS-SPEC-2026-04-XX.md`

### Day 3：评测脚本收口

- 用 `trace_eval` 固化报告模板（概览 + 分项 + interesting traces）
- 确保每日可一键运行并输出报告
- 产出：日报文件（按日期）

### Day 4：回归场景扩展

- 固化 2 条关键回归：
  - 有 state + 有 memory
  - 无 state + 无 memory
- 每次改动都跑并记录结果

### Day 5：周总结

- 汇总本周指标与主要失败类型
- 产出：`notes/context-memory/WEEK1-EVAL-SUMMARY-2026-04-XX.md`

---

## 3. Week 2：恢复闭环落地（偏 7）

### Day 1：失败分类与恢复动作映射

- 建立映射表：
  - failure_type -> default recovery action
- 产出：`notes/context-memory/FAILURE-RECOVERY-MAP-2026-04-XX.md`

### Day 2：高频失败优先处理

- 选 Top 2 失败类型，补默认恢复策略
- 确保不阻断主流程，失败时可回退

### Day 3：反馈闭环

- 把恢复结果写入 trace 可观测字段（success/fail/reason）
- 报告中增加“恢复成功率”段落

### Day 4：回归验证

- 运行全量校验（fmt/check/clippy/test）
- 运行评测日报并和 Week1 对照

### Day 5：阶段收尾

- 输出阶段报告：做了什么、改进了什么、仍未解决什么
- 更新 `CONTEXT-TECH-SELECTION-JOURNEY.md` 新增一节“8+7 阶段”

---

## 4. 风险与约束

1. 一次只改一个恢复机制，避免无法归因
2. 不引入新主线（如 embedding / multi-agent）
3. 所有变更必须可在 trace 中解释

---

## 5. 验收标准（DoD）

1. 两周内每日有评测摘要
2. 至少 2 类高频失败具备可验证恢复动作
3. 关键指标可横向对比（Week1 vs Week2）
4. 文档、代码、trace 三者口径一致
