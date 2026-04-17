use amclaw::session_summary::{
    session_recent_tail_with_notice, summarize_for_markdown, summarize_session_text_semantic,
    SessionSummaryStrategy,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
struct EvalSample {
    id: String,
    text: String,
    must_keep: Vec<String>,
    tail_must_keep: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct EvalResult {
    id: String,
    original_chars: usize,
    summary_chars: usize,
    compression_ratio: f64,
    must_keep_hit_rate: f64,
    tail_keep_hit_rate: f64,
    summary: String,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut input_path = PathBuf::from("notes/context-memory/eval_samples.jsonl");
    let mut output_path = PathBuf::from("notes/context-memory/SESSION-SUMMARY-EVAL-2026-04-17.md");
    let mut max_chars: usize = 140;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input" => {
                if let Some(v) = args.next() {
                    input_path = PathBuf::from(v);
                }
            }
            "--output" => {
                if let Some(v) = args.next() {
                    output_path = PathBuf::from(v);
                }
            }
            "--max-chars" => {
                if let Some(v) = args.next() {
                    if let Ok(n) = v.parse() {
                        max_chars = n;
                    }
                }
            }
            _ => {}
        }
    }

    let content = fs::read_to_string(&input_path).expect("读取样本文件失败");
    let mut samples: Vec<EvalSample> = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let sample: EvalSample = serde_json::from_str(line).expect("解析样本失败");
        samples.push(sample);
    }

    let semantic_results: Vec<EvalResult> = samples
        .iter()
        .map(|s| evaluate_sample(s, SessionSummaryStrategy::Semantic, max_chars))
        .collect();
    let truncate_results: Vec<EvalResult> = samples
        .iter()
        .map(|s| evaluate_sample(s, SessionSummaryStrategy::Truncate, max_chars))
        .collect();

    let report = build_markdown_report(&samples, &semantic_results, &truncate_results, max_chars);
    fs::write(&output_path, report).expect("写入报告失败");
    println!("报告已生成: {}", output_path.display());
}

fn evaluate_sample(
    sample: &EvalSample,
    strategy: SessionSummaryStrategy,
    max_chars: usize,
) -> EvalResult {
    let summary_compact = match strategy {
        SessionSummaryStrategy::Semantic => {
            summarize_session_text_semantic(&sample.text, max_chars)
        }
        SessionSummaryStrategy::Truncate => summarize_for_markdown(&sample.text, max_chars),
    };
    let original_chars = sample.text.chars().count();
    let summary_chars = summary_compact.chars().count();
    let compression_ratio = if original_chars == 0 {
        0.0
    } else {
        summary_chars as f64 / original_chars as f64
    };

    let must_keep_hits = sample
        .must_keep
        .iter()
        .filter(|k| summary_compact.contains(k.as_str()))
        .count();
    let must_keep_hit_rate = if sample.must_keep.is_empty() {
        1.0
    } else {
        must_keep_hits as f64 / sample.must_keep.len() as f64
    };

    // 匹配运行时语义：boundary_compaction 下 planner 看到的是 summary_compact + recent_tail
    let recent_tail = session_recent_tail_with_notice(
        &sample.text,
        amclaw::session_summary::SESSION_TEXT_RECENT_TAIL_CHARS,
    );
    let visible_for_tail = if original_chars > amclaw::session_summary::SESSION_TEXT_FULL_MAX_CHARS
    {
        format!("{}\n{}", summary_compact, recent_tail)
    } else {
        sample.text.clone()
    };
    let tail_keep_hits = sample
        .tail_must_keep
        .iter()
        .filter(|k| visible_for_tail.contains(k.as_str()))
        .count();
    let tail_keep_hit_rate = if sample.tail_must_keep.is_empty() {
        1.0
    } else {
        tail_keep_hits as f64 / sample.tail_must_keep.len() as f64
    };

    EvalResult {
        id: sample.id.clone(),
        original_chars,
        summary_chars,
        compression_ratio,
        must_keep_hit_rate,
        tail_keep_hit_rate,
        summary: summary_compact,
    }
}

