crate::define_module_loggers!(pub(crate) info = log_pipeline_info, warn = log_pipeline_warn, error = log_pipeline_error);

#[cfg(test)]
pub(crate) fn build_pipeline_log_payload(
    level: &str,
    event: &str,
    fields: Vec<(&str, serde_json::Value)>,
) -> serde_json::Value {
    crate::logging::build_structured_log_payload(level, event, fields)
}
