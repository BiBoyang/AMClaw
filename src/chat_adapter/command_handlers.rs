use super::{
    log_chat_error, log_chat_info, log_chat_warn, sanitize_report_markdown_for_wechat,
    summarize_text_for_log,
};
use crate::agent_core::{
    is_agent_command, is_llm_auth_error, AgentRunContext, RuntimeSessionStateSnapshot,
};
use crate::command_router;
use crate::session_router::FlushReason;
use crate::task_store::{MarkTaskArchivedInput, TaskStore};
use serde_json::json;

pub(super) fn build_link_submission_reply(
    records: &[crate::task_store::LinkTaskRecord],
    failures: &[String],
) -> String {
    let mut lines = Vec::new();
    if !records.is_empty() {
        if records.len() == 1 && failures.is_empty() {
            let record = &records[0];
            let status = if record.created_new {
                "已收录链接"
            } else {
                "链接已存在"
            };
            lines.push(status.to_string());
            lines.push(format!("url: {}", record.normalized_url));
            lines.push(format!("task_id: {}", record.task_id));
        } else {
            lines.push("链接处理结果:".to_string());
            for record in records {
                let status = if record.created_new {
                    "新建"
                } else {
                    "已存在"
                };
                lines.push(format!(
                    "- {status} {} task_id={}",
                    record.normalized_url, record.task_id
                ));
            }
        }
    }
    for failure in failures {
        lines.push(format!("- 失败 {failure}"));
    }
    if lines.is_empty() {
        return "没有可入库的链接".to_string();
    }
    lines.join("\n")
}

pub(super) fn build_task_status_reply(status: &crate::task_store::TaskStatusRecord) -> String {
    let mut lines = vec![
        "任务状态".to_string(),
        format!("task_id: {}", status.task_id),
        format!("url: {}", status.normalized_url),
        format!(
            "source: {}",
            status.content_source.as_deref().unwrap_or("unknown")
        ),
        format!(
            "page_kind: {}",
            status.page_kind.as_deref().unwrap_or("unknown")
        ),
        format!("status: {}", status.status),
        format!("retry_count: {}", status.retry_count),
        format!("created_at: {}", status.created_at),
        format!("updated_at: {}", status.updated_at),
    ];
    if let Some(title) = &status.title {
        if !title.trim().is_empty() {
            lines.push(format!("title: {title}"));
        }
    }
    if let Some(output_path) = &status.output_path {
        if !output_path.trim().is_empty() {
            lines.push(format!("output_path: {output_path}"));
        }
    }
    if let Some(snapshot_path) = &status.snapshot_path {
        if !snapshot_path.trim().is_empty() {
            lines.push(format!("snapshot_path: {snapshot_path}"));
        }
    }
    if let Some(last_error) = &status.last_error {
        if !last_error.trim().is_empty() {
            lines.push(format!("last_error: {last_error}"));
        }
    }
    if status.status == "awaiting_manual_input" {
        lines.push(format!(
            "action_required: 请使用 补正文 {} :: <content>",
            status.task_id
        ));
    }
    lines.join("\n")
}

pub(super) fn build_recent_tasks_reply(tasks: &[crate::task_store::RecentTaskRecord]) -> String {
    if tasks.is_empty() {
        return "最近没有任务".to_string();
    }

    let mut lines = vec!["最近任务:".to_string()];
    for task in tasks {
        lines.push(format!(
            "- {} {} source={} page_kind={} task_id={}",
            task.status,
            task.normalized_url,
            task.content_source.as_deref().unwrap_or("unknown"),
            task.page_kind.as_deref().unwrap_or("unknown"),
            task.task_id
        ));
    }
    lines.join("\n")
}

pub(super) fn build_manual_tasks_reply(tasks: &[crate::task_store::RecentTaskRecord]) -> String {
    if tasks.is_empty() {
        return "当前没有待补录任务".to_string();
    }

    let mut lines = vec!["待补录任务:".to_string()];
    for task in tasks {
        lines.push(format!(
            "- {} {} source={} page_kind={} task_id={}",
            task.status,
            task.normalized_url,
            task.content_source.as_deref().unwrap_or("unknown"),
            task.page_kind.as_deref().unwrap_or("unknown"),
            task.task_id
        ));
    }
    lines.join("\n")
}

pub(super) fn build_user_memories_reply(
    memories: &[crate::task_store::UserMemoryRecord],
) -> String {
    if memories.is_empty() {
        return "当前还没有保存的记忆".to_string();
    }

    let mut lines = vec!["我的记忆:".to_string()];
    for memory in memories {
        lines.push(format!("- id: {} | {}", memory.id, memory.content));
    }
    lines.join("\n")
}

