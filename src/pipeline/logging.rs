use serde_json::Value;

pub(crate) fn log_pipeline_info(event: &str, fields: Vec<(&str, Value)>) {
    log_pipeline_event("info", event, fields);
}

pub(crate) fn log_pipeline_warn(event: &str, fields: Vec<(&str, Value)>) {
    log_pipeline_event("warn", event, fields);
}

pub(crate) fn log_pipeline_error(event: &str, fields: Vec<(&str, Value)>) {
    log_pipeline_event("error", event, fields);
}

fn log_pipeline_event(level: &str, event: &str, fields: Vec<(&str, Value)>) {
    crate::logging::emit_structured_log(level, event, fields);
}

#[cfg(test)]
pub(crate) fn build_pipeline_log_payload(
    level: &str,
    event: &str,
    fields: Vec<(&str, Value)>,
) -> Value {
    crate::logging::build_structured_log_payload(level, event, fields)
}
