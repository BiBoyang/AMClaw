//! Hybrid / Semantic 检索器共享的内部实现。
//!
//! 包含：
//! - [`cosine_similarity`]：余弦相似度
//! - [`fallback_to_rule`]：回退到 rule 结果的统一构造（补全 metadata）
//! - [`retrieve_with_fallback`]:「coarse 召回 -> embedding 打分 -> 排序截断 -> 补 metadata」
//!   公共骨架，失败时统一回退 rule
//!
//! hybrid 与 semantic 的差异仅通过 [`RerankConfig`] 表达（coarse 上限、打分权重、
//! metadata 口径），不再各自复制整套流程。

use crate::retriever::embedding::EmbeddingProvider;
use crate::retriever::rule::RuleRetriever;
use crate::retriever::{RetrieveQuery, RetrieveResult, RetrievedItem, Retriever};
use anyhow::{Context, Result};
use std::time::Instant;

const DEFAULT_LIMIT: usize = 15;

/// 重排序配置：表达 hybrid / semantic 的差异点。
pub(crate) struct RerankConfig<'a, F>
where
    F: Fn(f64, f64) -> f64,
{
    /// 错误上下文里的实现名（如 "HybridRetriever"）
    pub display_name: &'a str,
    /// retriever_name（如 "hybrid_v1"）
    pub name: &'a str,
    /// 写入 metadata.retrieval_mode；fallback 时为 "<mode>_fallback"
    pub mode: &'a str,
    /// coarse 召回上限
    pub coarse_limit: usize,
    /// (rule_score, semantic_score) -> final_score
    pub blend: F,
    /// 是否额外写 rule_score / final_score metadata（hybrid 的全量口径）
    pub include_blend_metadata: bool,
}

/// 计算余弦相似度。
/// 输入向量未归一化时自动除以模长。
pub(crate) fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
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

