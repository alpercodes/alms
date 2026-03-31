//! SubagentDispatcher trait -- lets tools spawn subagents without a direct
//! dependency on alms-coordinator.
//!
//! Defined here (in alms-tools) so that InvokeAgentTool can hold an
//! `Arc<dyn SubagentDispatcher>` without creating a cycle between
//! alms-tools and alms-coordinator.

use crate::event_forwarder::EventForwarder;
use alms_core::{AlmsError, AlmsResult, RunId, SessionId};
use async_trait::async_trait;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Implemented by `Coordinator` to allow tools to spawn subagents.
#[async_trait]
pub trait SubagentDispatcher: Send + Sync + std::fmt::Debug {
    /// Spawn a subagent, await its completion, and return the response text.
    ///
    /// Named subagents (`subagent_name` = Some) must be pre-registered in the
    /// agent registry via `alms agent create`. Their config (model, posture)
    /// and workspace files are loaded from the registry.
    ///
    /// `parent_event_fwd` is a type-erased event forwarder. When provided,
    /// the subagent's tool events are forwarded into the parent run's SSE
    /// stream so the UI can show subagent activity inline.
    async fn dispatch(
        &self,
        task: String,
        parent_session_id: SessionId,
        parent_run_id: Option<RunId>,
        parent_event_fwd: Option<Arc<dyn EventForwarder>>,
        subagent_name: Option<String>,
        parent_cancel_token: Option<CancellationToken>,
    ) -> AlmsResult<String>;

    /// Fire a subagent in the background and return its task ID immediately.
    ///
    /// The subagent runs concurrently with the parent's loop. Results are
    /// delivered automatically via the completion notification system.
    /// Subagent tool events are still forwarded into `parent_event_fwd`
    /// when provided.
    async fn dispatch_background(
        &self,
        _task: String,
        _parent_session_id: SessionId,
        _parent_run_id: Option<RunId>,
        _parent_event_fwd: Option<Arc<dyn EventForwarder>>,
        _subagent_name: Option<String>,
        _parent_cancel_token: Option<CancellationToken>,
    ) -> AlmsResult<Uuid> {
        Err(AlmsError::Runtime(
            "dispatch_background not supported by this dispatcher".to_string(),
        ))
    }
}
