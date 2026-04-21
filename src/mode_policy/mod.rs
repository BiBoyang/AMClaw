use serde_json::Value;

/// Agent 运行模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentMode {
    /// 受限模式：对高风险操作进行门禁拦截
    Restricted,
    /// 非受限模式：允许更宽范围的操作
    Unrestricted,
}

impl AgentMode {
    /// 从配置字符串解析
    pub fn from_config(s: &str) -> Self {
        if s.eq_ignore_ascii_case("unrestricted") {
            Self::Unrestricted
        } else {
            Self::Restricted
        }
    }
}

/// 策略决策结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDecision {
    pub allowed: bool,
    pub reason: String,
}

impl PolicyDecision {
    fn allow(reason: impl Into<String>) -> Self {
        Self {
            allowed: true,
            reason: reason.into(),
        }
    }

    fn deny(reason: impl Into<String>) -> Self {
        Self {
            allowed: false,
            reason: reason.into(),
        }
    }
}

/// 检查工具动作是否允许在指定模式下执行。
pub fn check_tool_action(mode: AgentMode, action: &str) -> PolicyDecision {
    match mode {
        AgentMode::Unrestricted => PolicyDecision::allow("unrestricted 模式放行所有工具动作"),
        AgentMode::Restricted => {
            // restricted 模式下禁止的高风险动作
            let denied_actions: &[&str] = &["run_command", "execute_shell", "exec"];
            if denied_actions.iter().any(|&a| action.contains(a)) {
                PolicyDecision::deny(format!("restricted 模式禁止执行高风险工具动作: {action}"))
            } else {
                PolicyDecision::allow("restricted 模式下允许的工具动作")
            }
        }
    }
}

/// 检查 URL 是否允许在指定模式下抓取。
pub fn check_url(mode: AgentMode, url: &str) -> PolicyDecision {
    match mode {
        AgentMode::Unrestricted => PolicyDecision::allow("unrestricted 模式放行所有 URL"),
        AgentMode::Restricted => {
            // 仅允许 HTTP/HTTPS 协议
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return PolicyDecision::deny(format!(
                    "restricted 模式仅允许 HTTP/HTTPS 协议: {url}"
                ));
            }
            // 复用 task_store 的私网/本地判断
            if crate::task_store::is_private_url(url) {
                return PolicyDecision::deny(format!(
                    "restricted 模式禁止抓取本地/私有地址: {url}"
                ));
            }
            PolicyDecision::allow("restricted 模式下允许的 URL")
        }
    }
}

#[allow(dead_code)]
fn log_policy_info(event: &str, fields: Vec<(&str, Value)>) {
    crate::logging::emit_structured_log("info", event, fields);
}

#[allow(dead_code)]
fn log_policy_warn(event: &str, fields: Vec<(&str, Value)>) {
    crate::logging::emit_structured_log("warn", event, fields);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restricted_denies_run_command() {
        let decision = check_tool_action(AgentMode::Restricted, "run_command ls");
        assert!(!decision.allowed);
        assert!(decision.reason.contains("restricted"));
    }

    #[test]
    fn restricted_allows_read() {
        let decision = check_tool_action(AgentMode::Restricted, "read");
        assert!(decision.allowed);
    }

    #[test]
    fn unrestricted_allows_all_tools() {
        let decision = check_tool_action(AgentMode::Unrestricted, "run_command rm -rf /");
        assert!(decision.allowed);
    }

    #[test]
    fn restricted_denies_localhost_url() {
        let decision = check_url(AgentMode::Restricted, "http://localhost:8080/api");
        assert!(!decision.allowed);
    }

    #[test]
    fn restricted_denies_file_protocol() {
        let decision = check_url(AgentMode::Restricted, "file:///etc/passwd");
        assert!(!decision.allowed);
    }

    #[test]
    fn restricted_allows_public_url() {
        let decision = check_url(AgentMode::Restricted, "https://example.com/article");
        assert!(decision.allowed);
    }

    #[test]
    fn restricted_allows_172_32() {
        // 172.32.x.x 不属于 RFC1918 私网段（172.16.0.0/12）
        let decision = check_url(AgentMode::Restricted, "http://172.32.1.1/a");
        assert!(decision.allowed);
    }

    #[test]
    fn restricted_denies_172_16() {
        let decision = check_url(AgentMode::Restricted, "http://172.16.1.1/a");
        assert!(!decision.allowed);
    }

    #[test]
    fn restricted_denies_127_0_0_1() {
        let decision = check_url(AgentMode::Restricted, "http://127.0.0.1/a");
        assert!(!decision.allowed);
    }

    #[test]
    fn restricted_denies_ipv6_loopback() {
        let decision = check_url(AgentMode::Restricted, "http://[::1]/a");
        assert!(!decision.allowed);
    }
}
