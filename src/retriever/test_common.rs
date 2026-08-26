//! retriever 测试共享的 fake EmbeddingProvider（hybrid / semantic 此前逐字节重复）。

use crate::retriever::embedding::EmbeddingProvider;
use anyhow::Result;

/// 一个假的 EmbeddingProvider，用于测试。
/// embed_query 返回基于文本 hash 的固定向量。
/// embed_documents 返回基于每个文本 hash 的固定向量。
pub(crate) struct FakeEmbeddingProvider;

impl EmbeddingProvider for FakeEmbeddingProvider {
    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        // 生成一个 4 维的伪向量，基于文本内容
        let hash = text
            .bytes()
            .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
        let mut vec = vec![0.0f32; 4];
        for (i, slot) in vec.iter_mut().enumerate() {
            *slot = ((hash.wrapping_add(i as u64)) % 1000) as f32 / 1000.0;
        }
        Ok(vec)
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        texts.iter().map(|text| self.embed_query(text)).collect()
    }

    fn model_name(&self) -> &str {
        "fake_test_model"
    }
}

/// 总是返回错误的 FakeProvider，用于测试 fallback。
pub(crate) struct FailingEmbeddingProvider;

impl EmbeddingProvider for FailingEmbeddingProvider {
    fn embed_query(&self, _text: &str) -> Result<Vec<f32>> {
        anyhow::bail!("simulated embedding failure")
    }

    fn embed_documents(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        anyhow::bail!("simulated embedding failure")
    }

    fn model_name(&self) -> &str {
        "failing_model"
    }
}
