use crate::tool_registry::{ToolAction, ToolRegistry};
use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::Client;
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;
use serde_json::json;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;

const DEFAULT_MAX_STEPS: usize = 8;
const DEFAULT_OPENAI_MODEL: &str = "deepseek-chat";
const DEFAULT_MOONSHOT_MODEL: &str = "kimi-k2.5";
const LLM_PROVIDER_PRIORITY: [&str; 3] = ["DEEPSEEK", "MOONSHOT", "OPENAI"];

#[derive(Debug)]
pub struct AgentCore {
    // 负责实际执行工具动作（读写文件等）
    tool_registry: ToolRegistry,
    llm_client: Option<LlmClient>,
    // 防止 Agent 无穷循环的安全阀
    max_steps: usize,
}

impl AgentCore {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Result<Self> {
        Self::with_max_steps(workspace_root, DEFAULT_MAX_STEPS)
    }

    pub fn with_max_steps(workspace_root: impl Into<PathBuf>, max_steps: usize) -> Result<Self> {
        if max_steps == 0 {
            bail!("max_steps 必须大于 0");
        }
        Ok(Self {
            tool_registry: ToolRegistry::new(workspace_root)?,
            llm_client: LlmClient::from_env()?,
            max_steps,
        })
    }

    pub fn run(&self, user_input: &str) -> Result<String> {
        let mut tool_result: Option<String> = None;
        // 最小 Agent Loop: 决策 -> 执行工具 -> 继续决策/结束
        for step in 0..self.max_steps {
            let decision = self.decide(user_input, tool_result.as_deref(), step)?;
            match decision {
                AgentDecision::CallTool(action) => {
                    let result = self.tool_registry.execute(action)?;
                    tool_result = Some(result.output);
                }
                AgentDecision::Final(answer) => return Ok(answer),
            }
        }
        bail!("达到最大步骤，未能收敛")
    }

