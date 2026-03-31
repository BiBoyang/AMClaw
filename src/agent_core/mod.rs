use crate::tool_registry::{ToolAction, ToolRegistry};
use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use chrono_tz::Asia::Shanghai;
use reqwest::blocking::Client;
use reqwest::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, Instant};
use uuid::Uuid;

const DEFAULT_MAX_STEPS: usize = 8;
const DEFAULT_OPENAI_MODEL: &str = "deepseek-chat";
const DEFAULT_MOONSHOT_MODEL: &str = "kimi-k2.5";
const LLM_PROVIDER_PRIORITY: [&str; 3] = ["DEEPSEEK", "MOONSHOT", "OPENAI"];

#[derive(Debug)]
pub struct AgentCore {
    workspace_root: PathBuf,
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
        let workspace_root = workspace_root.into();
        Ok(Self {
            workspace_root: workspace_root.clone(),
            tool_registry: ToolRegistry::new(workspace_root)?,
            llm_client: LlmClient::from_env()?,
            max_steps,
        })
    }

    pub fn run(&self, user_input: &str) -> Result<String> {
        let started = Instant::now();
        let mut trace = AgentRunTrace::new(&self.workspace_root, user_input);
        let mut tool_result: Option<String> = None;
        let result = (|| -> Result<String> {
            // 最小 Agent Loop: 决策 -> 执行工具 -> 继续决策/结束
            for step in 0..self.max_steps {
                trace.step_count = step + 1;
                let decision = self.decide(user_input, tool_result.as_deref(), step, &mut trace)?;
                match decision {
                    AgentDecision::CallTool(action) => {
                        let tool_trace = trace.start_tool_call(step, &action);
                        match self.tool_registry.execute(action) {
                            Ok(result) => {
                                trace.finish_tool_call_success(
                                    tool_trace,
                                    result.tool,
                                    &result.output,
                                );
                                tool_result = Some(result.output);
                            }
                            Err(err) => {
                                trace.finish_tool_call_error(tool_trace, &err.to_string());
                                return Err(err);
                            }
                        }
                    }
                    AgentDecision::Final(answer) => return Ok(answer),
                }
            }
            bail!("达到最大步骤，未能收敛")
        })();

        match &result {
            Ok(answer) => trace.finish_success(answer, started.elapsed()),
            Err(err) => trace.finish_error(&err.to_string(), started.elapsed()),
        }

        if let Err(err) = trace.persist() {
            eprintln!("[AgentTrace] persist_failed error={err}");
        }

        result
    }

    fn decide(
        &self,
        user_input: &str,
        tool_result: Option<&str>,
        step: usize,
        trace: &mut AgentRunTrace,
    ) -> Result<AgentDecision> {
        // 首轮根据用户输入决定是否调用工具
        if step == 0 {
            let mut llm_auth_err: Option<anyhow::Error> = None;
            if let Some(client) = &self.llm_client {
                match client.plan(user_input, trace) {
                    Ok(decision) => {
                        eprintln!("[Agent] planner=llm");
                        trace.record_decision(step, "llm", &decision);
                        return Ok(decision);
                    }
                    Err(err) => {
                        let err_text = err.to_string();
                        eprintln!("[Agent] planner=fallback reason={}", err_text);
                        trace.record_llm_fallback(&err_text);
                        if is_llm_auth_error(&err_text) {
                            llm_auth_err = Some(anyhow!(err_text));
                        }
                    }
                }
            } else {
                eprintln!("[Agent] planner=rule reason=no_llm_env");
                trace.record_llm_fallback("no_llm_env");
            }
            eprintln!("[Agent] planner=rule");
            match parse_user_command(user_input) {
                Ok(decision) => {
                    trace.record_decision(step, "rule", &decision);
                    return Ok(decision);
                }
                Err(parse_err) => {
                    trace.record_rule_parse_error(&parse_err.to_string());
                    if let Some(auth_err) = llm_auth_err {
                        return Err(auth_err);
                    }
                    return Err(parse_err);
                }
            }
        }

        // 有工具结果就直接收敛为最终回答
        if let Some(result) = tool_result {
            let decision = AgentDecision::Final(format!("完成: {}", result.trim()));
            trace.record_decision(step, "tool_result", &decision);
            return Ok(decision);
        }

        let decision = AgentDecision::Final("没有可执行的动作".to_string());
        trace.record_decision(step, "default", &decision);
        Ok(decision)
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

    fn plan(&self, user_input: &str, trace: &mut AgentRunTrace) -> Result<AgentDecision> {
        let mut last_auth_err: Option<anyhow::Error> = None;
        for (idx, config) in self.configs.iter().enumerate() {
            match self.plan_with_config(config, user_input, trace) {
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

    fn plan_with_config(
        &self,
        config: &LlmConfig,
        user_input: &str,
        trace: &mut AgentRunTrace,
    ) -> Result<AgentDecision> {
        let system_prompt = "你是一个文件工具规划器。只输出 JSON，不要解释。格式为 {\"action\":\"read|write|create|final\",\"path\":\"...\",\"content\":\"...\",\"answer\":\"...\"}。read 只需要 path；write/create 需要 path 与 content；final 需要 answer。";
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
                    "content": user_input
                }
            ]
        });
        if config.source == "MOONSHOT" {
            body["temperature"] = json!(1);
        } else {
            body["temperature"] = json!(0.0);
        }
        let llm_call = trace.start_llm_call(config, system_prompt, user_input, &body);
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
                eprintln!("[Agent] llm_retry reason=governor");
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
        let payload: OpenAiCompatResponse =
            serde_json::from_str(&text).map_err(|err| {
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
                trace.finish_llm_call_error(llm_call, attempts, Some(last_status), "LLM 返回内容为空");
                anyhow!("LLM 返回内容为空")
            })?;
        let decision = parse_llm_plan(&content).map_err(|err| {
            trace.finish_llm_call_error(
                llm_call,
                attempts,
                Some(last_status),
                &format!("解析 LLM 计划失败: {err}"),
            );
            err
        })?;
        trace.finish_llm_call_success(llm_call, attempts, &text, &content, last_status, &decision);
        Ok(decision)
    }
}

