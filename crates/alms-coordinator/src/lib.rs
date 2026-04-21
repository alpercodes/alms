pub mod message_bus;

use alms_core::{
    AgentId, AlmsResult, Run, RunId, RunRegistrar, SessionId, TokenUsage, truncate_to_char_boundary,
};
use alms_runtime::{AgentConfig, AgentRuntime, LlmClient, RunOutput};
use alms_session::SessionManager;
use alms_tools::event_forwarder::EventForwarder;
use alms_tools::subagent::SubagentDispatcher;
use async_trait::async_trait;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// How long (in seconds) a subagent is allowed to run before it times out,
/// and also how long its result is kept in memory after completion so that
/// the completion notification system can process it.
const SUBAGENT_TTL_SECS: u64 = 300;

/// Max characters in a completion notification summary.
const NOTIFICATION_SUMMARY_MAX_CHARS: usize = 800;

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, instrument, warn};
use uuid::Uuid;

/// Unique identifier for a subagent task
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub Uuid);

impl TaskId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

/// Request to spawn a subagent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentRequest {
    pub task: String,
    pub timeout: Duration,
    pub parent_session: SessionId,
    pub parent_run_id: Option<RunId>,
    /// Optional persistent name. When provided, the subagent must be
    /// pre-registered in the agent registry (`alms agent create --name ...`).
    /// Its config and workspace files are loaded from the registry.
    pub subagent_name: Option<String>,
}

/// Status of a subagent task
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Final result from a subagent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub task_id: TaskId,
    pub status: TaskStatus,
    pub result: serde_json::Value,
    pub tokens_used: Option<usize>,
}

/// Event sent when a background subagent finishes.
///
/// The gateway listens on the receiving end and creates follow-up runs
/// to notify the parent agent. This is the foundation for peer messaging:
/// the channel will evolve into a broader agent notification bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentCompletion {
    pub task_id: TaskId,
    pub subagent_name: Option<String>,
    pub status: TaskStatus,
    /// Truncated summary of the result (for context efficiency).
    pub summary: String,
    /// Parent session to notify.
    pub parent_session_id: SessionId,
    /// Parent agent ID (for run creation).
    pub parent_agent_id: AgentId,
    /// The subagent's own session ID (so the frontend can navigate to it).
    pub subagent_session_id: SessionId,
    /// The task/prompt given to the subagent (for display in completion cards).
    pub task_description: Option<String>,
    /// Number of tool calls the subagent made during its run.
    pub tool_count: Option<u32>,
    /// Wall-clock duration of the subagent run in milliseconds.
    pub duration_ms: Option<u64>,
    /// Token usage from the subagent run (prompt + completion).
    pub token_usage: Option<TokenUsage>,
}

/// Handle to a running subagent
#[derive(Debug)]
pub struct SubagentHandle {
    pub task_id: TaskId,
    pub status: TaskStatus,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub cancel_tx: oneshot::Sender<()>,
    pub parent_run_id: Option<RunId>,
    pub parent_session_id: SessionId,
    pub parent_agent_id: AgentId,
    /// The subagent's own session ID (for frontend navigation).
    pub subagent_session_id: SessionId,
    /// Whether this was spawned via `dispatch_background` (triggers completion notification).
    pub is_background: bool,
    /// Receiver for the final TaskResult — taken by `dispatch()` to await completion.
    pub result_rx: Option<oneshot::Receiver<TaskResult>>,
    /// Stored result for background tasks — set by `run_subagent` on completion
    /// so the completion notification system can access the result.
    pub completed_result: Option<TaskResult>,
}

/// Coordinator manages subagent lifecycle in a pure hierarchy.
///
/// Any agent can spawn subagents by calling `dispatch()`. Named subagents
/// must be pre-registered in the agent registry (`alms agent create`);
/// ephemeral (unnamed) subagents use default config.
/// There is no peer-to-peer communication between agents.
#[derive(Debug)]
pub struct Coordinator {
    /// Main agent ID (used for tracing/identification only)
    #[allow(dead_code)]
    main_agent: AgentId,
    /// Active subagents: TaskId -> SubagentHandle
    subagents: Arc<DashMap<TaskId, SubagentHandle>>,
    /// Named subagents currently executing — prevents concurrent invocations
    /// of the same named subagent which would corrupt shared session history.
    active_named: Arc<dashmap::DashSet<String>>,
    /// Shared session manager — used to give each subagent its own context
    session_manager: Arc<SessionManager>,
    /// LLM client — cloned for each subagent runtime
    llm: LlmClient,
    /// Base agent config — subagents inherit sandbox settings from this.
    /// Shared with the gateway's AppState so PATCH /settings updates are
    /// visible to subsequently-spawned subagents.
    base_agent_config: Arc<parking_lot::RwLock<AgentConfig>>,
    /// Workspace base directory — named subagents get workspaces under this dir
    workspace_dir: Option<std::path::PathBuf>,
    /// Absolute path to the gateway's data directory. Propagated to subagent
    /// shell_exec as `ALMS_DATA_DIR` so CLI commands find the right DB.
    data_dir: Option<std::path::PathBuf>,
    /// Tracks the last-used system_prompt per named subagent context key,
    /// so we can warn when a re-invocation uses a different prompt.
    subagent_prompts: Arc<DashMap<String, String>>,
    /// Channel for notifying the gateway when a background subagent completes.
    /// The gateway listens on the receiving end and creates follow-up runs.
    completion_tx: Option<mpsc::UnboundedSender<SubagentCompletion>>,
    /// Secrets store for API key resolution (per-agent provider overrides).
    secrets: Option<Arc<parking_lot::RwLock<alms_core::secrets::SecretsStore>>>,
    /// Optional run registrar — when set, subagent runs are registered as
    /// proper runs so they appear in GET /runs, the UI sidebar, and the CLI.
    run_registrar: Option<Arc<dyn RunRegistrar>>,
}

impl Coordinator {
    pub fn new(main_agent: AgentId, session_manager: Arc<SessionManager>, llm: LlmClient) -> Self {
        Self {
            main_agent,
            subagents: Arc::new(DashMap::new()),
            active_named: Arc::new(dashmap::DashSet::new()),
            session_manager,
            llm,
            base_agent_config: Arc::new(parking_lot::RwLock::new(AgentConfig::default())),
            workspace_dir: None,
            data_dir: None,
            subagent_prompts: Arc::new(DashMap::new()),
            completion_tx: None,
            secrets: None,
            run_registrar: None,
        }
    }

    /// Create a coordinator that inherits sandbox settings from the given config.
    ///
    /// The `Arc<RwLock<AgentConfig>>` is shared with the gateway's `AppState`
    /// so that PATCH /settings updates are visible to subsequently-spawned
    /// subagents without restarting the server.
    pub fn with_agent_config(
        main_agent: AgentId,
        session_manager: Arc<SessionManager>,
        llm: LlmClient,
        base_agent_config: Arc<parking_lot::RwLock<AgentConfig>>,
    ) -> Self {
        Self {
            main_agent,
            subagents: Arc::new(DashMap::new()),
            active_named: Arc::new(dashmap::DashSet::new()),
            session_manager,
            llm,
            base_agent_config,
            workspace_dir: None,
            data_dir: None,
            subagent_prompts: Arc::new(DashMap::new()),
            completion_tx: None,
            secrets: None,
            run_registrar: None,
        }
    }

