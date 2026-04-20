use anyhow::Result;
use std::collections::BTreeMap;

/// 检索请求参数。
///
/// 设计上轻量、可扩展：后续支持 embedding / BM25 / 混合检索时，
/// 只需新增可选字段，不破坏已有实现。
#[derive(Debug, Clone)]
pub struct RetrieveQuery {
    pub user_id: String,
    /// 可选的 query text（语义检索用；规则法可忽略）
    pub query_text: Option<String>,
    /// 返回候选上限
    pub limit: usize,
    /// 上下文提示（如当前 task 类型、计划步数等），供 retriever 做动态调整
    pub context_hints: BTreeMap<String, String>,
}

impl RetrieveQuery {
    pub fn new(user_id: impl Into<String>, limit: usize) -> Self {
        Self {
            user_id: user_id.into(),
            query_text: None,
            limit,
            context_hints: BTreeMap::new(),
        }
    }

    pub fn with_query_text(mut self, text: impl Into<String>) -> Self {
        self.query_text = Some(text.into());
        self
    }

    pub fn with_hint(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.context_hints.insert(key.into(), value.into());
        self
    }
}

/// 单个检索结果项。
///
/// score: 语义检索时填充相似度分数；规则法可填 None 或用 priority 映射。
/// metadata: 轻量键值，不引入复杂 schema。
#[derive(Debug, Clone)]
pub struct RetrievedItem {
    pub id: String,
    pub content: String,
    pub score: Option<f64>,
    pub source_type: String,
    pub metadata: BTreeMap<String, String>,
}

/// 单次检索的完整结果。
///
/// candidates: 原始候选列表（未经过预算裁剪）
/// hit_count: 实际命中/注入的条数（由上层 SessionState 裁剪后决定）
/// dropped_count: 被裁剪掉的条数
/// latency_ms: 检索耗时（毫秒）
/// retriever_name: 实现标识，用于 trace 与 A/B 对比
#[derive(Debug, Clone)]
pub struct RetrieveResult {
    pub candidates: Vec<RetrievedItem>,
    pub hit_count: usize,
    pub dropped_count: usize,
    pub latency_ms: u128,
    pub retriever_name: String,
}

impl RetrieveResult {
    pub fn empty(retriever_name: impl Into<String>) -> Self {
        Self {
            candidates: Vec::new(),
            hit_count: 0,
            dropped_count: 0,
            latency_ms: 0,
            retriever_name: retriever_name.into(),
        }
    }
}

/// 可插拔检索器 trait。
///
/// 设计约束：
/// - 不持有可变状态（检索器本身无 side-effect）
/// - retrieve 只负责"取回候选"，不负责预算裁剪或 feedback 回写
/// - 裁剪、feedback、trace 由调用方（agent_core）统一处理
pub trait Retriever {
    fn retrieve(&self, query: &RetrieveQuery) -> Result<RetrieveResult>;
}

pub mod cached_embedding;
pub mod embedding;
pub mod hybrid;
pub mod rule;
pub mod shadow;
