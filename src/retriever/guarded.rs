use crate::logging::emit_structured_log;
use crate::retriever::{RetrieveQuery, RetrieveResult, Retriever};
use anyhow::Result;
use serde_json::json;

/// GuardedRetriever：语义检索的灰度发布包装器。
///
/// 行为：
/// - !enabled → 始终回退到 fallback（rule）
/// - allow_users 非空且 query.user_id 不在列表 → 回退到 fallback
/// - 否则 → 用主链路（semantic / hybrid / shadow）
///
/// 给结果打 metadata：
/// - 回退：retrieval_mode=rollout_fallback_rule, rollout_reason
/// - 命中主链路：rollout_allowed=true
pub struct GuardedRetriever {
    primary: Box<dyn Retriever + Send + Sync>,
    fallback: Box<dyn Retriever + Send + Sync>,
    enabled: bool,
    allow_users: Vec<String>,
}

impl GuardedRetriever {
    pub fn new(
        primary: Box<dyn Retriever + Send + Sync>,
        fallback: Box<dyn Retriever + Send + Sync>,
        enabled: bool,
        allow_users: Vec<String>,
    ) -> Self {
        Self {
            primary,
            fallback,
            enabled,
            allow_users,
        }
    }

    /// 检查用户是否在允许列表中。
    /// allow_users 为空时视为全员放行。
    fn user_allowed(&self, user_id: &str) -> bool {
        if self.allow_users.is_empty() {
            return true;
        }
        self.allow_users.iter().any(|u| u == user_id)
    }
}

impl Retriever for GuardedRetriever {
    fn retrieve(&self, query: &RetrieveQuery) -> Result<RetrieveResult> {
        let (use_primary, reason) = if !self.enabled {
            (false, "rollout_disabled")
        } else if !self.user_allowed(&query.user_id) {
            (false, "user_not_in_allowlist")
        } else {
            (true, "")
        };

        if use_primary {
            let mut result = self.primary.retrieve(query)?;
            // 标记命中主链路
            for item in &mut result.candidates {
                item.metadata
                    .insert("rollout_allowed".to_string(), "true".to_string());
            }
            emit_structured_log(
                "info",
                "retriever_rollout_allowed",
                vec![
                    ("user_id", json!(&query.user_id)),
                    ("retriever_name", json!(&result.retriever_name)),
                ],
            );
            Ok(result)
        } else {
            let mut result = self.fallback.retrieve(query)?;
            // 标记回退
            for item in &mut result.candidates {
                item.metadata.insert(
                    "retrieval_mode".to_string(),
                    "rollout_fallback_rule".to_string(),
                );
                item.metadata
                    .insert("rollout_reason".to_string(), reason.to_string());
            }
            emit_structured_log(
                "info",
                "retriever_rollout_fallback",
                vec![
                    ("user_id", json!(&query.user_id)),
                    ("reason", json!(reason)),
                    ("retriever_name", json!(&result.retriever_name)),
                ],
            );
            Ok(result)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retriever::{RetrieveQuery, RetrieveResult, RetrievedItem, Retriever};
    use std::collections::BTreeMap;

    /// 总是返回固定名称的 mock retriever。
    struct MockRetriever {
        name: String,
    }

    impl Retriever for MockRetriever {
        fn retrieve(&self, _query: &RetrieveQuery) -> Result<RetrieveResult> {
            let mut result = RetrieveResult::empty(&self.name);
            result.candidates.push(RetrievedItem {
                id: "1".to_string(),
                content: "test".to_string(),
                score: None,
                source_type: "test".to_string(),
                metadata: BTreeMap::new(),
            });
            Ok(result)
        }
    }

    #[test]
    fn guarded_disabled_uses_fallback() {
        let primary = Box::new(MockRetriever {
            name: "primary".to_string(),
        });
        let fallback = Box::new(MockRetriever {
            name: "fallback".to_string(),
        });
        let guarded = GuardedRetriever::new(primary, fallback, false, vec![]);

        let result = guarded
            .retrieve(&RetrieveQuery::new("user-a", 5))
            .expect("检索应成功");

        assert_eq!(result.retriever_name, "fallback");
        assert_eq!(
            result.candidates[0].metadata.get("retrieval_mode"),
            Some(&"rollout_fallback_rule".to_string())
        );
        assert_eq!(
            result.candidates[0].metadata.get("rollout_reason"),
            Some(&"rollout_disabled".to_string())
        );
    }

    #[test]
    fn guarded_enabled_with_allowlist_miss_uses_fallback() {
        let primary = Box::new(MockRetriever {
            name: "primary".to_string(),
        });
        let fallback = Box::new(MockRetriever {
            name: "fallback".to_string(),
        });
        let guarded =
            GuardedRetriever::new(primary, fallback, true, vec!["user-allowed".to_string()]);

        let result = guarded
            .retrieve(&RetrieveQuery::new("user-blocked", 5))
            .expect("检索应成功");

        assert_eq!(result.retriever_name, "fallback");
        assert_eq!(
            result.candidates[0].metadata.get("rollout_reason"),
            Some(&"user_not_in_allowlist".to_string())
        );
    }

    #[test]
    fn guarded_enabled_with_allowlist_hit_uses_primary() {
        let primary = Box::new(MockRetriever {
            name: "primary".to_string(),
        });
        let fallback = Box::new(MockRetriever {
            name: "fallback".to_string(),
        });
        let guarded =
            GuardedRetriever::new(primary, fallback, true, vec!["user-allowed".to_string()]);

        let result = guarded
            .retrieve(&RetrieveQuery::new("user-allowed", 5))
            .expect("检索应成功");

        assert_eq!(result.retriever_name, "primary");
        assert_eq!(
            result.candidates[0].metadata.get("rollout_allowed"),
            Some(&"true".to_string())
        );
    }

    #[test]
    fn guarded_enabled_empty_allowlist_allows_all() {
        let primary = Box::new(MockRetriever {
            name: "primary".to_string(),
        });
        let fallback = Box::new(MockRetriever {
            name: "fallback".to_string(),
        });
        let guarded = GuardedRetriever::new(primary, fallback, true, vec![]);

        let result = guarded
            .retrieve(&RetrieveQuery::new("any-user", 5))
            .expect("检索应成功");

        assert_eq!(result.retriever_name, "primary");
        assert_eq!(
            result.candidates[0].metadata.get("rollout_allowed"),
            Some(&"true".to_string())
        );
    }
}
