use super::super::{AgentCore, AgentRunContext, AgentRunTrace, AgentTraceIndexEntry};
use super::temp_workspace;
use serde_json::Value;
use std::fs::OpenOptions;
use std::io::Write;

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
    let payload: Value =
        serde_json::from_str(&std::fs::read_to_string(&trace_path).expect("读取 trace 文件失败"))
            .expect("trace JSON 应合法");

    assert_eq!(payload["trace_version"], "agent_trace_v1");
    assert_eq!(payload["user_input"], "读文件 missing.txt");
    assert_eq!(payload["source_type"], "agent_demo");
    assert_eq!(payload["message_count"], 0);
    assert_eq!(payload["context_token_present"], false);
    assert!(payload["user_input_chars"].as_u64().unwrap_or(0) > 0);
    assert!(payload["tool_calls"].as_array().is_some());
    assert!(payload["decisions"].as_array().is_some());
    assert!(payload["observations"].as_array().is_some());
    assert!(payload["recovery_attempts"].is_array());
    assert!(payload.get("recovery_action").is_some());
    assert!(payload.get("recovery_result").is_some());

    let markdown_path = trace_path.with_extension("md");
    let markdown = std::fs::read_to_string(markdown_path).expect("应生成 markdown trace");
    assert!(markdown.contains("# Agent Trace"));
    assert!(markdown.contains("## Summary"));
    assert!(markdown.contains("## Tool Calls"));
    assert!(markdown.contains("## Observations"));

    let index_path = trace_root
        .join(
            std::fs::read_dir(&trace_root)
                .expect("应存在日期目录")
                .next()
                .expect("应存在日期目录")
                .expect("读取日期目录失败")
                .file_name(),
        )
        .join("index.jsonl");
    let index_content = std::fs::read_to_string(index_path).expect("应生成 index.jsonl");
    assert!(index_content.contains("\"trace_version\":\"agent_trace_v1\""));
    assert!(index_content.contains("\"run_id\""));
    assert!(index_content.contains("\"source_type\":\"agent_demo\""));
}

#[test]
fn agent_run_with_context_writes_upstream_metadata() {
    let root = temp_workspace();
    let agent = AgentCore::new(root.clone()).expect("初始化 agent 失败");

    agent
        .run_with_context(
            "读文件 missing.txt",
            AgentRunContext::wechat_chat(
                "user-trace",
                "commit",
                vec!["msg-a".to_string(), "msg-b".to_string()],
            )
            .with_task_id("task-trace")
            .with_article_id("article-trace")
            .with_session_text("session trace text")
            .with_context_token_present(true),
        )
        .expect_err("应当返回错误");

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
    let payload: Value =
        serde_json::from_str(&std::fs::read_to_string(&trace_path).expect("读取 trace 文件失败"))
            .expect("trace JSON 应合法");

    assert_eq!(payload["source_type"], "wechat_chat");
    assert_eq!(payload["trigger_type"], "commit");
    assert_eq!(payload["user_id"], "user-trace");
    assert_eq!(payload["message_count"], 2);
    assert_eq!(payload["task_id"], "task-trace");
    assert_eq!(payload["article_id"], "article-trace");
    assert_eq!(payload["session_text"], "session trace text");
    assert_eq!(payload["session_text_chars"], 18);
    assert_eq!(payload["context_token_present"], true);
    assert_eq!(payload["message_ids"][0], "msg-a");
    assert_eq!(payload["message_ids"][1], "msg-b");
}

#[test]
fn daily_index_markdown_is_generated() {
    let root = temp_workspace();
    let agent = AgentCore::new(root.clone()).expect("初始化 agent 失败");

    agent.run("读文件 missing-a.txt").expect_err("应当返回错误");
    agent
        .run_with_context(
            "读文件 missing-b.txt",
            AgentRunContext::wechat_chat("user-index", "timeout", vec!["msg-index".to_string()]),
        )
        .expect_err("应当返回错误");

    let trace_root = root.join("data").join("agent_traces");
    let day_name = std::fs::read_dir(&trace_root)
        .expect("应存在日期目录")
        .next()
        .expect("应存在日期目录")
        .expect("读取日期目录失败")
        .file_name();
    let index_md_path = trace_root.join(day_name).join("index.md");
    let index_md = std::fs::read_to_string(index_md_path).expect("应生成 index.md");

    assert!(index_md.contains("# Agent Trace Daily Index"));
    assert!(index_md.contains("- total_runs: 2"));
    assert!(index_md.contains("agent_demo(1)"));
    assert!(index_md.contains("wechat_chat(1)"));
    assert!(index_md
        .contains("| Time | Status | Run | Source | Trigger | User | Msgs | Input | Files |"));
    assert!(index_md.contains("[json]("));
    assert!(index_md.contains("[md]("));
    assert!(index_md.contains("user-index"));
    assert!(index_md.contains("timeout"));
}

