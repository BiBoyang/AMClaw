use super::{log_chat_error, log_chat_info, log_chat_warn, summarize_text_for_log};
use crate::agent_core::{
    merge_string_arrays_with_runtime_reserve, AgentRunContext, RuntimeSessionStateSnapshot,
};
use crate::session_router::FlushReason;
use crate::task_store::UserSessionStateRecord;
use chrono::Utc;
use serde_json::json;

pub(super) fn split_reply_into_chunks(reply: &str, max_chars: usize) -> Vec<String> {
    let total_chars = reply.chars().count();
    if total_chars <= max_chars {
        return vec![reply.to_string()];
    }

    // 递归收敛：先按 max_chars 切内容，得到总段数 n；
    // 再用实际前缀长度（i/n 前缀）重新切分，直到稳定。
    let mut prev_count = 0usize;
    let mut segments: Vec<String> = Vec::new();

    for _ in 0..5 {
        // 保守预算：max_chars 减去最长前缀的字符数（最后一段前缀最长）
        let longest_prefix = if segments.is_empty() {
            "（1/2）".chars().count()
        } else {
            format!("（{}/{}）", segments.len(), segments.len())
                .chars()
                .count()
        };
        let content_budget = max_chars.saturating_sub(longest_prefix);
        if content_budget == 0 {
            break;
        }

        segments = split_content_only(reply, content_budget);
        if segments.len() == prev_count {
            break; // 已收敛
        }
        prev_count = segments.len();
    }

    // 安全兜底：若 budget 过小导致无法收敛，退化为无前缀逐字符切分，
    // 保证单段仍不超过 max_chars。
    let fallback_no_prefix = segments.is_empty();
    if fallback_no_prefix {
        segments = split_content_only(reply, max_chars);
    }

    let total = segments.len();
    if fallback_no_prefix {
        segments
    } else {
        segments
            .into_iter()
            .enumerate()
            .map(|(i, content)| format!("（{}/{}）{}", i + 1, total, content))
            .collect()
    }
}

/// 仅按 content_budget 切分内容，不添加前缀。返回每段内容的 Vec<String>。
fn split_content_only(reply: &str, content_budget: usize) -> Vec<String> {
    if reply.is_empty() {
        return Vec::new();
    }
    // 0-budget 防御：无法分片时直接返回全文，避免进入死循环
    if content_budget == 0 {
        return vec![reply.to_string()];
    }
    let mut result = Vec::new();
    let mut char_indices = reply.char_indices().peekable();
    let mut start_byte = 0usize;

    while char_indices.peek().is_some() {
        let mut chars_count = 0usize;
        let mut end_byte = reply.len();
        while let Some((byte_idx, _)) = char_indices.peek() {
            if chars_count >= content_budget {
                end_byte = *byte_idx;
                break;
            }
            chars_count += 1;
            char_indices.next();
        }
        result.push(reply[start_byte..end_byte].to_string());
        start_byte = end_byte;
    }
    result
}

/// 判断是否应该发送“处理中”回执：trim 后长度达到阈值才回执。
pub(super) fn should_send_processing_ack(user_text: &str) -> bool {
    user_text.trim().chars().count() >= super::PROCESSING_ACK_MIN_INPUT_CHARS
}

/// 定时报告推送类型（区分 daily / weekly 的日志事件与状态字段）。
#[derive(Debug, Clone, Copy)]
pub(super) enum ReportPushKind {
    Daily,
    Weekly,
}

impl ReportPushKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
        }
    }

    /// 日志字段名：日报用 day、周报用 week。
    fn period_field(self) -> &'static str {
        match self {
            Self::Daily => "day",
            Self::Weekly => "week",
        }
    }
}

/// 计算分段发送在 failed_idx（0-based）处失败后需要持久化补发的剩余段，
/// 返回 (1-based chunk_index, chunk_total, chunk_text) 列表。
pub(super) fn remaining_chunks_after_failure(
    chunks: &[String],
    failed_idx: usize,
) -> Vec<(usize, usize, String)> {
    let chunk_total = chunks.len();
    chunks[failed_idx..]
        .iter()
        .enumerate()
        .map(|(offset, text)| (failed_idx + offset + 1, chunk_total, text.clone()))
        .collect()
}

