#[cfg(test)]
use serde_json::Value;

crate::define_module_loggers!(pub(crate) info = log_agent_info, warn = log_agent_warn);

#[cfg(test)]
pub(crate) fn build_agent_log_payload(
    level: &str,
    event: &str,
    fields: Vec<(&str, Value)>,
) -> Value {
    crate::logging::build_structured_log_payload(level, event, fields)
}
