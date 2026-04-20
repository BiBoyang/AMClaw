use crate::logging::emit_structured_log;
use crate::retriever::rule::RuleRetriever;
use crate::retriever::{RetrieveQuery, RetrieveResult, Retriever};
use anyhow::Result;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

/// Shadow 检索器：对外始终返回 rule 结果，可选并行计算 hybrid 仅用于日志/trace 比较。
///
/// 设计约束：
/// - 用户可见行为与 RuleRetriever 完全一致（零回归）
/// - hybrid 计算失败不影响 rule 结果（静默失败，仅日志）
/// - 保持一键回退 rule（shadow 即 rule + 可选观测）
///
/// 验收口径（3 条）：
/// 1. shadow 输出内容与 rule 一致（id / content / 顺序）—— 仅 metadata 允许差异（retrieval_mode=shadow）。
/// 2. shadow 不阻塞主返回（hybrid 慢时也快速返回 rule 结果，hybrid 在后台线程运行）。
/// 3. rollout 不放量时稳定回退 rule（GuardedRetriever enabled=false 时始终走 fallback）。
pub struct ShadowRetriever {
    rule_retriever: RuleRetriever,
    hybrid_retriever: Option<Arc<dyn Retriever + Send + Sync>>,
    name: String,
}

impl ShadowRetriever {
    pub fn new(
        db_path: impl Into<PathBuf>,
        hybrid_retriever: Option<Box<dyn Retriever + Send + Sync>>,
    ) -> Self {
        Self {
            rule_retriever: RuleRetriever::new(db_path),
            hybrid_retriever: hybrid_retriever.map(Arc::from),
            name: "shadow_v1".to_string(),
        }
    }

    /// 允许自定义名称
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
}

/// 结构化日志记录 shadow 与 hybrid 的对比结果。
fn log_shadow_comparison(
    query: &RetrieveQuery,
    rule_result: &RetrieveResult,
    hybrid_result: &RetrieveResult,
) {
    let rule_ids: Vec<&str> = rule_result
        .candidates
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    let hybrid_ids: Vec<&str> = hybrid_result
        .candidates
        .iter()
        .map(|c| c.id.as_str())
        .collect();

    let overlap_count = rule_ids.iter().filter(|id| hybrid_ids.contains(id)).count();

    let order_same = rule_ids.len() == hybrid_ids.len()
        && rule_ids.iter().zip(hybrid_ids.iter()).all(|(a, b)| a == b);

    emit_structured_log(
        "info",
        "shadow_compare",
        vec![
            ("user_id", json!(&query.user_id)),
            ("rule_count", json!(rule_result.candidates.len())),
            ("hybrid_count", json!(hybrid_result.candidates.len())),
            ("overlap", json!(overlap_count)),
            ("order_same", json!(order_same)),
            ("rule_latency_ms", json!(rule_result.latency_ms)),
            ("hybrid_latency_ms", json!(hybrid_result.latency_ms)),
        ],
    );
}

impl Retriever for ShadowRetriever {
    fn retrieve(&self, query: &RetrieveQuery) -> Result<RetrieveResult> {
        let started = Instant::now();

        // 始终获取 rule 结果（对外返回）
        let rule_result = self.rule_retriever.retrieve(query)?;

        // 可选：后台启动 hybrid 比较，不阻塞返回 rule 结果
        if let Some(ref hybrid) = self.hybrid_retriever {
            if query
                .query_text
                .as_deref()
                .map(|t| !t.trim().is_empty())
                .unwrap_or(false)
            {
                let hybrid = Arc::clone(hybrid);
                let query = query.clone();
                let rule_result = rule_result.clone();
                std::thread::spawn(move || match hybrid.retrieve(&query) {
                    Ok(hybrid_result) => {
                        log_shadow_comparison(&query, &rule_result, &hybrid_result);
                    }
                    Err(err) => {
                        emit_structured_log(
                            "warn",
                            "shadow_hybrid_failed",
                            vec![
                                ("user_id", json!(&query.user_id)),
                                ("error", json!(err.to_string())),
                            ],
                        );
                    }
                });
            }
        }

        // 对外始终返回 rule 结果，但带上 shadow 标识
        let mut result = rule_result;
        result.retriever_name = self.name.clone();
        result.latency_ms = started.elapsed().as_millis().max(1);

        // 给候选结果打上 shadow 标记
        for item in &mut result.candidates {
            item.metadata
                .insert("retrieval_mode".to_string(), "shadow".to_string());
        }

        Ok(result)
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
        temp_dir().join(format!("amclaw_shadow_test_{}.db", Uuid::new_v4()))
    }

    /// 一个总是返回固定结果的 mock hybrid retriever。
    struct MockHybridRetriever {
        name: String,
    }

    impl Retriever for MockHybridRetriever {
        fn retrieve(&self, _query: &RetrieveQuery) -> Result<RetrieveResult> {
            let mut result = RetrieveResult::empty(&self.name);
            // 模拟返回一个与 rule 不同的结果
            result.candidates.push(crate::retriever::RetrievedItem {
                id: "hybrid-only-id".to_string(),
                content: "hybrid result".to_string(),
                score: Some(0.99),
                source_type: "test".to_string(),
                metadata: std::collections::BTreeMap::new(),
            });
            result.latency_ms = 5;
            Ok(result)
        }
    }

    #[test]
    fn shadow_returns_rule_result_even_with_hybrid() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        store
            .add_user_memory_typed("user-s1", "测试内容", MemoryType::Auto, 70)
            .expect("写入失败");

