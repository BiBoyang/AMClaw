use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct AgentTrace {
    trace_version: String,
    run_id: String,
    started_at: String,
    finished_at: Option<String>,
    duration_ms: Option<u128>,
    success: bool,
    error: Option<String>,
    final_output: Option<String>,
    user_input: String,
    user_input_chars: usize,
    step_count: usize,
    llm_fallback_reason: Option<String>,
    #[serde(default)]
    recovery_action: Option<String>,
    #[serde(default)]
    recovery_result: Option<String>,
    #[serde(default)]
    memory_retrieved_count: usize,
    #[serde(default)]
    memory_hit_count: usize,
    #[serde(default)]
    memory_dropped_count: usize,
    #[serde(default)]
    memory_total_chars: usize,
    #[serde(default)]
    memory_ids: Vec<String>,
    #[serde(default)]
    retriever_name: String,
    #[serde(default)]
    retrieval_candidate_count: usize,
    #[serde(default)]
    retrieval_hit_count: usize,
    #[serde(default)]
    retrieval_latency_ms: u128,
    #[serde(default)]
    retrieval_mode: String,
    #[serde(default)]
    retrieval_fallback_reason: Option<String>,
    #[serde(default)]
    retrieval_scores_present: bool,
    #[serde(default)]
    persistent_state_present: bool,
    #[serde(default)]
    persistent_state_source: Option<String>,
    #[serde(default)]
    persistent_state_updated: bool,
    #[serde(default)]
    context_pack_present: bool,
    #[serde(default)]
    context_pack_drop_reasons: Vec<String>,
    #[serde(default)]
    context_pack_section_count: usize,
    #[serde(default)]
    context_pack_total_chars: usize,
    #[serde(default)]
    decisions: Vec<DecisionTrace>,
    #[serde(default)]
    failures: Vec<FailureTrace>,
    #[serde(default)]
    recovery_attempts: Vec<RecoveryAttemptTrace>,
    #[serde(default)]
    llm_calls: Vec<LlmCallTrace>,
    #[serde(default)]
    tool_calls: Vec<ToolCallTrace>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct DecisionTrace {
    step: usize,
    #[serde(default)]
    source: String,
    #[serde(default)]
    decision_type: String,
    #[serde(default)]
    summary: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct FailureTrace {
    #[serde(default)]
    step: usize,
    #[serde(default, alias = "kind")]
    failure_type: String,
    #[serde(default, alias = "detail")]
    message: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct RecoveryAttemptTrace {
    #[serde(default)]
    step: usize,
    #[serde(default)]
    failure_kind: String,
    #[serde(default)]
    action: String,
    /// 映射前原始 action（旧 trace 可能缺失，fallback 到 action）
    #[serde(default)]
    original_action: String,
    /// 实际执行的 action（可能因防循环升级，旧 trace 可能缺失）
    #[serde(default)]
    effective_action: String,
    #[serde(default)]
    outcome: String,
    #[serde(default)]
    successful: bool,
    #[serde(default)]
    escalated: bool,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct LlmCallTrace {
    #[serde(default)]
    source: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    success: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    decision_summary: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct ToolCallTrace {
    #[serde(default)]
    step: usize,
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    success: bool,
    #[serde(default)]
    error: Option<String>,
}

/// 当前支持的 trace schema 版本白名单（lib 端写入值见 src/agent_core/trace.rs）。
/// 加载时不认识的版本不丢弃，计入 TraceLoadStats.unsupported_version_count 并在报告展示。
const SUPPORTED_TRACE_VERSIONS: &[&str] = &["agent_trace_v1"];

/// 缺字段疑似漂移检测覆盖的核心字段（JSON 存在性检查，与 serde default 无关）。
/// success/step_count/trace_version 缺失时反序列化会失败（走既有"解析失败"路径），
/// llm_calls/failures 等带 serde default 的字段缺失会静默清零统计，正是本告警要捕获的场景。
const CORE_TRACE_FIELDS: &[&str] = &[
    "trace_version",
    "success",
    "step_count",
    "llm_calls",
    "failures",
];

/// L1 失败分类（taxonomy）全集，
/// 见 notes/agent-eval/specs/EVAL-FAILURE-TAXONOMY-2026-04-18.md §2。
const L1_FAILURE_TYPES: &[&str] = &[
    "llm_auth_error",
    "llm_transport_error",
    "tool_call_error",
    "context_overtrim",
    "memory_conflict",
    "session_state_missing_or_stale",
    "planning_stall_or_drift",
    "done_rule_validation_fail",
    "fallback_exhausted",
    "unknown_failure",
];

/// 运行时 failure kind（src/agent_core/recovery.rs `StepFailureKind::as_str()`）→ L1 taxonomy
/// 的显式映射，全表集中于此。只收录 spec 明确给出的对应关系（§2.G）：
/// `stalled_trajectory` / `trajectory_drift` → `planning_stall_or_drift`。
/// 其余运行时 kind 暂无 spec 对应，按 spec §1.4 约定收敛到 `unknown_failure` 待人工补字典。
const RUNTIME_FAILURE_KIND_TO_L1: &[(&str, &str)] = &[
    ("stalled_trajectory", "planning_stall_or_drift"),
    ("trajectory_drift", "planning_stall_or_drift"),
];

/// 把 trace 中的 failure type 归一到 L1 taxonomy：
/// - 已是 L1 名（旧合成 baseline 直接写 L1 名）→ 原样返回，保证幂等；
/// - 命中运行时映射表 → 对应 L1 类；
/// - 其余（未映射的运行时 kind、空串、未知字符串）→ `unknown_failure`。
fn map_failure_kind_to_l1(raw: &str) -> String {
    let kind = raw.trim();
    if L1_FAILURE_TYPES.contains(&kind) {
        return kind.to_string();
    }
    if let Some((_, l1)) = RUNTIME_FAILURE_KIND_TO_L1
        .iter()
        .find(|(runtime_kind, _)| *runtime_kind == kind)
    {
        return (*l1).to_string();
    }
    "unknown_failure".to_string()
}

/// trace 加载阶段的 schema 健康度统计（trace_version 门禁 + 缺字段疑似漂移）。
#[derive(Debug, Clone, Default)]
struct TraceLoadStats {
    /// trace_version 不在 SUPPORTED_TRACE_VERSIONS 白名单内的 trace 数（仍加载，不中断）。
    unsupported_version_count: usize,
    /// 核心字段缺失计数：key 为字段名，value 为缺该字段的 trace 文件数。
    missing_core_field_counts: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize)]
struct TraceSummary {
    run_id: String,
    started_at: String,
    user_input: String,
    user_input_chars: usize,
    success: bool,
    error_short: Option<String>,
    duration_ms: Option<u128>,
    step_count: usize,
    llm_fallback: bool,
    has_failures: bool,
    failure_count: usize,
    failure_types: Vec<String>,
    memory_retrieved: usize,
    memory_injected: usize,
    memory_dropped: usize,
    memory_total_chars: usize,
    retriever_name: String,
    retrieval_candidate_count: usize,
    retrieval_hit_count: usize,
    retrieval_latency_ms: u128,
    retrieval_mode: String,
    retrieval_fallback_reason: Option<String>,
    retrieval_scores_present: bool,
    state_present: bool,
    context_pack_dropped: bool,
    context_pack_drop_reasons: Vec<String>,
    llm_call_count: usize,
    llm_success_count: usize,
    llm_failure_count: usize,
    tool_call_count: usize,
    tool_success_count: usize,
    tool_failure_count: usize,
    tool_error_types: Vec<String>,
    has_recovery_attempt: bool,
    recovery_attempt_count: usize,
    recovery_success_count: usize,
    recovery_succeeded: Option<bool>,
    recovery_actions: Vec<String>,
    recovery_results: Vec<String>,
    recovery_attempt_details: Vec<RecoveryAttemptSummary>,
    in_baseline: bool,
    is_interesting: bool,
    interest_reasons: Vec<String>,
    persistent_state_updated: bool,
}

#[derive(Debug, Clone, Serialize)]
struct RecoveryAttemptSummary {
    failure_kind: String,
    successful: bool,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut trace_dir = PathBuf::from("data/agent_traces");
    let mut date = None;
    let mut output_path = PathBuf::from("notes/agent-eval/reports/TRACE-EVAL-REPORT.md");
    let mut baseline_path = Some(PathBuf::from(
        "notes/agent-eval/baselines/EVAL-BASELINE-SAMPLES-2026-04-18.md",
    ));
    let mut only_interesting = false;
    let mut compare_before = None;
    let mut compare_after = None;
    let mut compare_output = None;
    let mut gate_mode = false;
    let mut gate_strict = false;
    let mut gate_json = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => {
                if let Some(v) = args.next() {
                    trace_dir = PathBuf::from(v);
                }
            }
            "--date" => {
                if let Some(v) = args.next() {
                    date = Some(v);
                }
            }
            "--output" => {
                if let Some(v) = args.next() {
                    output_path = PathBuf::from(v);
                }
            }
            "--baseline" => {
                if let Some(v) = args.next() {
                    baseline_path = Some(PathBuf::from(v));
                }
            }
            "--no-baseline" => {
                baseline_path = None;
            }
            "--only-interesting" => {
                only_interesting = true;
            }
            "--compare-before" => {
                if let Some(v) = args.next() {
                    compare_before = Some(PathBuf::from(v));
                }
            }
            "--compare-after" => {
                if let Some(v) = args.next() {
                    compare_after = Some(PathBuf::from(v));
                }
            }
            "--compare-output" => {
                if let Some(v) = args.next() {
                    compare_output = Some(PathBuf::from(v));
                }
            }
            "--gate" => {
                gate_mode = true;
            }
            "--gate-strict" => {
                gate_mode = true;
                gate_strict = true;
            }
            "--gate-json" => {
                gate_json = true;
            }
            _ => {}
        }
    }

    // Check compare mode
    let is_compare_mode = compare_before.is_some() || compare_after.is_some();
    if is_compare_mode {
        let before = match compare_before {
            Some(p) => p,
            None => {
                eprintln!("error: --compare-after provided but --compare-before is missing");
                std::process::exit(1);
            }
        };
        let after = match compare_after {
            Some(p) => p,
            None => {
                eprintln!("error: --compare-before provided but --compare-after is missing");
                std::process::exit(1);
            }
        };
        run_compare_mode(
            &before,
            &after,
            compare_output.as_ref(),
            gate_mode,
            gate_strict,
            gate_json,
        );
        return;
    }

    let mut load_stats = TraceLoadStats::default();
    let traces = if let Some(ref d) = date {
        load_traces_for_date(&trace_dir, d, &mut load_stats)
    } else {
        load_all_traces(&trace_dir, &mut load_stats)
    };

    if traces.is_empty() {
        println!("未找到任何 trace 文件");
        return;
    }

    let baseline_run_ids = baseline_path
        .as_ref()
        .map(|path| load_baseline_run_ids(path))
        .unwrap_or_default();
    if baseline_path.is_some() && baseline_run_ids.is_empty() {
        eprintln!("baseline 样本未加载到 run_id，报告将只输出全量统计");
    }

    let summaries: Vec<TraceSummary> = traces
        .iter()
        .map(|trace| summarize_trace(trace, &baseline_run_ids))
        .collect();

    let report = build_report(
        &summaries,
        only_interesting,
        &baseline_run_ids,
        baseline_path.as_deref(),
        &load_stats,
    );
    fs::write(&output_path, report).expect("写入报告失败");
    // 同步写 JSON sidecar（同名 .json），供 compare 结构化消费；markdown 内容不变
    let sidecar = build_report_sidecar(&summaries, &baseline_run_ids, &load_stats);
    let sidecar_json = serde_json::to_string_pretty(&sidecar).expect("序列化报告 sidecar 失败");
    let sidecar_file = sidecar_path(&output_path);
    fs::write(&sidecar_file, sidecar_json).expect("写入报告 sidecar 失败");
    println!("报告已生成: {}", output_path.display());
    println!("报告 sidecar 已生成: {}", sidecar_file.display());
    println!(
        "总计 trace: {}，值得关注: {}",
        summaries.len(),
        summaries.iter().filter(|s| s.is_interesting).count()
    );
}

fn load_traces_for_date(root: &Path, date: &str, stats: &mut TraceLoadStats) -> Vec<AgentTrace> {
    let dir = root.join(date);
    load_traces_from_dir(&dir, stats)
}

fn load_all_traces(root: &Path, stats: &mut TraceLoadStats) -> Vec<AgentTrace> {
    let mut traces = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return traces;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            traces.extend(load_traces_from_dir(&path, stats));
        }
    }
    traces
}

fn load_traces_from_dir(dir: &Path, stats: &mut TraceLoadStats) -> Vec<AgentTrace> {
    let mut traces = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return traces;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        if path.file_name().and_then(|s| s.to_str()) == Some("index.jsonl") {
            continue;
        }
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        // 先按 JSON Value 做核心字段存在性统计（serde default 会把缺字段静默清零），
        // 再反序列化为结构体；Value 都解析不出来时走既有"解析失败"路径。
        let value = match serde_json::from_str::<serde_json::Value>(&content) {
            Ok(value) => value,
            Err(err) => {
                eprintln!("解析失败 {}: {}", path.display(), err);
                continue;
            }
        };
        for field in CORE_TRACE_FIELDS {
            if value.get(field).is_none() {
                *stats
                    .missing_core_field_counts
                    .entry((*field).to_string())
                    .or_insert(0) += 1;
            }
        }
        match serde_json::from_value::<AgentTrace>(value) {
            Ok(trace) => {
                if !SUPPORTED_TRACE_VERSIONS.contains(&trace.trace_version.as_str()) {
                    stats.unsupported_version_count += 1;
                    eprintln!(
                        "trace_version={} 不在支持白名单 {:?}，仍加载: {}",
                        trace.trace_version,
                        SUPPORTED_TRACE_VERSIONS,
                        path.display()
                    );
                }
                traces.push(trace);
            }
            Err(err) => eprintln!("解析失败 {}: {}", path.display(), err),
        }
    }
    traces
}

fn load_baseline_run_ids(path: &Path) -> HashSet<String> {
    let Ok(content) = fs::read_to_string(path) else {
        return HashSet::new();
    };
    let mut run_ids = HashSet::new();
    for line in content.lines() {
        if !line.contains('`') {
            continue;
        }
        let tokens: Vec<&str> = line.split('`').collect();
        for token in tokens.into_iter().skip(1).step_by(2) {
            if is_uuid_like(token) {
                run_ids.insert(token.to_string());
            }
        }
    }
    run_ids
}

fn is_uuid_like(token: &str) -> bool {
    token.len() == 36 && token.chars().all(|ch| ch.is_ascii_hexdigit() || ch == '-')
}

