use uuid::Uuid;

mod context_pack;
mod core_loop;
mod llm_plan;
mod memory_context;
mod recovery;
mod retriever_selection;
mod session_state;
mod trace_persistence;

fn temp_workspace() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("amclaw_agent_test_{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("创建测试目录失败");
    root
}

fn temp_db_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("amclaw_agent_test_{}.db", Uuid::new_v4()))
}
