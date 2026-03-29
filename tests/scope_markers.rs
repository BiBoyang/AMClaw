use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn normalized_content(path: &Path) -> String {
    let content = fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("读取文件失败: {} ({err})", path.display()));
    content.replace("\r\n", "\n")
}

fn assert_contains_scope(path: &Path, expected: &str) {
    let content = normalized_content(path);
    assert!(
        content.contains(expected),
        "文件 {} 缺少 scope: {}",
        path.display(),
        expected
    );
}

#[test]
fn root_scope_markers_are_global() {
    let root = repo_root();
    let agents = root.join("AGENTS.md");
    let claude = root.join("CLAUDE.md");

    assert_contains_scope(&agents, "@scope:global:v1");
    assert_contains_scope(&claude, "@scope:global:v1");
}

#[test]
fn module_scope_markers_match_directory() {
    let root = repo_root();
    let src = root.join("src");
    let entries =
        fs::read_dir(&src).unwrap_or_else(|err| panic!("读取目录失败: {} ({err})", src.display()));

    for entry in entries {
        let entry = entry.unwrap_or_else(|err| panic!("读取目录项失败: {err}"));
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let module = entry.file_name().to_string_lossy().to_string();
        let scope = format!("@scope:src/{module}:v1");
        let agents = path.join("AGENTS.md");
        let claude = path.join("CLAUDE.md");

        assert!(agents.exists(), "缺少文件: {}", agents.display());
        assert!(claude.exists(), "缺少文件: {}", claude.display());

        assert_contains_scope(&agents, &scope);
        assert_contains_scope(&claude, &scope);
    }
}