fn summarize_trace(trace: &AgentTrace, baseline_run_ids: &HashSet<String>) -> TraceSummary {
    let mut interest_reasons = Vec::new();

    if !trace.success {
        interest_reasons.push("failed".to_string());
    }
    if trace.memory_dropped_count > 0 {
        interest_reasons.push("memory_dropped".to_string());
    }
    if !trace.context_pack_drop_reasons.is_empty() {
        interest_reasons.push("context_pack_dropped".to_string());
    }
    if !trace.failures.is_empty() {
        interest_reasons.push("has_failures".to_string());
    }
    if trace.llm_fallback_reason.is_some() {
        interest_reasons.push("llm_fallback".to_string());
    }
    if trace.memory_retrieved_count > 0 && trace.memory_hit_count == 0 {
        interest_reasons.push("memory_retrieved_but_none_injected".to_string());
    }
    if trace.retrieval_fallback_reason.is_some() {
        interest_reasons.push("retrieval_fallback".to_string());
    }
    if trace.persistent_state_updated {
        interest_reasons.push("state_updated".to_string());
    }

    let is_interesting = !interest_reasons.is_empty();
    let mut seen_types = std::collections::HashSet::new();
    // 统一归一到 L1 taxonomy 后再去重：运行时 kind（stalled_trajectory 等）先映射，
    // 已是 L1 名的旧数据幂等不变；未映射的运行时 kind 收敛为 unknown_failure。
    let failure_types: Vec<String> = trace
        .failures
        .iter()
        .map(|failure| map_failure_kind_to_l1(&failure.failure_type))
        .filter(|ft| seen_types.insert(ft.clone()))
        .collect();

    let llm_success = trace.llm_calls.iter().filter(|call| call.success).count();
    let llm_failure = trace.llm_calls.len().saturating_sub(llm_success);
    let tool_success = trace.tool_calls.iter().filter(|call| call.success).count();
    let tool_failure = trace.tool_calls.len().saturating_sub(tool_success);
    let tool_error_types: Vec<String> = trace
        .tool_calls
        .iter()
        .filter(|call| !call.success && call.error.is_some())
        .filter_map(|call| call.error.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let mut recovery_actions = Vec::new();
    let mut recovery_results = Vec::new();
    let mut recovery_attempt_details = Vec::new();
    let (recovery_attempt_count, recovery_success_count, has_recovery_attempt, recovery_succeeded) =
        if !trace.recovery_attempts.is_empty() {
            let success_count = trace
                .recovery_attempts
                .iter()
                .filter(|attempt| attempt.successful)
                .count();
            for attempt in &trace.recovery_attempts {
                if !attempt.action.is_empty() {
                    recovery_actions.push(attempt.action.clone());
                }
                if !attempt.outcome.is_empty() {
                    recovery_results.push(attempt.outcome.clone());
                }
                recovery_attempt_details.push(RecoveryAttemptSummary {
                    failure_kind: if attempt.failure_kind.trim().is_empty() {
                        "unknown".to_string()
                    } else {
                        attempt.failure_kind.clone()
                    },
                    successful: attempt.successful,
                });
            }
            (
                trace.recovery_attempts.len(),
                success_count,
                true,
                Some(success_count > 0),
            )
        } else if !trace.failures.is_empty() {
            if let Some(action) = &trace.recovery_action {
                recovery_actions.push(action.clone());
            }
            if let Some(result) = &trace.recovery_result {
                recovery_results.push(result.clone());
            }
            let fallback_failure_kind = trace
                .failures
                .first()
                .map(|failure| {
                    if failure.failure_type.trim().is_empty() {
                        "unknown".to_string()
                    } else {
                        failure.failure_type.clone()
                    }
                })
                .unwrap_or_else(|| "unknown".to_string());
            recovery_attempt_details.push(RecoveryAttemptSummary {
                failure_kind: fallback_failure_kind,
                successful: trace.success,
            });
            (1, usize::from(trace.success), true, Some(trace.success))
        } else {
            (0, 0, false, None)
        };

    TraceSummary {
        run_id: trace.run_id.clone(),
        started_at: trace.started_at.clone(),
        user_input: trace.user_input.clone(),
        user_input_chars: trace.user_input_chars,
        success: trace.success,
        error_short: trace.error.as_ref().map(|err| {
            if err.chars().count() > 80 {
                format!("{}...", err.chars().take(80).collect::<String>())
            } else {
                err.clone()
            }
        }),
        duration_ms: trace.duration_ms,
        step_count: trace.step_count,
        llm_fallback: trace.llm_fallback_reason.is_some(),
        has_failures: !trace.failures.is_empty(),
        failure_count: trace.failures.len(),
        failure_types,
        memory_retrieved: trace.memory_retrieved_count,
        memory_injected: trace.memory_hit_count,
        memory_dropped: trace.memory_dropped_count,
        memory_total_chars: trace.memory_total_chars,
        retriever_name: trace.retriever_name.clone(),
        retrieval_candidate_count: trace.retrieval_candidate_count,
        retrieval_hit_count: trace.retrieval_hit_count,
        retrieval_latency_ms: trace.retrieval_latency_ms,
        retrieval_mode: trace.retrieval_mode.clone(),
        retrieval_fallback_reason: trace.retrieval_fallback_reason.clone(),
        retrieval_scores_present: trace.retrieval_scores_present,
        state_present: trace.persistent_state_present,
        context_pack_dropped: !trace.context_pack_drop_reasons.is_empty(),
        context_pack_drop_reasons: trace.context_pack_drop_reasons.clone(),
        llm_call_count: trace.llm_calls.len(),
        llm_success_count: llm_success,
        llm_failure_count: llm_failure,
        tool_call_count: trace.tool_calls.len(),
        tool_success_count: tool_success,
        tool_failure_count: tool_failure,
        tool_error_types,
        has_recovery_attempt,
        recovery_attempt_count,
        recovery_success_count,
        recovery_succeeded,
        recovery_actions,
        recovery_results,
        recovery_attempt_details,
        in_baseline: baseline_run_ids.contains(&trace.run_id),
        is_interesting,
        interest_reasons,
        persistent_state_updated: trace.persistent_state_updated,
    }
}

fn build_report(
    summaries: &[TraceSummary],
    only_interesting: bool,
    baseline_run_ids: &HashSet<String>,
    baseline_path: Option<&Path>,
    load_stats: &TraceLoadStats,
) -> String {
    let mut lines = vec![
        "# Trace Evaluation Report".to_string(),
        String::new(),
        format!("- generated: {}", chrono::Utc::now().to_rfc3339()),
        format!("- total traces: {}", summaries.len()),
        format!("- baseline_file: {}", display_path_or_na(baseline_path)),
        format!("- baseline_run_ids: {}", baseline_run_ids.len()),
        format!(
            "- interesting traces: {}",
            summaries
                .iter()
                .filter(|summary| summary.is_interesting)
                .count()
        ),
        String::new(),
        "## Summary Statistics".to_string(),
        String::new(),
    ];

    let total = summaries.len();
    let success_count = summaries.iter().filter(|summary| summary.success).count();
    let with_memory = summaries
        .iter()
        .filter(|summary| summary.memory_injected > 0)
        .count();
    let with_dropped = summaries
        .iter()
        .filter(|summary| summary.memory_dropped > 0)
        .count();
    let with_state = summaries
        .iter()
        .filter(|summary| summary.state_present)
        .count();
    let with_ctx_drop = summaries
        .iter()
        .filter(|summary| summary.context_pack_dropped)
        .count();
    let with_fallback = summaries
        .iter()
        .filter(|summary| summary.llm_fallback)
        .count();
    let with_failures = summaries
        .iter()
        .filter(|summary| summary.has_failures)
        .count();
    let with_state_updated = summaries
        .iter()
        .filter(|summary| summary.persistent_state_updated)
        .count();

    lines.push("| metric | count | ratio |".to_string());
    lines.push("| --- | ---: | ---: |".to_string());
    lines.push(format!("| total | {} | 100% |", total));
    lines.push(format!(
        "| success | {} | {} |",
        success_count,
        ratio_cell(success_count, total, Direction::HigherIsBetter)
    ));
    lines.push(format!(
        "| with memory injected | {} | {} |",
        with_memory,
        ratio_cell(with_memory, total, Direction::HigherIsBetter)
    ));
    lines.push(format!(
        "| with memory dropped | {} | {:.1}% |",
        with_dropped,
        pct(with_dropped, total)
    ));
    lines.push(format!(
        "| with session state | {} | {} |",
        with_state,
        ratio_cell(with_state, total, Direction::HigherIsBetter)
    ));
    lines.push(format!(
        "| with context pack dropped | {} | {} |",
        with_ctx_drop,
        ratio_cell(with_ctx_drop, total, Direction::LowerIsBetter)
    ));
    lines.push(format!(
        "| with llm fallback | {} | {} |",
        with_fallback,
        ratio_cell(with_fallback, total, Direction::LowerIsBetter)
    ));
    lines.push(format!(
        "| with failures | {} | {:.1}% |",
        with_failures,
        pct(with_failures, total)
    ));
    lines.push(format!(
        "| with persistent state updated | {} | {:.1}% |",
        with_state_updated,
        pct(with_state_updated, total)
    ));
    lines.push(String::new());

    // === Trace Schema Health（版本门禁 + 缺字段疑似漂移；仅观测告警，不中断加载）===
    lines.push("## Trace Schema Health".to_string());
    lines.push(String::new());
    lines.push("| metric | count |".to_string());
    lines.push("| --- | ---: |".to_string());
    lines.push(format!(
        "| unsupported_version_count | {} |",
        load_stats.unsupported_version_count
    ));
    for field in CORE_TRACE_FIELDS {
        let missing = load_stats
            .missing_core_field_counts
            .get(*field)
            .copied()
            .unwrap_or(0);
        lines.push(format!("| missing_field:{} | {} |", field, missing));
    }
    let missing_field_total: usize = load_stats.missing_core_field_counts.values().sum();
    if load_stats.unsupported_version_count > 0 || missing_field_total > 0 {
        lines.push(String::new());
        lines.push(format!(
            "> ⚠ 检测到 schema 疑似漂移：{} 条 trace 版本不在白名单 {:?}，核心字段缺失 {} 处；请核对 lib 端 trace 结构是否改名/删字段。",
            load_stats.unsupported_version_count, SUPPORTED_TRACE_VERSIONS, missing_field_total
        ));
    }
    lines.push(String::new());

    // === Persistent State Update Breakdown ===
    let updated_true_summaries: Vec<_> = summaries
        .iter()
        .filter(|s| s.persistent_state_updated)
        .collect();
    let updated_false_summaries: Vec<_> = summaries
        .iter()
        .filter(|s| !s.persistent_state_updated)
        .collect();
    let updated_true_count = updated_true_summaries.len();
    let updated_false_count = updated_false_summaries.len();
    let updated_true_success = updated_true_summaries.iter().filter(|s| s.success).count();
    let updated_false_success = updated_false_summaries.iter().filter(|s| s.success).count();
    let updated_true_success_rate = if updated_true_count > 0 {
        updated_true_success as f64 / updated_true_count as f64 * 100.0
    } else {
        0.0
    };
    let updated_false_success_rate = if updated_false_count > 0 {
        updated_false_success as f64 / updated_false_count as f64 * 100.0
    } else {
        0.0
    };

    lines.push("## Persistent State Update Breakdown".to_string());
    lines.push(String::new());
    lines.push("| updated | traces | success | success_rate |".to_string());
    lines.push("| --- | ---: | ---: | ---: |".to_string());
    lines.push(format!(
        "| true | {} | {} | {:.1}% |",
        updated_true_count, updated_true_success, updated_true_success_rate
    ));
    lines.push(format!(
        "| false | {} | {} | {:.1}% |",
        updated_false_count, updated_false_success, updated_false_success_rate
    ));
    lines.push(String::new());

    if !baseline_run_ids.is_empty() {
        let baseline_hit = summaries
            .iter()
            .filter(|summary| summary.in_baseline)
            .count();
        let baseline_missing = baseline_run_ids.len().saturating_sub(baseline_hit);
        lines.push("## Baseline Coverage".to_string());
        lines.push(String::new());
        lines.push("| metric | count | ratio |".to_string());
        lines.push("| --- | ---: | ---: |".to_string());
        lines.push(format!(
            "| baseline run ids | {} | 100% |",
            baseline_run_ids.len()
        ));
        lines.push(format!(
            "| baseline hits in current trace set | {} | {:.1}% |",
            baseline_hit,
            pct(baseline_hit, baseline_run_ids.len())
        ));
        lines.push(format!(
            "| baseline missing in current trace set | {} | {:.1}% |",
            baseline_missing,
            pct(baseline_missing, baseline_run_ids.len())
        ));
        lines.push(String::new());
    }

    // === Retrieval Dimension (enhanced) ===
    let mut all_latencies: Vec<u128> = Vec::new();
    let mut fallback_count = 0usize;
    let mut total_candidates = 0usize;
    let mut total_hits = 0usize;
    let mut retriever_counter: HashMap<String, (usize, usize, usize, u128)> = HashMap::new();
    let mut mode_counter: HashMap<String, (usize, usize, usize, u128)> = HashMap::new();

    for summary in summaries {
        if summary.retrieval_latency_ms > 0 {
            all_latencies.push(summary.retrieval_latency_ms);
        }
        if summary.retrieval_fallback_reason.is_some() {
            fallback_count += 1;
        }
        total_candidates += summary.retrieval_candidate_count;
        total_hits += summary.retrieval_hit_count;

        // by retriever_name
        let name = if summary.retriever_name.is_empty() {
            "(unknown)".to_string()
        } else {
            summary.retriever_name.clone()
        };
        let entry = retriever_counter.entry(name).or_insert((0, 0, 0, 0));
        entry.0 += 1;
        entry.1 += summary.retrieval_candidate_count;
        entry.2 += summary.retrieval_hit_count;
        entry.3 += summary.retrieval_latency_ms;

        // by retrieval_mode
        let mode = if summary.retrieval_mode.is_empty() {
            "(unknown)".to_string()
        } else {
            summary.retrieval_mode.clone()
        };
        let mentry = mode_counter.entry(mode).or_insert((0, 0, 0, 0));
        mentry.0 += 1;
        mentry.1 += summary.retrieval_candidate_count;
        mentry.2 += summary.retrieval_hit_count;
        mentry.3 += summary.retrieval_latency_ms;
    }

    lines.push("## Retrieval Statistics".to_string());
    lines.push(String::new());

    // Latency quantiles
    let (p50, p95) = latency_quantiles(&all_latencies);
    let fallback_rate = if total > 0 {
        fallback_count as f64 / total as f64 * 100.0
    } else {
        0.0
    };
    let hit_ratio = if total_candidates > 0 {
        total_hits as f64 / total_candidates as f64 * 100.0
    } else {
        0.0
    };

    lines.push("| metric | value |".to_string());
    lines.push("| --- | --- |".to_string());
    lines.push(format!("| latency_p50_ms | {} |", p50));
    lines.push(format!("| latency_p95_ms | {} |", p95));
    lines.push(format!("| fallback_rate | {:.1}% |", fallback_rate));
    lines.push(format!(
        "| candidate_hit_ratio | {:.1}% ({} / {}) |",
        hit_ratio, total_hits, total_candidates
    ));
    lines.push(String::new());

    // By retriever_name
    if !retriever_counter.is_empty() {
        lines.push("### By Retriever Name".to_string());
        lines.push(String::new());
        lines.push(
            "| retriever | traces | avg_candidates | avg_hits | avg_latency_ms |".to_string(),
        );
        lines.push("| --- | ---: | ---: | ---: | ---: |".to_string());
        let mut pairs = retriever_counter.into_iter().collect::<Vec<_>>();
        pairs.sort_by_key(|right| std::cmp::Reverse(right.1 .0));
        for (name, (count, candidates, hits, latency)) in pairs {
            let avg_candidates = if count > 0 {
                candidates as f64 / count as f64
            } else {
                0.0
            };
            let avg_hits = if count > 0 {
                hits as f64 / count as f64
            } else {
                0.0
            };
            let avg_latency = if count > 0 {
                latency as f64 / count as f64
            } else {
                0.0
            };
            lines.push(format!(
                "| {} | {} | {:.1} | {:.1} | {:.1} |",
                name, count, avg_candidates, avg_hits, avg_latency
            ));
        }
        lines.push(String::new());
    }

    // By retrieval_mode
    if !mode_counter.is_empty() {
        lines.push("### By Retrieval Mode".to_string());
        lines.push(String::new());
        lines.push("| mode | traces | avg_candidates | avg_hits | avg_latency_ms |".to_string());
        lines.push("| --- | ---: | ---: | ---: | ---: |".to_string());
        let mut pairs = mode_counter.into_iter().collect::<Vec<_>>();
        pairs.sort_by_key(|right| std::cmp::Reverse(right.1 .0));
        for (mode, (count, candidates, hits, latency)) in pairs {
            let avg_candidates = if count > 0 {
                candidates as f64 / count as f64
            } else {
                0.0
            };
            let avg_hits = if count > 0 {
                hits as f64 / count as f64
            } else {
                0.0
            };
            let avg_latency = if count > 0 {
                latency as f64 / count as f64
            } else {
                0.0
            };
            lines.push(format!(
                "| {} | {} | {:.1} | {:.1} | {:.1} |",
                mode, count, avg_candidates, avg_hits, avg_latency
            ));
        }
        lines.push(String::new());
    }

    lines.push("## Failure Type Distribution".to_string());
    lines.push(String::new());
    let mut failure_counter: HashMap<String, usize> = HashMap::new();
    for summary in summaries {
        for failure_type in &summary.failure_types {
            *failure_counter.entry(failure_type.clone()).or_insert(0) += 1;
        }
    }
    lines.push("| failure_type | count | ratio |".to_string());
    lines.push("| --- | ---: | ---: |".to_string());
    if failure_counter.is_empty() {
        lines.push("| (none) | 0 | 0.0% |".to_string());
    } else {
        let mut pairs = failure_counter.into_iter().collect::<Vec<_>>();
        pairs.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        for (failure_type, count) in pairs {
            let ratio = if failure_type == "unknown_failure" {
                ratio_cell(count, total, Direction::LowerIsBetter)
            } else {
                format!("{:.1}%", pct(count, total))
            };
            lines.push(format!("| {} | {} | {} |", failure_type, count, ratio));
        }
    }
    lines.push(String::new());

    // === Tool Use Dimension ===
    lines.push("## Tool Use Statistics".to_string());
    lines.push(String::new());
    let traces_with_tools = summaries.iter().filter(|s| s.tool_call_count > 0).count();
    let total_tool_calls: usize = summaries.iter().map(|s| s.tool_call_count).sum();
    let total_tool_success: usize = summaries.iter().map(|s| s.tool_success_count).sum();
    let total_tool_failure = total_tool_calls.saturating_sub(total_tool_success);
    let tool_failure_rate = if total_tool_calls > 0 {
        (total_tool_failure as f64 / total_tool_calls as f64) * 100.0
    } else {
        0.0
    };
    lines.push("| metric | count | ratio |".to_string());
    lines.push("| --- | ---: | ---: |".to_string());
    lines.push(format!(
        "| traces with tool calls | {} | {:.1}% |",
        traces_with_tools,
        pct(traces_with_tools, total)
    ));
    lines.push(format!("| total tool calls | {} | - |", total_tool_calls));
    lines.push(format!(
        "| tool success | {} | {} |",
        total_tool_success,
        ratio_cell(
            total_tool_success,
            total_tool_calls,
            Direction::HigherIsBetter
        )
    ));
    lines.push(format!(
        "| tool failure | {} | {:.1}% |",
        total_tool_failure, tool_failure_rate
    ));
    lines.push(String::new());

    // Tool error type topN
    let mut tool_error_counter: HashMap<String, usize> = HashMap::new();
    for summary in summaries {
        for error_type in &summary.tool_error_types {
            *tool_error_counter.entry(error_type.clone()).or_insert(0) += 1;
        }
    }
    if !tool_error_counter.is_empty() {
        lines.push("### Tool Error Type TopN".to_string());
        lines.push(String::new());
        lines.push("| error_type | count |".to_string());
        lines.push("| --- | ---: |".to_string());
        let mut pairs = tool_error_counter.into_iter().collect::<Vec<_>>();
        pairs.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        for (error_type, count) in pairs.iter().take(5) {
            lines.push(format!("| {} | {} |", error_type, count));
        }
        lines.push(String::new());
    }

    // Tool call count distribution
    lines.push("### Tool Call Count Distribution".to_string());
    lines.push(String::new());
    let mut tool_count_dist: HashMap<usize, usize> = HashMap::new();
    for summary in summaries {
        if summary.tool_call_count > 0 {
            *tool_count_dist.entry(summary.tool_call_count).or_insert(0) += 1;
        }
    }
    lines.push("| tool_calls | trace_count |".to_string());
    lines.push("| --- | ---: |".to_string());
    let mut dist_pairs = tool_count_dist.into_iter().collect::<Vec<_>>();
    dist_pairs.sort_by_key(|left| left.0);
    for (call_count, trace_count) in dist_pairs {
        lines.push(format!("| {} | {} |", call_count, trace_count));
    }
    lines.push(String::new());

    // === Planning Dimension ===
    lines.push("## Planning / ReAct Statistics".to_string());
    lines.push(String::new());
    let step_counts: Vec<usize> = summaries.iter().map(|s| s.step_count).collect();
    let min_steps = step_counts.iter().min().copied().unwrap_or(0);
    let max_steps = step_counts.iter().max().copied().unwrap_or(0);
    let avg_steps = if !step_counts.is_empty() {
        step_counts.iter().sum::<usize>() as f64 / step_counts.len() as f64
    } else {
        0.0
    };
    let unfinished_count = summaries
        .iter()
        .filter(|s| !s.success && s.step_count > 5)
        .count();
    let stall_drift_count = summaries
        .iter()
        .filter(|s| {
            s.failure_types
                .iter()
                .any(|ft| ft == "planning_stall_or_drift")
        })
        .count();
    lines.push("| metric | value |".to_string());
    lines.push("| --- | --- |".to_string());
    lines.push(format!(
        "| step_count min / max / avg | {} / {} / {:.1} |",
        min_steps, max_steps, avg_steps
    ));
    lines.push(format!(
        "| unfinished_plan (failed + steps > 5) | {} |",
        unfinished_count
    ));
    lines.push(format!(
        "| stall_or_drift hits | {} | {} |",
        stall_drift_count,
        ratio_cell(stall_drift_count, total, Direction::LowerIsBetter)
    ));
    lines.push(String::new());

    // Step count distribution
    lines.push("### Step Count Distribution".to_string());
    lines.push(String::new());
    let mut step_dist: HashMap<String, usize> = HashMap::new();
    for summary in summaries {
        let bucket = match summary.step_count {
            1 => "1".to_string(),
            2 => "2".to_string(),
            3..=5 => "3-5".to_string(),
            6..=10 => "6-10".to_string(),
            _ => "10+".to_string(),
        };
        *step_dist.entry(bucket).or_insert(0) += 1;
    }
    lines.push("| step_range | trace_count | ratio |".to_string());
    lines.push("| --- | ---: | ---: |".to_string());
    let bucket_order = vec!["1", "2", "3-5", "6-10", "10+"];
    for bucket in bucket_order {
        if let Some(count) = step_dist.get(bucket) {
            lines.push(format!(
                "| {} | {} | {:.1}% |",
                bucket,
                count,
                pct(*count, total)
            ));
        }
    }
    lines.push(String::new());

    // === Recovery Dimension ===
    lines.push("## Recovery Statistics".to_string());
    lines.push(String::new());
    let traces_with_recovery = summaries.iter().filter(|s| s.has_recovery_attempt).count();
    let recovery_attempts: usize = summaries.iter().map(|s| s.recovery_attempt_count).sum();
    let recovery_successes: usize = summaries.iter().map(|s| s.recovery_success_count).sum();
    let recovery_failures = recovery_attempts.saturating_sub(recovery_successes);
    let recovery_failure_rate = if recovery_attempts > 0 {
        (recovery_failures as f64 / recovery_attempts as f64) * 100.0
    } else {
        0.0
    };
    lines.push("| metric | count | ratio |".to_string());
    lines.push("| --- | ---: | ---: |".to_string());
    lines.push(format!(
        "| traces_with_recovery | {} | {:.1}% |",
        traces_with_recovery,
        pct(traces_with_recovery, total)
    ));
    lines.push(format!(
        "| recovery_attempt_count | {} | - |",
        recovery_attempts
    ));
    lines.push(format!(
        "| recovery_success | {} | {} |",
        recovery_successes,
        ratio_cell(
            recovery_successes,
            recovery_attempts,
            Direction::HigherIsBetter
        )
    ));
    lines.push(format!(
        "| recovery_failure | {} | {:.1}% |",
        recovery_failures, recovery_failure_rate
    ));
    lines.push(String::new());

    let mut recovery_action_counter: HashMap<String, usize> = HashMap::new();
    let mut recovery_result_counter: HashMap<String, usize> = HashMap::new();
    for summary in summaries {
        for action in &summary.recovery_actions {
            *recovery_action_counter.entry(action.clone()).or_insert(0) += 1;
        }
        for result in &summary.recovery_results {
            *recovery_result_counter.entry(result.clone()).or_insert(0) += 1;
        }
    }
    if !recovery_action_counter.is_empty() {
        lines.push("### Recovery Action Distribution".to_string());
        lines.push(String::new());
        lines.push("| action | count |".to_string());
        lines.push("| --- | ---: |".to_string());
        let mut pairs = recovery_action_counter.into_iter().collect::<Vec<_>>();
        pairs.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        for (action, count) in pairs {
            lines.push(format!("| {} | {} |", action, count));
        }
        lines.push(String::new());
    }
    if !recovery_result_counter.is_empty() {
        lines.push("### Recovery Result Distribution".to_string());
        lines.push(String::new());
        lines.push("| result | count |".to_string());
        lines.push("| --- | ---: |".to_string());
        let mut pairs = recovery_result_counter.into_iter().collect::<Vec<_>>();
        pairs.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        for (result, count) in pairs {
            lines.push(format!("| {} | {} |", result, count));
        }
        lines.push(String::new());
    }

    // Recovery by failure type
    if recovery_attempts > 0 {
        lines.push("### Recovery by Failure Type".to_string());
        lines.push(String::new());
        lines.push("| failure_type | attempt | success | failure |".to_string());
        lines.push("| --- | ---: | ---: | ---: |".to_string());
        let mut recovery_by_type: HashMap<String, (usize, usize, usize)> = HashMap::new();
        for summary in summaries {
            for attempt in &summary.recovery_attempt_details {
                let entry = recovery_by_type
                    .entry(attempt.failure_kind.clone())
                    .or_insert((0, 0, 0));
                entry.0 += 1;
                if attempt.successful {
                    entry.1 += 1;
                } else {
                    entry.2 += 1;
                }
            }
        }
        let mut pairs = recovery_by_type.into_iter().collect::<Vec<_>>();
        pairs.sort_by(|left, right| {
            let left_attempt = (left.1).0;
            let right_attempt = (right.1).0;
            right_attempt
                .cmp(&left_attempt)
                .then_with(|| left.0.cmp(&right.0))
        });
        for (failure_type, (attempt, success, failure)) in pairs {
            lines.push(format!(
                "| {} | {} | {} | {} |",
                failure_type, attempt, success, failure
            ));
        }
        lines.push(String::new());
    }

    lines.push("## Per-Trace Detail".to_string());
    lines.push(String::new());
    lines.push(
        "| run_id | success | baseline | steps | mem(r/i/d) | state | state_upd | ctx_drop | failures | reasons | input |"
            .to_string(),
    );
    lines.push("| --- | --- | --- | ---: | --- | --- | --- | --- | --- | --- | --- |".to_string());

    for summary in summaries {
        if only_interesting && !summary.is_interesting {
            continue;
        }
        let input_short = if summary.user_input.chars().count() > 40 {
            format!(
                "{}...",
                summary.user_input.chars().take(40).collect::<String>()
            )
        } else {
            summary.user_input.clone()
        };
        lines.push(format!(
            "| `{}` | {} | {} | {} | {}/{}/{} | {} | {} | {} | {} | {} | {} |",
            &summary.run_id[..8.min(summary.run_id.len())],
            if summary.success { "✓" } else { "✗" },
            if summary.in_baseline { "✓" } else { "·" },
            summary.step_count,
            summary.memory_retrieved,
            summary.memory_injected,
            summary.memory_dropped,
            if summary.state_present { "✓" } else { "·" },
            if summary.persistent_state_updated {
                "✓"
            } else {
                "·"
            },
            if summary.context_pack_dropped {
                "✓"
            } else {
                "·"
            },
            summary.failure_count,
            summary.interest_reasons.join(", "),
            input_short.replace("|", "\\|")
        ));
    }
    lines.push(String::new());

    let interesting: Vec<_> = summaries
        .iter()
        .filter(|summary| summary.is_interesting)
        .collect();
    if !interesting.is_empty() {
        lines.push("## Interesting Traces Deep Dive".to_string());
        lines.push(String::new());
        for summary in &interesting {
            lines.push(format!("### `{}`", summary.run_id));
            lines.push(String::new());
            lines.push(format!("- **user_input**: {}", summary.user_input));
            lines.push(format!("- **success**: {}", summary.success));
            lines.push(format!("- **in_baseline**: {}", summary.in_baseline));
            if let Some(ref error_short) = summary.error_short {
                lines.push(format!("- **error**: {}", error_short));
            }
            lines.push(format!(
                "- **duration**: {}ms, **steps**: {}",
                summary
                    .duration_ms
                    .map(|duration| duration.to_string())
                    .unwrap_or_else(|| "N/A".to_string()),
                summary.step_count
            ));
            lines.push(format!(
                "- **memory**: retrieved={}, injected={}, dropped={}, total_chars={}",
                summary.memory_retrieved,
                summary.memory_injected,
                summary.memory_dropped,
                summary.memory_total_chars
            ));
            lines.push(format!("- **session_state**: {}", summary.state_present));
            lines.push(format!(
                "- **persistent_state_updated**: {}",
                summary.persistent_state_updated
            ));
            lines.push(format!(
                "- **context_pack**: dropped={}, reasons={:?}",
                summary.context_pack_dropped, summary.context_pack_drop_reasons
            ));
            lines.push(format!(
                "- **llm_calls**: total={}, success={}, failed={}",
                summary.llm_call_count, summary.llm_success_count, summary.llm_failure_count
            ));
            lines.push(format!(
                "- **tool_calls**: total={}, success={}",
                summary.tool_call_count, summary.tool_success_count
            ));
            lines.push(format!(
                "- **recovery**: attempts={}, success={}, actions={:?}, results={:?}",
                summary.recovery_attempt_count,
                summary.recovery_success_count,
                summary.recovery_actions,
                summary.recovery_results
            ));
            if !summary.failure_types.is_empty() {
                lines.push(format!(
                    "- **failure_types**: {}",
                    summary.failure_types.join(", ")
                ));
            }
            lines.push(format!(
                "- **interest_reasons**: {}",
                summary.interest_reasons.join(", ")
            ));
            lines.push(String::new());
        }
    }

    lines.push("## Failure Taxonomy Annotation Template".to_string());
    lines.push(String::new());
    lines.push("对以上 interesting traces 进行人工评审时，可按下表标注：".to_string());
    lines.push(String::new());
    lines.push("| run_id | primary_failure | severity | notes |".to_string());
    lines.push("| --- | --- | --- | --- |".to_string());
    for summary in &interesting {
        lines.push(format!(
            "| `{}` | (待填) | (low/mid/high) | (待填) |",
            &summary.run_id[..8.min(summary.run_id.len())]
        ));
    }
    lines.push(String::new());

    lines.push("### Failure Taxonomy".to_string());
    lines.push(String::new());
    lines.push("- `llm_auth_error`: 模型调用鉴权失败（如 401）".to_string());
    lines.push("- `llm_transport_error`: 模型调用网络/传输失败（超时、连接失败等）".to_string());
    lines.push("- `tool_call_error`: 工具调用失败（路径、参数、执行错误）".to_string());
    lines.push("- `context_overtrim`: 上下文裁剪过度导致关键信息缺失".to_string());
    lines.push("- `memory_conflict`: 记忆冲突/降级导致信息不一致".to_string());
    lines.push("- `session_state_missing_or_stale`: SessionState 缺失、过期或不一致".to_string());
    lines.push("- `planning_stall_or_drift`: 规划循环停滞、重规划过多、轨迹漂移".to_string());
    lines.push("- `done_rule_validation_fail`: 工具成功但收敛判定失败".to_string());
    lines.push("- `fallback_exhausted`: 主链路失败后 fallback 仍未收敛".to_string());
    lines.push("- `unknown_failure`: 未命中以上分类的失败".to_string());
    lines.push(String::new());

    lines.push("---".to_string());
    lines.push("*本报告由 trace_eval 自动生成，人工评审后请将标注结果补充到上表中。*".to_string());
    lines.push(String::new());

    lines.join("\n")
}

fn display_path_or_na(path: Option<&Path>) -> String {
    path.map(|p| p.display().to_string())
        .unwrap_or_else(|| "N/A".to_string())
}

fn pct(part: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        (part as f64 / total as f64) * 100.0
    }
}