impl super::WeChatBot {
    pub(super) fn next_poll_timeout(&self) -> std::time::Duration {
        self.session_router
            .next_flush_delay(std::time::Instant::now())
            .map(|delay| {
                delay
                    .max(super::MIN_GET_UPDATES_TIMEOUT)
                    .min(super::DEFAULT_GET_UPDATES_TIMEOUT)
            })
            .unwrap_or(super::DEFAULT_GET_UPDATES_TIMEOUT)
    }

    pub(super) fn send_generated_reply(
        &mut self,
        user_id: &str,
        merged_text: &str,
        message_ids: &[String],
        reason: FlushReason,
    ) {
        if merged_text.trim().is_empty() {
            return;
        }

        log_chat_info(
            "session_flushed",
            vec![
                ("user_id", json!(user_id)),
                ("trigger", json!(reason.as_str())),
                ("message_ids", json!(message_ids)),
                ("message_count", json!(message_ids.len())),
                ("text_chars", json!(merged_text.chars().count())),
                (
                    "text_preview",
                    json!(summarize_text_for_log(merged_text, 160)),
                ),
            ],
        );

        log_chat_info(
            "agent_reply_started",
            vec![
                ("user_id", json!(user_id)),
                ("trigger", json!(reason.as_str())),
                ("message_ids", json!(message_ids)),
                ("message_count", json!(message_ids.len())),
            ],
        );

        // 长输入先发“处理中”回执，避免用户空等
        if should_send_processing_ack(merged_text) {
            self.send_reply_text(user_id, super::PROCESSING_ACK_TEXT);
        }

        // C2: 加载持久化 SessionState
        let session_state = match self.task_store.load_user_session_state(user_id) {
            Ok(state) => {
                log_chat_info(
                    "session_state_loaded",
                    vec![
                        ("user_id", json!(user_id)),
                        ("state_present", json!(state.is_some())),
                        (
                            "state_source",
                            json!(if state.is_some() { "db" } else { "none" }),
                        ),
                    ],
                );
                state
            }
            Err(err) => {
                log_chat_warn(
                    "session_state_load_failed",
                    vec![
                        ("user_id", json!(user_id)),
                        ("error_kind", json!("session_state_load_failed")),
                        ("detail", json!(err.to_string())),
                    ],
                );
                None
            }
        };

        let context = AgentRunContext::wechat_chat(user_id, reason.as_str(), message_ids.to_vec())
            .with_session_text(merged_text)
            .with_context_token_present(self.context_token_map.contains_key(user_id))
            .with_user_session_state(session_state);

        let (reply, run_id, trace_json_path, runtime_session_state) =
            self.generate_reply(user_id, merged_text, message_ids, reason, context);

        // C2: agent 完成后按保守策略 merge runtime session state 并回写持久层
        let mut state_updated = false;
        match self.task_store.load_user_session_state(user_id) {
            Ok(Some(state)) => {
                let mut updated = state.clone();
                if let Some(runtime) = runtime_session_state.as_ref() {
                    state_updated =
                        merge_runtime_session_state_into_persistent(&mut updated, runtime);
                }
                updated.updated_at = Utc::now().to_rfc3339();
                if let Err(err) = self.task_store.upsert_user_session_state(&updated) {
                    log_chat_warn(
                        "session_state_upsert_failed",
                        vec![
                            ("user_id", json!(user_id)),
                            ("error_kind", json!("session_state_upsert_failed")),
                            ("detail", json!(err.to_string())),
                        ],
                    );
                    state_updated = false;
                } else {
                    log_chat_info(
                        "session_state_refreshed",
                        vec![
                            ("user_id", json!(user_id)),
                            ("state_updated", json!(state_updated)),
                            ("v2_slots_populated", json!(updated.populated_slot_count())),
                        ],
                    );
                }
            }
            Ok(None) => {
                log_chat_info(
                    "session_state_noop",
                    vec![
                        ("user_id", json!(user_id)),
                        ("reason", json!("no_persistent_state")),
                    ],
                );
            }
            Err(err) => {
                log_chat_warn(
                    "session_state_upsert_read_failed",
                    vec![
                        ("user_id", json!(user_id)),
                        ("error_kind", json!("session_state_load_failed")),
                        ("detail", json!(err.to_string())),
                    ],
                );
            }
        }

        // 补更新 trace 中的 persistent_state_updated（按路径 patch，不再扫描目录）
        if state_updated {
            if let Some(path) = trace_json_path {
                if let Err(err) = self
                    .agent_core
                    .patch_trace_persistent_state_updated(&path, true)
                {
                    log_chat_warn(
                        "trace_patch_state_updated_failed",
                        vec![
                            ("user_id", json!(user_id)),
                            ("run_id", json!(&run_id)),
                            ("error_kind", json!("trace_patch_failed")),
                            ("detail", json!(err.to_string())),
                        ],
                    );
                }
            }
        }

        log_chat_info(
            "agent_reply_finished",
            vec![
                ("user_id", json!(user_id)),
                ("trigger", json!(reason.as_str())),
                ("message_ids", json!(message_ids)),
                ("message_count", json!(message_ids.len())),
                ("reply_chars", json!(reply.chars().count())),
                ("reply_preview", json!(summarize_text_for_log(&reply, 160))),
            ],
        );
        self.send_reply_text(user_id, &reply);
    }

