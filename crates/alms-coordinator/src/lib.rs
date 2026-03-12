use alms_core::agent::Capability;
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
use tokio::sync::oneshot;
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

/// Types of specialized subagents
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubagentType {
    /// Research and information gathering
    Research,
    /// Code generation and review
    Code,
    /// Data processing and analysis
    Data,
    /// External API integrations
    Integration,
    /// Security analysis
    Security,
    /// General purpose (default)
    General,
}

impl SubagentType {
    pub fn default_capabilities(&self) -> Vec<Capability> {
        match self {
            SubagentType::Research => vec![
                Capability::Search,
                Capability::Read,
                Capability::Custom("summarize".to_string()),
            ],
            SubagentType::Code => vec![
                Capability::CodeExecution,
                Capability::Custom("lint".to_string()),
                Capability::Custom("test_run".to_string()),
                Capability::Read,
                Capability::Write,
            ],
            SubagentType::Data => vec![
                Capability::Custom("query".to_string()),
                Capability::Custom("transform".to_string()),
                Capability::Custom("visualize".to_string()),
            ],
            SubagentType::Integration => vec![
                Capability::Http,
                Capability::Custom("webhook".to_string()),
                Capability::Custom("notify".to_string()),
            ],
            SubagentType::Security => vec![
                Capability::Custom("scan".to_string()),
                Capability::Custom("audit".to_string()),
                Capability::Custom("report".to_string()),
            ],
            SubagentType::General => vec![Capability::Custom("*".to_string())],
        }
    }
}

/// Request to spawn a subagent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentRequest {
    pub task: String,
    pub agent_type: SubagentType,
    pub timeout: Duration,
    pub capabilities: Vec<Capability>,
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

/// Handle to a running subagent
#[derive(Debug)]
pub struct SubagentHandle {
    pub task_id: TaskId,
    pub agent_type: SubagentType,
    pub status: TaskStatus,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub cancel_tx: oneshot::Sender<()>,
    pub parent_run_id: Option<RunId>,
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
    /// Shared session manager — used to give each subagent its own context
    session_manager: Arc<SessionManager>,
    /// LLM client — cloned for each subagent runtime
    llm: LlmClient,
    /// Base agent config — subagents inherit sandbox settings from this
    base_agent_config: AgentConfig,
    /// Workspace base directory — named subagents get workspaces under this dir
    workspace_dir: Option<std::path::PathBuf>,
}

impl Coordinator {
    pub fn new(main_agent: AgentId, session_manager: Arc<SessionManager>, llm: LlmClient) -> Self {
        Self {
            main_agent,
            subagents: Arc::new(DashMap::new()),
            session_manager,
            llm,
            base_agent_config: AgentConfig::default(),
            workspace_dir: None,
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
            session_manager,
            llm,
            base_agent_config,
            workspace_dir: None,
        }
    }

