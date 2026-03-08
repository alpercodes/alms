//! SubagentDispatcher trait — lets tools spawn subagents without a direct
//! dependency on alms-coordinator.
//!
//! Defined here (in alms-runtime) so that InvokeAgentTool can hold an
//! `Arc<dyn SubagentDispatcher>` without creating a cycle between
//! alms-runtime and alms-coordinator.

use crate::events::RuntimeEventSender;
use alms_core::{AlmsResult, RunId, SessionId};
use async_trait::async_trait;

/// Implemented by `Coordinator` to allow tools to spawn subagents.
#[async_trait]
pub trait SubagentDispatcher: Send + Sync + std::fmt::Debug {
    /// Spawn a subagent, await its completion, and return the response text.
    ///
    /// `parent_event_tx` is the parent run's runtime event sender. When
    /// provided, the subagent's tool events are forwarded into the parent
    /// run's SSE stream so the UI can show subagent activity inline.
    async fn dispatch(
        &self,
        task: String,
        system_prompt: Option<String>,
        parent_session_id: SessionId,
        parent_run_id: Option<RunId>,
        parent_event_tx: Option<RuntimeEventSender>,
    ) -> AlmsResult<String>;
}
