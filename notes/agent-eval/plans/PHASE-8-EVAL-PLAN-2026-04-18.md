# Phase 8：Agent 评测计划（2026-04-18）

> 主线：`8. Agent 评测`  
> 目标：把评测从“看日志感觉”升级为“有口径、有样本、有日报、有对比”的稳定流程。

---

## 1) 范围冻结（Step 0）

### In Scope

1. 基于现有 Trace 的离线评测（`src/bin/trace_eval.rs`）。
2. 固定评测指标口径（success/fallback/context drop/state/memory/recovery）。
3. 固定失败分类口径（failure taxonomy）。
4. 固定样本基线集（后续改动前后对比）。
5. 每日评测报告产出（Markdown）。

### Out of Scope

1. 在线 A/B 实验平台。
2. 大规模统计建模与数据仓库。
3. 新增重型观测基础设施。
4. 新一轮 Context / Memory 机制扩展。

---

## 2) 执行步骤（Step 0~8）

1. **Step 0**：范围冻结（本文档）
2. **Step 1**：指标口径文档  
   - 产出：`EVAL-METRICS-SPEC-2026-04-18.md`
3. **Step 2**：失败分类字典  
   - 产出：`EVAL-FAILURE-TAXONOMY-2026-04-18.md`
4. **Step 3**：样本基线集  
   - 产出：`EVAL-BASELINE-SAMPLES-2026-04-18.md`
5. **Step 4**：升级 `trace_eval` 输出
6. **Step 5**：改动前后对比规则  
   - 产出：`EVAL-COMPARISON-RULES-2026-04-18.md`
7. **Step 6**：接入收尾流程（评测成为常规动作）
8. **Step 7**：补评测字段回归测试
9. **Step 8**：阶段收尾总结  
   - 产出：`PHASE-8-EVAL-SUMMARY-2026-04-XX.md`

---

## 3) 两周节奏建议

### Week 1（基线搭建）

- Day 1：Step 1（指标口径）
- Day 2：Step 2（失败分类）
- Day 3：Step 3（样本基线）
- Day 4：Step 4（trace_eval 报告能力）
- Day 5：Week 1 总结与校正

### Week 2（闭环落地）

- Day 1：Step 5（对比规则）
- Day 2：Step 6（接入收尾流程）
- Day 3：Step 7（回归测试）
- Day 4：跑一轮完整对比评测
- Day 5：Step 8（阶段收尾报告）

---

## 4) 验收标准（DoD）

1. 有固定指标口径文档。
2. 有固定失败分类口径文档。
3. 有固定样本基线集。
4. `trace_eval` 能一键产生日报。
5. 可执行“改动前/后”对比并产出结论。
6. 收尾流程里包含评测动作与摘要记录。