pub(super) fn build_manual_archive_rejected_reply(task_store: &TaskStore, task_id: &str) -> String {
    match task_store.get_task_status(task_id) {
        Ok(Some(status)) => format!(
            "任务当前状态为 {}，不允许人工归档: {task_id}",
            status.status
        ),
        Ok(None) => format!("未找到对应任务: {task_id}"),
        Err(err) => format!("查询任务状态失败: {err}"),
    }
}

impl super::WeChatBot {
    pub(super) fn handle_task_status_query(&mut self, user_id: &str, task_id: &str) {
        let reply = match self.task_store.get_task_status(task_id) {
            Ok(Some(status)) => build_task_status_reply(&status),
            Ok(None) => format!("未找到对应任务: {task_id}"),
            Err(err) => format!("查询任务状态失败: {err}"),
        };
        self.send_reply_text(user_id, &reply);
    }

    pub(super) fn handle_recent_tasks_query(&mut self, user_id: &str) {
        let reply = match self.task_store.list_recent_tasks(5) {
            Ok(tasks) => build_recent_tasks_reply(&tasks),
            Err(err) => format!("查询最近任务失败: {err}"),
        };
        self.send_reply_text(user_id, &reply);
    }

    pub(super) fn handle_manual_tasks_query(&mut self, user_id: &str) {
        let reply = match self.task_store.list_manual_tasks(5) {
            Ok(tasks) => build_manual_tasks_reply(&tasks),
            Err(err) => format!("查询待补录任务失败: {err}"),
        };
        self.send_reply_text(user_id, &reply);
    }

    pub(super) fn handle_user_memories_query(&mut self, user_id: &str) {
        let reply = match self.task_store.list_user_memories(user_id, 10) {
            Ok(memories) => build_user_memories_reply(&memories),
            Err(err) => format!("查询记忆失败: {err}"),
        };
        self.send_reply_text(user_id, &reply);
    }

    pub(super) fn handle_link_submission(&mut self, user_id: &str, urls: Vec<String>) {
        let mut records = Vec::new();
        let mut failures = Vec::new();

        for url in urls {
            match self.task_store.record_link_submission(&url) {
                Ok(record) => records.push(record),
                Err(err) => failures.push(format!("{url} => {err}")),
            }
        }

        let reply = build_link_submission_reply(&records, &failures);
        self.send_reply_text(user_id, &reply);
    }

    pub(super) fn handle_task_retry(&mut self, user_id: &str, task_id: &str) {
        let reply = match self.task_store.retry_task(task_id) {
            Ok(Some(_status)) => {
                self.task_executor.enqueue(task_id.to_string());
                format!("任务已重置并加入执行队列: {task_id}")
            }
            Ok(None) => format!("未找到对应任务: {task_id}"),
            Err(err) => format!("重试任务失败: {err}"),
        };
        self.send_reply_text(user_id, &reply);
    }

    pub(super) fn handle_manual_content_submission(
        &mut self,
        user_id: &str,
        task_id: &str,
        content: &str,
    ) {
        let reply = match self.task_store.get_task_content(task_id) {
            Ok(Some(task)) => match self.pipeline.archive_manual_content(&task, content) {
                Ok(result) => {
                    let output_path = result.output_path.to_string_lossy().to_string();
                    match self.task_store.mark_task_archived(
                        task_id,
                        MarkTaskArchivedInput {
                            output_path: &output_path,
                            title: result.title.as_deref(),
                            page_kind: Some("manual_input"),
                            snapshot_path: None,
                            content_source: Some("manual_input"),
                            summary: None,
                        },
                    ) {
                        Ok(true) => format!(
                            "已写入人工补正文\ntask_id: {task_id}\noutput_path: {output_path}"
                        ),
                        Ok(false) => build_manual_archive_rejected_reply(&self.task_store, task_id),
                        Err(err) => format!("更新任务状态失败: {err}"),
                    }
                }
                Err(err) => format!("人工补录归档失败: {err}"),
            },
            Ok(None) => format!("未找到对应任务: {task_id}"),
            Err(err) => format!("查询任务上下文失败: {err}"),
        };
        self.send_reply_text(user_id, &reply);
    }

