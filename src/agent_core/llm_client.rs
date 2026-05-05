use super::{
    log_agent_info, log_agent_warn, AgentRunTrace, PlannedDecision, PlannerInput, PlanningPolicy,
    DEFAULT_MOONSHOT_MODEL, DEFAULT_OPENAI_MODEL, LLM_PROVIDER_PRIORITY,
};
use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::Client;
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;
use serde_json::json;
use std::thread::sleep;
use std::time::Duration;

#[derive(Debug, Clone)]
pub(crate) struct LlmClient {
    http: Client,
    configs: Vec<LlmConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LlmConfig {
    pub(crate) source: &'static str,
    pub(crate) api_key: String,
    pub(crate) model: String,
    pub(crate) base_url: String,
}

impl LlmClient {
    pub(crate) fn from_env() -> Result<Option<Self>> {
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
            log_agent_info(
                "agent_llm_config_enabled",
                vec![
                    ("source", json!(config.source)),
                    ("model", json!(config.model)),
                    ("base_url", json!(config.base_url)),
                    ("key_tail", json!(key_tail(&config.api_key))),
                ],
            );
        }
        if configs.len() > 1 {
            let order = configs
                .iter()
                .map(|v| v.source)
                .collect::<Vec<_>>()
                .join("->");
            log_agent_info(
                "agent_llm_multi_provider_enabled",
                vec![("order", json!(order))],
            );
        }
        Ok(Some(Self { http, configs }))
    }

