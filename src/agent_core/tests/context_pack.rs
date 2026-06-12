use super::super::{
    derive_runtime_session_state, load_business_context_snapshot, select_previous_observations,
    AgentCore, AgentObservation, AgentRunContext, AgentRunTrace, BusinessContextSnapshot,
    ContextAssembler, ContextPreviewMode, ExecutionPlan, GoalSignal, MemoryBudget, ObservationKind,
    PlannedDecision, RuntimeSessionStateSnapshot,
};
use super::{temp_db_path, temp_workspace};
use crate::context_pack::{ContextSectionChangeReason, ContextSectionKind};
use crate::retriever::rule::RuleRetriever;
use crate::session_summary::{
    build_session_text_section_lines, summarize_for_markdown, summarize_session_text_semantic,
    SessionSummaryStrategy, SESSION_TEXT_SUMMARY_MAX_CHARS,
};
use crate::task_store::TaskStore;

#[test]
fn context_assembler_includes_runtime_fields_and_observation() {
    let workspace = temp_workspace();
    let mut trace = AgentRunTrace::new(
        &workspace,
        "读文件 demo.txt",
        AgentRunContext::wechat_chat(
            "user-context",
            "commit",
            vec!["msg-1".to_string(), "msg-2".to_string()],
        )
        .with_task_id("task-ctx")
        .with_article_id("article-ctx")
        .with_session_text("session merged text")
        .with_context_token_present(true),
    );
    trace.record_decision(
        0,
        "scripted",
        &PlannedDecision::new(super::super::AgentDecision::CallTool(
            super::super::ToolAction::Read {
                path: "demo.txt".to_string(),
            },
        ))
        .with_plan(Some(ExecutionPlan {
            steps: vec!["读取 demo.txt".to_string(), "总结内容".to_string()],
        })),
    );
    let observation = AgentObservation::tool_result(
        1,
        "read_file",
        "hello from tool",
        Some(ObservationKind::Text),
    );
    let business_context = BusinessContextSnapshot {
        current_task: None,
        recent_tasks: Vec::new(),
        user_memories: Vec::new(),
    };
    let runtime_session_state =
        derive_runtime_session_state(&trace, "读文件 demo.txt", Some(&observation), None);
    let planner_input = ContextAssembler::default().assemble(
        &trace,
        "读文件 demo.txt",
        Some(&observation),
        Some(&runtime_session_state),
        &[
            "read: 读取工作区内文件，参数: path".to_string(),
            "get_task_status: 查询单个任务状态，参数: task_id".to_string(),
        ],
        Some(&business_context),
    );

    assert!(planner_input
        .assembled_user_prompt
        .contains("## Runtime Context"));
    assert!(planner_input
        .assembled_user_prompt
        .contains("source_type: wechat_chat"));
    assert!(planner_input
        .assembled_user_prompt
        .contains("task_id: task-ctx"));
    assert!(planner_input
        .assembled_user_prompt
        .contains("article_id: article-ctx"));
    assert!(planner_input
        .assembled_user_prompt
        .contains("session merged text"));
    assert!(planner_input
        .assembled_user_prompt
        .contains("replan_budget: 0/3"));
    assert!(planner_input
        .assembled_user_prompt
        .contains("current_step_index: 1"));
    assert!(planner_input
        .assembled_user_prompt
        .contains("## Session State"));
    assert!(planner_input
        .assembled_user_prompt
        .contains("## Latest Observation"));
    assert!(planner_input
        .assembled_user_prompt
        .contains("hello from tool"));
    assert!(planner_input.context_budget_summary.final_total_chars > 0);
    let latest_section = planner_input
        .context_sections
        .iter()
        .find(|section| section.kind == "latest_observation")
        .expect("应包含 latest_observation section");
    assert!(latest_section.line_count >= 6);
    assert!(latest_section.item_count >= 5);
    assert!(latest_section.char_count > 0);
    assert!(latest_section.content.contains("read_file"));
}