    pub(super) fn send_reply_text(&mut self, user_id: &str, reply: &str) {
        let token = self
            .context_token_map
            .get(user_id)
            .cloned()
            .or_else(|| self.task_store.get_context_token(user_id).ok().flatten());
        let Some(token) = token else {
            log_chat_warn(
                "reply_skipped_no_context_token",
                vec![
                    ("user_id", json!(user_id)),
                    ("reply_chars", json!(reply.chars().count())),
                    ("reply_preview", json!(summarize_text_for_log(reply, 120))),
                ],
            );
            return;
        };

        let chunks = split_reply_into_chunks(reply, super::WECHAT_REPLY_CHUNK_MAX_CHARS);
        let chunk_total = chunks.len();
        let mut sent_chunk_count = 0usize;
        let mut all_sent = true;
        let mut failed_idx = None;
        for (idx, chunk) in chunks.iter().enumerate() {
            match self.client.send_text_message(user_id, chunk, &token) {
                Ok(()) => {
                    sent_chunk_count += 1;
                    log_chat_info(
                        "reply_chunk_sent",
                        vec![
                            ("user_id", json!(user_id)),
                            ("chunk_index", json!(idx + 1)),
                            ("chunk_total", json!(chunk_total)),
                            ("chunk_chars", json!(chunk.chars().count())),
                        ],
                    )
                }
                Err(err) => {
                    all_sent = false;
                    failed_idx = Some(idx);
                    log_chat_error(
                        "reply_send_failed",
                        vec![
                            ("user_id", json!(user_id)),
                            ("chunk_index", json!(idx + 1)),
                            ("chunk_total", json!(chunk_total)),
                            ("error_kind", json!("wechat_send_failed")),
                            ("detail", json!(err.to_string())),
                        ],
                    );
                    // 一旦某段发送失败，停止后续段，避免乱序
                    break;
                }
            }
        }

        if all_sent {
            log_chat_info(
                "reply_sent",
                vec![
                    ("user_id", json!(user_id)),
                    ("reply_chars", json!(reply.chars().count())),
                    ("chunk_total", json!(chunk_total)),
                    ("sent_chunk_count", json!(sent_chunk_count)),
                    ("all_sent", json!(true)),
                    ("reply_preview", json!(summarize_text_for_log(reply, 120))),
                ],
            );
        } else if let Some(failed_idx) = failed_idx {
            // 把剩余未发送段持久化，供后续补发
            let remaining = remaining_chunks_after_failure(&chunks, failed_idx);
            if let Err(err) = self
                .task_store
                .insert_pending_chunks(user_id, &token, &remaining)
            {
                log_chat_error(
                    "pending_chunks_insert_failed",
                    vec![
                        ("user_id", json!(user_id)),
                        ("error_kind", json!("chunk_persist_failed")),
                        ("detail", json!(err.to_string())),
                    ],
                );
            }
            log_chat_warn(
                "reply_partially_sent",
                vec![
                    ("user_id", json!(user_id)),
                    ("reply_chars", json!(reply.chars().count())),
                    ("chunk_total", json!(chunk_total)),
                    ("sent_chunk_count", json!(sent_chunk_count)),
                    ("all_sent", json!(false)),
                    ("pending_chunks", json!(remaining.len())),
                    ("reply_preview", json!(summarize_text_for_log(reply, 120))),
                ],
            );
        }
    }

