use alms_core::{AgentId, AlmsResult, RunId, SessionId};
use alms_runtime::events::RuntimeEventSender;
use alms_runtime::subagent::{PollResult, SubagentDispatcher};
use alms_runtime::{AgentConfig, AgentRuntime, LlmClient, RunOutput};
use alms_session::SessionManager;
use async_trait::async_trait;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// How long (in seconds) a subagent is allowed to run before it times out,
/// and also how long its result is kept in memory after completion so that
/// background callers can poll via `get_task_result`.
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
    pub execution_time_ms: u64,
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
    pub execution_time_ms: u64,
    /// Parent session to notify.
    pub parent_session_id: SessionId,
    /// Parent agent ID (for run creation).
    pub parent_agent_id: AgentId,
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
    /// Whether this was spawned via `dispatch_background` (triggers completion notification).
    pub is_background: bool,
    /// Receiver for the final TaskResult — taken by `dispatch()` to await completion.
    pub result_rx: Option<oneshot::Receiver<TaskResult>>,
    /// Stored result for background tasks — set by `run_subagent` on completion
    /// so `poll_task` can retrieve it without consuming the oneshot receiver.
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
    /// Base agent config — subagents inherit sandbox settings from this
    base_agent_config: AgentConfig,
    /// Workspace base directory — named subagents get workspaces under this dir
    workspace_dir: Option<std::path::PathBuf>,
    /// Tracks the last-used system_prompt per named subagent context key,
    /// so we can warn when a re-invocation uses a different prompt.
    subagent_prompts: Arc<DashMap<String, String>>,
    /// Channel for notifying the gateway when a background subagent completes.
    /// The gateway listens on the receiving end and creates follow-up runs.
    completion_tx: Option<mpsc::UnboundedSender<SubagentCompletion>>,
}

impl Coordinator {
    pub fn new(main_agent: AgentId, session_manager: Arc<SessionManager>, llm: LlmClient) -> Self {
        Self {
            main_agent,
            subagents: Arc::new(DashMap::new()),
            active_named: Arc::new(dashmap::DashSet::new()),
            session_manager,
            llm,
            base_agent_config: AgentConfig::default(),
            workspace_dir: None,
            subagent_prompts: Arc::new(DashMap::new()),
            completion_tx: None,
        }
    }

    /// Create a coordinator that inherits sandbox settings from the given config.
    pub fn with_agent_config(
        main_agent: AgentId,
        session_manager: Arc<SessionManager>,
        llm: LlmClient,
        base_agent_config: AgentConfig,
    ) -> Self {
        Self {
            main_agent,
            subagents: Arc::new(DashMap::new()),
            active_named: Arc::new(dashmap::DashSet::new()),
            session_manager,
            llm,
            base_agent_config,
            workspace_dir: None,
            subagent_prompts: Arc::new(DashMap::new()),
            completion_tx: None,
        }
    }

    /// Set the workspace base directory. Named subagents will get workspaces
    /// under `{workspace_dir}/{agent_name}/`.
    pub fn with_workspace_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.workspace_dir = Some(dir);
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
        parent_event_tx: Option<RuntimeEventSender>,
        is_background: bool,
        parent_cancel_token: Option<CancellationToken>,
    ) -> AlmsResult<TaskId> {
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

        let handle = SubagentHandle {
            task_id,
            status: TaskStatus::Pending,
            started_at: chrono::Utc::now(),
            cancel_tx,
            parent_run_id,
            parent_session_id,
            parent_agent_id,
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
        let base_agent_config = self.base_agent_config.clone();
        let workspace_dir = self.workspace_dir.clone();
        let subagent_prompts = self.subagent_prompts.clone();
        let completion_tx = self.completion_tx.clone();

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
                    subagent_prompts,
                    completion_tx,
                    parent_cancel_token,
                )
                .await;
            }
            .instrument(span),
        );

        Ok(task_id)
    }

    /// Take the result receiver for a task (can only be called once per task).
    ///
    /// Returns `None` if the task does not exist or the receiver was already taken.
    pub fn take_result_rx(&self, task_id: TaskId) -> Option<oneshot::Receiver<TaskResult>> {
        self.subagents.get_mut(&task_id)?.result_rx.take()
    }

    /// Get the completed result for a finished background task.
    ///
    /// Returns `None` if the task is still running, not found, or is a
    /// foreground task whose result was consumed by `dispatch()`.
    pub fn get_completed_result(&self, task_id: TaskId) -> Option<TaskResult> {
        self.subagents.get(&task_id)?.completed_result.clone()
    }

    /// Cancel a running subagent
    pub fn cancel_subagent(&self, task_id: TaskId) -> AlmsResult<()> {
        if let Some((_, handle)) = self.subagents.remove(&task_id) {
            let _ = handle.cancel_tx.send(());
            info!("Cancelled subagent {:?}", task_id);
            Ok(())
        } else {
            Err(alms_core::AlmsError::AgentNotFound(task_id.0.to_string()))
        }
    }

    /// Get status of a subagent
    pub fn get_status(&self, task_id: TaskId) -> Option<TaskStatus> {
        self.subagents.get(&task_id).map(|h| h.status)
    }

    /// List all active subagents
    pub fn list_active(&self) -> Vec<(TaskId, TaskStatus)> {
        self.subagents
            .iter()
            .map(|e| (*e.key(), e.value().status))
            .collect()
    }
}

