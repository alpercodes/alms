//! get_task_result tool — poll a background subagent spawned via invoke_agent.

use crate::subagent::{PollResult, SubagentDispatcher};
use alms_sandbox::{SandboxError, Tool, error::SandboxResult};
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

/// Built-in tool that polls for the result of a background subagent task.
///
/// Background tasks are started with `invoke_agent(task, background=true)`,
/// which returns a `task_id`. This tool accepts that `task_id` and returns
/// either `{"status":"running"}` (not yet done) or the final result.
#[derive(Debug)]
pub struct GetTaskResultTool {
    dispatcher: Arc<dyn SubagentDispatcher>,
}

impl GetTaskResultTool {
    pub fn new(dispatcher: Arc<dyn SubagentDispatcher>) -> Self {
        Self { dispatcher }
    }
}

#[async_trait::async_trait]
impl Tool for GetTaskResultTool {
    fn name(&self) -> &str {
        "get_task_result"
    }

    fn description(&self) -> &str {
        "Poll for the result of a background subagent task started with \
         invoke_agent (background=true). Returns {\"status\":\"running\"} while \
         still in progress, {\"status\":\"completed\",\"result\":\"...\"} when done, \
         or {\"status\":\"failed\",\"error\":\"...\"} on failure."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The task ID returned by invoke_agent when background=true."
                }
            },
            "required": ["task_id"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let task_id_str = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SandboxError::InvalidParameters("'task_id' is required".to_string()))?;

        let task_id = Uuid::parse_str(task_id_str).map_err(|_| {
            SandboxError::InvalidParameters(format!("Invalid UUID: {}", task_id_str))
        })?;

        let poll = self
            .dispatcher
            .poll_task(task_id)
            .await
            .map_err(|e| SandboxError::Io(format!("Poll error: {}", e)))?;

        Ok(match poll {
            PollResult::Running => serde_json::json!({ "status": "running" }),
            PollResult::Completed(text) => {
                serde_json::json!({ "status": "completed", "result": text })
            }
            PollResult::Failed(msg) => serde_json::json!({ "status": "failed", "error": msg }),
            PollResult::Cancelled => serde_json::json!({ "status": "cancelled" }),
        })
    }

    fn is_builtin(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::RuntimeEventSender;
    use alms_core::{AlmsResult, RunId, SessionId};
    use async_trait::async_trait;

    #[derive(Debug)]
    struct MockDispatcher(PollResult);

    #[async_trait]
    impl SubagentDispatcher for MockDispatcher {
        async fn dispatch(
            &self,
            _task: String,
            _parent_session_id: SessionId,
            _parent_run_id: Option<RunId>,
            _parent_event_tx: Option<RuntimeEventSender>,
            _subagent_name: Option<String>,
        ) -> AlmsResult<String> {
            Ok("ok".to_string())
        }

        async fn poll_task(&self, _task_id: Uuid) -> AlmsResult<PollResult> {
            Ok(self.0.clone())
        }
    }

    fn make_tool(result: PollResult) -> GetTaskResultTool {
        GetTaskResultTool::new(Arc::new(MockDispatcher(result)))
    }

    fn valid_uuid() -> String {
        Uuid::new_v4().to_string()
    }

    #[tokio::test]
    async fn test_running_status() {
        let tool = make_tool(PollResult::Running);
        let r = tool
            .execute(serde_json::json!({ "task_id": valid_uuid() }))
            .await
            .unwrap();
        assert_eq!(r["status"], "running");
    }

    #[tokio::test]
    async fn test_completed_status() {
        let tool = make_tool(PollResult::Completed("done!".to_string()));
        let r = tool
            .execute(serde_json::json!({ "task_id": valid_uuid() }))
            .await
            .unwrap();
        assert_eq!(r["status"], "completed");
        assert_eq!(r["result"], "done!");
    }

    #[tokio::test]
    async fn test_failed_status() {
        let tool = make_tool(PollResult::Failed("boom".to_string()));
        let r = tool
            .execute(serde_json::json!({ "task_id": valid_uuid() }))
            .await
            .unwrap();
        assert_eq!(r["status"], "failed");
        assert_eq!(r["error"], "boom");
    }

    #[tokio::test]
    async fn test_missing_task_id() {
        let tool = make_tool(PollResult::Running);
        let err = tool.execute(serde_json::json!({})).await.unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }

    #[tokio::test]
    async fn test_invalid_uuid() {
        let tool = make_tool(PollResult::Running);
        let err = tool
            .execute(serde_json::json!({ "task_id": "not-a-uuid" }))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }
}