    /// Set the workspace base directory. Named subagents will get workspaces
    /// under `{workspace_dir}/{agent_name}/`.
    pub fn with_workspace_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.workspace_dir = Some(dir);
        self
    }

    /// Set the gateway's data directory so subagent shell_exec processes
    /// inherit `ALMS_DATA_DIR` and can find the correct database.
    pub fn with_data_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.data_dir = Some(dir);
        self
    }

    /// Set the secrets store for API key resolution in subagent provider overrides.
    pub fn with_secrets(
        mut self,
        secrets: Arc<parking_lot::RwLock<alms_core::secrets::SecretsStore>>,
    ) -> Self {
        self.secrets = Some(secrets);
        self
    }

    /// Set a run registrar so subagent runs are registered as proper runs
    /// visible in GET /runs, the UI sidebar, and the CLI.
    pub fn with_run_registrar(mut self, registrar: Arc<dyn RunRegistrar>) -> Self {
        self.run_registrar = Some(registrar);
        self
    }

    /// Set a completion notification channel. When a background subagent
    /// finishes, a [`SubagentCompletion`] is sent through this channel so
    /// the gateway can create a follow-up run on the parent session.
    pub fn with_completion_channel(
        mut self,
        tx: mpsc::UnboundedSender<SubagentCompletion>,
    ) -> Self {
        self.completion_tx = Some(tx);
        self
    }

    /// Spawn a new subagent for a task.
    ///
    /// Returns a `TaskId` immediately. The caller can await the result by
    /// calling `take_result_rx(task_id)` to get the oneshot receiver.
    #[instrument(
        level = "info",
        skip(self, request, parent_event_tx),
        fields(
            parent_session = %request.parent_session.0,
            timeout_secs = %request.timeout.as_secs(),
        )
    )]
    pub async fn spawn_subagent(
        &self,
        request: SubagentRequest,
        parent_event_tx: Option<Arc<dyn EventForwarder>>,
        is_background: bool,
        parent_cancel_token: Option<CancellationToken>,
    ) -> AlmsResult<(TaskId, SessionId)> {
        // Reject concurrent invocations of the same named subagent to prevent
        // session corruption from parallel writes to the same session history.
        if let Some(ref name) = request.subagent_name
            && !self.active_named.insert(name.clone())
        {
            return Err(alms_core::AlmsError::Runtime(format!(
                "Named subagent '{}' is already running — concurrent invocations \
                 of the same named subagent are not supported",
                name
            )));
        }

        let task_id = TaskId::new();
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let (result_tx, result_rx) = oneshot::channel::<TaskResult>();
        let parent_run_id = request.parent_run_id;
        let parent_session_id = request.parent_session;

        // Resolve the parent agent ID from the session.
        let parent_agent_id = match self.session_manager.get(parent_session_id) {
            Ok(session) => session.agent_id,
            Err(_) => {
                warn!(
                    parent_session = %parent_session_id.0,
                    "Parent session not found when spawning subagent — falling back to main agent ID"
                );
                self.main_agent
            }
        };

        // Derive the subagent's session early so it can be stored in the handle
        // (for completion notifications) and returned to the caller (for tool results).
        let (sub_agent_id, sub_context_id) = derive_subagent_identity(task_id, &request);
        let sub_session = self
            .session_manager
            .get_or_create(sub_agent_id, &sub_context_id);
        let sub_session_id = sub_session.id;

        let handle = SubagentHandle {
            task_id,
            status: TaskStatus::Pending,
            started_at: chrono::Utc::now(),
            cancel_tx,
            parent_run_id,
            parent_session_id,
            parent_agent_id,
            subagent_session_id: sub_session_id,
            is_background,
            result_rx: Some(result_rx),
            completed_result: None,
        };

        self.subagents.insert(task_id, handle);

        info!(
            target: "coordinator::subagent_spawned",
            task_id = %task_id.0,
            parent_session = %request.parent_session.0,
            "Subagent spawned"
        );

        let subagents = self.subagents.clone();
        let active_named = self.active_named.clone();
        let session_manager = self.session_manager.clone();
        let llm = self.llm.clone();
        // Snapshot the current config under the lock so that PATCH /settings
        // updates are reflected in subsequently-spawned subagents.
        let base_agent_config = self.base_agent_config.read().clone();
        let workspace_dir = self.workspace_dir.clone();
        let data_dir = self.data_dir.clone();
        let subagent_prompts = self.subagent_prompts.clone();
        let completion_tx = self.completion_tx.clone();
        let secrets = self.secrets.clone();
        let run_registrar = self.run_registrar.clone();

        let span = tracing::info_span!(
            "subagent::execute",
            task_id = %task_id.0,
            parent_run_id = ?parent_run_id.map(|r| r.0.to_string()),
        );
        tokio::spawn(
            async move {
                run_subagent(
                    task_id,
                    request,
                    subagents,
                    active_named,
                    cancel_rx,
                    result_tx,
                    session_manager,
                    llm,
                    parent_event_tx,
                    base_agent_config,
                    workspace_dir,
                    data_dir,
                    subagent_prompts,
                    completion_tx,
                    parent_cancel_token,
                    secrets,
                    run_registrar,
                    is_background,
                )
                .await;
            }
            .instrument(span),
        );

        Ok((task_id, sub_session_id))
    }

    /// Take the result receiver for a task (can only be called once per task).
    ///
    /// Returns `None` if the task does not exist or the receiver was already taken.
    pub fn take_result_rx(&self, task_id: TaskId) -> Option<oneshot::Receiver<TaskResult>> {
        self.subagents.get_mut(&task_id)?.result_rx.take()
    }
}

#[async_trait]
impl SubagentDispatcher for Coordinator {
    async fn dispatch(
        &self,
        task: String,
        parent_session_id: SessionId,
        parent_run_id: Option<RunId>,
        parent_event_tx: Option<Arc<dyn EventForwarder>>,
        subagent_name: Option<String>,
        parent_cancel_token: Option<CancellationToken>,
    ) -> AlmsResult<(String, SessionId)> {
        let request = SubagentRequest {
            task,
            timeout: Duration::from_secs(SUBAGENT_TTL_SECS),
            parent_session: parent_session_id,
            parent_run_id,
            subagent_name,
        };

        let (task_id, sub_session_id) = self
            .spawn_subagent(request, parent_event_tx, false, parent_cancel_token)
            .await?;

        // Take the result receiver — must happen immediately after spawn_subagent
        // since the handle is already in the DashMap.
        let result_rx = self.take_result_rx(task_id).ok_or_else(|| {
            alms_core::AlmsError::Runtime("No result channel for subagent".to_string())
        })?;

        // Block until the subagent completes (or is cancelled/times out)
        let task_result = result_rx.await.map_err(|_| {
            alms_core::AlmsError::Runtime("Subagent result channel closed unexpectedly".to_string())
        })?;

        match task_result.status {
            TaskStatus::Completed => Ok((
                task_result.result["response"]
                    .as_str()
                    .unwrap_or("[no response]")
                    .to_string(),
                sub_session_id,
            )),
            TaskStatus::Failed => Err(alms_core::AlmsError::Runtime(
                task_result.result["error"]
                    .as_str()
                    .unwrap_or("subagent failed")
                    .to_string(),
            )),
            TaskStatus::Cancelled => Err(alms_core::AlmsError::Runtime(
                "Subagent was cancelled".to_string(),
            )),
            _ => Err(alms_core::AlmsError::Runtime(
                "Subagent ended in unexpected state".to_string(),
            )),
        }
    }

    #[instrument(
        level = "info",
        skip(self, task, parent_event_tx, parent_cancel_token),
        fields(parent_session = %parent_session_id.0)
    )]
    async fn dispatch_background(
        &self,
        task: String,
        parent_session_id: SessionId,
        parent_run_id: Option<RunId>,
        parent_event_tx: Option<Arc<dyn EventForwarder>>,
        subagent_name: Option<String>,
        parent_cancel_token: Option<CancellationToken>,
    ) -> alms_core::AlmsResult<(Uuid, SessionId)> {
        let request = SubagentRequest {
            task,
            timeout: Duration::from_secs(SUBAGENT_TTL_SECS),
            parent_session: parent_session_id,
            parent_run_id,
            subagent_name,
        };
        let (task_id, sub_session_id) = self
            .spawn_subagent(request, parent_event_tx, true, parent_cancel_token)
            .await?;

        // Drop the oneshot receiver — the completion notification system reads
        // the result from `completed_result` on the SubagentHandle, not from
        // this channel. This frees the allocation; run_subagent's result_tx.send()
        // will silently fail (already uses `let _ = ...`), which is intentional.
        drop(self.take_result_rx(task_id));

        info!(
            task_id = %task_id.0,
            sub_session_id = %sub_session_id.0,
            "Background subagent spawned (non-blocking)"
        );
        Ok((task_id.0, sub_session_id))
    }
}

// ---------------------------------------------------------------------------
// RAII guard for named subagent lock
// ---------------------------------------------------------------------------

/// Removes a named subagent from the active set on drop, guaranteeing cleanup
/// even if the subagent task panics.
struct NamedSubagentGuard {
    name: Option<String>,
    active_named: Arc<dashmap::DashSet<String>>,
}

impl Drop for NamedSubagentGuard {
    fn drop(&mut self) {
        if let Some(ref name) = self.name {
            self.active_named.remove(name);
        }
    }
}

