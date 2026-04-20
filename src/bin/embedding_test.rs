/// 验证 embedding provider 配置是否正确。
///
/// 用法：
///   cargo run --bin embedding_test -- --provider moonshot --text "测试文本"
///   cargo run --bin embedding_test -- --provider noop --text "hello"
///
/// 环境变量（以 moonshot 为例）：
///   MOONSHOT_API_KEY=sk-xxx
///   MOONSHOT_BASE_URL=https://api.moonshot.cn/v1
///   MOONSHOT_EMBEDDING_MODEL=text-embedding-v2（可选，默认 text-embedding-v2）
use amclaw::retriever::embedding::create_embedding_provider;

fn main() {
    // 加载环境变量（同主进程启动逻辑）
    for path in [
        ".env.moonshot.local",
        ".env.moonshot",
        ".env.deepseek.local",
        ".env.deepseek",
    ] {
        if let Ok(content) = std::fs::read_to_string(path) {
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((key, value)) = line.split_once('=') {
                    if std::env::var_os(key).is_none() {
                        std::env::set_var(key, value.trim());
                    }
                }
            }
        }
    }
    let mut args = std::env::args().skip(1);
    let mut provider_name = "noop".to_string();
    let mut text = "这是一个测试句子".to_string();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--provider" => {
                if let Some(v) = args.next() {
                    provider_name = v;
                }
            }
            "--text" => {
                if let Some(v) = args.next() {
                    text = v;
                }
            }
            _ => {}
        }
    }

    println!("provider: {}", provider_name);
    println!("text: {}", text);
    println!();

    let provider = match create_embedding_provider(&provider_name) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("创建 provider 失败: {}", err);
            std::process::exit(1);
        }
    };

    println!("model_name: {}", provider.model_name());
    println!("embedding...");

    let started = std::time::Instant::now();
    match provider.embed_query(&text) {
        Ok(vector) => {
            let latency_ms = started.elapsed().as_millis();
            println!("OK! latency={}ms, dimension={}", latency_ms, vector.len());
            println!("first_5: {:?}", vector.iter().take(5).collect::<Vec<_>>());
        }
        Err(err) => {
            eprintln!("embedding 失败: {}", err);
            std::process::exit(1);
        }
    }
}