#[async_trait]
impl SubagentDispatcher for Coordinator {
    async fn dispatch(
        &self,
        task: String,
        parent_session_id: SessionId,
        parent_run_id: Option<RunId>,
        parent_event_tx: Option<RuntimeEventSender>,
        subagent_name: Option<String>,
        parent_cancel_token: Option<CancellationToken>,
    ) -> AlmsResult<String> {
        let request = SubagentRequest {
            task,
            timeout: Duration::from_secs(SUBAGENT_TTL_SECS),
            parent_session: parent_session_id,
            parent_run_id,
            subagent_name,
        };

        let task_id = self
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
            TaskStatus::Completed => Ok(task_result.result["response"]
                .as_str()
                .unwrap_or("[no response]")
                .to_string()),
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
        parent_event_tx: Option<RuntimeEventSender>,
        subagent_name: Option<String>,
        parent_cancel_token: Option<CancellationToken>,
    ) -> alms_core::AlmsResult<Uuid> {
        let request = SubagentRequest {
            task,
            timeout: Duration::from_secs(SUBAGENT_TTL_SECS),
            parent_session: parent_session_id,
            parent_run_id,
            subagent_name,
        };
        let task_id = self
            .spawn_subagent(request, parent_event_tx, true, parent_cancel_token)
            .await?;

        // Drop the oneshot receiver — background callers poll via completed_result,
        // not the channel. This frees the allocation; run_subagent's result_tx.send()
        // will silently fail (already uses `let _ = ...`), which is intentional.
        drop(self.take_result_rx(task_id));

        info!(
            task_id = %task_id.0,
            "Background subagent spawned (non-blocking)"
        );
        Ok(task_id.0)
    }

