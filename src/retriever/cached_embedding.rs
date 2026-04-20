use crate::retriever::embedding::EmbeddingProvider;
use crate::task_store::TaskStore;
use anyhow::Result;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

/// 带持久化缓存的 EmbeddingProvider 装饰器。
///
/// 设计约束：
/// - 缓存 miss 时透明回退到 inner provider，成功后写入缓存
/// - 缓存读写失败都不阻塞主流程（记录 warn 日志）
/// - key = hash(text) + model_name（切换模型时缓存自动失效）
/// - 命中收益通过内部计数器统计，也可通过日志 event 观测
pub struct CachedEmbeddingProvider {
    inner: Box<dyn EmbeddingProvider + Send + Sync>,
    db_path: PathBuf,
    model_name: String,
    hit_count: AtomicUsize,
    miss_count: AtomicUsize,
}

impl CachedEmbeddingProvider {
    pub fn new(
        inner: Box<dyn EmbeddingProvider + Send + Sync>,
        db_path: impl Into<PathBuf>,
    ) -> Self {
        let model_name = inner.model_name().to_string();
        Self {
            inner,
            db_path: db_path.into(),
            model_name,
            hit_count: AtomicUsize::new(0),
            miss_count: AtomicUsize::new(0),
        }
    }

    pub fn hit_count(&self) -> usize {
        self.hit_count.load(Ordering::SeqCst)
    }

    pub fn miss_count(&self) -> usize {
        self.miss_count.load(Ordering::SeqCst)
    }

    /// 获取底层 provider 的模型名（用于缓存 key）。
    fn cache_model_name(&self) -> &str {
        &self.model_name
    }

    /// 内部辅助：打开 task_store 连接读取缓存。
    fn get_cached(&self, text: &str) -> Option<Vec<f32>> {
        let store = TaskStore::open(&self.db_path).ok()?;
        store.get_embedding(text, self.cache_model_name())
    }

    /// 内部辅助：打开 task_store 连接写入缓存。
    fn put_cached(&self, text: &str, vector: &[f32]) {
        if let Ok(store) = TaskStore::open(&self.db_path) {
            store.put_embedding(text, self.cache_model_name(), vector);
        }
    }

    /// 内部辅助：批量读取缓存。
    fn get_cached_batch(&self, texts: &[String]) -> Vec<Option<Vec<f32>>> {
        let store = match TaskStore::open(&self.db_path) {
            Ok(s) => s,
            Err(_) => return vec![None; texts.len()],
        };
        store.get_embeddings_batch(texts, self.cache_model_name())
    }

    /// 内部辅助：批量写入缓存。
    fn put_cached_batch(&self, texts: &[String], vectors: &[Vec<f32>]) {
        if let Ok(store) = TaskStore::open(&self.db_path) {
            store.put_embeddings_batch(texts, self.cache_model_name(), vectors);
        }
    }
}

impl EmbeddingProvider for CachedEmbeddingProvider {
    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        // 1. 尝试读缓存
        if let Some(cached) = self.get_cached(text) {
            self.hit_count.fetch_add(1, Ordering::SeqCst);
            return Ok(cached);
        }

        // 2. 缓存 miss，调用 inner provider
        let started = Instant::now();
        let vector = self.inner.embed_query(text)?;
        let latency_ms = started.elapsed().as_millis();

        // 3. 写入缓存（不阻塞返回）
        self.put_cached(text, &vector);
        self.miss_count.fetch_add(1, Ordering::SeqCst);

        // 4. 记录 miss 日志（含实际 latency，用于计算命中收益）
        crate::logging::emit_structured_log(
            "info",
            "embedding_cache_miss",
            vec![
                ("model_name", json!(self.cache_model_name())),
                ("latency_ms", json!(latency_ms)),
                ("text_chars", json!(text.chars().count())),
            ],
        );

        Ok(vector)
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // 1. 批量尝试读缓存
        let cached = self.get_cached_batch(texts);
        let miss_count = cached.iter().filter(|v| v.is_none()).count();

        if miss_count == 0 {
            // 全部命中
            self.hit_count.fetch_add(texts.len(), Ordering::SeqCst);
            crate::logging::emit_structured_log(
                "info",
                "embedding_cache_hit_batch",
                vec![
                    ("model_name", json!(self.cache_model_name())),
                    ("batch_size", json!(texts.len())),
                    ("hit_count", json!(texts.len())),
                ],
            );
            return Ok(cached.into_iter().flatten().collect());
        }

        // 2. 收集未命中的索引和文本
        let mut miss_indices = Vec::with_capacity(miss_count);
        let mut miss_texts = Vec::with_capacity(miss_count);
        for (idx, vec_opt) in cached.iter().enumerate() {
            if vec_opt.is_none() {
                miss_indices.push(idx);
                miss_texts.push(texts[idx].clone());
            }
        }

        // 3. 调用 inner provider 批量编码未命中项
        let started = Instant::now();
        let miss_vectors = self.inner.embed_documents(&miss_texts)?;
        let latency_ms = started.elapsed().as_millis();

        if miss_vectors.len() != miss_texts.len() {
            anyhow::bail!(
                "embed_documents batch size mismatch: requested={}, returned={}",
                miss_texts.len(),
                miss_vectors.len()
            );
        }

        // 4. 写入缓存
        self.put_cached_batch(&miss_texts, &miss_vectors);
        self.hit_count
            .fetch_add(texts.len() - miss_count, Ordering::SeqCst);
        self.miss_count.fetch_add(miss_count, Ordering::SeqCst);

        // 5. 合并结果（命中 + 新编码）
        let mut result = cached;
        for (miss_idx_in_batch, vec) in miss_vectors.into_iter().enumerate() {
            let original_idx = miss_indices[miss_idx_in_batch];
            result[original_idx] = Some(vec);
        }

