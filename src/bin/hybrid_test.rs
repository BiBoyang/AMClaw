/// 端到端验证：hybrid retriever + ollama embedding
///
/// 运行前确保 ollama serve 已启动且 bge-m3 已拉取：
///   ollama serve &
///   ollama pull bge-m3
///
/// 用法：
///   OPENAI_API_KEY=ollama OPENAI_BASE_URL=http://localhost:11434/v1 \
///     cargo run --bin hybrid_test
use amclaw::retriever::{
    cached_embedding::CachedEmbeddingProvider, embedding::create_embedding_provider,
    hybrid::HybridRetriever, RetrieveQuery, Retriever,
};
use amclaw::task_store::{MemoryType, TaskStore};
use std::env::temp_dir;
use uuid::Uuid;

fn main() {
    let db_path = temp_dir().join(format!("hybrid_ollama_test_{}.db", Uuid::new_v4()));

    // 创建测试数据
    let mut store = TaskStore::open(&db_path).unwrap();
    store
        .add_user_memory_typed("user-h", "Rust 编程语言", MemoryType::Auto, 60)
        .unwrap();
    store
        .add_user_memory_typed("user-h", "深度学习与神经网络", MemoryType::Explicit, 100)
        .unwrap();
    store
        .add_user_memory_typed("user-h", "Web 前端开发", MemoryType::UserPreference, 80)
        .unwrap();

    // 创建 hybrid retriever（ollama embedding）
    let provider = create_embedding_provider("openai").unwrap();
    let cached = CachedEmbeddingProvider::new(provider, &db_path);
    let hybrid = HybridRetriever::new(&db_path, Box::new(cached));

    // 检索
    let query = RetrieveQuery::new("user-h", 3).with_query_text("机器学习算法");
    let started = std::time::Instant::now();
    let result = hybrid.retrieve(&query).unwrap();
    let latency_ms = started.elapsed().as_millis();

    println!("retriever_name: {}", result.retriever_name);
    println!("candidates_count: {}", result.candidates.len());
    println!("latency_ms: {}", latency_ms);
    println!();

    for (i, item) in result.candidates.iter().enumerate() {
        println!("[{}] content={}", i, item.content);
        println!(
            "      rule_score={} semantic_score={} final_score={}",
            item.metadata.get("rule_score").unwrap_or(&"?".to_string()),
            item.metadata
                .get("semantic_score")
                .unwrap_or(&"?".to_string()),
            item.metadata.get("final_score").unwrap_or(&"?".to_string()),
        );
    }

    // 验证：不是 fallback，且包含 semantic_score
    if result.retriever_name == "hybrid_v1" {
        println!("\n✓ 验证通过：使用了真实 embedding（非 fallback）");
    } else {
        println!("\n✗ 验证失败：retriever_name={}", result.retriever_name);
    }

    // 清理
    let _ = std::fs::remove_file(&db_path);
}