    #[instrument(level = "debug", skip(self), fields(task_id = %task_id))]
    async fn poll_task(&self, task_id: Uuid) -> alms_core::AlmsResult<PollResult> {
        let tid = TaskId(task_id);
        match self.get_status(tid) {
            None => Err(alms_core::AlmsError::Runtime(format!(
                "Task {task_id} not found (may have expired after {SUBAGENT_TTL_SECS}s)"
            ))),
            Some(TaskStatus::Pending | TaskStatus::Running) => Ok(PollResult::Running),
            Some(done_status) => match self.get_completed_result(tid) {
                None => Err(alms_core::AlmsError::Runtime(
                    "Task finished but result unavailable".to_string(),
                )),
                Some(result) => Ok(match done_status {
                    TaskStatus::Completed => PollResult::Completed(
                        result.result["response"]
                            .as_str()
                            .unwrap_or("[no response]")
                            .to_string(),
                    ),
                    TaskStatus::Failed => PollResult::Failed(
                        result.result["error"]
                            .as_str()
                            .unwrap_or("subagent failed")
                            .to_string(),
                    ),
                    TaskStatus::Cancelled => PollResult::Cancelled,
                    _ => PollResult::Failed("unexpected terminal state".to_string()),
                }),
            },
        }
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
    parent_event_tx: Option<RuntimeEventSender>,
    base_agent_config: AgentConfig,
    workspace_dir: Option<std::path::PathBuf>,
    subagent_prompts: Arc<DashMap<String, String>>,
    completion_tx: Option<mpsc::UnboundedSender<SubagentCompletion>>,
    parent_cancel_token: Option<CancellationToken>,
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

    let (new_status, result_value, tokens_used) = tokio::select! {
        _ = tokio::time::sleep(request.timeout) => {
            warn!(
                target: "subagent::timeout",
                task_id = %task_id.0,
                timeout_secs = %request.timeout.as_secs(),
                "Subagent timed out"
            );
            (TaskStatus::Failed, serde_json::json!({"error": "Timeout"}), None)
        }
        _ = child_cancel_token.cancelled() => {
            info!(
                target: "subagent::cancelled",
                task_id = %task_id.0,
                "Subagent cancelled"
            );
            (TaskStatus::Cancelled, serde_json::json!({"cancelled": true}), None)
        }
        output = run_agent_loop(task_id, &request, &session_manager, &llm, parent_event_tx, &base_agent_config, workspace_dir.as_deref(), &subagent_prompts, child_cancel_token.clone()) => {
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
                    )
                }
            }
        }
    };

    // Cancel child token to clean up the bridge task (if it's still waiting
    // on the oneshot). This is a no-op if the token was already cancelled.
    child_cancel_token.cancel();
    bridge_handle.abort();

    let task_result = TaskResult {
        task_id,
        status: new_status,
        result: result_value,
        execution_time_ms: start.elapsed().as_millis() as u64,
        tokens_used,
    };

    // Store result in the handle for background-mode polling, then update status.
    // Also capture background flag and parent info for the completion notification.
    let background_info = if let Some(mut handle) = subagents.get_mut(&task_id) {
        handle.status = new_status;
        handle.completed_result = Some(task_result.clone());
        if handle.is_background {
            Some((handle.parent_session_id, handle.parent_agent_id))
        } else {
            None
        }
    } else {
        None
    };

    // Fire completion notification for background subagents so the gateway
    // can auto-create a follow-up run on the parent session.
    if let Some((parent_session_id, parent_agent_id)) = background_info
        && let Some(ref tx) = completion_tx
    {
        let summary = truncate_for_notification(&task_result.result);
        let completion = SubagentCompletion {
            task_id,
            subagent_name: request.subagent_name.clone(),
            status: new_status,
            summary,
            execution_time_ms: task_result.execution_time_ms,
            parent_session_id,
            parent_agent_id,
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

    if text.len() <= NOTIFICATION_SUMMARY_MAX_CHARS {
        text.to_string()
    } else {
        // Truncate at a char boundary
        let mut end = NOTIFICATION_SUMMARY_MAX_CHARS;
        while !text.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}…[truncated]", &text[..end])
    }
}

/// Default system prompt for ephemeral (unnamed) subagents.
const DEFAULT_SUBAGENT_PROMPT: &str = "You are a general-purpose assistant. Complete the given task thoroughly and accurately. \
     You can run `alms --help` via shell_exec to discover CLI commands \
     for managing agents, sessions, and runs.";

/// Config extracted from an agent registry record for a named subagent.
struct SubagentRecordConfig {
    system_prompt: Option<String>,
    model: Option<String>,
    posture: Option<String>,
    provider: Option<String>,
}

/// Build an `AgentConfig` for a subagent. Named subagents get their config
/// from the agent registry; ephemeral subagents use a default prompt.
/// Both inherit sandbox, tool, and runtime settings (max_iterations,
/// max_tokens, context_config) from the parent's base config.
fn agent_config_for_subagent(
    record: Option<SubagentRecordConfig>,
    base: &AgentConfig,
) -> (AgentConfig, Option<String>, Option<String>) {
    let (system_prompt, model, posture_str, provider) = match record {
        Some(r) => (r.system_prompt, r.model, r.posture, r.provider),
        None => (None, None, None, None),
    };

    let posture = match posture_str.as_deref() {
        Some("guarded") => alms_runtime::Posture::Guarded,
        Some("full_control") => alms_runtime::Posture::FullControl,
        _ => alms_runtime::Posture::FullControl,
    };

    let config = AgentConfig {
        system_prompt: system_prompt.unwrap_or_else(|| DEFAULT_SUBAGENT_PROMPT.to_string()),
        posture,
        sandbox_root: base.sandbox_root.clone(),
        shell_policy: base.shell_policy.clone(),
        enabled_tools: base.enabled_tools.clone(),
        max_iterations: base.max_iterations,
        max_tokens: base.max_tokens,
        context_config: base.context_config.clone(),
        prompts: base.prompts.clone(),
    };
    (config, model, provider)
}