    /// Set the workspace base directory. Named subagents will get workspaces
    /// under `{workspace_dir}/{agent_name}/`.
    pub fn with_workspace_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.workspace_dir = Some(dir);
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
            subagent_type = ?request.agent_type,
            parent_session = %request.parent_session.0,
            timeout_secs = %request.timeout.as_secs(),
        )
    )]
    pub async fn spawn_subagent(
        &self,
        request: SubagentRequest,
        parent_event_tx: Option<RuntimeEventSender>,
    ) -> AlmsResult<TaskId> {
        let task_id = TaskId::new();
        let agent_type = request.agent_type;
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let (result_tx, result_rx) = oneshot::channel::<TaskResult>();
        let parent_run_id = request.parent_run_id;

        let handle = SubagentHandle {
            task_id,
            agent_type,
            status: TaskStatus::Pending,
            started_at: chrono::Utc::now(),
            cancel_tx,
            parent_run_id,
            result_rx: Some(result_rx),
            completed_result: None,
        };

        self.subagents.insert(task_id, handle);

        info!(
            target: "coordinator::subagent_spawned",
            task_id = %task_id.0,
            subagent_type = ?agent_type,
            parent_session = %request.parent_session.0,
            "Subagent spawned"
        );

        let subagents = self.subagents.clone();
        let session_manager = self.session_manager.clone();
        let llm = self.llm.clone();
        let base_agent_config = self.base_agent_config.clone();
        let workspace_dir = self.workspace_dir.clone();

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
                    cancel_rx,
                    result_tx,
                    session_manager,
                    llm,
                    parent_event_tx,
                    base_agent_config,
                    workspace_dir,
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
    pub fn list_active(&self) -> Vec<(TaskId, SubagentType, TaskStatus)> {
        self.subagents
            .iter()
            .map(|e| (*e.key(), e.value().agent_type, e.value().status))
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
    ) -> AlmsResult<String> {
        let request = SubagentRequest {
            task,
            agent_type: SubagentType::General,
            timeout: Duration::from_secs(SUBAGENT_TTL_SECS),
            capabilities: SubagentType::General.default_capabilities(),
            parent_session: parent_session_id,
            parent_run_id,
            subagent_name,
        };

        let task_id = self.spawn_subagent(request, parent_event_tx).await?;

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
        skip(self, task, parent_event_tx),
        fields(parent_session = %parent_session_id.0)
    )]
    async fn dispatch_background(
        &self,
        task: String,
        parent_session_id: SessionId,
        parent_run_id: Option<RunId>,
        parent_event_tx: Option<RuntimeEventSender>,
        subagent_name: Option<String>,
    ) -> alms_core::AlmsResult<Uuid> {
        let request = SubagentRequest {
            task,
            agent_type: SubagentType::General,
            timeout: Duration::from_secs(SUBAGENT_TTL_SECS),
            capabilities: SubagentType::General.default_capabilities(),
            parent_session: parent_session_id,
            parent_run_id,
            subagent_name,
        };
        let task_id = self.spawn_subagent(request, parent_event_tx).await?;

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
// Internal subagent runner
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn run_subagent(
    task_id: TaskId,
    request: SubagentRequest,
    subagents: Arc<DashMap<TaskId, SubagentHandle>>,
    mut cancel_rx: oneshot::Receiver<()>,
    result_tx: oneshot::Sender<TaskResult>,
    session_manager: Arc<SessionManager>,
    llm: LlmClient,
    parent_event_tx: Option<RuntimeEventSender>,
    base_agent_config: AgentConfig,
    workspace_dir: Option<std::path::PathBuf>,
) {
    let start = std::time::Instant::now();

    if let Some(mut handle) = subagents.get_mut(&task_id) {
        handle.status = TaskStatus::Running;
    }

    info!(
        target: "subagent::started",
        task_id = %task_id.0,
        task = %request.task,
        subagent_type = ?request.agent_type,
        "Subagent execution started"
    );

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
        _ = &mut cancel_rx => {
            info!(
                target: "subagent::cancelled",
                task_id = %task_id.0,
                "Subagent cancelled"
            );
            (TaskStatus::Cancelled, serde_json::json!({"cancelled": true}), None)
        }
        output = run_agent_loop(task_id, &request, &session_manager, &llm, parent_event_tx, &base_agent_config, workspace_dir.as_deref()) => {
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

    let task_result = TaskResult {
        task_id,
        status: new_status,
        result: result_value,
        execution_time_ms: start.elapsed().as_millis() as u64,
        tokens_used,
    };

    // Store result in the handle for background-mode polling, then update status.
    if let Some(mut handle) = subagents.get_mut(&task_id) {
        handle.status = new_status;
        handle.completed_result = Some(task_result.clone());
    }

    // Deliver result to dispatch() caller (foreground mode — may already be dropped)
    let _ = result_tx.send(task_result);

    // Keep the handle long enough for background callers to poll the result.
    tokio::time::sleep(Duration::from_secs(SUBAGENT_TTL_SECS)).await;
    subagents.remove(&task_id);
    debug!("Cleaned up subagent {:?}", task_id);
}

/// Default system prompt for ephemeral (unnamed) subagents.
const DEFAULT_SUBAGENT_PROMPT: &str =
    "You are a general-purpose assistant. Complete the given task thoroughly and accurately.";

/// Build an `AgentConfig` for a subagent. Named subagents get their config
/// from the agent registry; ephemeral subagents use a default prompt.
/// Both inherit sandbox settings from the parent's base config.
fn agent_config_for_subagent(system_prompt: Option<String>, base: &AgentConfig) -> AgentConfig {
    AgentConfig {
        system_prompt: system_prompt.unwrap_or_else(|| DEFAULT_SUBAGENT_PROMPT.to_string()),
        sandbox_root: base.sandbox_root.clone(),
        shell_policy: base.shell_policy.clone(),
        ..AgentConfig::default()
    }
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
async fn run_agent_loop(
    task_id: TaskId,
    request: &SubagentRequest,
    session_manager: &Arc<SessionManager>,
    llm: &LlmClient,
    parent_event_tx: Option<RuntimeEventSender>,
    base_agent_config: &AgentConfig,
    workspace_dir: Option<&std::path::Path>,
) -> AlmsResult<RunOutput> {
    // Derive identity and config based on whether the subagent is named
    let (agent_id, context_id, config, attach_workspace) =
        if let Some(ref name) = request.subagent_name {
            // Named: deterministic identity, look up agent registry for config
            let parent_as_agent = AgentId(request.parent_session.0);
            let stable_id = AgentId::deterministic(parent_as_agent, name);
            let stable_ctx = format!("subagent_{}_{}", request.parent_session.0, name);

            // Look up agent record in registry for system_prompt/model/posture
            let agent_system_prompt = session_manager
                .store()
                .and_then(|store| store.load_agent_by_name(name).ok())
                .flatten()
                .and_then(|record| record.system_prompt);

            let config = agent_config_for_subagent(agent_system_prompt, base_agent_config);
            (stable_id, stable_ctx, config, true)
        } else {
            // Ephemeral: fresh each invocation
            let config = agent_config_for_subagent(None, base_agent_config);
            (
                AgentId::new(),
                format!("subagent_{}", task_id.0),
                config,
                false,
            )
        };

    // Create a per-subagent event channel
    let (sub_tx, sub_rx) = tokio::sync::mpsc::unbounded_channel::<alms_runtime::RuntimeEvent>();

    let mut runtime = AgentRuntime::new(agent_id, config, llm.clone()).with_event_sender(sub_tx);

    // Attach workspace for named subagents: {workspace_dir}/{name}/
    if attach_workspace && let (Some(ws_dir), Some(name)) = (workspace_dir, &request.subagent_name)
    {
        let subagent_ws_dir = ws_dir.join(name);
        let workspace = alms_runtime::AgentWorkspace::new(subagent_ws_dir, agent_id);
        runtime = runtime.with_workspace(workspace);
    }

    // Forward subagent tool events into the parent run's event stream
    if let Some(parent_tx) = parent_event_tx {
        tokio::spawn(async move {
            let mut rx = sub_rx;
            while let Some(event) = rx.recv().await {
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
            .dispatch("Say hello".to_string(), test_session_id(), None, None, None)
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
            agent_type: SubagentType::General,
            timeout: Duration::from_secs(300),
            capabilities: SubagentType::General.default_capabilities(),
            parent_session: session_id,
            parent_run_id: None,
            subagent_name: None,
        };
        let task_id = coord.spawn_subagent(request, None).await.unwrap();

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
            agent_type: SubagentType::General,
            timeout: Duration::from_nanos(1),
            capabilities: SubagentType::General.default_capabilities(),
            parent_session: session_id,
            parent_run_id: None,
            subagent_name: None,
        };
        let task_id = coord.spawn_subagent(request, None).await.unwrap();
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
            agent_type: SubagentType::Research,
            timeout: Duration::from_secs(300),
            capabilities: SubagentType::Research.default_capabilities(),
            parent_session: session_id,
            parent_run_id: None,
            subagent_name: None,
        };
        let task_id = coord.spawn_subagent(request, None).await.unwrap();

        let active = coord.list_active();
        assert!(
            active.iter().any(|(id, _, _)| *id == task_id),
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
            agent_type: SubagentType::General,
            timeout: Duration::from_secs(300),
            capabilities: SubagentType::General.default_capabilities(),
            parent_session: session_id,
            parent_run_id: None,
            subagent_name: None,
        };
        let task_id = coord.spawn_subagent(request, None).await.unwrap();

        let first = coord.take_result_rx(task_id);
        assert!(first.is_some(), "first take should return the receiver");

        let second = coord.take_result_rx(task_id);
        assert!(second.is_none(), "second take should return None");
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

    #[tokio::test]
    async fn test_unnamed_subagent_ephemeral_session() {
        let coord = test_coordinator();
        let parent_session = test_session_id();

        // Two invocations without name — each should get a fresh session
        let _r1 = coord
            .dispatch("Task one".to_string(), parent_session, None, None, None)
            .await
            .expect("first dispatch should succeed");

        let _r2 = coord
            .dispatch("Task two".to_string(), parent_session, None, None, None)
            .await
            .expect("second dispatch should succeed");

        // Each ephemeral invocation creates its own session, so we can't
        // look up a single session with all 4 messages. This test verifies
        // that the calls succeed independently (no shared state).
    }
}
