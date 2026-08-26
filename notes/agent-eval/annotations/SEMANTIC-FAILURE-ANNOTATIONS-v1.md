# 语义失败标注 · 批次 v1

> 标注指引：`../specs/SEMANTIC-FAILURE-ANNOTATION-GUIDE-2026-08-26.md`
> 状态：待积累真实样本后启动（当前真实 run 约 5 条，目标 20+ 条）
> 标注日期：（填）｜ 标注人：（填）

## 标注表

| run_id | trace_file | source_type | not_agent_fault | primary_semantic_failure | notes |
| --- | --- | --- | --- | --- | --- |
| （例）`xxx` | `data/agent_traces/YYYY-MM-DD/run_....json` | real / synthetic | true / false | 五类 / none / n/a / uncertain | |

## 逐条依据（先依据后判定，判定列回填上表）

### run_id: （填）

- **evidence**：（引用 trace 具体字段与值，如 `memory_ids=[...]` 注入内容 vs `final_output` 的矛盾点）
- **判定**：（not_agent_fault / primary_semantic_failure）

---

## 批次小结（标注满 20 条后填写）

- 总样本：n = ｜ 真实 / 合成 = /
- 假失败（not_agent_fault=true）：x/n =
- 语义失败分布：forgot_known_fact / missed_retrieval / wrong_retrieval / state_drift / repeated_work / none / uncertain
- 发现的指引缺口 / 新失败形态（回馈指引修订）：
- 发现的 trace 字段缺口（回馈 trace 补字段）：