/// Run the actual agent loop for a subagent.
///
/// Creates a fresh `AgentRuntime`, forwards its events to the parent's
/// event channel (if provided), then calls `runtime.run()`.
///
/// **Named subagents** (`subagent_name` is Some): looked up in the agent
/// registry for config (system_prompt, model, posture). Workspace is
/// attached if `workspace_dir` is set. Session identity is deterministic
/// (UUID v5 from parent session + name) — conversation history preserved.
///
/// **Ephemeral subagents** (`subagent_name` is None): fresh agent ID,
/// fresh session, default config, no workspace.
#[allow(clippy::too_many_arguments)]
async fn run_agent_loop(
    task_id: TaskId,
    request: &SubagentRequest,
    session_manager: &Arc<SessionManager>,
    llm: &LlmClient,
    parent_event_tx: Option<RuntimeEventSender>,
    base_agent_config: &AgentConfig,
    workspace_dir: Option<&std::path::Path>,
    subagent_prompts: &DashMap<String, String>,
    cancel_token: CancellationToken,
) -> AlmsResult<RunOutput> {
    // Derive identity and config based on whether the subagent is named
    let (agent_id, context_id, config, model_override, provider_override, attach_workspace) =
        if let Some(ref name) = request.subagent_name {
            // Named: deterministic identity, look up agent registry for config
            let parent_as_agent = AgentId(request.parent_session.0);
            let stable_id = AgentId::deterministic(parent_as_agent, name);
            let stable_ctx = format!("subagent_{}_{}", request.parent_session.0, name);

            // Look up agent record in registry for system_prompt/model/posture
            let record_config = session_manager
                .store()
                .and_then(|store| store.load_agent_by_name(name).ok())
                .flatten()
                .map(|record| {
                    debug!("Loaded agent record for named subagent '{name}'");
                    SubagentRecordConfig {
                        system_prompt: record.system_prompt,
                        model: record.model,
                        posture: record.posture,
                        provider: record.provider,
                    }
                })
                .or_else(|| {
                    warn!(
                        "Named subagent '{name}' not found in agent registry — using defaults. \
                         Create it with: alms agent create --name {name}"
                    );
                    None
                });

            let (config, model, provider) =
                agent_config_for_subagent(record_config, base_agent_config);

            // Detect system_prompt drift: warn when the prompt changes between
            // invocations of the same named subagent within the same parent session.
            //
            // Safety: concurrent invocations of the same named subagent are
            // rejected by the active_named guard in spawn_subagent(), so this
            // get-then-insert is not racy for a given stable_ctx.
            if let Some(prev_prompt) = subagent_prompts.get(&stable_ctx)
                && *prev_prompt != config.system_prompt
            {
                warn!(
                    subagent_name = %name,
                    context_id = %stable_ctx,
                    "Named subagent '{name}' system_prompt has changed since the last \
                     invocation. The existing session history was built under the \
                     previous prompt — this may cause inconsistent behavior."
                );
            }
            subagent_prompts.insert(stable_ctx.clone(), config.system_prompt.clone());

            (stable_id, stable_ctx, config, model, provider, true)
        } else {
            // Ephemeral: fresh each invocation
            let (config, _, _) = agent_config_for_subagent(None, base_agent_config);
            (
                AgentId::new(),
                format!("subagent_{}", task_id.0),
                config,
                None,
                None,
                false,
            )
        };

    // Create a per-subagent event channel
    let (sub_tx, sub_rx) = tokio::sync::mpsc::unbounded_channel::<alms_runtime::RuntimeEvent>();

    // Apply provider override first, then model override
    let mut subagent_llm = llm.clone();
    if let Some(ref provider) = provider_override {
        info!("Named subagent using provider override: {provider}");
        subagent_llm = subagent_llm.with_provider(provider);
    }
    if let Some(model) = model_override {
        info!("Named subagent using model override: {model}");
        subagent_llm = subagent_llm.with_model(model);
    }

    let mut runtime = AgentRuntime::new(agent_id, config, subagent_llm)?
        .with_event_sender(sub_tx)
        .with_cancel_token(cancel_token);

    // Attach workspace for named subagents: {workspace_dir}/{name}/
    if attach_workspace && let (Some(ws_dir), Some(name)) = (workspace_dir, &request.subagent_name)
    {
        let subagent_ws_dir = ws_dir.join(name);
        let workspace = alms_runtime::AgentWorkspace::with_dir(subagent_ws_dir);
        runtime = runtime.with_workspace(workspace);
    }

    // Forward subagent tool events into the parent run's event stream,
    // tagging each event with the subagent's identity so the UI can
    // distinguish subagent activity from parent activity.
    if let Some(parent_tx) = parent_event_tx {
        let label = request
            .subagent_name
            .clone()
            .unwrap_or_else(|| format!("subagent-{}", &task_id.0.to_string()[..8]));
        tokio::spawn(async move {
            use alms_runtime::RuntimeEvent;
            let mut rx = sub_rx;
            while let Some(mut event) = rx.recv().await {
                match &mut event {
                    RuntimeEvent::ToolStart { source_agent, .. }
                    | RuntimeEvent::ToolEnd { source_agent, .. }
                    | RuntimeEvent::TokenDelta { source_agent, .. }
                    | RuntimeEvent::ApprovalRequired { source_agent, .. } => {
                        *source_agent = Some(label.clone());
                    }
                }
                let _ = parent_tx.send(event);
            }
        });
    } else {
        // Nobody is consuming — drop the receiver so sends silently fail
        drop(sub_rx);
    }

    runtime
        .run(session_manager, &context_id, &request.task)
        .await
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

        let response = result.expect("dispatch should succeed");
        // Mock LLM echoes "[mock] <input>" — the agent runtime wraps it as
        // the assistant response.
        assert!(
            response.contains("mock"),
            "Expected mock response, got: {response}"
        );
    }

    // -- (b) dispatch_background + poll_task lifecycle --------------------------

    #[tokio::test]
    async fn test_dispatch_background_lifecycle() {
        let coord = test_coordinator();
        let task_uuid = coord
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

        // The mock LLM completes almost instantly, but the task may still be
        // running when we first poll.  Retry briefly.
        let mut result = None;
        for _ in 0..50 {
            match coord.poll_task(task_uuid).await {
                Ok(PollResult::Running) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Ok(other) => {
                    result = Some(other);
                    break;
                }
                Err(e) => panic!("poll_task error: {e}"),
            }
        }

        match result {
            Some(PollResult::Completed(text)) => {
                assert!(text.contains("mock"), "Expected mock response, got: {text}");
            }
            other => panic!("Expected Completed, got: {other:?}"),
        }
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
        let task_id = coord
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
        let task_id = coord
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

    // -- (f) poll_task on unknown task_id → Err ---------------------------------

    #[tokio::test]
    async fn test_poll_unknown_task_returns_error() {
        let coord = test_coordinator();
        let result = coord.poll_task(Uuid::new_v4()).await;
        assert!(result.is_err(), "polling unknown task should return Err");
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
        let task_id = coord
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
        let task_id = coord
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
            },
            posture: alms_runtime::Posture::Guarded,
            sandbox_root: "/sandbox".into(),
            shell_policy: "unrestricted".into(),
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
        // system_prompt should be the default subagent prompt, not the parent's
        assert_eq!(config.system_prompt, DEFAULT_SUBAGENT_PROMPT);

        // Named subagent with registry overrides
        let record = SubagentRecordConfig {
            system_prompt: Some("custom prompt".into()),
            model: Some("gpt-5".into()),
            posture: Some("guarded".into()),
            provider: Some("anthropic".into()),
        };
        let (config2, model2, _provider2) = agent_config_for_subagent(Some(record), &parent);
        assert_eq!(model2.as_deref(), Some("gpt-5"));
        assert_eq!(config2.system_prompt, "custom prompt");
        assert_eq!(config2.posture, alms_runtime::Posture::Guarded);
        // Should still inherit runtime settings from parent
        assert_eq!(config2.max_iterations, 42);
        assert_eq!(config2.max_tokens, 9999);
        assert_eq!(config2.context_config.max_input_tokens, 50_000);
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
        let coord = test_coordinator();
        let parent_session = test_session_id();

        // First invocation with name "reviewer"
        let r1 = coord
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
        let r2 = coord
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
        let _task_id = coord
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
        let _r1 = coord
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

        let _r2 = coord
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