#[derive(Debug, Serialize)]
struct AgentRunTrace {
    trace_version: &'static str,
    run_id: String,
    started_at: String,
    finished_at: Option<String>,
    duration_ms: Option<u128>,
    success: bool,
    error: Option<String>,
    final_output: Option<String>,
    user_input: String,
    user_input_chars: usize,
    step_count: usize,
    workspace_root: String,
    llm_fallback_reason: Option<String>,
    rule_parse_error: Option<String>,
    decisions: Vec<DecisionTrace>,
    llm_calls: Vec<LlmCallTrace>,
    tool_calls: Vec<ToolCallTrace>,
    #[serde(skip_serializing)]
    trace_dir_root: PathBuf,
}

#[derive(Debug, Serialize)]
struct DecisionTrace {
    step: usize,
    source: String,
    decision_type: String,
    summary: String,
}

#[derive(Debug, Serialize)]
struct PromptSnapshot {
    system_prompt: String,
    user_prompt: String,
    request_body: String,
    system_prompt_chars: usize,
    user_prompt_chars: usize,
    request_body_chars: usize,
    estimated_prompt_chars: usize,
}

#[derive(Debug, Serialize)]
struct LlmCallTrace {
    source: String,
    model: String,
    base_url: String,
    prompt: PromptSnapshot,
    raw_response: Option<String>,
    raw_response_chars: Option<usize>,
    message_content: Option<String>,
    message_content_chars: Option<usize>,
    response_status: Option<u16>,
    attempts: usize,
    success: bool,
    error: Option<String>,
    decision_summary: Option<String>,
}