/// 单侧 99% 置信界使用的 z 值。
const WILSON_Z_99: f64 = 2.326;

/// 单侧 Wilson score 置信界（返回比例，0..1）。
/// HigherIsBetter 指标取下界，LowerIsBetter 指标取上界；total=0 时返回 None。
fn wilson_bound_99(hits: usize, total: usize, direction: Direction) -> Option<f64> {
    if total == 0 {
        return None;
    }
    let n = total as f64;
    let p = hits as f64 / n;
    let z2 = WILSON_Z_99 * WILSON_Z_99;
    let denom = 1.0 + z2 / n;
    let center = (p + z2 / (2.0 * n)) / denom;
    let half = (WILSON_Z_99 / denom) * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt();
    match direction {
        Direction::HigherIsBetter => Some((center - half).max(0.0)),
        Direction::LowerIsBetter => Some((center + half).min(1.0)),
    }
}

/// 比例单元格文本：`95.0% (38/40), Wilson99 下界 80.4%`。
/// total=0 时省略置信界，仅输出 `0.0% (0/0)`。
fn ratio_cell(hits: usize, total: usize, direction: Direction) -> String {
    let base = format!("{:.1}% ({}/{})", pct(hits, total), hits, total);
    match wilson_bound_99(hits, total, direction) {
        Some(bound) => {
            let label = match direction {
                Direction::HigherIsBetter => "下界",
                Direction::LowerIsBetter => "上界",
            };
            format!("{}, Wilson99 {} {:.1}%", base, label, bound * 100.0)
        }
        None => base,
    }
}

fn fmt_optional_rate(rate: Option<f64>) -> String {
    match rate {
        Some(v) => format!("{:.1}%", v),
        None => "N/A".to_string(),
    }
}

fn fmt_optional_rate_delta(before: Option<f64>, after: Option<f64>) -> String {
    match (before, after) {
        (Some(b), Some(a)) => format!("{:+.1}pp", a - b),
        _ => "N/A".to_string(),
    }
}

fn build_state_updated_gate_lines(
    before_count: usize,
    after_count: usize,
    before_rate: Option<f64>,
    after_rate: Option<f64>,
) -> (String, String) {
    let human_readable = format!(
        "STATE_UPDATED=before_count={} after_count={} before_rate={} after_rate={} delta={}",
        before_count,
        after_count,
        fmt_optional_rate(before_rate),
        fmt_optional_rate(after_rate),
        fmt_optional_rate_delta(before_rate, after_rate)
    );
    let machine = format!(
        "STATE_UPDATED_RAW=bc={}|ac={}|br={}|ar={}|d={}",
        before_count,
        after_count,
        before_rate
            .map(|v| format!("{:.1}", v))
            .unwrap_or_else(|| "NA".to_string()),
        after_rate
            .map(|v| format!("{:.1}", v))
            .unwrap_or_else(|| "NA".to_string()),
        match (before_rate, after_rate) {
            (Some(b), Some(a)) => format!("{:.1}", a - b),
            _ => "NA".to_string(),
        }
    );
    (human_readable, machine)
}

fn build_gate_output_lines(
    overall: Verdict,
    reasons: &[String],
    before: &CompareMetrics,
    after: &CompareMetrics,
) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!("OVERALL={}", verdict_str(overall)));
    let (state_updated_human, state_updated_machine) = build_state_updated_gate_lines(
        before.persistent_state_updated_count,
        after.persistent_state_updated_count,
        before.persistent_state_updated_rate,
        after.persistent_state_updated_rate,
    );
    lines.push(state_updated_human);
    lines.push(state_updated_machine);
    if !reasons.is_empty() {
        lines.push(format!("REASONS={}", reasons.join("; ")));
    }
    lines
}

#[derive(Debug, Clone, Serialize)]
struct GateJsonOutput {
    overall: String,
    reasons: Vec<String>,
    state_updated: GateJsonStateUpdated,
}

#[derive(Debug, Clone, Serialize)]
struct GateJsonStateUpdated {
    before_count: usize,
    after_count: usize,
    before_rate: Option<f64>,
    after_rate: Option<f64>,
    delta: Option<f64>,
}

fn build_gate_json_output(
    overall: Verdict,
    reasons: &[String],
    before: &CompareMetrics,
    after: &CompareMetrics,
) -> GateJsonOutput {
    let delta = match (
        before.persistent_state_updated_rate,
        after.persistent_state_updated_rate,
    ) {
        (Some(b), Some(a)) => Some(a - b),
        _ => None,
    };
    GateJsonOutput {
        overall: verdict_str(overall).to_string(),
        reasons: reasons.to_vec(),
        state_updated: GateJsonStateUpdated {
            before_count: before.persistent_state_updated_count,
            after_count: after.persistent_state_updated_count,
            before_rate: before.persistent_state_updated_rate,
            after_rate: after.persistent_state_updated_rate,
            delta,
        },
    }
}

/// 计算 latency 的 p50/p95（毫秒）。
/// 输入为空时返回 (0, 0)。
fn latency_quantiles(values: &[u128]) -> (u128, u128) {
    if values.is_empty() {
        return (0, 0);
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    let p50_idx = ((n as f64 * 0.5).ceil() as usize).saturating_sub(1);
    let p95_idx = ((n as f64 * 0.95).ceil() as usize).saturating_sub(1);
    let p50 = sorted[p50_idx.min(n - 1)];
    let p95 = sorted[p95_idx.min(n - 1)];
    (p50, p95)
}

// =============================================================================
// COMPARE MODE
// =============================================================================

#[derive(Debug, Clone, Default)]
struct CompareMetrics {
    // Core 7
    success_rate: Option<f64>,
    fallback_rate: Option<f64>,
    context_drop_rate: Option<f64>,
    state_present_rate: Option<f64>,
    memory_injected_rate: Option<f64>,
    recovery_success_rate: Option<f64>,
    unknown_failure_rate: Option<f64>,
    // L2 3
    tool_success_rate: Option<f64>,
    planning_stall_rate: Option<f64>,
    avg_step_count: Option<f64>,
    // Auxiliary
    total_runs: usize,
    baseline_run_ids: usize,
    baseline_hits: usize,
    // Persistent state update (observability, not gate)
    persistent_state_updated_count: usize,
    persistent_state_updated_rate: Option<f64>,
    // Trace schema health（observability, not gate；gate 判定路径不消费这两个字段）
    unsupported_version_count: usize,
    missing_core_field_count: usize,
    // Denominator info (for scenario B)
    recovery_attempt_count: usize,
    total_tool_calls: usize,
    // 各比例指标的 (分子, 分母)，从报告单元格 "(n/d)" 反解析；仅作观测信息，
    // gate 判定路径不消费这些字段
    rate_samples: HashMap<String, (usize, usize)>,
    // Failure type counts
    failure_type_counts: HashMap<String, usize>,
    // Missing fields
    missing_fields: Vec<String>,
    // Report date
    report_date: String,
}

impl CompareMetrics {
    fn get(&self, name: &str) -> f64 {
        match name {
            "success_rate" => self.success_rate.unwrap_or(0.0),
            "fallback_rate" => self.fallback_rate.unwrap_or(0.0),
            "context_drop_rate" => self.context_drop_rate.unwrap_or(0.0),
            "state_present_rate" => self.state_present_rate.unwrap_or(0.0),
            "memory_injected_rate" => self.memory_injected_rate.unwrap_or(0.0),
            "recovery_success_rate" => self.recovery_success_rate.unwrap_or(0.0),
            "unknown_failure_rate" => self.unknown_failure_rate.unwrap_or(0.0),
            "tool_success_rate" => self.tool_success_rate.unwrap_or(0.0),
            "planning_stall_rate" => self.planning_stall_rate.unwrap_or(0.0),
            "avg_step_count" => self.avg_step_count.unwrap_or(0.0),
            _ => 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Pass,
    Warn,
    Fail,
    Na,
}

#[derive(Debug, Clone, Copy)]
enum Direction {
    HigherIsBetter,
    LowerIsBetter,
}

struct MetricRule {
    name: &'static str,
    direction: Direction,
    warn_delta: f64,
    fail_delta: f64,
    absolute_fail: Option<fn(f64) -> bool>,
}

struct SingleVerdict {
    metric: &'static str,
    before: f64,
    after: f64,
    delta: f64,
    verdict: Verdict,
    note: String,
}

// -------------------------------------------------------------------------
// Report parsing
// -------------------------------------------------------------------------

fn parse_report(content: &str) -> Result<CompareMetrics, String> {
    let mut metrics = CompareMetrics::default();
    let mut missing = Vec::new();

    // Strict validation: must be a valid report file
    if !content.contains("# Trace Evaluation Report") {
        return Err(
            "报告缺少标题 '# Trace Evaluation Report'，可能不是有效的 trace_eval 报告".to_string(),
        );
    }

    // Extract header info
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("- generated:") {
            if let Some(date_part) = line.split('T').next() {
                metrics.report_date = date_part
                    .trim_start_matches("- generated:")
                    .trim()
                    .to_string();
            }
        } else if line.starts_with("- total traces:") {
            metrics.total_runs = extract_usize_after_colon(line);
        } else if line.starts_with("- baseline_run_ids:") {
            metrics.baseline_run_ids = extract_usize_after_colon(line);
        }
    }

    // Parse by sections
    let sections = split_into_sections(content);

    // Strict validation: must have Summary Statistics
    let has_summary = sections
        .iter()
        .any(|(title, _)| title == "Summary Statistics");
    if !has_summary {
        return Err("报告中未找到 '## Summary Statistics' 部分，无法提取核心指标".to_string());
    }
    for (title, body) in sections {
        match title.as_str() {
            "Summary Statistics" => {
                let rows = parse_markdown_table(&body);
                for row in rows {
                    if row.len() < 3 {
                        continue;
                    }
                    let name = row[0].trim();
                    let ratio = parse_percentage(&row[2]);
                    match name {
                        "success" => {
                            metrics.success_rate = ratio;
                            record_rate_sample(&mut metrics, "success_rate", &row[2]);
                        }
                        "with llm fallback" => {
                            metrics.fallback_rate = ratio;
                            record_rate_sample(&mut metrics, "fallback_rate", &row[2]);
                        }
                        "with context pack dropped" => {
                            metrics.context_drop_rate = ratio;
                            record_rate_sample(&mut metrics, "context_drop_rate", &row[2]);
                        }
                        "with session state" => {
                            metrics.state_present_rate = ratio;
                            record_rate_sample(&mut metrics, "state_present_rate", &row[2]);
                        }
                        "with memory injected" => {
                            metrics.memory_injected_rate = ratio;
                            record_rate_sample(&mut metrics, "memory_injected_rate", &row[2]);
                        }
                        "with persistent state updated" => {
                            metrics.persistent_state_updated_rate = ratio;
                            metrics.persistent_state_updated_count =
                                extract_usize_from_cell(&row[1]);
                        }
                        _ => {}
                    }
                }
            }
            "Baseline Coverage" => {
                let rows = parse_markdown_table(&body);
                for row in rows {
                    if row.len() < 3 {
                        continue;
                    }
                    let name = row[0].trim();
                    if name == "baseline hits in current trace set" {
                        metrics.baseline_hits = extract_usize_from_cell(&row[1]);
                    }
                }
            }
            "Failure Type Distribution" => {
                let rows = parse_markdown_table(&body);
                for row in rows {
                    if row.len() < 2 {
                        continue;
                    }
                    let name = row[0].trim();
                    if name == "(none)" {
                        continue;
                    }
                    let count = extract_usize_from_cell(&row[1]);
                    metrics.failure_type_counts.insert(name.to_string(), count);
                    if name == "unknown_failure" && row.len() >= 3 {
                        record_rate_sample(&mut metrics, "unknown_failure_rate", &row[2]);
                    }
                }
            }
            "Tool Use Statistics" => {
                let rows = parse_markdown_table(&body);
                for row in rows {
                    if row.len() < 3 {
                        continue;
                    }
                    let name = row[0].trim();
                    if name == "tool success" {
                        metrics.tool_success_rate = parse_percentage(&row[2]);
                        record_rate_sample(&mut metrics, "tool_success_rate", &row[2]);
                    } else if name == "total tool calls" {
                        metrics.total_tool_calls = extract_usize_from_cell(&row[1]);
                    }
                }
            }
            "Planning / ReAct Statistics" => {
                let rows = parse_markdown_table(&body);
                for row in rows {
                    if row.len() < 2 {
                        continue;
                    }
                    let name = row[0].trim();
                    if name == "step_count min / max / avg" {
                        // Format: "1 / 12 / 3.2"
                        let value_cell = row[1].trim();
                        if let Some(avg_str) = value_cell.split('/').nth(2) {
                            metrics.avg_step_count = avg_str.trim().parse().ok();
                        }
                    } else if name == "stall_or_drift hits" {
                        let count = extract_usize_from_cell(&row[1]);
                        if metrics.total_runs > 0 {
                            metrics.planning_stall_rate =
                                Some(count as f64 / metrics.total_runs as f64 * 100.0);
                        }
                        if row.len() >= 3 {
                            record_rate_sample(&mut metrics, "planning_stall_rate", &row[2]);
                        }
                    }
                }
            }
            "Recovery Statistics" => {
                let rows = parse_markdown_table(&body);
                for row in rows {
                    if row.len() < 3 {
                        continue;
                    }
                    let name = row[0].trim();
                    if name == "recovery_success" {
                        metrics.recovery_success_rate = parse_percentage(&row[2]);
                        record_rate_sample(&mut metrics, "recovery_success_rate", &row[2]);
                    } else if name == "recovery_attempt_count" {
                        metrics.recovery_attempt_count = extract_usize_from_cell(&row[1]);
                    }
                }
            }
            // 旧报告没有该小节：跳过即为 0，不影响既有指标解析
            "Trace Schema Health" => {
                let rows = parse_markdown_table(&body);
                for row in rows {
                    if row.len() < 2 {
                        continue;
                    }
                    let name = row[0].trim();
                    if name == "unsupported_version_count" {
                        metrics.unsupported_version_count = extract_usize_from_cell(&row[1]);
                    } else if name.starts_with("missing_field:") {
                        metrics.missing_core_field_count += extract_usize_from_cell(&row[1]);
                    }
                }
            }
            _ => {}
        }
    }

    // Compute derived metrics
    if metrics.total_runs > 0 {
        let unknown_count = metrics
            .failure_type_counts
            .get("unknown_failure")
            .copied()
            .unwrap_or(0);
        metrics.unknown_failure_rate =
            Some(unknown_count as f64 / metrics.total_runs as f64 * 100.0);
    } else {
        metrics.unknown_failure_rate = Some(0.0);
    }

    // Check missing core metrics
    if metrics.success_rate.is_none() {
        missing.push("success_rate".to_string());
    }
    if metrics.fallback_rate.is_none() {
        missing.push("fallback_rate".to_string());
    }
    if metrics.context_drop_rate.is_none() {
        missing.push("context_drop_rate".to_string());
    }
    if metrics.state_present_rate.is_none() {
        missing.push("state_present_rate".to_string());
    }
    if metrics.memory_injected_rate.is_none() {
        missing.push("memory_injected_rate".to_string());
    }
    // recovery_success_rate can be missing if no recovery attempts exist
    if metrics.recovery_success_rate.is_none() && metrics.recovery_attempt_count > 0 {
        missing.push("recovery_success_rate".to_string());
    }
    // unknown_failure_rate is always computed, never missing

    // Strict validation: total_runs must be > 0
    if metrics.total_runs == 0 {
        return Err("报告中 total_traces=0，无法进行对比".to_string());
    }

    // Strict validation: success_rate must be extractable (key sanity check)
    if metrics.success_rate.is_none() {
        return Err("无法从报告中提取 success_rate，可能不是有效的 trace_eval 报告".to_string());
    }

    metrics.missing_fields = missing;
    Ok(metrics)
}

fn split_into_sections(content: &str) -> Vec<(String, String)> {
    let mut sections = Vec::new();
    let mut current_title = String::new();
    let mut current_body = String::new();

    for line in content.lines() {
        if line.starts_with("## ") {
            if !current_title.is_empty() {
                sections.push((current_title, current_body));
            }
            current_title = line.trim_start_matches("## ").trim().to_string();
            current_body = String::new();
        } else if !current_title.is_empty() {
            current_body.push_str(line);
            current_body.push('\n');
        }
    }

    if !current_title.is_empty() {
        sections.push((current_title, current_body));
    }

    sections
}

fn parse_markdown_table(body: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if !line.starts_with('|') || !line.ends_with('|') {
            continue;
        }
        // Skip separator rows: all non-empty cells start with '-'
        let non_empty: Vec<&str> = line.split('|').filter(|s| !s.trim().is_empty()).collect();
        if non_empty.iter().all(|p| p.trim().starts_with('-')) {
            continue;
        }
        let cells: Vec<String> = line
            .split('|')
            .skip(1)
            .map(|s| s.trim().to_string())
            .take_while(|s| !s.is_empty())
            .collect();
        if !cells.is_empty() {
            rows.push(cells);
        }
    }
    rows
}

fn extract_usize_after_colon(line: &str) -> usize {
    line.split(':')
        .next_back()
        .unwrap_or("0")
        .trim()
        .parse()
        .unwrap_or(0)
}

fn parse_percentage(s: &str) -> Option<f64> {
    let s = s.trim();
    // 兼容新格式 "12.5% (5/40), Wilson99 上界 24.1%"：取第一个 '%' 之前的数值
    let s = match s.find('%') {
        Some(idx) => s[..idx].trim(),
        None => s,
    };
    s.parse().ok()
}

/// 从比例单元格解析 `(n/d)` 分母信息；旧格式无括号时返回 None。
fn parse_count_total(s: &str) -> Option<(usize, usize)> {
    let start = s.find('(')?;
    let end = s[start..].find(')')? + start;
    let inner = &s[start + 1..end];
    let mut parts = inner.split('/');
    let n = parts.next()?.trim().parse().ok()?;
    let d = parts.next()?.trim().parse().ok()?;
    Some((n, d))
}

/// 单元格含 "(n/d)" 时记录到 rate_samples；解析不出（旧格式）则不记录，走现有逻辑。
fn record_rate_sample(metrics: &mut CompareMetrics, key: &str, cell: &str) {
    if let Some(sample) = parse_count_total(cell) {
        metrics.rate_samples.insert(key.to_string(), sample);
    }
}

fn extract_usize_from_cell(s: &str) -> usize {
    s.trim().parse().unwrap_or(0)
}

// -------------------------------------------------------------------------
// JSON sidecar（报告结构化伴生文件）
// -------------------------------------------------------------------------

/// sidecar schema 版本号；compare 只消费认识的版本，否则回退 markdown 解析。
const REPORT_SIDECAR_SCHEMA_VERSION: &str = "trace_eval_report_v1";

/// 单个比例指标：rate 为百分比（0..100），与 markdown 单元格 `{:.1}%` 同一舍入；
/// wilson99_bound 为单侧 99% 置信界（HigherIsBetter 指标为下界，LowerIsBetter 指标为上界；
/// total=0 时为 None）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct RateMetric {
    rate: f64,
    hits: usize,
    total: usize,
    wilson99_bound: Option<f64>,
}