#[test]
fn context_pack_exposes_section_metadata() {
    let workspace = temp_workspace();
    let trace = AgentRunTrace::new(
        &workspace,
        "读文件 demo.txt",
        AgentRunContext::wechat_chat("user-context", "commit", vec![])
            .with_session_text("session merged text"),
    );
    let observation = AgentObservation::tool_result(
        1,
        "read_file",
        "hello from tool",
        Some(ObservationKind::Text),
    );
    let runtime_session_state =
        derive_runtime_session_state(&trace, "读文件 demo.txt", Some(&observation), None);
    let pack = ContextAssembler::default().build_pack(
        &trace,
        "读文件 demo.txt",
        Some(&observation),
        Some(&runtime_session_state),
        &["read: 读取工作区内文件，参数: path".to_string()],
        None,
    );

    let latest = pack
        .section(ContextSectionKind::LatestObservation)
        .expect("应包含 latest observation section");
    assert_eq!(latest.kind().as_str(), "latest_observation");
    assert!(latest.item_count() >= 5);
    assert!(latest.char_count() > 0);
    assert!(latest.lines().iter().any(|line| line.contains("read_file")));

    let rendered = pack.render();
    assert!(rendered.contains("## User Input"));
    assert!(rendered.contains("## Session State"));
    assert!(rendered.contains("## Session Text"));
    assert!(rendered.contains("## Available Tools"));
}

#[test]
fn latest_observation_pre_trims_to_section_budget() {
    let workspace = temp_workspace();
    let trace = AgentRunTrace::new(
        &workspace,
        "读文件 demo.txt",
        AgentRunContext::wechat_chat("user-context", "commit", vec![]),
    );
    // Content far exceeds both old 800-limit and new section max (560)
    let long_content = "observation content ".repeat(100);
    let observation =
        AgentObservation::tool_result(1, "read_file", &long_content, Some(ObservationKind::Text));
    let runtime_session_state =
        derive_runtime_session_state(&trace, "读文件 demo.txt", Some(&observation), None);
    let pack = ContextAssembler::default().build_pack(
        &trace,
        "读文件 demo.txt",
        Some(&observation),
        Some(&runtime_session_state),
        &["read: 读取工作区内文件，参数: path".to_string()],
        None,
    );

    let latest = pack
        .section(ContextSectionKind::LatestObservation)
        .expect("应包含 latest observation section");
    let max_chars = ContextSectionKind::LatestObservation.policy().max_chars;
    assert!(
        latest.char_count() <= max_chars,
        "latest_observation char_count {} exceeds max_chars {}",
        latest.char_count(),
        max_chars
    );
    // Pre-trimming means the section enters ContextSection::new already within budget,
    // so section-level trim should NOT fire (no double-truncation).
    assert!(
        !latest.trimmed(),
        "latest_observation should not be trimmed at section level when pre-trimmed"
    );
}

#[test]
fn session_text_section_keeps_full_text_for_short_input() {
    let workspace = temp_workspace();
    let trace = AgentRunTrace::new(
        &workspace,
        "继续处理",
        AgentRunContext::wechat_chat("user-session-short", "commit", vec![])
            .with_session_text("短会话文本"),
    );
    let runtime_session_state = derive_runtime_session_state(&trace, "继续处理", None, None);
    let pack = ContextAssembler::default().build_pack(
        &trace,
        "继续处理",
        None,
        Some(&runtime_session_state),
        &["read: 读取工作区内文件，参数: path".to_string()],
        None,
    );

    let session_section = pack
        .section(ContextSectionKind::SessionText)
        .expect("应包含 session_text section");
    let rendered = session_section.render();
    assert!(rendered.contains("mode: full"));
    assert!(rendered.contains("短会话文本"));
}

