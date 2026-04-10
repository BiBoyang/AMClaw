use crate::task_store::{RecentTaskRecord, TaskStatusRecord, TaskStore};
use anyhow::{bail, Context, Result};
use serde_json::json;
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
    // 查询单个任务状态
    GetTaskStatus { task_id: String },
    // 查询最近任务
    ListRecentTasks { limit: usize },
    // 查询待人工补录任务
    ListManualTasks { limit: usize },
    // 读取已归档文章内容
    ReadArticleArchive { task_id: String },
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
    task_store_db_path: Option<PathBuf>,
}

impl ToolRegistry {
    #[allow(dead_code)]
    pub fn new(workspace_root: impl Into<PathBuf>) -> Result<Self> {
        Self::with_task_store_db_path(workspace_root, None::<PathBuf>)
    }

    pub fn with_task_store_db_path(
        workspace_root: impl Into<PathBuf>,
        task_store_db_path: Option<impl Into<PathBuf>>,
    ) -> Result<Self> {
        let workspace_root = normalize_absolute(workspace_root.into())?;
        Ok(Self {
            workspace_root,
            task_store_db_path: task_store_db_path.map(|value| value.into()),
        })
    }

    pub fn execute(&self, action: ToolAction) -> Result<ToolResult> {
        // 统一工具分发入口
        match action {
            ToolAction::Read { path } => self.read_file(&path),
            ToolAction::Write { path, content } => self.write_file(&path, &content),
            ToolAction::Create { path, content } => self.create_file(&path, &content),
            ToolAction::GetTaskStatus { task_id } => self.get_task_status(&task_id),
            ToolAction::ListRecentTasks { limit } => self.list_recent_tasks(limit),
            ToolAction::ListManualTasks { limit } => self.list_manual_tasks(limit),
            ToolAction::ReadArticleArchive { task_id } => self.read_article_archive(&task_id),
        }
    }

