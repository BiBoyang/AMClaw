use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub agent: AgentConfig,
    pub storage: StorageConfig,
    pub scheduler: SchedulerConfig,
    pub llm: LlmConfig,
    pub browser: BrowserConfig,
    pub wechat: WechatConfig,
    pub session: SessionConfig,
    #[serde(skip)]
    base_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub mode: String,
    pub timezone: String,
    pub session_summary_strategy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    pub root_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SchedulerConfig {
    pub enabled: bool,
    pub daily_run_time: String,
    pub report_to_user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub provider: String,
    pub model: String,
    pub max_tokens: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WechatConfig {
    pub channel_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserConfig {
    pub enabled: bool,
    pub command: String,
    pub worker_script: PathBuf,
    pub timeout_secs: u64,
    pub headless: bool,
    pub mobile_viewport: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionConfig {
    pub merge_timeout_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBrowserConfig {
    pub command: String,
    pub worker_script: PathBuf,
    pub timeout: Duration,
    pub headless: bool,
    pub mobile_viewport: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            agent: AgentConfig::default(),
            storage: StorageConfig::default(),
            scheduler: SchedulerConfig::default(),
            llm: LlmConfig::default(),
            browser: BrowserConfig::default(),
            wechat: WechatConfig::default(),
            session: SessionConfig::default(),
            base_dir: PathBuf::new(),
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            mode: "restricted".to_string(),
            timezone: "Asia/Shanghai".to_string(),
            session_summary_strategy: "semantic".to_string(),
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            root_dir: PathBuf::from("./data"),
        }
    }
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            daily_run_time: "22:30".to_string(),
            report_to_user_id: None,
        }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: "deepseek".to_string(),
            model: "deepseek-chat".to_string(),
            max_tokens: 800,
        }
    }
}

impl Default for WechatConfig {
    fn default() -> Self {
        Self {
            channel_version: "1.0.0".to_string(),
        }
    }
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            command: "node".to_string(),
            worker_script: PathBuf::from("./tools/browser_worker/worker.mjs"),
            timeout_secs: 45,
            headless: true,
            mobile_viewport: true,
        }
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            merge_timeout_secs: 5,
        }
    }
}

impl AppConfig {
    pub fn load_or_create(config_path: impl AsRef<Path>) -> Result<Self> {
        let config_path = config_path.as_ref();
        let base_dir = config_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        if !config_path.exists() {
            let default = Self::default();
            let template = toml::to_string_pretty(&default).context("序列化默认配置失败")?;
            fs::write(config_path, format!("{template}\n"))
                .with_context(|| format!("写入默认配置失败: {}", config_path.display()))?;
            log_config_info(
                "config_default_created",
                vec![("path", json!(config_path.display().to_string()))],
            );
        }

        let content = fs::read_to_string(config_path)
            .with_context(|| format!("读取配置文件失败: {}", config_path.display()))?;
        let mut config: Self = toml::from_str(&content)
            .with_context(|| format!("解析配置失败: {}", config_path.display()))?;
        config.base_dir = base_dir;
        Ok(config)
    }

    pub fn resolved_root_dir(&self) -> PathBuf {
        resolve_path(&self.base_dir, &self.storage.root_dir)
    }

    pub fn db_path(&self) -> PathBuf {
        self.resolved_root_dir().join("amclaw.db")
    }

    pub fn session_merge_timeout(&self) -> Duration {
        Duration::from_secs(self.session.merge_timeout_secs)
    }

    pub fn resolved_browser(&self) -> Option<ResolvedBrowserConfig> {
        if !self.browser.enabled {
            return None;
        }
        Some(ResolvedBrowserConfig {
            command: self.browser.command.clone(),
            worker_script: resolve_path(&self.base_dir, &self.browser.worker_script),
            timeout: Duration::from_secs(self.browser.timeout_secs),
            headless: self.browser.headless,
            mobile_viewport: self.browser.mobile_viewport,
        })
    }
}

fn resolve_path(base_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

fn log_config_info(event: &str, fields: Vec<(&str, Value)>) {
    crate::logging::emit_structured_log("info", event, fields);
}

#[cfg(test)]
mod tests {
    use super::AppConfig;
    use std::fs;
    use std::time::Duration;
    use uuid::Uuid;

    fn temp_dir() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("amclaw_config_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("创建测试目录失败");
        root
    }

    #[test]
    fn missing_config_is_created_with_defaults() {
        let root = temp_dir();
        let config_path = root.join("config.toml");

        let config = AppConfig::load_or_create(&config_path).expect("加载配置失败");

        assert!(config_path.exists());
        assert_eq!(config.wechat.channel_version, "1.0.0");
        assert_eq!(config.agent.session_summary_strategy, "semantic");
        assert_eq!(config.db_path(), root.join("data").join("amclaw.db"));
        assert_eq!(config.resolved_browser(), None);
    }

    #[test]
    fn relative_storage_root_is_resolved_against_config_dir() {
        let root = temp_dir();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            r#"
[storage]
root_dir = "./custom-data"

[agent]
session_summary_strategy = "truncate"

[browser]
enabled = true
worker_script = "./tools/browser_worker/worker.mjs"

[session]
merge_timeout_secs = 9
"#,
        )
        .expect("写测试配置失败");

        let config = AppConfig::load_or_create(&config_path).expect("加载配置失败");

        assert_eq!(config.db_path(), root.join("custom-data").join("amclaw.db"));
        assert_eq!(config.agent.session_summary_strategy, "truncate");
        assert_eq!(config.session_merge_timeout(), Duration::from_secs(9));
        assert_eq!(
            config
                .resolved_browser()
                .expect("应启用浏览器配置")
                .worker_script,
            root.join("tools/browser_worker/worker.mjs")
        );
    }

    #[test]
    fn invalid_toml_returns_error() {
        let root = temp_dir();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            r#"
[session
merge_timeout_secs = "oops"
"#,
        )
        .expect("写测试配置失败");

        let err = AppConfig::load_or_create(&config_path).expect_err("非法 TOML 应失败");
        assert!(err.to_string().contains("解析配置失败"));
    }
}
