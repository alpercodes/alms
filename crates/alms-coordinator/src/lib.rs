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
    /// Optional system prompt override for the subagent.
    pub system_prompt: Option<String>,
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
/// Any agent can spawn subagents by calling `dispatch()`. Subagents are
/// ephemeral — they complete their task and return a result to the parent.
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
}

impl Coordinator {
    pub fn new(main_agent: AgentId, session_manager: Arc<SessionManager>, llm: LlmClient) -> Self {
        Self {
            main_agent,
            subagents: Arc::new(DashMap::new()),
            session_manager,
            llm,
        }
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
        system_prompt: Option<String>,
        parent_session_id: SessionId,
        parent_run_id: Option<RunId>,
        parent_event_tx: Option<RuntimeEventSender>,
    ) -> AlmsResult<String> {
        let request = SubagentRequest {
            task,
            agent_type: SubagentType::General,
            timeout: Duration::from_secs(SUBAGENT_TTL_SECS),
            capabilities: SubagentType::General.default_capabilities(),
            parent_session: parent_session_id,
            parent_run_id,
            system_prompt,
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
        skip(self, task, system_prompt, parent_event_tx),
        fields(parent_session = %parent_session_id.0)
    )]
    async fn dispatch_background(
        &self,
        task: String,
        system_prompt: Option<String>,
        parent_session_id: SessionId,
        parent_run_id: Option<RunId>,
        parent_event_tx: Option<RuntimeEventSender>,
    ) -> alms_core::AlmsResult<Uuid> {
        let request = SubagentRequest {
            task,
            agent_type: SubagentType::General,
            timeout: Duration::from_secs(SUBAGENT_TTL_SECS),
            capabilities: SubagentType::General.default_capabilities(),
            parent_session: parent_session_id,
            parent_run_id,
            system_prompt,
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
        output = run_agent_loop(task_id, &request, &session_manager, &llm, parent_event_tx) => {
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

/// Build an `AgentConfig` appropriate for the given subagent type.
fn agent_config_for_type(
    agent_type: SubagentType,
    system_prompt_override: Option<String>,
) -> AgentConfig {
    let default_prompt = match agent_type {
        SubagentType::Research => {
            "You are a research specialist. Gather information, analyse sources, \
             and provide comprehensive, well-structured summaries."
        }
        SubagentType::Code => {
            "You are a code specialist. Generate, review, and debug code with \
             precision, following best practices for the language in use."
        }
        SubagentType::Data => {
            "You are a data analysis specialist. Process, transform, and analyse \
             data to extract actionable insights."
        }
        SubagentType::Integration => {
            "You are an integration specialist. Interact with external APIs and \
             services efficiently and handle errors gracefully."
        }
        SubagentType::Security => {
            "You are a security analysis specialist. Identify vulnerabilities, \
             audit systems, and produce clear security reports."
        }
        SubagentType::General => {
            "You are a general-purpose assistant. Complete the given task \
             thoroughly and accurately."
        }
    };
    AgentConfig {
        system_prompt: system_prompt_override.unwrap_or_else(|| default_prompt.to_string()),
        ..AgentConfig::default()
    }
}

/// Run the actual agent loop for a subagent.
///
/// Creates a fresh `AgentRuntime`, forwards its events to the parent's
/// event channel (if provided), then calls `runtime.run()`.
async fn run_agent_loop(
    task_id: TaskId,
    request: &SubagentRequest,
    session_manager: &Arc<SessionManager>,
    llm: &LlmClient,
    parent_event_tx: Option<RuntimeEventSender>,
) -> AlmsResult<RunOutput> {
    let agent_id = AgentId::new();
    let config = agent_config_for_type(request.agent_type, request.system_prompt.clone());

    // Create a per-subagent event channel
    let (sub_tx, sub_rx) = tokio::sync::mpsc::unbounded_channel::<alms_runtime::RuntimeEvent>();

    let runtime = AgentRuntime::new(agent_id, config, llm.clone()).with_event_sender(sub_tx);

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

    // Each subagent gets its own context so it doesn't share history with the parent
    let context_id = format!("subagent_{}", task_id.0);
    runtime
        .run(session_manager, &context_id, &request.task)
        .await
}
