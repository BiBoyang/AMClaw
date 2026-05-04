use anyhow::{bail, Context, Result};
use chrono::Utc;
use rusqlite::params;
use serde_json::json;
use uuid::Uuid;

use super::{
    FeedbackKind, MemoryFeedbackState, MemoryType, MemoryWriteState, PromoteReason, SkipReason,
    UserMemoryRecord, WriteDecision,
};

/// 最大单条内容长度（写入时校验）
const MAX_MEMORY_WRITE_CHARS: usize = 500;
/// 写入门槛：过短内容的最小字符数（3 字符以下视为噪声）
const MIN_MEMORY_WRITE_CHARS: usize = 3;

/// 检查内容是否为噪声（过短或命中黑名单短句）。
/// 黑名单覆盖中文/英文常见无意义短回复。
fn is_memory_noise(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.chars().count() < MIN_MEMORY_WRITE_CHARS {
        return true;
    }
    let lower = trimmed.to_lowercase();
    let blacklist: &[&str] = &[
        "好的",
        "收到",
        "嗯嗯",
        "嗯",
        "哦",
        "啊",
        "行",
        "可以",
        "没问题",
        "知道了",
        "明白",
        "了解",
        "清楚",
        "ok",
        "yes",
        "no",
        "okay",
        "sure",
        "got it",
        "roger",
        "copy",
        "thx",
        "thanks",
        "thank you",
        "1",
        "111",
        "6",
        "666",
        "多谢",
        "谢谢",
        "不客气",
        "客气",
        "再见",
        "拜拜",
        "hello",
        "hi",
        "hey",
    ];
    blacklist.iter().any(|phrase| lower == *phrase)
}

impl super::TaskStore {
    /// 写入显式用户记忆（用户明确要求"记住"）
    #[cfg(test)]
    pub fn add_user_memory(&mut self, user_id: &str, content: &str) -> Result<UserMemoryRecord> {
        self.add_user_memory_typed(user_id, content, MemoryType::Explicit, 100)
    }

    /// 写入带类型和优先级的用户记忆
    pub fn add_user_memory_typed(
        &mut self,
        user_id: &str,
        content: &str,
        memory_type: MemoryType,
        priority: i64,
    ) -> Result<UserMemoryRecord> {
        let user_id = user_id.trim();
        let content = content.trim();
        if user_id.is_empty() || content.is_empty() {
            bail!("user_id/content 不能为空");
        }
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        self.conn
            .execute(
                r#"
                INSERT INTO user_memories (id, user_id, content, memory_type, status, priority, last_used_at, use_count, retrieved_count, injected_count, useful, created_at, updated_at)
                VALUES (?1, ?2, ?3, ?4, 'active', ?5, NULL, 0, 0, 0, 0, ?6, ?7)
                "#,
                params![id, user_id, content, memory_type.as_str(), priority, now.clone(), now.clone()],
            )
            .context("写入 user_memory 失败")?;
        Ok(UserMemoryRecord {
            id,
            user_id: user_id.to_string(),
            content: content.to_string(),
            memory_type,
            status: "active".to_string(),
            priority,
            last_used_at: None,
            use_count: 0,
            retrieved_count: 0,
            injected_count: 0,
            useful: false,
            created_at: now.clone(),
            updated_at: now,
        })
    }