        let hybrid = Box::new(MockHybridRetriever {
            name: "mock_hybrid".to_string(),
        });
        let shadow = ShadowRetriever::new(&db_path, Some(hybrid));
        let query = RetrieveQuery::new("user-s1", 5).with_query_text("测试");
        let result = shadow.retrieve(&query).expect("检索失败");

        // 对外应返回 rule 结果（不是 hybrid 的 mock 结果）
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].content, "测试内容");
        assert_eq!(result.retriever_name, "shadow_v1");

        // 检查 metadata 标记
        assert_eq!(
            result.candidates[0].metadata.get("retrieval_mode"),
            Some(&"shadow".to_string())
        );
    }

    #[test]
    fn shadow_without_hybrid_works_like_rule() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        store
            .add_user_memory_typed("user-s2", "纯规则", MemoryType::Auto, 80)
            .expect("写入失败");

        let shadow = ShadowRetriever::new(&db_path, None);
        let query = RetrieveQuery::new("user-s2", 5);
        let result = shadow.retrieve(&query).expect("检索失败");

        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].content, "纯规则");
        assert_eq!(result.retriever_name, "shadow_v1");
    }

    #[test]
    fn shadow_skips_hybrid_when_query_text_empty() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        store
            .add_user_memory_typed("user-s3", "内容", MemoryType::Auto, 60)
            .expect("写入失败");

        let hybrid = Box::new(MockHybridRetriever {
            name: "mock_hybrid".to_string(),
        });
        let shadow = ShadowRetriever::new(&db_path, Some(hybrid));
        // query_text 为 None，不应触发 hybrid
        let query = RetrieveQuery::new("user-s3", 5);
        let result = shadow.retrieve(&query).expect("检索失败");

        // 仍返回 rule 结果
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].content, "内容");
    }

    #[test]
    fn shadow_custom_name() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        store
            .add_user_memory_typed("user-s4", "x", MemoryType::Auto, 50)
            .expect("写入失败");

        let shadow = ShadowRetriever::new(&db_path, None).with_name("shadow_v2_test");
        let result = shadow
            .retrieve(&RetrieveQuery::new("user-s4", 5))
            .expect("检索失败");

        assert_eq!(result.retriever_name, "shadow_v2_test");
    }

    #[test]
    fn shadow_empty_result_for_unknown_user() {
        let db_path = temp_db_path();
        let shadow = ShadowRetriever::new(&db_path, None);
        let result = shadow
            .retrieve(&RetrieveQuery::new("unknown", 5))
            .expect("检索应成功");

        assert!(result.candidates.is_empty());
    }

    /// 一个慢速 hybrid mock，用于验证 shadow 非阻塞。
    struct SlowHybridRetriever {
        delay_ms: u64,
    }

    impl Retriever for SlowHybridRetriever {
        fn retrieve(&self, _query: &RetrieveQuery) -> Result<RetrieveResult> {
            std::thread::sleep(std::time::Duration::from_millis(self.delay_ms));
            Ok(RetrieveResult::empty("slow_hybrid"))
        }
    }

    #[test]
    fn shadow_retrieve_non_blocking() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        store
            .add_user_memory_typed("user-s5", "快速返回", MemoryType::Auto, 70)
            .expect("写入失败");

        let hybrid = Box::new(SlowHybridRetriever { delay_ms: 300 });
        let shadow = ShadowRetriever::new(&db_path, Some(hybrid));
        let query = RetrieveQuery::new("user-s5", 5).with_query_text("测试");

        let started = Instant::now();
        let result = shadow.retrieve(&query).expect("检索失败");
        let elapsed = started.elapsed().as_millis();

        // 验收口径 #2：shadow 不应被 slow hybrid 阻塞，应快速返回
        assert!(
            elapsed < 100,
            "shadow retrieve 应快速返回（<100ms），实际耗时 {}ms",
            elapsed
        );
        // 返回的仍是 rule 结果
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].content, "快速返回");
    }

    #[test]
    fn shadow_matches_rule_output() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        store
            .add_user_memory_typed("user-s6", "第一条", MemoryType::Explicit, 90)
            .expect("写入失败");
        store
            .add_user_memory_typed("user-s6", "第二条", MemoryType::Auto, 70)
            .expect("写入失败");
        store
            .add_user_memory_typed("user-s6", "第三条", MemoryType::UserPreference, 80)
            .expect("写入失败");

        // 验收口径 #1：shadow 输出内容与 rule 一致
        let rule = RuleRetriever::new(&db_path);
        let shadow = ShadowRetriever::new(&db_path, None);
        let query = RetrieveQuery::new("user-s6", 5);

        let rule_result = rule.retrieve(&query).expect("rule 检索失败");
        let shadow_result = shadow.retrieve(&query).expect("shadow 检索失败");

        assert_eq!(rule_result.candidates.len(), shadow_result.candidates.len());
        for (rule_item, shadow_item) in rule_result
            .candidates
            .iter()
            .zip(shadow_result.candidates.iter())
        {
            assert_eq!(rule_item.id, shadow_item.id);
            assert_eq!(rule_item.content, shadow_item.content);
            assert_eq!(rule_item.score, shadow_item.score);
            assert_eq!(rule_item.source_type, shadow_item.source_type);
        }

        // metadata 允许差异：shadow 应带 retrieval_mode=shadow
        assert_eq!(
            shadow_result.candidates[0].metadata.get("retrieval_mode"),
            Some(&"shadow".to_string())
        );
    }
}