#[derive(Debug, Serialize)]
struct ToolCallTrace {
    step: usize,
    tool_name: String,
    path: Option<String>,
    content_chars: Option<usize>,
    output: Option<String>,
    output_chars: Option<usize>,
    success: bool,
    error: Option<String>,
    duration_ms: Option<u128>,
    #[serde(skip_serializing)]
    started_at: Option<Instant>,
}

impl AgentRunTrace {
    fn new(workspace_root: &std::path::Path, user_input: &str) -> Self {
        Self {
            trace_version: "agent_trace_v1",
            run_id: Uuid::new_v4().to_string(),
            started_at: Utc::now().to_rfc3339(),
            finished_at: None,
            duration_ms: None,
            success: false,
            error: None,
            final_output: None,
            user_input: user_input.to_string(),
            user_input_chars: user_input.chars().count(),
            step_count: 0,
            workspace_root: workspace_root.display().to_string(),
            llm_fallback_reason: None,
            rule_parse_error: None,
            decisions: Vec::new(),
            llm_calls: Vec::new(),
            tool_calls: Vec::new(),
            trace_dir_root: workspace_root.join("data").join("agent_traces"),
        }
    }

    fn record_decision(&mut self, step: usize, source: &str, decision: &AgentDecision) {
        self.decisions.push(DecisionTrace {
            step,
            source: source.to_string(),
            decision_type: decision.kind().to_string(),
            summary: decision.summary(),
        });
    }

    fn record_llm_fallback(&mut self, reason: &str) {
        self.llm_fallback_reason = Some(reason.to_string());
    }

    fn record_rule_parse_error(&mut self, error: &str) {
        self.rule_parse_error = Some(error.to_string());
    }