    fn decide(
        &self,
        user_input: &str,
        tool_result: Option<&str>,
        step: usize,
    ) -> Result<AgentDecision> {
        // 首轮根据用户输入决定是否调用工具
        if step == 0 {
            let mut llm_auth_err: Option<anyhow::Error> = None;
            if let Some(client) = &self.llm_client {
                match client.plan(user_input) {
                    Ok(decision) => {
                        eprintln!("[Agent] planner=llm");
                        return Ok(decision);
                    }
                    Err(err) => {
                        let err_text = err.to_string();
                        eprintln!("[Agent] planner=fallback reason={}", err_text);
                        if is_llm_auth_error(&err_text) {
                            llm_auth_err = Some(anyhow!(err_text));
                        }
                    }
                }
            } else {
                eprintln!("[Agent] planner=rule reason=no_llm_env");
            }
            eprintln!("[Agent] planner=rule");
            match parse_user_command(user_input) {
                Ok(decision) => return Ok(decision),
                Err(parse_err) => {
                    if let Some(auth_err) = llm_auth_err {
                        return Err(auth_err);
                    }
                    return Err(parse_err);
                }
            }
        }

        // 有工具结果就直接收敛为最终回答
        if let Some(result) = tool_result {
            return Ok(AgentDecision::Final(format!("完成: {}", result.trim())));
        }

        Ok(AgentDecision::Final("没有可执行的动作".to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AgentDecision {
    // 继续行动：调用一个工具
    CallTool(ToolAction),
    // 结束循环：直接返回用户可读结果
    Final(String),
}

#[derive(Debug, Clone)]
struct LlmClient {
    http: Client,
    configs: Vec<LlmConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LlmConfig {
    source: &'static str,
    api_key: String,
    model: String,
    base_url: String,
}

impl LlmClient {
    fn from_env() -> Result<Option<Self>> {
        let mut configs = Vec::new();
        for provider in LLM_PROVIDER_PRIORITY {
            let loaded = match provider {
                "DEEPSEEK" => load_llm_config(
                    "DEEPSEEK",
                    "DEEPSEEK_API_KEY",
                    "DEEPSEEK_MODEL",
                    "DEEPSEEK_BASE_URL",
                ),
                "MOONSHOT" => load_llm_config(
                    "MOONSHOT",
                    "MOONSHOT_API_KEY",
                    "MOONSHOT_MODEL",
                    "MOONSHOT_BASE_URL",
                ),
                "OPENAI" => load_llm_config(
                    "OPENAI",
                    "OPENAI_API_KEY",
                    "OPENAI_MODEL",
                    "OPENAI_BASE_URL",
                ),
                _ => None,
            };
            if let Some(config) = loaded {
                if !configs.iter().any(|v| v == &config) {
                    configs.push(config);
                }
            }
        }
        if configs.is_empty() {
            return Ok(None);
        }
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .context("创建 LLM HTTP 客户端失败")?;
        for config in &configs {
            eprintln!(
                "[Agent] llm_config enabled=true source={} model={} base_url={} key_tail={}",
                config.source,
                config.model,
                config.base_url,
                key_tail(&config.api_key)
            );
        }
        if configs.len() > 1 {
            let order = configs
                .iter()
                .map(|v| v.source)
                .collect::<Vec<_>>()
                .join("->");
            eprintln!("[Agent] llm_config multi_provider_fallback=true order={order}");
        }
        Ok(Some(Self { http, configs }))
    }

    fn plan(&self, user_input: &str) -> Result<AgentDecision> {
        let mut last_auth_err: Option<anyhow::Error> = None;
        for (idx, config) in self.configs.iter().enumerate() {
            match self.plan_with_config(config, user_input) {
                Ok(decision) => {
                    if idx > 0 {
                        eprintln!(
                            "[Agent] llm_fallback_success source={} model={} base_url={}",
                            config.source, config.model, config.base_url
                        );
                    }
                    return Ok(decision);
                }
                Err(err) => {
                    let err_text = err.to_string();
                    if is_llm_auth_error(&err_text) && idx + 1 < self.configs.len() {
                        eprintln!(
                            "[Agent] llm_auth_failed source={} model={} base_url={} key_tail={} -> fallback_next",
                            config.source,
                            config.model,
                            config.base_url,
                            key_tail(&config.api_key)
                        );
                        last_auth_err = Some(err);
                        continue;
                    }
                    return Err(err);
                }
            }
        }
        if let Some(err) = last_auth_err {
            return Err(err);
        }
        bail!("LLM 配置为空");
    }

    fn plan_with_config(&self, config: &LlmConfig, user_input: &str) -> Result<AgentDecision> {
        let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
        let mut body = json!({
            "model": config.model,
            "messages": [
                {
                    "role": "system",
                    "content": "你是一个文件工具规划器。只输出 JSON，不要解释。格式为 {\"action\":\"read|write|create|final\",\"path\":\"...\",\"content\":\"...\",\"answer\":\"...\"}。read 只需要 path；write/create 需要 path 与 content；final 需要 answer。"
                },
                {
                    "role": "user",
                    "content": user_input
                }
            ]
        });
        if config.source == "MOONSHOT" {
            body["temperature"] = json!(1);
        } else {
            body["temperature"] = json!(0.0);
        }
        let mut last_status = 0u16;
        let mut last_text = String::new();
        let mut payload_text: Option<String> = None;
        for attempt in 1..=2 {
            let response = self
                .http
                .post(&url)
                .header(CONTENT_TYPE, "application/json")
                .header("Authorization", format!("Bearer {}", config.api_key))
                .json(&body)
                .send()
                .context("请求 LLM 失败")?;
            let status = response.status();
            let text = response.text().context("读取 LLM 响应失败")?;
            last_status = status.as_u16();
            last_text = text.clone();
            if status.is_success() {
                payload_text = Some(text);
                break;
            }
            let retryable = status.as_u16() == 401 && text.contains("governor");
            if retryable && attempt == 1 {
                eprintln!("[Agent] llm_retry reason=governor");
                sleep(Duration::from_millis(250));
                continue;
            }
            bail!(
                "LLM 请求失败(source={} model={} base_url={} key_tail={}): HTTP {} {}",
                config.source,
                config.model,
                config.base_url,
                key_tail(&config.api_key),
                status.as_u16(),
                text
            );
        }
        let text = payload_text.ok_or_else(|| {
            anyhow!(
                "LLM 请求失败(source={} model={} base_url={} key_tail={}): HTTP {} {}",
                config.source,
                config.model,
                config.base_url,
                key_tail(&config.api_key),
                last_status,
                last_text
            )
        })?;
        let payload: OpenAiCompatResponse =
            serde_json::from_str(&text).context("解析 LLM 响应 JSON 失败")?;
        let content = payload
            .choices
            .first()
            .map(|v| v.message.content.trim().to_string())
            .filter(|v| !v.is_empty())
            .context("LLM 返回内容为空")?;
        parse_llm_plan(&content)
    }
}

fn load_llm_config(
    source: &'static str,
    api_key_key: &str,
    model_key: &str,
    base_url_key: &str,
) -> Option<LlmConfig> {
    let api_key = get_env(api_key_key);
    let base_url = normalize_base_url(&get_env(base_url_key));
    if api_key.is_empty() || base_url.is_empty() {
        return None;
    }

    let model = get_env(model_key);
    let model = if model.is_empty() {
        match source {
            "MOONSHOT" => DEFAULT_MOONSHOT_MODEL.to_string(),
            _ => DEFAULT_OPENAI_MODEL.to_string(),
        }
    } else {
        model
    };

    Some(LlmConfig {
        source,
        api_key,
        model,
        base_url,
    })
}

fn normalize_base_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    let stripped = trimmed.strip_suffix("/chat/completions").unwrap_or(trimmed);
    stripped.trim_end_matches('/').to_string()
}

fn clean_env(input: String) -> String {
    input
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_matches('`')
        .trim()
        .to_string()
}

fn get_env(key: &str) -> String {
    clean_env(std::env::var(key).unwrap_or_default())
}

fn key_tail(api_key: &str) -> String {
    let suffix: String = api_key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if suffix.is_empty() {
        "none".to_string()
    } else {
        suffix
    }
}

fn is_llm_auth_error(err: &str) -> bool {
    err.contains("HTTP 401") || err.contains("Authentication Fails")
}

#[derive(Debug, Deserialize)]
struct OpenAiCompatResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAiMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct LlmPlan {
    action: String,
    path: Option<String>,
    content: Option<String>,
    answer: Option<String>,
}

fn parse_user_command(input: &str) -> Result<AgentDecision> {
    let text = normalize_user_command(input);
    if let Some(path) = text.strip_prefix("读文件 ") {
        return Ok(AgentDecision::CallTool(ToolAction::Read {
            path: path.trim().to_string(),
        }));
    }
    if let Some(rest) = text.strip_prefix("创建文件 ") {
        let (path, content) = split_path_and_content(rest)?;
        return Ok(AgentDecision::CallTool(ToolAction::Create {
            path,
            content,
        }));
    }
    if let Some(rest) = text.strip_prefix("写文件 ") {
        let (path, content) = split_path_and_content(rest)?;
        return Ok(AgentDecision::CallTool(ToolAction::Write { path, content }));
    }
    if let Some(path) = text.strip_prefix("read ") {
        return Ok(AgentDecision::CallTool(ToolAction::Read {
            path: path.trim().to_string(),
        }));
    }
    if let Some(rest) = text.strip_prefix("create ") {
        let (path, content) = split_path_and_content(rest)?;
        return Ok(AgentDecision::CallTool(ToolAction::Create {
            path,
            content,
        }));
    }
    if let Some(rest) = text.strip_prefix("write ") {
        let (path, content) = split_path_and_content(rest)?;
        return Ok(AgentDecision::CallTool(ToolAction::Write { path, content }));
    }

    bail!(
        "无法解析指令。可用格式: 读文件 <path> | 创建文件 <path> :: <content> | 写文件 <path> :: <content>"
    )
}

fn normalize_user_command(input: &str) -> &str {
    let text = input.trim();
    if let Some(rest) = text.strip_prefix("帮我运行：") {
        return rest.trim();
    }
    if let Some(rest) = text.strip_prefix("帮我运行:") {
        return rest.trim();
    }
    if let Some(rest) = text.strip_prefix("请帮我运行：") {
        return rest.trim();
    }
    if let Some(rest) = text.strip_prefix("请帮我运行:") {
        return rest.trim();
    }
    text
}

fn parse_llm_plan(raw: &str) -> Result<AgentDecision> {
    let normalized = extract_json_object(raw).unwrap_or(raw);
    let plan: LlmPlan = serde_json::from_str(normalized)
        .with_context(|| format!("LLM 输出不是合法 JSON: {raw}"))?;
    map_llm_plan(plan)
}

fn map_llm_plan(plan: LlmPlan) -> Result<AgentDecision> {
    match plan.action.trim().to_lowercase().as_str() {
        "read" => {
            let path = plan
                .path
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .context("LLM 缺少 path")?;
            Ok(AgentDecision::CallTool(ToolAction::Read { path }))
        }
        "write" => {
            let path = plan
                .path
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .context("LLM 缺少 path")?;
            let content = plan.content.unwrap_or_default();
            Ok(AgentDecision::CallTool(ToolAction::Write { path, content }))
        }
        "create" => {
            let path = plan
                .path
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .context("LLM 缺少 path")?;
            let content = plan.content.unwrap_or_default();
            Ok(AgentDecision::CallTool(ToolAction::Create {
                path,
                content,
            }))
        }
        "final" => {
            let answer = plan
                .answer
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .context("LLM 缺少 answer")?;
            Ok(AgentDecision::Final(answer))
        }
        other => bail!("LLM action 不支持: {other}"),
    }
}

fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    raw.get(start..=end)
}

fn split_path_and_content(raw: &str) -> Result<(String, String)> {
    // 约定格式：<path> :: <content>
    let (path, content) = raw
        .split_once("::")
        .context("写入/创建指令格式错误，缺少 :: 分隔符")?;
    let path = path.trim().to_string();
    if path.is_empty() {
        bail!("文件路径不能为空");
    }
    Ok((path, content.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::{map_llm_plan, parse_llm_plan, AgentCore, LlmPlan};
    use uuid::Uuid;

    fn temp_workspace() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("amclaw_agent_test_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("创建测试目录失败");
        root
    }

    #[test]
    fn loop_create_then_read() {
        let root = temp_workspace();
        let agent = AgentCore::new(root).expect("初始化 agent 失败");

        let create = agent
            .run("创建文件 demo/hello.txt :: 你好 AMClaw")
            .expect("创建文件失败");
        assert!(create.contains("完成:"));

        let read = agent.run("读文件 demo/hello.txt").expect("读取文件失败");
        assert!(read.contains("你好 AMClaw"));
    }

    #[test]
    fn invalid_command_returns_error() {
        let root = temp_workspace();
        let agent = AgentCore::new(root).expect("初始化 agent 失败");
        let err = agent.run("unknown command").expect_err("应当返回错误");
        assert!(err.to_string().contains("无法解析指令"));
    }

    #[test]
    fn one_step_is_not_enough_for_tool_then_finalize() {
        let root = temp_workspace();
        let agent = AgentCore::with_max_steps(root, 1).expect("初始化 agent 失败");
        let err = agent
            .run("创建文件 demo/hello.txt :: 你好")
            .expect_err("单步应当无法收敛");
        assert!(err.to_string().contains("达到最大步骤"));
    }

    #[test]
    fn prefix_command_is_supported() {
        let root = temp_workspace();
        let agent = AgentCore::new(root).expect("初始化 agent 失败");
        let result = agent
            .run("帮我运行：创建文件 demo/prefix.txt :: prefix ok")
            .expect("前缀命令执行失败");
        assert!(result.contains("完成:"));
    }

    #[test]
    fn llm_plan_json_is_supported() {
        let decision = parse_llm_plan("{\"action\":\"read\",\"path\":\"demo/a.txt\"}")
            .expect("LLM JSON 解析失败");
        assert!(matches!(
            decision,
            super::AgentDecision::CallTool(super::ToolAction::Read { .. })
        ));
    }

    #[test]
    fn llm_plan_markdown_json_is_supported() {
        let raw = "```json\n{\"action\":\"final\",\"answer\":\"ok\"}\n```";
        let decision = parse_llm_plan(raw).expect("Markdown JSON 解析失败");
        assert!(matches!(decision, super::AgentDecision::Final(_)));
    }

    #[test]
    fn map_llm_plan_requires_path_for_read() {
        let err = map_llm_plan(LlmPlan {
            action: "read".to_string(),
            path: None,
            content: None,
            answer: None,
        })
        .expect_err("read 无 path 应失败");
        assert!(err.to_string().contains("path"));
    }
}
