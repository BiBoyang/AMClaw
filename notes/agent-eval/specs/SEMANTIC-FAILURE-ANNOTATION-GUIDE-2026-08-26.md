# 语义失败标注指引（2026-08-26）

> 对应阶段：Phase 5（trace 驱动评测闭环）
> 目标：对真实 run 做人工标注，把 context/memory 语义失败从"感觉不对"变成可统计的二元标签。
> 方法论来源：`/Users/boyang/Desktop/trace+eval/trace-eval-v2.md`（假通过/假失败归因学、judge 校准纪律、小样本统计口径）。

---

## 1) 标注对象与分层

标注对象是 `data/agent_traces/` 下的真实 run（合成样本可标注但必须在 `source_type` 注明，统计时分层）。

标注分两层，顺序固定：

1. **先归假失败**：`not_agent_fault` —— 这次失败真的是 agent 的失败吗？
2. **再归语义失败**：`primary_semantic_failure` —— 若确为 agent 问题，主因是五类中的哪一类（或 none）。

理由：基础设施失败（LLM 401、网络超时）若被记入语义失败，就是在冤枉 agent（假失败）；这两层混在一起统计，结论必歪。

## 2) 第一层：假失败筛除（not_agent_fault）

判定为 `true` 的情形（对应 `EVAL-FAILURE-TAXONOMY-2026-04-18.md` 的 L1 运行时分类）：

| 情形 | trace 线索 |
|---|---|
| LLM 鉴权/传输失败 | `llm_calls[].error` 含 auth/transport 关键字 |
| 工具执行环境失败 | `tool_calls[].success = false` 且错误为环境性（路径、权限、网络） |
| 超时/外部限流 | run 以 timeout 收场或错误信息指向 provider 限流 |
| harness/评测系统自身问题 | trace 文件残缺、字段缺失导致无法判读 |

`not_agent_fault = true` 的样本：`primary_semantic_failure` 记 `n/a`，不计入语义失败分母，但**保留在标注文件中**（假失败率本身也是指标）。

## 3) 第二层：五类语义失败（二元主标签）

每条样本只选**一个主因**。判不下去标 `uncertain`，不许硬猜。

| 标签 | 定义 | 判定线索（先看什么字段） |
|---|---|---|
| `forgot_known_fact` | 事实就在注入的 context/memory 里，输出却与之矛盾或未使用 | `context_pack` 渲染内容 / `memory_ids` 注入内容 vs `final_output` |
| `missed_retrieval` | 该取的记忆没取到（DB 里有相关记忆，但 injected 里没有） | DB 中该 user 的 active 记忆 vs `memory_retrieved_count` / `memory_ids` |
| `wrong_retrieval` | 取到了，但取错了（注入了无关记忆，盖过或混淆了正确信息） | 注入记忆内容与 `user_input` 的相关性 |
| `state_drift` | session state 与对话实际进展脱节（goal/constraints/next_step 过期或错误） | `session_state_snapshot` / `final_runtime_session_state` 逐步对比 |
| `repeated_work` | 重复做已完成的事（重复调用、重复计划、返工无进展） | `tool_calls` 序列 / `replan_count` / step 间的重复模式 |
| `none` | 确认无语义失败 | — |
| `uncertain` | 证据不足，判不下去 | 必须在 evidence 写明缺什么 |

**易混边界**：

- `missed` vs `wrong`：DB 里**有**该取的没取到 → missed；取到了**不该取的**或取错对象 → wrong。两者同时发生，以"对最终结果伤害更大的"为主因，另一个写进 notes。
- `forgot_known_fact` vs `missed_retrieval`：事实**已在注入内容里**但没用上 → forgot；事实**不在注入内容里**（压根没检索到）→ missed。判这个边界需要看清渲染后 prompt 实际内容——若 trace 只有计数没有内容切片，标 `uncertain` 并记录字段缺口（这正是"eval 反向塑造 trace"的反馈点）。
- `state_drift` vs `repeated_work`：状态过期**导致**的重复劳动，主因记 `state_drift`；状态正确但行为层面重复 → `repeated_work`。

## 4) 标注格式纪律

1. **先依据后判定**：每条标注先写 `evidence`（引用 trace 具体字段和值），最后才写判定列。禁止先看输出拍脑袋再补理由。
2. **二元标签**：不搞 1–5 细粒度分。终点是二元判定，起点就该二元。
3. **报告带分母**：任何基于标注的结论必须写 `x/n`（如"语义失败率 3/17"），n < 20 时不下比例结论。
4. **标注指引本身会漂移**（criteria drift）：标注中发现新失败形态或边界不清时，先记在当批文件的 notes，批次结束时回本文件修订——标注集不是一次性资产。
5. 校准集与验证集分开：用于修订指引/调阈值的批次，不再作为后续机制变更的验证批次。

## 5) 标注文件

批次文件放 `notes/agent-eval/annotations/`，命名 `SEMANTIC-FAILURE-ANNOTATIONS-v<N>.md`，模板见 `SEMANTIC-FAILURE-ANNOTATIONS-v1.md`。

样本量目标：20 条起步（可下方向性结论），50 条达到可比较口径；其中真实 run 占比过低时结论只适用于合成分布，需注明。
