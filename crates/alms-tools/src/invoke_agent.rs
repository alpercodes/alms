//! invoke_agent tool — lets a running agent spawn a subagent via the Coordinator.

use crate::event_forwarder::EventForwarder;
use crate::subagent::SubagentDispatcher;
use alms_core::{AgentId, RunId, SessionId};
use alms_sandbox::{SandboxError, Tool, ToolContext, error::SandboxResult};
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

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
        "Delegate a task to a subagent: it runs its own independent LLM loop and \
         returns its final response to you. This creates a SUBORDINATE \
         relationship -- the work belongs to your run: you set the task, the \
         result lands in your context, and the subagent's transcript is yours \
         to read (read_subagent_session admits only the agent that invoked \
         it). Use it for work you are directing and will report on. When you \
         are addressing a PEER instead -- asking another agent for a review or \
         an opinion, or anything where the answer is theirs and the exchange \
         may go back and forth -- use send_message: that starts a conversation \
         the other agent owns, and their reply comes back to you as a new run \
         rather than as this tool's result."
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
                    "description": "The name of a registered agent to assign the task to \
                                    (e.g. 'researcher', 'summarizer'). Pick a name for work \
                                    you are handing down; to ask a peer for something (a \
                                    review, an opinion) use send_message instead. The agent \
                                    must be pre-created via `alms agent create --name <name>`. \
                                    Named agents retain conversation history and workspace \
                                    files across invocations. When omitted, an ephemeral \
                                    subagent is created (fresh session, no workspace, \
                                    default config)."
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
        // Legacy path with no per-call context — falls through with
        // `parent_tool_invocation_id: None`. The agent runtime routes
        // every real invoke_agent call through `execute_with_context`
        // (#1105); only synthetic call sites (a few unit tests in this
        // file, future external callers) end up here, where the
        // `subagent_started` event is harmless to skip because the
        // legacy completion path still delivers the session id.
        self.execute_inner(params, None).await
    }

    async fn execute_with_context(&self, params: Value, ctx: ToolContext) -> SandboxResult<Value> {
        // #1105: thread the parent's `invocation_id` into the
        // dispatcher so the coordinator can carry it on the
        // `subagent_started` SSE event back to the parent's stream.
        // The frontend's resolver in
        // `crates/alms-gateway/static/ui/hooks/use-session-stream.js`
        // falls back to this id when `subagent_name` is `None` (the
        // ephemeral / unnamed subagent case) — without it the new
        // session id would land on the first running unnamed
        // SubagentBar entry the resolver finds, which is wrong when
        // multiple concurrent unnamed subagents are in flight.
        self.execute_inner(params, Some(ctx.invocation_id)).await
    }

    fn is_builtin(&self) -> bool {
        true
    }
}

impl InvokeAgentTool {
    /// Shared implementation for [`Tool::execute`] and
    /// [`Tool::execute_with_context`]. The only difference between
    /// the two entry points is whether `parent_tool_invocation_id`
    /// is populated; everything else (param parsing, name
    /// validation, foreground/background routing) is identical.
    async fn execute_inner(
        &self,
        params: Value,
        parent_tool_invocation_id: Option<Uuid>,
    ) -> SandboxResult<Value> {
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
                    parent_tool_invocation_id,
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
                parent_tool_invocation_id,
            )
            .await
            .map_err(SandboxError::from)?;

        Ok(serde_json::json!({
            "response": response,
            "session_id": sub_session_id.0.to_string(),
        }))
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
            _parent_tool_invocation_id: Option<Uuid>,
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

