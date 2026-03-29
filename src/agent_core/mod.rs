use crate::tool_registry::{ToolAction, ToolRegistry};
use anyhow::{bail, Context, Result};
use std::path::PathBuf;

const DEFAULT_MAX_STEPS: usize = 8;

#[derive(Debug)]
pub struct AgentCore {
    // 负责实际执行工具动作（读写文件等）
    tool_registry: ToolRegistry,
    // 防止 Agent 无穷循环的安全阀
    max_steps: usize,
}

impl AgentCore {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Result<Self> {
        Self::with_max_steps(workspace_root, DEFAULT_MAX_STEPS)
    }

    pub fn with_max_steps(workspace_root: impl Into<PathBuf>, max_steps: usize) -> Result<Self> {
        if max_steps == 0 {
            bail!("max_steps 必须大于 0");
        }
        Ok(Self {
            tool_registry: ToolRegistry::new(workspace_root)?,
            max_steps,
        })
    }

    pub fn run(&self, user_input: &str) -> Result<String> {
        let mut tool_result: Option<String> = None;
        // 最小 Agent Loop: 决策 -> 执行工具 -> 继续决策/结束
        for step in 0..self.max_steps {
            let decision = self.decide(user_input, tool_result.as_deref(), step)?;
            match decision {
                AgentDecision::CallTool(action) => {
                    let result = self.tool_registry.execute(action)?;
                    tool_result = Some(result.output);
                }
                AgentDecision::Final(answer) => return Ok(answer),
            }
        }
        bail!("达到最大步骤，未能收敛")
    }

    fn decide(
        &self,
        user_input: &str,
        tool_result: Option<&str>,
        step: usize,
    ) -> Result<AgentDecision> {
        // 首轮根据用户输入决定是否调用工具
        if step == 0 {
            return parse_user_command(user_input);
        }

        // 有工具结果就直接收敛为最终回答
        if let Some(result) = tool_result {
            return Ok(AgentDecision::Final(format!("完成: {}", result.trim())));
        }

        Ok(AgentDecision::Final("没有可执行的动作".to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AgentDecision {
    // 继续行动：调用一个工具
    CallTool(ToolAction),
    // 结束循环：直接返回用户可读结果
    Final(String),
}

fn parse_user_command(input: &str) -> Result<AgentDecision> {
    let text = normalize_user_command(input);
    if let Some(path) = text.strip_prefix("读文件 ") {
        return Ok(AgentDecision::CallTool(ToolAction::Read {
            path: path.trim().to_string(),
        }));
    }
    if let Some(rest) = text.strip_prefix("创建文件 ") {
        let (path, content) = split_path_and_content(rest)?;
        return Ok(AgentDecision::CallTool(ToolAction::Create {
            path,
            content,
        }));
    }
    if let Some(rest) = text.strip_prefix("写文件 ") {
        let (path, content) = split_path_and_content(rest)?;
        return Ok(AgentDecision::CallTool(ToolAction::Write { path, content }));
    }
    if let Some(path) = text.strip_prefix("read ") {
        return Ok(AgentDecision::CallTool(ToolAction::Read {
            path: path.trim().to_string(),
        }));
    }
    if let Some(rest) = text.strip_prefix("create ") {
        let (path, content) = split_path_and_content(rest)?;
        return Ok(AgentDecision::CallTool(ToolAction::Create {
            path,
            content,
        }));
    }
    if let Some(rest) = text.strip_prefix("write ") {
        let (path, content) = split_path_and_content(rest)?;
        return Ok(AgentDecision::CallTool(ToolAction::Write { path, content }));
    }

    bail!(
        "无法解析指令。可用格式: 读文件 <path> | 创建文件 <path> :: <content> | 写文件 <path> :: <content>"
    )
}

fn normalize_user_command(input: &str) -> &str {
    let text = input.trim();
    if let Some(rest) = text.strip_prefix("帮我运行：") {
        return rest.trim();
    }
    if let Some(rest) = text.strip_prefix("帮我运行:") {
        return rest.trim();
    }
    if let Some(rest) = text.strip_prefix("请帮我运行：") {
        return rest.trim();
    }
    if let Some(rest) = text.strip_prefix("请帮我运行:") {
        return rest.trim();
    }
    text
}

fn split_path_and_content(raw: &str) -> Result<(String, String)> {
    // 约定格式：<path> :: <content>
    let (path, content) = raw
        .split_once("::")
        .context("写入/创建指令格式错误，缺少 :: 分隔符")?;
    let path = path.trim().to_string();
    if path.is_empty() {
        bail!("文件路径不能为空");
    }
    Ok((path, content.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::AgentCore;
    use uuid::Uuid;

    fn temp_workspace() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("amclaw_agent_test_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("创建测试目录失败");
        root
    }

    #[test]
    fn loop_create_then_read() {
        let root = temp_workspace();
        let agent = AgentCore::new(root).expect("初始化 agent 失败");

        let create = agent
            .run("创建文件 demo/hello.txt :: 你好 AMClaw")
            .expect("创建文件失败");
        assert!(create.contains("完成:"));

        let read = agent.run("读文件 demo/hello.txt").expect("读取文件失败");
        assert!(read.contains("你好 AMClaw"));
    }

    #[test]
    fn invalid_command_returns_error() {
        let root = temp_workspace();
        let agent = AgentCore::new(root).expect("初始化 agent 失败");
        let err = agent.run("unknown command").expect_err("应当返回错误");
        assert!(err.to_string().contains("无法解析指令"));
    }

    #[test]
    fn one_step_is_not_enough_for_tool_then_finalize() {
        let root = temp_workspace();
        let agent = AgentCore::with_max_steps(root, 1).expect("初始化 agent 失败");
        let err = agent
            .run("创建文件 demo/hello.txt :: 你好")
            .expect_err("单步应当无法收敛");
        assert!(err.to_string().contains("达到最大步骤"));
    }

    #[test]
    fn prefix_command_is_supported() {
        let root = temp_workspace();
        let agent = AgentCore::new(root).expect("初始化 agent 失败");
        let result = agent
            .run("帮我运行：创建文件 demo/prefix.txt :: prefix ok")
            .expect("前缀命令执行失败");
        assert!(result.contains("完成:"));
    }
}
