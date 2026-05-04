use anyhow::{bail, Result};
use serde_json::Value;

pub(super) fn is_agent_command(text: &str) -> bool {
    let raw = text.trim();
    raw.starts_with("读文件 ")
        || raw.starts_with("创建文件 ")
        || raw.starts_with("写文件 ")
        || raw.starts_with("read ")
        || raw.starts_with("create ")
        || raw.starts_with("write ")
        || raw.starts_with("帮我运行：")
        || raw.starts_with("帮我运行:")
        || raw.starts_with("请帮我运行：")
        || raw.starts_with("请帮我运行:")
}

pub(super) fn is_llm_auth_error(err: &str) -> bool {
    err.contains("HTTP 401") || err.contains("Authentication Fails")
}

pub(super) fn sanitize_report_markdown_for_wechat(content: &str) -> String {
    content
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with("- output_path:")
                && !trimmed.starts_with("- snapshot_path:")
                && !trimmed.starts_with("- markdown_path:")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn is_poll_timeout_error(err: &anyhow::Error) -> bool {
    err.chain()
        .filter_map(|cause| cause.downcast_ref::<reqwest::Error>())
        .any(reqwest::Error::is_timeout)
}

pub(super) fn assert_ok(resp: &Value, action: &str) -> Result<()> {
    let ret = get_i64(resp, "ret").unwrap_or(0);
    let errcode = get_i64(resp, "errcode").unwrap_or(0);
    if ret != 0 || errcode != 0 {
        let errmsg = get_str(resp, "errmsg").unwrap_or_default();
        bail!("{action} 失败: ret={ret} errcode={errcode} errmsg={errmsg}");
    }
    Ok(())
}

pub(super) fn get_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

pub(super) fn get_str(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
}

pub(super) fn first_non_empty<const N: usize>(items: [Option<String>; N]) -> Option<String> {
    items.into_iter().flatten().find(|v| !v.trim().is_empty())
}

pub(super) fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<json-serialize-error>".to_string())
}

pub(super) fn log_chat_info(event: &str, fields: Vec<(&str, Value)>) {
    log_chat_event("info", event, fields);
}

pub(super) fn log_chat_warn(event: &str, fields: Vec<(&str, Value)>) {
    log_chat_event("warn", event, fields);
}

pub(super) fn log_chat_error(event: &str, fields: Vec<(&str, Value)>) {
    log_chat_event("error", event, fields);
}

pub(super) fn log_chat_event(level: &str, event: &str, fields: Vec<(&str, Value)>) {
    crate::logging::emit_structured_log(level, event, fields);
}

pub(super) fn truncate_for_log(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut out: String = input.chars().take(max_chars).collect();
    out.push_str("...");
    out
}

pub(super) fn summarize_text_for_log(input: &str, max_chars: usize) -> String {
    truncate_for_log(&input.replace('\n', "\\n"), max_chars)
}

pub(super) fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(v) => Some(v.clone()),
        Value::Number(v) => Some(v.to_string()),
        Value::Object(map) => map
            .get("id")
            .or_else(|| map.get("str"))
            .or_else(|| map.get("value"))
            .and_then(value_to_string),
        _ => None,
    }
}
