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

fn normalized_mirror_body(path: &Path) -> String {
    let content = normalized_content(path);
    let mut lines = content.lines();
    let _title = lines.next();
    let body = lines.collect::<Vec<_>>().join("\n");
    body.trim_end().to_string()
}

fn collect_agents_files(root: &Path) -> Vec<PathBuf> {
    let mut files = vec![root.join("AGENTS.md")];
    let src = root.join("src");
    let entries =
        fs::read_dir(&src).unwrap_or_else(|err| panic!("读取目录失败: {} ({err})", src.display()));

    for entry in entries {
        let entry = entry.unwrap_or_else(|err| panic!("读取目录项失败: {err}"));
        let path = entry.path();
        if path.is_dir() {
            let agents = path.join("AGENTS.md");
            if agents.exists() {
                files.push(agents);
            }
        }
    }

    files.sort();
    files
}

#[test]
fn claude_files_exist_for_all_agents_files() {
    let root = repo_root();

    for agents in collect_agents_files(&root) {
        let claude = agents.with_file_name("CLAUDE.md");
        assert!(
            claude.exists(),
            "缺少镜像文件: {} (源文件: {})",
            claude.display(),
            agents.display()
        );
    }
}

#[test]
fn claude_files_match_agents_files_except_title() {
    let root = repo_root();

    for agents in collect_agents_files(&root) {
        let claude = agents.with_file_name("CLAUDE.md");
        assert!(
            claude.exists(),
            "缺少镜像文件: {} (源文件: {})",
            claude.display(),
            agents.display()
        );

        let agents_body = normalized_mirror_body(&agents);
        let claude_body = normalized_mirror_body(&claude);

        assert_eq!(
            claude_body,
            agents_body,
            "文档镜像漂移: {} <-> {}",
            agents.display(),
            claude.display()
        );
    }
}
