use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

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
    // flags for filtering
    is_interesting: bool,
    interest_reasons: Vec<String>,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut trace_dir = PathBuf::from("data/agent_traces");
    let mut date = None;
    let mut output_path = PathBuf::from("notes/context-memory/TRACE-EVAL-REPORT.md");
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
            "--only-interesting" => {
                only_interesting = true;
            }
            _ => {}
        }
    }

    let traces = if let Some(d) = date {
        load_traces_for_date(&trace_dir, &d)
    } else {
        load_all_traces(&trace_dir)
    };

    if traces.is_empty() {
        println!("未找到任何 trace 文件");
        return;
    }

    let summaries: Vec<TraceSummary> = traces.iter().map(summarize_trace).collect();

    let report = build_report(&summaries, only_interesting);
    fs::write(&output_path, report).expect("写入报告失败");
    println!("报告已生成: {}", output_path.display());
    println!(
        "总计 trace: {}，值得关注: {}",
        summaries.len(),
        summaries.iter().filter(|s| s.is_interesting).count()
    );
}

fn load_traces_for_date(root: &PathBuf, date: &str) -> Vec<AgentTrace> {
    let dir = root.join(date);
    load_traces_from_dir(&dir)
}

fn load_all_traces(root: &PathBuf) -> Vec<AgentTrace> {
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

fn load_traces_from_dir(dir: &PathBuf) -> Vec<AgentTrace> {
    let mut traces = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return traces;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        // skip index.jsonl
        if path.file_name().and_then(|s| s.to_str()) == Some("index.jsonl") {
            continue;
        }
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        match serde_json::from_str::<AgentTrace>(&content) {
            Ok(t) => traces.push(t),
            Err(e) => eprintln!("解析失败 {}: {}", path.display(), e),
        }
    }
    traces
}

