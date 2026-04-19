# AMClaw Retriever Semantic v2 设计草案（2026-04-20）

## 1. 背景与现状

截至 `2026-04-20`，`AMClaw` 已完成检索抽象第一步：

- `src/retriever/mod.rs` 已定义统一契约：
  - `RetrieveQuery`
  - `RetrievedItem`
  - `RetrieveResult`
  - `Retriever` trait
- `src/retriever/rule.rs` 提供默认实现 `RuleRetriever`（等价现有 `task_store.search_user_memories` 排序逻辑）。
- `agent_core` 已通过 retriever 接口取回候选，预算裁剪与 feedback 回写仍在调用方统一处理。
- trace / trace_eval 已有最小 retriever 可观测：
  - `retriever_name`
  - `retrieval_candidate_count`
  - `retrieval_hit_count`
  - `retrieval_latency_ms`

这意味着现在已经有“可插拔检索位”，下一步可以在不破坏主链路的前提下引入语义检索。

---

## 2. v2 目标（In Scope）

v2 目标不是“一步到位最强检索”，而是把语义检索变成一个**可灰度、可回退、可评测**的能力。

### 2.1 业务目标

1. 提升“文本不完全重合”情况下的记忆召回能力。
2. 在不降低稳定性的前提下，逐步提高注入命中质量。
3. 保持用户隔离与现有写侧治理规则不变。

### 2.2 工程目标

1. 继续维持 `Retriever trait` 为唯一检索入口。
2. 默认链路保持 `rule_v1` 可用，语义失败可自动降级。
3. 新增语义信息必须可观测（trace + eval）。

---

## 3. v2 非目标（Out of Scope）

1. 不在 v2 里引入复杂外部向量服务强依赖（除非显式配置开启）。
2. 不在 retriever 层做预算裁剪、feedback 回写、prompt 拼装。
3. 不做多租户跨库检索，继续维持单用户隔离语义。
4. 不一次性引入“图谱 + 多跳检索 + reranker 大模型”。

---

## 4. 目标架构（推荐）

## 4.1 检索模式

新增配置位（建议放 `config.agent.retriever`）：

- `mode = "rule" | "semantic" | "hybrid" | "shadow"`

语义解释：

1. `rule`：仅规则法（当前默认）。
2. `semantic`：仅语义分数排序（仍受 user_id 过滤与上层 budget 约束）。
3. `hybrid`：规则 + 语义混合打分。
4. `shadow`：对线上返回仍用 `rule`，同时计算语义结果并仅写日志/trace，用于离线比较。

## 4.2 候选管线（Hybrid v1）

建议采用“两段式”：

1. **Coarse Retrieval（规则法召回）**
   - 先从 `task_store` 取 `N` 条（如 30）候选，保持当前稳定排序与用户隔离。
2. **Semantic Scoring**
   - 用 query embedding 与候选 embedding 算相似度（cosine）。
3. **Hybrid Re-rank**
   - `final_score = α * semantic_score + (1 - α) * rule_score_norm`
4. **Top-K 输出**
   - 输出 `limit` 条 `RetrievedItem`，并在 `metadata` 标注各分数与来源。

这样可以避免“全量库向量检索”的早期复杂度，也降低 schema 与运维风险。

## 4.3 Embedding Provider 抽象（建议）

建议在 `src/retriever/` 下新增 provider 抽象（名称可调整）：

- `EmbeddingProvider` trait
  - `embed_query(text) -> Vec<f32>`
  - `embed_documents(texts) -> Vec<Vec<f32>>`
  - `model_name()`

实现策略：

1. `NoOpEmbeddingProvider`：用于关闭语义模式时的空实现。
2. `ConfiguredHttpEmbeddingProvider`：仅在显式配置开启时可用。
3. provider 异常时 retriever 自动降级到 `rule_v1`。

---

## 5. 数据与契约演进

## 5.1 `RetrieveQuery` 建议扩展字段（向后兼容）

建议新增可选字段（不破坏现有调用）：

- `query_text: Option<String>`（已存在，v2 作为语义输入主字段）
- `preferred_memory_types: Option<Vec<String>>`
- `trace_run_id: Option<String>`（仅用于观测关联）

要求：

- 若 `query_text` 为空，`semantic/hybrid` 自动回退 `rule` 路径并写原因。

