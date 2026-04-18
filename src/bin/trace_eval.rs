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
    #[serde(default)]
    failure_type: String,
    #[serde(default)]
    message: String,
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
    recovery_succeeded: Option<bool>,
    in_baseline: bool,
    is_interesting: bool,
    interest_reasons: Vec<String>,
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
            _ => {}
        }
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
        // markdown 常见格式：`run_id`
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
    let has_recovery_attempt = !trace.failures.is_empty();
    let recovery_succeeded = if has_recovery_attempt {
        Some(trace.success)
    } else {
        None
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
        recovery_succeeded,
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
    let recovery_attempts = summaries.iter().filter(|s| s.has_recovery_attempt).count();
    let recovery_successes = summaries
        .iter()
        .filter(|s| s.has_recovery_attempt && s.recovery_succeeded == Some(true))
        .count();
    let recovery_failures = summaries
        .iter()
        .filter(|s| s.has_recovery_attempt && s.recovery_succeeded == Some(false))
        .count();
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
        "| recovery_attempt_count | {} | {:.1}% |",
        recovery_attempts,
        pct(recovery_attempts, total)
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

    // Recovery by failure type
    if recovery_attempts > 0 {
        lines.push("### Recovery by Failure Type".to_string());
        lines.push(String::new());
        lines.push("| failure_type | attempt | success | failure |".to_string());
        lines.push("| --- | ---: | ---: | ---: |".to_string());
        let mut recovery_by_type: HashMap<String, (usize, usize, usize)> = HashMap::new();
        for summary in summaries {
            if !summary.has_recovery_attempt {
                continue;
            }
            let primary = summary
                .failure_types
                .first()
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            let entry = recovery_by_type.entry(primary).or_insert((0, 0, 0));
            entry.0 += 1;
            if summary.recovery_succeeded == Some(true) {
                entry.1 += 1;
            } else {
                entry.2 += 1;
            }
        }
        let mut pairs = recovery_by_type.into_iter().collect::<Vec<_>>();
        pairs.sort_by(|left, right| right.1 .0.cmp(&left.1 .0));
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