    /// #1296: the `name` example must not be a *review request*.
    ///
    /// `docs/layer2-peer-messaging-design.md` § 9.1 uses exactly that case
    /// as its worked example of when to prefer `send_message` ("a developer
    /// agent asking a reviewer agent for a review should use
    /// `send_message`"), so offering `'reviewer'` as the archetypal thing to
    /// invoke handed the model the doc's own counterexample. Most of this
    /// rewrite is prose and not pinnable; the presence of that one token is.
    #[test]
    fn name_example_is_not_the_design_docs_counterexample() {
        let tool = make_tool("ok");
        let name_desc = tool.parameters()["properties"]["name"]["description"]
            .as_str()
            .expect("`name` must document itself")
            .to_ascii_lowercase();
        assert!(
            !name_desc.contains("'reviewer'"),
            "invoke_agent's `name` example must not be 'reviewer' -- § 9.1 says a \
             review request is the send_message case (#1296). Got: {name_desc}"
        );
        assert!(
            name_desc.contains("send_message"),
            "the `name` parameter must point at send_message for peer requests (#1296)"
        );
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
                _parent_tool_invocation_id: Option<Uuid>,
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
                _parent_tool_invocation_id: Option<Uuid>,
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
            _parent_tool_invocation_id: Option<Uuid>,
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
            _parent_tool_invocation_id: Option<Uuid>,
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
            _parent_tool_invocation_id: Option<Uuid>,
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

    // ── #1105: parent tool_invocation_id is threaded through ────────────────

    /// A dispatcher that captures the `parent_tool_invocation_id` it
    /// receives on both `dispatch` and `dispatch_background`.
    #[derive(Debug)]
    struct InvocationCapturingDispatcher {
        foreground: std::sync::Mutex<Option<Option<Uuid>>>,
        background: std::sync::Mutex<Option<Option<Uuid>>>,
    }

    impl InvocationCapturingDispatcher {
        fn new() -> Self {
            Self {
                foreground: std::sync::Mutex::new(None),
                background: std::sync::Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl SubagentDispatcher for InvocationCapturingDispatcher {
        async fn dispatch(
            &self,
            _task: String,
            _parent_session_id: SessionId,
            _parent_agent_id: AgentId,
            _parent_run_id: Option<RunId>,
            _parent_event_tx: Option<Arc<dyn EventForwarder>>,
            _subagent_name: Option<String>,
            _parent_cancel_token: Option<CancellationToken>,
            parent_tool_invocation_id: Option<Uuid>,
        ) -> AlmsResult<(String, SessionId)> {
            *self.foreground.lock().unwrap() = Some(parent_tool_invocation_id);
            Ok(("ok".to_string(), SessionId::new()))
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
            parent_tool_invocation_id: Option<Uuid>,
        ) -> AlmsResult<(Uuid, SessionId)> {
            *self.background.lock().unwrap() = Some(parent_tool_invocation_id);
            Ok((Uuid::new_v4(), SessionId::new()))
        }
    }

    /// `execute_with_context` carries the parent's invocation_id into
    /// the dispatcher for foreground invocations — this is what feeds
    /// the coordinator's `subagent_started` SSE emit (#1105).
    #[tokio::test]
    async fn test_execute_with_context_threads_invocation_id_foreground() {
        let dispatcher = Arc::new(InvocationCapturingDispatcher::new());
        let tool = InvokeAgentTool::new(
            dispatcher.clone(),
            SessionId::new(),
            AgentId::new(),
            None,
            None,
        );
        let inv_id = Uuid::new_v4();
        tool.execute_with_context(serde_json::json!({ "task": "x" }), ToolContext::new(inv_id))
            .await
            .unwrap();
        let captured = dispatcher.foreground.lock().unwrap().take().unwrap();
        assert_eq!(captured, Some(inv_id));
    }

    /// Same thing for background — Atlas's acceptance criteria explicitly
    /// require the event to fire on both paths so the wire shape is
    /// consistent. The frontend handler is idempotent for backgrounds
    /// (the tool result + completion marker also carry the session id),
    /// but emitting symmetrically keeps the contract simple.
    #[tokio::test]
    async fn test_execute_with_context_threads_invocation_id_background() {
        let dispatcher = Arc::new(InvocationCapturingDispatcher::new());
        let tool = InvokeAgentTool::new(
            dispatcher.clone(),
            SessionId::new(),
            AgentId::new(),
            None,
            None,
        );
        let inv_id = Uuid::new_v4();
        tool.execute_with_context(
            serde_json::json!({ "task": "x", "background": true }),
            ToolContext::new(inv_id),
        )
        .await
        .unwrap();
        let captured = dispatcher.background.lock().unwrap().take().unwrap();
        assert_eq!(captured, Some(inv_id));
    }

    /// Plain `execute` (no context) preserves the legacy contract — the
    /// dispatcher sees `None`. This keeps `Tool` consumers that call
    /// `execute` directly (a few unit tests in this crate, future
    /// external callers) from breaking.
    #[tokio::test]
    async fn test_execute_without_context_passes_none() {
        let dispatcher = Arc::new(InvocationCapturingDispatcher::new());
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
        let captured = dispatcher.foreground.lock().unwrap().take().unwrap();
        assert_eq!(captured, None);
    }
}
