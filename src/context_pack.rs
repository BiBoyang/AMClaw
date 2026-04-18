//! Structured context pack: single-entry rendering with traceable trim/drop reasons.
//!
//! Design goal: all prompt assembly goes through ContextPack, never scattered string
//! concatenation. Each source (runtime, session_state, memories, etc.) becomes an
//! explicit section with observable metadata.

use crate::session_summary::summarize_for_markdown;
use serde::Serialize;

pub const DEFAULT_CONTEXT_MAX_TOTAL_CHARS: usize = 2600;

/// A full context pack composed of sections, with total budget enforcement.
#[derive(Debug, Clone)]
pub struct ContextPack {
    sections: Vec<ContextSection>,
    max_total_chars: usize,
}

impl ContextPack {
    pub fn new() -> Self {
        Self {
            sections: Vec::new(),
            max_total_chars: DEFAULT_CONTEXT_MAX_TOTAL_CHARS,
        }
    }

    pub fn with_max_total_chars(mut self, max_total_chars: usize) -> Self {
        self.max_total_chars = max_total_chars;
        self
    }

    pub fn set_max_total_chars(&mut self, max_total_chars: usize) {
        self.max_total_chars = max_total_chars;
    }

    pub fn push(&mut self, section: ContextSection) {
        self.sections.push(section);
    }

    #[cfg(test)]
    pub fn section(&self, kind: ContextSectionKind) -> Option<&ContextSection> {
        self.sections.iter().find(|section| section.kind == kind)
    }

    /// Render included sections into a single prompt string.
    pub fn render(&self) -> String {
        let mut rendered = Vec::new();
        for section in &self.sections {
            if section.included {
                rendered.extend(section.lines.iter().cloned());
            }
        }
        rendered.join("\n")
    }

    pub fn total_chars(&self) -> usize {
        self.sections.iter().map(ContextSection::char_count).sum()
    }

    /// Apply total budget by dropping lowest-priority non-required sections.
    pub fn apply_total_budget(&mut self) {
        while self.total_chars() > self.max_total_chars {
            let drop_idx = self
                .sections
                .iter()
                .enumerate()
                .filter(|(_, section)| section.included && !section.policy.required)
                .min_by(|(_, left), (_, right)| {
                    left.policy
                        .priority
                        .cmp(&right.policy.priority)
                        .then_with(|| right.char_count().cmp(&left.char_count()))
                })
                .map(|(idx, _)| idx);

            let Some(drop_idx) = drop_idx else {
                break;
            };
            self.sections[drop_idx]
                .drop_from_prompt(ContextSectionChangeReason::TotalBudgetExceeded);
        }
    }

    pub fn budget_summary(&self) -> ContextBudgetSummary {
        ContextBudgetSummary {
            max_total_chars: self.max_total_chars,
            final_total_chars: self.total_chars(),
            trimmed_section_count: self
                .sections
                .iter()
                .filter(|section| section.trimmed)
                .count(),
            dropped_section_count: self
                .sections
                .iter()
                .filter(|section| !section.included)
                .count(),
        }
    }

    pub fn snapshot(&self) -> Vec<ContextSectionSnapshot> {
        self.sections
            .iter()
            .map(|section| ContextSectionSnapshot {
                kind: section.kind.as_str().to_string(),
                priority: section.policy.priority,
                max_chars: section.policy.max_chars,
                original_char_count: section.original_char_count(),
                line_count: section.line_count(),
                item_count: section.item_count(),
                char_count: section.char_count(),
                included: section.included,
                trimmed: section.trimmed,
                trim_reason: section.trim_reason.clone(),
                drop_reason: section.drop_reason.clone(),
                content: section.content_for_snapshot(),
            })
            .collect()
    }

    /// Collect all drop reasons from dropped sections.
    pub fn drop_reasons(&self) -> Vec<String> {
        self.sections
            .iter()
            .filter(|s| !s.included)
            .filter_map(|s| s.drop_reason.as_ref().map(|r| r.as_str().to_string()))
            .collect()
    }

    pub fn section_count(&self) -> usize {
        self.sections.len()
    }
}

impl Default for ContextPack {
    fn default() -> Self {
        Self::new()
    }
}

/// A single context section with its own budget and priority policy.
#[derive(Debug, Clone)]
pub struct ContextSection {
    kind: ContextSectionKind,
    lines: Vec<String>,
    policy: ContextSectionPolicy,
    original_content: String,
    trimmed: bool,
    trim_reason: Option<ContextSectionChangeReason>,
    included: bool,
    drop_reason: Option<ContextSectionChangeReason>,
}

impl ContextSection {
    pub fn new(kind: ContextSectionKind, lines: Vec<String>) -> Self {
        let original_content = lines.join("\n");
        let policy = kind.policy();
        let mut section = Self {
            kind,
            lines,
            policy,
            original_content,
            trimmed: false,
            trim_reason: None,
            included: true,
            drop_reason: None,
        };
        section.apply_section_budget();
        section
    }