    fn start_llm_call(
        &mut self,
        config: &LlmConfig,
        system_prompt: &str,
        user_input: &str,
        body: &serde_json::Value,
    ) -> usize {
        let request_body = serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string());
        self.llm_calls.push(LlmCallTrace {
            source: config.source.to_string(),
            model: config.model.clone(),
            base_url: config.base_url.clone(),
            prompt: PromptSnapshot {
                system_prompt: system_prompt.to_string(),
                user_prompt: user_input.to_string(),
                system_prompt_chars: system_prompt.chars().count(),
                user_prompt_chars: user_input.chars().count(),
                request_body_chars: request_body.chars().count(),
                estimated_prompt_chars: system_prompt.chars().count()
                    + user_input.chars().count()
                    + request_body.chars().count(),
                request_body,
            },
            raw_response: None,
            raw_response_chars: None,
            message_content: None,
            message_content_chars: None,
            response_status: None,
            attempts: 0,
            success: false,
            error: None,
            decision_summary: None,
        });
        self.llm_calls.len() - 1
    }

    fn finish_llm_call_success(
        &mut self,
        index: usize,
        attempts: usize,
        raw_response: &str,
        message_content: &str,
        status: u16,
        decision: &AgentDecision,
    ) {
        if let Some(call) = self.llm_calls.get_mut(index) {
            call.raw_response = Some(raw_response.to_string());
            call.raw_response_chars = Some(raw_response.chars().count());
            call.message_content = Some(message_content.to_string());
            call.message_content_chars = Some(message_content.chars().count());
            call.response_status = Some(status);
            call.attempts = attempts;
            call.success = true;
            call.decision_summary = Some(decision.summary());
        }
    }

    fn finish_llm_call_error(
        &mut self,
        index: usize,
        attempts: usize,
        status: Option<u16>,
        error: &str,
    ) {
        if let Some(call) = self.llm_calls.get_mut(index) {
            call.attempts = attempts;
            call.response_status = status;
            call.error = Some(error.to_string());
        }
    }

    fn start_tool_call(&mut self, step: usize, action: &ToolAction) -> usize {
        self.tool_calls.push(ToolCallTrace {
            step,
            tool_name: action.name().to_string(),
            path: action.path().map(ToOwned::to_owned),
            content_chars: action.content().map(|v| v.chars().count()),
            output: None,
            output_chars: None,
            success: false,
            error: None,
            duration_ms: None,
            started_at: Some(Instant::now()),
        });
        self.tool_calls.len() - 1
    }

    fn finish_tool_call_success(&mut self, index: usize, tool_name: &str, output: &str) {
        if let Some(call) = self.tool_calls.get_mut(index) {
            call.tool_name = tool_name.to_string();
            call.output = Some(output.to_string());
            call.output_chars = Some(output.chars().count());
            call.success = true;
            call.duration_ms = call.started_at.map(|v| v.elapsed().as_millis());
            call.started_at = None;
        }
    }

    fn finish_tool_call_error(&mut self, index: usize, error: &str) {
        if let Some(call) = self.tool_calls.get_mut(index) {
            call.error = Some(error.to_string());
            call.duration_ms = call.started_at.map(|v| v.elapsed().as_millis());
            call.started_at = None;
        }
    }

    fn finish_success(&mut self, output: &str, duration: Duration) {
        self.success = true;
        self.final_output = Some(output.to_string());
        self.finished_at = Some(Utc::now().to_rfc3339());
        self.duration_ms = Some(duration.as_millis());
    }

    fn finish_error(&mut self, error: &str, duration: Duration) {
        self.success = false;
        self.error = Some(error.to_string());
        self.finished_at = Some(Utc::now().to_rfc3339());
        self.duration_ms = Some(duration.as_millis());
        for call in &mut self.llm_calls {
            if !call.success && call.error.is_none() {
                call.error = Some(error.to_string());
            }
        }
    }

    fn persist(&self) -> Result<()> {
        let day = Utc::now().with_timezone(&Shanghai).format("%Y-%m-%d").to_string();
        let dir = self.trace_dir_root.join(day);
        fs::create_dir_all(&dir)
            .with_context(|| format!("创建 agent trace 目录失败: {}", dir.display()))?;
        let timestamp = Utc::now()
            .with_timezone(&Shanghai)
            .format("%Y%m%dT%H%M%S")
            .to_string();
        let json_path = dir.join(format!("run_{}_{}.json", timestamp, self.run_id));
        let json_content =
            serde_json::to_string_pretty(self).context("序列化 agent trace 失败")?;
        fs::write(&json_path, format!("{json_content}\n"))
            .with_context(|| format!("写入 agent trace 失败: {}", json_path.display()))?;

        let markdown_path = dir.join(format!("run_{}_{}.md", timestamp, self.run_id));
        let markdown_content = self.to_markdown();
        fs::write(&markdown_path, markdown_content)
            .with_context(|| format!("写入 agent trace markdown 失败: {}", markdown_path.display()))?;
        Ok(())
    }

    fn to_markdown(&self) -> String {
        let mut lines = vec![
            format!("# Agent Trace {}", self.run_id),
            String::new(),
            "## Summary".to_string(),
            String::new(),
            format!("- success: {}", self.success),
            format!("- started_at: {}", self.started_at),
            format!(
                "- finished_at: {}",
                self.finished_at.as_deref().unwrap_or("(running)")
            ),
            format!("- duration_ms: {}", self.duration_ms.unwrap_or(0)),
            format!("- step_count: {}", self.step_count),
            format!("- user_input_chars: {}", self.user_input_chars),
            String::new(),
            "## User Input".to_string(),
            String::new(),
            "```text".to_string(),
            self.user_input.clone(),
            "```".to_string(),
        ];

        if let Some(reason) = &self.llm_fallback_reason {
            lines.push(String::new());
            lines.push("## LLM Fallback".to_string());
            lines.push(String::new());
            lines.push(format!("- reason: {}", reason));
        }

        if let Some(error) = &self.rule_parse_error {
            lines.push(String::new());
            lines.push("## Rule Parse Error".to_string());
            lines.push(String::new());
            lines.push(format!("- error: {}", error));
        }

        lines.push(String::new());
        lines.push("## Decisions".to_string());
        lines.push(String::new());
        if self.decisions.is_empty() {
            lines.push("- (none)".to_string());
        } else {
            for decision in &self.decisions {
                lines.push(format!(
                    "- step={} source={} type={} summary={}",
                    decision.step, decision.source, decision.decision_type, decision.summary
                ));
            }
        }

        lines.push(String::new());
        lines.push("## LLM Calls".to_string());
        lines.push(String::new());
        if self.llm_calls.is_empty() {
            lines.push("- (none)".to_string());
        } else {
            for (idx, call) in self.llm_calls.iter().enumerate() {
                lines.push(format!("### LLM Call {}", idx + 1));
                lines.push(String::new());
                lines.push(format!("- source: {}", call.source));
                lines.push(format!("- model: {}", call.model));
                lines.push(format!("- base_url: {}", call.base_url));
                lines.push(format!("- success: {}", call.success));
                lines.push(format!("- attempts: {}", call.attempts));
                lines.push(format!(
                    "- response_status: {}",
                    call.response_status
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "(none)".to_string())
                ));
                lines.push(format!(
                    "- estimated_prompt_chars: {}",
                    call.prompt.estimated_prompt_chars
                ));
                lines.push(format!(
                    "- system_prompt_chars: {}",
                    call.prompt.system_prompt_chars
                ));
                lines.push(format!("- user_prompt_chars: {}", call.prompt.user_prompt_chars));
                lines.push(format!(
                    "- request_body_chars: {}",
                    call.prompt.request_body_chars
                ));
                lines.push(format!(
                    "- raw_response_chars: {}",
                    call.raw_response_chars
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "(none)".to_string())
                ));
                lines.push(format!(
                    "- message_content_chars: {}",
                    call.message_content_chars
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "(none)".to_string())
                ));
                if let Some(error) = &call.error {
                    lines.push(format!("- error: {}", error));
                }
                if let Some(summary) = &call.decision_summary {
                    lines.push(format!("- decision_summary: {}", summary));
                }
                lines.push(String::new());
                lines.push("#### User Prompt".to_string());
                lines.push(String::new());
                lines.push("```text".to_string());
                lines.push(summarize_for_markdown(&call.prompt.user_prompt, 800));
                lines.push("```".to_string());
                if let Some(content) = &call.message_content {
                    lines.push(String::new());
                    lines.push("#### Message Content Summary".to_string());
                    lines.push(String::new());
                    lines.push("```text".to_string());
                    lines.push(summarize_for_markdown(content, 1000));
                    lines.push("```".to_string());
                }
                lines.push(String::new());
            }
        }

        lines.push("## Tool Calls".to_string());
        lines.push(String::new());
        if self.tool_calls.is_empty() {
            lines.push("- (none)".to_string());
        } else {
            for (idx, call) in self.tool_calls.iter().enumerate() {
                lines.push(format!("### Tool Call {}", idx + 1));
                lines.push(String::new());
                lines.push(format!("- step: {}", call.step));
                lines.push(format!("- tool_name: {}", call.tool_name));
                lines.push(format!(
                    "- path: {}",
                    call.path.as_deref().unwrap_or("(none)")
                ));
                lines.push(format!(
                    "- content_chars: {}",
                    call.content_chars
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "(none)".to_string())
                ));
                lines.push(format!(
                    "- output_chars: {}",
                    call.output_chars
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "(none)".to_string())
                ));
                lines.push(format!("- success: {}", call.success));
                lines.push(format!(
                    "- duration_ms: {}",
                    call.duration_ms
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "(none)".to_string())
                ));
                if let Some(error) = &call.error {
                    lines.push(format!("- error: {}", error));
                }
                if let Some(output) = &call.output {
                    lines.push(String::new());
                    lines.push("#### Output Summary".to_string());
                    lines.push(String::new());
                    lines.push("```text".to_string());
                    lines.push(summarize_for_markdown(output, 1200));
                    lines.push("```".to_string());
                }
                lines.push(String::new());
            }
        }

        lines.push("## Final Output".to_string());
        lines.push(String::new());
        lines.push("```text".to_string());
        lines.push(
            summarize_for_markdown(
                self.final_output
                    .as_deref()
                    .unwrap_or_else(|| self.error.as_deref().unwrap_or("(none)")),
                1200,
            ),
        );
        lines.push("```".to_string());
        lines.push(String::new());

        lines.join("\n")
    }
}