    pub(super) fn handle_user_memory_write(&mut self, user_id: &str, content: &str) {
        let mut write_state = crate::task_store::MemoryWriteState::default();
        let decision = self.task_store.govern_memory_write(
            user_id,
            content,
            crate::task_store::MemoryType::Explicit,
            100,
            &mut write_state,
        );
        let reply = match &decision {
            crate::task_store::WriteDecision::Written(record) => {
                log_chat_info(
                    "user_memory_explicit_written",
                    vec![
                        ("user_id", json!(user_id)),
                        ("memory_id", json!(record.id)),
                        (
                            "content_preview",
                            json!(summarize_text_for_log(content, 120)),
                        ),
                    ],
                );
                format!("已记住\n- {}", content.trim())
            }
            crate::task_store::WriteDecision::Skipped { reason, .. } => {
                log_chat_info(
                    "user_memory_explicit_skipped",
                    vec![
                        ("user_id", json!(user_id)),
                        ("skip_reason", json!(reason.to_string())),
                    ],
                );
                format!("未能记住: {}", reason)
            }
            crate::task_store::WriteDecision::Promoted { id, reason } => {
                log_chat_info(
                    "user_memory_explicit_promoted",
                    vec![
                        ("user_id", json!(user_id)),
                        ("memory_id", json!(id)),
                        ("promote_reason", json!(reason.to_string())),
                    ],
                );
                let id_preview: String = id.chars().take(8).collect();
                format!("已提升已有记忆为显式记忆 (id: {})", id_preview)
            }
        };
        self.send_reply_text(user_id, &reply);
    }

    pub(super) fn handle_user_memory_suppress(&mut self, user_id: &str, memory_id: &str) {
        let reply = match self.task_store.suppress_memory(user_id, memory_id) {
            Ok(()) => format!("已屏蔽记忆: {memory_id}"),
            Err(err) => format!("屏蔽记忆失败: {err}"),
        };
        self.send_reply_text(user_id, &reply);
    }

    pub(super) fn handle_user_memory_useful(&mut self, user_id: &str, memory_id: &str) {
        let reply = match self.task_store.confirm_memory_useful(user_id, memory_id) {
            Ok(()) => format!("已标记记忆有用: {memory_id}"),
            Err(err) => format!("标记记忆有用失败: {err}"),
        };
        self.send_reply_text(user_id, &reply);
    }

    pub(super) fn handle_daily_report_query(&mut self, user_id: &str, day: Option<&str>) {
        let reply = self.build_daily_report_query_reply(day);
        self.send_reply_text(user_id, &reply);
    }

    pub(super) fn handle_weekly_report_query(&mut self, user_id: &str, week: Option<&str>) {
        let reply = self.build_weekly_report_query_reply(week);
        self.send_reply_text(user_id, &reply);
    }

    pub(super) fn build_daily_report_query_reply(&self, day: Option<&str>) -> String {
        let day = normalize_report_query_target(day, self.reporter.current_day());
        // 读优先：scheduler 线程是报告的唯一主动生成点，查询/推送优先复用
        // 已生成快照，避免每次查询都重写 daily-{day}.md；快照缺失时才现生成兜底。
        if let Some(reply) = read_existing_report_reply(&self.reporter.daily_report_path(&day)) {
            return reply;
        }
        match self.reporter.generate_for_day(&day) {
            Ok(report) => render_report_reply_for_wechat(
                &report.markdown_path,
                &report.summary,
                report.item_count,
            ),
            Err(err) => format!("生成日报失败: {err}"),
        }
    }

    pub(super) fn build_weekly_report_query_reply(&self, week: Option<&str>) -> String {
        let week = normalize_report_query_target(week, self.reporter.current_week());
        // 读优先：同日报，优先复用 weekly-{week}.md 快照，缺失时才现生成。
        if let Some(reply) = read_existing_report_reply(&self.reporter.weekly_report_path(&week)) {
            return reply;
        }
        match self.reporter.generate_weekly_for_week(&week) {
            Ok(report) => render_report_reply_for_wechat(
                &report.markdown_path,
                &report.summary,
                report.item_count,
            ),
            Err(err) => format!("生成周报失败: {err}"),
        }
    }

