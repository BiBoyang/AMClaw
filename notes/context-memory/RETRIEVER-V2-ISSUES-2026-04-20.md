# Retriever v2 Issue 拆分清单（2026-04-20）

## 说明

本清单基于 `RETRIEVER-SEMANTIC-V2-PLAN-2026-04-20.md`，用于把语义检索 v2 拆成可并行、可验收的工程任务。

默认原则：

1. 优先保证 `rule_v1` 可回退。
2. 优先保证可观测，再追求召回上限。
3. 每个 issue 必须具备明确 DoD（Definition of Done）。

---

## ISSUE-01 配置与模式接线（P0）

**标题建议**
`feat(retriever): add retrieval mode config and runtime wiring`

**目标**
增加 `retriever.mode` 配置，支持 `rule | semantic | hybrid | shadow` 四种模式，并完成 runtime 接线。

**范围**

- `config` 增加 retriever 配置结构。
- `agent_core` 初始化 retriever 时按 mode 选择实现。
- 默认值保持 `rule`。

**DoD**

- 默认配置下行为与当前一致（零回归）。
- 错误配置有明确报错或回退策略。
- 相关单元测试通过。

**依赖**
无。

---

## ISSUE-02 EmbeddingProvider 抽象（P0）

**标题建议**
`feat(retriever): introduce embedding provider abstraction`

**目标**
新增 embedding provider 抽象，支持后续语义检索接入，同时保持关闭语义时无额外外部依赖。

**范围**

- 新增 `EmbeddingProvider` trait。
- 新增 `NoOpEmbeddingProvider`。
- provider 初始化失败时可观测并可回退。

**DoD**

- 语义关闭时不触发网络依赖。
- provider 错误路径有测试覆盖。
- 不影响现有 `rule_v1` 检索结果。

**依赖**
建议在 ISSUE-01 之后。

---

## ISSUE-03 HybridRetriever 基础实现（P0）

**标题建议**
`feat(retriever): add hybrid retriever with coarse+semantic rerank`

**目标**
实现最小可用 `HybridRetriever`：规则法粗召回 + 语义打分 + 混合重排。

**范围**

- coarse 候选来自规则法（用户隔离不变）。
- 增加 `rule_score`、`semantic_score`、`final_score`。
- 输出 metadata 带 `retrieval_mode` 与 `embedding_model`。

**DoD**

- 排序稳定、结果可复现。
- 召回数量 obey `limit`。
- 失败时可自动回退 `rule_v1` 并记录原因。

**依赖**
ISSUE-02。

---

## ISSUE-04 AgentCore 查询输入增强（P1）

**标题建议**
`feat(agent_core): enrich retrieve query with semantic inputs`

**目标**
在 `agent_core` 构造 `RetrieveQuery` 时补齐语义检索需要的输入。

**范围**

- 填充 `query_text`（来自用户输入/上下文摘要）。
- 填充 run 级 hints（如 step_count、has_current_task）。
- query 缺失关键字段时走 rule fallback。

**DoD**

- semantic/hybrid 模式下 query 参数可用。
- fallback 原因可观察（日志/trace）。
- 不改变 memory budget 与 feedback 责任边界。

**依赖**
ISSUE-01、ISSUE-03。

---

## ISSUE-05 Trace 可观测字段扩展（P1）

**标题建议**
`feat(agent_trace): add retrieval mode and fallback observability`

**目标**
扩展 trace 检索观测字段，确保线上问题可定位。

**范围**

- 新增（建议）：
  - `retrieval_mode`
  - `retrieval_fallback_reason`
  - `retrieval_scores_present`
- markdown / json 双通道展示。

**DoD**

- 新字段对旧 trace 消费方兼容。
- 至少覆盖：正常路径、fallback 路径、无检索路径。

**依赖**
ISSUE-03、ISSUE-04。

---

## ISSUE-06 trace_eval 指标扩展（P1）

**标题建议**
`feat(trace_eval): extend retriever statistics with latency and fallback metrics`

**目标**
让 `trace_eval` 能评估 v2 检索效果，而不只看平均值。

**范围**

- 增加按 retriever 的：
  - `p50/p95 latency`
  - fallback rate
  - candidate->hit 转化率
- 保留对旧字段缺失 trace 的兼容解析。

**DoD**

- 报告可同时处理旧/新 trace。
- 新增统计有测试覆盖。

**依赖**
ISSUE-05。

---

## ISSUE-07 Embedding 缓存持久化（P2，可选）

**标题建议**
`feat(task_store): add embedding cache storage for memory retrieval`

**目标**
降低语义检索延迟与调用成本，支持 embedding 缓存。

**范围**

- `task_store` 增 embedding 缓存表（单独 migration）。
- 提供读写接口与 model_version 字段。
- 失效策略（惰性更新或批量重建）给出最小实现。

**DoD**

- 缓存命中时延迟明显下降（以本地对比为准）。
- schema 变更有迁移测试与回滚策略说明。
- provider 失败不影响 rule fallback。

**依赖**
ISSUE-03（可并行准备）。

---

## ISSUE-08 灰度与回归策略（P0）

**标题建议**
`chore(retriever): add shadow rollout and regression guardrails`

**目标**
保证 v2 上线过程可灰度、可止损。

**范围**

- `shadow` 模式对线上输出保持 `rule`，仅记录语义结果。
- 增加白名单或环境开关（开发环境先行）。
- 增加端到端回归脚本/用例（至少覆盖 rule vs shadow 一致性）。

**DoD**

- shadow 模式下线上行为与 rule 保持一致。
- 异常时可一键回切 `rule`。
- 回归结果可在 session 收尾中复用。

**依赖**
ISSUE-01 到 ISSUE-06。

---

## 推荐执行顺序

主线：

1. ISSUE-01
2. ISSUE-02
3. ISSUE-03
4. ISSUE-04
5. ISSUE-05
6. ISSUE-06
7. ISSUE-08

可选并行：

- ISSUE-07（在 ISSUE-03 之后并行）

---

## 验收门槛（v2 阶段性）

在进入默认 `hybrid` 前，至少满足：

1. `shadow` 连续稳定运行（无正确性回归）。
2. `trace_eval` 可输出 retriever 维度差异报告。
3. fallback rate 与 latency 在可接受范围内（阈值另行冻结）。
4. 保留并验证 `rule` 一键回退路径。
