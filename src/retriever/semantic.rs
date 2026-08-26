use crate::retriever::rule::RuleRetriever;
use crate::retriever::semantic_common::{self, RerankConfig};
use crate::retriever::{embedding::EmbeddingProvider, RetrieveQuery, RetrieveResult, Retriever};
use anyhow::Result;
use std::path::PathBuf;

const SEMANTIC_COARSE_LIMIT: usize = 1000;

/// 纯语义检索器：全量规则召回后按 embedding 相似度排序。
///
/// 流程：
/// 1. coarse：RuleRetriever 召回最多 `SEMANTIC_COARSE_LIMIT` 条（默认 1000）
/// 2. semantic：调用 EmbeddingProvider 计算 query 与候选的相似度
/// 3. 按余弦相似度降序取 limit
///
/// 与 HybridRetriever 的区别：
/// - Hybrid：规则召回量小（limit*3 或 15），最终分数 = α * 语义 + (1-α) * 规则
/// - Semantic：规则召回量大（1000），最终分数 = 纯语义相似度
///
/// 具体实现共享自 [`semantic_common`]，本结构只表达 semantic 的差异配置。
///
/// 容错：
/// - provider 报错 / query_text 为空 -> 回退到 rule（retriever_name 带 semantic_fallback）
pub struct SemanticRetriever {
    rule_retriever: RuleRetriever,
    embedding_provider: Box<dyn EmbeddingProvider + Send + Sync>,
    name: String,
}

impl SemanticRetriever {
    pub fn new(
        db_path: impl Into<PathBuf>,
        embedding_provider: Box<dyn EmbeddingProvider + Send + Sync>,
    ) -> Self {
        Self {
            rule_retriever: RuleRetriever::new(db_path),
            embedding_provider,
            name: "semantic_v1".to_string(),
        }
    }

    /// 允许自定义名称（用于 A/B 对比）
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
}

