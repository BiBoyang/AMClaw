# AGENTS.md

@scope:src/retriever:v1

## 模块定位

检索抽象层，为 agent_core 提供可插拔的 memory / 上下文检索能力。

## 当前职责

1. 定义统一检索契约（RetrieveQuery / RetrievedItem / RetrieveResult / Retriever trait）。
2. 提供默认规则法实现（RuleRetriever），等价现有 task_store.search_user_memories 行为。
3. 提供混合检索实现（HybridRetriever）：规则召回 + embedding 语义重排序。
4. 提供 Shadow 检索实现（ShadowRetriever）：对外始终返回 rule 结果，内部可选运行 hybrid 用于日志对比。
5. 提供 EmbeddingProvider 抽象与缓存装饰器（CachedEmbeddingProvider）：SQLite 持久化缓存层。
6. 支持 OpenAI 兼容格式的 embedding API（DeepSeek / Moonshot / OpenAI / ollama / MLX server）。

## 后续职责

1. 支持 BM25 / 混合检索。
2. 支持 A/B 对比与检索效果评估。

## 不做事项

1. 不直接操作 DB schema。
2. 不做预算裁剪或 feedback 回写（由调用方负责）。
3. 不引入外部检索服务依赖（除非显式配置）。
