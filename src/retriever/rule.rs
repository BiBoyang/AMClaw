use crate::retriever::{RetrieveQuery, RetrieveResult, RetrievedItem, Retriever};
use crate::task_store::{TaskStore, UserMemoryRecord};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

const DEFAULT_RETRIEVE_LIMIT: usize = 15;

/// 规则法检索器 —— 等价现有 `task_store.search_user_memories` 行为。
///
/// 排序：priority DESC > useful DESC > use_count DESC > last_used_at DESC > id ASC
/// 不做 embedding / 语义相似度计算。
pub struct RuleRetriever {
    db_path: PathBuf,
    name: String,
}

impl RuleRetriever {
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
            name: "rule_v1".to_string(),
        }
    }

    /// 允许自定义名称（用于 A/B 对比时区分不同规则版本）
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
}

impl Retriever for RuleRetriever {
    fn retrieve(&self, query: &RetrieveQuery) -> Result<RetrieveResult> {
        let started = Instant::now();
        let store = TaskStore::open(&self.db_path)
            .with_context(|| format!("RuleRetriever 打开 task_store 失败: {:?}", self.db_path))?;

        let limit = if query.limit > 0 {
            query.limit
        } else {
            DEFAULT_RETRIEVE_LIMIT
        };

        let records = store
            .search_user_memories(&query.user_id, limit)
            .with_context(|| "RuleRetriever 检索 user_memories 失败")?;

        let candidates: Vec<RetrievedItem> = records.into_iter().map(record_to_item).collect();
        let latency_ms = started.elapsed().as_millis().max(1);

        Ok(RetrieveResult {
            candidates,
            hit_count: 0,     // 由上层 SessionState 裁剪后回填
            dropped_count: 0, // 同上
            latency_ms,
            retriever_name: self.name.clone(),
        })
    }
}

fn record_to_item(record: UserMemoryRecord) -> RetrievedItem {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "memory_type".to_string(),
        record.memory_type.as_str().to_string(),
    );
    metadata.insert("priority".to_string(), record.priority.to_string());
    metadata.insert("status".to_string(), record.status.clone());
    metadata.insert("use_count".to_string(), record.use_count.to_string());
    metadata.insert(
        "retrieved_count".to_string(),
        record.retrieved_count.to_string(),
    );
    metadata.insert(
        "injected_count".to_string(),
        record.injected_count.to_string(),
    );
    metadata.insert("useful".to_string(), record.useful.to_string());
    if let Some(last_used_at) = &record.last_used_at {
        metadata.insert("last_used_at".to_string(), last_used_at.clone());
    }
    metadata.insert("created_at".to_string(), record.created_at.clone());
    metadata.insert("updated_at".to_string(), record.updated_at.clone());

    // score 用 priority 归一化到 0-1（规则法的简化评分）
    let score = Some(normalize_priority(record.priority));

    RetrievedItem {
        id: record.id,
        content: record.content,
        score,
        source_type: record.memory_type.as_str().to_string(),
        metadata,
    }
}