#[test]
fn session_text_section_uses_boundary_compaction_for_long_input() {
    let workspace = temp_workspace();
    let session_text = format!(
        "{}{}{}",
        "head-start ".repeat(25),
        "middle-noise ".repeat(50),
        "tail-marker-xyz"
    );
    let trace = AgentRunTrace::new(
        &workspace,
        "继续处理",
        AgentRunContext::wechat_chat("user-session-long", "commit", vec![])
            .with_session_text(session_text),
    );
    let runtime_session_state = derive_runtime_session_state(&trace, "继续处理", None, None);
    let pack = ContextAssembler::default().build_pack(
        &trace,
        "继续处理",
        None,
        Some(&runtime_session_state),
        &["read: 读取工作区内文件，参数: path".to_string()],
        None,
    );

    let session_section = pack
        .section(ContextSectionKind::SessionText)
        .expect("应包含 session_text section");
    let rendered = session_section.render();
    assert!(rendered.contains("mode: boundary_compaction"));
    assert!(rendered.contains("summary_strategy: semantic"));
    assert!(rendered.contains("summary_compact"));
    assert!(rendered.contains("recent_tail"));
    assert!(rendered.contains("tail-marker-xyz"));
}

#[test]
fn session_text_compaction_supports_truncate_strategy() {
    let session_text = format!(
        "{}\n{}\n下一步: 修复关键问题并输出结论。\n风险: 需要补充参数。",
        "背景噪音内容".repeat(50),
        "历史过程信息".repeat(50),
    );
    let semantic_summary =
        summarize_session_text_semantic(&session_text, SESSION_TEXT_SUMMARY_MAX_CHARS);
    let truncate_summary = summarize_for_markdown(&session_text, SESSION_TEXT_SUMMARY_MAX_CHARS);
    let semantic =
        build_session_text_section_lines(&session_text, SessionSummaryStrategy::Semantic)
            .join("\n");
    let truncate =
        build_session_text_section_lines(&session_text, SessionSummaryStrategy::Truncate)
            .join("\n");

    assert!(semantic.contains("summary_strategy: semantic"));
    assert!(truncate.contains("summary_strategy: truncate"));
    assert!(semantic_summary.contains("下一步: 修复关键问题并输出结论。"));
    assert!(!truncate_summary.contains("下一步: 修复关键问题并输出结论。"));
}

#[test]
fn preview_context_respects_agent_config_summary_strategy() {
    let workspace = temp_workspace();
    let db_path = temp_db_path();
    let config = crate::config::AgentConfig {
        session_summary_strategy: "truncate".to_string(),
        ..Default::default()
    };
    let agent = AgentCore::with_task_store_db_path_and_agent_config(workspace, &db_path, &config)
        .expect("初始化失败");

    let session_text = format!(
        "{}\n{}\n下一步: 修复关键问题并输出结论。\n风险: 需要补充参数。",
        "背景噪音内容".repeat(50),
        "历史过程信息".repeat(50),
    );
    let preview = agent
        .preview_context_with_context_mode(
            "帮我总结当前进展",
            AgentRunContext::wechat_chat("user-summary-config", "context_debug", vec![])
                .with_session_text(session_text),
            ContextPreviewMode::Verbose,
        )
        .expect("应成功生成 preview");

    assert!(preview.contains("summary_strategy: truncate"));
}