// ---------------------------------------------------------------------------
// Internal subagent runner
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn run_subagent(
    task_id: TaskId,
    request: SubagentRequest,
    subagents: Arc<DashMap<TaskId, SubagentHandle>>,
    active_named: Arc<dashmap::DashSet<String>>,
    cancel_rx: oneshot::Receiver<()>,
    result_tx: oneshot::Sender<TaskResult>,
    session_manager: Arc<SessionManager>,
    llm: LlmClient,
    parent_event_tx: Option<Arc<dyn EventForwarder>>,
    base_agent_config: AgentConfig,
    workspace_dir: Option<std::path::PathBuf>,
    data_dir: Option<std::path::PathBuf>,
    subagent_prompts: Arc<DashMap<String, String>>,
    completion_tx: Option<mpsc::UnboundedSender<SubagentCompletion>>,
    parent_cancel_token: Option<CancellationToken>,
    secrets: Option<Arc<parking_lot::RwLock<alms_core::secrets::SecretsStore>>>,
    run_registrar: Option<Arc<dyn RunRegistrar>>,
    is_background: bool,
) {
    // RAII guard: removes the name from active_named on drop (including panics).
    let _named_guard = NamedSubagentGuard {
        name: request.subagent_name.clone(),
        active_named,
    };

    let start = std::time::Instant::now();

    if let Some(mut handle) = subagents.get_mut(&task_id) {
        handle.status = TaskStatus::Running;
    }

    info!(
        target: "subagent::started",
        task_id = %task_id.0,
        task = %request.task,
        "Subagent execution started"
    );

    // Create a child cancellation token that fires when EITHER:
    //   1. The parent run's CancellationToken is cancelled, OR
    //   2. The explicit `cancel_subagent()` oneshot fires.
    // This unifies both cancellation paths into a single token that
    // gets attached to the subagent's AgentRuntime.
    let child_cancel_token = parent_cancel_token
        .as_ref()
        .map(|p| p.child_token())
        .unwrap_or_default();

    // Bridge the oneshot cancel_rx to the child token: when cancel_subagent()
    // sends on the oneshot, we cancel the child token.
    let bridge_token = child_cancel_token.clone();
    let bridge_handle = tokio::spawn(async move {
        if cancel_rx.await.is_ok() {
            bridge_token.cancel();
        }
    });

    // Derive the subagent's identity early so we can register the run
    // *before* the tokio::select!.  This ensures the run record is always
    // updated — even when timeout or cancellation wins the select and the
    // run_agent_loop future is dropped.
    let (sub_agent_id, sub_context_id) = derive_subagent_identity(task_id, &request);

    // Resolve the subagent's session early so we can (a) register the run
    // and (b) include the session ID in the completion notification / tool result.
    let sub_session = session_manager.get_or_create(sub_agent_id, &sub_context_id);
    let sub_session_id = sub_session.id;

    // Register the subagent run with the RunRegistrar (if available) so it
    // appears in GET /runs, the UI sidebar, and CLI `alms run list`.
    let subagent_run = if let Some(ref registrar) = run_registrar {
        let mut run = if let Some(parent_rid) = request.parent_run_id {
            Run::for_subagent(
                sub_session_id,
                sub_agent_id,
                request.task.clone(),
                parent_rid,
            )
        } else {
            Run::new(sub_session_id, sub_agent_id, request.task.clone())
        };
        run.mark_running();
        registrar.register_run(run.clone());
        Some(run)
    } else {
        None
    };

    // The select returns the task status, a JSON result value (for the
    // TaskResult / completion notification), and optionally the full
    // RunOutput so we can record accurate token usage in the run record.
    let (new_status, result_value, tokens_used, run_output) = tokio::select! {
        _ = tokio::time::sleep(request.timeout) => {
            warn!(
                target: "subagent::timeout",
                task_id = %task_id.0,
                timeout_secs = %request.timeout.as_secs(),
                "Subagent timed out"
            );
            (TaskStatus::Failed, serde_json::json!({"error": "Timeout"}), None, None)
        }
        _ = child_cancel_token.cancelled() => {
            info!(
                target: "subagent::cancelled",
                task_id = %task_id.0,
                "Subagent cancelled"
            );
            (TaskStatus::Cancelled, serde_json::json!({"cancelled": true}), None, None)
        }
        output = run_agent_loop(task_id, &request, sub_agent_id, &sub_context_id, &session_manager, &llm, parent_event_tx, &base_agent_config, workspace_dir.as_deref(), data_dir.as_deref(), &subagent_prompts, child_cancel_token.clone(), secrets.as_ref(), is_background) => {
            match output {
                Ok(run_output) => {
                    info!(
                        target: "subagent::completed",
                        task_id = %task_id.0,
                        elapsed_ms = %start.elapsed().as_millis(),
                        "Subagent completed"
                    );
                    let tokens = (run_output.usage.prompt_tokens
                        + run_output.usage.completion_tokens) as usize;
                    (
                        TaskStatus::Completed,
                        serde_json::json!({"response": run_output.response}),
                        Some(tokens),
                        Some(run_output),
                    )
                }
                Err(e) => {
                    tracing::error!(
                        target: "subagent::error",
                        task_id = %task_id.0,
                        error = %e,
                        "Subagent run failed"
                    );
                    (
                        TaskStatus::Failed,
                        serde_json::json!({"error": e.to_string()}),
                        None,
                        None,
                    )
                }
            }
        }
    };

    // Cancel child token to clean up the bridge task (if it's still waiting
    // on the oneshot). This is a no-op if the token was already cancelled.
    child_cancel_token.cancel();
    bridge_handle.abort();

    // Update the run record with the outcome.  This executes regardless of
    // which tokio::select! branch fired (normal completion, timeout, or
    // cancellation), preventing orphaned "Running" records.
    if let (Some(registrar), Some(mut run)) = (&run_registrar, subagent_run) {
        match new_status {
            TaskStatus::Completed => {
                if let Some(ref output) = run_output {
                    run.mark_completed(
                        output.response.clone(),
                        alms_core::TokenUsage {
                            prompt_tokens: output.usage.prompt_tokens,
                            completion_tokens: output.usage.completion_tokens,
                            reasoning_tokens: output.usage.reasoning_tokens,
                        },
                    );
                }
            }
            TaskStatus::Failed => {
                let error = result_value["error"]
                    .as_str()
                    .unwrap_or("unknown error")
                    .to_string();
                run.mark_failed(error);
            }
            TaskStatus::Cancelled => {
                run.mark_cancelled();
            }
            _ => {}
        }
        registrar.update_run(run);
    }

    let task_result = TaskResult {
        task_id,
        status: new_status,
        result: result_value,
        tokens_used,
    };

    // Store result in the handle for background-mode polling, then update status.
    // Also capture background flag and parent info for the completion notification.
    let background_info = if let Some(mut handle) = subagents.get_mut(&task_id) {
        handle.status = new_status;
        handle.completed_result = Some(task_result.clone());
        if handle.is_background {
            Some((
                handle.parent_session_id,
                handle.parent_agent_id,
                handle.subagent_session_id,
            ))
        } else {
            None
        }
    } else {
        None
    };

    // Fire completion notification for background subagents so the gateway
    // can auto-create a follow-up run on the parent session.
    if let Some((parent_session_id, parent_agent_id, subagent_session_id)) = background_info
        && let Some(ref tx) = completion_tx
    {
        let summary = truncate_for_notification(&task_result.result);
        let elapsed_ms = start.elapsed().as_millis() as u64;
        let (tool_count, token_usage) = match &run_output {
            Some(output) => (
                Some(output.tool_calls.len() as u32),
                Some(TokenUsage {
                    prompt_tokens: output.usage.prompt_tokens,
                    completion_tokens: output.usage.completion_tokens,
                    reasoning_tokens: output.usage.reasoning_tokens,
                }),
            ),
            None => (None, None),
        };
        // Cap task_description to the same limit as the summary (800 chars)
        // to prevent unbounded metadata in persisted lifecycle markers.
        let task_desc = {
            let raw = &request.task;
            let truncated = truncate_to_char_boundary(raw, NOTIFICATION_SUMMARY_MAX_CHARS);
            if truncated.len() == raw.len() {
                raw.clone()
            } else {
                format!("{}…[truncated]", truncated)
            }
        };
        let completion = SubagentCompletion {
            task_id,
            subagent_name: request.subagent_name.clone(),
            status: new_status,
            summary,
            parent_session_id,
            parent_agent_id,
            subagent_session_id,
            task_description: Some(task_desc),
            tool_count,
            duration_ms: Some(elapsed_ms),
            token_usage,
        };
        if let Err(e) = tx.send(completion) {
            warn!(
                task_id = %task_id.0,
                error = %e,
                "Failed to send completion notification (receiver dropped)"
            );
        }
    }

    // Release the named subagent lock before sending the result, so that
    // callers who receive the result can immediately re-invoke the same name.
    // The guard also handles panic cleanup via Drop.
    drop(_named_guard);

    // Clean up ephemeral workspace directory to prevent unbounded disk growth.
    // Named subagents keep their workspace (persistent identity files).
    if request.subagent_name.is_none()
        && let Some(ref ws_dir) = workspace_dir
    {
        let ephemeral_dir = ws_dir.join(".ephemeral").join(task_id.0.to_string());
        if ephemeral_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&ephemeral_dir) {
                warn!(
                    task_id = %task_id.0,
                    path = %ephemeral_dir.display(),
                    error = %e,
                    "Failed to clean up ephemeral workspace directory"
                );
            } else {
                debug!(
                    task_id = %task_id.0,
                    path = %ephemeral_dir.display(),
                    "Cleaned up ephemeral workspace directory"
                );
            }
        }
    }

    // Deliver result to dispatch() caller (foreground mode — may already be dropped)
    let _ = result_tx.send(task_result);

    // Keep the handle long enough for background callers to poll the result.
    tokio::time::sleep(Duration::from_secs(SUBAGENT_TTL_SECS)).await;
    subagents.remove(&task_id);
    // Clean up cached prompt to prevent unbounded memory growth.
    if let Some(ref name) = request.subagent_name {
        let ctx_key = format!("subagent_{}_{}", request.parent_session.0, name);
        subagent_prompts.remove(&ctx_key);
    }
    debug!("Cleaned up subagent {:?}", task_id);
}

