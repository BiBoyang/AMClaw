use crate::tool_registry::ToolAction;

use super::ObservationKind;

pub(crate) fn normalize_optional_text(input: String) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(crate) fn resulting_source_name(action: &ToolAction) -> String {
    format!("tool:{}", action.name())
}

pub(crate) fn observation_kind_for_action(action: &ToolAction) -> Option<ObservationKind> {
    match action {
        ToolAction::Read { .. } => Some(ObservationKind::Text),
        ToolAction::Write { .. } | ToolAction::Create { .. } => Some(ObservationKind::FileMutation),
        ToolAction::GetTaskStatus { .. } => Some(ObservationKind::TaskStatus),
        ToolAction::ListRecentTasks { .. } | ToolAction::ListManualTasks { .. } => {
            Some(ObservationKind::TaskList)
        }
        ToolAction::ReadArticleArchive { .. } => Some(ObservationKind::ArchiveContent),
    }
}
