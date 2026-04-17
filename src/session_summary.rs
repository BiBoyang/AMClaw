//! Session text summary strategies: semantic vs truncate.
//! Exposed for reuse by both runtime and offline evaluation.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSummaryStrategy {
    Semantic,
    Truncate,
}

impl SessionSummaryStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Semantic => "semantic",
            Self::Truncate => "truncate",
        }
    }

    pub fn from_config_text(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "truncate" => Self::Truncate,
            _ => Self::Semantic,
        }
    }
}

pub const SESSION_TEXT_FULL_MAX_CHARS: usize = 220;
pub const SESSION_TEXT_SUMMARY_MAX_CHARS: usize = 140;
pub const SESSION_TEXT_RECENT_TAIL_CHARS: usize = 120;

/// Truncate strategy: keep a head chunk and append a truncation notice.
pub fn summarize_for_markdown(input: &str, max_chars: usize) -> String {
    let count = input.chars().count();
    if count <= max_chars {
        return input.to_string();
    }
    let notice = format!("\n\n...[truncated, total_chars={count}]");
    let notice_chars = notice.chars().count();
    if notice_chars >= max_chars {
        return input.chars().take(max_chars).collect();
    }
    let head_chars = max_chars
        .saturating_sub(80)
        .max(40)
        .min(max_chars - notice_chars);
    let mut text: String = input.chars().take(head_chars).collect();
    text.push_str(&notice);
    text
}

/// Build the full session text section lines using the chosen strategy.
pub fn build_session_text_section_lines(
    session_text: &str,
    session_summary_strategy: SessionSummaryStrategy,
) -> Vec<String> {
    let total_chars = session_text.chars().count();
    let mut lines = vec![
        String::new(),
        "## Session Text".to_string(),
        format!("- total_chars: {total_chars}"),
    ];
    if total_chars <= SESSION_TEXT_FULL_MAX_CHARS {
        lines.push("- mode: full".to_string());
        lines.push("```text".to_string());
        lines.push(session_text.to_string());
        lines.push("```".to_string());
        return lines;
    }

    lines.push("- mode: boundary_compaction".to_string());
    lines.push(format!(
        "- summary_strategy: {}",
        session_summary_strategy.as_str()
    ));
    lines.push("- summary_compact:".to_string());
    lines.push("```text".to_string());
    let summary_compact = match session_summary_strategy {
        SessionSummaryStrategy::Semantic => {
            summarize_session_text_semantic(session_text, SESSION_TEXT_SUMMARY_MAX_CHARS)
        }
        SessionSummaryStrategy::Truncate => {
            summarize_for_markdown(session_text, SESSION_TEXT_SUMMARY_MAX_CHARS)
        }
    };
    lines.push(summary_compact);
    lines.push("```".to_string());
    lines.push("- recent_tail:".to_string());
    lines.push("```text".to_string());
    lines.push(session_recent_tail_with_notice(
        session_text,
        SESSION_TEXT_RECENT_TAIL_CHARS,
    ));
    lines.push("```".to_string());
    lines
}

pub fn session_recent_tail_with_notice(input: &str, max_chars: usize) -> String {
    let total_chars = input.chars().count();
    if total_chars <= max_chars {
        return input.to_string();
    }
    let omitted_chars = total_chars.saturating_sub(max_chars);
    let tail: String = input.chars().skip(omitted_chars).collect();
    format!("...[{omitted_chars} chars omitted]\n{tail}")
}

/// Semantic strategy: score and select high-value segments.
pub fn summarize_session_text_semantic(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let segments = split_session_segments(input);
    if segments.is_empty() {
        return summarize_for_markdown(input, max_chars);
    }

    let mut ranked = segments
        .iter()
        .enumerate()
        .map(|(idx, segment)| {
            (
                idx,
                score_session_segment(segment, idx, segments.len()),
                segment.chars().count(),
            )
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| right.0.cmp(&left.0))
            .then_with(|| left.2.cmp(&right.2))
    });

    let mut selected = std::collections::BTreeSet::new();
    let last_index = segments.len() - 1;
    selected.insert(last_index);
    let mut budget_used = segments[last_index].chars().count();
    for (idx, _, seg_chars) in ranked {
        if selected.contains(&idx) {
            continue;
        }
        let join_cost = usize::from(!selected.is_empty());
        if budget_used + join_cost + seg_chars > max_chars {
            continue;
        }
        selected.insert(idx);
        budget_used += join_cost + seg_chars;
    }

    let mut summary = selected
        .iter()
        .map(|idx| segments[*idx].clone())
        .collect::<Vec<_>>()
        .join("\n");
    if summary.chars().count() > max_chars {
        summary = summarize_for_markdown(&summary, max_chars);
    }
    if summary.trim().is_empty() {
        summarize_for_markdown(input, max_chars)
    } else {
        summary
    }
}

fn split_session_segments(input: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    for ch in input.chars() {
        current.push(ch);
        if matches!(ch, '\n' | '。' | '！' | '？' | '!' | '?') {
            let trimmed = current.trim();
            if !trimmed.is_empty() {
                segments.push(trimmed.to_string());
            }
            current.clear();
        }
    }
    let tail = current.trim();
    if !tail.is_empty() {
        segments.push(tail.to_string());
    }
    segments
}

fn score_session_segment(segment: &str, idx: usize, total: usize) -> i32 {
    let mut score = 1_i32;
    let normalized = segment.to_ascii_lowercase();
    let keyword_hits = [
        "目标",
        "下一步",
        "todo",
        "待办",
        "问题",
        "卡住",
        "失败",
        "错误",
        "风险",
        "结论",
        "决定",
        "计划",
        "完成",
        "next",
        "issue",
        "blocker",
        "should",
        "must",
    ]
    .iter()
    .filter(|keyword| segment.contains(**keyword) || normalized.contains(**keyword))
    .count() as i32;
    score += keyword_hits * 3;

    if segment.starts_with("- ")
        || segment.starts_with("* ")
        || segment
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
    {
        score += 2;
    }

    let recency_bonus = if total <= 1 {
        3
    } else {
        (idx as i32 * 3) / (total as i32 - 1)
    };
    score + recency_bonus
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_summary_keeps_keywords_and_last_segment() {
        let text = "先做一些普通描述。\n下一步: 修复关键问题并输出结论。\n最后收尾。";
        let summary = summarize_session_text_semantic(text, 80);
        assert!(summary.contains("下一步"));
        assert!(summary.contains("最后收尾"));
    }

    #[test]
    fn truncate_summary_adds_notice() {
        let text = "a".repeat(300);
        let summary = summarize_for_markdown(&text, 140);
        assert!(summary.contains("...[truncated"));
    }

    #[test]
    fn build_session_text_uses_full_for_short_input() {
        let lines = build_session_text_section_lines("hello", SessionSummaryStrategy::Semantic);
        assert!(lines.iter().any(|l| l.contains("mode: full")));
    }

    #[test]
    fn build_session_text_uses_boundary_for_long_input() {
        let text = "x".repeat(500);
        let lines = build_session_text_section_lines(&text, SessionSummaryStrategy::Semantic);
        assert!(lines
            .iter()
            .any(|l| l.contains("mode: boundary_compaction")));
        assert!(lines
            .iter()
            .any(|l| l.contains("summary_strategy: semantic")));
    }
}