/// Truncate a subagent result value to a short summary for completion notifications.
fn truncate_for_notification(result: &serde_json::Value) -> String {
    let text = result["response"]
        .as_str()
        .or_else(|| result["error"].as_str())
        .unwrap_or("[no content]");

    let truncated = truncate_to_char_boundary(text, NOTIFICATION_SUMMARY_MAX_CHARS);
    if truncated.len() == text.len() {
        text.to_string()
    } else {
        format!("{}…[truncated]", truncated)
    }
}

/// Default system prompt for ephemeral (unnamed) subagents.
///
/// Loaded at compile time from `crates/alms-runtime/prompts/subagent.md`.
const DEFAULT_SUBAGENT_PROMPT: &str =
    include_str!("../../alms-runtime/prompts/subagent.md").trim_ascii();

/// Config extracted from an agent registry record for a named subagent.
struct SubagentRecordConfig {
    model: Option<String>,
    posture: Option<String>,
    provider: Option<String>,
    /// Per-named-subagent Anthropic extended-thinking budget override.
    ///
    /// Mirrors the top-level agent path in `apply_overrides` (gateway
    /// `runs/mod.rs`): `Some(n)` (including `Some(0)`) is treated as an
    /// explicit override; `None` inherits the parent's effective budget.
    /// This keeps the three-layer precedence (per-run > per-agent >
    /// server default) intact for named subagents too.
    thinking_budget_tokens: Option<u32>,
    /// Per-named-subagent OpenAI-compat reasoning-effort override (#768).
    /// Mirrors `thinking_budget_tokens` for the OpenAI reasoning path:
    /// `Some(effort)` wins over the parent; `None` inherits the parent's
    /// effective effort.
    reasoning_effort: Option<alms_core::config::ReasoningEffort>,
}

/// Build an `AgentConfig` for a subagent. Named subagents get their config
/// from the agent registry; ephemeral subagents use a default prompt.
/// Both inherit sandbox, tool, and runtime settings (max_iterations,
/// max_tokens, context_config) from the parent's base config.
fn agent_config_for_subagent(
    record: Option<SubagentRecordConfig>,
    base: &AgentConfig,
) -> (AgentConfig, Option<String>, Option<String>) {
    let (model, posture_str, provider, thinking_budget_override, reasoning_effort_override) =
        match record {
            Some(r) => (
                r.model,
                r.posture,
                r.provider,
                r.thinking_budget_tokens,
                r.reasoning_effort,
            ),
            None => (None, None, None, None, None),
        };

    let posture = posture_str
        .as_deref()
        .and_then(|s| s.parse::<alms_runtime::Posture>().ok())
        .unwrap_or(alms_runtime::Posture::FullControl);

    // Per-named-subagent Anthropic thinking budget override. `Some(0)` is a
    // legitimate override meaning "disable extended thinking for this
    // subagent even when the parent enables it", matching the gateway's
    // top-level `apply_overrides` semantics. `None` inherits the parent's
    // effective budget so unconfigured subagents stay consistent with their
    // parent's extended-thinking policy.
    let anthropic_thinking_budget =
        thinking_budget_override.unwrap_or(base.anthropic_thinking_budget);

    // Per-named-subagent OpenAI reasoning-effort override (#768). Same
    // shape as the thinking budget: `Some(effort)` overrides the parent;
    // `None` inherits the parent's effective value.
    let openai_reasoning_effort = reasoning_effort_override.or(base.openai_reasoning_effort);

    let config = AgentConfig {
        system_prompt: DEFAULT_SUBAGENT_PROMPT.to_string(),
        posture,
        sandbox_root: base.sandbox_root.clone(),
        shell_policy: base.shell_policy.clone(),
        shell_permissions: base.shell_permissions.clone(),
        shell_classification_mode: base.shell_classification_mode,
        // Subagents inherit the parent's spill policy so >30 KB shell output
        // spills to disk for them too (issue #756). The actual spill directory
        // is wired in at `run_agent_loop` via `with_shell_spill` using a
        // per-subagent subdir (`{data_dir}/shell_output/sub-{task_id}/`), which
        // keeps the retention sweep's directory walk well-defined.
        shell_spill: base.shell_spill.clone(),
        enabled_tools: base.enabled_tools.clone(),
        fs_edit_fuzzy_match: base.fs_edit_fuzzy_match,
        max_iterations: base.max_iterations,
        max_tokens: base.max_tokens,
        context_config: base.context_config.clone(),
        prompts: base.prompts.clone(),
        debug_mode: false,
        anthropic_thinking_budget,
        openai_reasoning_effort,
    };
    (config, model, provider)
}

/// Derive the subagent's identity (agent_id, context_id) without building
/// the full config.  Called by `run_subagent` *before* `tokio::select!` so
/// that the run can be registered early and updated after timeout/cancel.
fn derive_subagent_identity(task_id: TaskId, request: &SubagentRequest) -> (AgentId, String) {
    if let Some(ref name) = request.subagent_name {
        let parent_as_agent = AgentId(request.parent_session.0);
        let stable_id = AgentId::deterministic(parent_as_agent, name);
        let stable_ctx = format!("subagent_{}_{}", request.parent_session.0, name);
        (stable_id, stable_ctx)
    } else {
        (AgentId::new(), format!("subagent_{}", task_id.0))
    }
}

/// Resolve a subagent's effective posture.
///
/// Background subagents have no human in the loop to approve tool calls,
/// so `Guarded` posture would cause them to hang indefinitely.  This
/// function overrides `Guarded` to `Autonomous` for background subagents,
/// matching the pattern used for system-triggered runs in the gateway
/// (`resolve_posture_for_run`).  All other combinations are returned
/// unchanged.
pub fn resolve_subagent_posture(
    is_background: bool,
    posture: alms_runtime::Posture,
) -> alms_runtime::Posture {
    if is_background && posture == alms_runtime::Posture::Guarded {
        alms_runtime::Posture::Autonomous
    } else {
        posture
    }
}