    pub(super) fn resend_pending_chunks(&mut self) {
        let chunks = match self.task_store.list_pending_chunks(20) {
            Ok(c) => c,
            Err(err) => {
                log_chat_error(
                    "resend_pending_chunks_query_failed",
                    vec![
                        ("error_kind", json!("chunk_query_failed")),
                        ("detail", json!(err.to_string())),
                    ],
                );
                return;
            }
        };

        for chunk in chunks {
            match self.client.send_text_message(
                &chunk.user_id,
                &chunk.chunk_text,
                &chunk.context_token,
            ) {
                Ok(()) => {
                    if let Err(err) = self.task_store.delete_pending_chunk(chunk.id) {
                        log_chat_error(
                            "pending_chunk_delete_failed",
                            vec![
                                ("chunk_id", json!(chunk.id)),
                                ("error_kind", json!("chunk_delete_failed")),
                                ("detail", json!(err.to_string())),
                            ],
                        );
                    } else {
                        log_chat_info(
                            "pending_chunk_resent",
                            vec![
                                ("chunk_id", json!(chunk.id)),
                                ("user_id", json!(chunk.user_id)),
                                ("chunk_index", json!(chunk.chunk_index)),
                                ("chunk_total", json!(chunk.chunk_total)),
                            ],
                        );
                    }
                }
                Err(err) => {
                    log_chat_warn(
                        "pending_chunk_resend_failed",
                        vec![
                            ("chunk_id", json!(chunk.id)),
                            ("user_id", json!(chunk.user_id)),
                            ("chunk_index", json!(chunk.chunk_index)),
                            ("chunk_total", json!(chunk.chunk_total)),
                            ("error_kind", json!("wechat_send_failed")),
                            ("detail", json!(err.to_string())),
                        ],
                    );
                }
            }
        }
    }

    pub(super) fn process_scheduled_daily_report_push(&mut self) {
        let Some(schedule) = &self.daily_report_schedule else {
            return;
        };
        let Some(day) =
            schedule.should_run_now(Utc::now(), self.last_daily_report_push_day.as_deref())
        else {
            return;
        };
        // 未配置推送目标时静默跳过（调度仍可正常生成快照）
        let Some(target_user_id) = schedule.report_to_user_id().map(str::to_string) else {
            return;
        };
        // 读优先：scheduler 线程生成的快照存在时直接复用，缺失才现生成
        let reply = self.build_daily_report_query_reply(Some(&day));
        if self.push_scheduled_report(ReportPushKind::Daily, &day, &target_user_id, &reply) {
            self.last_daily_report_push_day = Some(day);
        }
    }

    pub(super) fn process_scheduled_weekly_report_push(&mut self) {
        let Some(schedule) = &self.weekly_report_schedule else {
            return;
        };
        let Some(week) =
            schedule.should_run_now(Utc::now(), self.last_weekly_report_push_week.as_deref())
        else {
            return;
        };
        // 未配置推送目标时静默跳过（调度仍可正常生成快照）
        let Some(target_user_id) = schedule.report_to_user_id().map(str::to_string) else {
            return;
        };
        // 读优先：scheduler 线程生成的快照存在时直接复用，缺失才现生成
        let reply = self.build_weekly_report_query_reply(Some(&week));
        if self.push_scheduled_report(ReportPushKind::Weekly, &week, &target_user_id, &reply) {
            self.last_weekly_report_push_week = Some(week);
        }
    }