    /// 统一写入治理入口
    ///
    /// 执行：validate → dedup → promote/skip → persist
    /// 返回 WriteDecision，调用方不直接决定是否写入。
    ///
    /// 冲突规则（按优先级链：explicit > project_fact > user_preference > lesson > auto）：
    /// - 高优先级类型可覆盖低优先级类型（promote）
    /// - 低优先级类型不能覆盖高优先级类型（skip）
    /// - 同内容同类型：重复，skip
    pub fn govern_memory_write(
        &mut self,
        user_id: &str,
        content: &str,
        memory_type: MemoryType,
        priority: i64,
        write_state: &mut MemoryWriteState,
    ) -> WriteDecision {
        write_state.candidate_count += 1;
        let content = content.trim();
        let content_preview = if content.chars().count() > 20 {
            let truncated: String = content.chars().take(20).collect();
            format!("{}...", truncated)
        } else {
            content.to_string()
        };

        // 1. Validate: 空/whitespace
        if content.is_empty() {
            let decision = WriteDecision::Skipped {
                content_preview,
                reason: SkipReason::Empty,
            };
            write_state.record(decision.clone());
            return decision;
        }

        // 2. Validate: 超长
        if content.chars().count() > MAX_MEMORY_WRITE_CHARS {
            let decision = WriteDecision::Skipped {
                content_preview,
                reason: SkipReason::TooLong,
            };
            write_state.record(decision.clone());
            return decision;
        }

        // 3. Validate: 噪声过滤（过短 / 黑名单短句）
        if is_memory_noise(content) {
            let decision = WriteDecision::Skipped {
                content_preview,
                reason: SkipReason::Noise,
            };
            write_state.record(decision.clone());
            return decision;
        }

        // 4. Validate: user_id
        if user_id.trim().is_empty() {
            let decision = WriteDecision::Skipped {
                content_preview,
                reason: SkipReason::Invalid,
            };
            write_state.record(decision.clone());
            return decision;
        }

        // 4. Dedup: 检查已有记忆（normalize 后比较：trim + 大小写归一 + 多空格压缩）
        let normalized: String = content
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let existing = self.search_user_memories(user_id, 50);

        match existing {
            Ok(memories) => {
                for mem in &memories {
                    let existing_normalized: String = mem
                        .content
                        .to_lowercase()
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ");
                    if existing_normalized != normalized {
                        continue;
                    }
                    // normalize 后相同 —— 按优先级链决定
                    if memory_type.can_promote(&mem.memory_type) {
                        // 新类型优先级更高：promote
                        if let Err(e) = self.promote_memory(&mem.id, memory_type, priority) {
                            let decision = WriteDecision::Skipped {
                                content_preview,
                                reason: SkipReason::StorageError,
                            };
                            write_state.record(decision.clone());
                            super::log_task_store_warn(
                                "memory_promote_failed",
                                vec![
                                    ("error_kind", json!("promote_failed")),
                                    ("detail", json!(e.to_string())),
                                ],
                            );
                            return decision;
                        }
                        let decision = WriteDecision::Promoted {
                            id: mem.id.clone(),
                            reason: PromoteReason::TypePromotesLower {
                                from: memory_type,
                                to: mem.memory_type,
                            },
                        };
                        write_state.record(decision.clone());
                        return decision;
                    }
                    if mem.memory_type.can_promote(&memory_type) {
                        // 已有类型优先级更高：低优先级不允许覆盖
                        let decision = WriteDecision::Skipped {
                            content_preview,
                            reason: SkipReason::LowerPriorityWouldDowngradeHigher,
                        };
                        write_state.record(decision.clone());
                        return decision;
                    }
                    // 同优先级（同类型重复）
                    let decision = WriteDecision::Skipped {
                        content_preview,
                        reason: SkipReason::Duplicate,
                    };
                    write_state.record(decision.clone());
                    return decision;
                }
            }
            Err(err) => {
                super::log_task_store_warn(
                    "memory_govern_dedup_lookup_failed",
                    vec![
                        ("error_kind", json!("dedup_lookup_failed")),
                        ("detail", json!(err.to_string())),
                    ],
                );
                // 查询失败时 conservative：允许写入
            }
        }

        // 5. 新写入
        match self.add_user_memory_typed(user_id, content, memory_type, priority) {
            Ok(record) => {
                let decision = WriteDecision::Written(Box::new(record));
                write_state.record(decision.clone());
                decision
            }
            Err(_) => {
                let decision = WriteDecision::Skipped {
                    content_preview,
                    reason: SkipReason::StorageError,
                };
                write_state.record(decision.clone());
                decision
            }
        }
    }

    /// 将已有记忆提升为指定类型（更新 type + priority）
    fn promote_memory(
        &self,
        memory_id: &str,
        target_type: MemoryType,
        priority: i64,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn
            .execute(
                "UPDATE user_memories SET memory_type = ?1, priority = ?2, updated_at = ?3 WHERE id = ?4",
                params![target_type.as_str(), priority, now, memory_id],
            )
            .context("提升 memory 失败")?;
        Ok(())
    }

    pub fn list_user_memories(&self, user_id: &str, limit: usize) -> Result<Vec<UserMemoryRecord>> {
        let limit = i64::try_from(limit).context("memory limit 超出范围")?;
        let mut stmt = self
            .conn
            .prepare(
                r#"
                SELECT id, user_id, content, memory_type, status, priority, last_used_at, use_count, retrieved_count, injected_count, useful, created_at, updated_at
                FROM user_memories
                WHERE user_id = ?1 AND status = 'active'
                ORDER BY priority DESC, useful DESC, use_count DESC, COALESCE(last_used_at, updated_at) DESC, id ASC
                LIMIT ?2
                "#,
            )
            .context("准备 user_memory 查询失败")?;
        let rows = stmt
            .query_map(params![user_id, limit], |row| {
                let mt_str: String = row.get(3)?;
                let memory_type = mt_str.parse().unwrap_or_else(|e| {
                    super::log_task_store_warn(
                        "memory_type_unknown_fallback",
                        vec![("raw_type", json!(mt_str)), ("error", json!(e))],
                    );
                    MemoryType::Auto
                });
                Ok(UserMemoryRecord {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    content: row.get(2)?,
                    memory_type,
                    status: row.get(4)?,
                    priority: row.get(5)?,
                    last_used_at: row.get(6)?,
                    use_count: row.get(7)?,
                    retrieved_count: row.get(8)?,
                    injected_count: row.get(9)?,
                    useful: row.get::<_, i64>(10)? != 0,
                    created_at: row.get(11)?,
                    updated_at: row.get(12)?,
                })
            })
            .context("查询 user_memory 失败")?;
        let mut memories = Vec::new();
        for row in rows {
            memories.push(row.context("读取 user_memory 失败")?);
        }
        Ok(memories)
    }

