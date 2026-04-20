use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

/// Embedding 抽象：为 semantic / hybrid 检索提供向量编码能力。
///
/// 设计约束：
/// - 当前为最小形态，使用 blocking reqwest
/// - 具体 provider 实现（OpenAI / DeepSeek / local）从配置选择
/// - 调用方负责处理错误并决定 fallback 策略
pub trait EmbeddingProvider {
    /// 将查询文本编码为向量。
    fn embed_query(&self, text: &str) -> Result<Vec<f32>>;

    /// 将多个文档批量编码为向量。
    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// 模型标识，用于 trace 与可观测。
    fn model_name(&self) -> &str;
}

/// 占位实现：未配置 embedding 时使用。
///
/// 所有 embed_* 方法均返回显式错误，拒绝静默 fallback，
/// 方便调用方捕获后降级到规则法检索。
pub struct NoOpEmbeddingProvider;

impl NoOpEmbeddingProvider {
    pub const MODEL_NAME: &str = "noop";

    pub fn new() -> Self {
        Self
    }
}

impl Default for NoOpEmbeddingProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl EmbeddingProvider for NoOpEmbeddingProvider {
    fn embed_query(&self, _text: &str) -> Result<Vec<f32>> {
        bail!("embedding provider is not configured (NoOp)")
    }

    fn embed_documents(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        bail!("embedding provider is not configured (NoOp)")
    }

    fn model_name(&self) -> &str {
        Self::MODEL_NAME
    }
}

/// OpenAI 兼容 embeddings API 实现。
///
/// 支持 DeepSeek、OpenAI 等兼容 OpenAI embeddings 格式的服务商。
/// 从环境变量读取配置：
/// - `{PREFIX}_API_KEY`：API 密钥
/// - `{PREFIX}_BASE_URL`：API 基础地址
/// - `{PREFIX}_MODEL`：模型名（如 `text-embedding-3-small`）
pub struct OpenAiEmbeddingProvider {
    http: Client,
    api_key: String,
    model: String,
    base_url: String,
    name: String,
}

impl OpenAiEmbeddingProvider {
    /// 从环境变量加载配置。
    ///
    /// `env_prefix` 如 "DEEPSEEK" 或 "OPENAI"。
    pub fn from_env(env_prefix: &str) -> Result<Self> {
        let api_key_key = format!("{env_prefix}_API_KEY");
        let base_url_key = format!("{env_prefix}_BASE_URL");
        let model_key = format!("{env_prefix}_EMBEDDING_MODEL");

        let api_key = std::env::var(&api_key_key).unwrap_or_default();
        let base_url = std::env::var(&base_url_key).unwrap_or_default();
        let model = std::env::var(&model_key).unwrap_or_else(|_| match env_prefix {
            "MOONSHOT" => "text-embedding-v2".to_string(),
            _ => "text-embedding-3-small".to_string(),
        });

        if api_key.is_empty() {
            bail!("{api_key_key} 环境变量未设置");
        }
        if base_url.is_empty() {
            bail!("{base_url_key} 环境变量未设置");
        }

        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .context("创建 embedding HTTP 客户端失败")?;

        let name = format!("openai_compat_{}", env_prefix.to_lowercase());

        Ok(Self {
            http,
            api_key,
            model,
            base_url: base_url.trim_end_matches('/').to_string(),
            name,
        })
    }

    /// 发送 embeddings 请求。
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let url = format!("{}/embeddings", self.base_url);
        let body = json!({
            "model": self.model,
            "input": texts,
        });

        let response = self
            .http
            .post(&url)
            .header(CONTENT_TYPE, "application/json")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .with_context(|| format!("embedding 请求失败: {}", self.base_url))?;

        let status = response.status();
        let text = response.text().context("读取 embedding 响应失败")?;

        if !status.is_success() {
            bail!(
                "embedding API 错误 (status={} model={} base_url={}): {}",
                status.as_u16(),
                self.model,
                self.base_url,
                text
            );
        }

        let payload: EmbeddingResponse = serde_json::from_str(&text)
            .with_context(|| format!("解析 embedding 响应 JSON 失败: {}", text))?;

        let mut vectors = Vec::with_capacity(payload.data.len());
        for item in payload.data {
            vectors.push(item.embedding);
        }

        if vectors.len() != texts.len() {
            bail!(
                "embedding 返回向量数不匹配: requested={}, returned={}",
                texts.len(),
                vectors.len()
            );
        }

        Ok(vectors)
    }
}

impl EmbeddingProvider for OpenAiEmbeddingProvider {
    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let vectors = self.embed_batch(&[text.to_string()])?;
        vectors
            .into_iter()
            .next()
            .context("embedding API 返回空向量列表")
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        self.embed_batch(texts)
    }

    fn model_name(&self) -> &str {
        &self.name
    }
}