/// 报告 JSON sidecar：与同名 .md 报告配套落盘，供 compare 结构化消费。
/// 比例指标以现有指标名为 key（success_rate 等）；均值类指标（avg_step_count）只有值。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReportSidecar {
    schema_version: String,
    report_date: String,
    total_runs: usize,
    baseline_run_ids: usize,
    baseline_hits: usize,
    recovery_attempt_count: usize,
    total_tool_calls: usize,
    avg_step_count: f64,
    failure_type_counts: BTreeMap<String, usize>,
    rates: BTreeMap<String, RateMetric>,
    // schema 健康度（新增字段带 serde default：旧 sidecar 缺字段时按 0 兼容，不触发回退）
    #[serde(default)]
    unsupported_version_count: usize,
    #[serde(default)]
    missing_core_field_counts: BTreeMap<String, usize>,
}

/// 报告路径对应的 JSON sidecar 路径（`REPORT.md` -> `REPORT.json`）。
fn sidecar_path(report_path: &Path) -> PathBuf {
    report_path.with_extension("json")
}

/// 报告比例指标的方向（与 compare 规则一致）：HigherIsBetter 取 Wilson 下界。
fn rate_metric_direction(name: &str) -> Direction {
    match name {
        "success_rate"
        | "state_present_rate"
        | "memory_injected_rate"
        | "recovery_success_rate"
        | "tool_success_rate"
        | "persistent_state_updated_rate" => Direction::HigherIsBetter,
        _ => Direction::LowerIsBetter,
    }
}

fn insert_rate_metric(
    rates: &mut BTreeMap<String, RateMetric>,
    name: &str,
    hits: usize,
    total: usize,
) {
    let bound = wilson_bound_99(hits, total, rate_metric_direction(name)).map(|b| b * 100.0);
    // rate 与 markdown 单元格的 `{:.1}%` 保持同一舍入结果，保证混合 compare
    // （一侧 sidecar、一侧旧 markdown 报告）时数值完全一致，不翻转判定
    let exact = pct(hits, total);
    let rate = format!("{:.1}", exact).parse::<f64>().unwrap_or(exact);
    rates.insert(
        name.to_string(),
        RateMetric {
            rate,
            hits,
            total,
            wilson99_bound: bound,
        },
    );
}

/// 从 summaries 构建报告 JSON sidecar（指标口径与 build_report 的 markdown 单元格一致）。
fn build_report_sidecar(
    summaries: &[TraceSummary],
    baseline_run_ids: &HashSet<String>,
    load_stats: &TraceLoadStats,
) -> ReportSidecar {
    let total = summaries.len();
    let mut rates = BTreeMap::new();

    insert_rate_metric(
        &mut rates,
        "success_rate",
        summaries.iter().filter(|s| s.success).count(),
        total,
    );
    insert_rate_metric(
        &mut rates,
        "fallback_rate",
        summaries.iter().filter(|s| s.llm_fallback).count(),
        total,
    );
    insert_rate_metric(
        &mut rates,
        "context_drop_rate",
        summaries.iter().filter(|s| s.context_pack_dropped).count(),
        total,
    );
    insert_rate_metric(
        &mut rates,
        "state_present_rate",
        summaries.iter().filter(|s| s.state_present).count(),
        total,
    );
    insert_rate_metric(
        &mut rates,
        "memory_injected_rate",
        summaries.iter().filter(|s| s.memory_injected > 0).count(),
        total,
    );
    insert_rate_metric(
        &mut rates,
        "persistent_state_updated_rate",
        summaries
            .iter()
            .filter(|s| s.persistent_state_updated)
            .count(),
        total,
    );

    let total_tool_calls: usize = summaries.iter().map(|s| s.tool_call_count).sum();
    let total_tool_success: usize = summaries.iter().map(|s| s.tool_success_count).sum();
    insert_rate_metric(
        &mut rates,
        "tool_success_rate",
        total_tool_success,
        total_tool_calls,
    );

    let stall_drift_count = summaries
        .iter()
        .filter(|s| {
            s.failure_types
                .iter()
                .any(|ft| ft == "planning_stall_or_drift")
        })
        .count();
    insert_rate_metric(&mut rates, "planning_stall_rate", stall_drift_count, total);

    let recovery_attempt_count: usize = summaries.iter().map(|s| s.recovery_attempt_count).sum();
    let recovery_successes: usize = summaries.iter().map(|s| s.recovery_success_count).sum();
    insert_rate_metric(
        &mut rates,
        "recovery_success_rate",
        recovery_successes,
        recovery_attempt_count,
    );

    let mut failure_type_counts: BTreeMap<String, usize> = BTreeMap::new();
    for summary in summaries {
        for failure_type in &summary.failure_types {
            *failure_type_counts.entry(failure_type.clone()).or_insert(0) += 1;
        }
    }
    let unknown_count = failure_type_counts
        .get("unknown_failure")
        .copied()
        .unwrap_or(0);
    insert_rate_metric(&mut rates, "unknown_failure_rate", unknown_count, total);

    let avg_step_count = if total > 0 {
        summaries.iter().map(|s| s.step_count).sum::<usize>() as f64 / total as f64
    } else {
        0.0
    };

    ReportSidecar {
        schema_version: REPORT_SIDECAR_SCHEMA_VERSION.to_string(),
        report_date: chrono::Utc::now().format("%Y-%m-%d").to_string(),
        total_runs: total,
        baseline_run_ids: baseline_run_ids.len(),
        baseline_hits: summaries.iter().filter(|s| s.in_baseline).count(),
        recovery_attempt_count,
        total_tool_calls,
        avg_step_count,
        failure_type_counts,
        rates,
        unsupported_version_count: load_stats.unsupported_version_count,
        missing_core_field_counts: load_stats.missing_core_field_counts.clone(),
    }
}

/// 从 JSON sidecar 构建 CompareMetrics；schema 版本不认识或 JSON 损坏时返回 Err，
/// 由调用方回退到 markdown 文本解析。
fn parse_sidecar(content: &str) -> Result<CompareMetrics, String> {
    let sidecar: ReportSidecar =
        serde_json::from_str(content).map_err(|e| format!("JSON 解析失败: {}", e))?;
    if sidecar.schema_version != REPORT_SIDECAR_SCHEMA_VERSION {
        return Err(format!(
            "不认识的 schema_version: {}",
            sidecar.schema_version
        ));
    }
    Ok(compare_metrics_from_sidecar(&sidecar))
}

fn compare_metrics_from_sidecar(sidecar: &ReportSidecar) -> CompareMetrics {
    let mut metrics = CompareMetrics {
        report_date: sidecar.report_date.clone(),
        total_runs: sidecar.total_runs,
        baseline_run_ids: sidecar.baseline_run_ids,
        baseline_hits: sidecar.baseline_hits,
        recovery_attempt_count: sidecar.recovery_attempt_count,
        total_tool_calls: sidecar.total_tool_calls,
        avg_step_count: Some(sidecar.avg_step_count),
        failure_type_counts: sidecar
            .failure_type_counts
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect(),
        unsupported_version_count: sidecar.unsupported_version_count,
        missing_core_field_count: sidecar.missing_core_field_counts.values().sum(),
        ..Default::default()
    };
    for (name, sample) in &sidecar.rates {
        let rate = Some(sample.rate);
        match name.as_str() {
            "success_rate" => metrics.success_rate = rate,
            "fallback_rate" => metrics.fallback_rate = rate,
            "context_drop_rate" => metrics.context_drop_rate = rate,
            "state_present_rate" => metrics.state_present_rate = rate,
            "memory_injected_rate" => metrics.memory_injected_rate = rate,
            "recovery_success_rate" => metrics.recovery_success_rate = rate,
            "unknown_failure_rate" => metrics.unknown_failure_rate = rate,
            "tool_success_rate" => metrics.tool_success_rate = rate,
            "planning_stall_rate" => metrics.planning_stall_rate = rate,
            "persistent_state_updated_rate" => {
                metrics.persistent_state_updated_rate = rate;
                metrics.persistent_state_updated_count = sample.hits;
            }
            // 未知指标（可能来自更新版本）：忽略，保持向前兼容
            _ => continue,
        }
        metrics
            .rate_samples
            .insert(name.clone(), (sample.hits, sample.total));
    }
    metrics
}

/// 加载 compare 输入：优先消费同名 `.json` sidecar（schema 认识时）；
/// sidecar 缺失、损坏或版本不认识时回退到现有 markdown 文本解析，行为与之前一致。
fn load_compare_metrics(report_path: &Path) -> Result<CompareMetrics, String> {
    let sidecar_file = sidecar_path(report_path);
    if let Ok(content) = fs::read_to_string(&sidecar_file) {
        match parse_sidecar(&content) {
            Ok(metrics) => return Ok(metrics),
            Err(reason) => eprintln!(
                "sidecar {} 不可用（{}），回退 markdown 解析",
                sidecar_file.display(),
                reason
            ),
        }
    }
    let content = fs::read_to_string(report_path)
        .map_err(|e| format!("读取报告失败 {}: {}", report_path.display(), e))?;
    parse_report(&content)
}

// -------------------------------------------------------------------------
// Single metric evaluation
// -------------------------------------------------------------------------

fn evaluate_single(
    metric_name: &'static str,
    before: f64,
    after: f64,
    rule: &MetricRule,
    relaxed: bool,
) -> SingleVerdict {
    let mut warn_threshold = rule.warn_delta;
    let mut fail_threshold = rule.fail_delta;

    if relaxed {
        warn_threshold += 2.0;
        fail_threshold += 2.0;
    }

    // Check improvement or no change first (PASS column priority)
    let improved_or_same = match rule.direction {
        Direction::HigherIsBetter => after >= before,
        Direction::LowerIsBetter => after <= before,
    };

    if improved_or_same {
        return SingleVerdict {
            metric: metric_name,
            before,
            after,
            delta: after - before,
            verdict: Verdict::Pass,
            note: "无退化".to_string(),
        };
    }

    // Check absolute fail condition only when degraded
    if let Some(check) = rule.absolute_fail {
        if check(after) {
            return SingleVerdict {
                metric: metric_name,
                before,
                after,
                delta: after - before,
                verdict: Verdict::Fail,
                note: format!("绝对值触发 FAIL (after={:.1})", after),
            };
        }
    }

    // Calculate degradation
    let degrade = match rule.direction {
        Direction::HigherIsBetter => before - after,
        Direction::LowerIsBetter => after - before,
    };

    let verdict = if degrade > fail_threshold {
        Verdict::Fail
    } else if degrade >= warn_threshold {
        Verdict::Warn
    } else {
        Verdict::Pass
    };

    let unit = if metric_name == "avg_step_count" {
        "步"
    } else {
        "pp"
    };
    let note = match verdict {
        Verdict::Pass => "无退化".to_string(),
        Verdict::Warn => format!("退化 {:.1}{}", degrade.abs(), unit),
        Verdict::Fail => format!("明显退化 {:.1}{}", degrade.abs(), unit),
        Verdict::Na => "N/A".to_string(),
    };

    SingleVerdict {
        metric: metric_name,
        before,
        after,
        delta: after - before,
        verdict,
        note,
    }
}

fn has_valid_denominator(metric_name: &str, metrics: &CompareMetrics) -> bool {
    match metric_name {
        "recovery_success_rate" => metrics.recovery_attempt_count > 0,
        "tool_success_rate" => metrics.total_tool_calls > 0,
        _ => true,
    }
}

fn build_na_verdict(
    metric_name: &'static str,
    before: &CompareMetrics,
    after: &CompareMetrics,
) -> SingleVerdict {
    let before_valid = has_valid_denominator(metric_name, before);
    let after_valid = has_valid_denominator(metric_name, after);
    let note = match (before_valid, after_valid) {
        (false, false) => "N/A（分母为 0，不纳入综合判定）",
        (false, true) => "N/A -> 有数据（分母为 0，不纳入综合判定）",
        (true, false) => "有数据 -> N/A（分母为 0，不纳入综合判定）",
        _ => unreachable!(),
    };
    SingleVerdict {
        metric: metric_name,
        before: before.get(metric_name),
        after: after.get(metric_name),
        delta: after.get(metric_name) - before.get(metric_name),
        verdict: Verdict::Na,
        note: note.to_string(),
    }
}

// -------------------------------------------------------------------------
// Overall verdict
// -------------------------------------------------------------------------

fn compute_overall(
    core_verdicts: &[SingleVerdict],
    _l2_verdicts: &[SingleVerdict],
    before: &CompareMetrics,
    after: &CompareMetrics,
) -> (Verdict, Vec<String>) {
    let mut reasons = Vec::new();

    // Count core verdicts
    let fail_count = core_verdicts
        .iter()
        .filter(|v| v.verdict == Verdict::Fail)
        .count();
    let warn_count = core_verdicts
        .iter()
        .filter(|v| v.verdict == Verdict::Warn)
        .count();
    let na_count = core_verdicts
        .iter()
        .filter(|v| v.verdict == Verdict::Na)
        .count();

    // Base verdict
    let mut overall = if fail_count > 0 {
        reasons.push(format!("核心指标中存在 {} 项 FAIL", fail_count));
        Verdict::Fail
    } else if warn_count > 0 {
        reasons.push(format!("核心指标中存在 {} 项 WARN", warn_count));
        Verdict::Warn
    } else {
        reasons.push("全部核心指标 PASS".to_string());
        Verdict::Pass
    };

    // N/A rule: 超过 2 个 N/A 时总体最多 WARN
    if na_count > 2 {
        reasons.push(format!(
            "核心指标中 {} 项 N/A（>2），综合结论封顶 WARN",
            na_count
        ));
        overall = cap_at_warn(overall);
    }

    // Hard thresholds (additional safety check)
    let unknown_rate = after.unknown_failure_rate.unwrap_or(0.0);
    if unknown_rate > 10.0 {
        reasons.push(format!(
            "unknown_failure_rate={:.1}% > 10%，触发硬门槛",
            unknown_rate
        ));
        overall = Verdict::Fail;
    }

    let success_delta = after.success_rate.unwrap_or(0.0) - before.success_rate.unwrap_or(0.0);
    if success_delta < -5.0 {
        reasons.push(format!(
            "success_rate 降幅={:.1}pp > 5pp，触发硬门槛",
            -success_delta
        ));
        overall = Verdict::Fail;
    }

    // Scenario C: baseline coverage drop > 20pp
    let before_baseline_rate = if before.baseline_run_ids > 0 {
        before.baseline_hits as f64 / before.baseline_run_ids as f64 * 100.0
    } else {
        0.0
    };
    let after_baseline_rate = if after.baseline_run_ids > 0 {
        after.baseline_hits as f64 / after.baseline_run_ids as f64 * 100.0
    } else {
        0.0
    };
    let baseline_drop = before_baseline_rate - after_baseline_rate;

    if baseline_drop > 20.0 {
        reasons.push(format!(
            "baseline 覆盖率下降 {:.1}pp > 20pp，综合结论降级一档",
            baseline_drop
        ));
        overall = downgrade(overall);
    }

    // Sample protection: after.total_runs < 20 => 总体最多 WARN
    if after.total_runs < 20 {
        reasons.push(format!(
            "after 样本量 {} < 20，综合结论封顶 WARN",
            after.total_runs
        ));
        overall = cap_at_warn(overall);
    }

    (overall, reasons)
}

fn downgrade(v: Verdict) -> Verdict {
    match v {
        Verdict::Pass => Verdict::Warn,
        Verdict::Warn => Verdict::Fail,
        Verdict::Fail => Verdict::Fail,
        Verdict::Na => Verdict::Na,
    }
}

/// 封顶为 WARN（不升级到 FAIL，不降级到 PASS）。
fn cap_at_warn(v: Verdict) -> Verdict {
    match v {
        Verdict::Pass => Verdict::Warn,
        other => other,
    }
}

// -------------------------------------------------------------------------
// Report generation
// -------------------------------------------------------------------------

fn verdict_str(v: Verdict) -> &'static str {
    match v {
        Verdict::Pass => "PASS",
        Verdict::Warn => "WARN",
        Verdict::Fail => "FAIL",
        Verdict::Na => "N/A",
    }
}

fn verdict_emoji(v: Verdict) -> &'static str {
    match v {
        Verdict::Pass => "PASS",
        Verdict::Warn => "WARN",
        Verdict::Fail => "FAIL",
        Verdict::Na => "N/A",
    }
}

fn build_compare_report(
    before: &CompareMetrics,
    after: &CompareMetrics,
    core_verdicts: &[SingleVerdict],
    l2_verdicts: &[SingleVerdict],
    overall: Verdict,
    reasons: &[String],
) -> String {
    let mut lines = Vec::new();

    lines.push("# Trace Evaluation Comparison Report".to_string());
    lines.push(String::new());
    lines.push(format!(
        "- **对比窗口**: before={} after={}",
        before.report_date, after.report_date
    ));
    lines.push(format!(
        "- **样本数**: before={} after={}",
        before.total_runs, after.total_runs
    ));

    let before_baseline_rate = if before.baseline_run_ids > 0 {
        before.baseline_hits as f64 / before.baseline_run_ids as f64 * 100.0
    } else {
        0.0
    };
    let after_baseline_rate = if after.baseline_run_ids > 0 {
        after.baseline_hits as f64 / after.baseline_run_ids as f64 * 100.0
    } else {
        0.0
    };

    lines.push(format!(
        "- **baseline 覆盖**: before={:.1}% after={:.1}%",
        before_baseline_rate, after_baseline_rate
    ));
    lines.push(format!("- **综合判定**: {}", verdict_str(overall)));
    lines.push(String::new());

    // Core metrics
    lines.push("## 核心指标（7项）".to_string());
    lines.push(String::new());
    lines.push("| # | 指标 | before | after | 变动 | 判定 | 说明 |".to_string());
    lines.push("| ---: | --- | ---: | ---: | ---: | --- | --- |".to_string());

    for (idx, v) in core_verdicts.iter().enumerate() {
        let unit = if v.metric == "avg_step_count" {
            ""
        } else {
            "%"
        };
        let delta_str = if v.metric == "avg_step_count" {
            format!("{:.1}步", v.delta)
        } else {
            format!("{:.1}pp", v.delta)
        };
        lines.push(format!(
            "| {} | {} | {:.1}{} | {:.1}{} | {}{} | {} | {} |",
            idx + 1,
            v.metric,
            v.before,
            unit,
            v.after,
            unit,
            if v.delta > 0.0 { "+" } else { "" },
            delta_str,
            verdict_emoji(v.verdict),
            v.note
        ));
    }

    let core_pass = core_verdicts
        .iter()
        .filter(|v| v.verdict == Verdict::Pass)
        .count();
    let core_warn = core_verdicts
        .iter()
        .filter(|v| v.verdict == Verdict::Warn)
        .count();
    let core_fail = core_verdicts
        .iter()
        .filter(|v| v.verdict == Verdict::Fail)
        .count();
    lines.push(String::new());
    lines.push(format!(
        "**核心指标统计**: PASS={} WARN={} FAIL={}",
        core_pass, core_warn, core_fail
    ));

    // L2 metrics
    if !l2_verdicts.is_empty() {
        lines.push(String::new());
        lines.push("## L2 扩展指标（3项）".to_string());
        lines.push(String::new());
        lines.push("| # | 指标 | before | after | 变动 | 判定 | 说明 |".to_string());
        lines.push("| ---: | --- | ---: | ---: | ---: | --- | --- |".to_string());

        for (idx, v) in l2_verdicts.iter().enumerate() {
            let unit = if v.metric == "avg_step_count" {
                ""
            } else {
                "%"
            };
            let delta_str = if v.metric == "avg_step_count" {
                format!("{:.1}步", v.delta)
            } else {
                format!("{:.1}pp", v.delta)
            };
            lines.push(format!(
                "| {} | {} | {:.1}{} | {:.1}{} | {}{} | {} | {} |",
                idx + 8,
                v.metric,
                v.before,
                unit,
                v.after,
                unit,
                if v.delta > 0.0 { "+" } else { "" },
                delta_str,
                verdict_emoji(v.verdict),
                v.note
            ));
        }
    }

    // Persistent State Update Trend (observability only, not gate)
    lines.push(String::new());
    lines.push("## Persistent State Update Trend".to_string());
    lines.push(String::new());
    lines.push("| metric | before | after | delta |".to_string());
    lines.push("| --- | ---: | ---: | ---: |".to_string());
    lines.push(format!(
        "| count | {} | {} | {:+} |",
        before.persistent_state_updated_count,
        after.persistent_state_updated_count,
        after.persistent_state_updated_count as isize
            - before.persistent_state_updated_count as isize
    ));
    lines.push(format!(
        "| rate | {} | {} | {} |",
        fmt_optional_rate(before.persistent_state_updated_rate),
        fmt_optional_rate(after.persistent_state_updated_rate),
        fmt_optional_rate_delta(
            before.persistent_state_updated_rate,
            after.persistent_state_updated_rate
        )
    ));

    // Trace Schema Health (observability only, not gate)
    lines.push(String::new());
    lines.push("## Trace Schema Health (observability only, not gate)".to_string());
    lines.push(String::new());
    lines.push("| metric | before | after |".to_string());
    lines.push("| --- | ---: | ---: |".to_string());
    lines.push(format!(
        "| unsupported_version_count | {} | {} |",
        before.unsupported_version_count, after.unsupported_version_count
    ));
    lines.push(format!(
        "| missing_core_field_count | {} | {} |",
        before.missing_core_field_count, after.missing_core_field_count
    ));

    // Reasons
    lines.push(String::new());
    lines.push("## 判定依据".to_string());
    lines.push(String::new());
    for reason in reasons {
        lines.push(format!("- {}", reason));
    }

    // Missing fields annotation
    if !before.missing_fields.is_empty() || !after.missing_fields.is_empty() {
        lines.push(String::new());
        lines.push("## 缺失指标标注".to_string());
        lines.push(String::new());
        if !before.missing_fields.is_empty() {
            lines.push(format!(
                "- before 缺失指标（已按 0 处理）: {}",
                before.missing_fields.join(", ")
            ));
        }
        if !after.missing_fields.is_empty() {
            lines.push(format!(
                "- after 缺失指标（已按 0 处理）: {}",
                after.missing_fields.join(", ")
            ));
        }
    }

    // Follow-up actions
    lines.push(String::new());
    lines.push("## 后续动作建议".to_string());
    lines.push(String::new());
    match overall {
        Verdict::Pass => {
            lines.push("- 综合结论 PASS：改动无 regressions，可合并/发布".to_string());
            lines.push("- 建议：继续观察后续 trace".to_string());
        }
        Verdict::Warn => {
            lines.push("- 综合结论 WARN：有波动需关注，建议 review 但不阻塞".to_string());
            let warn_metrics: Vec<_> = core_verdicts
                .iter()
                .filter(|v| v.verdict == Verdict::Warn)
                .map(|v| v.metric)
                .collect();
            if !warn_metrics.is_empty() {
                lines.push(format!("- 关注指标: {}", warn_metrics.join(", ")));
            }
        }
        Verdict::Fail => {
            lines.push("- 综合结论 FAIL：存在明确 regression，必须修复后再合并".to_string());
            let fail_metrics: Vec<_> = core_verdicts
                .iter()
                .filter(|v| v.verdict == Verdict::Fail)
                .map(|v| v.metric)
                .collect();
            if !fail_metrics.is_empty() {
                lines.push(format!("- 需修复指标: {}", fail_metrics.join(", ")));
            }
            lines.push(
                "- 修复后重新运行 `cargo run --bin trace_eval` 生成新报告并再次对比".to_string(),
            );
        }
        Verdict::Na => {
            lines.push("- 存在 N/A 指标，建议人工确认后再判定".to_string());
        }
    }

    lines.push(String::new());
    lines.push("---".to_string());
    lines.push("*本对比报告由 trace_eval --compare 自动生成。*".to_string());

    lines.join("\n")
}