    pub fn available_tool_descriptions(&self) -> Vec<String> {
        let mut tools = vec![
            "read: 读取工作区内文件，参数: path".to_string(),
            "create: 创建工作区内新文件，参数: path, content".to_string(),
            "write: 覆盖写入工作区内文件，参数: path, content".to_string(),
        ];
        if self.task_store_db_path.is_some() {
            tools.push("get_task_status: 查询单个任务状态，参数: task_id".to_string());
            tools.push("list_recent_tasks: 查询最近任务，参数: limit".to_string());
            tools.push("list_manual_tasks: 查询待人工补录任务，参数: limit".to_string());
            tools.push("read_article_archive: 读取已归档文章内容，参数: task_id".to_string());
        }
        tools
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

    fn get_task_status(&self, task_id: &str) -> Result<ToolResult> {
        let task_id = task_id.trim();
        if task_id.is_empty() {
            bail!("task_id 不能为空");
        }
        let store = self.open_task_store()?;
        let payload = match store.get_task_status(task_id)? {
            Some(status) => json!({
                "found": true,
                "task": render_task_status_record(&status),
            }),
            None => json!({
                "found": false,
                "task_id": task_id,
            }),
        };
        Ok(ToolResult {
            tool: "get_task_status",
            output: serde_json::to_string_pretty(&payload).context("序列化任务状态结果失败")?,
        })
    }

    fn list_recent_tasks(&self, limit: usize) -> Result<ToolResult> {
        let store = self.open_task_store()?;
        let tasks = store.list_recent_tasks(limit)?;
        let payload = json!({
            "limit": limit,
            "count": tasks.len(),
            "tasks": tasks
                .iter()
                .map(render_recent_task_record)
                .collect::<Vec<_>>(),
        });
        Ok(ToolResult {
            tool: "list_recent_tasks",
            output: serde_json::to_string_pretty(&payload).context("序列化最近任务结果失败")?,
        })
    }

    fn list_manual_tasks(&self, limit: usize) -> Result<ToolResult> {
        let store = self.open_task_store()?;
        let tasks = store.list_manual_tasks(limit)?;
        let payload = json!({
            "limit": limit,
            "count": tasks.len(),
            "tasks": tasks
                .iter()
                .map(render_recent_task_record)
                .collect::<Vec<_>>(),
        });
        Ok(ToolResult {
            tool: "list_manual_tasks",
            output: serde_json::to_string_pretty(&payload).context("序列化待补录任务结果失败")?,
        })
    }

    fn read_article_archive(&self, task_id: &str) -> Result<ToolResult> {
        let task_id = task_id.trim();
        if task_id.is_empty() {
            bail!("task_id 不能为空");
        }
        let store = self.open_task_store()?;
        let status = store
            .get_task_status(task_id)?
            .with_context(|| format!("未找到对应任务: {task_id}"))?;
        let output_path = status
            .output_path
            .clone()
            .filter(|value| !value.trim().is_empty())
            .context("任务尚未生成归档 output_path")?;
        let resolved_path = if Path::new(&output_path).is_absolute() {
            PathBuf::from(&output_path)
        } else {
            self.workspace_root.join(&output_path)
        };
        let content = fs::read_to_string(&resolved_path)
            .with_context(|| format!("读取归档文件失败: {}", resolved_path.display()))?;
        let payload = json!({
            "task_id": task_id,
            "output_path": resolved_path.display().to_string(),
            "content_chars": content.chars().count(),
            "content": content,
        });
        Ok(ToolResult {
            tool: "read_article_archive",
            output: serde_json::to_string_pretty(&payload).context("序列化归档内容结果失败")?,
        })
    }

    fn open_task_store(&self) -> Result<TaskStore> {
        let Some(path) = &self.task_store_db_path else {
            bail!("当前运行未配置 task_store 数据源，无法使用业务查询工具");
        };
        TaskStore::open(path)
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

fn render_task_status_record(record: &TaskStatusRecord) -> serde_json::Value {
    json!({
        "task_id": record.task_id,
        "article_id": record.article_id,
        "normalized_url": record.normalized_url,
        "title": record.title,
        "content_source": record.content_source,
        "page_kind": record.page_kind,
        "status": record.status,
        "retry_count": record.retry_count,
        "last_error": record.last_error,
        "output_path": record.output_path,
        "snapshot_path": record.snapshot_path,
        "created_at": record.created_at,
        "updated_at": record.updated_at,
    })
}

fn render_recent_task_record(record: &RecentTaskRecord) -> serde_json::Value {
    json!({
        "task_id": record.task_id,
        "status": record.status,
        "content_source": record.content_source,
        "page_kind": record.page_kind,
        "normalized_url": record.normalized_url,
        "updated_at": record.updated_at,
    })
}

#[cfg(test)]
mod tests {
    use super::{ToolAction, ToolRegistry};
    use crate::task_store::TaskStore;
    use serde_json::Value;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn temp_workspace() -> PathBuf {
        let root = std::env::temp_dir().join(format!("amclaw_tool_test_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("创建测试目录失败");
        root
    }

    fn temp_db_path() -> PathBuf {
        std::env::temp_dir().join(format!("amclaw_tool_test_{}.db", Uuid::new_v4()))
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

    #[test]
    fn get_task_status_reads_from_task_store() {
        let root = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let record = store
            .record_link_submission("https://example.com/tool-status")
            .expect("写入任务失败");
        let registry = ToolRegistry::with_task_store_db_path(root, Some(db_path))
            .expect("初始化 registry 失败");

        let result = registry
            .execute(ToolAction::GetTaskStatus {
                task_id: record.task_id.clone(),
            })
            .expect("查询任务状态失败");
        let payload: Value =
            serde_json::from_str(&result.output).expect("工具输出应为合法 JSON");

        assert_eq!(result.tool, "get_task_status");
        assert_eq!(payload["found"], true);
        assert_eq!(payload["task"]["task_id"], record.task_id);
        assert_eq!(payload["task"]["status"], "pending");
    }

    #[test]
    fn list_recent_and_manual_tasks_read_from_task_store() {
        let root = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let recent = store
            .record_link_submission("https://example.com/recent")
            .expect("写入最近任务失败");
        let manual = store
            .record_link_submission("https://example.com/manual")
            .expect("写入待补录任务失败");
        store
            .mark_task_awaiting_manual_input(
                &manual.task_id,
                "need manual",
                "manual_required",
                None,
                Some("browser_capture"),
            )
            .expect("更新待补录状态失败");
        let registry = ToolRegistry::with_task_store_db_path(root, Some(db_path))
            .expect("初始化 registry 失败");

        let recent_result = registry
            .execute(ToolAction::ListRecentTasks { limit: 5 })
            .expect("查询最近任务失败");
        let recent_payload: Value =
            serde_json::from_str(&recent_result.output).expect("工具输出应为合法 JSON");
        assert_eq!(recent_result.tool, "list_recent_tasks");
        assert_eq!(recent_payload["count"], 2);

        let manual_result = registry
            .execute(ToolAction::ListManualTasks { limit: 5 })
            .expect("查询待补录任务失败");
        let manual_payload: Value =
            serde_json::from_str(&manual_result.output).expect("工具输出应为合法 JSON");
        assert_eq!(manual_result.tool, "list_manual_tasks");
        assert_eq!(manual_payload["count"], 1);
        assert_eq!(manual_payload["tasks"][0]["task_id"], manual.task_id);
        assert_ne!(recent.task_id, manual.task_id);
    }

    #[test]
    fn read_article_archive_reads_output_file_from_task_status() {
        let root = temp_workspace();
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let record = store
            .record_link_submission("https://example.com/archive")
            .expect("写入任务失败");
        let archive_path = root.join("processed").join(format!("{}.md", record.task_id));
        std::fs::create_dir_all(
            archive_path
                .parent()
                .expect("归档路径应存在父目录"),
        )
        .expect("创建归档目录失败");
        std::fs::write(&archive_path, "# Archived\n\nhello archive")
            .expect("写入归档文件失败");
        store
            .mark_task_archived(
                &record.task_id,
                &archive_path.display().to_string(),
                Some("Archive Title"),
                None,
                None,
                Some("http"),
            )
            .expect("更新 archived 状态失败");

        let registry = ToolRegistry::with_task_store_db_path(root, Some(db_path))
            .expect("初始化 registry 失败");
        let result = registry
            .execute(ToolAction::ReadArticleArchive {
                task_id: record.task_id.clone(),
            })
            .expect("读取归档内容失败");
        let payload: Value =
            serde_json::from_str(&result.output).expect("工具输出应为合法 JSON");

        assert_eq!(result.tool, "read_article_archive");
        assert_eq!(payload["task_id"], record.task_id);
        assert!(payload["content"].as_str().unwrap_or("").contains("hello archive"));
    }
}