#[derive(Debug, Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingDataItem>,
    #[allow(dead_code)]
    model: String,
    #[allow(dead_code)]
    usage: Option<EmbeddingUsage>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingDataItem {
    embedding: Vec<f32>,
    #[allow(dead_code)]
    index: usize,
}

#[derive(Debug, Deserialize)]
struct EmbeddingUsage {
    #[allow(dead_code)]
    prompt_tokens: usize,
    #[allow(dead_code)]
    total_tokens: usize,
}

/// 根据环境变量前缀创建 embedding provider。
///
/// 支持前缀：
/// - "noop" → NoOpEmbeddingProvider
/// - "deepseek" → 从 DEEPSEEK_* 环境变量读取（需兼容 OpenAI embeddings 格式）
/// - "moonshot" → 从 MOONSHOT_* 环境变量读取
/// - "openai" → 从 OPENAI_* 环境变量读取
///
/// 注：DeepSeek 官方 API 暂不原生提供 embedding，若配置 deepseek，
/// 通常需将 DEEPSEEK_BASE_URL 指向第三方兼容服务（如 SiliconFlow）。
pub fn create_embedding_provider(
    provider_name: &str,
) -> Result<Box<dyn EmbeddingProvider + Send + Sync>> {
    match provider_name.trim().to_lowercase().as_str() {
        "noop" => Ok(Box::new(NoOpEmbeddingProvider::new())),
        "deepseek" => Ok(Box::new(OpenAiEmbeddingProvider::from_env("DEEPSEEK")?)),
        "moonshot" => Ok(Box::new(OpenAiEmbeddingProvider::from_env("MOONSHOT")?)),
        "openai" => Ok(Box::new(OpenAiEmbeddingProvider::from_env("OPENAI")?)),
        other => {
            bail!("不支持的 embedding provider: {other}。支持: noop, deepseek, moonshot, openai")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_provider_has_stable_model_name() {
        let provider = NoOpEmbeddingProvider::new();
        assert_eq!(provider.model_name(), "noop");
    }

    #[test]
    fn noop_embed_query_returns_explicit_error() {
        let provider = NoOpEmbeddingProvider::new();
        let err = provider.embed_query("hello").expect_err("应返回错误");
        let msg = err.to_string();
        assert!(
            msg.contains("not configured"),
            "错误应提示未配置, 实际: {msg}"
        );
        assert!(msg.contains("NoOp"), "错误应包含 NoOp 标识, 实际: {msg}");
    }

    #[test]
    fn noop_embed_documents_returns_explicit_error() {
        let provider = NoOpEmbeddingProvider::new();
        let texts = vec!["hello".to_string(), "world".to_string()];
        let err = provider.embed_documents(&texts).expect_err("应返回错误");
        let msg = err.to_string();
        assert!(
            msg.contains("not configured"),
            "错误应提示未配置, 实际: {msg}"
        );
        assert!(msg.contains("NoOp"), "错误应包含 NoOp 标识, 实际: {msg}");
    }

    #[test]
    fn create_provider_noop_works() {
        let provider = create_embedding_provider("noop").unwrap();
        assert_eq!(provider.model_name(), "noop");
    }

    #[test]
    fn create_provider_unknown_fails() {
        match create_embedding_provider("unknown") {
            Err(err) => {
                let msg = err.to_string();
                assert!(msg.contains("不支持"), "错误应提示不支持, 实际: {msg}");
                assert!(msg.contains("moonshot"), "错误应列出 moonshot, 实际: {msg}");
            }
            Ok(_) => panic!("应返回错误"),
        }
    }

    #[test]
    fn create_provider_deepseek_without_env_fails() {
        // 确保环境变量不存在
        std::env::remove_var("DEEPSEEK_API_KEY");
        std::env::remove_var("DEEPSEEK_BASE_URL");
        match create_embedding_provider("deepseek") {
            Err(err) => assert!(err.to_string().contains("DEEPSEEK_API_KEY")),
            Ok(_) => panic!("应返回错误"),
        }
    }

    #[test]
    fn create_provider_moonshot_without_env_fails() {
        std::env::remove_var("MOONSHOT_API_KEY");
        std::env::remove_var("MOONSHOT_BASE_URL");
        match create_embedding_provider("moonshot") {
            Err(err) => assert!(err.to_string().contains("MOONSHOT_API_KEY")),
            Ok(_) => panic!("应返回错误"),
        }
    }

    #[test]
    fn embedding_response_parsing() {
        let json = r#"{
            "object": "list",
            "data": [
                {"object": "embedding", "embedding": [0.1, 0.2, 0.3], "index": 0}
            ],
            "model": "text-embedding-3-small",
            "usage": {"prompt_tokens": 5, "total_tokens": 5}
        }"#;
        let resp: EmbeddingResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.len(), 1);
        assert_eq!(resp.data[0].embedding, vec![0.1, 0.2, 0.3]);
        assert_eq!(resp.model, "text-embedding-3-small");
    }
}
