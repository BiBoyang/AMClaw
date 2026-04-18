# 从“看日志感觉”到“可复现闭环”：AMClaw Agent 评测体系 Phase 8 技术复盘（2026-04-18）

> 仓库路径：`/Users/boyang/Desktop/AMClaw`  
> 阶段目标：不扩新功能，先把 Agent 评测流程工程化。  
> 关键词：指标口径、失败分类、基线样本、自动报告、收尾接入。

---

## 1. 背景：为什么这一轮要先做评测闭环

在连续迭代 Agent runtime 的过程中，最容易出现的问题不是“没有功能”，而是“没有稳定评估方式”。  
之前的评估方式更偏人工经验：跑几条 case、看日志、判断“这版看起来可以”。这种方式在单点调试时效率很高，但随着改动变多，很快会暴露三个系统性问题：

1. **指标口径不一致**：同样叫“成功率”，不同人、不同轮次统计方式可能不同。  
2. **样本不可对比**：本轮和上轮拿的样本集合不同，变化无法直接归因。  
3. **收尾不可复盘**：改完代码虽然能跑，但缺少结构化评测摘要，几天后回看很难判断“到底哪一步带来了提升或退化”。

因此，Phase 8 的定位不是功能扩展，而是评测体系建设：把“看感觉”变成“可复现、可对比、可沉淀”。

---

## 2. 本轮交付：把评测流程拆成 5 个可执行模块

### 2.1 指标口径固定

文档：`notes/agent-eval/specs/EVAL-METRICS-SPEC-2026-04-18.md`  
作用：明确核心指标的定义、字段来源、计算方式与缺失字段处理策略。

核心口径包含：
- `success_rate`
- `fallback_rate`
- `context_drop_rate`
- `state_present_rate`
- `memory_injected_rate`
- `recovery_success_rate`

这样做的目的，是让每一轮报告在“语义层”保持一致，避免术语漂移。

### 2.2 失败分类固定

文档：`notes/agent-eval/specs/EVAL-FAILURE-TAXONOMY-2026-04-18.md`  
作用：把失败从“散乱错误文本”映射为可统计的分类体系。

示例类型：
- `llm_auth_error`
- `llm_transport_error`
- `tool_call_error`
- `planning_stall_or_drift`
- `fallback_exhausted`
- `unknown_failure`

这一步是后续“趋势分析”和“恢复策略改进”的前提。

### 2.3 基线样本固定

文档：`notes/agent-eval/baselines/EVAL-BASELINE-SAMPLES-2026-04-18.md`  
作用：建立一组固定 run_id 样本池，作为改动前后比较的共同参照。

当前 v1 基线：
- 总样本：20 条
- 真实 trace：4 条（2026-04-01）
- 合成 trace：16 条（2026-04-18）
- 覆盖：成功样本、fallback、context drop、失败与恢复场景

这让“对比”从主观描述升级为可重复计算。

### 2.4 报告自动生成

代码：`src/bin/trace_eval.rs`  
输出：`notes/agent-eval/reports/TRACE-EVAL-REPORT.md`

相比早期版本，报告新增三块核心维度：
- **Tool**：工具成功率、错误类型 TopN、调用分布
- **Planning**：步骤分布、stall/drift 命中、unfinished 统计
- **Recovery**：恢复尝试、恢复成功率、按失败类型拆分

### 2.5 收尾流程接入

模板：`sessions/SESSION-TEMPLATE.md`  
新增固定段：评测摘要（命令、报告路径、核心指标、判定结论、判定依据）。

效果是：评测从“可选动作”变成“收尾标准动作”。

---

## 3. 可复现执行路径（命令 + 路径）

### 3.1 基础校验

```bash
cd /Users/boyang/Desktop/AMClaw
cargo fmt --check
cargo check
cargo clippy --all-targets --all-features
cargo test
```

本轮结果：通过（`246 passed`）。

### 3.2 生成 Agent 评测报告

```bash
cd /Users/boyang/Desktop/AMClaw
cargo run --bin trace_eval
```

默认输出：
- 报告：`notes/agent-eval/reports/TRACE-EVAL-REPORT.md`
- baseline：`notes/agent-eval/baselines/EVAL-BASELINE-SAMPLES-2026-04-18.md`

### 3.3 常用参数

```bash
# 指定日期窗口
cargo run --bin trace_eval -- --date 2026-04-18

# 指定 trace 根目录
cargo run --bin trace_eval -- --dir data/agent_traces

# 指定输出路径
cargo run --bin trace_eval -- --output notes/agent-eval/reports/TRACE-EVAL-REPORT.md

# 指定 baseline 文件
cargo run --bin trace_eval -- --baseline notes/agent-eval/baselines/EVAL-BASELINE-SAMPLES-2026-04-18.md

# 不使用 baseline
cargo run --bin trace_eval -- --no-baseline

# 仅输出 interesting traces
cargo run --bin trace_eval -- --only-interesting
```

---

## 4. 本轮观测结果（2026-04-18 基线）

来自 `notes/agent-eval/reports/TRACE-EVAL-REPORT.md` 的关键指标：

- `success_rate`: **75%**（15/20）
- `fallback_rate`: **35%**（7/20）
- `context_drop_rate`: **15%**（3/20）
- `tool_success_rate`: **82.1%**（23/28）
- `recovery_success_rate`: **16.7%**（1/6）
- `stall_or_drift`: **1**

这些指标不追求“看起来完美”，而是强调三点：
1. 计算口径固定；
2. 样本可追溯；
3. 结果可重跑。

这三个条件同时满足，才能把后续优化建立在稳定证据上。

---

## 5. 这轮成果的本质：从日志可见性走向评测可决策

这一轮最重要的变化并非某个具体数字，而是流程能力的升级。可以把它理解成四层：

1. **日志层（Log）**：记录发生了什么（原始 trace）。  
2. **归因层（Attribution）**：把失败映射成标准分类（taxonomy）。  
3. **评测层（Evaluation）**：把单条事件聚合成可比较指标。  
4. **决策层（Decision）**：基于阈值做 PASS/WARN/FAIL 判断并驱动下一步动作。

Phase 8 的交付，覆盖了这四层中的 2~4 层，并把 1 层已有能力（trace）有效接入。  
换句话说，它不是“只做日志”，也不是“只做错误归因”，而是构建了一个**最小可运行的评测决策闭环**。

---

## 6. 当前不足与下一步计划

### 6.1 已知不足

1. 真实样本占比仍偏低（4/20）。  
2. `recovery_success_rate` 仍使用临时代理口径（`failures 非空 && success=true`）。  
3. 对比规则已文档化，但自动化 `--compare` 仍待落地。

### 6.2 下一步优先级（建议）

**P0**
1. 在 `trace_eval` 增加 `--compare`，直接输出 PASS/WARN/FAIL。  
2. 补两条针对统计边界的回归测试（0 分母比率、主失败类型稳定性）。

**P1**
3. 在 trace 中新增 `replan_count`、`recovery_action`、`recovery_result` 字段。  
4. baseline 扩至 30 条并提升真实样本占比到 >=50%。

**P2**
5. 增加一份 before/after 的完整对比样例，沉淀为固定复盘模板。

---

## 7. 结语

如果用一句话总结这次阶段工作：  
**我们把“评测”从临时观察动作，改造成了可复现的工程流程。**

后续每次改动，不再只是“改完能跑”，而是可以明确回答：
- 指标有没有退化？
- 退化是否超过阈值？
- 是否允许合并？
- 下一步应该补哪一类能力？

这正是 Agent 项目从“能工作”走向“可演进”的关键分水岭。

