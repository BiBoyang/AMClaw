use serde_json::Value;

pub(crate) fn log_agent_info(event: &str, fields: Vec<(&str, Value)>) {
    log_agent_event("info", event, fields);
}

pub(crate) fn log_agent_warn(event: &str, fields: Vec<(&str, Value)>) {
    log_agent_event("warn", event, fields);
}

fn log_agent_event(level: &str, event: &str, fields: Vec<(&str, Value)>) {
    crate::logging::emit_structured_log(level, event, fields);
}

#[cfg(test)]
pub(crate) fn build_agent_log_payload(
    level: &str,
    event: &str,
    fields: Vec<(&str, Value)>,
) -> Value {
    crate::logging::build_structured_log_payload(level, event, fields)
}