    pub(crate) fn plan(
        &self,
        planning_policy: PlanningPolicy,
        planner_input: &PlannerInput,
        trace: &mut AgentRunTrace,
    ) -> Result<PlannedDecision> {
        let mut last_auth_err: Option<anyhow::Error> = None;
        for (idx, config) in self.configs.iter().enumerate() {
            match self.plan_with_config(config, planning_policy, planner_input, trace) {
                Ok(decision) => {
                    if idx > 0 {
                        log_agent_info(
                            "agent_llm_fallback_success",
                            vec![
                                ("source", json!(config.source)),
                                ("model", json!(config.model)),
                                ("base_url", json!(config.base_url)),
                            ],
                        );
                    }
                    return Ok(decision);
                }
                Err(err) => {
                    let err_text = err.to_string();
                    if is_llm_auth_error(&err_text) && idx + 1 < self.configs.len() {
                        log_agent_warn(
                            "agent_llm_auth_failed",
                            vec![
                                ("source", json!(config.source)),
                                ("model", json!(config.model)),
                                ("base_url", json!(config.base_url)),
                                ("key_tail", json!(key_tail(&config.api_key))),
                                ("detail", json!("fallback_next")),
                            ],
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
        bail!("LLM 配置为空")
    }

    fn plan_with_config(
        &self,
        config: &LlmConfig,
        planning_policy: PlanningPolicy,
        planner_input: &PlannerInput,
        trace: &mut AgentRunTrace,
    ) -> Result<PlannedDecision> {
        let system_prompt = build_system_prompt(planning_policy);
        let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
        let mut body = json!({
            "model": config.model,
            "messages": [
                {
                    "role": "system",
                    "content": system_prompt
                },
                {
                    "role": "user",
                    "content": planner_input.assembled_user_prompt
                }
            ]
        });
        if config.source == "MOONSHOT" {
            body["temperature"] = json!(1);
        } else {
            body["temperature"] = json!(0.0);
        }
        let llm_call = trace.start_llm_call(config, system_prompt, planner_input, &body);
        let mut last_status = 0u16;
        let mut last_text = String::new();
        let mut payload_text: Option<String> = None;
        let mut attempts = 0usize;
        for attempt in 1..=2 {
            attempts = attempt;
            let response = self
                .http
                .post(&url)
                .header(CONTENT_TYPE, "application/json")
                .header("Authorization", format!("Bearer {}", config.api_key))
                .json(&body)
                .send()
                .map_err(|err| {
                    trace.finish_llm_call_error(
                        llm_call,
                        attempts,
                        None,
                        &format!("请求 LLM 失败: {err}"),
                    );
                    anyhow!("请求 LLM 失败: {err}")
                })?;
            let status = response.status();
            let text = response.text().map_err(|err| {
                trace.finish_llm_call_error(
                    llm_call,
                    attempts,
                    Some(status.as_u16()),
                    &format!("读取 LLM 响应失败: {err}"),
                );
                anyhow!("读取 LLM 响应失败: {err}")
            })?;
            last_status = status.as_u16();
            last_text = text.clone();
            if status.is_success() {
                payload_text = Some(text);
                break;
            }
            let retryable = status.as_u16() == 401 && text.contains("governor");
            if retryable && attempt == 1 {
                log_agent_warn(
                    "agent_llm_retry",
                    vec![
                        ("reason", json!("governor")),
                        ("attempt", json!(attempt)),
                        ("source", json!(config.source)),
                    ],
                );
                sleep(Duration::from_millis(250));
                continue;
            }
            let error = format!(
                "LLM 请求失败(source={} model={} base_url={} key_tail={}): HTTP {} {}",
                config.source,
                config.model,
                config.base_url,
                key_tail(&config.api_key),
                status.as_u16(),
                text
            );
            trace.finish_llm_call_error(llm_call, attempts, Some(status.as_u16()), &error);
            bail!(error);
        }
        let text = payload_text.ok_or_else(|| {
            let error = format!(
                "LLM 请求失败(source={} model={} base_url={} key_tail={}): HTTP {} {}",
                config.source,
                config.model,
                config.base_url,
                key_tail(&config.api_key),
                last_status,
                last_text
            );
            trace.finish_llm_call_error(llm_call, attempts, Some(last_status), &error);
            anyhow!(error)
        })?;
        let payload: OpenAiCompatResponse = serde_json::from_str(&text).map_err(|err| {
            trace.finish_llm_call_error(
                llm_call,
                attempts,
                Some(last_status),
                &format!("解析 LLM 响应 JSON 失败: {err}"),
            );
            anyhow!("解析 LLM 响应 JSON 失败: {err}")
        })?;
        let content = payload
            .choices
            .first()
            .map(|v| v.message.content.trim().to_string())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                trace.finish_llm_call_error(
                    llm_call,
                    attempts,
                    Some(last_status),
                    "LLM 返回内容为空",
                );
                anyhow!("LLM 返回内容为空")
            })?;
        let planned = super::parse_llm_plan(&content).map_err(|err| {
            trace.finish_llm_call_error(
                llm_call,
                attempts,
                Some(last_status),
                &format!("解析 LLM 计划失败: {err}"),
            );
            err
        })?;
        trace.finish_llm_call_success(llm_call, attempts, &text, &content, last_status, &planned);
        Ok(planned)
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

pub(crate) fn is_llm_auth_error(err: &str) -> bool {
    err.contains("HTTP 401") || err.contains("Authentication Fails")
}

fn build_system_prompt(planning_policy: PlanningPolicy) -> &'static str {
    match planning_policy {
        PlanningPolicy::Reactive => {
            "你是一个工具规划器，采用最小 ReAct 风格工作：先根据上下文判断下一步，再决定是调用一个工具还是直接给出最终结果。每轮最多只调用一个工具。只输出 JSON，不要解释。格式为 {\"action\":\"read|write|create|get_task_status|list_recent_tasks|list_manual_tasks|read_article_archive|final\",\"path\":\"...\",\"content\":\"...\",\"task_id\":\"...\",\"limit\":5,\"answer\":\"...\",\"plan\":[\"步骤1\",\"步骤2\"],\"progress_note\":\"当前做到哪\",\"expected_kind\":\"text|json_object|file_mutation|task_status|task_list|archive_content\",\"done_rule\":\"tool_success|non_empty_output|required_json_field\",\"required_field\":\"field_name\",\"expected_fields\":[\"field_a\",\"field_b\"],\"minimum_novelty\":\"different_from_last\"}。read 只需要 path；write/create 需要 path 与 content；get_task_status 需要 task_id；list_recent_tasks/list_manual_tasks 可选 limit；read_article_archive 需要 task_id；final 需要 answer。plan、progress_note、expected_kind、done_rule、required_field、expected_fields、minimum_novelty 都是可选字段，用于表达当前计划、进度和期望观测。"
        }
    }
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