fn summarize_trace(trace: &AgentTrace) -> TraceSummary {
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

    let failure_types: Vec<String> = trace
        .failures
        .iter()
        .map(|f| f.failure_type.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    let llm_success = trace.llm_calls.iter().filter(|c| c.success).count();
    let llm_failure = trace.llm_calls.len() - llm_success;

    let tool_success = trace.tool_calls.iter().filter(|c| c.success).count();

    TraceSummary {
        run_id: trace.run_id.clone(),
        started_at: trace.started_at.clone(),
        user_input: trace.user_input.clone(),
        user_input_chars: trace.user_input_chars,
        success: trace.success,
        error_short: trace.error.as_ref().map(|e| {
            if e.chars().count() > 80 {
                format!("{}...", e.chars().take(80).collect::<String>())
            } else {
                e.clone()
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
        is_interesting,
        interest_reasons,
    }
}

fn build_report(summaries: &[TraceSummary], only_interesting: bool) -> String {
    let mut lines = vec![
        "# Trace Evaluation Report".to_string(),
        String::new(),
        format!("- generated: {}", chrono::Utc::now().to_rfc3339()),
        format!("- total traces: {}", summaries.len()),
        format!(
            "- interesting traces: {}",
            summaries.iter().filter(|s| s.is_interesting).count()
        ),
        String::new(),
        "## Summary Statistics".to_string(),
        String::new(),
    ];

    // Overall stats
    let total = summaries.len();
    let success_count = summaries.iter().filter(|s| s.success).count();
    let with_memory = summaries.iter().filter(|s| s.memory_injected > 0).count();
    let with_dropped = summaries.iter().filter(|s| s.memory_dropped > 0).count();
    let with_state = summaries.iter().filter(|s| s.state_present).count();
    let with_ctx_drop = summaries.iter().filter(|s| s.context_pack_dropped).count();
    let with_fallback = summaries.iter().filter(|s| s.llm_fallback).count();
    let with_failures = summaries.iter().filter(|s| s.has_failures).count();

    lines.push(format!("| metric | count | ratio |"));
    lines.push(format!("| --- | ---: | ---: |"));
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

    // Per-trace detail
    lines.push("## Per-Trace Detail".to_string());
    lines.push(String::new());
    lines.push(
        "| run_id | success | steps | mem(r/i/d) | state | ctx_drop | failures | reasons | input |"
            .to_string(),
    );
    lines.push("| --- | --- | ---: | --- | --- | --- | --- | --- | --- |".to_string());

    for s in summaries {
        if only_interesting && !s.is_interesting {
            continue;
        }
        let input_short = if s.user_input.chars().count() > 40 {
            format!("{}...", s.user_input.chars().take(40).collect::<String>())
        } else {
            s.user_input.clone()
        };
        lines.push(format!(
            "| `{}` | {} | {} | {}/{}/{} | {} | {} | {} | {} | {} |",
            &s.run_id[..8.min(s.run_id.len())],
            if s.success { "✓" } else { "✗" },
            s.step_count,
            s.memory_retrieved,
            s.memory_injected,
            s.memory_dropped,
            if s.state_present { "✓" } else { "·" },
            if s.context_pack_dropped { "✓" } else { "·" },
            s.failure_count,
            s.interest_reasons.join(", "),
            input_short.replace("|", "\\|")
        ));
    }
    lines.push(String::new());

    // Interesting traces deep dive
    let interesting: Vec<_> = summaries.iter().filter(|s| s.is_interesting).collect();
    if !interesting.is_empty() {
        lines.push("## Interesting Traces Deep Dive".to_string());
        lines.push(String::new());
        for s in &interesting {
            lines.push(format!("### `{}`", s.run_id));
            lines.push(String::new());
            lines.push(format!("- **user_input**: {}", s.user_input));
            lines.push(format!("- **success**: {}", s.success));
            if let Some(ref e) = s.error_short {
                lines.push(format!("- **error**: {}", e));
            }
            lines.push(format!(
                "- **duration**: {}ms, **steps**: {}",
                s.duration_ms
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "N/A".to_string()),
                s.step_count
            ));
            lines.push(format!(
                "- **memory**: retrieved={}, injected={}, dropped={}, total_chars={}",
                s.memory_retrieved, s.memory_injected, s.memory_dropped, s.memory_total_chars
            ));
            lines.push(format!("- **session_state**: {}", s.state_present));
            lines.push(format!(
                "- **context_pack**: dropped={}, reasons={:?}",
                s.context_pack_dropped, s.context_pack_drop_reasons
            ));
            lines.push(format!(
                "- **llm_calls**: total={}, success={}, failed={}",
                s.llm_call_count, s.llm_success_count, s.llm_failure_count
            ));
            lines.push(format!(
                "- **tool_calls**: total={}, success={}",
                s.tool_call_count, s.tool_success_count
            ));
            if !s.failure_types.is_empty() {
                lines.push(format!(
                    "- **failure_types**: {}",
                    s.failure_types.join(", ")
                ));
            }
            lines.push(format!(
                "- **interest_reasons**: {}",
                s.interest_reasons.join(", ")
            ));
            lines.push(String::new());
        }
    }

    // Failure taxonomy annotation template
    lines.push("## Failure Taxonomy Annotation Template".to_string());
    lines.push(String::new());
    lines.push("对以上 interesting traces 进行人工评审时，可按下表标注：".to_string());
    lines.push(String::new());
    lines.push("| run_id | primary_failure | severity | notes |".to_string());
    lines.push("| --- | --- | --- | --- |".to_string());

    for s in interesting {
        lines.push(format!(
            "| `{}` | (待填) | (low/mid/high) | (待填) |",
            &s.run_id[..8.min(s.run_id.len())]
        ));
    }
    lines.push(String::new());

    lines.push("### Failure Taxonomy".to_string());
    lines.push(String::new());
    lines.push("- `forgot_known_fact`: 系统明知但本次未使用".to_string());
    lines.push("- `missed_retrieval`: 应该检索到记忆但没检索到".to_string());
    lines.push("- `wrong_retrieval`: 检索到了不相关记忆".to_string());
    lines.push("- `overcompressed_summary`: session summary 丢失了关键信息".to_string());
    lines.push("- `state_drift`: session state 与实际情况不一致".to_string());
    lines.push("- `repeated_work`: 重复执行了已完成的步骤".to_string());
    lines.push("- `llm_error`: LLM 调用失败或返回无效".to_string());
    lines.push("- `tool_error`: 工具执行失败".to_string());
    lines.push("- `other`: 其他".to_string());
    lines.push(String::new());

    lines.push("---".to_string());
    lines.push("*本报告由 trace_eval 自动生成，人工评审后请将标注结果补充到上表中。*".to_string());
    lines.push(String::new());

    lines.join("\n")
}

fn pct(part: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        (part as f64 / total as f64) * 100.0
    }
}
