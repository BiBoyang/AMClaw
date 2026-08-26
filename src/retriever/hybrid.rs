use crate::retriever::rule::RuleRetriever;
use crate::retriever::semantic_common::{self, RerankConfig};
use crate::retriever::{embedding::EmbeddingProvider, RetrieveQuery, RetrieveResult, Retriever};
use anyhow::Result;
use std::path::PathBuf;

const DEFAULT_ALPHA: f64 = 0.5;
const MIN_COARSE_MULTIPLIER: usize = 3;
const MIN_COARSE_LIMIT: usize = 15;
const DEFAULT_LIMIT: usize = 15;

/// 混合检索器：规则法召回 + 语义重排序。
///
/// 流程：
/// 1. coarse：RuleRetriever 召回 `max(limit*3, 15)` 条
/// 2. semantic：调用 EmbeddingProvider 计算 query 与候选的相似度
/// 3. final_score = α * semantic_score + (1-α) * rule_score
/// 4. 按 final_score 降序取 limit
///
/// 具体实现共享自 [`semantic_common`]，本结构只表达 hybrid 的差异配置。
///
/// 容错：
/// - provider 报错 / query_text 为空 -> 回退到 rule（retriever_name 带 hybrid_fallback）
pub struct HybridRetriever {
    rule_retriever: RuleRetriever,
    embedding_provider: Box<dyn EmbeddingProvider + Send + Sync>,
    alpha: f64,
    name: String,
}

impl HybridRetriever {
    pub fn new(
        db_path: impl Into<PathBuf>,
        embedding_provider: Box<dyn EmbeddingProvider + Send + Sync>,
    ) -> Self {
        Self {
            rule_retriever: RuleRetriever::new(db_path),
            embedding_provider,
            alpha: DEFAULT_ALPHA,
            name: "hybrid_v1".to_string(),
        }
    }

    /// 允许自定义 α（语义分权重）
    pub fn with_alpha(mut self, alpha: f64) -> Self {
        self.alpha = alpha.clamp(0.0, 1.0);
        self
    }

    /// 允许自定义名称（用于 A/B 对比）
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
}

impl Retriever for HybridRetriever {
    fn retrieve(&self, query: &RetrieveQuery) -> Result<RetrieveResult> {
        let limit = if query.limit > 0 {
            query.limit
        } else {
            DEFAULT_LIMIT
        };
        let coarse_limit = (limit * MIN_COARSE_MULTIPLIER).max(MIN_COARSE_LIMIT);
        let alpha = self.alpha;

        semantic_common::retrieve_with_fallback(
            &self.rule_retriever,
            self.embedding_provider.as_ref(),
            query,
            RerankConfig {
                display_name: "HybridRetriever",
                name: &self.name,
                mode: "hybrid",
                coarse_limit,
                blend: move |rule_score, semantic_score| {
                    alpha * semantic_score + (1.0 - alpha) * rule_score
                },
                include_blend_metadata: true,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retriever::test_common::{FailingEmbeddingProvider, FakeEmbeddingProvider};
    use crate::retriever::Retriever;
    use crate::task_store::{MemoryType, TaskStore};
    use std::env::temp_dir;
    use uuid::Uuid;

    fn temp_db_path() -> PathBuf {
        temp_dir().join(format!("amclaw_hybrid_test_{}.db", Uuid::new_v4()))
    }

    #[test]
    fn hybrid_retriever_basic_flow() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        store
            .add_user_memory_typed("user-h1", " Rust 编程", MemoryType::Auto, 60)
            .expect("写入失败");
        store
            .add_user_memory_typed("user-h1", "深度学习", MemoryType::Explicit, 100)
            .expect("写入失败");
        store
            .add_user_memory_typed("user-h1", "Web 开发", MemoryType::UserPreference, 80)
            .expect("写入失败");

        let provider = Box::new(FakeEmbeddingProvider);
        let retriever = HybridRetriever::new(&db_path, provider);
        let query = RetrieveQuery::new("user-h1", 2).with_query_text("机器学习");
        let result = retriever.retrieve(&query).expect("检索失败");

        // 应返回 2 条（limit=2）
        assert_eq!(result.candidates.len(), 2);
        assert_eq!(result.retriever_name, "hybrid_v1");
        assert!(result.latency_ms > 0);

        // 检查 metadata
        for item in &result.candidates {
            assert!(item.metadata.contains_key("rule_score"), "应有 rule_score");
            assert!(
                item.metadata.contains_key("semantic_score"),
                "应有 semantic_score"
            );
            assert!(
                item.metadata.contains_key("final_score"),
                "应有 final_score"
            );
            assert_eq!(
                item.metadata.get("retrieval_mode"),
                Some(&"hybrid".to_string())
            );
            assert_eq!(
                item.metadata.get("embedding_model"),
                Some(&"fake_test_model".to_string())
            );
        }
    }

    #[test]
    fn hybrid_retriever_fallback_on_embedding_error() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        store
            .add_user_memory_typed("user-h2", "测试内容", MemoryType::Auto, 70)
            .expect("写入失败");

        let provider = Box::new(FailingEmbeddingProvider);
        let retriever = HybridRetriever::new(&db_path, provider);
        let query = RetrieveQuery::new("user-h2", 5).with_query_text("测试");
        let result = retriever.retrieve(&query).expect("检索应成功（fallback）");

        // 应 fallback 到 rule
        assert!(result.retriever_name.contains("fallback"));
        assert_eq!(result.candidates.len(), 1);

        let item = &result.candidates[0];
        assert_eq!(
            item.metadata.get("retrieval_mode"),
            Some(&"hybrid_fallback".to_string())
        );
        assert!(
            item.metadata.contains_key("fallback_reason"),
            "应有 fallback_reason"
        );
        let reason = item.metadata.get("fallback_reason").unwrap();
        assert!(
            reason.contains("embedding_error"),
            "fallback reason 应说明 embedding 错误"
        );
    }

    #[test]
    fn hybrid_retriever_fallback_on_empty_query_text() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        store
            .add_user_memory_typed("user-h3", "测试内容", MemoryType::Auto, 70)
            .expect("写入失败");

        let provider = Box::new(FakeEmbeddingProvider);
        let retriever = HybridRetriever::new(&db_path, provider);
        // query_text 为 None
        let query = RetrieveQuery::new("user-h3", 5);
        let result = retriever.retrieve(&query).expect("检索应成功（fallback）");

        assert!(result.retriever_name.contains("fallback"));
        assert_eq!(result.candidates.len(), 1);
        let item = &result.candidates[0];
        assert_eq!(
            item.metadata.get("fallback_reason"),
            Some(&"query_text_empty".to_string())
        );
    }

