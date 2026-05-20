//! SubagentDispatcher trait -- lets tools spawn subagents without a direct
//! dependency on alms-coordinator.
//!
//! Defined here (in alms-tools) so that InvokeAgentTool can hold an
//! `Arc<dyn SubagentDispatcher>` without creating a cycle between
//! alms-tools and alms-coordinator.

use crate::event_forwarder::EventForwarder;
use alms_core::{AgentId, AlmsError, AlmsResult, RunId, SessionId};
use async_trait::async_trait;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Implemented by `Coordinator` to allow tools to spawn subagents.
#[async_trait]
pub trait SubagentDispatcher: Send + Sync + std::fmt::Debug {
    /// Spawn a subagent, await its completion, and return the response text
    /// along with the subagent's own session ID.
    ///
    /// Named subagents (`subagent_name` = Some) must be pre-registered in the
    /// agent registry via `alms agent create`. Their config (model, posture)
    /// and workspace files are loaded from the registry. Their persistent
    /// session is keyed on `(parent_agent_id, name)` (#1051) — so the same
    /// named subagent resolves to the same session across every chat the
    /// parent agent participates in.
    ///
    /// `parent_event_fwd` is a type-erased event forwarder. When provided,
    /// the subagent's tool events are forwarded into the parent run's SSE
    /// stream so the UI can show subagent activity inline.
    ///
    /// `parent_tool_invocation_id` is the parent's `invoke_agent`
    /// invocation id (#1105). When provided, the coordinator emits the
    /// `subagent_started` SSE event back to the parent's stream carrying
    /// this id so the UI's resolver can attach the new session id to
    /// the right SubagentBar entry — including ephemeral / unnamed
    /// subagents where `subagent_name` alone cannot disambiguate.
    /// `None` is accepted for legacy code paths and tests that don't
    /// need the event; the coordinator skips the emit entirely in that
    /// case (the frontend resolver requires `tool_invocation_id` or
    /// `subagent_name` to attach the session id, and would warn-and-no-op
    /// without either).
    ///
    /// Returns `(response_text, subagent_session_id)`.
    #[allow(clippy::too_many_arguments)]
    async fn dispatch(
        &self,
        task: String,
        parent_session_id: SessionId,
        parent_agent_id: AgentId,
        parent_run_id: Option<RunId>,
        parent_event_fwd: Option<Arc<dyn EventForwarder>>,
        subagent_name: Option<String>,
        parent_cancel_token: Option<CancellationToken>,
        parent_tool_invocation_id: Option<Uuid>,
    ) -> AlmsResult<(String, SessionId)>;

    /// Fire a subagent in the background and return its task ID immediately,
    /// along with the subagent's own session ID.
    ///
    /// The subagent runs concurrently with the parent's loop. Results are
    /// delivered automatically via the completion notification system.
    /// Subagent tool events are still forwarded into `parent_event_fwd`
    /// when provided. `parent_tool_invocation_id` has the same meaning
    /// and #1105 semantics as on [`Self::dispatch`].
    ///
    /// Returns `(task_uuid, subagent_session_id)`.
    #[allow(clippy::too_many_arguments)]
    async fn dispatch_background(
        &self,
        _task: String,
        _parent_session_id: SessionId,
        _parent_agent_id: AgentId,
        _parent_run_id: Option<RunId>,
        _parent_event_fwd: Option<Arc<dyn EventForwarder>>,
        _subagent_name: Option<String>,
        _parent_cancel_token: Option<CancellationToken>,
        _parent_tool_invocation_id: Option<Uuid>,
    ) -> AlmsResult<(Uuid, SessionId)> {
        Err(AlmsError::Runtime(
            "dispatch_background not supported by this dispatcher".to_string(),
        ))
    }
}