## 5.2 `RetrievedItem.metadata` 建议标准键

建议统一保留以下键（字符串值）：

- `memory_type`
- `priority`
- `status`
- `use_count`
- `retrieved_count`
- `injected_count`
- `last_used_at`
- `created_at`
- `updated_at`
- `rule_score`
- `semantic_score`
- `final_score`
- `retrieval_mode`
- `embedding_model`

说明：

- `agent_core` 的映射逻辑只依赖基础字段（上面前 9 个），后续字段仅用于 observability / eval。

## 5.3 存储层建议（由 task_store 负责，不在 retriever 直接改 schema）

可选方案（按复杂度从低到高）：

1. **v2.1（最小）**：不落库 embedding，每次请求对候选即时 embedding（低复杂度，高延迟）。
2. **v2.2（推荐）**：新增 memory embedding 表并缓存向量（中复杂度，延迟稳定）。
3. **v2.3（后续）**：支持 embedding model 版本并行，分批重建索引。

---

## 6. 可观测与评测

## 6.1 Trace 字段（在现有基础上补充）

当前已有：

- `retriever_name`
- `retrieval_candidate_count`
- `retrieval_hit_count`
- `retrieval_latency_ms`

建议后续新增（可选）：

- `retrieval_mode`（rule/semantic/hybrid/shadow）
- `retrieval_fallback_reason`（如 embedding_timeout）
- `retrieval_scores_present`（bool）

## 6.2 trace_eval 评测维度（建议）

在现有 `Retriever Statistics` 上新增：

1. 按 `retriever_name` 的 `p50/p95 latency`。
2. `candidate->hit` 转化率。
3. `fallback_rate`（语义模式下回退 rule 的比例）。
4. `useful_confirmation_rate`（需结合 memory feedback）。

---

## 7. 逐步落地计划（建议）

## Phase A：Shadow 基建（低风险）

目标：

- 增加 `semantic/shadow` 配置位与 provider 抽象。
- `shadow` 模式只记录语义排序结果，不影响线上注入内容。

DoD：

- 线上行为与 `rule_v1` 一致。
- trace 能看到 shadow 统计与 fallback 原因。

## Phase B：Hybrid 受控放量

目标：

- 在 `hybrid` 模式下启用混合排序并小流量使用（先开发环境 / 单用户白名单）。

DoD：

- 无正确性回归（test + 手工回归）。
- `retrieval_latency_ms` 在可接受阈值内（阈值待定，如 < 150ms）。

## Phase C：语义持久化与版本治理

目标：

- 引入 embedding 缓存与模型版本管理。
- 支持后台重建或惰性更新策略。

DoD：

- 模型切换时可平滑回退。
- trace_eval 可对比不同模型版本表现。

---

## 8. 风险清单与缓解

1. **延迟上升**
   - 缓解：先 coarse 后 semantic；增加缓存；设置超时并回退。
2. **召回噪声增加**
   - 缓解：保持规则先验参与打分；延续写侧治理与 memory 类型权重。
3. **模型漂移导致不稳定**
   - 缓解：`shadow` 先观测，再灰度；保留 `rule` 一键回退。
4. **字段契约漂移**
   - 缓解：固定 metadata 基础键集合；加契约测试。

---

## 9. 最小任务拆分（可直接建 issue）

1. `retriever`: 增加 `RetrievalMode` 与配置映射。
2. `retriever`: 增加 `EmbeddingProvider` trait + `NoOp` 实现。
3. `retriever`: 新增 `HybridRetriever`（coarse + semantic + rerank）。
4. `agent_core`: 在 query 中补 `query_text` 与 run 级 hints。
5. `agent_core`: 增加 fallback 原因日志字段。
6. `trace_eval`: 增加 retriever latency 分位与 fallback 统计。
7. `task_store`:（可选）embedding 缓存读写接口（schema 变更单独 PR）。
8. `tests`: 增加 shadow 不改行为、hybrid 排序、fallback 回退等回归。

---

## 10. 当前建议结论

建议采用：

- **短期**：`shadow -> hybrid` 两步走；
- **中期**：引入 embedding 缓存与模型版本；
- **长期**：在稳定 observability 基础上再讨论 BM25/hybrid+/rerank。

核心原则保持不变：

> 先保证可回退与可评测，再追求召回上限。