// -------------------------------------------------------------------------
// Compare mode orchestration
// -------------------------------------------------------------------------

fn run_compare_mode(
    before_path: &Path,
    after_path: &Path,
    output_path: Option<&PathBuf>,
    gate_mode: bool,
    gate_strict: bool,
    gate_json: bool,
) {
    let before = match load_compare_metrics(before_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("加载 before 报告失败: {}", e);
            std::process::exit(1);
        }
    };
    let after = match load_compare_metrics(after_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("加载 after 报告失败: {}", e);
            std::process::exit(1);
        }
    };

    // Scenario A: sample size increase > 50%
    let relaxed = after.total_runs as f64 > before.total_runs as f64 * 1.5;

    // Evaluate core metrics
    let core_rules = [
        MetricRule {
            name: "success_rate",
            direction: Direction::HigherIsBetter,
            warn_delta: 3.0,
            fail_delta: 5.0,
            absolute_fail: None,
        },
        MetricRule {
            name: "fallback_rate",
            direction: Direction::LowerIsBetter,
            warn_delta: 3.0,
            fail_delta: 5.0,
            absolute_fail: None,
        },
        MetricRule {
            name: "context_drop_rate",
            direction: Direction::LowerIsBetter,
            warn_delta: 3.0,
            fail_delta: 5.0,
            absolute_fail: None,
        },
        MetricRule {
            name: "state_present_rate",
            direction: Direction::HigherIsBetter,
            warn_delta: 5.0,
            fail_delta: 10.0,
            absolute_fail: None,
        },
        MetricRule {
            name: "memory_injected_rate",
            direction: Direction::HigherIsBetter,
            warn_delta: 5.0,
            fail_delta: 10.0,
            absolute_fail: None,
        },
        MetricRule {
            name: "recovery_success_rate",
            direction: Direction::HigherIsBetter,
            warn_delta: 10.0,
            fail_delta: 20.0,
            absolute_fail: Some(|v| v < 60.0),
        },
        MetricRule {
            name: "unknown_failure_rate",
            direction: Direction::LowerIsBetter,
            warn_delta: 2.0,
            fail_delta: 5.0,
            absolute_fail: Some(|v| v > 10.0),
        },
    ];

    let mut core_verdicts = Vec::new();
    for rule in &core_rules {
        let before_val = before.get(rule.name);
        let after_val = after.get(rule.name);

        // Scenario B: denominator zero
        if !has_valid_denominator(rule.name, &before) || !has_valid_denominator(rule.name, &after) {
            core_verdicts.push(build_na_verdict(rule.name, &before, &after));
            continue;
        }

        core_verdicts.push(evaluate_single(
            rule.name, before_val, after_val, rule, relaxed,
        ));
    }

    // Evaluate L2 metrics
    let l2_rules = [
        MetricRule {
            name: "tool_success_rate",
            direction: Direction::HigherIsBetter,
            warn_delta: 3.0,
            fail_delta: 5.0,
            absolute_fail: None,
        },
        MetricRule {
            name: "planning_stall_rate",
            direction: Direction::LowerIsBetter,
            warn_delta: 2.0,
            fail_delta: 5.0,
            absolute_fail: None,
        },
        MetricRule {
            name: "avg_step_count",
            direction: Direction::LowerIsBetter,
            warn_delta: 0.5,
            fail_delta: 1.0,
            absolute_fail: None,
        },
    ];

    let mut l2_verdicts = Vec::new();
    for rule in &l2_rules {
        let before_val = before.get(rule.name);
        let after_val = after.get(rule.name);

        if !has_valid_denominator(rule.name, &before) || !has_valid_denominator(rule.name, &after) {
            l2_verdicts.push(build_na_verdict(rule.name, &before, &after));
            continue;
        }

        l2_verdicts.push(evaluate_single(
            rule.name, before_val, after_val, rule, relaxed,
        ));
    }

    // Compute overall verdict
    let (overall, reasons) = compute_overall(&core_verdicts, &l2_verdicts, &before, &after);

    // Build report
    let report = build_compare_report(
        &before,
        &after,
        &core_verdicts,
        &l2_verdicts,
        overall,
        &reasons,
    );

    // Gate mode:精简输出
    if gate_mode {
        if gate_json {
            let output = build_gate_json_output(overall, &reasons, &before, &after);
            println!(
                "{}",
                serde_json::to_string(&output).unwrap_or_else(|_| "{}".to_string())
            );
        } else {
            for line in build_gate_output_lines(overall, &reasons, &before, &after) {
                println!("{}", line);
            }
        }
        match overall {
            Verdict::Pass => std::process::exit(0),
            Verdict::Warn => {
                if gate_strict {
                    std::process::exit(2);
                } else {
                    std::process::exit(0);
                }
            }
            Verdict::Fail => std::process::exit(1),
            Verdict::Na => std::process::exit(2),
        }
    }

    if let Some(path) = output_path {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(path, &report).expect("写入对比报告失败");
        println!("对比报告已生成: {}", path.display());
    } else {
        println!("{}", report);
    }

    println!("综合判定: {}", verdict_str(overall));
}