#[test]
fn trace_index_backcompat_missing_injected_alias_fields_defaults_to_zero() {
    let legacy_line = r#"{
            "trace_version":"agent_trace_v1",
            "run_id":"run-legacy",
            "started_at":"2026-04-01T00:00:00Z",
            "finished_at":"2026-04-01T00:00:01Z",
            "duration_ms":1,
            "success":true,
            "user_input":"legacy",
            "user_input_chars":6,
            "source_type":"agent_demo",
            "trigger_type":null,
            "user_id":null,
            "message_ids":[],
            "message_count":0,
            "task_id":null,
            "article_id":null,
            "session_text_chars":0,
            "context_token_present":false,
            "step_count":1,
            "llm_call_count":0,
            "tool_call_count":0,
            "observation_count":0,
            "final_output_chars":null,
            "error":null,
            "llm_fallback_reason":null,
            "memory_hit_count":0,
            "memory_retrieved_count":0,
            "memory_total_chars":0,
            "memory_dropped_count":0,
            "json_file":"legacy.json",
            "markdown_file":"legacy.md"
        }"#;

    let entry: AgentTraceIndexEntry =
        serde_json::from_str(legacy_line).expect("旧版 index 行应可反序列化");
    assert_eq!(entry.memory_injected_count, 0);
    assert_eq!(entry.memory_injected_total_chars, 0);
}

#[test]
fn persist_twice_appends_index_without_overwrite() {
    let workspace = temp_workspace();
    let mut trace = AgentRunTrace::new(&workspace, "第一次", AgentRunContext::agent_demo());
    trace.finish_success("第一次结果", std::time::Duration::from_secs(1));
    let path1 = trace.persist().expect("第一次 persist 失败");
    let run_id1 = trace.run_id.clone();

    // 第二次运行（不同 run_id）
    let mut trace2 = AgentRunTrace::new(&workspace, "第二次", AgentRunContext::agent_demo());
    trace2.finish_success("第二次结果", std::time::Duration::from_secs(1));
    let path2 = trace2.persist().expect("第二次 persist 失败");
    let run_id2 = trace2.run_id.clone();

    // 两个 json 文件应不同
    assert_ne!(path1, path2);

    // index.jsonl 应有两行
    let day_dir = path1.parent().expect("应有父目录");
    let index_path = day_dir.join("index.jsonl");
    let index_content = std::fs::read_to_string(&index_path).expect("读取 index.jsonl 失败");
    let lines: Vec<&str> = index_content.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "index.jsonl 应有两行，实际: {}",
        lines.len()
    );

    // 两行应分别包含两个 run_id
    assert!(lines[0].contains(&run_id1), "第一行应包含 run_id1");
    assert!(lines[1].contains(&run_id2), "第二行应包含 run_id2");
}

#[test]
fn write_daily_index_markdown_skips_invalid_lines() {
    let workspace = temp_workspace();
    let mut trace = AgentRunTrace::new(&workspace, "测试", AgentRunContext::agent_demo());
    trace.finish_success("结果", std::time::Duration::from_secs(1));
    let _ = trace.persist().expect("persist 失败");

    let day_dir = workspace.join("data").join("agent_traces");
    let day_dir = std::fs::read_dir(&day_dir)
        .expect("应存在日期目录")
        .next()
        .expect("应存在日期目录")
        .expect("读取日期目录失败")
        .path();
    let index_path = day_dir.join("index.jsonl");

    // 追加一行坏数据
    let mut file = OpenOptions::new()
        .append(true)
        .open(&index_path)
        .expect("打开 index.jsonl 失败");
    file.write_all(b"this is not json\n").expect("写入坏行失败");
    drop(file);

    // 再 persist 一次（追加一行好的）
    let mut trace2 = AgentRunTrace::new(&workspace, "第二次", AgentRunContext::agent_demo());
    trace2.finish_success("结果2", std::time::Duration::from_secs(1));
    let _path2 = trace2.persist().expect("第二次 persist 失败");

    // 验证 index.md 存在且包含有效 run
    let markdown_path = day_dir.join("index.md");
    assert!(markdown_path.exists(), "index.md 应存在");
    let markdown = std::fs::read_to_string(&markdown_path).expect("读取 index.md 失败");
    assert!(
        markdown.contains("total_runs: 2"),
        "index.md 应显示 2 条有效 run"
    );
    assert!(
        markdown.contains(&trace2.run_id),
        "index.md 应包含第二个 run_id"
    );
}