impl Retriever for SemanticRetriever {
    fn retrieve(&self, query: &RetrieveQuery) -> Result<RetrieveResult> {
        semantic_common::retrieve_with_fallback(
            &self.rule_retriever,
            self.embedding_provider.as_ref(),
            query,
            RerankConfig {
                display_name: "SemanticRetriever",
                name: &self.name,
                mode: "semantic",
                coarse_limit: SEMANTIC_COARSE_LIMIT,
                blend: |_rule_score, semantic_score| semantic_score,
                include_blend_metadata: false,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retriever::hybrid::HybridRetriever;
    use crate::retriever::test_common::{FailingEmbeddingProvider, FakeEmbeddingProvider};
    use crate::retriever::Retriever;
    use crate::task_store::{MemoryType, TaskStore};
    use std::env::temp_dir;
    use uuid::Uuid;

    fn temp_db_path() -> PathBuf {
        temp_dir().join(format!("amclaw_semantic_test_{}.db", Uuid::new_v4()))
    }

    #[test]
    fn semantic_retriever_basic_flow() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        store
            .add_user_memory_typed("user-sem1", "Rust 编程语言", MemoryType::Auto, 60)
            .expect("写入失败");
        store
            .add_user_memory_typed("user-sem1", "Python 深度学习", MemoryType::Explicit, 100)
            .expect("写入失败");
        store
            .add_user_memory_typed("user-sem1", "Go 并发编程", MemoryType::UserPreference, 80)
            .expect("写入失败");

        let provider = Box::new(FakeEmbeddingProvider);
        let semantic = SemanticRetriever::new(&db_path, provider);
        let query = RetrieveQuery::new("user-sem1", 2).with_query_text("Rust 语言");
        let result = semantic.retrieve(&query).expect("检索失败");

        // 应返回 2 条（limit=2）
        assert_eq!(result.candidates.len(), 2);
        assert_eq!(result.retriever_name, "semantic_v1");

        // 验证 metadata
        for item in &result.candidates {
            assert!(
                item.metadata.contains_key("semantic_score"),
                "semantic 结果应包含 semantic_score"
            );
            assert_eq!(
                item.metadata.get("retrieval_mode"),
                Some(&"semantic".to_string())
            );
            assert_eq!(
                item.metadata.get("embedding_model"),
                Some(&"fake_test_model".to_string())
            );
        }
    }

    #[test]
    fn semantic_retriever_fallback_on_embedding_error() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        store
            .add_user_memory_typed("user-sem2", "测试内容", MemoryType::Auto, 70)
            .expect("写入失败");

        let provider = Box::new(FailingEmbeddingProvider);
        let semantic = SemanticRetriever::new(&db_path, provider);
        let query = RetrieveQuery::new("user-sem2", 5).with_query_text("测试");
        let result = semantic.retrieve(&query).expect("检索应成功（fallback）");

        assert_eq!(result.candidates.len(), 1);
        assert!(
            result.retriever_name.contains("fallback"),
            "embedding 失败时应 fallback, 实际: {}",
            result.retriever_name
        );
        assert_eq!(
            result.candidates[0].metadata.get("retrieval_mode"),
            Some(&"semantic_fallback".to_string())
        );
    }

    #[test]
    fn semantic_fallback_metadata_matches_hybrid_fallback() {
        // 同样的失败 provider 下，两种实现的 fallback metadata 口径应一致
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        store
            .add_user_memory_typed("user-sem-fb", "测试内容", MemoryType::Auto, 70)
            .expect("写入失败");

        let query = RetrieveQuery::new("user-sem-fb", 5).with_query_text("测试");

        let semantic = SemanticRetriever::new(&db_path, Box::new(FailingEmbeddingProvider));
        let sem_result = semantic.retrieve(&query).expect("检索应成功（fallback）");
        let hybrid = HybridRetriever::new(&db_path, Box::new(FailingEmbeddingProvider));
        let hyb_result = hybrid.retrieve(&query).expect("检索应成功（fallback）");

        for (mode, item) in [
            ("semantic_fallback", &sem_result.candidates[0]),
            ("hybrid_fallback", &hyb_result.candidates[0]),
        ] {
            assert_eq!(item.metadata.get("retrieval_mode"), Some(&mode.to_string()));
            assert!(
                item.metadata.contains_key("fallback_reason"),
                "{mode} 应有 fallback_reason"
            );
            assert!(
                item.metadata.contains_key("rule_score"),
                "{mode} 应补 rule_score"
            );
            assert_eq!(
                item.metadata.get("embedding_model"),
                Some(&"failing_model".to_string()),
                "{mode} 应补 embedding_model"
            );
        }
    }

    #[test]
    fn semantic_retriever_fallback_on_empty_query_text() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        store
            .add_user_memory_typed("user-sem3", "内容", MemoryType::Auto, 60)
            .expect("写入失败");

        let provider = Box::new(FakeEmbeddingProvider);
        let semantic = SemanticRetriever::new(&db_path, provider);
        // query_text 为 None
        let query = RetrieveQuery::new("user-sem3", 5);
        let result = semantic.retrieve(&query).expect("检索应成功");

        assert_eq!(result.candidates.len(), 1);
        assert!(
            result.retriever_name.contains("fallback"),
            "无 query_text 时应 fallback"
        );
    }

    #[test]
    fn semantic_retriever_respects_limit() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        for i in 0..10 {
            store
                .add_user_memory_typed(
                    "user-sem4",
                    &format!("记忆 {}", i),
                    MemoryType::Auto,
                    50 + i as i64,
                )
                .expect("写入失败");
        }

        let provider = Box::new(FakeEmbeddingProvider);
        let semantic = SemanticRetriever::new(&db_path, provider);
        let query = RetrieveQuery::new("user-sem4", 3).with_query_text("记忆");
        let result = semantic.retrieve(&query).expect("检索失败");

        assert_eq!(result.candidates.len(), 3, "应严格返回 limit=3 条");
    }

    #[test]
    fn semantic_retriever_custom_name() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        store
            .add_user_memory_typed("user-sem5", "x", MemoryType::Auto, 50)
            .expect("写入失败");

        let provider = Box::new(FakeEmbeddingProvider);
        let semantic = SemanticRetriever::new(&db_path, provider).with_name("semantic_v2_test");
        let result = semantic
            .retrieve(&RetrieveQuery::new("user-sem5", 5).with_query_text("x"))
            .expect("检索失败");

        assert_eq!(result.retriever_name, "semantic_v2_test");
    }

    #[test]
    fn semantic_retriever_empty_result_for_unknown_user() {
        let db_path = temp_db_path();
        let provider = Box::new(FakeEmbeddingProvider);
        let semantic = SemanticRetriever::new(&db_path, provider);
        let result = semantic
            .retrieve(&RetrieveQuery::new("unknown", 5).with_query_text("测试"))
            .expect("检索应成功");

        assert!(result.candidates.is_empty());
        assert_eq!(result.retriever_name, "semantic_v1");
    }
}
