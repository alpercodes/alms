//! invoke_agent tool — lets a running agent spawn a subagent via the Coordinator.

use crate::events::RuntimeEventSender;
use crate::subagent::SubagentDispatcher;
use alms_core::{RunId, SessionId};
use alms_sandbox::{SandboxError, Tool, error::SandboxResult};
use serde_json::Value;
use std::sync::Arc;

/// Built-in tool that spawns a subagent and awaits its result.
///
/// The subagent runs its own full AgentRuntime loop with its own tool set
/// and system prompt, then returns its final text response as the tool result.
#[derive(Debug)]
pub struct InvokeAgentTool {
    dispatcher: Arc<dyn SubagentDispatcher>,
    parent_session_id: SessionId,
    parent_run_id: Option<RunId>,
    /// Clone of the parent run's event sender so subagent tool events
    /// are forwarded into the parent's SSE stream.
    parent_event_tx: Option<RuntimeEventSender>,
}

impl InvokeAgentTool {
    pub fn new(
        dispatcher: Arc<dyn SubagentDispatcher>,
        parent_session_id: SessionId,
        parent_run_id: Option<RunId>,
        parent_event_tx: Option<RuntimeEventSender>,
    ) -> Self {
        Self {
            dispatcher,
            parent_session_id,
            parent_run_id,
            parent_event_tx,
        }
    }
}

#[async_trait::async_trait]
impl Tool for InvokeAgentTool {
    fn name(&self) -> &str {
        "invoke_agent"
    }

    fn description(&self) -> &str {
        "Spawn a subagent to handle a specific task. The subagent runs its own \
         independent LLM loop and returns its final response. Use this to delegate \
         specialised work or run subtasks that need their own reasoning loop."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "The task description for the subagent to complete."
                },
                "system_prompt": {
                    "type": "string",
                    "description": "Optional system prompt override for the subagent. \
                                    If omitted, the subagent uses a default general-purpose prompt."
                }
            },
            "required": ["task"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let task = params
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SandboxError::InvalidParameters("'task' is required".to_string()))?
            .to_string();

        let system_prompt = params
            .get("system_prompt")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let response = self
            .dispatcher
            .dispatch(
                task,
                system_prompt,
                self.parent_session_id,
                self.parent_run_id,
                self.parent_event_tx.clone(),
            )
            .await
            .map_err(|e| SandboxError::Io(format!("Subagent error: {}", e)))?;

        Ok(serde_json::json!({ "response": response }))
    }

    fn is_builtin(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alms_core::{AlmsResult, RunId, SessionId};
    use async_trait::async_trait;

    #[derive(Debug)]
    struct MockDispatcher(String);

    #[async_trait]
    impl SubagentDispatcher for MockDispatcher {
        async fn dispatch(
            &self,
            _task: String,
            _system_prompt: Option<String>,
            _parent_session_id: SessionId,
            _parent_run_id: Option<RunId>,
            _parent_event_tx: Option<RuntimeEventSender>,
        ) -> AlmsResult<String> {
            Ok(self.0.clone())
        }
    }

    fn make_tool(response: &str) -> InvokeAgentTool {
        InvokeAgentTool::new(
            Arc::new(MockDispatcher(response.to_string())),
            SessionId::new(),
            None,
            None,
        )
    }

    #[tokio::test]
    async fn test_invoke_returns_response() {
        let tool = make_tool("subagent done");
        let result = tool
            .execute(serde_json::json!({ "task": "do something" }))
            .await
            .unwrap();
        assert_eq!(result["response"], "subagent done");
    }

    #[tokio::test]
    async fn test_missing_task_is_error() {
        let tool = make_tool("ok");
        let err = tool.execute(serde_json::json!({})).await.unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }

    #[tokio::test]
    async fn test_system_prompt_optional() {
        // Should succeed without system_prompt
        let tool = make_tool("response");
        let result = tool
            .execute(serde_json::json!({ "task": "research something" }))
            .await
            .unwrap();
        assert_eq!(result["response"], "response");
    }

    #[tokio::test]
    async fn test_dispatcher_error_propagates() {
        #[derive(Debug)]
        struct FailDispatcher;
        #[async_trait]
        impl SubagentDispatcher for FailDispatcher {
            async fn dispatch(
                &self,
                _task: String,
                _system_prompt: Option<String>,
                _parent_session_id: SessionId,
                _parent_run_id: Option<RunId>,
                _parent_event_tx: Option<RuntimeEventSender>,
            ) -> AlmsResult<String> {
                Err(alms_core::AlmsError::Runtime("subagent failed".to_string()))
            }
        }
        let tool = InvokeAgentTool::new(Arc::new(FailDispatcher), SessionId::new(), None, None);
        let err = tool
            .execute(serde_json::json!({ "task": "fail" }))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::Io(_)));
    }

    #[test]
    fn test_schema_has_required_task() {
        let tool = make_tool("x");
        let schema = tool.parameters();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "task"));
    }
}