#[test]
fn context_pack_records_trim_and_drop_reasons() {
    let workspace = temp_workspace();
    let trace = AgentRunTrace::new(
        &workspace,
        "请帮我处理一个很长的上下文请求",
        AgentRunContext::wechat_chat("user-budget", "commit", vec![])
            .with_session_text("session ".repeat(240)),
    );
    let observation = AgentObservation::tool_result(
        1,
        "read_file",
        &"observation ".repeat(240),
        Some(ObservationKind::Text),
    );
    let runtime_session_state = RuntimeSessionStateSnapshot {
        goal: Some("推进一个需要大量上下文的信息整理任务".to_string()),
        current_subtask: Some("先整理线索，再决定是否继续调工具".to_string()),
        constraints: vec!["避免重复无效动作".to_string(); 4],
        confirmed_facts: vec!["已有较长 session_text 和 latest_observation".to_string(); 4],
        done_items: vec!["已完成初步读取".to_string(); 3],
        next_step: Some("优先收敛上下文而不是继续膨胀 prompt".to_string()),
        open_questions: vec!["哪些 section 可以先被丢弃".to_string(); 3],
        goal_signal: GoalSignal::PersistentHigh,
    };
    let mut pack = ContextAssembler::default().build_pack(
        &trace,
        "请帮我处理一个很长的上下文请求",
        Some(&observation),
        Some(&runtime_session_state),
        &[
            format!("read: {}", "tool ".repeat(120)),
            format!("write: {}", "tool ".repeat(120)),
        ],
        None,
    );
    pack.set_max_total_chars(1500);
    pack.apply_total_budget();

    let snapshot = pack.snapshot();
    assert!(snapshot.iter().any(|section| {
        section.trimmed
            && section.trim_reason == Some(ContextSectionChangeReason::SectionBudgetExceeded)
    }));
    assert!(snapshot.iter().any(|section| {
        !section.included
            && section.drop_reason == Some(ContextSectionChangeReason::TotalBudgetExceeded)
    }));
    assert!(pack.budget_summary().final_total_chars <= 1500);
}

#[test]
fn context_pack_includes_previous_observation_summaries() {
    let workspace = temp_workspace();
    let mut trace = AgentRunTrace::new(
        &workspace,
        "继续处理",
        AgentRunContext::wechat_chat("user-obs", "commit", vec![]),
    );
    let older = AgentObservation::tool_result(
        0,
        "read_file",
        "older observation payload",
        Some(ObservationKind::Text),
    );
    let latest = AgentObservation::tool_result(
        1,
        "write_file",
        "latest observation payload",
        Some(ObservationKind::FileMutation),
    );
    trace.record_observation(&older);
    trace.record_observation(&latest);

    let runtime_session_state =
        derive_runtime_session_state(&trace, "继续处理", Some(&latest), None);
    let pack = ContextAssembler {
        include_previous_observations: true,
    }
    .build_pack(
        &trace,
        "继续处理",
        Some(&latest),
        Some(&runtime_session_state),
        &["read: 读取工作区内文件，参数: path".to_string()],
        None,
    );

    let previous = pack
        .section(ContextSectionKind::PreviousObservations)
        .expect("应包含 previous_observations section");
    let rendered = previous.render();
    assert!(rendered.contains("older observation payload"));
    assert!(!rendered.contains("latest observation payload"));
}

#[test]
fn previous_observations_skip_duplicate_summaries() {
    let workspace = temp_workspace();
    let mut trace = AgentRunTrace::new(
        &workspace,
        "继续处理",
        AgentRunContext::wechat_chat("user-obs", "commit", vec![]),
    );
    let repeated =
        AgentObservation::tool_result(0, "read_file", "same payload", Some(ObservationKind::Text));
    let repeated_again =
        AgentObservation::tool_result(1, "read_file", "same payload", Some(ObservationKind::Text));
    let latest = AgentObservation::tool_result(
        2,
        "write_file",
        "different payload",
        Some(ObservationKind::FileMutation),
    );
    trace.record_observation(&repeated);
    trace.record_observation(&repeated_again);
    trace.record_observation(&latest);

    let selected = select_previous_observations(&trace, Some(&latest));
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].summary, "same payload");
}

#[test]
fn business_context_snapshot_reads_current_and_recent_tasks() {
    let workspace = temp_workspace();
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
    let current = store
        .record_link_submission("https://example.com/current-task")
        .expect("写入当前任务失败");
    let _recent = store
        .record_link_submission("https://example.com/recent-task")
        .expect("写入最近任务失败");
    let trace = AgentRunTrace::new(
        &workspace,
        "状态 task",
        AgentRunContext::wechat_chat("user-biz", "commit", vec![])
            .with_task_id(current.task_id.clone()),
    );

    let (business, _) = load_business_context_snapshot(
        Some(db_path.as_path()),
        &trace,
        MemoryBudget::default(),
        &RuleRetriever::new(&db_path),
        false,
    )
    .expect("读取业务上下文失败");
    let business = business.expect("应存在业务上下文");

    assert_eq!(
        business
            .current_task
            .as_ref()
            .map(|value| value.task_id.as_str()),
        Some(current.task_id.as_str())
    );
    assert!(!business.recent_tasks.is_empty());
}

