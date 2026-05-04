use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde_json::json;

impl super::TaskStore {
    // -------------------------------------------------------------------------
    // Embedding Cache
    // -------------------------------------------------------------------------

    /// 计算文本的稳定哈希（用于缓存 key）。
    fn text_hash(text: &str) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    /// 从缓存读取 embedding 向量。
    ///
    /// 返回 `None` 表示缓存未命中（或读取失败，失败时记录 warn 日志）。
    pub fn get_embedding(&self, text: &str, model_name: &str) -> Option<Vec<f32>> {
        let hash = Self::text_hash(text);
        let result: Result<Option<(String, i64)>> = self
            .conn
            .query_row(
                "SELECT vector_json, dimension FROM embedding_cache WHERE text_hash = ?1 AND model_name = ?2",
                params![hash, model_name],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()
            .context("查询 embedding_cache 失败");

        match result {
            Ok(Some((vector_json, dimension))) => {
                match serde_json::from_str::<Vec<f32>>(&vector_json) {
                    Ok(vec) => {
                        let expected_dim = vec.len() as i64;
                        if expected_dim == dimension {
                            super::log_task_store_info(
                                "embedding_cache_hit",
                                vec![
                                    ("model_name", json!(model_name)),
                                    ("dimension", json!(dimension)),
                                ],
                            );
                            Some(vec)
                        } else {
                            super::log_task_store_warn(
                                "embedding_cache_dimension_mismatch",
                                vec![
                                    ("model_name", json!(model_name)),
                                    ("cached_dimension", json!(dimension)),
                                    ("actual_dimension", json!(expected_dim)),
                                ],
                            );
                            None
                        }
                    }
                    Err(err) => {
                        super::log_task_store_warn(
                            "embedding_cache_parse_failed",
                            vec![
                                ("model_name", json!(model_name)),
                                ("error", json!(err.to_string())),
                            ],
                        );
                        None
                    }
                }
            }
            Ok(None) => None,
            Err(err) => {
                super::log_task_store_warn(
                    "embedding_cache_read_failed",
                    vec![
                        ("model_name", json!(model_name)),
                        ("error", json!(err.to_string())),
                    ],
                );
                None
            }
        }
    }

    /// 批量从缓存读取 embedding 向量。
    ///
    /// 返回与输入文本一一对应的 `Option<Vec<f32>>` 列表。
    /// 任意一项读取失败都视为未命中（记录 warn 日志），不影响其他项。
    pub fn get_embeddings_batch(
        &self,
        texts: &[String],
        model_name: &str,
    ) -> Vec<Option<Vec<f32>>> {
        texts
            .iter()
            .map(|text| self.get_embedding(text, model_name))
            .collect()
    }

    /// 将 embedding 向量写入缓存。
    ///
    /// 写入失败时记录 warn 日志，不返回错误（透明回退）。
    pub fn put_embedding(&self, text: &str, model_name: &str, vector: &[f32]) {
        let hash = Self::text_hash(text);
        let vector_json = match serde_json::to_string(vector) {
            Ok(json) => json,
            Err(err) => {
                super::log_task_store_warn(
                    "embedding_cache_serialize_failed",
                    vec![
                        ("model_name", json!(model_name)),
                        ("error", json!(err.to_string())),
                    ],
                );
                return;
            }
        };
        let dimension = vector.len() as i64;
        let now = Utc::now().to_rfc3339();

        if let Err(err) = self
            .conn
            .execute(
                r#"
                INSERT INTO embedding_cache (text_hash, model_name, vector_json, dimension, created_at)
                VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT(text_hash, model_name) DO UPDATE SET
                    vector_json = excluded.vector_json,
                    dimension = excluded.dimension,
                    created_at = excluded.created_at
                "#,
                params![hash, model_name, vector_json, dimension, now],
            )
            .context("写入 embedding_cache 失败")
        {
            super::log_task_store_warn(
                "embedding_cache_write_failed",
                vec![
                    ("model_name", json!(model_name)),
                    ("dimension", json!(dimension)),
                    ("error", json!(err.to_string())),
                ],
            );
        }
    }

    /// 批量将 embedding 向量写入缓存。
    ///
    /// 任意一项写入失败都记录 warn 日志，不影响其他项。
    pub fn put_embeddings_batch(&self, texts: &[String], model_name: &str, vectors: &[Vec<f32>]) {
        if texts.len() != vectors.len() {
            super::log_task_store_warn(
                "embedding_cache_batch_mismatch",
                vec![
                    ("text_count", json!(texts.len())),
                    ("vector_count", json!(vectors.len())),
                ],
            );
            return;
        }
        for (text, vector) in texts.iter().zip(vectors.iter()) {
            self.put_embedding(text, model_name, vector);
        }
    }

    /// 清除 embedding_cache 中指定模型的全部条目（用于模型切换时手动清理）。
    pub fn clear_embedding_cache(&self, model_name: &str) -> Result<usize> {
        let count = self
            .conn
            .execute(
                "DELETE FROM embedding_cache WHERE model_name = ?1",
                params![model_name],
            )
            .context("清除 embedding_cache 失败")?;
        super::log_task_store_info(
            "embedding_cache_cleared",
            vec![
                ("model_name", json!(model_name)),
                ("deleted_count", json!(count)),
            ],
        );
        Ok(count)
    }

    /// 获取 embedding_cache 的统计信息：(总条目数, 唯一模型数)。
    pub fn embedding_cache_stats(&self) -> Result<(usize, usize)> {
        let total: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM embedding_cache", [], |row| row.get(0))
            .context("统计 embedding_cache 总数失败")?;
        let models: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(DISTINCT model_name) FROM embedding_cache",
                [],
                |row| row.get(0),
            )
            .context("统计 embedding_cache 模型数失败")?;
        Ok((total as usize, models as usize))
    }
}