fn truncate_for_trace(input: &str, max_chars: usize) -> String {
    let count = input.chars().count();
    if count <= max_chars {
        return input.to_string();
    }
    let mut text: String = input.chars().take(max_chars).collect();
    text.push_str("...");
    text
}

fn summarize_for_markdown(input: &str, max_chars: usize) -> String {
    let count = input.chars().count();
    if count <= max_chars {
        return input.to_string();
    }
    let head_chars = max_chars.saturating_sub(80).max(40);
    let mut text: String = input.chars().take(head_chars).collect();
    text.push_str(&format!("\n\n...[truncated, total_chars={count}]"));
    text
}

impl AgentDecision {
    fn kind(&self) -> &'static str {
        match self {
            Self::CallTool(_) => "call_tool",
            Self::Final(_) => "final",
        }
    }

    fn summary(&self) -> String {
        match self {
            Self::CallTool(action) => format!("tool={} path={}", action.name(), action.path().unwrap_or("")),
            Self::Final(answer) => truncate_for_trace(answer, 240),
        }
    }
}

impl ToolAction {
    fn name(&self) -> &'static str {
        match self {
            Self::Read { .. } => "read",
            Self::Write { .. } => "write",
            Self::Create { .. } => "create",
        }
    }

    fn path(&self) -> Option<&str> {
        match self {
            Self::Read { path } | Self::Write { path, .. } | Self::Create { path, .. } => {
                Some(path.as_str())
            }
        }
    }

    fn content(&self) -> Option<&str> {
        match self {
            Self::Read { .. } => None,
            Self::Write { content, .. } | Self::Create { content, .. } => Some(content.as_str()),
        }
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
    use serde_json::Value;
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
    fn agent_run_writes_trace_file() {
        let root = temp_workspace();
        let agent = AgentCore::new(root.clone()).expect("初始化 agent 失败");

        agent.run("读文件 missing.txt").expect_err("应当返回错误");

        let trace_root = root.join("data").join("agent_traces");
        let day_dir = std::fs::read_dir(&trace_root)
            .expect("应存在 trace 根目录")
            .next()
            .expect("应存在日期目录")
            .expect("读取日期目录失败")
            .path();
        let trace_path = std::fs::read_dir(day_dir)
            .expect("应存在 trace 文件")
            .filter_map(|entry| entry.ok().map(|v| v.path()))
            .find(|path| path.extension().and_then(|v| v.to_str()) == Some("json"))
            .expect("应存在至少一个 json trace 文件");
        let payload: Value = serde_json::from_str(
            &std::fs::read_to_string(&trace_path).expect("读取 trace 文件失败"),
        )
        .expect("trace JSON 应合法");

        assert_eq!(payload["trace_version"], "agent_trace_v1");
        assert_eq!(payload["user_input"], "读文件 missing.txt");
        assert!(payload["user_input_chars"].as_u64().unwrap_or(0) > 0);
        assert!(payload["tool_calls"].as_array().is_some());
        assert!(payload["decisions"].as_array().is_some());

        let markdown_path = trace_path.with_extension("md");
        let markdown = std::fs::read_to_string(markdown_path).expect("应生成 markdown trace");
        assert!(markdown.contains("# Agent Trace"));
        assert!(markdown.contains("## Summary"));
        assert!(markdown.contains("## Tool Calls"));
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
