//! invoke_agent tool — lets a running agent spawn a subagent via the Coordinator.

use crate::event_forwarder::EventForwarder;
use crate::subagent::SubagentDispatcher;
use alms_core::{AgentId, RunId, SessionId};
use alms_sandbox::{SandboxError, Tool, error::SandboxResult};
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Built-in tool that spawns a subagent and awaits its result.
///
/// The subagent runs its own full AgentRuntime loop with its own tool set
/// and system prompt, then returns its final text response as the tool result.
#[derive(Debug)]
pub struct InvokeAgentTool {
    dispatcher: Arc<dyn SubagentDispatcher>,
    parent_session_id: SessionId,
    /// Parent agent's persistent ID — drives the `(parent_agent_id, name)`
    /// keying for named subagent sessions (#1051).
    parent_agent_id: AgentId,
    parent_run_id: Option<RunId>,
    /// Clone of the parent run's event sender so subagent tool events
    /// are forwarded into the parent's SSE stream.
    parent_event_tx: Option<Arc<dyn EventForwarder>>,
    /// Parent run's cancellation token — threaded into subagent dispatch so
    /// cancelling the parent also cancels active subagents.
    parent_cancel_token: Option<CancellationToken>,
    /// Separate event sender for background subagents. Goes to session
    /// subscribers directly (not through the parent's runtime channel)
    /// so it doesn't block the parent run from finishing.
    background_event_tx: Option<Arc<dyn EventForwarder>>,
}

impl InvokeAgentTool {
    pub fn new(
        dispatcher: Arc<dyn SubagentDispatcher>,
        parent_session_id: SessionId,
        parent_agent_id: AgentId,
        parent_run_id: Option<RunId>,
        parent_event_tx: Option<Arc<dyn EventForwarder>>,
    ) -> Self {
        Self {
            dispatcher,
            parent_session_id,
            parent_agent_id,
            parent_run_id,
            parent_event_tx,
            parent_cancel_token: None,
            background_event_tx: None,
        }
    }

    /// Attach a separate event forwarder for background subagent dispatch.
    /// Events from background subagents flow to session subscribers directly.
    pub fn with_background_event_fwd(mut self, fwd: Arc<dyn EventForwarder>) -> Self {
        self.background_event_tx = Some(fwd);
        self
    }

    /// Attach the parent run's cancellation token so that cancelling the parent
    /// propagates to all subagents spawned by this tool.
    pub fn with_cancel_token(mut self, token: CancellationToken) -> Self {
        self.parent_cancel_token = Some(token);
        self
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
                    "description": "The name of a registered agent to invoke (e.g. 'reviewer', \
                                    'researcher'). The agent must be pre-created via \
                                    `alms agent create --name <name>`. Named agents retain \
                                    conversation history and workspace files across invocations. \
                                    When omitted, an ephemeral subagent is created (fresh session, \
                                    no workspace, default config)."
                },
                "background": {
                    "type": "boolean",
                    "description": "When true, spawn the subagent in the background and return \
                                    immediately. You will be automatically notified when the \
                                    subagent completes. Default: false."
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

        // Validate name against agent naming rules to prevent bad filesystem paths
        if let Some(ref name) = subagent_name {
            alms_core::validate_agent_name(name).map_err(|e| {
                SandboxError::InvalidParameters(format!("invalid subagent name: {e}"))
            })?;
        }

        let background = params
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if background {
            // Background subagents use a SEPARATE event sender that goes to
            // session subscribers directly (not through the parent's runtime
            // channel). This provides live activity for the SubagentBar
            // without blocking the parent run from finishing. See #231.
            let (task_id, sub_session_id) = self
                .dispatcher
                .dispatch_background(
                    task,
                    self.parent_session_id,
                    self.parent_agent_id,
                    self.parent_run_id,
                    self.background_event_tx.clone(),
                    subagent_name,
                    self.parent_cancel_token.clone(),
                )
                .await
                .map_err(SandboxError::from)?;

            return Ok(serde_json::json!({
                "task_id": task_id.to_string(),
                "session_id": sub_session_id.0.to_string(),
            }));
        }

        let (response, sub_session_id) = self
            .dispatcher
            .dispatch(
                task,
                self.parent_session_id,
                self.parent_agent_id,
                self.parent_run_id,
                self.parent_event_tx.clone(),
                subagent_name,
                self.parent_cancel_token.clone(),
            )
            .await
            .map_err(SandboxError::from)?;

        Ok(serde_json::json!({
            "response": response,
            "session_id": sub_session_id.0.to_string(),
        }))
    }

