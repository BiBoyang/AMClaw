use chrono::Utc;
use chrono_tz::Asia::Shanghai;
use serde_json::{json, Map, Value};

/// Generates module-level structured-logging wrappers that delegate to
/// [`emit_structured_log`].  Each arm defines one or two functions; the
/// caller picks the combination it actually needs so unused variants
/// aren't generated.
///
/// # Examples
///
/// ```ignore
/// define_module_loggers!(pub(crate) info = log_foo_info, warn = log_foo_warn, error = log_foo_error);
/// define_module_loggers!(pub(crate) info = log_bar_info, warn = log_bar_warn);
/// define_module_loggers!(info = log_baz_info, error = log_baz_error);
/// ```
#[macro_export]
macro_rules! define_module_loggers {
    // info + warn + error
    ($vis:vis info = $info:ident, warn = $warn:ident, error = $error:ident $(,)?) => {
        $vis fn $info(event: &str, fields: Vec<(&str, serde_json::Value)>) {
            $crate::logging::emit_structured_log("info", event, fields);
        }
        $vis fn $warn(event: &str, fields: Vec<(&str, serde_json::Value)>) {
            $crate::logging::emit_structured_log("warn", event, fields);
        }
        $vis fn $error(event: &str, fields: Vec<(&str, serde_json::Value)>) {
            $crate::logging::emit_structured_log("error", event, fields);
        }
    };
    // info + warn only
    ($vis:vis info = $info:ident, warn = $warn:ident $(,)?) => {
        $vis fn $info(event: &str, fields: Vec<(&str, serde_json::Value)>) {
            $crate::logging::emit_structured_log("info", event, fields);
        }
        $vis fn $warn(event: &str, fields: Vec<(&str, serde_json::Value)>) {
            $crate::logging::emit_structured_log("warn", event, fields);
        }
    };
    // info + error only
    ($vis:vis info = $info:ident, error = $error:ident $(,)?) => {
        $vis fn $info(event: &str, fields: Vec<(&str, serde_json::Value)>) {
            $crate::logging::emit_structured_log("info", event, fields);
        }
        $vis fn $error(event: &str, fields: Vec<(&str, serde_json::Value)>) {
            $crate::logging::emit_structured_log("error", event, fields);
        }
    };
}

pub(crate) fn build_structured_log_payload(
    level: &str,
    event: &str,
    fields: Vec<(&str, Value)>,
) -> Value {
    let mut payload = Map::new();
    payload.insert(
        "ts".to_string(),
        json!(Utc::now().with_timezone(&Shanghai).to_rfc3339()),
    );
    payload.insert("level".to_string(), json!(level));
    payload.insert("event".to_string(), json!(event));

    for (key, value) in fields {
        if !value.is_null() {
            payload.insert(key.to_string(), value);
        }
    }

    Value::Object(payload)
}

pub fn emit_structured_log(level: &str, event: &str, fields: Vec<(&str, Value)>) {
    let line = build_structured_log_payload(level, event, fields).to_string();
    match level {
        "error" => eprintln!("{line}"),
        _ => println!("{line}"),
    }
}

#[cfg(test)]
mod tests {
    use super::build_structured_log_payload;
    use serde_json::{json, Value};

    #[test]
    fn structured_log_payload_has_core_fields_and_drops_nulls() {
        let payload = build_structured_log_payload(
            "info",
            "test_event",
            vec![("task_id", json!("task-1")), ("detail", Value::Null)],
        );

        assert_eq!(payload["level"], "info");
        assert_eq!(payload["event"], "test_event");
        assert_eq!(payload["task_id"], "task-1");
        assert!(payload.get("ts").is_some());
        assert!(payload.get("detail").is_none());
    }
}
