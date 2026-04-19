# AGENTS.md

@scope:src/retriever:v1

## 模块定位

检索抽象层，为 agent_core 提供可插拔的 memory / 上下文检索能力。

## 当前职责

1. 定义统一检索契约（RetrieveQuery / RetrievedItem / RetrieveResult / Retriever trait）。
2. 提供默认规则法实现（RuleRetriever），等价现有 task_store.search_user_memories 行为。

## 后续职责

1. 支持语义检索（embedding-based）。
2. 支持 BM25 / 混合检索。
3. 支持 A/B 对比与检索效果评估。

## 不做事项

1. 不直接操作 DB schema。
2. 不做预算裁剪或 feedback 回写（由调用方负责）。
3. 不引入外部检索服务依赖（除非显式配置）。
