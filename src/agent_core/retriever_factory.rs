use crate::retriever::cached_embedding::CachedEmbeddingProvider;
use crate::retriever::embedding::NoOpEmbeddingProvider;
use crate::retriever::guarded::GuardedRetriever;
use crate::retriever::hybrid::HybridRetriever;
use crate::retriever::rule::RuleRetriever;
use crate::retriever::semantic::SemanticRetriever;
use crate::retriever::shadow::ShadowRetriever;
use crate::retriever::Retriever;
use anyhow::{bail, Result};
use serde_json::json;
use std::path::Path;

/// 检索模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum RetrieverMode {
    /// 规则法（默认）：priority / useful / use_count 排序
    Rule,
    /// 语义检索（纯语义排序）
    Semantic,
    /// 混合检索（规则粗召回 + 语义重排序）
    Hybrid,
    /// Shadow：并行运行语义但对外只返回规则结果
    Shadow,
}

impl RetrieverMode {
    /// 从配置字符串解析。非法值明确报错。
    pub(crate) fn from_config(text: &str) -> Result<Self> {
        match text {
            "rule" => Ok(Self::Rule),
            "semantic" => Ok(Self::Semantic),
            "hybrid" => Ok(Self::Hybrid),
            "shadow" => Ok(Self::Shadow),
            other => bail!("非法 retriever_mode: {other}。合法值: rule, semantic, hybrid, shadow"),
        }
    }

    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Rule => "rule",
            Self::Semantic => "semantic",
            Self::Hybrid => "hybrid",
            Self::Shadow => "shadow",
        }
    }
}

/// 根据 mode、db_path 和 embedding_provider 选择 retriever。
///
/// - semantic / hybrid / shadow 根据 embedding_provider 配置选择 provider
/// - embedding_provider = "noop" 时 fallback 到 rule
/// - semantic / hybrid / shadow 外层包 GuardedRetriever 做灰度控制
pub(crate) fn select_retriever(
    mode: RetrieverMode,
    db_path: Option<&Path>,
    embedding_provider_name: &str,
    rollout_enabled: bool,
    allow_users: &[String],
) -> Box<dyn Retriever + Send + Sync> {
    // rollout 关闭时，semantic/hybrid/shadow 直接短路到 fallback rule，
    // 避免初始化 embedding provider（降低默认不放量路径的噪音和开销）。
    if !rollout_enabled {
        if let Some(path) = db_path {
            if mode != RetrieverMode::Rule {
                super::log_agent_info(
                    "retriever_rollout_short_circuit",
                    vec![
                        ("requested_mode", json!(mode.as_str())),
                        ("actual_mode", json!("rule")),
                        ("reason", json!("rollout_disabled")),
                    ],
                );
                return Box::new(GuardedRetriever::fallback_only(Box::new(
                    RuleRetriever::new(path),
                )));
            }
        }
    }

    match (mode, db_path) {
        (RetrieverMode::Rule, Some(path)) => Box::new(RuleRetriever::new(path)),
        (RetrieverMode::Rule, None) => Box::new(NoOpRetriever),
        (RetrieverMode::Semantic, Some(path)) => {
            let inner_provider = match crate::retriever::embedding::create_embedding_provider(
                embedding_provider_name,
            ) {
                Ok(p) => p,
                Err(err) => {
                    super::log_agent_warn(
                        "embedding_provider_init_failed",
                        vec![
                            ("provider", json!(embedding_provider_name)),
                            ("error", json!(err.to_string())),
                            ("fallback", json!("NoOpEmbeddingProvider")),
                        ],
                    );
                    Box::new(NoOpEmbeddingProvider::new())
                }
            };
            let provider = Box::new(CachedEmbeddingProvider::new(inner_provider, path));
            let primary = Box::new(SemanticRetriever::new(path, provider));
            let fallback = Box::new(RuleRetriever::new(path));
            Box::new(GuardedRetriever::new(
                primary,
                fallback,
                rollout_enabled,
                allow_users.to_vec(),
            ))
        }
        (RetrieverMode::Semantic, None) => {
            super::log_agent_info(
                "retriever_mode_fallback_noop",
                vec![
                    ("requested_mode", json!("semantic")),
                    ("actual_mode", json!("noop")),
                    ("reason", json!("no db_path, using NoOpRetriever")),
                ],
            );
            Box::new(NoOpRetriever)
        }
        (RetrieverMode::Hybrid, Some(path)) => {
            let inner_provider = match crate::retriever::embedding::create_embedding_provider(
                embedding_provider_name,
            ) {
                Ok(p) => p,
                Err(err) => {
                    super::log_agent_warn(
                        "embedding_provider_init_failed",
                        vec![
                            ("provider", json!(embedding_provider_name)),
                            ("error", json!(err.to_string())),
                            ("fallback", json!("NoOpEmbeddingProvider")),
                        ],
                    );
                    Box::new(NoOpEmbeddingProvider::new())
                }
            };
            let provider = Box::new(CachedEmbeddingProvider::new(inner_provider, path));
            let primary = Box::new(HybridRetriever::new(path, provider));
            let fallback = Box::new(RuleRetriever::new(path));
            Box::new(GuardedRetriever::new(
                primary,
                fallback,
                rollout_enabled,
                allow_users.to_vec(),
            ))
        }
        (RetrieverMode::Hybrid, None) => {
            super::log_agent_info(
                "retriever_mode_fallback_noop",
                vec![
                    ("requested_mode", json!("hybrid")),
                    ("actual_mode", json!("noop")),
                    ("reason", json!("no db_path, using NoOpRetriever")),
                ],
            );
            Box::new(NoOpRetriever)
        }
        (RetrieverMode::Shadow, Some(path)) => {
            let inner_provider = match crate::retriever::embedding::create_embedding_provider(
                embedding_provider_name,
            ) {
                Ok(p) => p,
                Err(err) => {
                    super::log_agent_warn(
                        "embedding_provider_init_failed",
                        vec![
                            ("provider", json!(embedding_provider_name)),
                            ("error", json!(err.to_string())),
                            ("fallback", json!("NoOpEmbeddingProvider")),
                        ],
                    );
                    Box::new(NoOpEmbeddingProvider::new())
                }
            };
            let provider = Box::new(CachedEmbeddingProvider::new(inner_provider, path));
            let hybrid = Box::new(HybridRetriever::new(path, provider));
            let primary = Box::new(ShadowRetriever::new(path, Some(hybrid)));
            let fallback = Box::new(RuleRetriever::new(path));
            Box::new(GuardedRetriever::new(
                primary,
                fallback,
                rollout_enabled,
                allow_users.to_vec(),
            ))
        }
        (RetrieverMode::Shadow, None) => {
            super::log_agent_info(
                "retriever_mode_fallback_noop",
                vec![
                    ("requested_mode", json!("shadow")),
                    ("actual_mode", json!("noop")),
                    ("reason", json!("no db_path, using NoOpRetriever")),
                ],
            );
            Box::new(NoOpRetriever)
        }
    }
}

pub(crate) struct NoOpRetriever;

impl crate::retriever::Retriever for NoOpRetriever {
    fn retrieve(
        &self,
        _query: &crate::retriever::RetrieveQuery,
    ) -> anyhow::Result<crate::retriever::RetrieveResult> {
        Ok(crate::retriever::RetrieveResult::empty("noop"))
    }
}