    #[cfg(test)]
    pub fn has_user_memory(&self, user_id: &str, content: &str) -> Result<bool> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM user_memories WHERE user_id = ?1 AND content = ?2 AND status = 'active'",
                params![user_id, content],
                |row| row.get(0),
            )
            .context("查询 user_memory 去重失败")?;
        Ok(count > 0)
    }

    /// 检索 active 记忆（排序后返回，不含裁剪逻辑）
    /// 排序：priority DESC > useful DESC > use_count DESC > last_used_at DESC > id ASC
    /// 裁剪（去重 + 预算）由上层 SessionState 负责
    pub fn search_user_memories(
        &self,
        user_id: &str,
        limit: usize,
    ) -> Result<Vec<UserMemoryRecord>> {
        let limit = i64::try_from(limit).context("memory limit 超出范围")?;
        let mut stmt = self
            .conn
            .prepare(
                r#"
                SELECT id, user_id, content, memory_type, status, priority, last_used_at, use_count, retrieved_count, injected_count, useful, created_at, updated_at
                FROM user_memories
                WHERE user_id = ?1 AND status = 'active'
                ORDER BY priority DESC, useful DESC, use_count DESC, COALESCE(last_used_at, updated_at) DESC, id ASC
                LIMIT ?2
                "#,
            )
            .context("准备 user_memory 检索失败")?;
        let rows = stmt
            .query_map(params![user_id, limit], |row| {
                let mt_str: String = row.get(3)?;
                let memory_type = mt_str.parse().unwrap_or_else(|e| {
                    super::log_task_store_warn(
                        "memory_type_unknown_fallback",
                        vec![("raw_type", json!(mt_str)), ("error", json!(e))],
                    );
                    MemoryType::Auto
                });
                Ok(UserMemoryRecord {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    content: row.get(2)?,
                    memory_type,
                    status: row.get(4)?,
                    priority: row.get(5)?,
                    last_used_at: row.get(6)?,
                    use_count: row.get(7)?,
                    retrieved_count: row.get(8)?,
                    injected_count: row.get(9)?,
                    useful: row.get::<_, i64>(10)? != 0,
                    created_at: row.get(11)?,
                    updated_at: row.get(12)?,
                })
            })
            .context("检索 user_memory 失败")?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("读取 user_memory 失败")?);
        }
        Ok(results)
    }

    /// 统一 feedback 写回入口
    ///
    /// 将 MemoryFeedbackState 中记录的 feedback 一次性写回长期字段：
    /// - Retrieved: retrieved_count += N
    /// - Injected: injected_count += N
    /// - Useful: use_count += N, useful = 1, last_used_at = now
    pub fn apply_memory_feedback(&self, feedback_state: &MemoryFeedbackState) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        for memory_id in feedback_state.memory_ids() {
            let retrieved = feedback_state.retrieved_count(&memory_id);
            let injected = feedback_state.injected_count(&memory_id);
            let useful = feedback_state.useful_count(&memory_id);

            if retrieved > 0 {
                self.conn.execute(
                    "UPDATE user_memories SET retrieved_count = retrieved_count + ?1 WHERE id = ?2",
                    params![retrieved as i64, memory_id],
                ).context("更新 retrieved_count 失败")?;
            }
            if injected > 0 {
                self.conn.execute(
                    "UPDATE user_memories SET injected_count = injected_count + ?1 WHERE id = ?2",
                    params![injected as i64, memory_id],
                ).context("更新 injected_count 失败")?;
            }
            if useful > 0 {
                self.conn.execute(
                    "UPDATE user_memories SET use_count = use_count + ?1, useful = 1, last_used_at = ?2 WHERE id = ?3",
                    params![useful as i64, now.clone(), memory_id],
                ).context("更新 useful/use_count 失败")?;
            }
        }
        Ok(())
    }

    /// 用户显式确认某条记忆"有用"
    ///
    /// - 校验该记忆归属于当前用户且仍为 active
    /// - 统一走 apply_memory_feedback 写回 Useful
    pub fn confirm_memory_useful(&self, user_id: &str, memory_id: &str) -> Result<()> {
        let exists: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM user_memories WHERE id = ?1 AND user_id = ?2 AND status = 'active'",
                params![memory_id, user_id],
                |row| row.get(0),
            )
            .context("校验 useful memory 归属失败")?;
        if exists == 0 {
            bail!("未找到该记忆，或无权标记有用: {memory_id}");
        }
        let mut feedback_state = MemoryFeedbackState::default();
        feedback_state.record(memory_id, FeedbackKind::Useful);
        self.apply_memory_feedback(&feedback_state)
    }

    /// 软删除：将 status 设为 'suppressed'
    pub fn suppress_memory(&self, user_id: &str, memory_id: &str) -> Result<()> {
        let affected = self
            .conn
            .execute(
                "UPDATE user_memories SET status = 'suppressed' WHERE id = ?1 AND user_id = ?2 AND status = 'active'",
                params![memory_id, user_id],
            )
            .context("抑制 memory 失败")?;
        if affected == 0 {
            bail!("未找到该记忆，或无权屏蔽: {memory_id}");
        }
        Ok(())
    }
}
