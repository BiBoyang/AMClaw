use crate::task_store::PendingTaskRecord;
use anyhow::{Context, Result};
use chrono::Utc;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Pipeline {
    root_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineResult {
    pub output_path: PathBuf,
}

impl Pipeline {
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
        }
    }

    pub fn process_pending_task(&self, task: &PendingTaskRecord) -> Result<PipelineResult> {
        let day = Utc::now().format("%Y-%m-%d").to_string();
        let output_dir = self.root_dir.join("processed").join(day);
        fs::create_dir_all(&output_dir)
            .with_context(|| format!("创建归档目录失败: {}", output_dir.display()))?;

        let output_path = output_dir.join(format!("{}.md", task.task_id));
        let content = format!(
            "# Archived Link\n\n- task_id: {}\n- article_id: {}\n- normalized_url: {}\n- original_url: {}\n- archived_at: {}\n",
            task.task_id,
            task.article_id,
            task.normalized_url,
            task.original_url,
            Utc::now().to_rfc3339()
        );
        fs::write(&output_path, content)
            .with_context(|| format!("写入归档文件失败: {}", output_path.display()))?;

        Ok(PipelineResult { output_path })
    }
}

#[cfg(test)]
mod tests {
    use super::Pipeline;
    use crate::task_store::PendingTaskRecord;
    use std::fs;
    use uuid::Uuid;

    fn temp_dir() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("amclaw_pipeline_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("创建测试目录失败");
        root
    }

    #[test]
    fn processing_pending_task_creates_markdown_file() {
        let root = temp_dir();
        let pipeline = Pipeline::new(&root);
        let task = PendingTaskRecord {
            task_id: "task-1".to_string(),
            article_id: "article-1".to_string(),
            normalized_url: "https://example.com".to_string(),
            original_url: "https://example.com".to_string(),
        };

        let result = pipeline
            .process_pending_task(&task)
            .expect("处理 pending 任务失败");
        let content = fs::read_to_string(&result.output_path).expect("读取归档文件失败");

        assert!(result.output_path.starts_with(root.join("processed")));
        assert!(content.contains("task-1"));
        assert!(content.contains("https://example.com"));
    }
}
