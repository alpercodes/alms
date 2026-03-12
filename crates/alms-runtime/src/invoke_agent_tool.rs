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
                "name": {
                    "type": "string",
                    "description": "Optional persistent name for this subagent (e.g. 'reviewer', \
                                    'researcher'). When provided, the subagent retains conversation \
                                    history across invocations — subsequent calls with the same name \
                                    continue the same session. When omitted, the subagent is ephemeral \
                                    (fresh session each call)."
                },
                "system_prompt": {
                    "type": "string",
                    "description": "Optional system prompt override for the subagent. \
                                    If omitted, the subagent uses a default general-purpose prompt."
                },
                "background": {
                    "type": "boolean",
                    "description": "When true, spawn the subagent in the background and return \
                                    immediately with a task_id. Poll for the result with \
                                    get_task_result(task_id). Default: false."
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

        let subagent_name = params
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        let system_prompt = params
            .get("system_prompt")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let background = params
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if background {
            let task_id = self
                .dispatcher
                .dispatch_background(
                    task,
                    system_prompt,
                    self.parent_session_id,
                    self.parent_run_id,
                    self.parent_event_tx.clone(),
                    subagent_name,
                )
                .await
                .map_err(|e| SandboxError::Io(format!("Subagent error: {}", e)))?;

            return Ok(serde_json::json!({ "task_id": task_id.to_string() }));
        }

        let response = self
            .dispatcher
            .dispatch(
                task,
                system_prompt,
                self.parent_session_id,
                self.parent_run_id,
                self.parent_event_tx.clone(),
                subagent_name,
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
    use uuid::Uuid;

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
            _subagent_name: Option<String>,
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
                _subagent_name: Option<String>,
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

    // ── background=true tests ───────────────────────────────────────────────

    /// A dispatcher that implements dispatch_background, returning a fixed UUID.
    #[derive(Debug)]
    struct BackgroundDispatcher(Uuid);

    #[async_trait]
    impl SubagentDispatcher for BackgroundDispatcher {
        async fn dispatch(
            &self,
            _task: String,
            _system_prompt: Option<String>,
            _parent_session_id: SessionId,
            _parent_run_id: Option<RunId>,
            _parent_event_tx: Option<RuntimeEventSender>,
            _subagent_name: Option<String>,
        ) -> AlmsResult<String> {
            Ok("foreground".to_string())
        }

        async fn dispatch_background(
            &self,
            _task: String,
            _system_prompt: Option<String>,
            _parent_session_id: SessionId,
            _parent_run_id: Option<RunId>,
            _parent_event_tx: Option<RuntimeEventSender>,
            _subagent_name: Option<String>,
        ) -> AlmsResult<Uuid> {
            Ok(self.0)
        }
    }

    fn make_background_tool(task_id: Uuid) -> InvokeAgentTool {
        InvokeAgentTool::new(
            Arc::new(BackgroundDispatcher(task_id)),
            SessionId::new(),
            None,
            None,
        )
    }

    #[tokio::test]
    async fn test_background_returns_task_id() {
        let expected = Uuid::new_v4();
        let tool = make_background_tool(expected);
        let result = tool
            .execute(serde_json::json!({ "task": "do something", "background": true }))
            .await
            .unwrap();

        // Must have a "task_id" field and no "response" field
        assert!(result.get("task_id").is_some(), "missing task_id field");
        assert!(
            result.get("response").is_none(),
            "unexpected response field"
        );
        assert_eq!(result["task_id"], expected.to_string());
    }

    #[tokio::test]
    async fn test_background_false_uses_dispatch() {
        // background=false should fall through to regular dispatch
        let tool = make_tool("direct result");
        let result = tool
            .execute(serde_json::json!({ "task": "do something", "background": false }))
            .await
            .unwrap();
        assert_eq!(result["response"], "direct result");
    }

    #[tokio::test]
    async fn test_background_default_is_foreground() {
        // Omitting background should behave the same as background=false
        let tool = make_tool("default result");
        let result = tool
            .execute(serde_json::json!({ "task": "do something" }))
            .await
            .unwrap();
        assert_eq!(result["response"], "default result");
    }

    // ── name parameter tests ────────────────────────────────────────────────

    /// A dispatcher that captures the subagent_name it receives.
    #[derive(Debug)]
    struct NameCapturingDispatcher(std::sync::Mutex<Option<Option<String>>>);

    #[async_trait]
    impl SubagentDispatcher for NameCapturingDispatcher {
        async fn dispatch(
            &self,
            _task: String,
            _system_prompt: Option<String>,
            _parent_session_id: SessionId,
            _parent_run_id: Option<RunId>,
            _parent_event_tx: Option<RuntimeEventSender>,
            subagent_name: Option<String>,
        ) -> AlmsResult<String> {
            *self.0.lock().unwrap() = Some(subagent_name);
            Ok("ok".to_string())
        }
    }

    #[tokio::test]
    async fn test_name_passed_to_dispatcher() {
        let dispatcher = Arc::new(NameCapturingDispatcher(std::sync::Mutex::new(None)));
        let tool = InvokeAgentTool::new(dispatcher.clone(), SessionId::new(), None, None);
        tool.execute(serde_json::json!({ "task": "x", "name": "reviewer" }))
            .await
            .unwrap();
        let captured = dispatcher.0.lock().unwrap().take().unwrap();
        assert_eq!(captured, Some("reviewer".to_string()));
    }

    #[tokio::test]
    async fn test_empty_name_treated_as_none() {
        let dispatcher = Arc::new(NameCapturingDispatcher(std::sync::Mutex::new(None)));
        let tool = InvokeAgentTool::new(dispatcher.clone(), SessionId::new(), None, None);
        tool.execute(serde_json::json!({ "task": "x", "name": "" }))
            .await
            .unwrap();
        let captured = dispatcher.0.lock().unwrap().take().unwrap();
        assert_eq!(captured, None, "empty name should be treated as None (ephemeral)");
    }

    #[tokio::test]
    async fn test_missing_name_is_none() {
        let dispatcher = Arc::new(NameCapturingDispatcher(std::sync::Mutex::new(None)));
        let tool = InvokeAgentTool::new(dispatcher.clone(), SessionId::new(), None, None);
        tool.execute(serde_json::json!({ "task": "x" }))
            .await
            .unwrap();
        let captured = dispatcher.0.lock().unwrap().take().unwrap();
        assert_eq!(captured, None);
    }
}
