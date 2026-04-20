use crate::retriever::rule::RuleRetriever;
use crate::retriever::{
    embedding::EmbeddingProvider, RetrieveQuery, RetrieveResult, RetrievedItem, Retriever,
};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::Instant;

const DEFAULT_ALPHA: f64 = 0.5;
const MIN_COARSE_MULTIPLIER: usize = 3;
const MIN_COARSE_LIMIT: usize = 15;

/// 混合检索器：规则法召回 + 语义重排序。
///
/// 流程：
/// 1. coarse：RuleRetriever 召回 `max(limit*3, 15)` 条
/// 2. semantic：调用 EmbeddingProvider 计算 query 与候选的相似度
/// 3. final_score = α * semantic_score + (1-α) * rule_score
/// 4. 按 final_score 降序取 limit
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

    /// 执行 hybrid 检索，失败时回退到 rule。
    fn retrieve_with_fallback(&self, query: &RetrieveQuery) -> Result<RetrieveResult> {
        let started = Instant::now();
        let limit = if query.limit > 0 { query.limit } else { 15 };
        let coarse_limit = (limit * MIN_COARSE_MULTIPLIER).max(MIN_COARSE_LIMIT);

        // --- coarse 召回 ---
        let coarse_query = RetrieveQuery {
            user_id: query.user_id.clone(),
            query_text: query.query_text.clone(),
            limit: coarse_limit,
            context_hints: query.context_hints.clone(),
        };
        let coarse_result = self
            .rule_retriever
            .retrieve(&coarse_query)
            .with_context(|| "HybridRetriever coarse 召回失败")?;

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

        let contents: Vec<String> = coarse_result
            .candidates
            .iter()
            .map(|item| item.content.clone())
            .collect();

        if contents.is_empty() {
            // coarse 无结果，直接返回空
            return Ok(RetrieveResult {
                candidates: Vec::new(),
                hit_count: 0,
                dropped_count: 0,
                latency_ms: started.elapsed().as_millis().max(1),
                retriever_name: self.name.clone(),
            });
        }

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

        // --- 混合打分并排序 ---
        let mut scored: Vec<(usize, f64)> = coarse_result
            .candidates
            .iter()
            .zip(doc_vecs.iter())
            .enumerate()
            .map(|(idx, (item, doc_vec))| {
                let rule_score = item.score.unwrap_or(0.5);
                let semantic_score = Self::cosine_similarity(&query_vec, doc_vec).clamp(0.0, 1.0);
                let final_score = self.alpha * semantic_score + (1.0 - self.alpha) * rule_score;
                (idx, final_score, semantic_score, rule_score)
            })
            .map(|(idx, final_score, _semantic, _rule)| (idx, final_score))
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        // --- 构建结果，补 metadata ---
        let model_name = self.embedding_provider.model_name().to_string();
        let mut candidates = Vec::with_capacity(scored.len());
        for (original_idx, final_score) in scored {
            let mut item = coarse_result.candidates[original_idx].clone();
            let semantic_score =
                Self::cosine_similarity(&query_vec, &doc_vecs[original_idx]).clamp(0.0, 1.0);
            let rule_score = item.score.unwrap_or(0.5);

            item.score = Some(final_score);
            item.metadata
                .insert("rule_score".to_string(), format!("{:.4}", rule_score));
            item.metadata.insert(
                "semantic_score".to_string(),
                format!("{:.4}", semantic_score),
            );
            item.metadata
                .insert("final_score".to_string(), format!("{:.4}", final_score));
            item.metadata
                .insert("retrieval_mode".to_string(), "hybrid".to_string());
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
        let mut candidates: Vec<RetrievedItem> = coarse_result
            .candidates
            .into_iter()
            .take(limit)
            .map(|mut item| {
                item.metadata
                    .insert("retrieval_mode".to_string(), "hybrid_fallback".to_string());
                item.metadata
                    .insert("fallback_reason".to_string(), reason.to_string());
                if let Some(rule_score) = item.score {
                    item.metadata
                        .insert("rule_score".to_string(), format!("{:.4}", rule_score));
                }
                item
            })
            .collect();

        // 如果没有任何候选，不需要额外处理
        for item in &mut candidates {
            if !item.metadata.contains_key("embedding_model") {
                item.metadata.insert(
                    "embedding_model".to_string(),
                    self.embedding_provider.model_name().to_string(),
                );
            }
        }

        RetrieveResult {
            candidates,
            hit_count: 0,
            dropped_count: 0,
            latency_ms: started.elapsed().as_millis().max(1),
            retriever_name: format!("{}_fallback", self.name),
        }
    }
}

impl Retriever for HybridRetriever {
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
        temp_dir().join(format!("amclaw_hybrid_test_{}.db", Uuid::new_v4()))
    }

    /// 一个假的 EmbeddingProvider，用于测试。
    /// embed_query 返回基于文本 hash 的固定向量。
    /// embed_documents 返回基于每个文本 hash 的固定向量。
    struct FakeEmbeddingProvider;

    impl EmbeddingProvider for FakeEmbeddingProvider {
        fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
            // 生成一个 4 维的伪向量，基于文本内容
            let hash = text
                .bytes()
                .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
            let mut vec = vec![0.0f32; 4];
            for i in 0..4 {
                let val = ((hash.wrapping_add(i as u64)) % 1000) as f32 / 1000.0;
                vec[i] = val;
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

    /// 另一个 FakeProvider，总是返回错误，用于测试 fallback。
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

    #[test]
    fn cosine_similarity_identical_vectors() {
        let a = vec![1.0f32, 2.0, 3.0];
        let b = vec![1.0f32, 2.0, 3.0];
        let sim = HybridRetriever::cosine_similarity(&a, &b);
        assert!((sim - 1.0).abs() < 1e-6, "相同向量 cos sim 应为 1.0");
    }

    #[test]
    fn cosine_similarity_opposite_vectors() {
        let a = vec![1.0f32, 0.0, 0.0];
        let b = vec![-1.0f32, 0.0, 0.0];
        let sim = HybridRetriever::cosine_similarity(&a, &b);
        assert!((sim - (-1.0)).abs() < 1e-6, "相反向量 cos sim 应为 -1.0");
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors() {
        let a = vec![1.0f32, 0.0, 0.0];
        let b = vec![0.0f32, 1.0, 0.0];
        let sim = HybridRetriever::cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6, "正交向量 cos sim 应为 0.0");
    }

    #[test]
    fn cosine_similarity_zero_vector() {
        let a = vec![0.0f32, 0.0, 0.0];
        let b = vec![1.0f32, 2.0, 3.0];
        let sim = HybridRetriever::cosine_similarity(&a, &b);
        assert_eq!(sim, 0.0, "零向量 cos sim 应为 0.0");
    }

    #[test]
    fn cosine_similarity_different_dimensions() {
        let a = vec![1.0f32, 2.0];
        let b = vec![1.0f32, 2.0, 3.0];
        let sim = HybridRetriever::cosine_similarity(&a, &b);
        assert_eq!(sim, 0.0, "维度不一致应返回 0.0");
    }
}