// =============================================================================
// UNIT TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Parser tests
    // -----------------------------------------------------------------------

    fn minimal_report() -> String {
        let lines = vec![
            "# Trace Evaluation Report".to_string(),
            String::new(),
            "- generated: 2026-04-18T08:36:22.517783+00:00".to_string(),
            "- total traces: 20".to_string(),
            "- baseline_file: notes/baseline.md".to_string(),
            "- baseline_run_ids: 20".to_string(),
            "- interesting traces: 15".to_string(),
            String::new(),
            "## Summary Statistics".to_string(),
            String::new(),
            "| metric | count | ratio |".to_string(),
            "| --- | ---: | ---: |".to_string(),
            "| total | 20 | 100% |".to_string(),
            "| success | 15 | 75.0% |".to_string(),
            "| with memory injected | 4 | 20.0% |".to_string(),
            "| with memory dropped | 2 | 10.0% |".to_string(),
            "| with session state | 3 | 15.0% |".to_string(),
            "| with context pack dropped | 3 | 15.0% |".to_string(),
            "| with llm fallback | 7 | 35.0% |".to_string(),
            "| with failures | 6 | 30.0% |".to_string(),
            "| with persistent state updated | 2 | 10.0% |".to_string(),
            String::new(),
            "## Persistent State Update Breakdown".to_string(),
            String::new(),
            "| updated | traces | success | success_rate |".to_string(),
            "| --- | ---: | ---: | ---: |".to_string(),
            "| true | 2 | 1 | 50.0% |".to_string(),
            "| false | 18 | 14 | 77.8% |".to_string(),
            String::new(),
            "## Baseline Coverage".to_string(),
            String::new(),
            "| metric | count | ratio |".to_string(),
            "| --- | ---: | ---: |".to_string(),
            "| baseline run ids | 20 | 100% |".to_string(),
            "| baseline hits in current trace set | 20 | 100.0% |".to_string(),
            "| baseline missing in current trace set | 0 | 0.0% |".to_string(),
            String::new(),
            "## Failure Type Distribution".to_string(),
            String::new(),
            "| failure_type | count | ratio |".to_string(),
            "| --- | ---: | ---: |".to_string(),
            "| tool_call_error | 2 | 10.0% |".to_string(),
            "| unknown_failure | 1 | 5.0% |".to_string(),
            String::new(),
            "## Tool Use Statistics".to_string(),
            String::new(),
            "| metric | count | ratio |".to_string(),
            "| --- | ---: | ---: |".to_string(),
            "| traces with tool calls | 17 | 85.0% |".to_string(),
            "| total tool calls | 28 | - |".to_string(),
            "| tool success | 23 | 82.1% |".to_string(),
            "| tool failure | 5 | 17.9% |".to_string(),
            String::new(),
            "## Planning / ReAct Statistics".to_string(),
            String::new(),
            "| metric | value |".to_string(),
            "| --- | --- |".to_string(),
            "| step_count min / max / avg | 1 / 12 / 3.2 |".to_string(),
            "| unfinished_plan (failed + steps > 5) | 1 |".to_string(),
            "| stall_or_drift hits | 1 |".to_string(),
            String::new(),
            "## Recovery Statistics".to_string(),
            String::new(),
            "| metric | count | ratio |".to_string(),
            "| --- | ---: | ---: |".to_string(),
            "| traces_with_recovery | 6 | 30.0% |".to_string(),
            "| recovery_attempt_count | 6 | - |".to_string(),
            "| recovery_success | 1 | 16.7% |".to_string(),
            "| recovery_failure | 5 | 83.3% |".to_string(),
        ];
        lines.join("\n")
    }

    fn make_summary(
        run_id: &str,
        recovery_attempt_details: Vec<RecoveryAttemptSummary>,
    ) -> TraceSummary {
        let recovery_attempt_count = recovery_attempt_details.len();
        let recovery_success_count = recovery_attempt_details
            .iter()
            .filter(|attempt| attempt.successful)
            .count();
        let has_recovery_attempt = recovery_attempt_count > 0;
        let recovery_succeeded = if has_recovery_attempt {
            Some(recovery_success_count > 0)
        } else {
            None
        };

        TraceSummary {
            run_id: run_id.to_string(),
            started_at: "2026-04-18T08:36:22.517783+00:00".to_string(),
            user_input: "test".to_string(),
            user_input_chars: 4,
            success: true,
            error_short: None,
            duration_ms: Some(100),
            step_count: 1,
            llm_fallback: false,
            has_failures: false,
            failure_count: 0,
            failure_types: Vec::new(),
            memory_retrieved: 0,
            memory_injected: 0,
            memory_dropped: 0,
            memory_total_chars: 0,
            retriever_name: String::new(),
            retrieval_candidate_count: 0,
            retrieval_hit_count: 0,
            retrieval_latency_ms: 0,
            retrieval_mode: String::new(),
            retrieval_fallback_reason: None,
            retrieval_scores_present: false,
            state_present: false,
            context_pack_dropped: false,
            context_pack_drop_reasons: Vec::new(),
            llm_call_count: 0,
            llm_success_count: 0,
            llm_failure_count: 0,
            tool_call_count: 0,
            tool_success_count: 0,
            tool_failure_count: 0,
            tool_error_types: Vec::new(),
            has_recovery_attempt,
            recovery_attempt_count,
            recovery_success_count,
            recovery_succeeded,
            recovery_actions: Vec::new(),
            recovery_results: Vec::new(),
            recovery_attempt_details,
            in_baseline: false,
            is_interesting: false,
            interest_reasons: Vec::new(),
            persistent_state_updated: false,
        }
    }

    // -----------------------------------------------------------------------
    // Wilson bound tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_wilson_bound_99_reference_values() {
        // n=200, x=6 (p=3%), z=2.326 单侧上界，独立公式核算值
        let upper = wilson_bound_99(6, 200, Direction::LowerIsBetter).unwrap();
        assert!((upper - 0.072706).abs() < 1e-4, "upper={}", upper);
        // HigherIsBetter 取下界：x=38, n=40 (p=95%)
        let lower = wilson_bound_99(38, 40, Direction::HigherIsBetter).unwrap();
        assert!((lower - 0.804008).abs() < 1e-4, "lower={}", lower);
    }

    #[test]
    fn test_wilson_bound_99_edge_cases() {
        // n=0：无界
        assert!(wilson_bound_99(0, 0, Direction::HigherIsBetter).is_none());
        assert!(wilson_bound_99(0, 0, Direction::LowerIsBetter).is_none());
        // p=0：下界=0，上界=z²/(n+z²)
        let lower = wilson_bound_99(0, 40, Direction::HigherIsBetter).unwrap();
        let upper = wilson_bound_99(0, 40, Direction::LowerIsBetter).unwrap();
        assert!(lower.abs() < 1e-12, "lower={}", lower);
        assert!((upper - 0.119142).abs() < 1e-4, "upper={}", upper);
        // p=1：上界=1，下界=n/(n+z²)
        let lower = wilson_bound_99(40, 40, Direction::HigherIsBetter).unwrap();
        let upper = wilson_bound_99(40, 40, Direction::LowerIsBetter).unwrap();
        assert!((lower - 0.880858).abs() < 1e-4, "lower={}", lower);
        assert!((upper - 1.0).abs() < 1e-12, "upper={}", upper);
    }

    #[test]
    fn test_parse_percentage_new_and_old_format() {
        // 旧格式
        assert!((parse_percentage("12.5%").unwrap() - 12.5).abs() < 1e-9);
        // 新格式：带分母与 Wilson 界
        let v = parse_percentage("12.5% (5/40), Wilson99 上界 24.1%").unwrap();
        assert!((v - 12.5).abs() < 1e-9);
        assert!(parse_percentage("-").is_none());
    }

    #[test]
    fn test_parse_count_total_formats() {
        assert_eq!(
            parse_count_total("12.5% (5/40), Wilson99 上界 24.1%"),
            Some((5, 40))
        );
        // 兼容带空格的 "(n / d)" 形态
        assert_eq!(parse_count_total("85.0% (17 / 20)"), Some((17, 20)));
        // 旧格式无括号
        assert_eq!(parse_count_total("12.5%"), None);
        assert_eq!(parse_count_total("-"), None);
    }

    #[test]
    fn test_parse_report_old_format_has_no_rate_samples() {
        // 旧报告（无分母无区间）解析行为不变：rate 照常读出，rate_samples 为空
        let metrics = parse_report(&minimal_report()).unwrap();
        assert!((metrics.success_rate.unwrap() - 75.0).abs() < 0.01);
        assert!((metrics.tool_success_rate.unwrap() - 82.1).abs() < 0.1);
        assert!(metrics.rate_samples.is_empty());
    }

    #[test]
    fn test_parse_report_new_format_roundtrip() {
        // build_report 产出带 (n/d) + Wilson99 的单元格，parse_report 应还原 rate 与分母
        let mut s1 = make_summary("run-1", vec![]);
        s1.success = true;
        s1.llm_fallback = true;
        s1.tool_call_count = 2;
        s1.tool_success_count = 2;
        let mut s2 = make_summary("run-2", vec![]);
        s2.success = false;
        s2.tool_call_count = 2;
        s2.tool_success_count = 1;

        let report = build_report(
            &[s1, s2],
            false,
            &HashSet::new(),
            None,
            &TraceLoadStats::default(),
        );
        assert!(report.contains("Wilson99"), "report:\n{}", report);

        let metrics = parse_report(&report).unwrap();
        assert!((metrics.success_rate.unwrap() - 50.0).abs() < 0.01);
        assert!((metrics.fallback_rate.unwrap() - 50.0).abs() < 0.01);
        assert!((metrics.tool_success_rate.unwrap() - 75.0).abs() < 0.01);
        // planning_stall_rate 由 count/total_runs 推导，与单元格分母一致
        assert!(metrics.planning_stall_rate.unwrap().abs() < 0.01);
        assert_eq!(metrics.rate_samples.get("success_rate"), Some(&(1, 2)));
        assert_eq!(metrics.rate_samples.get("fallback_rate"), Some(&(1, 2)));
        assert_eq!(metrics.rate_samples.get("tool_success_rate"), Some(&(3, 4)));
        assert_eq!(
            metrics.rate_samples.get("planning_stall_rate"),
            Some(&(0, 2))
        );
        assert_eq!(
            metrics.rate_samples.get("recovery_success_rate"),
            Some(&(0, 0))
        );
    }

    // -----------------------------------------------------------------------
    // JSON sidecar tests
    // -----------------------------------------------------------------------

    fn temp_test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "trace_eval_sidecar_test_{}_{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sidecar_sample_summaries() -> Vec<TraceSummary> {
        let mut s1 = make_summary("run-1", vec![]);
        s1.success = true;
        s1.llm_fallback = true;
        s1.state_present = true;
        s1.memory_injected = 2;
        s1.persistent_state_updated = true;
        s1.tool_call_count = 2;
        s1.tool_success_count = 2;
        s1.step_count = 3;
        let mut s2 = make_summary("run-2", vec![]);
        s2.success = false;
        s2.context_pack_dropped = true;
        s2.tool_call_count = 2;
        s2.tool_success_count = 1;
        s2.step_count = 5;
        vec![s1, s2]
    }

    #[test]
    fn test_sidecar_roundtrip_matches_markdown_parse() {
        // sidecar 写出 -> 读回，与同一批 summaries 的 markdown 解析结果一致
        let summaries = sidecar_sample_summaries();
        let sidecar = build_report_sidecar(&summaries, &HashSet::new(), &TraceLoadStats::default());
        assert_eq!(sidecar.schema_version, REPORT_SIDECAR_SCHEMA_VERSION);

        let json = serde_json::to_string_pretty(&sidecar).unwrap();
        let from_sidecar = parse_sidecar(&json).unwrap();
        let from_markdown = parse_report(&build_report(
            &summaries,
            false,
            &HashSet::new(),
            None,
            &TraceLoadStats::default(),
        ))
        .unwrap();

        assert_eq!(from_sidecar.total_runs, from_markdown.total_runs);
        assert_eq!(
            from_sidecar.persistent_state_updated_count,
            from_markdown.persistent_state_updated_count
        );
        assert_eq!(from_sidecar.recovery_attempt_count, 0);
        assert_eq!(from_sidecar.total_tool_calls, 4);
        // sidecar rate 与 markdown 单元格同一舍入，应精确一致
        for (name, sidecar_val, markdown_val) in [
            (
                "success_rate",
                from_sidecar.success_rate,
                from_markdown.success_rate,
            ),
            (
                "fallback_rate",
                from_sidecar.fallback_rate,
                from_markdown.fallback_rate,
            ),
            (
                "context_drop_rate",
                from_sidecar.context_drop_rate,
                from_markdown.context_drop_rate,
            ),
            (
                "state_present_rate",
                from_sidecar.state_present_rate,
                from_markdown.state_present_rate,
            ),
            (
                "memory_injected_rate",
                from_sidecar.memory_injected_rate,
                from_markdown.memory_injected_rate,
            ),
            (
                "recovery_success_rate",
                from_sidecar.recovery_success_rate,
                from_markdown.recovery_success_rate,
            ),
            (
                "unknown_failure_rate",
                from_sidecar.unknown_failure_rate,
                from_markdown.unknown_failure_rate,
            ),
            (
                "tool_success_rate",
                from_sidecar.tool_success_rate,
                from_markdown.tool_success_rate,
            ),
            (
                "planning_stall_rate",
                from_sidecar.planning_stall_rate,
                from_markdown.planning_stall_rate,
            ),
        ] {
            let diff = (sidecar_val.unwrap() - markdown_val.unwrap()).abs();
            assert!(diff < 1e-9, "{}: sidecar vs markdown diff={}", name, diff);
        }
        assert!((from_sidecar.avg_step_count.unwrap() - 4.0).abs() < 1e-9);
        // 分母信息与 markdown 反解析一致
        for key in [
            "success_rate",
            "fallback_rate",
            "tool_success_rate",
            "planning_stall_rate",
            "recovery_success_rate",
        ] {
            assert_eq!(
                from_sidecar.rate_samples.get(key),
                from_markdown.rate_samples.get(key),
                "rate_samples[{}]",
                key
            );
        }
        // sidecar 数据齐全，无缺失字段
        assert!(from_sidecar.missing_fields.is_empty());
        // wilson 界：total=0 时为 None，total>0 时有值
        assert!(from_sidecar
            .rate_samples
            .contains_key("persistent_state_updated_rate"));
        let success_metric =
            &build_report_sidecar(&summaries, &HashSet::new(), &TraceLoadStats::default()).rates
                ["success_rate"];
        assert!(success_metric.wilson99_bound.is_some());
        let recovery_metric = &sidecar.rates["recovery_success_rate"];
        assert!(recovery_metric.wilson99_bound.is_none());
    }

    #[test]
    fn test_load_compare_metrics_falls_back_to_markdown_without_sidecar() {
        // 无 sidecar 的旧报告：走 markdown 解析，行为与之前完全一致
        let dir = temp_test_dir("fallback");
        let report_path = dir.join("old-report.md");
        fs::write(&report_path, minimal_report()).unwrap();

        let loaded = load_compare_metrics(&report_path).unwrap();
        let parsed = parse_report(&minimal_report()).unwrap();
        assert_eq!(loaded.total_runs, parsed.total_runs);
        assert!((loaded.success_rate.unwrap() - parsed.success_rate.unwrap()).abs() < 1e-9);
        assert!(
            (loaded.tool_success_rate.unwrap() - parsed.tool_success_rate.unwrap()).abs() < 1e-9
        );
        assert!(loaded.rate_samples.is_empty());
        assert_eq!(loaded.report_date, "2026-04-18");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_sidecar_unknown_schema_version_falls_back() {
        // schema_version 不认识：parse_sidecar 明确报错，load 回退 markdown（更安全的选项）
        let sidecar = build_report_sidecar(
            &sidecar_sample_summaries(),
            &HashSet::new(),
            &TraceLoadStats::default(),
        );
        let json = serde_json::to_string(&sidecar)
            .unwrap()
            .replace(REPORT_SIDECAR_SCHEMA_VERSION, "trace_eval_report_v999");
        let err = parse_sidecar(&json).unwrap_err();
        assert!(err.contains("trace_eval_report_v999"), "err={}", err);

        let dir = temp_test_dir("schema");
        let report_path = dir.join("report.md");
        fs::write(&report_path, minimal_report()).unwrap();
        fs::write(report_path.with_extension("json"), &json).unwrap();
        let loaded = load_compare_metrics(&report_path).unwrap();
        // 回退到 markdown 解析结果，而非 sidecar 内容
        assert_eq!(loaded.total_runs, 20);
        assert!((loaded.success_rate.unwrap() - 75.0).abs() < 0.01);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_compare_metrics_ignores_broken_sidecar() {
        // sidecar 存在但 JSON 损坏：回退 markdown 解析
        let dir = temp_test_dir("broken");
        let report_path = dir.join("report.md");
        fs::write(&report_path, minimal_report()).unwrap();
        fs::write(report_path.with_extension("json"), "not a json").unwrap();

        let loaded = load_compare_metrics(&report_path).unwrap();
        assert_eq!(loaded.total_runs, 20);
        assert!((loaded.success_rate.unwrap() - 75.0).abs() < 0.01);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_compare_metrics_prefers_sidecar() {
        // sidecar 存在且 schema 认识：优先消费 sidecar，不再依赖 markdown 文本
        let dir = temp_test_dir("prefer");
        let report_path = dir.join("report.md");
        fs::write(&report_path, "故意写非报告内容，证明未被读取").unwrap();
        let sidecar = build_report_sidecar(
            &sidecar_sample_summaries(),
            &HashSet::new(),
            &TraceLoadStats::default(),
        );
        fs::write(
            report_path.with_extension("json"),
            serde_json::to_string_pretty(&sidecar).unwrap(),
        )
        .unwrap();

        let loaded = load_compare_metrics(&report_path).unwrap();
        assert_eq!(loaded.total_runs, 2);
        assert!((loaded.success_rate.unwrap() - 50.0).abs() < 1e-9);
        assert_eq!(loaded.rate_samples.get("tool_success_rate"), Some(&(3, 4)));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_parse_report_basic() {
        let report = minimal_report();
        let metrics = parse_report(&report).unwrap();

        assert_eq!(metrics.total_runs, 20);
        assert_eq!(metrics.baseline_run_ids, 20);
        assert_eq!(metrics.baseline_hits, 20);
        assert_eq!(metrics.report_date, "2026-04-18");

        assert!((metrics.success_rate.unwrap() - 75.0).abs() < 0.01);
        assert!((metrics.fallback_rate.unwrap() - 35.0).abs() < 0.01);
        assert!((metrics.context_drop_rate.unwrap() - 15.0).abs() < 0.01);
        assert!((metrics.state_present_rate.unwrap() - 15.0).abs() < 0.01);
        assert!((metrics.memory_injected_rate.unwrap() - 20.0).abs() < 0.01);

        // Persistent state updated
        assert_eq!(metrics.persistent_state_updated_count, 2);
        assert!((metrics.persistent_state_updated_rate.unwrap() - 10.0).abs() < 0.01);

        // Derived: unknown_failure = 1 / 20 * 100 = 5.0%
        assert!((metrics.unknown_failure_rate.unwrap() - 5.0).abs() < 0.01);

        // Tool success rate
        assert!((metrics.tool_success_rate.unwrap() - 82.1).abs() < 0.1);
        assert_eq!(metrics.total_tool_calls, 28);

        // Planning
        assert!((metrics.avg_step_count.unwrap() - 3.2).abs() < 0.01);
        // stall_or_drift = 1 / 20 * 100 = 5.0%
        assert!((metrics.planning_stall_rate.unwrap() - 5.0).abs() < 0.01);

        // Recovery
        assert!((metrics.recovery_success_rate.unwrap() - 16.7).abs() < 0.1);
        assert_eq!(metrics.recovery_attempt_count, 6);

        assert!(metrics.missing_fields.is_empty());
    }

    #[test]
    fn test_recovery_by_failure_type_uses_attempt_level_aggregation() {
        let summary1 = make_summary(
            "run-1",
            vec![
                RecoveryAttemptSummary {
                    failure_kind: "tool_call_error".to_string(),
                    successful: true,
                },
                RecoveryAttemptSummary {
                    failure_kind: "tool_call_error".to_string(),
                    successful: false,
                },
            ],
        );
        let summary2 = make_summary(
            "run-2",
            vec![RecoveryAttemptSummary {
                failure_kind: "planning_stall_or_drift".to_string(),
                successful: false,
            }],
        );

        let report = build_report(
            &[summary1, summary2],
            false,
            &HashSet::new(),
            None,
            &TraceLoadStats::default(),
        );

        assert!(report.contains("| tool_call_error | 2 | 1 | 1 |"));
        assert!(report.contains("| planning_stall_or_drift | 1 | 0 | 1 |"));
    }

    #[test]
    fn test_parse_report_missing_fields() {
        let report = "# Trace Evaluation Report\n\n- generated: 2026-04-18T00:00:00Z\n- total traces: 10\n\n## Summary Statistics\n\n| metric | count | ratio |\n| --- | ---: | ---: |\n| total | 10 | 100% |\n| success | 8 | 80.0% |\n".to_string();
        let metrics = parse_report(&report).unwrap();
        assert_eq!(metrics.total_runs, 10);
        assert!((metrics.success_rate.unwrap() - 80.0).abs() < 0.01);
        assert!(metrics.fallback_rate.is_none()); // missing
        assert!(metrics
            .missing_fields
            .contains(&"fallback_rate".to_string()));
    }

    #[test]
    fn test_parse_report_invalid_plain_text() {
        // Plain text file should fail to parse
        let result = parse_report("this is just plain text");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("trace_eval 报告"));
    }

    #[test]
    fn test_parse_report_invalid_no_summary() {
        // Missing Summary Statistics section
        let result = parse_report(
            "# Trace Evaluation Report\n\n- generated: 2026-04-18T00:00:00Z\n- total traces: 10\n\n## Baseline Coverage\n\n| metric | count | ratio |\n| --- | ---: | ---: |\n| baseline run ids | 20 | 100% |\n"
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Summary Statistics"));
    }

    #[test]
    fn test_parse_report_invalid_no_success() {
        // Summary Statistics present but no success row
        let result = parse_report(
            "# Trace Evaluation Report\n\n- generated: 2026-04-18T00:00:00Z\n- total traces: 10\n\n## Summary Statistics\n\n| metric | count | ratio |\n| --- | ---: | ---: |\n| total | 10 | 100% |\n| with llm fallback | 2 | 20.0% |\n"
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("success_rate"));
    }

    #[test]
    fn test_parse_report_invalid_zero_traces() {
        // total_traces = 0
        let result = parse_report(
            "# Trace Evaluation Report\n\n- generated: 2026-04-18T00:00:00Z\n- total traces: 0\n\n## Summary Statistics\n\n| metric | count | ratio |\n| --- | ---: | ---: |\n| total | 0 | 100% |\n| success | 0 | 0.0% |\n"
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("total_traces=0"));
    }

    // -----------------------------------------------------------------------
    // Threshold boundary tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_success_rate_pass() {
        let rule = MetricRule {
            name: "success_rate",
            direction: Direction::HigherIsBetter,
            warn_delta: 3.0,
            fail_delta: 5.0,
            absolute_fail: None,
        };
        // No change
        let v = evaluate_single("success_rate", 80.0, 80.0, &rule, false);
        assert_eq!(v.verdict, Verdict::Pass);
        // Improved
        let v = evaluate_single("success_rate", 80.0, 85.0, &rule, false);
        assert_eq!(v.verdict, Verdict::Pass);
        // Small drop (< 3pp)
        let v = evaluate_single("success_rate", 80.0, 77.5, &rule, false);
        assert_eq!(v.verdict, Verdict::Pass);
    }

    #[test]
    fn test_success_rate_warn() {
        let rule = MetricRule {
            name: "success_rate",
            direction: Direction::HigherIsBetter,
            warn_delta: 3.0,
            fail_delta: 5.0,
            absolute_fail: None,
        };
        // Drop = 3pp (boundary, should be WARN)
        let v = evaluate_single("success_rate", 80.0, 77.0, &rule, false);
        assert_eq!(v.verdict, Verdict::Warn);
        // Drop = 5pp (boundary, should be WARN)
        let v = evaluate_single("success_rate", 80.0, 75.0, &rule, false);
        assert_eq!(v.verdict, Verdict::Warn);
    }

    #[test]
    fn test_success_rate_fail() {
        let rule = MetricRule {
            name: "success_rate",
            direction: Direction::HigherIsBetter,
            warn_delta: 3.0,
            fail_delta: 5.0,
            absolute_fail: None,
        };
        // Drop = 5.1pp (> 5, should be FAIL)
        let v = evaluate_single("success_rate", 80.0, 74.9, &rule, false);
        assert_eq!(v.verdict, Verdict::Fail);
        // Drop = 6pp
        let v = evaluate_single("success_rate", 80.0, 74.0, &rule, false);
        assert_eq!(v.verdict, Verdict::Fail);
    }

    #[test]
    fn test_fallback_rate_lower_is_better() {
        let rule = MetricRule {
            name: "fallback_rate",
            direction: Direction::LowerIsBetter,
            warn_delta: 3.0,
            fail_delta: 5.0,
            absolute_fail: None,
        };
        // Improved (lower)
        let v = evaluate_single("fallback_rate", 35.0, 30.0, &rule, false);
        assert_eq!(v.verdict, Verdict::Pass);
        // Increase = 3pp (boundary, WARN)
        let v = evaluate_single("fallback_rate", 35.0, 38.0, &rule, false);
        assert_eq!(v.verdict, Verdict::Warn);
        // Increase = 5pp (boundary, WARN)
        let v = evaluate_single("fallback_rate", 35.0, 40.0, &rule, false);
        assert_eq!(v.verdict, Verdict::Warn);
        // Increase = 5.1pp (FAIL)
        let v = evaluate_single("fallback_rate", 35.0, 40.1, &rule, false);
        assert_eq!(v.verdict, Verdict::Fail);
    }

    #[test]
    fn test_unknown_failure_absolute_fail() {
        let rule = MetricRule {
            name: "unknown_failure_rate",
            direction: Direction::LowerIsBetter,
            warn_delta: 2.0,
            fail_delta: 5.0,
            absolute_fail: Some(|v| v > 10.0),
        };
        // after = 11% > 10%, absolute FAIL regardless of delta
        let v = evaluate_single("unknown_failure_rate", 5.0, 11.0, &rule, false);
        assert_eq!(v.verdict, Verdict::Fail);
        assert!(v.note.contains("绝对值"));
    }

    #[test]
    fn test_recovery_success_absolute_fail() {
        let rule = MetricRule {
            name: "recovery_success_rate",
            direction: Direction::HigherIsBetter,
            warn_delta: 10.0,
            fail_delta: 20.0,
            absolute_fail: Some(|v| v < 60.0),
        };
        // after = 50% < 60%, absolute FAIL
        let v = evaluate_single("recovery_success_rate", 80.0, 50.0, &rule, false);
        assert_eq!(v.verdict, Verdict::Fail);
        assert!(v.note.contains("绝对值"));
    }

    #[test]
    fn test_avg_step_count_boundaries() {
        let rule = MetricRule {
            name: "avg_step_count",
            direction: Direction::LowerIsBetter,
            warn_delta: 0.5,
            fail_delta: 1.0,
            absolute_fail: None,
        };
        // Increase = 0.5 (boundary, WARN)
        let v = evaluate_single("avg_step_count", 3.0, 3.5, &rule, false);
        assert_eq!(v.verdict, Verdict::Warn);
        // Increase = 1.0 (boundary, WARN)
        let v = evaluate_single("avg_step_count", 3.0, 4.0, &rule, false);
        assert_eq!(v.verdict, Verdict::Warn);
        // Increase = 1.1 (FAIL)
        let v = evaluate_single("avg_step_count", 3.0, 4.1, &rule, false);
        assert_eq!(v.verdict, Verdict::Fail);
        // Small increase (PASS)
        let v = evaluate_single("avg_step_count", 3.0, 3.4, &rule, false);
        assert_eq!(v.verdict, Verdict::Pass);
    }

    // -----------------------------------------------------------------------
    // Special scenario tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_scenario_a_relax_threshold() {
        let rule = MetricRule {
            name: "success_rate",
            direction: Direction::HigherIsBetter,
            warn_delta: 3.0,
            fail_delta: 5.0,
            absolute_fail: None,
        };
        // Normal: drop = 3pp -> WARN
        let v = evaluate_single("success_rate", 80.0, 77.0, &rule, false);
        assert_eq!(v.verdict, Verdict::Warn);
        // Relaxed: drop = 3pp < warn_threshold(5.0) -> PASS
        let v = evaluate_single("success_rate", 80.0, 77.0, &rule, true);
        assert_eq!(v.verdict, Verdict::Pass);
        // Relaxed: drop = 6pp -> WARN (5 <= 6 <= 7)
        let v = evaluate_single("success_rate", 80.0, 74.0, &rule, true);
        assert_eq!(v.verdict, Verdict::Warn);
        // Relaxed: drop = 7.1pp -> FAIL (> 7)
        let v = evaluate_single("success_rate", 80.0, 72.9, &rule, true);
        assert_eq!(v.verdict, Verdict::Fail);
    }

    #[test]
    fn test_scenario_b_na_transition() {
        // before=0, after>0: N/A -> has data
        let before = CompareMetrics {
            recovery_attempt_count: 0,
            total_tool_calls: 0,
            ..Default::default()
        };
        let after = CompareMetrics {
            recovery_attempt_count: 5,
            total_tool_calls: 10,
            ..Default::default()
        };
        assert!(!has_valid_denominator("recovery_success_rate", &before));
        assert!(has_valid_denominator("recovery_success_rate", &after));
        assert!(!has_valid_denominator("tool_success_rate", &before));
        assert!(has_valid_denominator("tool_success_rate", &after));
        // Other metrics always have valid denominator
        assert!(has_valid_denominator("success_rate", &before));
        assert!(has_valid_denominator("success_rate", &after));

        // Both have data -> valid denominator
        let before2 = CompareMetrics {
            recovery_attempt_count: 3,
            total_tool_calls: 5,
            ..Default::default()
        };
        assert!(has_valid_denominator("recovery_success_rate", &before2));
        assert!(has_valid_denominator("tool_success_rate", &before2));

        // Both zero -> invalid denominator (should be N/A)
        let both_zero = CompareMetrics {
            recovery_attempt_count: 0,
            total_tool_calls: 0,
            ..Default::default()
        };
        assert!(!has_valid_denominator("recovery_success_rate", &both_zero));
        assert!(!has_valid_denominator("tool_success_rate", &both_zero));
    }

    #[test]
    fn test_scenario_c_baseline_downgrade() {
        let before = CompareMetrics {
            baseline_run_ids: 100,
            baseline_hits: 100,
            ..Default::default()
        };
        let after = CompareMetrics {
            baseline_run_ids: 100,
            baseline_hits: 70,
            ..Default::default()
        };

        let core = vec![SingleVerdict {
            metric: "success_rate",
            before: 80.0,
            after: 80.0,
            delta: 0.0,
            verdict: Verdict::Pass,
            note: "无退化".to_string(),
        }];
        let (overall, reasons) = compute_overall(&core, &[], &before, &after);
        // All core PASS, but baseline drops 30pp > 20pp -> downgrade to WARN
        assert_eq!(overall, Verdict::Warn);
        assert!(reasons.iter().any(|r| r.contains("降级")));
    }

    #[test]
    fn test_scenario_c_no_downgrade_when_small_drop() {
        let before = CompareMetrics {
            baseline_run_ids: 100,
            baseline_hits: 100,
            ..Default::default()
        };
        let after = CompareMetrics {
            baseline_run_ids: 100,
            baseline_hits: 85,
            total_runs: 25,
            ..Default::default()
        };

        let core = vec![SingleVerdict {
            metric: "success_rate",
            before: 80.0,
            after: 80.0,
            delta: 0.0,
            verdict: Verdict::Pass,
            note: "无退化".to_string(),
        }];
        let (overall, _reasons) = compute_overall(&core, &[], &before, &after);
        // Drop = 15pp <= 20pp, no downgrade
        assert_eq!(overall, Verdict::Pass);
    }

    // -----------------------------------------------------------------------
    // Overall verdict tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_overall_all_pass() {
        let before = CompareMetrics::default();
        let after = CompareMetrics {
            total_runs: 25,
            ..Default::default()
        };
        let core = vec![
            SingleVerdict {
                metric: "success_rate",
                before: 80.0,
                after: 82.0,
                delta: 2.0,
                verdict: Verdict::Pass,
                note: "无退化".to_string(),
            },
            SingleVerdict {
                metric: "fallback_rate",
                before: 30.0,
                after: 28.0,
                delta: -2.0,
                verdict: Verdict::Pass,
                note: "无退化".to_string(),
            },
        ];
        let (overall, _reasons) = compute_overall(&core, &[], &before, &after);
        assert_eq!(overall, Verdict::Pass);
    }

    #[test]
    fn test_overall_warn_only() {
        let before = CompareMetrics::default();
        let after = CompareMetrics {
            total_runs: 25,
            ..Default::default()
        };
        let core = vec![
            SingleVerdict {
                metric: "success_rate",
                before: 80.0,
                after: 77.0,
                delta: -3.0,
                verdict: Verdict::Warn,
                note: "退化 3.0pp".to_string(),
            },
            SingleVerdict {
                metric: "fallback_rate",
                before: 30.0,
                after: 28.0,
                delta: -2.0,
                verdict: Verdict::Pass,
                note: "无退化".to_string(),
            },
        ];
        let (overall, _reasons) = compute_overall(&core, &[], &before, &after);
        assert_eq!(overall, Verdict::Warn);
    }

    #[test]
    fn test_overall_any_fail() {
        let before = CompareMetrics::default();
        let after = CompareMetrics {
            total_runs: 25,
            ..Default::default()
        };
        let core = vec![
            SingleVerdict {
                metric: "success_rate",
                before: 80.0,
                after: 74.0,
                delta: -6.0,
                verdict: Verdict::Fail,
                note: "明显退化 6.0pp".to_string(),
            },
            SingleVerdict {
                metric: "fallback_rate",
                before: 30.0,
                after: 28.0,
                delta: -2.0,
                verdict: Verdict::Pass,
                note: "无退化".to_string(),
            },
        ];
        let (overall, _reasons) = compute_overall(&core, &[], &before, &after);
        assert_eq!(overall, Verdict::Fail);
    }

    #[test]
    fn test_overall_unknown_failure_hard_threshold() {
        let before = CompareMetrics::default();
        let after = CompareMetrics {
            unknown_failure_rate: Some(11.0),
            total_runs: 25,
            ..Default::default()
        };
        let core = vec![SingleVerdict {
            metric: "success_rate",
            before: 80.0,
            after: 82.0,
            delta: 2.0,
            verdict: Verdict::Pass,
            note: "无退化".to_string(),
        }];
        let (overall, reasons) = compute_overall(&core, &[], &before, &after);
        assert_eq!(overall, Verdict::Fail);
        assert!(reasons.iter().any(|r| r.contains("unknown_failure_rate")));
    }

    #[test]
    fn test_overall_success_rate_hard_threshold() {
        let before = CompareMetrics {
            success_rate: Some(80.0),
            ..Default::default()
        };
        let after = CompareMetrics {
            success_rate: Some(74.0),
            total_runs: 25,
            ..Default::default()
        };
        let core = vec![];
        let (overall, reasons) = compute_overall(&core, &[], &before, &after);
        assert_eq!(overall, Verdict::Fail);
        assert!(reasons.iter().any(|r| r.contains("success_rate")));
    }

    // -----------------------------------------------------------------------
    // Markdown table parser tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_markdown_table_basic() {
        let body = "| metric | count | ratio |\n| --- | ---: | ---: |\n| total | 20 | 100% |\n| success | 15 | 75.0% |\n";
        let rows = parse_markdown_table(body);
        // Header row is included (not filtered out)
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], vec!["metric", "count", "ratio"]);
        assert_eq!(rows[1], vec!["total", "20", "100%"]);
        assert_eq!(rows[2], vec!["success", "15", "75.0%"]);
    }

    #[test]
    fn test_parse_markdown_table_empty() {
        // Only separator row, no data
        let body = "Some text\n\n| --- | --- |\n";
        let rows = parse_markdown_table(body);
        assert!(rows.is_empty());
    }

    #[test]
    fn test_split_into_sections() {
        let content = "## Summary\n\nFoo\n\n## Details\n\nBar\n";
        let sections = split_into_sections(content);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].0, "Summary");
        assert!(sections[0].1.contains("Foo"));
        assert_eq!(sections[1].0, "Details");
        assert!(sections[1].1.contains("Bar"));
    }

    // -----------------------------------------------------------------------
    // End-to-end compare mode (report generation smoke test)
    // -----------------------------------------------------------------------

    #[test]
    fn test_compare_report_generation() {
        let before = CompareMetrics {
            success_rate: Some(80.0),
            fallback_rate: Some(30.0),
            context_drop_rate: Some(10.0),
            state_present_rate: Some(50.0),
            memory_injected_rate: Some(20.0),
            recovery_success_rate: Some(70.0),
            unknown_failure_rate: Some(5.0),
            tool_success_rate: Some(90.0),
            planning_stall_rate: Some(3.0),
            avg_step_count: Some(3.0),
            total_runs: 20,
            baseline_run_ids: 20,
            baseline_hits: 20,
            recovery_attempt_count: 5,
            total_tool_calls: 30,
            rate_samples: HashMap::new(),
            persistent_state_updated_count: 2,
            persistent_state_updated_rate: Some(10.0),
            unsupported_version_count: 0,
            missing_core_field_count: 0,
            failure_type_counts: HashMap::new(),
            missing_fields: Vec::new(),
            report_date: "2026-04-17".to_string(),
        };

        let after = CompareMetrics {
            success_rate: Some(82.0),
            fallback_rate: Some(28.0),
            context_drop_rate: Some(11.0),
            state_present_rate: Some(48.0),
            memory_injected_rate: Some(22.0),
            recovery_success_rate: Some(65.0),
            unknown_failure_rate: Some(4.0),
            tool_success_rate: Some(88.0),
            planning_stall_rate: Some(2.0),
            avg_step_count: Some(3.5),
            total_runs: 22,
            baseline_run_ids: 20,
            baseline_hits: 19,
            recovery_attempt_count: 6,
            total_tool_calls: 35,
            rate_samples: HashMap::new(),
            persistent_state_updated_count: 5,
            persistent_state_updated_rate: Some(22.7),
            unsupported_version_count: 0,
            missing_core_field_count: 0,
            failure_type_counts: HashMap::new(),
            missing_fields: Vec::new(),
            report_date: "2026-04-18".to_string(),
        };

        let core_rules = [
            MetricRule {
                name: "success_rate",
                direction: Direction::HigherIsBetter,
                warn_delta: 3.0,
                fail_delta: 5.0,
                absolute_fail: None,
            },
            MetricRule {
                name: "fallback_rate",
                direction: Direction::LowerIsBetter,
                warn_delta: 3.0,
                fail_delta: 5.0,
                absolute_fail: None,
            },
            MetricRule {
                name: "context_drop_rate",
                direction: Direction::LowerIsBetter,
                warn_delta: 3.0,
                fail_delta: 5.0,
                absolute_fail: None,
            },
            MetricRule {
                name: "state_present_rate",
                direction: Direction::HigherIsBetter,
                warn_delta: 5.0,
                fail_delta: 10.0,
                absolute_fail: None,
            },
            MetricRule {
                name: "memory_injected_rate",
                direction: Direction::HigherIsBetter,
                warn_delta: 5.0,
                fail_delta: 10.0,
                absolute_fail: None,
            },
            MetricRule {
                name: "recovery_success_rate",
                direction: Direction::HigherIsBetter,
                warn_delta: 10.0,
                fail_delta: 20.0,
                absolute_fail: Some(|v| v < 60.0),
            },
            MetricRule {
                name: "unknown_failure_rate",
                direction: Direction::LowerIsBetter,
                warn_delta: 2.0,
                fail_delta: 5.0,
                absolute_fail: Some(|v| v > 10.0),
            },
        ];

        let mut core_verdicts = Vec::new();
        for rule in &core_rules {
            core_verdicts.push(evaluate_single(
                rule.name,
                before.get(rule.name),
                after.get(rule.name),
                rule,
                false,
            ));
        }

        let l2_rules = [
            MetricRule {
                name: "tool_success_rate",
                direction: Direction::HigherIsBetter,
                warn_delta: 3.0,
                fail_delta: 5.0,
                absolute_fail: None,
            },
            MetricRule {
                name: "planning_stall_rate",
                direction: Direction::LowerIsBetter,
                warn_delta: 2.0,
                fail_delta: 5.0,
                absolute_fail: None,
            },
            MetricRule {
                name: "avg_step_count",
                direction: Direction::LowerIsBetter,
                warn_delta: 0.5,
                fail_delta: 1.0,
                absolute_fail: None,
            },
        ];

        let mut l2_verdicts = Vec::new();
        for rule in &l2_rules {
            l2_verdicts.push(evaluate_single(
                rule.name,
                before.get(rule.name),
                after.get(rule.name),
                rule,
                false,
            ));
        }

        let (overall, reasons) = compute_overall(&core_verdicts, &l2_verdicts, &before, &after);
        let report = build_compare_report(
            &before,
            &after,
            &core_verdicts,
            &l2_verdicts,
            overall,
            &reasons,
        );

        // Smoke test: report contains expected sections
        assert!(report.contains("# Trace Evaluation Comparison Report"));
        assert!(report.contains("核心指标"));
        assert!(report.contains("判定依据"));
        assert!(report.contains("后续动作建议"));
        assert!(report.contains("success_rate"));
        assert!(report.contains("2026-04-17"));
        assert!(report.contains("2026-04-18"));

        // Persistent State Update Trend section
        assert!(
            report.contains("## Persistent State Update Trend"),
            "report:\n{}",
            report
        );
        assert!(
            report.contains("| count | 2 | 5 | +3 |"),
            "report:\n{}",
            report
        );
        // rate delta = 22.7 - 10.0 = +12.7pp
        assert!(
            report.contains("| rate | 10.0% | 22.7% | +12.7pp |"),
            "report:\n{}",
            report
        );

        // Verify some verdicts
        let success_v = core_verdicts
            .iter()
            .find(|v| v.metric == "success_rate")
            .unwrap();
        assert_eq!(success_v.verdict, Verdict::Pass); // improved

        let state_v = core_verdicts
            .iter()
            .find(|v| v.metric == "state_present_rate")
            .unwrap();
        assert_eq!(state_v.verdict, Verdict::Pass); // drop 2pp < 5pp

        let recovery_v = core_verdicts
            .iter()
            .find(|v| v.metric == "recovery_success_rate")
            .unwrap();
        assert_eq!(recovery_v.verdict, Verdict::Pass); // drop 5pp < 10pp

        let step_v = l2_verdicts
            .iter()
            .find(|v| v.metric == "avg_step_count")
            .unwrap();
        assert_eq!(step_v.verdict, Verdict::Warn); // increase 0.5 step = WARN
    }

    #[test]
    fn missing_state_updated_field_compatible() {
        // 老报告不含 with persistent state updated 行，应兼容不崩
        let old_report = "# Trace Evaluation Report\n\n- generated: 2026-04-18T00:00:00Z\n- total traces: 10\n\n## Summary Statistics\n\n| metric | count | ratio |\n| --- | ---: | ---: |\n| total | 10 | 100% |\n| success | 8 | 80.0% |\n| with memory injected | 2 | 20.0% |\n| with memory dropped | 1 | 10.0% |\n| with session state | 1 | 10.0% |\n| with context pack dropped | 1 | 10.0% |\n| with llm fallback | 2 | 20.0% |\n| with failures | 2 | 20.0% |\n".to_string();

        let metrics = parse_report(&old_report).unwrap();
        assert_eq!(metrics.persistent_state_updated_count, 0);
        assert!(metrics.persistent_state_updated_rate.is_none());

        // 用该 metrics 参与 compare report 生成不应 panic
        let report = build_compare_report(
            &metrics,
            &metrics,
            &[],
            &[],
            Verdict::Pass,
            &["全 PASS".to_string()],
        );
        assert!(report.contains("Persistent State Update Trend"));
        assert!(report.contains("| count | 0 | 0 | +0 |"));
        assert!(report.contains("| rate | N/A | N/A | N/A |"));
    }

    #[test]
    fn state_updated_one_side_missing_rate_shows_na() {
        let before = CompareMetrics {
            total_runs: 20,
            persistent_state_updated_count: 2,
            persistent_state_updated_rate: None,
            ..Default::default()
        };
        let after = CompareMetrics {
            total_runs: 25,
            persistent_state_updated_count: 5,
            persistent_state_updated_rate: Some(20.0),
            ..Default::default()
        };

        let report = build_compare_report(&before, &after, &[], &[], Verdict::Pass, &[]);

        // before rate 缺失 → N/A；after rate 存在 → 20.0%
        assert!(
            report.contains("| rate | N/A | 20.0% | N/A |"),
            "report:\n{}",
            report
        );
    }

    #[test]
    fn state_updated_delta_computation_correct() {
        let before = CompareMetrics {
            total_runs: 20,
            persistent_state_updated_count: 2,
            persistent_state_updated_rate: Some(10.0),
            ..Default::default()
        };
        let after = CompareMetrics {
            total_runs: 25,
            persistent_state_updated_count: 8,
            persistent_state_updated_rate: Some(32.0),
            ..Default::default()
        };

        let report = build_compare_report(&before, &after, &[], &[], Verdict::Pass, &[]);

        // count delta = 8 - 2 = +6
        assert!(
            report.contains("| count | 2 | 8 | +6 |"),
            "report:\n{}",
            report
        );
        // rate delta = 32.0 - 10.0 = +22.0pp
        assert!(
            report.contains("| rate | 10.0% | 32.0% | +22.0pp |"),
            "report:\n{}",
            report
        );
    }

    #[test]
    fn state_updated_gate_lines_both_present() {
        let (human, machine) = build_state_updated_gate_lines(2, 5, Some(10.0), Some(22.5));
        assert!(human.contains("STATE_UPDATED=before_count=2 after_count=5"));
        assert!(human.contains("before_rate=10.0% after_rate=22.5% delta=+12.5pp"));
        assert_eq!(
            machine,
            "STATE_UPDATED_RAW=bc=2|ac=5|br=10.0|ar=22.5|d=12.5"
        );
    }

    #[test]
    fn state_updated_gate_lines_one_side_missing() {
        let (human, machine) = build_state_updated_gate_lines(2, 5, None, Some(20.0));
        assert!(human.contains("before_rate=N/A after_rate=20.0% delta=N/A"));
        assert_eq!(machine, "STATE_UPDATED_RAW=bc=2|ac=5|br=NA|ar=20.0|d=NA");
    }

    #[test]
    fn state_updated_gate_lines_both_missing() {
        let (human, machine) = build_state_updated_gate_lines(0, 0, None, None);
        assert!(human.contains("before_rate=N/A after_rate=N/A delta=N/A"));
        assert_eq!(machine, "STATE_UPDATED_RAW=bc=0|ac=0|br=NA|ar=NA|d=NA");
    }

    #[test]
    fn gate_output_contract_pass_with_reasons() {
        let before = CompareMetrics {
            persistent_state_updated_count: 2,
            persistent_state_updated_rate: Some(10.0),
            ..Default::default()
        };
        let after = CompareMetrics {
            persistent_state_updated_count: 5,
            persistent_state_updated_rate: Some(22.5),
            ..Default::default()
        };
        let lines = build_gate_output_lines(
            Verdict::Pass,
            &["全部核心指标 PASS".to_string()],
            &before,
            &after,
        );
        assert_eq!(lines.len(), 4);
        assert!(lines[0].starts_with("OVERALL=PASS"));
        assert!(lines[1].starts_with("STATE_UPDATED="));
        assert!(lines[2].starts_with("STATE_UPDATED_RAW="));
        assert!(lines[3].starts_with("REASONS=全部核心指标 PASS"));
    }

    #[test]
    fn gate_output_contract_without_reasons() {
        let before = CompareMetrics::default();
        let after = CompareMetrics::default();
        let lines = build_gate_output_lines(Verdict::Pass, &[], &before, &after);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("OVERALL=PASS"));
        assert!(lines[1].starts_with("STATE_UPDATED="));
        assert!(lines[2].starts_with("STATE_UPDATED_RAW="));
        for line in &lines {
            assert!(!line.starts_with("REASONS="));
        }
    }

    #[test]
    fn gate_output_contract_state_updated_na() {
        let before = CompareMetrics {
            persistent_state_updated_count: 0,
            persistent_state_updated_rate: None,
            ..Default::default()
        };
        let after = CompareMetrics {
            persistent_state_updated_count: 0,
            persistent_state_updated_rate: None,
            ..Default::default()
        };
        let lines = build_gate_output_lines(Verdict::Pass, &[], &before, &after);
        assert_eq!(lines.len(), 3);
        // 人类可读行含 N/A
        assert!(lines[1].contains("before_rate=N/A"));
        assert!(lines[1].contains("after_rate=N/A"));
        assert!(lines[1].contains("delta=N/A"));
        // 机器行含 NA
        assert!(lines[2].contains("br=NA"));
        assert!(lines[2].contains("ar=NA"));
        assert!(lines[2].contains("d=NA"));
    }

    #[test]
    fn gate_json_with_reasons() {
        let before = CompareMetrics {
            persistent_state_updated_count: 2,
            persistent_state_updated_rate: Some(10.0),
            ..Default::default()
        };
        let after = CompareMetrics {
            persistent_state_updated_count: 5,
            persistent_state_updated_rate: Some(22.5),
            ..Default::default()
        };
        let output = build_gate_json_output(
            Verdict::Pass,
            &["全部核心指标 PASS".to_string()],
            &before,
            &after,
        );
        assert_eq!(output.overall, "PASS");
        assert_eq!(output.reasons, vec!["全部核心指标 PASS"]);
        assert_eq!(output.state_updated.before_count, 2);
        assert_eq!(output.state_updated.after_count, 5);
        assert!((output.state_updated.before_rate.unwrap() - 10.0).abs() < 0.01);
        assert!((output.state_updated.after_rate.unwrap() - 22.5).abs() < 0.01);
        assert!((output.state_updated.delta.unwrap() - 12.5).abs() < 0.01);

        let json_str = serde_json::to_string(&output).unwrap();
        assert!(json_str.contains("\"overall\":\"PASS\""));
        assert!(json_str.contains("\"reasons\":[\"全部核心指标 PASS\"]"));
    }

    #[test]
    fn gate_json_without_reasons() {
        let before = CompareMetrics::default();
        let after = CompareMetrics::default();
        let output = build_gate_json_output(Verdict::Pass, &[], &before, &after);
        assert_eq!(output.overall, "PASS");
        assert!(output.reasons.is_empty());
        let json_str = serde_json::to_string(&output).unwrap();
        assert!(json_str.contains("\"reasons\":[]"));
    }

    #[test]
    fn gate_json_state_updated_missing_rate() {
        let before = CompareMetrics {
            persistent_state_updated_count: 0,
            persistent_state_updated_rate: None,
            ..Default::default()
        };
        let after = CompareMetrics {
            persistent_state_updated_count: 0,
            persistent_state_updated_rate: None,
            ..Default::default()
        };
        let output = build_gate_json_output(Verdict::Pass, &[], &before, &after);
        assert!(output.state_updated.before_rate.is_none());
        assert!(output.state_updated.after_rate.is_none());
        assert!(output.state_updated.delta.is_none());

        let json_str = serde_json::to_string(&output).unwrap();
        // JSON 中缺失应为 null，不是 0
        assert!(json_str.contains("\"before_rate\":null"));
        assert!(json_str.contains("\"after_rate\":null"));
        assert!(json_str.contains("\"delta\":null"));
    }

    #[test]
    fn recovery_success_rate_computation_with_mixed_attempts() {
        // 混合成功/失败的 recovery attempts，计算成功率
        let summary1 = make_summary(
            "run-1",
            vec![
                RecoveryAttemptSummary {
                    failure_kind: "transient".to_string(),
                    successful: true,
                },
                RecoveryAttemptSummary {
                    failure_kind: "transient".to_string(),
                    successful: false,
                },
                RecoveryAttemptSummary {
                    failure_kind: "low_value_observation".to_string(),
                    successful: false,
                },
            ],
        );
        let summary2 = make_summary(
            "run-2",
            vec![RecoveryAttemptSummary {
                failure_kind: "repeated_action".to_string(),
                successful: true,
            }],
        );

        let report = build_report(
            &[summary1, summary2],
            false,
            &HashSet::new(),
            None,
            &TraceLoadStats::default(),
        );
        // 4 attempts, 2 successes = 50% recovery success rate（新格式带分母与 Wilson99 下界）
        assert!(
            report.contains("| recovery_success | 2 | 50.0% (2/4), Wilson99 下界 12.1% |"),
            "report:\n{}",
            report
        );
        assert!(report.contains("| recovery_attempt_count | 4 | - |"));
        // recovery by failure type
        assert!(report.contains("| transient | 2 | 1 | 1 |"));
        assert!(report.contains("| low_value_observation | 1 | 0 | 1 |"));
        assert!(report.contains("| repeated_action | 1 | 1 | 0 |"));
    }

    #[test]
    fn report_parser_tolerates_missing_recovery_fields() {
        // 模拟旧 trace 无 recovery_attempts，只有 failures + recovery_action
        let trace = AgentTrace {
            trace_version: "agent_trace_v1".to_string(),
            run_id: "run-old".to_string(),
            started_at: "2026-04-18T00:00:00Z".to_string(),
            finished_at: None,
            duration_ms: Some(100),
            success: false,
            error: Some("error".to_string()),
            final_output: None,
            user_input: "test".to_string(),
            user_input_chars: 4,
            step_count: 2,
            llm_fallback_reason: None,
            recovery_action: Some("replan".to_string()),
            recovery_result: Some("failed".to_string()),
            memory_retrieved_count: 0,
            memory_hit_count: 0,
            memory_dropped_count: 0,
            memory_total_chars: 0,
            memory_ids: vec![],
            retriever_name: String::new(),
            retrieval_candidate_count: 0,
            retrieval_hit_count: 0,
            retrieval_latency_ms: 0,
            retrieval_mode: String::new(),
            retrieval_fallback_reason: None,
            retrieval_scores_present: false,
            persistent_state_present: false,
            persistent_state_source: None,
            persistent_state_updated: false,
            context_pack_present: false,
            context_pack_drop_reasons: vec![],
            context_pack_section_count: 0,
            context_pack_total_chars: 0,
            decisions: vec![],
            failures: vec![FailureTrace {
                step: 1,
                failure_type: "semantic".to_string(),
                message: "old failure".to_string(),
            }],
            recovery_attempts: vec![],
            llm_calls: vec![],
            tool_calls: vec![],
        };

        let summary = summarize_trace(&trace, &HashSet::new());
        assert_eq!(summary.recovery_attempt_count, 1);
        assert_eq!(summary.recovery_success_count, 0);
        assert!(summary.has_recovery_attempt);
        assert_eq!(summary.recovery_succeeded, Some(false));
        assert_eq!(summary.recovery_actions, vec!["replan"]);
        assert_eq!(summary.recovery_results, vec!["failed"]);
        assert_eq!(summary.recovery_attempt_details.len(), 1);
        assert_eq!(summary.recovery_attempt_details[0].failure_kind, "semantic");
        assert!(!summary.recovery_attempt_details[0].successful);
    }

    #[test]
    fn report_parser_tolerates_missing_retriever_fields() {
        // 模拟旧 trace：retriever 字段为空（默认）
        let trace = AgentTrace {
            trace_version: "agent_trace_v1".to_string(),
            run_id: "run-no-retriever".to_string(),
            started_at: "2026-04-18T00:00:00Z".to_string(),
            finished_at: None,
            duration_ms: Some(100),
            success: true,
            error: None,
            final_output: None,
            user_input: "test".to_string(),
            user_input_chars: 4,
            step_count: 1,
            llm_fallback_reason: None,
            recovery_action: None,
            recovery_result: None,
            memory_retrieved_count: 3,
            memory_hit_count: 2,
            memory_dropped_count: 1,
            memory_total_chars: 100,
            memory_ids: vec!["m1".to_string()],
            retriever_name: String::new(),
            retrieval_candidate_count: 0,
            retrieval_hit_count: 0,
            retrieval_latency_ms: 0,
            retrieval_mode: String::new(),
            retrieval_fallback_reason: None,
            retrieval_scores_present: false,
            persistent_state_present: false,
            persistent_state_source: None,
            persistent_state_updated: false,
            context_pack_present: false,
            context_pack_drop_reasons: vec![],
            context_pack_section_count: 0,
            context_pack_total_chars: 0,
            decisions: vec![],
            failures: vec![],
            recovery_attempts: vec![],
            llm_calls: vec![],
            tool_calls: vec![],
        };

        let summary = summarize_trace(&trace, &HashSet::new());
        assert_eq!(summary.retriever_name, "");
        assert_eq!(summary.retrieval_candidate_count, 0);
        assert_eq!(summary.retrieval_hit_count, 0);
        assert_eq!(summary.retrieval_latency_ms, 0);

        let report = build_report(
            &[summary],
            false,
            &HashSet::new(),
            None,
            &TraceLoadStats::default(),
        );
        // 空 retriever_name 应显示为 (unknown)
        assert!(report.contains("(unknown)"));
    }

    #[test]
    fn report_aggregates_metrics_by_retriever_name() {
        let s1 = TraceSummary {
            run_id: "r1".to_string(),
            started_at: "2026-04-18T00:00:00Z".to_string(),
            user_input: "a".to_string(),
            user_input_chars: 1,
            success: true,
            error_short: None,
            duration_ms: None,
            step_count: 1,
            llm_fallback: false,
            has_failures: false,
            failure_count: 0,
            failure_types: vec![],
            memory_retrieved: 0,
            memory_injected: 0,
            memory_dropped: 0,
            memory_total_chars: 0,
            retriever_name: "rule_v1".to_string(),
            retrieval_candidate_count: 10,
            retrieval_hit_count: 5,
            retrieval_latency_ms: 20,
            retrieval_mode: String::new(),
            retrieval_fallback_reason: None,
            retrieval_scores_present: false,
            state_present: false,
            context_pack_dropped: false,
            context_pack_drop_reasons: vec![],
            llm_call_count: 0,
            llm_success_count: 0,
            llm_failure_count: 0,
            tool_call_count: 0,
            tool_success_count: 0,
            tool_failure_count: 0,
            tool_error_types: vec![],
            has_recovery_attempt: false,
            recovery_attempt_count: 0,
            recovery_success_count: 0,
            recovery_succeeded: None,
            recovery_actions: vec![],
            recovery_results: vec![],
            recovery_attempt_details: vec![],
            in_baseline: false,
            is_interesting: false,
            interest_reasons: vec![],
            persistent_state_updated: false,
        };
        let s2 = TraceSummary {
            run_id: "r2".to_string(),
            started_at: "2026-04-18T00:00:00Z".to_string(),
            user_input: "b".to_string(),
            user_input_chars: 1,
            success: true,
            error_short: None,
            duration_ms: None,
            step_count: 1,
            llm_fallback: false,
            has_failures: false,
            failure_count: 0,
            failure_types: vec![],
            memory_retrieved: 0,
            memory_injected: 0,
            memory_dropped: 0,
            memory_total_chars: 0,
            retriever_name: "rule_v1".to_string(),
            retrieval_candidate_count: 8,
            retrieval_hit_count: 4,
            retrieval_latency_ms: 15,
            retrieval_mode: String::new(),
            retrieval_fallback_reason: None,
            retrieval_scores_present: false,
            state_present: false,
            context_pack_dropped: false,
            context_pack_drop_reasons: vec![],
            llm_call_count: 0,
            llm_success_count: 0,
            llm_failure_count: 0,
            tool_call_count: 0,
            tool_success_count: 0,
            tool_failure_count: 0,
            tool_error_types: vec![],
            has_recovery_attempt: false,
            recovery_attempt_count: 0,
            recovery_success_count: 0,
            recovery_succeeded: None,
            recovery_actions: vec![],
            recovery_results: vec![],
            recovery_attempt_details: vec![],
            in_baseline: false,
            is_interesting: false,
            interest_reasons: vec![],
            persistent_state_updated: false,
        };
        let s3 = TraceSummary {
            run_id: "r3".to_string(),
            started_at: "2026-04-18T00:00:00Z".to_string(),
            user_input: "c".to_string(),
            user_input_chars: 1,
            success: true,
            error_short: None,
            duration_ms: None,
            step_count: 1,
            llm_fallback: false,
            has_failures: false,
            failure_count: 0,
            failure_types: vec![],
            memory_retrieved: 0,
            memory_injected: 0,
            memory_dropped: 0,
            memory_total_chars: 0,
            retriever_name: "semantic_v1".to_string(),
            retrieval_candidate_count: 20,
            retrieval_hit_count: 8,
            retrieval_latency_ms: 50,
            retrieval_mode: String::new(),
            retrieval_fallback_reason: None,
            retrieval_scores_present: false,
            state_present: false,
            context_pack_dropped: false,
            context_pack_drop_reasons: vec![],
            llm_call_count: 0,
            llm_success_count: 0,
            llm_failure_count: 0,
            tool_call_count: 0,
            tool_success_count: 0,
            tool_failure_count: 0,
            tool_error_types: vec![],
            has_recovery_attempt: false,
            recovery_attempt_count: 0,
            recovery_success_count: 0,
            recovery_succeeded: None,
            recovery_actions: vec![],
            recovery_results: vec![],
            recovery_attempt_details: vec![],
            in_baseline: false,
            is_interesting: false,
            interest_reasons: vec![],
            persistent_state_updated: false,
        };

        let report = build_report(
            &[s1, s2, s3],
            false,
            &HashSet::new(),
            None,
            &TraceLoadStats::default(),
        );
        // rule_v1: 2 traces, avg_candidates=9.0, avg_hits=4.5, avg_latency=17.5
        assert!(
            report.contains("| rule_v1 | 2 | 9.0 | 4.5 | 17.5 |"),
            "report:\n{}",
            report
        );
        // semantic_v1: 1 trace, avg_candidates=20.0, avg_hits=8.0, avg_latency=50.0
        assert!(
            report.contains("| semantic_v1 | 1 | 20.0 | 8.0 | 50.0 |"),
            "report:\n{}",
            report
        );
    }

    // -----------------------------------------------------------------------
    // Gate mode tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_gate_all_pass_overall_pass() {
        let before = CompareMetrics::default();
        let after = CompareMetrics {
            total_runs: 25,
            ..Default::default()
        };
        let core = vec![
            SingleVerdict {
                metric: "success_rate",
                before: 80.0,
                after: 82.0,
                delta: 2.0,
                verdict: Verdict::Pass,
                note: "无退化".to_string(),
            },
            SingleVerdict {
                metric: "fallback_rate",
                before: 30.0,
                after: 28.0,
                delta: -2.0,
                verdict: Verdict::Pass,
                note: "无退化".to_string(),
            },
        ];
        let (overall, reasons) = compute_overall(&core, &[], &before, &after);
        assert_eq!(
            overall,
            Verdict::Pass,
            "全 PASS 应总体 PASS, reasons={:?}",
            reasons
        );
    }

    #[test]
    fn test_gate_core_fail_overall_fail() {
        let before = CompareMetrics::default();
        let after = CompareMetrics {
            total_runs: 25,
            ..Default::default()
        };
        let core = vec![
            SingleVerdict {
                metric: "success_rate",
                before: 80.0,
                after: 74.0,
                delta: -6.0,
                verdict: Verdict::Fail,
                note: "明显退化".to_string(),
            },
            SingleVerdict {
                metric: "fallback_rate",
                before: 30.0,
                after: 28.0,
                delta: -2.0,
                verdict: Verdict::Pass,
                note: "无退化".to_string(),
            },
        ];
        let (overall, reasons) = compute_overall(&core, &[], &before, &after);
        assert_eq!(
            overall,
            Verdict::Fail,
            "core 有 FAIL 应总体 FAIL, reasons={:?}",
            reasons
        );
    }

    #[test]
    fn test_gate_only_warn_overall_warn() {
        let before = CompareMetrics::default();
        let after = CompareMetrics {
            total_runs: 25,
            ..Default::default()
        };
        let core = vec![
            SingleVerdict {
                metric: "success_rate",
                before: 80.0,
                after: 77.0,
                delta: -3.0,
                verdict: Verdict::Warn,
                note: "退化 3pp".to_string(),
            },
            SingleVerdict {
                metric: "fallback_rate",
                before: 30.0,
                after: 28.0,
                delta: -2.0,
                verdict: Verdict::Pass,
                note: "无退化".to_string(),
            },
        ];
        let (overall, reasons) = compute_overall(&core, &[], &before, &after);
        assert_eq!(
            overall,
            Verdict::Warn,
            "仅 WARN 应总体 WARN, reasons={:?}",
            reasons
        );
    }

    #[test]
    fn test_gate_small_sample_caps_at_warn() {
        let before = CompareMetrics::default();
        let after = CompareMetrics {
            total_runs: 15, // < 20
            ..Default::default()
        };
        let core = vec![SingleVerdict {
            metric: "success_rate",
            before: 80.0,
            after: 82.0,
            delta: 2.0,
            verdict: Verdict::Pass,
            note: "无退化".to_string(),
        }];
        let (overall, reasons) = compute_overall(&core, &[], &before, &after);
        // 全 PASS 但样本不足，封顶 WARN
        assert_eq!(
            overall,
            Verdict::Warn,
            "样本不足时应封顶 WARN, reasons={:?}",
            reasons
        );
        assert!(reasons.iter().any(|r| r.contains("样本量")));
    }

    #[test]
    fn persistent_state_updated_count_and_ratio() {
        let mut s1 = make_summary("run-1", vec![]);
        s1.persistent_state_updated = true;
        s1.success = true;
        let mut s2 = make_summary("run-2", vec![]);
        s2.persistent_state_updated = true;
        s2.success = false;
        let mut s3 = make_summary("run-3", vec![]);
        s3.persistent_state_updated = false;
        s3.success = true;

        let report = build_report(
            &[s1, s2, s3],
            false,
            &HashSet::new(),
            None,
            &TraceLoadStats::default(),
        );
        assert!(
            report.contains("| with persistent state updated | 2 | 66.7% |"),
            "report:\n{}",
            report
        );
    }

    #[test]
    fn persistent_state_updated_breakdown() {
        let mut s1 = make_summary("run-1", vec![]);
        s1.persistent_state_updated = true;
        s1.success = true;
        let mut s2 = make_summary("run-2", vec![]);
        s2.persistent_state_updated = true;
        s2.success = false;
        let mut s3 = make_summary("run-3", vec![]);
        s3.persistent_state_updated = false;
        s3.success = true;
        let mut s4 = make_summary("run-4", vec![]);
        s4.persistent_state_updated = false;
        s4.success = false;

        let report = build_report(
            &[s1, s2, s3, s4],
            false,
            &HashSet::new(),
            None,
            &TraceLoadStats::default(),
        );
        // true: 2 traces, 1 success = 50.0%
        assert!(
            report.contains("| true | 2 | 1 | 50.0% |"),
            "report:\n{}",
            report
        );
        // false: 2 traces, 1 success = 50.0%
        assert!(
            report.contains("| false | 2 | 1 | 50.0% |"),
            "report:\n{}",
            report
        );
    }

    #[test]
    fn missing_persistent_state_updated_defaults_to_false() {
        // 模拟旧 trace：persistent_state_updated 字段缺失，serde(default) 应设为 false
        let trace = AgentTrace {
            trace_version: "agent_trace_v1".to_string(),
            run_id: "run-old".to_string(),
            started_at: "2026-04-18T00:00:00Z".to_string(),
            finished_at: None,
            duration_ms: Some(100),
            success: true,
            error: None,
            final_output: None,
            user_input: "test".to_string(),
            user_input_chars: 4,
            step_count: 1,
            llm_fallback_reason: None,
            recovery_action: None,
            recovery_result: None,
            memory_retrieved_count: 0,
            memory_hit_count: 0,
            memory_dropped_count: 0,
            memory_total_chars: 0,
            memory_ids: vec![],
            retriever_name: String::new(),
            retrieval_candidate_count: 0,
            retrieval_hit_count: 0,
            retrieval_latency_ms: 0,
            retrieval_mode: String::new(),
            retrieval_fallback_reason: None,
            retrieval_scores_present: false,
            persistent_state_present: false,
            persistent_state_source: None,
            persistent_state_updated: false, // 显式 false 模拟缺失字段的默认值
            context_pack_present: false,
            context_pack_drop_reasons: vec![],
            context_pack_section_count: 0,
            context_pack_total_chars: 0,
            decisions: vec![],
            failures: vec![],
            recovery_attempts: vec![],
            llm_calls: vec![],
            tool_calls: vec![],
        };

        let summary = summarize_trace(&trace, &HashSet::new());
        assert!(!summary.persistent_state_updated);
        assert!(!summary.is_interesting); // 无 failures 等，且 state_updated=false

        let report = build_report(
            &[summary],
            false,
            &HashSet::new(),
            None,
            &TraceLoadStats::default(),
        );
        assert!(
            report.contains("| with persistent state updated | 0 | 0.0% |"),
            "report:\n{}",
            report
        );
        // Breakdown 中 true=0, false=1
        assert!(
            report.contains("| true | 0 | 0 | 0.0% |"),
            "report:\n{}",
            report
        );
        assert!(
            report.contains("| false | 1 | 1 | 100.0% |"),
            "report:\n{}",
            report
        );
    }

    #[test]
    fn test_gate_baseline_drop_downgrades() {
        let before = CompareMetrics {
            baseline_run_ids: 100,
            baseline_hits: 100,
            total_runs: 30,
            ..Default::default()
        };
        let after = CompareMetrics {
            baseline_run_ids: 100,
            baseline_hits: 75, // 下降 25pp > 20pp
            total_runs: 30,
            ..Default::default()
        };
        let core = vec![SingleVerdict {
            metric: "success_rate",
            before: 80.0,
            after: 80.0,
            delta: 0.0,
            verdict: Verdict::Pass,
            note: "无退化".to_string(),
        }];
        let (overall, reasons) = compute_overall(&core, &[], &before, &after);
        // 全 PASS 但 baseline 覆盖下降 >20pp，降级一档 = WARN
        assert_eq!(
            overall,
            Verdict::Warn,
            "baseline 下降应降级, reasons={:?}",
            reasons
        );
        assert!(reasons.iter().any(|r| r.contains("baseline 覆盖率下降")));
    }

    // -----------------------------------------------------------------------
    // Runtime failure kind -> L1 taxonomy mapping
    // -----------------------------------------------------------------------

    #[test]
    fn runtime_to_l1_mapping_covers_spec_pairs() {
        // spec §2.G 明确给出的映射对：stalled/drift -> planning_stall_or_drift
        assert_eq!(
            map_failure_kind_to_l1("stalled_trajectory"),
            "planning_stall_or_drift"
        );
        assert_eq!(
            map_failure_kind_to_l1("trajectory_drift"),
            "planning_stall_or_drift"
        );
        // 映射表所有目标必须是合法 L1 名（防止表内笔误）
        for (runtime_kind, l1) in RUNTIME_FAILURE_KIND_TO_L1 {
            assert!(
                L1_FAILURE_TYPES.contains(l1),
                "映射目标 {} -> {} 不是合法 L1 名",
                runtime_kind,
                l1
            );
        }
    }

    #[test]
    fn l1_failure_types_map_to_themselves() {
        // 幂等：旧数据/合成 baseline 已是 L1 名，映射后行为不变
        for l1 in L1_FAILURE_TYPES {
            assert_eq!(map_failure_kind_to_l1(l1), *l1);
        }
    }

    #[test]
    fn unmapped_runtime_kinds_fall_back_to_unknown_failure() {
        // spec 未给出对应关系的其余 8 种运行时 kind（StepFailureKind::as_str 全集减去 stalled/drift）
        // 及任意未知字符串，统一收敛到 unknown_failure（spec §1.4 未知类型约定）
        for kind in [
            "transient",
            "expectation",
            "low_value_observation",
            "repeated_action",
            "budget_exhausted",
            "manual_intervention",
            "semantic",
            "irrecoverable",
        ] {
            assert_eq!(
                map_failure_kind_to_l1(kind),
                "unknown_failure",
                "kind={}",
                kind
            );
        }
        assert_eq!(map_failure_kind_to_l1("brand_new_kind"), "unknown_failure");
        assert_eq!(map_failure_kind_to_l1(""), "unknown_failure");
    }

    // -----------------------------------------------------------------------
    // Trace schema health（版本门禁 + 缺字段疑似漂移）
    // -----------------------------------------------------------------------

    /// 字段齐全的最小合法 trace JSON（含带 serde default 的 llm_calls/failures）。
    fn minimal_trace_json() -> serde_json::Value {
        serde_json::json!({
            "trace_version": "agent_trace_v1",
            "run_id": "run-minimal",
            "started_at": "2026-08-26T08:00:00Z",
            "success": true,
            "user_input": "hi",
            "user_input_chars": 2,
            "step_count": 1,
            "llm_calls": [],
            "failures": []
        })
    }

    fn trace_from_json(value: serde_json::Value) -> AgentTrace {
        serde_json::from_value(value).expect("测试 trace JSON 应能反序列化为 AgentTrace")
    }

    fn write_trace_file(dir: &Path, name: &str, value: serde_json::Value) {
        fs::write(
            dir.join(name),
            serde_json::to_string_pretty(&value).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn summarize_maps_runtime_failure_kinds_to_l1() {
        let mut value = minimal_trace_json();
        value["success"] = serde_json::json!(false);
        value["failures"] = serde_json::json!([
            { "step": 3, "kind": "stalled_trajectory", "detail": "no progress over 3 steps" },
            { "step": 4, "kind": "trajectory_drift", "detail": "actions diverged from plan" }
        ]);
        let trace = trace_from_json(value);
        let summary = summarize_trace(&trace, &HashSet::new());
        // 两个运行时 kind 映射到同一 L1 类，trace 内去重后只剩 1 项
        assert_eq!(
            summary.failure_types,
            vec!["planning_stall_or_drift".to_string()]
        );
    }

    #[test]
    fn summarize_keeps_l1_failure_types_idempotent() {
        // 旧合成数据直接写 L1 名：映射后行为不变
        let mut value = minimal_trace_json();
        value["failures"] =
            serde_json::json!([{ "step": 1, "kind": "tool_call_error", "detail": "tool failed" }]);
        let trace = trace_from_json(value);
        let summary = summarize_trace(&trace, &HashSet::new());
        assert_eq!(summary.failure_types, vec!["tool_call_error".to_string()]);
    }

    #[test]
    fn runtime_stall_kinds_count_into_planning_stall_rate() {
        // 真实运行时 kind（stalled_trajectory）现在能计入 planning_stall_rate（修复前恒 0）
        let mut stalled = minimal_trace_json();
        stalled["run_id"] = serde_json::json!("run-stalled");
        stalled["success"] = serde_json::json!(false);
        stalled["failures"] =
            serde_json::json!([{ "step": 3, "kind": "stalled_trajectory", "detail": "x" }]);
        let mut ok = minimal_trace_json();
        ok["run_id"] = serde_json::json!("run-ok");
        let summaries = vec![
            summarize_trace(&trace_from_json(stalled), &HashSet::new()),
            summarize_trace(&trace_from_json(ok), &HashSet::new()),
        ];
        let load_stats = TraceLoadStats::default();

        let report = build_report(&summaries, false, &HashSet::new(), None, &load_stats);
        assert!(
            report.contains("| planning_stall_or_drift | 1 |"),
            "report:\n{}",
            report
        );
        assert!(
            report.contains("| stall_or_drift hits | 1 |"),
            "report:\n{}",
            report
        );

        let metrics = parse_report(&report).unwrap();
        assert!((metrics.planning_stall_rate.unwrap() - 50.0).abs() < 0.01);

        let sidecar = build_report_sidecar(&summaries, &HashSet::new(), &load_stats);
        assert_eq!(sidecar.rates["planning_stall_rate"].hits, 1);
        assert_eq!(sidecar.failure_type_counts["planning_stall_or_drift"], 1);
    }

    #[test]
    fn load_counts_unsupported_version_without_dropping_trace() {
        let dir = temp_test_dir("unsupported_version");
        let mut ok = minimal_trace_json();
        ok["run_id"] = serde_json::json!("run-ok");
        let mut future = minimal_trace_json();
        future["run_id"] = serde_json::json!("run-future");
        future["trace_version"] = serde_json::json!("agent_trace_v9");
        write_trace_file(&dir, "a.json", ok);
        write_trace_file(&dir, "b.json", future);

        let mut stats = TraceLoadStats::default();
        let traces = load_traces_from_dir(&dir, &mut stats);
        // 不认识的版本不中断加载，但计入告警
        assert_eq!(traces.len(), 2);
        assert_eq!(stats.unsupported_version_count, 1);
        assert!(stats.missing_core_field_counts.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_counts_missing_core_fields_as_suspected_drift() {
        let dir = temp_test_dir("missing_core_fields");
        write_trace_file(&dir, "a.json", minimal_trace_json());
        // 缺 llm_calls / failures：serde default 静默补空，加载成功但应被计数
        let mut drifted = minimal_trace_json();
        drifted["run_id"] = serde_json::json!("run-drifted");
        let obj = drifted.as_object_mut().unwrap();
        obj.remove("llm_calls");
        obj.remove("failures");
        write_trace_file(&dir, "b.json", drifted);

        let mut stats = TraceLoadStats::default();
        let traces = load_traces_from_dir(&dir, &mut stats);
        assert_eq!(traces.len(), 2);
        assert_eq!(stats.unsupported_version_count, 0);
        assert_eq!(stats.missing_core_field_counts.get("llm_calls"), Some(&1));
        assert_eq!(stats.missing_core_field_counts.get("failures"), Some(&1));
        assert_eq!(stats.missing_core_field_counts.get("success"), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn report_shows_trace_schema_health_section() {
        let summary = make_summary("run-1", vec![]);
        let load_stats = TraceLoadStats {
            unsupported_version_count: 2,
            missing_core_field_counts: BTreeMap::from([("llm_calls".to_string(), 3)]),
        };

        let report = build_report(&[summary], false, &HashSet::new(), None, &load_stats);
        assert!(
            report.contains("## Trace Schema Health"),
            "report:\n{}",
            report
        );
        assert!(
            report.contains("| unsupported_version_count | 2 |"),
            "report:\n{}",
            report
        );
        assert!(
            report.contains("| missing_field:llm_calls | 3 |"),
            "report:\n{}",
            report
        );
        // 未缺失的字段也展示为 0，保证表格行稳定可解析
        assert!(
            report.contains("| missing_field:failures | 0 |"),
            "report:\n{}",
            report
        );
        // 有计数时附带疑似漂移告警行
        assert!(report.contains("schema 疑似漂移"), "report:\n{}", report);
    }

    #[test]
    fn schema_health_roundtrip_sidecar_and_markdown() {
        let summaries = sidecar_sample_summaries();
        let load_stats = TraceLoadStats {
            unsupported_version_count: 1,
            missing_core_field_counts: BTreeMap::from([("failures".to_string(), 2)]),
        };

        let sidecar = build_report_sidecar(&summaries, &HashSet::new(), &load_stats);
        assert_eq!(sidecar.unsupported_version_count, 1);
        assert_eq!(sidecar.missing_core_field_counts.get("failures"), Some(&2));

        // compare 两条消费路径（sidecar 优先 / markdown 回退）都能读出新计数
        let json = serde_json::to_string_pretty(&sidecar).unwrap();
        let from_sidecar = parse_sidecar(&json).unwrap();
        assert_eq!(from_sidecar.unsupported_version_count, 1);
        assert_eq!(from_sidecar.missing_core_field_count, 2);

        let from_markdown = parse_report(&build_report(
            &summaries,
            false,
            &HashSet::new(),
            None,
            &load_stats,
        ))
        .unwrap();
        assert_eq!(from_markdown.unsupported_version_count, 1);
        assert_eq!(from_markdown.missing_core_field_count, 2);
    }

    #[test]
    fn old_sidecar_and_markdown_without_schema_health_parse_as_zero() {
        // 旧 sidecar（无新字段）：serde default 兜底为 0，不触发 markdown 回退
        let sidecar = build_report_sidecar(
            &sidecar_sample_summaries(),
            &HashSet::new(),
            &TraceLoadStats::default(),
        );
        let mut value = serde_json::to_value(&sidecar).unwrap();
        let obj = value.as_object_mut().unwrap();
        obj.remove("unsupported_version_count");
        obj.remove("missing_core_field_counts");
        let metrics = parse_sidecar(&serde_json::to_string(&value).unwrap()).unwrap();
        assert_eq!(metrics.unsupported_version_count, 0);
        assert_eq!(metrics.missing_core_field_count, 0);

        // 旧 markdown 报告（无 Trace Schema Health 小节）：解析不受影响
        let metrics = parse_report(&minimal_report()).unwrap();
        assert_eq!(metrics.unsupported_version_count, 0);
        assert_eq!(metrics.missing_core_field_count, 0);
    }

    #[test]
    fn compare_report_shows_schema_health_observability() {
        let before = CompareMetrics {
            unsupported_version_count: 1,
            missing_core_field_count: 2,
            ..Default::default()
        };
        let after = CompareMetrics::default();
        let report = build_compare_report(&before, &after, &[], &[], Verdict::Pass, &[]);
        assert!(
            report.contains("## Trace Schema Health (observability only, not gate)"),
            "report:\n{}",
            report
        );
        assert!(
            report.contains("| unsupported_version_count | 1 | 0 |"),
            "report:\n{}",
            report
        );
        assert!(
            report.contains("| missing_core_field_count | 2 | 0 |"),
            "report:\n{}",
            report
        );
    }
}