    #[test]
    fn hybrid_retriever_empty_result_for_unknown_user() {
        let db_path = temp_db_path();
        let provider = Box::new(FakeEmbeddingProvider);
        let retriever = HybridRetriever::new(&db_path, provider);
        let query = RetrieveQuery::new("unknown-user", 5).with_query_text(" anything");
        let result = retriever.retrieve(&query).expect("检索应成功");

        assert!(result.candidates.is_empty());
        assert_eq!(result.retriever_name, "hybrid_v1");
    }

    #[test]
    fn hybrid_retriever_respects_limit() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        for i in 0..10 {
            store
                .add_user_memory_typed(
                    "user-h4",
                    &format!("内容 {}", i),
                    MemoryType::Auto,
                    50 + i as i64 * 5,
                )
                .expect("写入失败");
        }

        let provider = Box::new(FakeEmbeddingProvider);
        let retriever = HybridRetriever::new(&db_path, provider);
        let query = RetrieveQuery::new("user-h4", 3).with_query_text("内容");
        let result = retriever.retrieve(&query).expect("检索失败");

        // limit=3，应返回 3 条
        assert_eq!(result.candidates.len(), 3);
    }

    #[test]
    fn hybrid_retriever_custom_alpha() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        store
            .add_user_memory_typed("user-h5", "A", MemoryType::Auto, 50)
            .expect("写入失败");
        store
            .add_user_memory_typed("user-h5", "B", MemoryType::Auto, 100)
            .expect("写入失败");

        let provider = Box::new(FakeEmbeddingProvider);
        let retriever = HybridRetriever::new(&db_path, provider).with_alpha(0.8);
        let query = RetrieveQuery::new("user-h5", 5).with_query_text("A");
        let result = retriever.retrieve(&query).expect("检索失败");

        assert_eq!(result.retriever_name, "hybrid_v1");
        // 只要成功即可，alpha 影响排序权重
        assert!(!result.candidates.is_empty());
    }

    #[test]
    fn hybrid_retriever_custom_name() {
        let db_path = temp_db_path();
        let provider = Box::new(FakeEmbeddingProvider);
        let retriever = HybridRetriever::new(&db_path, provider).with_name("hybrid_v2_test");
        let query = RetrieveQuery::new("user-h6", 5).with_query_text("test");
        let result = retriever.retrieve(&query).expect("检索失败");

        assert_eq!(result.retriever_name, "hybrid_v2_test");
    }
}
