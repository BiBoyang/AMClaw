use serde_json::Value;

pub fn log_task_store_info(event: &str, fields: Vec<(&str, Value)>) {
    log_task_store_event("info", event, fields);
}

pub fn log_task_store_warn(event: &str, fields: Vec<(&str, Value)>) {
    log_task_store_event("warn", event, fields);
}

pub fn log_task_store_error(event: &str, fields: Vec<(&str, Value)>) {
    log_task_store_event("error", event, fields);
}

fn log_task_store_event(level: &str, event: &str, fields: Vec<(&str, Value)>) {
    crate::logging::emit_structured_log(level, event, fields);
}

#[cfg(test)]
pub fn build_task_store_log_payload(level: &str, event: &str, fields: Vec<(&str, Value)>) -> Value {
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
