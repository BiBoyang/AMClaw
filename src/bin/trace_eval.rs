use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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
    #[serde(default)]
    outcome: String,
    #[serde(default)]
    successful: bool,
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
        run_compare_mode(&before, &after, compare_output.as_ref());
        return;
    }

    let traces = if let Some(ref d) = date {
        load_traces_for_date(&trace_dir, d)
    } else {
        load_all_traces(&trace_dir)
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
    );
    fs::write(&output_path, report).expect("写入报告失败");
    println!("报告已生成: {}", output_path.display());
    println!(
        "总计 trace: {}，值得关注: {}",
        summaries.len(),
        summaries.iter().filter(|s| s.is_interesting).count()
    );
}

fn load_traces_for_date(root: &Path, date: &str) -> Vec<AgentTrace> {
    let dir = root.join(date);
    load_traces_from_dir(&dir)
}

fn load_all_traces(root: &Path) -> Vec<AgentTrace> {
    let mut traces = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return traces;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            traces.extend(load_traces_from_dir(&path));
        }
    }
    traces
}

fn load_traces_from_dir(dir: &Path) -> Vec<AgentTrace> {
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
        match serde_json::from_str::<AgentTrace>(&content) {
            Ok(trace) => traces.push(trace),
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

    let is_interesting = !interest_reasons.is_empty();
    let mut seen_types = std::collections::HashSet::new();
    let failure_types: Vec<String> = trace
        .failures
        .iter()
        .map(|failure| failure.failure_type.clone())
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
    }
}

fn build_report(
    summaries: &[TraceSummary],
    only_interesting: bool,
    baseline_run_ids: &HashSet<String>,
    baseline_path: Option<&Path>,
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

    lines.push("| metric | count | ratio |".to_string());
    lines.push("| --- | ---: | ---: |".to_string());
    lines.push(format!("| total | {} | 100% |", total));
    lines.push(format!(
        "| success | {} | {:.1}% |",
        success_count,
        pct(success_count, total)
    ));
    lines.push(format!(
        "| with memory injected | {} | {:.1}% |",
        with_memory,
        pct(with_memory, total)
    ));
    lines.push(format!(
        "| with memory dropped | {} | {:.1}% |",
        with_dropped,
        pct(with_dropped, total)
    ));
    lines.push(format!(
        "| with session state | {} | {:.1}% |",
        with_state,
        pct(with_state, total)
    ));
    lines.push(format!(
        "| with context pack dropped | {} | {:.1}% |",
        with_ctx_drop,
        pct(with_ctx_drop, total)
    ));
    lines.push(format!(
        "| with llm fallback | {} | {:.1}% |",
        with_fallback,
        pct(with_fallback, total)
    ));
    lines.push(format!(
        "| with failures | {} | {:.1}% |",
        with_failures,
        pct(with_failures, total)
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
            lines.push(format!(
                "| {} | {} | {:.1}% |",
                failure_type,
                count,
                pct(count, total)
            ));
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
    let tool_success_rate = if total_tool_calls > 0 {
        (total_tool_success as f64 / total_tool_calls as f64) * 100.0
    } else {
        0.0
    };
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
        "| tool success | {} | {:.1}% |",
        total_tool_success, tool_success_rate
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
    dist_pairs.sort_by(|left, right| left.0.cmp(&right.0));
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
    lines.push(format!("| stall_or_drift hits | {} |", stall_drift_count));
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
    let recovery_success_rate = if recovery_attempts > 0 {
        (recovery_successes as f64 / recovery_attempts as f64) * 100.0
    } else {
        0.0
    };
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
        "| recovery_success | {} | {:.1}% |",
        recovery_successes, recovery_success_rate
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
        "| run_id | success | baseline | steps | mem(r/i/d) | state | ctx_drop | failures | reasons | input |"
            .to_string(),
    );
    lines.push("| --- | --- | --- | ---: | --- | --- | --- | --- | --- | --- |".to_string());

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
            "| `{}` | {} | {} | {} | {}/{}/{} | {} | {} | {} | {} | {} |",
            &summary.run_id[..8.min(summary.run_id.len())],
            if summary.success { "✓" } else { "✗" },
            if summary.in_baseline { "✓" } else { "·" },
            summary.step_count,
            summary.memory_retrieved,
            summary.memory_injected,
            summary.memory_dropped,
            if summary.state_present { "✓" } else { "·" },
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
    // Denominator info (for scenario B)
    recovery_attempt_count: usize,
    total_tool_calls: usize,
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
                        "success" => metrics.success_rate = ratio,
                        "with llm fallback" => metrics.fallback_rate = ratio,
                        "with context pack dropped" => metrics.context_drop_rate = ratio,
                        "with session state" => metrics.state_present_rate = ratio,
                        "with memory injected" => metrics.memory_injected_rate = ratio,
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
                    } else if name == "recovery_attempt_count" {
                        metrics.recovery_attempt_count = extract_usize_from_cell(&row[1]);
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
    let s = s.trim_end_matches('%');
    s.parse().ok()
}

fn extract_usize_from_cell(s: &str) -> usize {
    s.trim().parse().unwrap_or(0)
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

fn run_compare_mode(before_path: &Path, after_path: &Path, output_path: Option<&PathBuf>) {
    let before_content = match fs::read_to_string(before_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("读取 before 报告失败 {}: {}", before_path.display(), e);
            std::process::exit(1);
        }
    };
    let after_content = match fs::read_to_string(after_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("读取 after 报告失败 {}: {}", after_path.display(), e);
            std::process::exit(1);
        }
    };

    let before = match parse_report(&before_content) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("解析 before 报告失败: {}", e);
            std::process::exit(1);
        }
    };
    let after = match parse_report(&after_content) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("解析 after 报告失败: {}", e);
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
        }
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

        let report = build_report(&[summary1, summary2], false, &HashSet::new(), None);

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
        let after = CompareMetrics::default();
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
        let after = CompareMetrics::default();
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
        let after = CompareMetrics::default();
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
}
