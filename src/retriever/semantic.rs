use crate::retriever::rule::RuleRetriever;
use crate::retriever::{
    embedding::EmbeddingProvider, RetrieveQuery, RetrieveResult, RetrievedItem, Retriever,
};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::Instant;

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

    /// 计算余弦相似度。
    /// 输入向量未归一化时自动除以模长。
    fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
        if a.len() != b.len() || a.is_empty() {
            return 0.0;
        }
        let mut dot = 0.0f64;
        let mut norm_a = 0.0f64;
        let mut norm_b = 0.0f64;
        for (av, bv) in a.iter().zip(b.iter()) {
            let av64 = *av as f64;
            let bv64 = *bv as f64;
            dot += av64 * bv64;
            norm_a += av64 * av64;
            norm_b += bv64 * bv64;
        }
        if norm_a == 0.0 || norm_b == 0.0 {
            return 0.0;
        }
        (dot / (norm_a.sqrt() * norm_b.sqrt())).clamp(-1.0, 1.0)
    }

    /// 执行语义检索，失败时回退到 rule。
    fn retrieve_with_fallback(&self, query: &RetrieveQuery) -> Result<RetrieveResult> {
        let started = Instant::now();
        let limit = if query.limit > 0 { query.limit } else { 15 };

        // --- coarse 召回：取大量候选供语义排序 ---
        let coarse_query = RetrieveQuery {
            user_id: query.user_id.clone(),
            query_text: query.query_text.clone(),
            limit: SEMANTIC_COARSE_LIMIT,
            context_hints: query.context_hints.clone(),
        };
        let coarse_result = self
            .rule_retriever
            .retrieve(&coarse_query)
            .with_context(|| "SemanticRetriever coarse 召回失败")?;

        // --- 检查 query_text ---
        let query_text = match query.query_text.as_deref() {
            Some(text) if !text.trim().is_empty() => text.trim(),
            _ => {
                return Ok(self.fallback_to_rule(
                    coarse_result,
                    "query_text_empty",
                    started,
                    limit,
                ));
            }
        };

        // --- embedding 编码 ---
        let query_vec = match self.embedding_provider.embed_query(query_text) {
            Ok(v) => v,
            Err(err) => {
                return Ok(self.fallback_to_rule(
                    coarse_result,
                    &format!("embedding_error: {}", err),
                    started,
                    limit,
                ));
            }
        };

        if coarse_result.candidates.is_empty() {
            return Ok(RetrieveResult {
                candidates: Vec::new(),
                hit_count: 0,
                dropped_count: 0,
                latency_ms: started.elapsed().as_millis().max(1),
                retriever_name: self.name.clone(),
            });
        }

        let contents: Vec<String> = coarse_result
            .candidates
            .iter()
            .map(|item| item.content.clone())
            .collect();

        let doc_vecs = match self.embedding_provider.embed_documents(&contents) {
            Ok(v) => v,
            Err(err) => {
                return Ok(self.fallback_to_rule(
                    coarse_result,
                    &format!("embedding_documents_error: {}", err),
                    started,
                    limit,
                ));
            }
        };

        if doc_vecs.len() != coarse_result.candidates.len() {
            return Ok(self.fallback_to_rule(
                coarse_result,
                "embedding_documents_count_mismatch",
                started,
                limit,
            ));
        }

        // --- 纯语义打分并排序 ---
        let mut scored: Vec<(usize, f64)> = coarse_result
            .candidates
            .iter()
            .zip(doc_vecs.iter())
            .enumerate()
            .map(|(idx, (_item, doc_vec))| {
                let semantic_score = Self::cosine_similarity(&query_vec, doc_vec).clamp(0.0, 1.0);
                (idx, semantic_score)
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        // --- 构建结果，补 metadata ---
        let model_name = self.embedding_provider.model_name().to_string();
        let mut candidates = Vec::with_capacity(scored.len());
        for (original_idx, semantic_score) in scored {
            let mut item = coarse_result.candidates[original_idx].clone();

            item.score = Some(semantic_score);
            item.metadata.insert(
                "semantic_score".to_string(),
                format!("{:.4}", semantic_score),
            );
            item.metadata
                .insert("retrieval_mode".to_string(), "semantic".to_string());
            item.metadata
                .insert("embedding_model".to_string(), model_name.clone());

            candidates.push(item);
        }

        Ok(RetrieveResult {
            candidates,
            hit_count: 0,
            dropped_count: 0,
            latency_ms: started.elapsed().as_millis().max(1),
            retriever_name: self.name.clone(),
        })
    }

    /// 回退到 rule 结果，限制条数并标记 fallback。
    fn fallback_to_rule(
        &self,
        coarse_result: RetrieveResult,
        reason: &str,
        started: Instant,
        limit: usize,
    ) -> RetrieveResult {
        let candidates: Vec<RetrievedItem> = coarse_result
            .candidates
            .into_iter()
            .take(limit)
            .map(|mut item| {
                item.metadata.insert(
                    "retrieval_mode".to_string(),
                    "semantic_fallback".to_string(),
                );
                item.metadata
                    .insert("fallback_reason".to_string(), reason.to_string());
                item
            })
            .collect();

        RetrieveResult {
            candidates,
            hit_count: 0,
            dropped_count: 0,
            latency_ms: started.elapsed().as_millis().max(1),
            retriever_name: format!("{}_fallback", self.name),
        }
    }
}

impl Retriever for SemanticRetriever {
    fn retrieve(&self, query: &RetrieveQuery) -> Result<RetrieveResult> {
        self.retrieve_with_fallback(query)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retriever::Retriever;
    use crate::task_store::{MemoryType, TaskStore};
    use std::env::temp_dir;
    use uuid::Uuid;

    fn temp_db_path() -> PathBuf {
        temp_dir().join(format!("amclaw_semantic_test_{}.db", Uuid::new_v4()))
    }

    /// 一个假的 EmbeddingProvider，用于测试。
    /// embed_query 返回基于文本 hash 的固定向量。
    /// embed_documents 返回基于每个文本 hash 的固定向量。
    struct FakeEmbeddingProvider;

    impl EmbeddingProvider for FakeEmbeddingProvider {
        fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
            let hash = text
                .bytes()
                .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
            let mut vec = vec![0.0f32; 4];
            for (i, slot) in vec.iter_mut().enumerate() {
                *slot = ((hash.wrapping_add(i as u64)) % 1000) as f32 / 1000.0;
            }
            Ok(vec)
        }

        fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            texts.iter().map(|text| self.embed_query(text)).collect()
        }

        fn model_name(&self) -> &str {
            "fake_test_model"
        }
    }

    /// 总是返回错误的 FakeProvider，用于测试 fallback。
    struct FailingEmbeddingProvider;

    impl EmbeddingProvider for FailingEmbeddingProvider {
        fn embed_query(&self, _text: &str) -> Result<Vec<f32>> {
            anyhow::bail!("simulated embedding failure")
        }

        fn embed_documents(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
            anyhow::bail!("simulated embedding failure")
        }

        fn model_name(&self) -> &str {
            "failing_model"
        }
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
