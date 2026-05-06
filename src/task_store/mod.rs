use anyhow::{Context, Result};
use rusqlite::Connection;
use std::fs;
use std::path::Path;

mod chunk_queue;
mod embedding_cache;
mod logging;
mod memory;
mod schema;
mod sessions;
mod tasks;
mod types;
mod url_guard;

pub use self::types::{
    ArchivedTaskRecord, ClaimableTaskRecord, FeedbackKind, LinkTaskRecord, MarkTaskArchivedInput,
    MemoryFeedbackState, MemoryType, MemoryWriteState, PendingChunkRecord, PendingTaskRecord,
    PromoteReason, RecentTaskRecord, SkipReason, StoredSessionRecord, TaskContentRecord,
    TaskStatusRecord, TaskStoreError, UserMemoryRecord, UserSessionStateRecord, WriteDecision,
};

/// 最大单条内容长度（写入时校验）

#[derive(Debug)]
pub struct TaskStore {
    conn: Connection,
}

impl TaskStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建数据库目录失败: {}", parent.display()))?;
        }

        let conn = Connection::open(path)
            .with_context(|| format!("打开 SQLite 数据库失败: {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_millis(5000))
            .with_context(|| format!("设置 SQLite busy_timeout 失败: {}", path.display()))?;
        for attempt in 0..5 {
            match conn.pragma_update(None, "journal_mode", "WAL") {
                Ok(_) => break,
                Err(rusqlite::Error::SqliteFailure(err, _))
                    if err.code == rusqlite::ErrorCode::DatabaseBusy =>
                {
                    if attempt == 4 {
                        return Err(rusqlite::Error::SqliteFailure(err, None)).with_context(|| {
                            format!("设置 SQLite WAL 模式失败: {}", path.display())
                        });
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("设置 SQLite WAL 模式失败: {}", path.display()));
                }
            }
        }
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }
}

#[cfg(test)]
pub(super) use self::logging::build_task_store_log_payload;
pub(super) use self::logging::{
    log_task_store_error, log_task_store_info, log_task_store_warn, summarize_text_for_log,
};
#[cfg(test)]
pub(super) use self::url_guard::is_private_host_with;
pub use self::url_guard::is_private_url;
pub(super) use self::url_guard::{normalize_url, source_domain};

#[cfg(test)]
mod tests;