    /// 定时报告推送共用实现：取 context_token → 分段发送。
    ///
    /// 返回 true 表示本周期已处理完毕（调用方推进 last_push 状态）：
    /// - 全部发送成功；
    /// - 中途失败但未发段落已成功落入 pending_chunks（由 resend_pending_chunks 补发，
    ///   已发段不重复、未发段不丢失）。
    ///
    /// 返回 false 表示本周期未处理完，下个 poll 周期整体重试：
    /// - 无可用 context_token（尚未发送任何段落）；
    /// - 中途失败且 pending_chunks 落库失败（宁可整份重发产生重复段，也不丢段）。
    fn push_scheduled_report(
        &mut self,
        kind: ReportPushKind,
        period_key: &str,
        target_user_id: &str,
        reply: &str,
    ) -> bool {
        let kind_str = kind.as_str();
        let period_field = kind.period_field();
        let token = self
            .context_token_map
            .get(target_user_id)
            .cloned()
            .or_else(|| {
                self.task_store
                    .get_context_token(target_user_id)
                    .ok()
                    .flatten()
            });
        let Some(token) = token else {
            log_chat_warn(
                &format!("scheduler_{kind_str}_report_skipped"),
                vec![
                    (period_field, json!(period_key)),
                    ("user_id", json!(target_user_id)),
                    ("error_kind", json!("missing_context_token")),
                ],
            );
            return false;
        };

        // 报告正文可能较长，复用分段发送
        let chunks = split_reply_into_chunks(reply, super::WECHAT_REPLY_CHUNK_MAX_CHARS);
        let chunk_total = chunks.len();
        for (idx, chunk) in chunks.iter().enumerate() {
            match self.client.send_text_message(target_user_id, chunk, &token) {
                Ok(()) => log_chat_info(
                    &format!("scheduler_{kind_str}_report_chunk_sent"),
                    vec![
                        (period_field, json!(period_key)),
                        ("user_id", json!(target_user_id)),
                        ("chunk_index", json!(idx + 1)),
                        ("chunk_total", json!(chunk_total)),
                        ("chunk_chars", json!(chunk.chars().count())),
                    ],
                ),
                Err(err) => {
                    log_chat_error(
                        &format!("scheduler_{kind_str}_report_send_failed"),
                        vec![
                            (period_field, json!(period_key)),
                            ("user_id", json!(target_user_id)),
                            ("chunk_index", json!(idx + 1)),
                            ("chunk_total", json!(chunk_total)),
                            (
                                "error_kind",
                                json!(format!("scheduler_{kind_str}_report_send_failed")),
                            ),
                            ("detail", json!(err.to_string())),
                        ],
                    );
                    // 中途失败：未发段落落 pending_chunks 供补发，与用户回复路径同一机制
                    return self.persist_remaining_report_chunks(
                        kind,
                        period_key,
                        target_user_id,
                        &token,
                        &chunks,
                        idx,
                    );
                }
            }
        }
        log_chat_info(
            &format!("scheduler_{kind_str}_report_sent"),
            vec![
                (period_field, json!(period_key)),
                ("user_id", json!(target_user_id)),
                ("reply_chars", json!(reply.chars().count())),
                ("chunk_total", json!(chunk_total)),
            ],
        );
        true
    }

    /// 报告推送中途失败时，把未发段落写入补发队列。
    /// 落库成功返回 true（本周期视为已处理，剩余段由补发循环送达）；
    /// 落库失败返回 false（调用方不推进 last_push，下个周期整份重试）。
    pub(super) fn persist_remaining_report_chunks(
        &mut self,
        kind: ReportPushKind,
        period_key: &str,
        target_user_id: &str,
        token: &str,
        chunks: &[String],
        failed_idx: usize,
    ) -> bool {
        let remaining = remaining_chunks_after_failure(chunks, failed_idx);
        if let Err(err) = self
            .task_store
            .insert_pending_chunks(target_user_id, token, &remaining)
        {
            log_chat_error(
                "pending_chunks_insert_failed",
                vec![
                    ("user_id", json!(target_user_id)),
                    ("error_kind", json!("chunk_persist_failed")),
                    ("detail", json!(err.to_string())),
                ],
            );
            return false;
        }
        log_chat_warn(
            &format!("scheduler_{}_report_partially_sent", kind.as_str()),
            vec![
                (kind.period_field(), json!(period_key)),
                ("user_id", json!(target_user_id)),
                ("chunk_total", json!(chunks.len())),
                ("sent_chunk_count", json!(failed_idx)),
                ("pending_chunks", json!(remaining.len())),
            ],
        );
        true
    }