    fn is_builtin(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alms_core::{AgentId, AlmsResult, RunId, SessionId};
    use async_trait::async_trait;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    #[derive(Debug)]
    struct MockDispatcher(String);

    #[async_trait]
    impl SubagentDispatcher for MockDispatcher {
        async fn dispatch(
            &self,
            _task: String,
            _parent_session_id: SessionId,
            _parent_agent_id: AgentId,
            _parent_run_id: Option<RunId>,
            _parent_event_tx: Option<Arc<dyn EventForwarder>>,
            _subagent_name: Option<String>,
            _parent_cancel_token: Option<CancellationToken>,
        ) -> AlmsResult<(String, SessionId)> {
            Ok((self.0.clone(), SessionId::new()))
        }
    }

    fn make_tool(response: &str) -> InvokeAgentTool {
        InvokeAgentTool::new(
            Arc::new(MockDispatcher(response.to_string())),
            SessionId::new(),
            AgentId::new(),
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
        assert!(
            result.get("session_id").is_some(),
            "foreground result should include session_id"
        );
    }

    #[tokio::test]
    async fn test_missing_task_is_error() {
        let tool = make_tool("ok");
        let err = tool.execute(serde_json::json!({})).await.unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }

    #[tokio::test]
    async fn test_dispatcher_error_propagates() {
        // #920: dispatcher errors must propagate as the structured
        // `SandboxError::Subagent(Box<AlmsError>)` variant — *not*
        // stringified into `SandboxError::Io(format!("Subagent error:
        // {e}"))`. The runtime tool registry's catch-all unwraps the
        // box back to the typed `AlmsError` so the parent agent's
        // `tool_result` reads as one tractable line instead of the
        // legacy 4-prefix wrap. This test pins that boundary.
        #[derive(Debug)]
        struct FailDispatcher;
        #[async_trait]
        impl SubagentDispatcher for FailDispatcher {
            async fn dispatch(
                &self,
                _task: String,
                _parent_session_id: SessionId,
                _parent_agent_id: AgentId,
                _parent_run_id: Option<RunId>,
                _parent_event_tx: Option<Arc<dyn EventForwarder>>,
                _subagent_name: Option<String>,
                _parent_cancel_token: Option<CancellationToken>,
            ) -> AlmsResult<(String, SessionId)> {
                Err(alms_core::AlmsError::SubagentLlmError {
                    provider: "anthropic".to_string(),
                    status: 400,
                    body: r#"{"error":{"message":"prompt is too long"}}"#.to_string(),
                })
            }
        }
        let tool = InvokeAgentTool::new(
            Arc::new(FailDispatcher),
            SessionId::new(),
            AgentId::new(),
            None,
            None,
        );
        let err = tool
            .execute(serde_json::json!({ "task": "fail" }))
            .await
            .unwrap_err();
        // Pin the variant — must be `Subagent(...)`, not `Io(...)`.
        let inner = match err {
            SandboxError::Subagent(boxed) => *boxed,
            other => panic!("expected SandboxError::Subagent, got {other:?}"),
        };
        // Pin the inner `AlmsError` shape — the typed
        // `SubagentLlmError` triple must arrive at the SandboxError
        // boundary verbatim, ready for `ToolRegistry::execute` to
        // unwrap it back to the parent's tool_result message.
        match inner {
            alms_core::AlmsError::SubagentLlmError {
                provider,
                status,
                body,
            } => {
                assert_eq!(provider, "anthropic");
                assert_eq!(status, 400);
                assert!(body.contains("prompt is too long"));
            }
            other => panic!("expected inner SubagentLlmError, got {other:?}"),
        }
    }

    /// #920: a generic (non-SubagentLlmError) `AlmsError` from the
    /// dispatcher must also flow through `SandboxError::Subagent` so the
    /// `From` boundary is the same regardless of the underlying variant.
    /// The runtime side then propagates the typed variant verbatim.
    #[tokio::test]
    async fn test_dispatcher_generic_error_uses_structured_subagent_variant() {
        #[derive(Debug)]
        struct GenericFailDispatcher;
        #[async_trait]
        impl SubagentDispatcher for GenericFailDispatcher {
            async fn dispatch(
                &self,
                _task: String,
                _parent_session_id: SessionId,
                _parent_agent_id: AgentId,
                _parent_run_id: Option<RunId>,
                _parent_event_tx: Option<Arc<dyn EventForwarder>>,
                _subagent_name: Option<String>,
                _parent_cancel_token: Option<CancellationToken>,
            ) -> AlmsResult<(String, SessionId)> {
                Err(alms_core::AlmsError::Runtime(
                    "downstream blew up".to_string(),
                ))
            }
        }
        let tool = InvokeAgentTool::new(
            Arc::new(GenericFailDispatcher),
            SessionId::new(),
            AgentId::new(),
            None,
            None,
        );
        let err = tool
            .execute(serde_json::json!({ "task": "fail" }))
            .await
            .unwrap_err();
        assert!(
            matches!(err, SandboxError::Subagent(_)),
            "expected SandboxError::Subagent, got {err:?}"
        );
        // Defense-in-depth: the legacy `IO error: Subagent error:`
        // prefix must not appear in the rendered string.
        let s = err.to_string();
        assert!(
            !s.contains("IO error:"),
            "structured Subagent variant must not render as IO error, got: {s}"
        );
        assert!(
            !s.contains("Subagent error:"),
            "structured Subagent variant must not have legacy prefix, got: {s}"
        );
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
            _parent_session_id: SessionId,
            _parent_agent_id: AgentId,
            _parent_run_id: Option<RunId>,
            _parent_event_tx: Option<Arc<dyn EventForwarder>>,
            _subagent_name: Option<String>,
            _parent_cancel_token: Option<CancellationToken>,
        ) -> AlmsResult<(String, SessionId)> {
            Ok(("foreground".to_string(), SessionId::new()))
        }

        async fn dispatch_background(
            &self,
            _task: String,
            _parent_session_id: SessionId,
            _parent_agent_id: AgentId,
            _parent_run_id: Option<RunId>,
            _parent_event_tx: Option<Arc<dyn EventForwarder>>,
            _subagent_name: Option<String>,
            _parent_cancel_token: Option<CancellationToken>,
        ) -> AlmsResult<(Uuid, SessionId)> {
            Ok((self.0, SessionId::new()))
        }
    }

    fn make_background_tool(task_id: Uuid) -> InvokeAgentTool {
        InvokeAgentTool::new(
            Arc::new(BackgroundDispatcher(task_id)),
            SessionId::new(),
            AgentId::new(),
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

        // Must have "task_id" and "session_id" fields, and no "response" field
        assert!(result.get("task_id").is_some(), "missing task_id field");
        assert!(
            result.get("session_id").is_some(),
            "missing session_id field in background result"
        );
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
            _parent_session_id: SessionId,
            _parent_agent_id: AgentId,
            _parent_run_id: Option<RunId>,
            _parent_event_tx: Option<Arc<dyn EventForwarder>>,
            subagent_name: Option<String>,
            _parent_cancel_token: Option<CancellationToken>,
        ) -> AlmsResult<(String, SessionId)> {
            *self.0.lock().unwrap() = Some(subagent_name);
            Ok(("ok".to_string(), SessionId::new()))
        }
    }

    #[tokio::test]
    async fn test_name_passed_to_dispatcher() {
        let dispatcher = Arc::new(NameCapturingDispatcher(std::sync::Mutex::new(None)));
        let tool = InvokeAgentTool::new(
            dispatcher.clone(),
            SessionId::new(),
            AgentId::new(),
            None,
            None,
        );
        tool.execute(serde_json::json!({ "task": "x", "name": "reviewer" }))
            .await
            .unwrap();
        let captured = dispatcher.0.lock().unwrap().take().unwrap();
        assert_eq!(captured, Some("reviewer".to_string()));
    }

    #[tokio::test]
    async fn test_empty_name_treated_as_none() {
        let dispatcher = Arc::new(NameCapturingDispatcher(std::sync::Mutex::new(None)));
        let tool = InvokeAgentTool::new(
            dispatcher.clone(),
            SessionId::new(),
            AgentId::new(),
            None,
            None,
        );
        tool.execute(serde_json::json!({ "task": "x", "name": "" }))
            .await
            .unwrap();
        let captured = dispatcher.0.lock().unwrap().take().unwrap();
        assert_eq!(
            captured, None,
            "empty name should be treated as None (ephemeral)"
        );
    }

    #[tokio::test]
    async fn test_missing_name_is_none() {
        let dispatcher = Arc::new(NameCapturingDispatcher(std::sync::Mutex::new(None)));
        let tool = InvokeAgentTool::new(
            dispatcher.clone(),
            SessionId::new(),
            AgentId::new(),
            None,
            None,
        );
        tool.execute(serde_json::json!({ "task": "x" }))
            .await
            .unwrap();
        let captured = dispatcher.0.lock().unwrap().take().unwrap();
        assert_eq!(captured, None);
    }

    #[tokio::test]
    async fn test_invalid_name_rejected() {
        let tool = make_tool("ok");
        // Uppercase not allowed
        let err = tool
            .execute(serde_json::json!({ "task": "x", "name": "Bad Name!" }))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }

    #[tokio::test]
    async fn test_valid_name_accepted() {
        let tool = make_tool("ok");
        let result = tool
            .execute(serde_json::json!({ "task": "x", "name": "my-agent" }))
            .await;
        assert!(result.is_ok());
    }
}