/// Run the actual agent loop for a subagent.
///
/// Creates a fresh `AgentRuntime`, forwards its events to the parent's
/// event channel (if provided), then calls `runtime.run()`.
///
/// **Named subagents** (`subagent_name` is Some): looked up in the agent
/// registry for config (model, posture). Workspace is
/// attached if `workspace_dir` is set. Session identity is deterministic
/// (UUID v5 from parent session + name) — conversation history preserved.
///
/// **Ephemeral subagents** (`subagent_name` is None): fresh agent ID,
/// fresh session, default config, disposable workspace at
/// `{workspace_dir}/.ephemeral/{task_id}/` for fs sandbox scoping.
///
/// Run registration/update is handled by the caller (`run_subagent`) to
/// ensure the run record is always updated, even on timeout or cancellation.
#[allow(clippy::too_many_arguments)]
async fn run_agent_loop(
    task_id: TaskId,
    request: &SubagentRequest,
    agent_id: AgentId,
    context_id: &str,
    session_manager: &Arc<SessionManager>,
    llm: &LlmClient,
    parent_event_tx: Option<Arc<dyn EventForwarder>>,
    base_agent_config: &AgentConfig,
    workspace_dir: Option<&std::path::Path>,
    data_dir: Option<&std::path::Path>,
    subagent_prompts: &DashMap<String, String>,
    cancel_token: CancellationToken,
    secrets: Option<&Arc<parking_lot::RwLock<alms_core::secrets::SecretsStore>>>,
    is_background: bool,
) -> AlmsResult<RunOutput> {
    // Derive config based on whether the subagent is named (identity already resolved
    // by the caller via `derive_subagent_identity`).
    let (mut config, model_override, provider_override, attach_workspace) = if let Some(ref name) =
        request.subagent_name
    {
        // Named: look up agent registry for config
        let record_config = session_manager
            .store()
            .and_then(|store| store.load_agent_by_name(name).ok())
            .flatten()
            .map(|record| {
                debug!("Loaded agent record for named subagent '{name}'");
                SubagentRecordConfig {
                    model: record.model,
                    posture: record.posture,
                    provider: record.provider,
                    thinking_budget_tokens: record.thinking_budget_tokens,
                    reasoning_effort: record.reasoning_effort,
                }
            })
            .or_else(|| {
                warn!(
                    "Named subagent '{name}' not found in agent registry — using defaults. \
                         Create it with: alms agent create --name {name}"
                );
                None
            });

        let (config, model, provider) = agent_config_for_subagent(record_config, base_agent_config);

        // Detect system_prompt drift: warn when the prompt changes between
        // invocations of the same named subagent within the same parent session.
        //
        // Safety: concurrent invocations of the same named subagent are
        // rejected by the active_named guard in spawn_subagent(), so this
        // get-then-insert is not racy for a given stable_ctx.
        if let Some(prev_prompt) = subagent_prompts.get(context_id)
            && *prev_prompt != config.system_prompt
        {
            warn!(
                subagent_name = %name,
                context_id = %context_id,
                "Named subagent '{name}' system_prompt has changed since the last \
                 invocation. The existing session history was built under the \
                 previous prompt — this may cause inconsistent behavior."
            );
        }
        subagent_prompts.insert(context_id.to_owned(), config.system_prompt.clone());

        (config, model, provider, true)
    } else {
        // Ephemeral: fresh each invocation.
        // Still attach a workspace scoped to a temporary directory so that
        // fs_read/fs_write/fs_list/fs_edit are narrowed (preventing project-root access).
        let (config, _, _) = agent_config_for_subagent(None, base_agent_config);
        (
            config, None, None, true, // attach an ephemeral workspace to restrict fs_* sandbox
        )
    };

    // Resolve posture: background subagents with Guarded posture are
    // overridden to Autonomous (no human in the loop to approve tool calls).
    let resolved = resolve_subagent_posture(is_background, config.posture);
    if resolved != config.posture {
        info!(
            task_id = %task_id.0,
            "Background subagent — overriding Guarded posture to Autonomous"
        );
        config.posture = resolved;
    }

    // Create a per-subagent event channel
    let (sub_tx, sub_rx) = tokio::sync::mpsc::unbounded_channel::<alms_runtime::RuntimeEvent>();

    // Apply provider override first (with secrets for key resolution), then model
    let mut subagent_llm = llm.clone();
    if let Some(ref provider) = provider_override {
        info!("Named subagent using provider override: {provider}");
        subagent_llm = if let Some(s) = secrets {
            subagent_llm.with_provider_and_secrets(provider, &s.read())
        } else {
            subagent_llm.with_provider(provider)
        };
    } else if let Some(s) = secrets {
        // No per-agent provider override — re-resolve the key for the
        // server-default provider from the live secrets store.
        subagent_llm = subagent_llm.with_secrets(&s.read());
    }
    if let Some(model) = model_override {
        info!("Named subagent using model override: {model}");
        subagent_llm = subagent_llm.with_model(model);
    }

    // Snapshot the subagent's spill policy before `config` is moved into the
    // runtime — we need it below to wire `with_shell_spill` with the
    // per-subagent run directory.
    let subagent_spill_cfg = config.shell_spill.clone();

    let mut runtime = AgentRuntime::new(agent_id, config, subagent_llm)?
        .with_event_sender(sub_tx)
        .with_cancel_token(cancel_token);

    // Set agent name for perspective mapping in DM sessions.
    if let Some(ref name) = request.subagent_name {
        runtime = runtime.with_agent_name(name.clone());
    }

    // Inject ALMS_DATA_DIR and ALMS_WORKSPACE_DIR into subagent shell_exec
    // processes so CLI commands find the right database.
    {
        let shell_env = alms_core::build_shell_default_env(data_dir, workspace_dir);
        if !shell_env.is_empty() {
            runtime = runtime.with_shell_default_env(shell_env);
        }
    }

    // Inherit the parent's shell-output spill policy (issue #756). Subagents
    // that produce >30 KB of shell output would otherwise get silently
    // truncated with no spill file — a regression from the parent's
    // behaviour. Each subagent gets its own per-subagent spill subdirectory
    // (`{data_dir}/shell_output/sub-{task_id}/`) which is still walked by
    // `sweep_expired` at gateway startup because that routine iterates every
    // child of `{data_dir}/shell_output/`. Must be called *before*
    // `with_workspace` so that workspace's re-registration of the fs_*
    // read-extras includes the subagent's spill dir.
    if let Some(dir) = data_dir {
        let sub_run_dir = dir
            .join(alms_runtime::spill::SPILL_DIR_NAME)
            .join(format!("sub-{}", task_id.0));
        runtime = runtime.with_shell_spill(sub_run_dir, subagent_spill_cfg.enabled);
    }

    // Attach workspace to scope the fs_* sandbox.
    //
    // Named subagents:    {workspace_dir}/{name}/
    // Ephemeral subagents: {workspace_dir}/.ephemeral/{task_id}/
    //
    // Ephemeral subagents get a disposable workspace so their fs_read/fs_write/
    // fs_list/fs_edit tools are sandboxed to a narrow directory instead of inheriting
    // the project-root sandbox (which would expose sensitive state, the SQLite
    // database, and other agents' workspace files).
    if attach_workspace {
        if let Some(ws_dir) = workspace_dir {
            let subagent_ws_dir = if let Some(name) = &request.subagent_name {
                ws_dir.join(name)
            } else {
                ws_dir.join(".ephemeral").join(task_id.0.to_string())
            };
            let workspace = alms_runtime::AgentWorkspace::with_dir(subagent_ws_dir);
            runtime = runtime.with_workspace(workspace);
        } else {
            warn!(
                task_id = %task_id.0,
                subagent_name = ?request.subagent_name,
                "attach_workspace is true but workspace_dir is None — subagent will \
                 inherit the project-root sandbox. Set workspace_dir on the Coordinator \
                 to enable per-subagent sandbox scoping."
            );
        }
    }

    // Forward subagent tool events into the parent run's event stream,
    // tagging each event with the subagent's identity so the UI can
    // distinguish subagent activity from parent activity.
    if let Some(parent_fwd) = parent_event_tx {
        let label = request
            .subagent_name
            .clone()
            .unwrap_or_else(|| format!("subagent-{}", &task_id.0.to_string()[..8]));
        let task_id_str = task_id.0.to_string();
        tokio::spawn(async move {
            use alms_runtime::RuntimeEvent;
            let mut rx = sub_rx;
            while let Some(event) = rx.recv().await {
                let agent_label = Some(label.clone());
                let tid = Some(task_id_str.clone());
                match event {
                    RuntimeEvent::ToolStart {
                        invocation_id,
                        tool,
                        params,
                        ..
                    } => {
                        parent_fwd.forward_tool_start(
                            invocation_id,
                            tool,
                            params,
                            agent_label,
                            tid,
                        );
                    }
                    RuntimeEvent::ToolEnd {
                        invocation_id,
                        ok,
                        result,
                        ..
                    } => {
                        parent_fwd.forward_tool_end(invocation_id, ok, result, agent_label, tid);
                    }
                    RuntimeEvent::TokenDelta { delta, .. } => {
                        parent_fwd.forward_token_delta(delta, agent_label);
                    }
                    RuntimeEvent::ReasoningDelta { text, .. } => {
                        parent_fwd.forward_reasoning_delta(text, agent_label);
                    }
                    // Suppress subagent status events -- they would overwrite
                    // the parent's thinking indicator with the subagent's phase,
                    // which is confusing. The user doesn't need to know that a
                    // subagent is "building context" or "calling LLM".
                    RuntimeEvent::Status { .. } => continue,
                    // ApprovalRequired cannot be forwarded through EventForwarder
                    // (it requires a oneshot channel).  Background subagents
                    // should already have Guarded overridden to Autonomous, so
                    // this path is a fallback for FullControl subagents (which
                    // intentionally keep their posture).  Auto-deny the tool
                    // call immediately so the subagent doesn't hang.
                    RuntimeEvent::ApprovalRequired {
                        tool, decision_tx, ..
                    } => {
                        warn!(
                            tool = %tool,
                            "Subagent requested approval — auto-denying (approval not routable)"
                        );
                        let _ = decision_tx.send(false);
                        continue;
                    }
                    // Forward warnings from subagents, tagged with the
                    // subagent label so the operator can tell them apart
                    // from parent warnings.
                    RuntimeEvent::Warning { code, message, .. } => {
                        parent_fwd.forward_warning(code, message, agent_label);
                    }
                    // ContextDebug events from subagents are suppressed --
                    // they are only useful for the top-level agent's context.
                    RuntimeEvent::ContextDebug { .. } => continue,
                }
            }
        });
    } else {
        // Nobody is consuming -- drop the receiver so sends silently fail
        drop(sub_rx);
    }

    runtime
        .run(session_manager, context_id, &request.task)
        .await
}