        crate::logging::emit_structured_log(
            "info",
            "embedding_cache_miss_batch",
            vec![
                ("model_name", json!(self.cache_model_name())),
                ("batch_size", json!(texts.len())),
                ("hit_count", json!(texts.len() - miss_count)),
                ("miss_count", json!(miss_count)),
                ("latency_ms", json!(latency_ms)),
            ],
        );

        Ok(result.into_iter().flatten().collect())
    }

    fn model_name(&self) -> &str {
        &self.model_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_store::TaskStore;
    use std::env::temp_dir;
    use uuid::Uuid;

    fn temp_db_path() -> PathBuf {
        temp_dir().join(format!(
            "amclaw_cached_embedding_test_{}.db",
            Uuid::new_v4()
        ))
    }

    struct FakeProvider;

    impl EmbeddingProvider for FakeProvider {
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
            "fake_cached_model"
        }
    }

    #[test]
    fn cached_provider_miss_then_hit() {
        let db_path = temp_db_path();
        let cached = CachedEmbeddingProvider::new(Box::new(FakeProvider), &db_path);

        // 第一次：miss
        let v1 = cached.embed_query("hello").unwrap();
        assert_eq!(v1.len(), 4);
        assert_eq!(cached.miss_count(), 1);
        assert_eq!(cached.hit_count(), 0);

        // 第二次：hit
        let v2 = cached.embed_query("hello").unwrap();
        assert_eq!(v1, v2);
        assert_eq!(cached.miss_count(), 1);
        assert_eq!(cached.hit_count(), 1);
    }

    #[test]
    fn cached_provider_batch_partial_hit() {
        let db_path = temp_db_path();
        let cached = CachedEmbeddingProvider::new(Box::new(FakeProvider), &db_path);

        // 先缓存 "a"
        let _ = cached.embed_query("a").unwrap();
        assert_eq!(cached.miss_count(), 1);

        // 批量："a" 命中，"b" miss
        let texts = vec!["a".to_string(), "b".to_string()];
        let results = cached.embed_documents(&texts).unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(cached.hit_count(), 1);
        assert_eq!(cached.miss_count(), 2);
    }

    #[test]
    fn cached_provider_empty_batch() {
        let db_path = temp_db_path();
        let cached = CachedEmbeddingProvider::new(Box::new(FakeProvider), &db_path);

        let results = cached.embed_documents(&[]).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn cached_provider_model_name_returns_inner() {
        let db_path = temp_db_path();
        let cached = CachedEmbeddingProvider::new(Box::new(FakeProvider), &db_path);

        assert_eq!(cached.model_name(), "fake_cached_model");
    }

    #[test]
    fn cached_provider_survives_db_missing() {
        // db 路径不存在父目录，缓存应透明失败，不影响 inner provider
        let bad_path = "/nonexistent/dir/test.db";
        let cached = CachedEmbeddingProvider::new(Box::new(FakeProvider), bad_path);

        // 仍应能返回结果（只是不缓存）
        let result = cached.embed_query("test").unwrap();
        assert_eq!(result.len(), 4);
        assert_eq!(cached.miss_count(), 1);
    }

    #[test]
    fn task_store_embedding_cache_roundtrip() {
        let db_path = temp_db_path();
        let store = TaskStore::open(&db_path).unwrap();

        // 写入
        store.put_embedding("hello world", "test-model", &[0.1, 0.2, 0.3]);

        // 读取命中
        let cached = store.get_embedding("hello world", "test-model");
        assert!(cached.is_some());
        let vec = cached.unwrap();
        assert_eq!(vec, vec![0.1, 0.2, 0.3]);

        // 不同文本 miss
        let missed = store.get_embedding("different text", "test-model");
        assert!(missed.is_none());

        // 不同模型 miss
        let missed_model = store.get_embedding("hello world", "other-model");
        assert!(missed_model.is_none());
    }

    #[test]
    fn task_store_embedding_cache_batch_roundtrip() {
        let db_path = temp_db_path();
        let store = TaskStore::open(&db_path).unwrap();

        let texts = vec!["a".to_string(), "b".to_string()];
        store.put_embeddings_batch(&texts, "batch-model", &[vec![1.0, 2.0], vec![3.0, 4.0]]);

        let results = store.get_embeddings_batch(&texts, "batch-model");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0], Some(vec![1.0, 2.0]));
        assert_eq!(results[1], Some(vec![3.0, 4.0]));
    }

    #[test]
    fn task_store_embedding_cache_stats() {
        let db_path = temp_db_path();
        let store = TaskStore::open(&db_path).unwrap();

        let (total, models) = store.embedding_cache_stats().unwrap();
        assert_eq!(total, 0);
        assert_eq!(models, 0);

        store.put_embedding("x", "model-a", &[1.0]);
        store.put_embedding("y", "model-a", &[2.0]);
        store.put_embedding("z", "model-b", &[3.0]);

        let (total, models) = store.embedding_cache_stats().unwrap();
        assert_eq!(total, 3);
        assert_eq!(models, 2);
    }

    #[test]
    fn task_store_embedding_cache_clear() {
        let db_path = temp_db_path();
        let store = TaskStore::open(&db_path).unwrap();

        store.put_embedding("x", "model-a", &[1.0]);
        store.put_embedding("y", "model-b", &[2.0]);

        let deleted = store.clear_embedding_cache("model-a").unwrap();
        assert_eq!(deleted, 1);

        assert!(store.get_embedding("x", "model-a").is_none());
        assert!(store.get_embedding("y", "model-b").is_some());
    }
}