/// 公共骨架：rule coarse 召回 + embedding 重排序，失败时回退 rule。
pub(crate) fn retrieve_with_fallback<F>(
    rule_retriever: &RuleRetriever,
    embedding_provider: &(dyn EmbeddingProvider + Send + Sync),
    query: &RetrieveQuery,
    config: RerankConfig<'_, F>,
) -> Result<RetrieveResult>
where
    F: Fn(f64, f64) -> f64,
{
    let started = Instant::now();
    let limit = if query.limit > 0 {
        query.limit
    } else {
        DEFAULT_LIMIT
    };
    let model_name = embedding_provider.model_name().to_string();

    // --- coarse 召回 ---
    let coarse_query = RetrieveQuery {
        user_id: query.user_id.clone(),
        query_text: query.query_text.clone(),
        limit: config.coarse_limit,
        context_hints: query.context_hints.clone(),
    };
    let coarse_result = rule_retriever
        .retrieve(&coarse_query)
        .with_context(|| format!("{} coarse 召回失败", config.display_name))?;

    // --- 检查 query_text ---
    let query_text = match query.query_text.as_deref() {
        Some(text) if !text.trim().is_empty() => text.trim(),
        _ => {
            return Ok(fallback_to_rule(
                coarse_result,
                "query_text_empty",
                started,
                limit,
                config.mode,
                config.name,
                &model_name,
            ));
        }
    };

    // --- embedding 编码 ---
    let query_vec = match embedding_provider.embed_query(query_text) {
        Ok(v) => v,
        Err(err) => {
            return Ok(fallback_to_rule(
                coarse_result,
                &format!("embedding_error: {}", err),
                started,
                limit,
                config.mode,
                config.name,
                &model_name,
            ));
        }
    };

    if coarse_result.candidates.is_empty() {
        // coarse 无结果，直接返回空
        return Ok(RetrieveResult {
            candidates: Vec::new(),
            hit_count: 0,
            dropped_count: 0,
            latency_ms: started.elapsed().as_millis().max(1),
            retriever_name: config.name.to_string(),
        });
    }

    let contents: Vec<String> = coarse_result
        .candidates
        .iter()
        .map(|item| item.content.clone())
        .collect();

    let doc_vecs = match embedding_provider.embed_documents(&contents) {
        Ok(v) => v,
        Err(err) => {
            return Ok(fallback_to_rule(
                coarse_result,
                &format!("embedding_documents_error: {}", err),
                started,
                limit,
                config.mode,
                config.name,
                &model_name,
            ));
        }
    };

    if doc_vecs.len() != coarse_result.candidates.len() {
        return Ok(fallback_to_rule(
            coarse_result,
            "embedding_documents_count_mismatch",
            started,
            limit,
            config.mode,
            config.name,
            &model_name,
        ));
    }

    // --- 打分并排序 ---
    let blend = &config.blend;
    let mut scored: Vec<(usize, f64, f64, f64)> = coarse_result
        .candidates
        .iter()
        .zip(doc_vecs.iter())
        .enumerate()
        .map(|(idx, (item, doc_vec))| {
            let rule_score = item.score.unwrap_or(0.5);
            let semantic_score = cosine_similarity(&query_vec, doc_vec).clamp(0.0, 1.0);
            let final_score = blend(rule_score, semantic_score);
            (idx, rule_score, semantic_score, final_score)
        })
        .collect();

    scored.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);

    // --- 构建结果，补 metadata ---
    let mut candidates = Vec::with_capacity(scored.len());
    for (original_idx, rule_score, semantic_score, final_score) in scored {
        let mut item = coarse_result.candidates[original_idx].clone();

        item.score = Some(final_score);
        item.metadata.insert(
            "semantic_score".to_string(),
            format!("{:.4}", semantic_score),
        );
        if config.include_blend_metadata {
            item.metadata
                .insert("rule_score".to_string(), format!("{:.4}", rule_score));
            item.metadata
                .insert("final_score".to_string(), format!("{:.4}", final_score));
        }
        item.metadata
            .insert("retrieval_mode".to_string(), config.mode.to_string());
        item.metadata
            .insert("embedding_model".to_string(), model_name.clone());

        candidates.push(item);
    }

    Ok(RetrieveResult {
        candidates,
        hit_count: 0,
        dropped_count: 0,
        latency_ms: started.elapsed().as_millis().max(1),
        retriever_name: config.name.to_string(),
    })
}