    pub(super) fn process_pending_tasks(&mut self) {
        let claimable = match self.task_store.list_claimable_tasks(5) {
            Ok(tasks) => tasks,
            Err(err) => {
                log_chat_error(
                    "claimable_tasks_query_failed",
                    vec![
                        ("error_kind", json!("claimable_tasks_query_failed")),
                        ("detail", json!(err.to_string())),
                    ],
                );
                return;
            }
        };

        for task in claimable {
            let task_id = task.task_id.clone();
            let enqueued = self.task_executor.enqueue(task_id.clone());
            if !enqueued {
                log_chat_info(
                    "claimable_task_enqueue_skipped",
                    vec![
                        ("task_id", json!(task_id)),
                        ("reason", json!("already_inflight")),
                    ],
                );
            }
        }
    }
}

/// 按保守策略将 runtime session state 合并到持久化记录中。
/// 返回 true 表示发生了结构化字段写入（不仅是时间戳刷新）。
pub(crate) fn merge_runtime_session_state_into_persistent(
    persistent: &mut UserSessionStateRecord,
    runtime: &RuntimeSessionStateSnapshot,
) -> bool {
    // low-signal 保护：空或低信号时不写脏状态
    if runtime.is_empty() || runtime.is_low_signal() {
        return false;
    }

    let mut dirty = false;

    // goal: 已有持久化 goal 时默认保留；为空时才用 runtime 值
    let before_goal = persistent.goal.clone();
    if persistent.goal.is_none() && runtime.goal.is_some() {
        persistent.goal = runtime.goal.clone();
    }
    if persistent.goal != before_goal {
        dirty = true;
    }

    // current_subtask: runtime 有值则覆盖写入，空值不覆盖旧值
    if let Some(ref subtask) = runtime.current_subtask {
        if persistent.current_subtask.as_deref() != Some(subtask.as_str()) {
            dirty = true;
        }
        persistent.current_subtask = Some(subtask.clone());
    }

    // next_step: runtime 有值则覆盖写入，空值不覆盖旧值
    if let Some(ref next) = runtime.next_step {
        if persistent.next_step.as_deref() != Some(next.as_str()) {
            dirty = true;
        }
        persistent.next_step = Some(next.clone());
    }

    // constraints: 去重合并，最多 4 条，runtime 至少保底 1 条
    let persistent_items = persistent.constraints();
    let merged = merge_string_arrays_with_runtime_reserve(
        persistent_items.clone(),
        runtime.constraints.clone(),
        4,
        1,
    );
    if merged != persistent_items {
        persistent.set_constraints(merged);
        dirty = true;
    }

    // confirmed_facts: 去重合并，最多 5 条，runtime 至少保底 1 条
    let persistent_items = persistent.confirmed_facts();
    let merged = merge_string_arrays_with_runtime_reserve(
        persistent_items.clone(),
        runtime.confirmed_facts.clone(),
        5,
        1,
    );
    if merged != persistent_items {
        persistent.set_confirmed_facts(merged);
        dirty = true;
    }

    // done_items: 去重合并，最多 3 条，runtime 不保底
    let persistent_items = persistent.done_items();
    let merged = merge_string_arrays_with_runtime_reserve(
        persistent_items.clone(),
        runtime.done_items.clone(),
        3,
        0,
    );
    if merged != persistent_items {
        persistent.set_done_items(merged);
        dirty = true;
    }

    // open_questions: 去重合并，最多 3 条，runtime 至少保底 1 条
    let persistent_items = persistent.open_questions();
    let merged = merge_string_arrays_with_runtime_reserve(
        persistent_items.clone(),
        runtime.open_questions.clone(),
        3,
        1,
    );
    if merged != persistent_items {
        persistent.set_open_questions(merged);
        dirty = true;
    }

    dirty
}