/// 将 priority（0-100 常见范围）归一化到 0.0-1.0
fn normalize_priority(priority: i64) -> f64 {
    (priority as f64 / 100.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retriever::Retriever;
    use crate::task_store::{MemoryType, TaskStore};
    use std::env::temp_dir;
    use uuid::Uuid;

    fn temp_db_path() -> PathBuf {
        temp_dir().join(format!("amclaw_retriever_test_{}.db", Uuid::new_v4()))
    }

    #[test]
    fn rule_retriever_returns_same_order_as_task_store_search() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");

        // 按不同 priority 写入
        store
            .add_user_memory_typed("user-a", "低优先级", MemoryType::Auto, 60)
            .expect("写入失败");
        store
            .add_user_memory_typed("user-a", "高优先级", MemoryType::Explicit, 100)
            .expect("写入失败");
        store
            .add_user_memory_typed("user-a", "中优先级", MemoryType::UserPreference, 80)
            .expect("写入失败");

        // 直接查 task_store
        let direct = store.search_user_memories("user-a", 15).expect("查询失败");

        // 通过 RuleRetriever
        let retriever = RuleRetriever::new(&db_path);
        let result = retriever
            .retrieve(&RetrieveQuery::new("user-a", 15))
            .expect("检索失败");

        assert_eq!(result.candidates.len(), 3);
        assert_eq!(result.retriever_name, "rule_v1");

        // 顺序应与 task_store 一致（priority DESC）
        for (direct_rec, item) in direct.iter().zip(result.candidates.iter()) {
            assert_eq!(direct_rec.id, item.id);
            assert_eq!(direct_rec.content, item.content);
        }

        // 优先级顺序验证
        assert_eq!(result.candidates[0].content, "高优先级");
        assert_eq!(result.candidates[1].content, "中优先级");
        assert_eq!(result.candidates[2].content, "低优先级");
    }

    #[test]
    fn retrieve_result_contains_retriever_name_and_counts() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        store
            .add_user_memory_typed("user-b", "测试内容", MemoryType::ProjectFact, 85)
            .expect("写入失败");

        let retriever = RuleRetriever::new(&db_path);
        let result = retriever
            .retrieve(&RetrieveQuery::new("user-b", 10))
            .expect("检索失败");

        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.retriever_name, "rule_v1");
        assert_eq!(result.hit_count, 0); // 上层未裁剪，保持默认值
        assert_eq!(result.dropped_count, 0);
        assert!(result.latency_ms < 1000, "检索应在 1 秒内完成");
    }

    #[test]
    fn rule_retriever_custom_name() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        store
            .add_user_memory_typed("user-c", "内容", MemoryType::Lesson, 75)
            .expect("写入失败");

        let retriever = RuleRetriever::new(&db_path).with_name("rule_v2_test");
        let result = retriever
            .retrieve(&RetrieveQuery::new("user-c", 10))
            .expect("检索失败");

        assert_eq!(result.retriever_name, "rule_v2_test");
    }

    #[test]
    fn rule_retriever_user_isolation() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        store
            .add_user_memory_typed("user-x", "X 的记忆", MemoryType::UserPreference, 80)
            .expect("写入失败");
        store
            .add_user_memory_typed("user-y", "Y 的记忆", MemoryType::Lesson, 75)
            .expect("写入失败");

        let retriever = RuleRetriever::new(&db_path);
        let result_x = retriever
            .retrieve(&RetrieveQuery::new("user-x", 10))
            .expect("检索失败");
        let result_y = retriever
            .retrieve(&RetrieveQuery::new("user-y", 10))
            .expect("检索失败");

        assert_eq!(result_x.candidates.len(), 1);
        assert_eq!(result_x.candidates[0].content, "X 的记忆");
        assert_eq!(result_y.candidates.len(), 1);
        assert_eq!(result_y.candidates[0].content, "Y 的记忆");
    }

    #[test]
    fn rule_retriever_returns_metadata_with_memory_type_and_priority() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        store
            .add_user_memory_typed("user-d", "元数据测试", MemoryType::ProjectFact, 85)
            .expect("写入失败");

        let retriever = RuleRetriever::new(&db_path);
        let result = retriever
            .retrieve(&RetrieveQuery::new("user-d", 10))
            .expect("检索失败");

        let item = &result.candidates[0];
        assert_eq!(
            item.metadata.get("memory_type"),
            Some(&"project_fact".to_string())
        );
        assert_eq!(item.metadata.get("priority"), Some(&"85".to_string()));
        assert_eq!(item.source_type, "project_fact");
        assert!((item.score.unwrap() - 0.85).abs() < f64::EPSILON);
        assert!(item.metadata.contains_key("created_at"));
        assert!(item.metadata.contains_key("updated_at"));
    }

    #[test]
    fn rule_retriever_empty_result_for_unknown_user() {
        let db_path = temp_db_path();
        let retriever = RuleRetriever::new(&db_path);
        let result = retriever
            .retrieve(&RetrieveQuery::new("unknown-user", 10))
            .expect("检索应成功，即使无结果");

        assert!(result.candidates.is_empty());
        assert_eq!(result.retriever_name, "rule_v1");
    }
}