/// 回退到 rule 结果：截断到 limit 并统一补齐 fallback metadata。
///
/// 统一口径（以原 hybrid 实现为准，semantic 此前缺失 rule_score / embedding_model）：
/// - retrieval_mode = "<mode>_fallback"
/// - fallback_reason = reason
/// - rule_score（item.score 存在时）
/// - embedding_model（coarse 结果未携带时补）
pub(crate) fn fallback_to_rule(
    coarse_result: RetrieveResult,
    reason: &str,
    started: Instant,
    limit: usize,
    mode: &str,
    name: &str,
    model_name: &str,
) -> RetrieveResult {
    let candidates: Vec<RetrievedItem> = coarse_result
        .candidates
        .into_iter()
        .take(limit)
        .map(|mut item| {
            item.metadata
                .insert("retrieval_mode".to_string(), format!("{}_fallback", mode));
            item.metadata
                .insert("fallback_reason".to_string(), reason.to_string());
            if let Some(rule_score) = item.score {
                item.metadata
                    .insert("rule_score".to_string(), format!("{:.4}", rule_score));
            }
            if !item.metadata.contains_key("embedding_model") {
                item.metadata
                    .insert("embedding_model".to_string(), model_name.to_string());
            }
            item
        })
        .collect();

    RetrieveResult {
        candidates,
        hit_count: 0,
        dropped_count: 0,
        latency_ms: started.elapsed().as_millis().max(1),
        retriever_name: format!("{}_fallback", name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn coarse_item(id: &str, score: Option<f64>) -> RetrievedItem {
        RetrievedItem {
            id: id.to_string(),
            content: format!("内容 {id}"),
            score,
            source_type: "auto".to_string(),
            metadata: BTreeMap::new(),
        }
    }

    fn coarse_result(items: Vec<RetrievedItem>) -> RetrieveResult {
        RetrieveResult {
            candidates: items,
            hit_count: 0,
            dropped_count: 0,
            latency_ms: 1,
            retriever_name: "rule_v1".to_string(),
        }
    }

    #[test]
    fn cosine_similarity_identical_vectors() {
        let a = vec![1.0f32, 2.0, 3.0];
        let b = vec![1.0f32, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 1.0).abs() < 1e-6, "相同向量 cos sim 应为 1.0");
    }

    #[test]
    fn cosine_similarity_opposite_vectors() {
        let a = vec![1.0f32, 0.0, 0.0];
        let b = vec![-1.0f32, 0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - (-1.0)).abs() < 1e-6, "相反向量 cos sim 应为 -1.0");
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors() {
        let a = vec![1.0f32, 0.0, 0.0];
        let b = vec![0.0f32, 1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6, "正交向量 cos sim 应为 0.0");
    }

    #[test]
    fn cosine_similarity_zero_vector() {
        let a = vec![0.0f32, 0.0, 0.0];
        let b = vec![1.0f32, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b);
        assert_eq!(sim, 0.0, "零向量 cos sim 应为 0.0");
    }

    #[test]
    fn cosine_similarity_different_dimensions() {
        let a = vec![1.0f32, 2.0];
        let b = vec![1.0f32, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b);
        assert_eq!(sim, 0.0, "维度不一致应返回 0.0");
    }

    #[test]
    fn fallback_to_rule_fills_unified_metadata() {
        let result = fallback_to_rule(
            coarse_result(vec![coarse_item("a", Some(0.7)), coarse_item("b", None)]),
            "embedding_error: boom",
            Instant::now(),
            10,
            "semantic",
            "semantic_v1",
            "fake_test_model",
        );

        assert_eq!(result.retriever_name, "semantic_v1_fallback");
        let with_score = &result.candidates[0];
        assert_eq!(
            with_score.metadata.get("retrieval_mode"),
            Some(&"semantic_fallback".to_string())
        );
        assert_eq!(
            with_score.metadata.get("fallback_reason"),
            Some(&"embedding_error: boom".to_string())
        );
        assert_eq!(
            with_score.metadata.get("rule_score"),
            Some(&"0.7000".to_string())
        );
        assert_eq!(
            with_score.metadata.get("embedding_model"),
            Some(&"fake_test_model".to_string())
        );

        // score 为 None 的候选不写 rule_score，但仍补 embedding_model
        let no_score = &result.candidates[1];
        assert!(!no_score.metadata.contains_key("rule_score"));
        assert!(no_score.metadata.contains_key("embedding_model"));
    }

    #[test]
    fn fallback_to_rule_respects_limit() {
        let items: Vec<RetrievedItem> = (0..5)
            .map(|i| coarse_item(&format!("id-{i}"), Some(0.5)))
            .collect();
        let result = fallback_to_rule(
            coarse_result(items),
            "query_text_empty",
            Instant::now(),
            2,
            "hybrid",
            "hybrid_v1",
            "fake_test_model",
        );

        assert_eq!(result.candidates.len(), 2);
        assert_eq!(result.retriever_name, "hybrid_v1_fallback");
    }

    #[test]
    fn fallback_to_rule_keeps_existing_embedding_model() {
        let mut item = coarse_item("a", Some(0.5));
        item.metadata
            .insert("embedding_model".to_string(), "original_model".to_string());
        let result = fallback_to_rule(
            coarse_result(vec![item]),
            "query_text_empty",
            Instant::now(),
            10,
            "hybrid",
            "hybrid_v1",
            "other_model",
        );

        assert_eq!(
            result.candidates[0].metadata.get("embedding_model"),
            Some(&"original_model".to_string()),
            "已有 embedding_model 不应被覆盖"
        );
    }
}
