crate::define_module_loggers!(pub info = log_task_store_info, warn = log_task_store_warn, error = log_task_store_error);

#[cfg(test)]
pub fn build_task_store_log_payload(
    level: &str,
    event: &str,
    fields: Vec<(&str, serde_json::Value)>,
) -> serde_json::Value {
    crate::logging::build_structured_log_payload(level, event, fields)
}

pub fn summarize_text_for_log(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut output: String = input.chars().take(max_chars).collect();
    output.push_str("...");
    output
}
