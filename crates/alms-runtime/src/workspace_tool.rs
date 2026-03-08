//! workspace_write tool — lets the agent update its own goals.md and memories.md.

use crate::workspace::{AgentWorkspace, WorkspaceFile};
use alms_sandbox::{SandboxError, Tool, error::SandboxResult};
use serde_json::Value;

/// Built-in tool that lets the agent write to its own workspace files.
///
/// The agent may write to `goals.md` and `memories.md` only.
/// `personality.md` is user-only and will be rejected with an error.
#[derive(Debug, Clone)]
pub struct WorkspaceWriteTool {
    workspace: AgentWorkspace,
}

impl WorkspaceWriteTool {
    pub fn new(workspace: AgentWorkspace) -> Self {
        Self { workspace }
    }
}

#[async_trait::async_trait]
impl Tool for WorkspaceWriteTool {
    fn name(&self) -> &str {
        "workspace_write"
    }

    fn description(&self) -> &str {
        "Write or append to the agent's own workspace files (goals.md or memories.md). \
         Use this to persist goals and memories across conversations."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "enum": ["goals", "memories"],
                    "description": "Which workspace file to write. \
                                    'goals' for current objectives, \
                                    'memories' for learned facts and preferences."
                },
                "content": {
                    "type": "string",
                    "description": "The content to write."
                },
                "mode": {
                    "type": "string",
                    "enum": ["write", "append"],
                    "description": "Whether to overwrite the file ('write') or add to it ('append'). \
                                    Defaults to 'write'."
                }
            },
            "required": ["file", "content"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let file_str = params
            .get("file")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SandboxError::InvalidParameters("'file' is required".to_string()))?;

        let workspace_file = match file_str {
            "goals" => WorkspaceFile::Goals,
            "memories" => WorkspaceFile::Memories,
            "personality" => {
                return Err(SandboxError::InvalidParameters(
                    "File 'personality' is read-only (user-managed only)".to_string(),
                ));
            }
            other => {
                return Err(SandboxError::InvalidParameters(format!(
                    "Unknown file '{}': must be 'goals' or 'memories'",
                    other
                )));
            }
        };

        let content = params
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SandboxError::InvalidParameters("'content' is required".to_string()))?;

        let mode = params
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("write");

        match mode {
            "write" => self.workspace.write_file(workspace_file, content)?,
            "append" => self.workspace.append_file(workspace_file, content)?,
            other => {
                return Err(SandboxError::InvalidParameters(format!(
                    "Unknown mode '{}': must be 'write' or 'append'",
                    other
                )));
            }
        }

        Ok(serde_json::json!({
            "ok": true,
            "file": file_str,
            "mode": mode,
        }))
    }

    fn is_builtin(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alms_core::AgentId;
    use tempfile::TempDir;

    fn test_tool() -> (TempDir, WorkspaceWriteTool) {
        let dir = TempDir::new().unwrap();
        let workspace = AgentWorkspace::new(dir.path(), AgentId::new());
        let tool = WorkspaceWriteTool::new(workspace);
        (dir, tool)
    }

    #[tokio::test]
    async fn test_write_goals() {
        let (_dir, tool) = test_tool();
        let result = tool
            .execute(serde_json::json!({
                "file": "goals",
                "content": "Build the MVP"
            }))
            .await
            .unwrap();
        assert_eq!(result["ok"], true);

        let content = tool.workspace.read_file(WorkspaceFile::Goals).unwrap();
        assert_eq!(content, "Build the MVP");
    }

    #[tokio::test]
    async fn test_append_memories() {
        let (_dir, tool) = test_tool();
        tool.execute(serde_json::json!({
            "file": "memories",
            "content": "Fact 1",
            "mode": "write"
        }))
        .await
        .unwrap();
        tool.execute(serde_json::json!({
            "file": "memories",
            "content": "Fact 2",
            "mode": "append"
        }))
        .await
        .unwrap();

        let content = tool.workspace.read_file(WorkspaceFile::Memories).unwrap();
        assert!(content.contains("Fact 1"));
        assert!(content.contains("Fact 2"));
    }

    #[tokio::test]
    async fn test_personality_rejected_with_clear_message() {
        let (_dir, tool) = test_tool();
        let err = tool
            .execute(serde_json::json!({
                "file": "personality",
                "content": "overwrite"
            }))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
        assert!(err.to_string().contains("read-only"));
    }

    #[tokio::test]
    async fn test_unknown_file_rejected() {
        let (_dir, tool) = test_tool();
        let err = tool
            .execute(serde_json::json!({
                "file": "secrets",
                "content": "oops"
            }))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }

    #[tokio::test]
    async fn test_missing_content_rejected() {
        let (_dir, tool) = test_tool();
        let err = tool
            .execute(serde_json::json!({"file": "goals"}))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }

    #[test]
    fn test_parameters_schema_has_required() {
        let (_dir, tool) = test_tool();
        let schema = tool.parameters();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "file"));
        assert!(required.iter().any(|v| v == "content"));
    }
}