#[test]
fn context_assembler_includes_business_context_sections() {
    let workspace = temp_workspace();
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
    let current = store
        .record_link_submission("https://example.com/current-context")
        .expect("写入任务失败");
    let _recent = store
        .record_link_submission("https://example.com/recent-context")
        .expect("写入最近任务失败");
    let trace = AgentRunTrace::new(
        &workspace,
        "帮我看任务",
        AgentRunContext::wechat_chat("user-biz-ctx", "commit", vec![])
            .with_task_id(current.task_id.clone()),
    );
    let (business, _) = load_business_context_snapshot(
        Some(db_path.as_path()),
        &trace,
        MemoryBudget::default(),
        &RuleRetriever::new(&db_path),
        false,
    )
    .expect("读取业务上下文失败");
    let business = business.expect("应存在业务上下文");
    let runtime_session_state =
        derive_runtime_session_state(&trace, "帮我看任务", None, Some(&business));

    let planner_input = ContextAssembler::default().assemble(
        &trace,
        "帮我看任务",
        None,
        Some(&runtime_session_state),
        &["get_task_status: 查询单个任务状态，参数: task_id".to_string()],
        Some(&business),
    );

    assert!(planner_input
        .assembled_user_prompt
        .contains("## Current Task"));
    assert!(planner_input
        .assembled_user_prompt
        .contains(&current.task_id));
    assert!(planner_input
        .assembled_user_prompt
        .contains("## Recent Tasks"));
    assert!(planner_input
        .context_sections
        .iter()
        .any(|section| section.kind == "current_task"));
    assert!(planner_input
        .context_sections
        .iter()
        .any(|section| section.kind == "recent_tasks"));
}

#[test]
fn preview_context_renders_budget_and_sections() {
    let workspace = temp_workspace();
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
    store
        .add_user_memory("user-preview", "我喜欢短摘要")
        .expect("写入 memory 失败");
    let agent = AgentCore::with_task_store_db_path(workspace, &db_path).expect("初始化失败");

    let preview = agent
        .preview_context_with_context(
            "帮我总结一下当前上下文",
            AgentRunContext::wechat_chat("user-preview", "context_debug", vec![])
                .with_session_text("帮我总结一下当前上下文"),
        )
        .expect("应成功生成 preview");

    assert!(preview.contains("# Context Preview"));
    assert!(preview.contains("## Session State"));
    assert!(preview.contains("## Sections"));
    assert!(preview.contains("context_budget: final="));
    assert!(preview.contains("kind=user_memories"));
    assert!(preview.contains("## Prompt Preview"));
}

#[test]
fn preview_context_verbose_includes_section_content_and_memory_drop_details() {
    let workspace = temp_workspace();
    let db_path = temp_db_path();
    let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
    store
        .add_user_memory("user-preview", "这是一条很长的记忆 ".repeat(40).as_str())
        .expect("写入 memory 失败");
    let agent = AgentCore::with_task_store_db_path(workspace, &db_path).expect("初始化失败");

    let preview = agent
        .preview_context_with_context_mode(
            "帮我总结一下当前上下文",
            AgentRunContext::wechat_chat("user-preview", "context_debug", vec![])
                .with_session_text("帮我总结一下当前上下文"),
            ContextPreviewMode::Verbose,
        )
        .expect("应成功生成 preview");

    assert!(preview.contains("## Memory Dropped"));
    assert!(preview.contains("single_item_too_long"));
    assert!(preview.contains("## Section Content Preview"));
    // 低价值 SessionState（仅有默认 goal）已被过滤，不再断言 session_state section
}