    pub(super) fn maybe_persist_auto_memory(
        &mut self,
        user_id: &str,
        intent: &command_router::RouteIntent,
    ) {
        let text = match intent {
            command_router::RouteIntent::ChatContinue { text }
            | command_router::RouteIntent::ChatCommit { text }
            | command_router::RouteIntent::ChatPending { text } => text.as_str(),
            _ => return,
        };
        let Some((memory, memory_type)) = extract_auto_memory_candidate(text) else {
            return;
        };
        let mut write_state = crate::task_store::MemoryWriteState::default();
        let decision = self.task_store.govern_memory_write(
            user_id,
            &memory,
            memory_type,
            memory_type.default_priority(),
            &mut write_state,
        );
        match &decision {
            crate::task_store::WriteDecision::Written(_) => {
                log_chat_info(
                    "user_memory_auto_recorded",
                    vec![
                        ("user_id", json!(user_id)),
                        ("memory_type", json!(memory_type.as_str())),
                        (
                            "memory_preview",
                            json!(summarize_text_for_log(&memory, 120)),
                        ),
                    ],
                );
            }
            crate::task_store::WriteDecision::Skipped { reason, .. } => {
                log_chat_info(
                    "user_memory_auto_skipped",
                    vec![
                        ("user_id", json!(user_id)),
                        ("memory_type", json!(memory_type.as_str())),
                        ("skip_reason", json!(reason.to_string())),
                    ],
                );
            }
            crate::task_store::WriteDecision::Promoted { id, reason } => {
                // 归位后的类型（user_preference/project_fact）可 promote 历史 auto 记忆
                log_chat_info(
                    "user_memory_auto_promoted",
                    vec![
                        ("user_id", json!(user_id)),
                        ("memory_id", json!(id)),
                        ("memory_type", json!(memory_type.as_str())),
                        ("promote_reason", json!(reason.to_string())),
                    ],
                );
            }
        }
    }

    pub(super) fn handle_context_debug_query(
        &mut self,
        user_id: &str,
        extra_text: Option<&str>,
        verbose: bool,
    ) {
        let pending = self.session_router.snapshot(user_id);
        let mut parts = Vec::new();
        let mut message_ids = Vec::new();
        if let Some(snapshot) = &pending {
            if !snapshot.merged_text.trim().is_empty() {
                parts.push(snapshot.merged_text.trim().to_string());
            }
            message_ids = snapshot.message_ids.clone();
        }
        if let Some(extra_text) = extra_text.filter(|value| !value.trim().is_empty()) {
            parts.push(extra_text.trim().to_string());
        }

        if parts.is_empty() {
            self.send_reply_text(
                user_id,
                "当前没有待提交会话。可直接发送 `/context 你的问题` 预览一次上下文装配。",
            );
            return;
        }

        let merged_text = parts.join("\n");
        let mode = if verbose {
            crate::agent_core::ContextPreviewMode::Verbose
        } else {
            crate::agent_core::ContextPreviewMode::Summary
        };

        // 加载持久化 session state 供预览（只读，不破坏已持久化 state）
        let session_state = self
            .task_store
            .load_user_session_state(user_id)
            .ok()
            .flatten();
        let context = AgentRunContext::wechat_chat(user_id, "context_debug", message_ids)
            .with_session_text(&merged_text)
            .with_context_token_present(self.context_token_map.contains_key(user_id))
            .with_user_session_state(session_state);

        let reply = match if matches!(mode, crate::agent_core::ContextPreviewMode::Summary) {
            self.agent_core
                .preview_context_with_context(&merged_text, context)
        } else {
            self.agent_core
                .preview_context_with_context_mode(&merged_text, context, mode)
        } {
            Ok(reply) => reply,
            Err(err) => format!("生成 context preview 失败: {err}"),
        };
        self.send_reply_text(user_id, &reply);
    }

    pub(super) fn generate_reply(
        &self,
        user_id: &str,
        user_text: &str,
        message_ids: &[String],
        reason: FlushReason,
        trace_context: AgentRunContext,
    ) -> (
        String,
        Option<String>,
        Option<std::path::PathBuf>,
        Option<RuntimeSessionStateSnapshot>,
    ) {
        match self.agent_core.run_with_context(user_text, trace_context) {
            Ok(result) => (
                result.output,
                Some(result.run_id),
                result.trace_json_path,
                result.runtime_session_state,
            ),
            Err(err) => {
                let err_text = err.to_string();
                if is_agent_command(user_text) {
                    log_chat_error(
                        "agent_reply_failed",
                        vec![
                            ("user_id", json!(user_id)),
                            ("trigger", json!(reason.as_str())),
                            ("message_ids", json!(message_ids)),
                            ("message_count", json!(message_ids.len())),
                            ("error_kind", json!("agent_command_failed")),
                            ("detail", json!(err_text.clone())),
                        ],
                    );
                    return (format!("执行失败: {err_text}"), None, None, None);
                }
                if is_llm_auth_error(&err_text) {
                    log_chat_warn(
                        "agent_reply_fallback",
                        vec![
                            ("user_id", json!(user_id)),
                            ("trigger", json!(reason.as_str())),
                            ("message_ids", json!(message_ids)),
                            ("message_count", json!(message_ids.len())),
                            ("error_kind", json!("llm_auth_failed")),
                            ("detail", json!(err_text.clone())),
                        ],
                    );
                    return (
                        "LLM 鉴权失败，请检查 MOONSHOT_* / DEEPSEEK_* / OPENAI_* 配置".to_string(),
                        None,
                        None,
                        None,
                    );
                }
                log_chat_warn(
                    "agent_reply_fallback",
                    vec![
                        ("user_id", json!(user_id)),
                        ("trigger", json!(reason.as_str())),
                        ("message_ids", json!(message_ids)),
                        ("message_count", json!(message_ids.len())),
                        ("error_kind", json!("agent_run_failed")),
                        ("detail", json!(err_text.clone())),
                    ],
                );
                // pre-agent 时代的 hello/时间/帮助/echo 硬编码 fallback 已删除：
                // agent 失败直接向用户报错误信息，不再用脱节文案兜底。
                (format!("执行失败: {err_text}"), None, None, None)
            }
        }
    }
}

