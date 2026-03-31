use reqwest::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteIntent {
    ManualContentSubmission { task_id: String, content: String },
    TaskRetryRequest { task_id: String },
    ManualTasksQuery,
    RecentTasksQuery,
    TaskStatusQuery { task_id: String },
    LinkSubmission { urls: Vec<String> },
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

    if is_manual_tasks_query(text) {
        return RouteIntent::ManualTasksQuery;
    }

    if let Some((task_id, content)) = parse_manual_content_submission(text) {
        return RouteIntent::ManualContentSubmission { task_id, content };
    }

    if is_recent_tasks_query(text) {
        return RouteIntent::RecentTasksQuery;
    }

    if let Some(task_id) = parse_retry_query(text) {
        return RouteIntent::TaskRetryRequest { task_id };
    }

    if let Some(task_id) = parse_status_query(text) {
        return RouteIntent::TaskStatusQuery { task_id };
    }

    let urls = extract_urls(text);
    if !urls.is_empty() {
        return RouteIntent::LinkSubmission { urls };
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

fn parse_status_query(input: &str) -> Option<String> {
    let rest = input
        .strip_prefix("状态 ")
        .or_else(|| input.strip_prefix("status "))?;
    let task_id = rest.trim();
    if task_id.is_empty() {
        return None;
    }
    Some(task_id.to_string())
}

fn parse_manual_content_submission(input: &str) -> Option<(String, String)> {
    let rest = input.strip_prefix("补正文 ")?;
    let (task_id, content) = rest.split_once("::")?;
    let task_id = task_id.trim();
    let content = content.trim();
    if task_id.is_empty() || content.is_empty() {
        return None;
    }
    Some((task_id.to_string(), content.to_string()))
}

fn parse_retry_query(input: &str) -> Option<String> {
    let rest = input
        .strip_prefix("重试 ")
        .or_else(|| input.strip_prefix("retry "))?;
    let task_id = rest.trim();
    if task_id.is_empty() {
        return None;
    }
    Some(task_id.to_string())
}

fn is_recent_tasks_query(input: &str) -> bool {
    matches!(input, "最近任务" | "最新任务" | "recent tasks" | "recent")
}

fn is_manual_tasks_query(input: &str) -> bool {
    matches!(
        input,
        "待补录任务" | "manual tasks" | "awaiting manual input"
    )
}

fn extract_urls(input: &str) -> Vec<String> {
    let mut urls = Vec::new();
    for token in input.split_whitespace() {
        let candidate = token
            .trim_matches(is_opening_wrapper)
            .trim_end_matches(is_trailing_wrapper_or_punct)
            .trim();
        if let Some(url) = normalize_supported_url(candidate) {
            if !urls.iter().any(|v| v == &url) {
                urls.push(url);
            }
        }
    }
    urls
}

fn normalize_supported_url(input: &str) -> Option<String> {
    if input.starts_with("http://") || input.starts_with("https://") {
        return Some(input.to_string());
    }

    if input.is_empty() || input.contains('@') || input.contains("://") {
        return None;
    }

    let prefixed = format!("https://{input}");
    let parsed = Url::parse(&prefixed).ok()?;
    let host = parsed.host_str()?;
    if !looks_like_supported_host(host) {
        return None;
    }

    Some(prefixed)
}

fn looks_like_supported_host(host: &str) -> bool {
    host.contains('.')
        && host.chars().any(|ch| ch.is_ascii_alphabetic())
        && host.split('.').all(is_valid_host_label)
}

fn is_valid_host_label(label: &str) -> bool {
    !label.is_empty()
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
}

fn is_opening_wrapper(ch: char) -> bool {
    matches!(
        ch,
        '"' | '\'' | '<' | '(' | '[' | '{' | '“' | '‘' | '（' | '【' | '《'
    )
}

fn is_trailing_wrapper_or_punct(ch: char) -> bool {
    matches!(
        ch,
        '"' | '\''
            | '>'
            | ')'
            | ']'
            | '}'
            | ','
            | '.'
            | '!'
            | '?'
            | ';'
            | ':'
            | '”'
            | '’'
            | '）'
            | '】'
            | '》'
            | '，'
            | '。'
            | '！'
            | '？'
            | '；'
            | '：'
    )
}

#[cfg(test)]
mod tests {
    use super::{route_text, RouteIntent};

    #[test]
    fn url_becomes_link_submission() {
        assert_eq!(
            route_text("https://example.com"),
            RouteIntent::LinkSubmission {
                urls: vec!["https://example.com".to_string()]
            }
        );
    }

    #[test]
    fn status_command_becomes_task_query() {
        assert_eq!(
            route_text("状态 task-123"),
            RouteIntent::TaskStatusQuery {
                task_id: "task-123".to_string()
            }
        );
    }

    #[test]
    fn english_status_command_is_supported() {
        assert_eq!(
            route_text("status task-456"),
            RouteIntent::TaskStatusQuery {
                task_id: "task-456".to_string()
            }
        );
    }

    #[test]
    fn retry_command_becomes_task_retry() {
        assert_eq!(
            route_text("重试 task-123"),
            RouteIntent::TaskRetryRequest {
                task_id: "task-123".to_string()
            }
        );
    }

    #[test]
    fn recent_tasks_command_is_supported() {
        assert_eq!(route_text("最近任务"), RouteIntent::RecentTasksQuery);
    }

    #[test]
    fn manual_content_submission_is_supported() {
        assert_eq!(
            route_text("补正文 task-123 :: 这是人工补录的正文"),
            RouteIntent::ManualContentSubmission {
                task_id: "task-123".to_string(),
                content: "这是人工补录的正文".to_string()
            }
        );
    }

    #[test]
    fn manual_tasks_command_is_supported() {
        assert_eq!(route_text("待补录任务"), RouteIntent::ManualTasksQuery);
    }

    #[test]
    fn mixed_text_with_url_becomes_link_submission() {
        assert_eq!(
            route_text("看看这个 https://example.com/path?q=1"),
            RouteIntent::LinkSubmission {
                urls: vec!["https://example.com/path?q=1".to_string()]
            }
        );
    }

    #[test]
    fn trailing_punctuation_is_removed_from_url() {
        assert_eq!(
            route_text("收藏 https://example.com/abc!!"),
            RouteIntent::LinkSubmission {
                urls: vec!["https://example.com/abc".to_string()]
            }
        );
    }

    #[test]
    fn duplicate_urls_are_deduplicated() {
        assert_eq!(
            route_text("https://example.com https://example.com"),
            RouteIntent::LinkSubmission {
                urls: vec!["https://example.com".to_string()]
            }
        );
    }

    #[test]
    fn bare_domain_becomes_link_submission() {
        assert_eq!(
            route_text("mp.weixin.qq.com"),
            RouteIntent::LinkSubmission {
                urls: vec!["https://mp.weixin.qq.com".to_string()]
            }
        );
    }

    #[test]
    fn bare_domain_with_path_becomes_link_submission() {
        assert_eq!(
            route_text("看看这个 mp.weixin.qq.com/s/abc?scene=1"),
            RouteIntent::LinkSubmission {
                urls: vec!["https://mp.weixin.qq.com/s/abc?scene=1".to_string()]
            }
        );
    }

    #[test]
    fn email_like_text_is_not_treated_as_link() {
        assert_eq!(
            route_text("test@example.com"),
            RouteIntent::ChatPending {
                text: "test@example.com".to_string()
            }
        );
    }

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