#[cfg(test)]
impl Coordinator {
    /// Get the completed result for a finished background task (test-only).
    pub fn get_completed_result(&self, task_id: TaskId) -> Option<TaskResult> {
        self.subagents.get(&task_id)?.completed_result.clone()
    }

    /// Cancel a running subagent (test-only).
    pub fn cancel_subagent(&self, task_id: TaskId) -> AlmsResult<()> {
        if let Some((_, handle)) = self.subagents.remove(&task_id) {
            let _ = handle.cancel_tx.send(());
            info!("Cancelled subagent {:?}", task_id);
            Ok(())
        } else {
            Err(alms_core::AlmsError::AgentNotFound(task_id.0.to_string()))
        }
    }

    /// Get status of a subagent (test-only).
    pub fn get_status(&self, task_id: TaskId) -> Option<TaskStatus> {
        self.subagents.get(&task_id).map(|h| h.status)
    }

    /// List all active subagents (test-only).
    pub fn list_active(&self) -> Vec<(TaskId, TaskStatus)> {
        self.subagents
            .iter()
            .map(|e| (*e.key(), e.value().status))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alms_runtime::llm_types::LlmConfig;

    /// Build a Coordinator wired to the mock LLM and an in-memory SessionManager.
    fn test_coordinator() -> Coordinator {
        let session_manager = Arc::new(SessionManager::new(alms_session::SessionConfig::default()));
        let llm_config = LlmConfig {
            mock: true,
            ..LlmConfig::default()
        };
        let llm = LlmClient::new(llm_config).unwrap();
        Coordinator::new(AgentId::new(), session_manager, llm)
    }

    fn test_session_id() -> SessionId {
        SessionId::new()
    }

    // -- (a) dispatch foreground — success path returns response text -----------

    #[tokio::test]
    async fn test_dispatch_foreground_success() {
        let coord = test_coordinator();
        let result = coord
            .dispatch(
                "Say hello".to_string(),
                test_session_id(),
                None,
                None,
                None,
                None,
            )
            .await;

        let (response, sub_session_id) = result.expect("dispatch should succeed");
        // Mock LLM echoes "[mock] <input>" — the agent runtime wraps it as
        // the assistant response.
        assert!(
            response.contains("mock"),
            "Expected mock response, got: {response}"
        );
        // The subagent session ID should be a valid (non-nil) UUID.
        assert_ne!(
            sub_session_id.0,
            uuid::Uuid::nil(),
            "subagent session ID should be non-nil"
        );
    }

    // -- (b) dispatch_background spawns successfully ----------------------------

    #[tokio::test]
    async fn test_dispatch_background_spawns() {
        let coord = test_coordinator();
        let (task_uuid, sub_session_id) = coord
            .dispatch_background(
                "Background work".to_string(),
                test_session_id(),
                None,
                None,
                None,
                None,
            )
            .await
            .expect("dispatch_background should succeed");

        // The returned UUID should be non-nil (a real task was created).
        assert_ne!(task_uuid, uuid::Uuid::nil());
        // The subagent session ID should be non-nil too.
        assert_ne!(
            sub_session_id.0,
            uuid::Uuid::nil(),
            "subagent session ID should be non-nil"
        );

        // Wait briefly for the mock LLM to complete — the task should
        // eventually reach a terminal state in the DashMap.
        let tid = TaskId(task_uuid);
        let mut found_terminal = false;
        for _ in 0..50 {
            match coord.get_status(tid) {
                Some(TaskStatus::Completed) | Some(TaskStatus::Failed) => {
                    found_terminal = true;
                    break;
                }
                _ => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
        assert!(
            found_terminal,
            "Background subagent should reach terminal state"
        );
    }

    // -- (d) cancel_subagent removes handle from DashMap -----------------------
    //
    // NOTE: The mock LLM completes synchronously, so by the time we call
    // cancel_subagent the subagent has likely already finished.  This test
    // verifies the DashMap removal path, not true mid-execution cancellation.
    // Testing real cancellation would require a mock LLM with injected latency.

    #[tokio::test]
    async fn test_cancel_subagent_removes_handle() {
        let coord = test_coordinator();
        let session_id = test_session_id();

        let request = SubagentRequest {
            task: "Long running task".to_string(),
            timeout: Duration::from_secs(300),
            parent_session: session_id,
            parent_run_id: None,
            subagent_name: None,
        };
        let (task_id, _sub_session_id) = coord
            .spawn_subagent(request, None, false, None)
            .await
            .unwrap();

        let cancel_result = coord.cancel_subagent(task_id);
        assert!(cancel_result.is_ok(), "cancel should succeed");

        // cancel_subagent removes the handle from the DashMap.
        assert!(
            coord.get_status(task_id).is_none(),
            "Handle should be removed after cancel"
        );
    }

    // -- (e) very short timeout — race between timeout and mock completion ------
    //
    // NOTE: The mock LLM's complete() returns without yielding, so
    // tokio::select! may resolve the agent-loop branch before the 1ns timer.
    // This test accepts both Failed (timeout won) and Completed (mock won).

    #[tokio::test]
    async fn test_very_short_timeout() {
        let coord = test_coordinator();
        let session_id = test_session_id();

        let request = SubagentRequest {
            task: "Will timeout".to_string(),
            timeout: Duration::from_nanos(1),
            parent_session: session_id,
            parent_run_id: None,
            subagent_name: None,
        };
        let (task_id, _sub_session_id) = coord
            .spawn_subagent(request, None, false, None)
            .await
            .unwrap();
        let result_rx = coord.take_result_rx(task_id).unwrap();

        let task_result = result_rx.await.expect("should receive result");
        assert!(
            task_result.status == TaskStatus::Failed || task_result.status == TaskStatus::Completed,
            "Expected Failed or Completed, got: {:?}",
            task_result.status
        );
    }

    // -- (g) list_active shows spawned subagents --------------------------------

    #[tokio::test]
    async fn test_list_active_includes_spawned() {
        let coord = test_coordinator();
        let session_id = test_session_id();

        let request = SubagentRequest {
            task: "List test".to_string(),
            timeout: Duration::from_secs(300),
            parent_session: session_id,
            parent_run_id: None,
            subagent_name: None,
        };
        let (task_id, _sub_session_id) = coord
            .spawn_subagent(request, None, false, None)
            .await
            .unwrap();

        let active = coord.list_active();
        assert!(
            active.iter().any(|(id, _)| *id == task_id),
            "Spawned task should appear in list_active"
        );
    }

    // -- (h) cancel unknown task → Err ------------------------------------------

    #[tokio::test]
    async fn test_cancel_unknown_task_returns_error() {
        let coord = test_coordinator();
        let result = coord.cancel_subagent(TaskId::new());
        assert!(result.is_err(), "cancelling unknown task should return Err");
    }

    // -- (i) take_result_rx — second call returns None --------------------------

    #[tokio::test]
    async fn test_take_result_rx_only_once() {
        let coord = test_coordinator();
        let session_id = test_session_id();

        let request = SubagentRequest {
            task: "Take rx test".to_string(),
            timeout: Duration::from_secs(300),
            parent_session: session_id,
            parent_run_id: None,
            subagent_name: None,
        };
        let (task_id, _sub_session_id) = coord
            .spawn_subagent(request, None, false, None)
            .await
            .unwrap();

        let first = coord.take_result_rx(task_id);
        assert!(first.is_some(), "first take should return the receiver");

        let second = coord.take_result_rx(task_id);
        assert!(second.is_none(), "second take should return None");
    }

    // -- (j-pre) subagent config inherits parent runtime settings ---------------

    #[test]
    fn test_subagent_inherits_parent_config() {
        let parent = AgentConfig {
            system_prompt: "parent prompt".into(),
            max_iterations: 42,
            max_tokens: 9999,
            context_config: alms_core::config::ContextConfig {
                strategy: "sliding-summary".into(),
                max_input_tokens: 50_000,
                recent_window: 5,
                summary_interval: 10,
                summary_model: Some("cheap-model".into()),
                ..Default::default()
            },
            posture: alms_runtime::Posture::Guarded,
            sandbox_root: "/sandbox".into(),
            shell_policy: "unrestricted".into(),
            shell_spill: alms_core::config::ShellSpillConfig {
                enabled: false,
                retention_days: 42,
            },
            enabled_tools: vec!["echo".into(), "math".into()],
            ..AgentConfig::default()
        };

        // Ephemeral subagent (no registry record)
        let (config, model, _provider) = agent_config_for_subagent(None, &parent);
        assert!(model.is_none());
        // Should inherit runtime settings from parent
        assert_eq!(config.max_iterations, 42);
        assert_eq!(config.max_tokens, 9999);
        assert_eq!(config.context_config.strategy, "sliding-summary");
        assert_eq!(config.context_config.max_input_tokens, 50_000);
        assert_eq!(config.context_config.recent_window, 5);
        // Should inherit sandbox settings
        assert_eq!(config.sandbox_root, "/sandbox");
        assert_eq!(config.shell_policy, "unrestricted");
        assert_eq!(config.enabled_tools, vec!["echo", "math"]);
        // Should inherit the shell spill policy (issue #756 subagent inheritance)
        assert!(!config.shell_spill.enabled);
        assert_eq!(config.shell_spill.retention_days, 42);
        // system_prompt should be the default subagent prompt, not the parent's
        assert_eq!(config.system_prompt, DEFAULT_SUBAGENT_PROMPT);

        // Named subagent with registry overrides
        let record = SubagentRecordConfig {
            model: Some("gpt-5".into()),
            posture: Some("guarded".into()),
            provider: Some("anthropic".into()),
            thinking_budget_tokens: None,
            reasoning_effort: None,
        };
        let (config2, model2, _provider2) = agent_config_for_subagent(Some(record), &parent);
        assert_eq!(model2.as_deref(), Some("gpt-5"));
        // system_prompt is always the default subagent prompt (not overridable per-agent)
        assert_eq!(config2.system_prompt, DEFAULT_SUBAGENT_PROMPT);
        assert_eq!(config2.posture, alms_runtime::Posture::Guarded);
        // Should still inherit runtime settings from parent
        assert_eq!(config2.max_iterations, 42);
        assert_eq!(config2.max_tokens, 9999);
        assert_eq!(config2.context_config.max_input_tokens, 50_000);
        // Shell spill policy still inherited through the registry-override path
        assert!(!config2.shell_spill.enabled);
        assert_eq!(config2.shell_spill.retention_days, 42);
    }

    // -- (j-pre-2) subagent inherits shell spill policy (issue #756) ------------
    //
    // Regression guard for Tim's `[important]` finding on PR #761: subagents
    // built via `agent_config_for_subagent` must carry the parent's
    // `shell_spill` state so the coordinator's subagent spawn path can wire
    // a spill directory into the subagent's ShellTool. Without this, a
    // subagent whose shell command produces >30 KB of output gets silent
    // truncation with no spill file — a regression from the parent's
    // behaviour.
    #[test]
    fn test_subagent_inherits_shell_spill_policy() {
        // Non-default spill config on the parent: flipped `enabled`, custom
        // retention.  Both fields must copy through verbatim.
        let parent = AgentConfig {
            shell_spill: alms_core::config::ShellSpillConfig {
                enabled: true,
                retention_days: 14,
            },
            ..AgentConfig::default()
        };

        // Ephemeral subagent path
        let (ephemeral, _, _) = agent_config_for_subagent(None, &parent);
        assert!(ephemeral.shell_spill.enabled);
        assert_eq!(ephemeral.shell_spill.retention_days, 14);

        // Named subagent path — registry overrides must not wipe the
        // inherited spill config.
        let record = SubagentRecordConfig {
            model: Some("gpt-5".into()),
            posture: None,
            provider: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
        };
        let (named, _, _) = agent_config_for_subagent(Some(record), &parent);
        assert!(named.shell_spill.enabled);
        assert_eq!(named.shell_spill.retention_days, 14);

        // Opt-out must also propagate — an operator who disabled spill in
        // `alms.toml` should see their subagents honour that too.
        let disabled_parent = AgentConfig {
            shell_spill: alms_core::config::ShellSpillConfig {
                enabled: false,
                retention_days: 1,
            },
            ..AgentConfig::default()
        };
        let (sub, _, _) = agent_config_for_subagent(None, &disabled_parent);
        assert!(!sub.shell_spill.enabled);
        assert_eq!(sub.shell_spill.retention_days, 1);
    }

    // -- (j-pre-3) subagent thinking-budget three-layer precedence (Tim S1) -----
    //
    // Parity with gateway `test_thinking_per_agent_zero_disables`: a named
    // subagent registered with `thinking_budget_tokens = Some(0)` must
    // honour its own registry override and disable extended thinking, even
    // when the parent enables it with `Some(4096)`. Ephemeral subagents
    // (record = None) still inherit the parent budget.
    #[test]
    fn test_subagent_thinking_budget_override() {
        // Parent has extended thinking enabled at 4096 tokens.
        let parent = AgentConfig {
            anthropic_thinking_budget: 4096,
            ..AgentConfig::default()
        };

        // Ephemeral subagent: no registry → inherit parent's 4096.
        let (ephemeral, _, _) = agent_config_for_subagent(None, &parent);
        assert_eq!(
            ephemeral.anthropic_thinking_budget, 4096,
            "ephemeral subagents inherit the parent's thinking budget"
        );

        // Named subagent registered with Some(0): explicit per-agent opt-out
        // must win over the parent's enabled-by-default 4096.
        let record_zero = SubagentRecordConfig {
            model: None,
            posture: None,
            provider: None,
            thinking_budget_tokens: Some(0),
            reasoning_effort: None,
        };
        let (sub_zero, _, _) = agent_config_for_subagent(Some(record_zero), &parent);
        assert_eq!(
            sub_zero.anthropic_thinking_budget, 0,
            "named subagent Some(0) must disable thinking even when parent enables it"
        );

        // Named subagent registered with Some(n > 0): explicit opt-in with a
        // different budget overrides the parent's value.
        let record_explicit = SubagentRecordConfig {
            model: None,
            posture: None,
            provider: None,
            thinking_budget_tokens: Some(8192),
            reasoning_effort: None,
        };
        let (sub_explicit, _, _) = agent_config_for_subagent(Some(record_explicit), &parent);
        assert_eq!(
            sub_explicit.anthropic_thinking_budget, 8192,
            "named subagent Some(n) must override the parent's thinking budget"
        );

        // Named subagent registered with None: unconfigured → inherit parent.
        let record_none = SubagentRecordConfig {
            model: None,
            posture: None,
            provider: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
        };
        let (sub_none, _, _) = agent_config_for_subagent(Some(record_none), &parent);
        assert_eq!(
            sub_none.anthropic_thinking_budget, 4096,
            "named subagent with no override inherits the parent's budget"
        );
    }

    // -- (j-pre-4) subagent reasoning-effort three-layer precedence (#768) ------
    //
    // Mirrors the Anthropic path above: a named subagent registered with
    // `reasoning_effort = Some(Low)` must honour its own registry override
    // and override the parent's `Some(High)`. Ephemeral subagents
    // (record = None) still inherit the parent's effort.
    #[test]
    fn test_subagent_reasoning_effort_override() {
        use alms_core::config::ReasoningEffort;

        // Parent has reasoning set to High.
        let parent = AgentConfig {
            openai_reasoning_effort: Some(ReasoningEffort::High),
            ..AgentConfig::default()
        };

        // Ephemeral subagent: no registry → inherit parent's High.
        let (ephemeral, _, _) = agent_config_for_subagent(None, &parent);
        assert_eq!(
            ephemeral.openai_reasoning_effort,
            Some(ReasoningEffort::High),
            "ephemeral subagents inherit the parent's reasoning effort"
        );

        // Named subagent registered with Some(Low): must override parent.
        let record_low = SubagentRecordConfig {
            model: None,
            posture: None,
            provider: None,
            thinking_budget_tokens: None,
            reasoning_effort: Some(ReasoningEffort::Low),
        };
        let (sub_low, _, _) = agent_config_for_subagent(Some(record_low), &parent);
        assert_eq!(
            sub_low.openai_reasoning_effort,
            Some(ReasoningEffort::Low),
            "named subagent Some(Low) must override parent's High"
        );

        // Named subagent with None override: inherit parent's High.
        let record_none = SubagentRecordConfig {
            model: None,
            posture: None,
            provider: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
        };
        let (sub_none, _, _) = agent_config_for_subagent(Some(record_none), &parent);
        assert_eq!(
            sub_none.openai_reasoning_effort,
            Some(ReasoningEffort::High),
            "named subagent with None override inherits the parent's effort"
        );
    }

    // -- (j) get_completed_result on unknown task → None ------------------------

    #[tokio::test]
    async fn test_get_completed_result_unknown() {
        let coord = test_coordinator();
        assert!(
            coord.get_completed_result(TaskId::new()).is_none(),
            "Unknown task should return None"
        );
    }

    // -- (k) named subagent reuses session across invocations --------------------

    #[tokio::test]
    async fn test_named_subagent_persistent_session() {
        // Use tempfile::TempDir for RAII cleanup — automatic drop even on panic.
        // (Previously this test used workspace_dir: None — see #55.)
        let workspace_tmp = tempfile::TempDir::new().unwrap();
        let workspace_dir = workspace_tmp.path().to_path_buf();
        let coord = test_coordinator().with_workspace_dir(workspace_dir.clone());
        let parent_session = test_session_id();

        // First invocation with name "reviewer"
        let (r1, sub_sid_1) = coord
            .dispatch(
                "First task".to_string(),
                parent_session,
                None,
                None,
                Some("reviewer".to_string()),
                None,
            )
            .await
            .expect("first dispatch should succeed");
        assert!(r1.contains("mock"), "Expected mock response: {r1}");

        // Second invocation with same name — should reuse session (history preserved)
        let (r2, sub_sid_2) = coord
            .dispatch(
                "Follow up".to_string(),
                parent_session,
                None,
                None,
                Some("reviewer".to_string()),
                None,
            )
            .await
            .expect("second dispatch should succeed");
        assert!(r2.contains("mock"), "Expected mock response: {r2}");

        // Named subagents reuse sessions — both calls should return the same session ID.
        assert_eq!(
            sub_sid_1, sub_sid_2,
            "Named subagent should reuse the same session across invocations"
        );

        // Verify session was reused: the session manager should have exactly one
        // session for the derived (agent_id, context_id) pair
        let parent_as_agent = AgentId(parent_session.0);
        let stable_id = AgentId::deterministic(parent_as_agent, "reviewer");
        let stable_ctx = format!("subagent_{}_{}", parent_session.0, "reviewer");
        let session = coord.session_manager.get_or_create(stable_id, &stable_ctx);

        // Should have 4 messages: user1, assistant1, user2, assistant2
        let messages = coord.session_manager.get_history(session.id).unwrap();
        assert_eq!(
            messages.len(),
            4,
            "Named subagent should have 4 messages (2 turns), got {}",
            messages.len()
        );

        // Verify workspace attachment: the named subagent's workspace directory
        // should have been created at {workspace_dir}/reviewer/
        let reviewer_ws = workspace_dir.join("reviewer");
        assert!(
            reviewer_ws.exists(),
            "Named subagent workspace directory should exist at {}",
            reviewer_ws.display()
        );
        // workspace_tmp drops here — automatic cleanup even on panic
    }

    // -- (l) concurrent named subagent invocations are rejected -----------------

    #[tokio::test]
    async fn test_concurrent_named_subagent_rejected() {
        let coord = test_coordinator();
        let session_id = test_session_id();

        // Spawn a named subagent with a long timeout so it stays active
        let request = SubagentRequest {
            task: "Long task".to_string(),
            timeout: Duration::from_secs(300),
            parent_session: session_id,
            parent_run_id: None,
            subagent_name: Some("researcher".to_string()),
        };
        let (_task_id, _sub_session_id) = coord
            .spawn_subagent(request, None, false, None)
            .await
            .unwrap();

        // Second invocation with the same name should be rejected
        let request2 = SubagentRequest {
            task: "Another task".to_string(),
            timeout: Duration::from_secs(300),
            parent_session: session_id,
            parent_run_id: None,
            subagent_name: Some("researcher".to_string()),
        };
        let result = coord.spawn_subagent(request2, None, false, None).await;
        assert!(
            result.is_err(),
            "Second concurrent spawn should be rejected"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("already running"),
            "Error should mention 'already running': {err}"
        );

        // Different name should still work
        let request3 = SubagentRequest {
            task: "Different agent".to_string(),
            timeout: Duration::from_secs(300),
            parent_session: session_id,
            parent_run_id: None,
            subagent_name: Some("coder".to_string()),
        };
        assert!(
            coord
                .spawn_subagent(request3, None, false, None)
                .await
                .is_ok(),
            "Different named subagent should succeed"
        );
    }

    #[tokio::test]
    async fn test_unnamed_subagent_ephemeral_session() {
        let coord = test_coordinator();
        let parent_session = test_session_id();

        // Two invocations without name — each should get a fresh session
        let (_r1, sub_sid_1) = coord
            .dispatch(
                "Task one".to_string(),
                parent_session,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("first dispatch should succeed");

        let (_r2, sub_sid_2) = coord
            .dispatch(
                "Task two".to_string(),
                parent_session,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("second dispatch should succeed");

        // Ephemeral subagents get unique sessions — IDs should differ.
        assert_ne!(
            sub_sid_1, sub_sid_2,
            "Ephemeral subagents should have different session IDs"
        );

        // Each ephemeral invocation creates its own session, so we can't
        // look up a single session with all 4 messages. This test verifies
        // that the calls succeed independently (no shared state).
    }

    // -- truncate_for_notification -----------------------------------------------

    #[test]
    fn test_truncate_short_response() {
        let result = serde_json::json!({"response": "Hello world"});
        assert_eq!(truncate_for_notification(&result), "Hello world");
    }

    #[test]
    fn test_truncate_long_response() {
        let long = "a".repeat(1000);
        let result = serde_json::json!({"response": long});
        let truncated = truncate_for_notification(&result);
        assert!(truncated.len() < 1000);
        assert!(truncated.ends_with("…[truncated]"));
        // 800 chars of 'a' + the suffix
        assert!(truncated.starts_with(&"a".repeat(800)));
    }

    #[test]
    fn test_truncate_error_field() {
        let result = serde_json::json!({"error": "something broke"});
        assert_eq!(truncate_for_notification(&result), "something broke");
    }

    #[test]
    fn test_truncate_no_content() {
        let result = serde_json::json!({"cancelled": true});
        assert_eq!(truncate_for_notification(&result), "[no content]");
    }

    // -- event bridge auto-denies subagent ApprovalRequired -----------------

    #[tokio::test]
    async fn test_event_bridge_auto_denies_approval() {
        use alms_runtime::RuntimeEvent;

        // Create a channel pair simulating the subagent's event channel.
        let (sub_tx, mut sub_rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();

        // Create the approval oneshot.
        let (decision_tx, decision_rx) = tokio::sync::oneshot::channel::<bool>();

        // Simulate the subagent emitting an ApprovalRequired event.
        sub_tx
            .send(RuntimeEvent::ApprovalRequired {
                approval_id: Uuid::new_v4(),
                tool: "shell_exec".to_string(),
                params: serde_json::json!({"cmd": "rm -rf /"}),
                decision_tx,
                source_agent: None,
            })
            .unwrap();
        drop(sub_tx); // close channel so the loop terminates

        // Simulate the coordinator event bridge logic: read events and
        // auto-deny ApprovalRequired.
        while let Some(event) = sub_rx.recv().await {
            if let RuntimeEvent::ApprovalRequired { decision_tx, .. } = event {
                let _ = decision_tx.send(false);
            }
        }

        // The subagent side should receive `false` (denial).
        let result = decision_rx.await;
        assert_eq!(result, Ok(false), "ApprovalRequired should be auto-denied");
    }

    // -- background subagent posture override (Fixes #396) -------------------
    //
    // These tests exercise the extracted `resolve_subagent_posture()` helper
    // directly, so they stay in sync with the logic used by `run_agent_loop`.

    #[test]
    fn test_background_subagent_guarded_overridden_to_autonomous() {
        assert_eq!(
            resolve_subagent_posture(true, alms_runtime::Posture::Guarded),
            alms_runtime::Posture::Autonomous,
            "Guarded posture should be overridden to Autonomous for background subagents"
        );
    }

    #[test]
    fn test_background_subagent_autonomous_unchanged() {
        assert_eq!(
            resolve_subagent_posture(true, alms_runtime::Posture::Autonomous),
            alms_runtime::Posture::Autonomous,
            "Autonomous posture should remain unchanged for background subagents"
        );
    }

    #[test]
    fn test_background_subagent_full_control_unchanged() {
        assert_eq!(
            resolve_subagent_posture(true, alms_runtime::Posture::FullControl),
            alms_runtime::Posture::FullControl,
            "FullControl posture should NOT be overridden for background subagents"
        );
    }

    #[test]
    fn test_foreground_subagent_guarded_unchanged() {
        assert_eq!(
            resolve_subagent_posture(false, alms_runtime::Posture::Guarded),
            alms_runtime::Posture::Guarded,
            "Guarded posture should be preserved for foreground subagents"
        );
    }

    #[test]
    fn test_truncate_multibyte_boundary() {
        // 799 ASCII chars + a 2-byte char at position 799-800 = would split mid-char at 800
        let mut s = "a".repeat(799);
        s.push('é'); // 2-byte UTF-8
        s.push_str("zzz");
        let result = serde_json::json!({"response": s});
        let truncated = truncate_for_notification(&result);
        assert!(truncated.ends_with("…[truncated]"));
        // Must not panic or produce invalid UTF-8
        assert!(truncated.is_char_boundary(truncated.len()));
    }
}
