/// Agent 运行模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentMode {
    /// 受限模式：对 URL 抓取进行门禁拦截（仅 HTTP/HTTPS 且禁止本地/私网地址）
    Restricted,
    /// 非受限模式：允许更宽范围的 URL 抓取
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

#[cfg(test)]
mod tests {
    use super::*;

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
