#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteIntent {
    ChatContinue { text: String },
    ChatCommit { text: String },
    ChatPending { text: String },
    Ignore,
}

pub fn route_text(input: &str) -> RouteIntent {
    let text = input.trim();
    if text.is_empty() {
        return RouteIntent::Ignore;
    }

    if text.ends_with("..") {
        return strip_suffix_and_trim(text, "..")
            .map(|text| RouteIntent::ChatContinue { text })
            .unwrap_or(RouteIntent::Ignore);
    }
    if text.ends_with("!!") {
        return strip_suffix_and_trim(text, "!!")
            .map(|text| RouteIntent::ChatCommit { text })
            .unwrap_or(RouteIntent::Ignore);
    }

    RouteIntent::ChatPending {
        text: text.to_string(),
    }
}

fn strip_suffix_and_trim(input: &str, suffix: &str) -> Option<String> {
    let stripped = input.strip_suffix(suffix)?.trim();
    if stripped.is_empty() {
        return None;
    }
    Some(stripped.to_string())
}

#[cfg(test)]
mod tests {
    use super::{route_text, RouteIntent};

    #[test]
    fn plain_text_becomes_pending() {
        assert_eq!(
            route_text("hello"),
            RouteIntent::ChatPending {
                text: "hello".to_string()
            }
        );
    }

    #[test]
    fn dot_dot_suffix_becomes_continue() {
        assert_eq!(
            route_text("hello.."),
            RouteIntent::ChatContinue {
                text: "hello".to_string()
            }
        );
    }

    #[test]
    fn bang_bang_suffix_becomes_commit() {
        assert_eq!(
            route_text("hello!!"),
            RouteIntent::ChatCommit {
                text: "hello".to_string()
            }
        );
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(
            route_text("   hello!!   "),
            RouteIntent::ChatCommit {
                text: "hello".to_string()
            }
        );
    }

    #[test]
    fn empty_text_is_ignored() {
        assert_eq!(route_text(""), RouteIntent::Ignore);
    }

    #[test]
    fn bare_continue_marker_is_ignored() {
        assert_eq!(route_text(".."), RouteIntent::Ignore);
    }

    #[test]
    fn bare_commit_marker_is_ignored() {
        assert_eq!(route_text("!!"), RouteIntent::Ignore);
    }
}