/// 归一化报告查询目标：空/空白入参回退到当前周期。
fn normalize_report_query_target(input: Option<&str>, fallback: String) -> String {
    input
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or(fallback)
}

/// 读优先加载已生成的报告快照并渲染为微信回复；文件不存在或读取失败返回 None。
fn read_existing_report_reply(markdown_path: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(markdown_path).ok()?;
    let item_count = parse_archived_count(&content);
    Some(render_report_content_for_wechat(&content, item_count))
}

/// 从报告内容头解析 archived_count（reporter 写入的固定格式 "- archived_count: N"）。
fn parse_archived_count(content: &str) -> Option<usize> {
    content.lines().find_map(|line| {
        line.trim()
            .strip_prefix("- archived_count: ")?
            .trim()
            .parse()
            .ok()
    })
}

/// 渲染报告回复：优先读 markdown 全文并适配微信（超长截断），读不到时回退 summary。
fn render_report_reply_for_wechat(
    markdown_path: &std::path::Path,
    summary: &str,
    item_count: usize,
) -> String {
    match std::fs::read_to_string(markdown_path) {
        Ok(content) => render_report_content_for_wechat(&content, Some(item_count)),
        Err(_) => summary.to_string(),
    }
}

/// 报告正文适配微信：去掉服务器路径字段，超长截断（截断提示附 archived_count）。
fn render_report_content_for_wechat(content: &str, item_count: Option<usize>) -> String {
    let content = sanitize_report_markdown_for_wechat(content);
    // 微信单条消息限制约 4096 字符，截断到安全长度
    if content.chars().count() > 3800 {
        let truncated: String = content.chars().take(3800).collect();
        match item_count {
            Some(item_count) => format!("{truncated}\n\n...(已截断，共 {item_count} 条)"),
            None => format!("{truncated}\n\n...(已截断)"),
        }
    } else {
        content
    }
}

/// 从聊天文本提炼记忆候选，返回 (内容, 归位后的记忆类型)。
///
/// 类型归位（Phase 4）：
/// - 偏好类触发词 → `MemoryType::UserPreference`
/// - 主题/在做类触发词 → `MemoryType::ProjectFact`
///
/// 内容保留 `偏好: ` / `主题: ` 文本前缀以保证与历史 auto 记忆的 dedup/promote 连续性；
/// prompt 渲染侧会去掉与类型标签重复的文本前缀，避免双重标注。
fn extract_auto_memory_candidate(input: &str) -> Option<(String, crate::task_store::MemoryType)> {
    let text = input.trim();
    if text.is_empty() {
        return None;
    }

    for prefix in ["我更喜欢", "我喜欢", "我偏好", "I prefer ", "I like "] {
        if let Some(rest) = text.strip_prefix(prefix) {
            let value = rest.trim();
            if !value.is_empty() {
                return Some((
                    format!("偏好: {value}"),
                    crate::task_store::MemoryType::UserPreference,
                ));
            }
        }
    }

    for prefix in [
        "我关注",
        "我在研究",
        "我最近在看",
        "我想了解",
        "我在做",
        "I am researching ",
        "I'm researching ",
        "I want to learn ",
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            let value = rest.trim();
            if !value.is_empty() {
                return Some((
                    format!("主题: {value}"),
                    crate::task_store::MemoryType::ProjectFact,
                ));
            }
        }
    }

    None
}