    pub fn render(&self) -> String {
        self.lines.join("\n")
    }

    pub fn char_count(&self) -> usize {
        if self.included {
            self.render().chars().count()
        } else {
            0
        }
    }

    pub fn original_char_count(&self) -> usize {
        self.original_content.chars().count()
    }

    pub fn item_count(&self) -> usize {
        if self.included {
            self.lines.iter().filter(|line| !line.is_empty()).count()
        } else {
            0
        }
    }

    pub fn line_count(&self) -> usize {
        if self.included {
            self.lines.len()
        } else {
            0
        }
    }

    pub fn kind(&self) -> &ContextSectionKind {
        &self.kind
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn trimmed(&self) -> bool {
        self.trimmed
    }

    pub fn included(&self) -> bool {
        self.included
    }

    fn content_for_snapshot(&self) -> String {
        if self.included {
            self.render()
        } else {
            summarize_for_markdown(&self.original_content, 240)
        }
    }

    fn drop_from_prompt(&mut self, reason: ContextSectionChangeReason) {
        self.included = false;
        self.drop_reason = Some(reason);
    }

    fn apply_section_budget(&mut self) {
        if self.original_char_count() <= self.policy.max_chars {
            return;
        }
        self.trimmed = true;
        self.trim_reason = Some(ContextSectionChangeReason::SectionBudgetExceeded);
        self.lines =
            trim_section_lines(&self.lines, self.policy.max_chars, self.policy.pinned_lines);
    }
}

/// Per-section budget and priority policy.
#[derive(Debug, Clone, Copy)]
pub struct ContextSectionPolicy {
    pub priority: u8,
    pub max_chars: usize,
    pub pinned_lines: usize,
    pub required: bool,
}

/// Types of context sections, ordered by semantic role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextSectionKind {
    Preamble,
    CurrentIntent,
    RuntimeContext,
    SessionState,
    SessionText,
    PreviousObservations,
    LatestObservation,
    RuntimePlan,
    CurrentTask,
    RecentTasks,
    UserMemories,
    ToolDescriptions,
    ResponseContract,
}

impl ContextSectionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Preamble => "preamble",
            Self::CurrentIntent => "current_intent",
            Self::RuntimeContext => "runtime_context",
            Self::SessionState => "session_state",
            Self::SessionText => "session_text",
            Self::PreviousObservations => "previous_observations",
            Self::LatestObservation => "latest_observation",
            Self::RuntimePlan => "runtime_plan",
            Self::CurrentTask => "current_task",
            Self::RecentTasks => "recent_tasks",
            Self::UserMemories => "user_memories",
            Self::ToolDescriptions => "tool_descriptions",
            Self::ResponseContract => "response_contract",
        }
    }

    pub fn policy(&self) -> ContextSectionPolicy {
        match self {
            Self::Preamble => ContextSectionPolicy {
                priority: 100,
                max_chars: 120,
                pinned_lines: 1,
                required: true,
            },
            Self::CurrentIntent => ContextSectionPolicy {
                priority: 100,
                max_chars: 360,
                pinned_lines: 3,
                required: true,
            },
            Self::RuntimeContext => ContextSectionPolicy {
                priority: 95,
                max_chars: 520,
                pinned_lines: 10,
                required: true,
            },
            Self::SessionState => ContextSectionPolicy {
                priority: 94,
                max_chars: 560,
                pinned_lines: 4,
                required: false,
            },
            Self::SessionText => ContextSectionPolicy {
                priority: 55,
                max_chars: 460,
                pinned_lines: 6,
                required: false,
            },
            Self::PreviousObservations => ContextSectionPolicy {
                priority: 70,
                max_chars: 360,
                pinned_lines: 2,
                required: false,
            },
            Self::LatestObservation => ContextSectionPolicy {
                priority: 92,
                max_chars: 560,
                pinned_lines: 4,
                required: false,
            },
            Self::RuntimePlan => ContextSectionPolicy {
                priority: 93,
                max_chars: 520,
                pinned_lines: 4,
                required: false,
            },
            Self::CurrentTask => ContextSectionPolicy {
                priority: 94,
                max_chars: 420,
                pinned_lines: 5,
                required: false,
            },
            Self::RecentTasks => ContextSectionPolicy {
                priority: 50,
                max_chars: 300,
                pinned_lines: 2,
                required: false,
            },
            Self::UserMemories => ContextSectionPolicy {
                priority: 75,
                max_chars: 420,
                pinned_lines: 2,
                required: false,
            },
            Self::ToolDescriptions => ContextSectionPolicy {
                priority: 40,
                max_chars: 360,
                pinned_lines: 2,
                required: true,
            },
            Self::ResponseContract => ContextSectionPolicy {
                priority: 100,
                max_chars: 260,
                pinned_lines: 2,
                required: true,
            },
        }
    }
}

