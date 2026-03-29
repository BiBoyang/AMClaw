use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolAction {
    // 读取文件内容
    Read { path: String },
    // 覆盖写入文件内容（不存在会自动创建）
    Write { path: String, content: String },
    // 创建新文件（已存在则报错）
    Create { path: String, content: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub tool: &'static str,
    pub output: String,
}

#[derive(Debug, Clone)]
pub struct ToolRegistry {
    // 工具允许访问的根目录边界
    workspace_root: PathBuf,
}

impl ToolRegistry {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Result<Self> {
        let workspace_root = normalize_absolute(workspace_root.into())?;
        Ok(Self { workspace_root })
    }

    pub fn execute(&self, action: ToolAction) -> Result<ToolResult> {
        // 统一工具分发入口
        match action {
            ToolAction::Read { path } => self.read_file(&path),
            ToolAction::Write { path, content } => self.write_file(&path, &content),
            ToolAction::Create { path, content } => self.create_file(&path, &content),
        }
    }

    fn read_file(&self, raw_path: &str) -> Result<ToolResult> {
        let path = self.resolve_path(raw_path)?;
        let content = fs::read_to_string(&path)
            .with_context(|| format!("读取文件失败: {}", path.display()))?;
        Ok(ToolResult {
            tool: "read_file",
            output: content,
        })
    }

    fn write_file(&self, raw_path: &str, content: &str) -> Result<ToolResult> {
        let path = self.resolve_path(raw_path)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败: {}", parent.display()))?;
        }
        fs::write(&path, content).with_context(|| format!("写入文件失败: {}", path.display()))?;
        Ok(ToolResult {
            tool: "write_file",
            output: format!("ok: {}", path.display()),
        })
    }

    fn create_file(&self, raw_path: &str, content: &str) -> Result<ToolResult> {
        let path = self.resolve_path(raw_path)?;
        if path.exists() {
            bail!("文件已存在: {}", path.display());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败: {}", parent.display()))?;
        }
        fs::write(&path, content).with_context(|| format!("创建文件失败: {}", path.display()))?;
        Ok(ToolResult {
            tool: "create_file",
            output: format!("ok: {}", path.display()),
        })
    }

    fn resolve_path(&self, raw_path: &str) -> Result<PathBuf> {
        let raw_path = raw_path.trim();
        if raw_path.is_empty() {
            bail!("文件路径不能为空");
        }

        let joined = if Path::new(raw_path).is_absolute() {
            PathBuf::from(raw_path)
        } else {
            self.workspace_root.join(raw_path)
        };

        let normalized = normalize_absolute(joined)?;
        // 路径必须落在 workspace_root 内，阻断 ../../ 越界访问
        if !normalized.starts_with(&self.workspace_root) {
            bail!("路径越界，禁止访问工作区外路径: {}", normalized.display());
        }
        Ok(normalized)
    }
}

fn normalize_absolute(path: PathBuf) -> Result<PathBuf> {
    if !path.is_absolute() {
        bail!("只允许绝对路径: {}", path.display());
    }

    // 规范化路径中的 "." 与 ".."，得到稳定绝对路径
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    bail!("非法路径: {}", path.display());
                }
            }
            Component::Normal(segment) => normalized.push(segment),
            Component::Prefix(_) => bail!("不支持的路径格式: {}", path.display()),
        }
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::{ToolAction, ToolRegistry};
    use std::path::PathBuf;
    use uuid::Uuid;

    fn temp_workspace() -> PathBuf {
        let root = std::env::temp_dir().join(format!("amclaw_tool_test_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("创建测试目录失败");
        root
    }

    #[test]
    fn create_write_read_file_works() {
        let root = temp_workspace();
        let registry = ToolRegistry::new(root.clone()).expect("初始化 registry 失败");

        registry
            .execute(ToolAction::Create {
                path: "notes/todo.txt".to_string(),
                content: "hello".to_string(),
            })
            .expect("创建文件失败");
        registry
            .execute(ToolAction::Write {
                path: "notes/todo.txt".to_string(),
                content: "hello world".to_string(),
            })
            .expect("写入文件失败");
        let result = registry
            .execute(ToolAction::Read {
                path: "notes/todo.txt".to_string(),
            })
            .expect("读取文件失败");

        assert_eq!(result.output, "hello world");
    }

    #[test]
    fn deny_outside_workspace_path() {
        let root = temp_workspace();
        let registry = ToolRegistry::new(root).expect("初始化 registry 失败");

        let err = registry
            .execute(ToolAction::Read {
                path: "../../etc/hosts".to_string(),
            })
            .expect_err("应当禁止越界路径");

        assert!(err.to_string().contains("路径越界"));
    }
}
