use reqwest::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteIntent {
    ManualContentSubmission { task_id: String, content: String },
    TaskRetryRequest { task_id: String },
    ManualTasksQuery,
    RecentTasksQuery,
    UserMemoryWrite { content: String },
    UserMemoryUseful { memory_id: String },
    UserMemorySuppress { memory_id: String },
    UserMemoriesQuery,
    ContextDebugQuery { text: Option<String> },
    DailyReportQuery { day: Option<String> },
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

    if is_user_memories_query(text) {
        return RouteIntent::UserMemoriesQuery;
    }

    if let Some(text) = parse_context_debug_query(text) {
        return RouteIntent::ContextDebugQuery { text };
    }

    if let Some(memory_id) = parse_user_memory_useful(text) {
        return RouteIntent::UserMemoryUseful { memory_id };
    }

    if let Some(memory_id) = parse_user_memory_suppress(text) {
        return RouteIntent::UserMemorySuppress { memory_id };
    }

    if let Some(content) = parse_user_memory_write(text) {
        return RouteIntent::UserMemoryWrite { content };
    }

    if let Some(day) = parse_daily_report_query(text) {
        return RouteIntent::DailyReportQuery { day };
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

fn parse_daily_report_query(input: &str) -> Option<Option<String>> {
    if matches!(input, "日报" | "今日整理" | "daily report" | "today digest") {
        return Some(None);
    }
    let rest = input
        .strip_prefix("日报 ")
        .or_else(|| input.strip_prefix("daily report "))?;
    let day = rest.trim();
    if day.is_empty() {
        return Some(None);
    }
    Some(Some(day.to_string()))
}

fn parse_user_memory_write(input: &str) -> Option<String> {
    let rest = input
        .strip_prefix("记住 ")
        .or_else(|| input.strip_prefix("记一下 "))?;
    let content = rest.trim();
    if content.is_empty() {
        return None;
    }
    Some(content.to_string())
}

fn is_recent_tasks_query(input: &str) -> bool {
    matches!(input, "最近任务" | "最新任务" | "recent tasks" | "recent")
}

fn is_user_memories_query(input: &str) -> bool {
    matches!(input, "我的记忆" | "我的偏好" | "memories" | "my memories")
}

fn parse_context_debug_query(input: &str) -> Option<Option<String>> {
    if matches!(input, "/context" | "上下文" | "context") {
        return Some(None);
    }
    let rest = input
        .strip_prefix("/context ")
        .or_else(|| input.strip_prefix("上下文 "))
        .or_else(|| input.strip_prefix("context "))?;
    let text = rest.trim();
    if text.is_empty() {
        return Some(None);
    }
    Some(Some(text.to_string()))
}

fn parse_user_memory_suppress(input: &str) -> Option<String> {
    let rest = input
        .strip_prefix("忘记 ")
        .or_else(|| input.strip_prefix("屏蔽记忆 "))
        .or_else(|| input.strip_prefix("forget "))?;
    let memory_id = rest.trim();
    if memory_id.is_empty() {
        return None;
    }
    Some(memory_id.to_string())
}

fn parse_user_memory_useful(input: &str) -> Option<String> {
    let rest = input
        .strip_prefix("有用 ")
        .or_else(|| input.strip_prefix("标记有用 "))
        .or_else(|| input.strip_prefix("useful "))?;
    let memory_id = rest.trim();
    if memory_id.is_empty() {
        return None;
    }
    Some(memory_id.to_string())
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
    fn daily_report_command_is_supported() {
        assert_eq!(
            route_text("日报"),
            RouteIntent::DailyReportQuery { day: None }
        );
        assert_eq!(
            route_text("今日整理"),
            RouteIntent::DailyReportQuery { day: None }
        );
        assert_eq!(
            route_text("日报 2026-04-10"),
            RouteIntent::DailyReportQuery {
                day: Some("2026-04-10".to_string())
            }
        );
    }

    #[test]
    fn user_memory_commands_are_supported() {
        assert_eq!(route_text("我的记忆"), RouteIntent::UserMemoriesQuery);
        assert_eq!(
            route_text("/context"),
            RouteIntent::ContextDebugQuery { text: None }
        );
        assert_eq!(
            route_text("/context 帮我总结一下"),
            RouteIntent::ContextDebugQuery {
                text: Some("帮我总结一下".to_string())
            }
        );
        assert_eq!(
            route_text("记住 我更喜欢短摘要"),
            RouteIntent::UserMemoryWrite {
                content: "我更喜欢短摘要".to_string()
            }
        );
        assert_eq!(
            route_text("有用 abc-123"),
            RouteIntent::UserMemoryUseful {
                memory_id: "abc-123".to_string()
            }
        );
    }

    #[test]
    fn user_memory_suppress_commands_are_supported() {
        assert_eq!(
            route_text("忘记 abc-123"),
            RouteIntent::UserMemorySuppress {
                memory_id: "abc-123".to_string()
            }
        );
        assert_eq!(
            route_text("屏蔽记忆 abc-123"),
            RouteIntent::UserMemorySuppress {
                memory_id: "abc-123".to_string()
            }
        );
        assert_eq!(
            route_text("forget abc-123"),
            RouteIntent::UserMemorySuppress {
                memory_id: "abc-123".to_string()
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