/// Serializable snapshot of a section for trace and preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContextSectionSnapshot {
    pub kind: String,
    pub priority: u8,
    pub max_chars: usize,
    pub original_char_count: usize,
    pub line_count: usize,
    pub item_count: usize,
    pub char_count: usize,
    pub included: bool,
    pub trimmed: bool,
    pub trim_reason: Option<ContextSectionChangeReason>,
    pub drop_reason: Option<ContextSectionChangeReason>,
    pub content: String,
}

/// Reason for section-level trim or drop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSectionChangeReason {
    SectionBudgetExceeded,
    TotalBudgetExceeded,
}

impl ContextSectionChangeReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SectionBudgetExceeded => "section_budget_exceeded",
            Self::TotalBudgetExceeded => "total_budget_exceeded",
        }
    }
}

/// Summary of overall budget usage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContextBudgetSummary {
    pub max_total_chars: usize,
    pub final_total_chars: usize,
    pub trimmed_section_count: usize,
    pub dropped_section_count: usize,
}

/// Trim lines to fit within max_chars while preserving pinned prefix lines.
pub fn trim_section_lines(lines: &[String], max_chars: usize, pinned_lines: usize) -> Vec<String> {
    let rendered = lines.join("\n");
    if rendered.chars().count() <= max_chars {
        return lines.to_vec();
    }
    if max_chars < 48 {
        return vec![summarize_for_markdown(&rendered, max_chars)];
    }

    let pinned = pinned_lines.min(lines.len());
    let prefix = lines[..pinned].to_vec();
    let prefix_rendered = prefix.join("\n");
    let prefix_chars = prefix_rendered.chars().count();
    if prefix.is_empty() || prefix_chars + 48 >= max_chars {
        return vec![summarize_for_markdown(&rendered, max_chars)];
    }

    let body = lines[pinned..].join("\n");
    let available_for_body = max_chars.saturating_sub(prefix_chars + 16);
    let body_summary = summarize_for_markdown(&body, available_for_body.max(24));
    let mut trimmed = prefix;
    trimmed.push(format!("- trimmed: {}", body_summary));
    trimmed
}

/// Render prompt string from a context pack (single-entry API).
pub fn render_prompt_from_context_pack(pack: &ContextPack) -> String {
    pack.render()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_pack_builds_with_sections() {
        let mut pack = ContextPack::new();
        pack.push(ContextSection::new(
            ContextSectionKind::Preamble,
            vec!["preamble line".to_string()],
        ));
        pack.push(ContextSection::new(
            ContextSectionKind::CurrentIntent,
            vec!["user input".to_string()],
        ));

        assert_eq!(pack.section_count(), 2);
        let rendered = render_prompt_from_context_pack(&pack);
        assert!(rendered.contains("preamble line"));
        assert!(rendered.contains("user input"));
    }

    #[test]
    fn context_pack_drop_reasons_on_budget_exceeded() {
        let mut pack = ContextPack::new();
        pack.push(ContextSection::new(
            ContextSectionKind::Preamble,
            vec!["required preamble".to_string()],
        ));
        pack.push(ContextSection::new(
            ContextSectionKind::RecentTasks,
            vec!["a very long recent tasks section that will be dropped".to_string()],
        ));
        pack.max_total_chars = 20;
        pack.apply_total_budget();

        let reasons = pack.drop_reasons();
        assert_eq!(reasons.len(), 1);
        assert_eq!(reasons[0], "total_budget_exceeded");
        assert_eq!(pack.budget_summary().dropped_section_count, 1);
    }

    #[test]
    fn context_pack_no_state_degradation() {
        // Build a minimal pack with only required sections
        let mut pack = ContextPack::new();
        pack.push(ContextSection::new(
            ContextSectionKind::Preamble,
            vec!["preamble".to_string()],
        ));
        pack.push(ContextSection::new(
            ContextSectionKind::CurrentIntent,
            vec!["intent".to_string()],
        ));
        pack.push(ContextSection::new(
            ContextSectionKind::ResponseContract,
            vec!["contract".to_string()],
        ));

        let snapshot = pack.snapshot();
        assert_eq!(snapshot.len(), 3);
        assert!(snapshot.iter().all(|s| s.included));
        assert!(snapshot.iter().all(|s| !s.trimmed));
    }

    #[test]
    fn context_pack_trim_records_reason() {
        let long_content = "x".repeat(600);
        let section = ContextSection::new(ContextSectionKind::SessionText, vec![long_content]);

        assert!(section.trimmed());
        assert!(section.char_count() <= section.policy.max_chars);
    }

    #[test]
    fn section_access_by_kind() {
        let mut pack = ContextPack::new();
        pack.push(ContextSection::new(
            ContextSectionKind::RuntimeContext,
            vec!["runtime".to_string()],
        ));

        assert!(pack.section(ContextSectionKind::RuntimeContext).is_some());
        assert!(pack.section(ContextSectionKind::SessionState).is_none());
    }
}
