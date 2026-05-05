use crate::task_store::PendingChunkRecord;
use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::params;
use serde_json::json;

impl super::TaskStore {
    /// 将剩余未发送的消息段持久化，供后续补发。
    pub fn insert_pending_chunks(
        &mut self,
        user_id: &str,
        context_token: &str,
        chunks: &[(usize, usize, String)],
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let tx = self.conn.transaction().context("开启 chunks 事务失败")?;
        for (chunk_index, chunk_total, chunk_text) in chunks {
            tx.execute(
                r#"
                INSERT INTO outbound_pending_chunks
                (user_id, context_token, chunk_text, chunk_index, chunk_total, created_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                "#,
                params![
                    user_id,
                    context_token,
                    chunk_text,
                    *chunk_index as i64,
                    *chunk_total as i64,
                    now.clone(),
                ],
            )
            .context("插入 pending chunk 失败")?;
        }
        tx.commit().context("提交 chunks 事务失败")?;
        super::log_task_store_info(
            "pending_chunks_inserted",
            vec![
                ("user_id", json!(user_id)),
                ("chunk_count", json!(chunks.len())),
            ],
        );
        Ok(())
    }

    /// 查询最早的一批待补发消息段（按 created_at 排序）。
    pub fn list_pending_chunks(&self, limit: usize) -> Result<Vec<PendingChunkRecord>> {
        let limit = i64::try_from(limit).context("chunk limit 超出范围")?;
        let mut stmt = self
            .conn
            .prepare(
                r#"
                SELECT id, user_id, context_token, chunk_text, chunk_index, chunk_total, created_at
                FROM outbound_pending_chunks
                ORDER BY created_at ASC
                LIMIT ?1
                "#,
            )
            .context("准备 pending chunks 查询失败")?;
        let rows = stmt
            .query_map([limit], |row| {
                Ok(PendingChunkRecord {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    context_token: row.get(2)?,
                    chunk_text: row.get(3)?,
                    chunk_index: row.get::<_, usize>(4)?,
                    chunk_total: row.get::<_, usize>(5)?,
                    created_at: row.get(6)?,
                })
            })
            .context("查询 pending chunks 失败")?;
        let mut chunks = Vec::new();
        for row in rows {
            chunks.push(row.context("读取 pending chunk 记录失败")?);
        }
        Ok(chunks)
    }

    /// 删除指定待补发消息段。
    pub fn delete_pending_chunk(&mut self, id: i64) -> Result<bool> {
        let deleted = self
            .conn
            .execute("DELETE FROM outbound_pending_chunks WHERE id = ?1", [id])
            .context("删除 pending chunk 失败")?;
        Ok(deleted > 0)
    }
}
