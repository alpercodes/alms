//! SubagentDispatcher trait — lets tools spawn subagents without a direct
//! dependency on alms-coordinator.
//!
//! Defined here (in alms-runtime) so that InvokeAgentTool can hold an
//! `Arc<dyn SubagentDispatcher>` without creating a cycle between
//! alms-runtime and alms-coordinator.

use crate::events::RuntimeEventSender;
use alms_core::{AlmsError, AlmsResult, RunId, SessionId};
use async_trait::async_trait;
use uuid::Uuid;

/// Outcome of polling a background subagent task.
#[derive(Debug, Clone, PartialEq)]
pub enum PollResult {
    /// Task is still running — call `get_task_result` again later.
    Running,
    /// Task completed successfully with this response text.
    Completed(String),
    /// Task failed with this error message.
    Failed(String),
    /// Task was cancelled before it could complete.
    Cancelled,
}

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
        subagent_name: Option<String>,
    ) -> AlmsResult<String>;

    /// Fire a subagent in the background and return its task ID immediately.
    ///
    /// The caller can poll for the result using `poll_task(task_id)`. The
    /// subagent runs concurrently with the parent's loop. Subagent tool events
    /// are still forwarded into `parent_event_tx` when provided.
    async fn dispatch_background(
        &self,
        _task: String,
        _system_prompt: Option<String>,
        _parent_session_id: SessionId,
        _parent_run_id: Option<RunId>,
        _parent_event_tx: Option<RuntimeEventSender>,
        _subagent_name: Option<String>,
    ) -> AlmsResult<Uuid> {
        Err(AlmsError::Runtime(
            "dispatch_background not supported by this dispatcher".to_string(),
        ))
    }

    /// Poll a background task spawned via `dispatch_background`.
    ///
    /// Returns `PollResult::Running` while the task is in progress.
    /// Returns an error if the task ID is unknown or has already expired.
    async fn poll_task(&self, _task_id: Uuid) -> AlmsResult<PollResult> {
        Err(AlmsError::Runtime(
            "poll_task not supported by this dispatcher".to_string(),
        ))
    }
}