fn build_markdown_report(
    samples: &[EvalSample],
    semantic: &[EvalResult],
    truncate: &[EvalResult],
    max_chars: usize,
) -> String {
    let mut lines = vec![
        "# Session Summary Strategy Evaluation".to_string(),
        String::new(),
        format!("- date: 2026-04-17"),
        format!("- samples: {}", samples.len()),
        format!("- max_chars: {}", max_chars),
        String::new(),
        "## Per-Sample Comparison".to_string(),
        String::new(),
        "| id | orig | strat | sum_chars | compress | must_keep | tail_keep | summary |"
            .to_string(),
        "| --- | ---: | --- | ---: | ---: | ---: | ---: | --- |".to_string(),
    ];

    for i in 0..samples.len() {
        let s = &semantic[i];
        let t = &truncate[i];
        lines.push(format_result_row(&s.id, s, "semantic"));
        lines.push(format_result_row(&t.id, t, "truncate"));
    }

    lines.push(String::new());
    lines.push("## Overall Averages".to_string());
    lines.push(String::new());
    lines.push("| strategy | avg_compress | avg_must_keep | avg_tail_keep |".to_string());
    lines.push("| --- | ---: | ---: | ---: |".to_string());
    lines.push(format_overall_row("semantic", semantic));
    lines.push(format_overall_row("truncate", truncate));

    lines.push(String::new());
    lines.push("## Conclusion".to_string());
    lines.push(String::new());
    let (sem_must, sem_tail) = avg_rates(semantic);
    let (trunc_must, trunc_tail) = avg_rates(truncate);
    if sem_must >= trunc_must && sem_tail >= trunc_tail {
        lines.push(format!(
            "**semantic** 在关键句保留（must_keep {:.2}）和尾部保留（tail_keep {:.2}）上均优于或等于 truncate，推荐继续使用 semantic 策略。",
            sem_must, sem_tail
        ));
    } else if trunc_must > sem_must && trunc_tail > sem_tail {
        lines.push(format!(
            "**truncate** 在关键句保留（must_keep {:.2}）和尾部保留（tail_keep {:.2}）上均优于 semantic，可考虑切换策略。",
            trunc_must, trunc_tail
        ));
    } else {
        lines.push(format!(
            "**semantic** must_keep={:.2}, tail_keep={:.2}; **truncate** must_keep={:.2}, tail_keep={:.2}。两者各有优劣，建议按实际场景选择。",
            sem_must, sem_tail, trunc_must, trunc_tail
        ));
    }
    lines.push(String::new());

    lines.join("\n")
}

fn format_result_row(id: &str, r: &EvalResult, strategy: &str) -> String {
    let summary_escaped = r.summary.replace("|", "\\|").replace("\n", " ");
    let summary_truncated = if summary_escaped.chars().count() > 60 {
        format!(
            "{}...",
            summary_escaped.chars().take(60).collect::<String>()
        )
    } else {
        summary_escaped
    };
    format!(
        "| {} | {} | {} | {} | {:.2} | {:.2} | {:.2} | {} |",
        id,
        r.original_chars,
        strategy,
        r.summary_chars,
        r.compression_ratio,
        r.must_keep_hit_rate,
        r.tail_keep_hit_rate,
        summary_truncated
    )
}

fn format_overall_row(strategy: &str, results: &[EvalResult]) -> String {
    let avg_compress =
        results.iter().map(|r| r.compression_ratio).sum::<f64>() / results.len() as f64;
    let avg_must = results.iter().map(|r| r.must_keep_hit_rate).sum::<f64>() / results.len() as f64;
    let avg_tail = results.iter().map(|r| r.tail_keep_hit_rate).sum::<f64>() / results.len() as f64;
    format!(
        "| {} | {:.2} | {:.2} | {:.2} |",
        strategy, avg_compress, avg_must, avg_tail
    )
}

fn avg_rates(results: &[EvalResult]) -> (f64, f64) {
    let must = results.iter().map(|r| r.must_keep_hit_rate).sum::<f64>() / results.len() as f64;
    let tail = results.iter().map(|r| r.tail_keep_hit_rate).sum::<f64>() / results.len() as f64;
    (must, tail)
}
