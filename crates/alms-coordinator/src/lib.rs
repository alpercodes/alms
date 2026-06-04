pub mod message_bus;

use alms_core::{
    AgentId, AlmsError, AlmsResult, Run, RunId, RunRegistrar, SessionId, TokenUsage,
    truncate_to_char_boundary,
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
    /// The parent agent's persistent ID. Named subagent sessions are keyed
    /// on `(parent_agent_id, name)` so the same named subagent resolves to
    /// the same persistent session across every chat session the parent
    /// agent participates in (#1051).
    pub parent_agent_id: AgentId,
    pub parent_run_id: Option<RunId>,
    /// Optional persistent name. When provided, the subagent must be
    /// pre-registered in the agent registry (`alms agent create --name ...`).
    /// Its config and workspace files are loaded from the registry.
    pub subagent_name: Option<String>,
    /// Parent's `invoke_agent` tool invocation id (#1105). When `Some`,
    /// `spawn_subagent` emits a `subagent_started` SSE event onto the
    /// parent's stream carrying this id so the UI's resolver can attach
    /// the new session id to the right SubagentBar entry — including
    /// ephemeral / unnamed subagents where `subagent_name` alone cannot
    /// disambiguate concurrent invocations. `None` for legacy callers
    /// and unit tests; the coordinator skips the emit in that case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_invocation_id: Option<Uuid>,
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
    /// Parent's `invoke_agent` tool invocation id (#1125, A1-2). Mirrors
    /// `SubagentRequest::parent_tool_invocation_id` / the id carried by the
    /// sibling `subagent_started` event so the frontend can resolve the
    /// completion to the right SubagentBar entry by invocation id rather
    /// than the name-only first-match heuristic — which cross-wires when two
    /// unnamed/ephemeral subagents run concurrently. `None` for legacy callers
    /// and unit tests that don't supply it; the SSE field is then omitted.
    pub parent_tool_invocation_id: Option<Uuid>,
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
    /// Receiver for the structured `AlmsError` produced by the subagent
    /// (when it failed) — taken by `dispatch()` so it can return the
    /// typed variant unchanged instead of stringifying-and-rewrapping
    /// `task_result.result["error"]` as `AlmsError::Runtime(...)`.
    /// `None` for completed / cancelled / timed-out runs. Issue #920.
    pub error_rx: Option<oneshot::Receiver<AlmsError>>,
}

/// Coordinator manages subagent lifecycle in a pure hierarchy.
///
/// Any agent can spawn subagents by calling `dispatch()`. Named subagents
/// must be pre-registered in the agent registry (`alms agent create`);
/// ephemeral (unnamed) subagents use default config.
/// There is no peer-to-peer communication between agents.
#[derive(Debug)]
pub struct Coordinator {
    /// Main agent ID — write-only after #1051 removed the
    /// `session_manager.get(parent_session_id)` fallback in `spawn_subagent`.
    /// Slated for removal in #1069 (constructor-API change deferred from
    /// #1068's fix-up scope).
    #[allow(dead_code)]
    main_agent: AgentId,
    /// Active subagents: TaskId -> SubagentHandle
    subagents: Arc<DashMap<TaskId, SubagentHandle>>,
    /// Named subagents currently executing — prevents concurrent invocations
    /// of the same named subagent which would corrupt shared session history.
    ///
    /// Keyed on `(parent_agent_id, name)` to match the new session-key scope
    /// from #1051: agent A's "reviewer" and agent B's "reviewer" resolve to
    /// disjoint sessions, so they must also have disjoint concurrency guards
    /// (otherwise spawning one would lock the other out).
    active_named: Arc<dashmap::DashSet<(AgentId, String)>>,
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
    /// Absolute path to the project root (#945) — the agent's filesystem
    /// sandbox boundary. Subagents inherit it verbatim so a parent and its
    /// subagent (named or ephemeral) share the single-root sandbox model.
    project_root: Option<std::path::PathBuf>,
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
    /// Security config snapshot (#947) — config-file-only, loaded once at
    /// gateway boot. The Coordinator consults
    /// [`SecurityConfig::is_full_os_access_agent`] for each named subagent
    /// to decide whether to inherit the parent's project-root sandbox or
    /// drop it via `with_unrestricted_filesystem`. Inherited subagents
    /// (no name) are never listed by construction, so they always pick up
    /// the project-root sandbox.
    security_config: alms_core::config::SecurityConfig,
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
            project_root: None,
            subagent_prompts: Arc::new(DashMap::new()),
            completion_tx: None,
            secrets: None,
            run_registrar: None,
            security_config: alms_core::config::SecurityConfig::default(),
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
            project_root: None,
            subagent_prompts: Arc::new(DashMap::new()),
            completion_tx: None,
            secrets: None,
            run_registrar: None,
            security_config: alms_core::config::SecurityConfig::default(),
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

    /// Set the project root (#945) so subagents inherit the parent's
    /// filesystem-sandbox boundary. Without this set, subagents fall back
    /// to whatever sandbox root their `AgentConfig` resolves at construction
    /// time — same as the pre-#945 behaviour, useful for unit tests that
    /// don't drive the gateway's full plumbing.
    pub fn with_project_root(mut self, dir: std::path::PathBuf) -> Self {
        self.project_root = Some(dir);
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

    /// Set the security-config snapshot so subagents named in
    /// `[security].allow_full_os_access` (#947) inherit the operator's
    /// "no project-root sandbox" decision the same way HTTP-triggered
    /// runs do. Defaults to an empty list — no agent is listed.
    pub fn with_security_config(
        mut self,
        security_config: alms_core::config::SecurityConfig,
    ) -> Self {
        self.security_config = security_config;
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
        //
        // Keyed on `(parent_agent_id, name)` (#1051): two different parent
        // agents spawning a same-named subagent concurrently land in disjoint
        // sessions, so they must not block each other here.
        if let Some(ref name) = request.subagent_name
            && !self
                .active_named
                .insert((request.parent_agent_id, name.clone()))
        {
            return Err(alms_core::AlmsError::Runtime(format!(
                "Named subagent '{}' is already running for this parent agent — \
                 concurrent invocations of the same named subagent from the same \
                 parent are not supported",
                name
            )));
        }

        let task_id = TaskId::new();
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let (result_tx, result_rx) = oneshot::channel::<TaskResult>();
        // #920: typed error channel, parallel to the JSON `result_rx`.
        // `run_subagent` fires this when the agent loop returns an error
        // so `dispatch()` can propagate the structured `AlmsError`
        // (e.g. `SubagentLlmError`) without going through the JSON
        // stringification round trip that previously double-wrapped the
        // error as `AlmsError::Runtime(stringified)`.
        let (error_tx, error_rx) = oneshot::channel::<AlmsError>();
        let parent_run_id = request.parent_run_id;
        let parent_session_id = request.parent_session;
        // The parent agent ID is the single source of truth for named-subagent
        // session keying (#1051). Callers (Coordinator::dispatch /
        // dispatch_background) populate this from the gateway, which knows the
        // parent agent that started the run.
        let parent_agent_id = request.parent_agent_id;

        // Derive the subagent's identity once, here. This is the single source
        // of truth for `(sub_agent_id, sub_context_id, sub_session_id)` — both
        // the handle (returned to the parent's `invoke_agent` tool result) and
        // the spawned `run_subagent` task use these exact values. Re-deriving
        // inside `run_subagent` would mint a fresh `AgentId::new()` for ephemeral
        // subagents (because `derive_subagent_identity` is non-deterministic in
        // that branch), producing a second `(agent_id, context_id)` key on the
        // session map and creating a duplicate session row — leaving the
        // handle's `sub_session_id` pointing at an empty orphan while the
        // subagent's actual messages land on the second row (#1075).
        let (sub_agent_id, sub_context_id) = derive_subagent_identity(task_id, &request);
        let sub_session_id = self
            .session_manager
            .get_or_create(sub_agent_id, &sub_context_id)
            .id;

        // #1105: emit `subagent_started` onto the parent's event stream
        // the moment we know the subagent's session id, so the UI's
        // SubagentBar can render the "View session" button live during
        // a foreground `invoke_agent` run — instead of only after
        // `tool_end` arrives, which for foreground subagents means
        // *after the subagent has finished*. Ordering invariant
        // (Tim's note on PR #1113):
        //   1. parent's `tool_start (invoke_agent)` -- already queued
        //      onto `runtime_tx` by the agent loop before
        //      `tool.execute()` ran;
        //   2. `subagent_started` -- queued here, AFTER step 1 by
        //      FIFO of `runtime_tx`;
        //   3. nested `tool_start` events from inside the subagent --
        //      can only fire once the spawned subagent task below
        //      begins its agent loop, which happens after this point.
        // Fires for both foreground and background paths (Atlas's
        // acceptance criteria) — `parent_event_tx` is the parent's
        // runtime channel for foreground and the background event
        // forwarder for background. The background path preserves the
        // ordering invariant above because the gateway's bg event
        // forwarder task re-routes `SubagentStarted` back onto the
        // parent's `runtime_tx` rather than emitting SSE on the bg
        // channel directly, so both paths share the same FIFO line
        // with the parent's `tool_start (invoke_agent)`. Skipped when
        // `parent_tool_invocation_id` is `None` because the frontend
        // resolver needs the id (or `subagent_name`) to attach the
        // session id to the right SubagentBar entry; legacy code
        // paths and tests that don't supply it just fall through.
        if let (Some(tx), Some(parent_inv_id)) =
            (parent_event_tx.as_ref(), request.parent_tool_invocation_id)
        {
            tx.forward_subagent_started(
                parent_inv_id,
                request.subagent_name.clone(),
                sub_session_id.0,
                is_background,
            );
        }

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
            error_rx: Some(error_rx),
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
        let project_root = self.project_root.clone();
        let subagent_prompts = self.subagent_prompts.clone();
        let completion_tx = self.completion_tx.clone();
        let secrets = self.secrets.clone();
        let run_registrar = self.run_registrar.clone();
        let security_config = self.security_config.clone();

        // Move identity values into the spawned task — `run_subagent` and
        // `run_agent_loop` use these instead of re-deriving so the entire
        // subagent path operates on a single `(agent_id, context_id)` key
        // and therefore a single session row (#1075).
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
                    sub_agent_id,
                    sub_context_id,
                    sub_session_id,
                    subagents,
                    active_named,
                    cancel_rx,
                    result_tx,
                    error_tx,
                    session_manager,
                    llm,
                    parent_event_tx,
                    base_agent_config,
                    workspace_dir,
                    data_dir,
                    project_root,
                    subagent_prompts,
                    completion_tx,
                    parent_cancel_token,
                    secrets,
                    run_registrar,
                    security_config,
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

    /// Take the typed-error receiver for a task (can only be called once
    /// per task). Companion to [`Self::take_result_rx`] used by
    /// [`SubagentDispatcher::dispatch`] to recover the structured
    /// `AlmsError` produced by the subagent without going through
    /// `task_result.result["error"]` and an `AlmsError::Runtime` rewrap.
    /// Issue #920.
    pub fn take_error_rx(&self, task_id: TaskId) -> Option<oneshot::Receiver<AlmsError>> {
        self.subagents.get_mut(&task_id)?.error_rx.take()
    }
}

#[async_trait]
impl SubagentDispatcher for Coordinator {
    async fn dispatch(
        &self,
        task: String,
        parent_session_id: SessionId,
        parent_agent_id: AgentId,
        parent_run_id: Option<RunId>,
        parent_event_tx: Option<Arc<dyn EventForwarder>>,
        subagent_name: Option<String>,
        parent_cancel_token: Option<CancellationToken>,
        parent_tool_invocation_id: Option<Uuid>,
    ) -> AlmsResult<(String, SessionId)> {
        let request = SubagentRequest {
            task,
            timeout: Duration::from_secs(SUBAGENT_TTL_SECS),
            parent_session: parent_session_id,
            parent_agent_id,
            parent_run_id,
            subagent_name,
            parent_tool_invocation_id,
        };

        let (task_id, sub_session_id) = self
            .spawn_subagent(request, parent_event_tx, false, parent_cancel_token)
            .await?;

        // Take the result receiver — must happen immediately after spawn_subagent
        // since the handle is already in the DashMap.
        let result_rx = self.take_result_rx(task_id).ok_or_else(|| {
            alms_core::AlmsError::Runtime("No result channel for subagent".to_string())
        })?;
        // #920: typed-error receiver. Carries the structured `AlmsError`
        // when the subagent's loop returned an error, so we can return
        // it verbatim instead of rebuilding an `AlmsError::Runtime` from
        // the JSON `result["error"]` string (which previously produced
        // the `Runtime error: Runtime error:` double prefix at the
        // parent's tool-result message).
        let error_rx = self.take_error_rx(task_id);

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
            TaskStatus::Failed => {
                // Prefer the typed `AlmsError` when present so the
                // structured variant (e.g. `SubagentLlmError`) survives
                // the boundary unchanged. Fall back to the JSON string
                // for paths that never produced a structured error
                // (timeout, missing typed channel) — those still come
                // through as `AlmsError::Runtime` exactly as before.
                if let Some(error_rx) = error_rx
                    && let Ok(typed) = error_rx.await
                {
                    return Err(typed);
                }
                Err(alms_core::AlmsError::Runtime(
                    task_result.result["error"]
                        .as_str()
                        .unwrap_or("subagent failed")
                        .to_string(),
                ))
            }
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
        parent_agent_id: AgentId,
        parent_run_id: Option<RunId>,
        parent_event_tx: Option<Arc<dyn EventForwarder>>,
        subagent_name: Option<String>,
        parent_cancel_token: Option<CancellationToken>,
        parent_tool_invocation_id: Option<Uuid>,
    ) -> alms_core::AlmsResult<(Uuid, SessionId)> {
        let request = SubagentRequest {
            task,
            timeout: Duration::from_secs(SUBAGENT_TTL_SECS),
            parent_session: parent_session_id,
            parent_agent_id,
            parent_run_id,
            subagent_name,
            parent_tool_invocation_id,
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
///
/// Keyed on `(parent_agent_id, name)` to match the per-parent-agent scope of
/// `active_named` (#1051).
struct NamedSubagentGuard {
    key: Option<(AgentId, String)>,
    active_named: Arc<dashmap::DashSet<(AgentId, String)>>,
}

impl Drop for NamedSubagentGuard {
    fn drop(&mut self) {
        if let Some(ref key) = self.key {
            self.active_named.remove(key);
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
    // Identity is resolved once by `spawn_subagent` and threaded through here
    // so the entire subagent lifecycle (handle creation, run registration,
    // `runtime.run` lookup) shares a single `(agent_id, context_id)` key and
    // therefore a single session row (#1075). Re-deriving here would mint a
    // fresh `AgentId::new()` for ephemeral subagents.
    sub_agent_id: AgentId,
    sub_context_id: String,
    sub_session_id: SessionId,
    subagents: Arc<DashMap<TaskId, SubagentHandle>>,
    active_named: Arc<dashmap::DashSet<(AgentId, String)>>,
    cancel_rx: oneshot::Receiver<()>,
    result_tx: oneshot::Sender<TaskResult>,
    // #920: typed-error sender, parallel to `result_tx`. Fired with the
    // structured `AlmsError` from `run_agent_loop` when the subagent
    // failed; never fired on success / cancel / timeout. `dispatch()`
    // awaits this in preference to the JSON `result["error"]` so the
    // typed variant (e.g. `SubagentLlmError`) propagates without a
    // stringification round trip.
    error_tx: oneshot::Sender<AlmsError>,
    session_manager: Arc<SessionManager>,
    llm: LlmClient,
    parent_event_tx: Option<Arc<dyn EventForwarder>>,
    base_agent_config: AgentConfig,
    workspace_dir: Option<std::path::PathBuf>,
    data_dir: Option<std::path::PathBuf>,
    project_root: Option<std::path::PathBuf>,
    subagent_prompts: Arc<DashMap<String, String>>,
    completion_tx: Option<mpsc::UnboundedSender<SubagentCompletion>>,
    parent_cancel_token: Option<CancellationToken>,
    secrets: Option<Arc<parking_lot::RwLock<alms_core::secrets::SecretsStore>>>,
    run_registrar: Option<Arc<dyn RunRegistrar>>,
    security_config: alms_core::config::SecurityConfig,
    is_background: bool,
) {
    // RAII guard: removes the (parent_agent_id, name) key from active_named on
    // drop (including panics). Keyed by parent agent so two different parents
    // spawning a same-named subagent don't clobber each other's guard slot.
    let _named_guard = NamedSubagentGuard {
        key: request
            .subagent_name
            .clone()
            .map(|n| (request.parent_agent_id, n)),
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

    // Identity and session are already resolved by `spawn_subagent` and
    // passed in as parameters (#1075). Re-deriving here would mint a fresh
    // `AgentId::new()` for ephemeral subagents and create a duplicate row.

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
    //
    // #920: also returns the structured `AlmsError` (when the agent loop
    // produced one) so the caller can forward it down the typed-error
    // oneshot. Timeouts and cancellations have no underlying `AlmsError`
    // and use `None` here — `dispatch()` falls back to the JSON path
    // for those, exactly as before.
    let (new_status, result_value, tokens_used, run_output, typed_error) = tokio::select! {
        _ = tokio::time::sleep(request.timeout) => {
            warn!(
                target: "subagent::timeout",
                task_id = %task_id.0,
                timeout_secs = %request.timeout.as_secs(),
                "Subagent timed out"
            );
            (TaskStatus::Failed, serde_json::json!({"error": "Timeout"}), None, None, None)
        }
        _ = child_cancel_token.cancelled() => {
            info!(
                target: "subagent::cancelled",
                task_id = %task_id.0,
                "Subagent cancelled"
            );
            (TaskStatus::Cancelled, serde_json::json!({"cancelled": true}), None, None, None)
        }
        output = run_agent_loop(task_id, &request, sub_agent_id, &sub_context_id, &session_manager, &llm, parent_event_tx, &base_agent_config, workspace_dir.as_deref(), data_dir.as_deref(), project_root.as_deref(), &subagent_prompts, child_cancel_token.clone(), secrets.as_ref(), &security_config, is_background) => {
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
                        None,
                    )
                }
                Err(e) => {
                    tracing::error!(
                        target: "subagent::error",
                        task_id = %task_id.0,
                        error = %e,
                        "Subagent run failed"
                    );
                    let err_text = e.to_string();
                    (
                        TaskStatus::Failed,
                        serde_json::json!({"error": err_text}),
                        None,
                        None,
                        Some(e),
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
                    // Intentional discard: the coordinator persists the
                    // updated run via `registrar.update_run(run)` below
                    // and does not broadcast an SSE event, so the #1046
                    // duplicate-broadcast guard does not apply here.
                    let _ = run.mark_completed(
                        output.response.clone(),
                        alms_core::TokenUsage {
                            prompt_tokens: output.usage.prompt_tokens,
                            completion_tokens: output.usage.completion_tokens,
                            reasoning_tokens: output.usage.reasoning_tokens,
                            cache_creation_input_tokens: output.usage.cache_creation_input_tokens,
                            cache_read_input_tokens: output.usage.cache_read_input_tokens,
                        },
                    );
                }
            }
            TaskStatus::Failed => {
                let error = result_value["error"]
                    .as_str()
                    .unwrap_or("unknown error")
                    .to_string();
                // Intentional discard: same coordinator-persistence-only
                // path as the `Completed` branch above.
                let _ = run.mark_failed(error);
            }
            TaskStatus::Cancelled => {
                // Intentional discard: same coordinator-persistence-only
                // path as the `Completed` / `Failed` branches above.
                let _ = run.mark_cancelled();
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
                    cache_creation_input_tokens: output.usage.cache_creation_input_tokens,
                    cache_read_input_tokens: output.usage.cache_read_input_tokens,
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
            parent_tool_invocation_id: request.parent_tool_invocation_id,
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

    // #920: Send the typed error first so `dispatch()` (which awaits
    // `result_rx` and then `error_rx` in sequence) is guaranteed to see
    // the typed value the moment it unblocks on `result_rx`. Foreground
    // and background paths both wire this — `let _ = ...` swallows
    // closed-receiver errors when nobody's listening (background mode,
    // or dispatch dropped before completion).
    //
    // Why the ordering matters: tokio oneshot sends are synchronous and
    // visible to the receiver immediately on completion of `send()`, so
    // sending `error_tx` *before* `result_tx` guarantees that when
    // `dispatch()` unblocks on `result_rx.await` the typed error is
    // already sitting in `error_rx`. There's no possible race where
    // `error_rx.await` would see `Closed` while a typed error is still
    // in flight; the only way it returns `Closed` is the genuine
    // "no typed error was produced" path (timeout, cancel, success).
    if let Some(typed) = typed_error {
        let _ = error_tx.send(typed);
    }

    // Deliver result to dispatch() caller (foreground mode — may already be dropped)
    let _ = result_tx.send(task_result);

    // Keep the handle long enough for background callers to poll the result.
    tokio::time::sleep(Duration::from_secs(SUBAGENT_TTL_SECS)).await;
    subagents.remove(&task_id);
    // Clean up cached prompt to prevent unbounded memory growth.
    //
    // The key shape MUST match `derive_subagent_identity` (#1051) — keyed on
    // `(parent_agent_id, name)`, not `(parent_session, name)`. Using
    // `sub_context_id` here guarantees insert/remove parity so the cache
    // shrinks back to its prior state and we never leak entries.
    if request.subagent_name.is_some() {
        subagent_prompts.remove(&sub_context_id);
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
    /// Mirrors the top-level agent path in `resolve_agent_config`
    /// (gateway `runs/mod.rs`): `Some(n)` (including `Some(0)`) is
    /// treated as an explicit override; `None` inherits the parent's
    /// effective budget. This keeps the two-layer precedence (per-agent
    /// > server default) intact for named subagents too.
    thinking_budget_tokens: Option<u32>,
    /// Per-named-subagent OpenAI-compat reasoning-effort override (#768).
    /// Mirrors `thinking_budget_tokens` for the OpenAI reasoning path:
    /// `Some(effort)` wins over the parent; `None` inherits the parent's
    /// effective effort.
    reasoning_effort: Option<alms_core::config::ReasoningEffort>,
    /// Per-named-subagent Gemini extended-thinking budget override (#794).
    /// Same shape as `thinking_budget_tokens` and `reasoning_effort`:
    /// `Some(n)` (including `Some(0)`) wins over the parent's effective
    /// budget; `None` inherits it. Keeps the two-layer precedence
    /// (per-agent > server default) intact for named subagents on the
    /// Gemini path.
    gemini_thinking_budget: Option<u32>,
    /// Per-named-subagent summary provider override (#872). When the
    /// subagent has its own `summary_provider`/`summary_model` pair set
    /// on the registry record, those values override the parent's
    /// effective summary config for this subagent's run. `None` (on
    /// either field) inherits the parent's effective summary config —
    /// which is itself the result of (parent's per-agent ?? server-
    /// level). The pair-only invariant guarantees these arrive together
    /// so the two-field overlay below is symmetric.
    summary_provider: Option<String>,
    /// Per-named-subagent summary model override (#872). See
    /// [`SubagentRecordConfig::summary_provider`] for semantics.
    summary_model: Option<String>,
}

/// Build an `AgentConfig` for a subagent. Named subagents get their config
/// from the agent registry; ephemeral subagents use a default prompt.
/// Both inherit sandbox, tool, and runtime settings (max_tokens,
/// context_config) from the parent's base config.
fn agent_config_for_subagent(
    record: Option<SubagentRecordConfig>,
    base: &AgentConfig,
) -> (AgentConfig, Option<String>, Option<String>) {
    let (
        model,
        posture_str,
        provider,
        thinking_budget_override,
        reasoning_effort_override,
        gemini_thinking_budget_override,
        summary_provider_override,
        summary_model_override,
    ) = match record {
        Some(r) => (
            r.model,
            r.posture,
            r.provider,
            r.thinking_budget_tokens,
            r.reasoning_effort,
            r.gemini_thinking_budget,
            r.summary_provider,
            r.summary_model,
        ),
        None => (None, None, None, None, None, None, None, None),
    };

    let posture = posture_str
        .as_deref()
        .and_then(|s| s.parse::<alms_runtime::Posture>().ok())
        .unwrap_or(alms_runtime::Posture::FullControl);

    // Per-named-subagent Anthropic thinking budget override. `Some(0)` is a
    // legitimate override meaning "disable extended thinking for this
    // subagent even when the parent enables it", matching the gateway's
    // top-level `resolve_agent_config` semantics. `None` inherits the
    // parent's effective budget so unconfigured subagents stay consistent
    // with their parent's extended-thinking policy.
    let anthropic_thinking_budget =
        thinking_budget_override.unwrap_or(base.anthropic_thinking_budget);

    // Per-named-subagent OpenAI reasoning-effort override (#768). Same
    // shape as the thinking budget: `Some(effort)` overrides the parent;
    // `None` inherits the parent's effective value.
    let openai_reasoning_effort = reasoning_effort_override.or(base.openai_reasoning_effort);

    // Per-named-subagent Gemini thinking budget override (#794). Same
    // shape as the Anthropic path: `Some(n)` (including `Some(0)`) is an
    // explicit override; `None` inherits the parent's effective budget.
    // Using `.or()` here rather than `.unwrap_or(base.gemini_thinking_budget)`
    // because `base.gemini_thinking_budget` is itself `Option<u32>`, and
    // we want the parent's `None` state (= "inherit server default") to
    // propagate verbatim when the subagent record has no override.
    let gemini_thinking_budget = gemini_thinking_budget_override.or(base.gemini_thinking_budget);

    // Inherit the parent's effective context config (which already has
    // per-agent summary overrides applied via `resolve_agent_config`),
    // then overlay any summary_provider/summary_model the subagent's
    // own registry record carries. The pair-only validator on
    // `POST /agents` / `PATCH /agents/{id}` guarantees these arrive
    // symmetric, so we honour each field independently — `Some` wins
    // over the parent; `None` inherits.
    let mut subagent_context_config = base.context_config.clone();
    if let Some(provider) = summary_provider_override {
        subagent_context_config.summary_provider = Some(provider);
    }
    if let Some(model) = summary_model_override {
        subagent_context_config.summary_model = Some(model);
    }

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
        // In-loop tool-output truncation (issue #851) is also inherited
        // verbatim — same per-subagent directory layout, same retention
        // sweep — so a subagent's tool calls cannot blow the context
        // window any more than its parent's can.
        tool_output_truncate: base.tool_output_truncate.clone(),
        enabled_tools: base.enabled_tools.clone(),
        fs_edit_fuzzy_match: base.fs_edit_fuzzy_match,
        max_tokens: base.max_tokens,
        context_config: subagent_context_config,
        prompts: base.prompts.clone(),
        debug_mode: false,
        anthropic_thinking_budget,
        // Prompt caching (#766) — server-level only, inherited verbatim
        // by subagents so they benefit from the same cached prefix.
        anthropic_prompt_cache_enabled: base.anthropic_prompt_cache_enabled,
        openai_reasoning_effort,
        // Gemini thinking (#794) — two-layer precedence: per-named-
        // subagent > parent's effective budget (which is itself
        // per-agent > server default). Resolved above via
        // `gemini_thinking_budget_override.or(base.gemini_thinking_budget)`.
        gemini_thinking_budget,
        // Gemini caching (#769) — server-level only, inherited verbatim
        // by subagents so they share cache entries where possible.
        gemini_cache_enabled: base.gemini_cache_enabled,
        gemini_cache_ttl_seconds: base.gemini_cache_ttl_seconds,
    };
    (config, model, provider)
}

/// Derive the subagent's identity (agent_id, context_id) without building
/// the full config.  Called by `run_subagent` *before* `tokio::select!` so
/// that the run can be registered early and updated after timeout/cancel.
///
/// Named subagent sessions are keyed on `(parent_agent_id, name)` — not
/// `(parent_session, name)` — so the same named subagent resolves to the
/// same persistent session no matter which of the parent agent's chat
/// sessions invoked it. See #1051 for the design decision.
fn derive_subagent_identity(task_id: TaskId, request: &SubagentRequest) -> (AgentId, String) {
    if let Some(ref name) = request.subagent_name {
        let stable_id = AgentId::deterministic(request.parent_agent_id, name);
        let stable_ctx = format!("subagent_{}_{}", request.parent_agent_id.0, name);
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
    project_root: Option<&std::path::Path>,
    subagent_prompts: &DashMap<String, String>,
    cancel_token: CancellationToken,
    secrets: Option<&Arc<parking_lot::RwLock<alms_core::secrets::SecretsStore>>>,
    security_config: &alms_core::config::SecurityConfig,
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
                    gemini_thinking_budget: record.gemini_thinking_budget,
                    summary_provider: record.summary_provider,
                    summary_model: record.summary_model,
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
    // Snapshot the in-loop tool-output truncation policy too (issue #851)
    // so subagents inherit the same per-tool cap their parent has.
    let subagent_trunc_cfg = config.tool_output_truncate.clone();

    // #871: snapshot the subagent's `summary_provider` / `summary_model`
    // before `config` is moved into the runtime so we can build a dedicated
    // summary client and wire it via `with_summary_llm`. Subagents inherit
    // the parent's `[context].summary_provider` through `base_agent_config`
    // (cloned at spawn time via `self.base_agent_config.read().clone()`),
    // but pre-#871 the field was silently ignored at the runtime level —
    // the subagent loop fell back to its own `self.llm` for summarization,
    // defeating the parent's "summary on a different provider" intent
    // (Tim's review on PR #871, item 5).
    let subagent_summary_provider = config.context_config.summary_provider.clone();
    let subagent_summary_model = config.context_config.summary_model.clone();

    // Build the dedicated summary client BEFORE `subagent_llm` is moved into
    // `AgentRuntime::new`. Delegates to the shared `alms_runtime::build_summary_client`
    // helper so the #866 + #871 leak-guard rules cannot drift between the
    // gateway run path and this subagent inheritance path. When
    // `summary_provider` is None the helper returns a clone we discard; we
    // only call `with_summary_llm` when the user opted in.
    let summary_llm_for_subagent: Option<alms_runtime::LlmClient> =
        subagent_summary_provider.as_deref().map(|provider| {
            let secrets_guard = secrets.as_ref().map(|s| s.read());
            let summary_client = alms_runtime::build_summary_client(
                &subagent_llm,
                Some(provider),
                subagent_summary_model.as_deref(),
                secrets_guard.as_deref(),
            );
            info!(
                task_id = %task_id.0,
                agent_provider = %subagent_llm.provider(),
                summary_provider = %provider,
                summary_model = %subagent_summary_model.as_deref().unwrap_or("<inherit>"),
                "Subagent inheriting parent summary_provider config (#871)"
            );
            summary_client
        });

    let mut runtime = AgentRuntime::new(agent_id, config, subagent_llm)?
        .with_event_sender(sub_tx)
        .with_cancel_token(cancel_token);

    if let Some(summary_client) = summary_llm_for_subagent {
        runtime = runtime.with_summary_llm(summary_client);
    }

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

    // Inherit the parent's in-loop tool-output truncation policy (issue
    // #851). Same per-subagent layout
    // (`{data_dir}/tool-output/sub-{task_id}/`) so the parent's
    // gateway-startup retention sweep collects subagent spills the same
    // way it collects parent spills.
    if let Some(dir) = data_dir {
        let sub_run_dir = dir
            .join(alms_runtime::tool_output_truncate::TOOL_OUTPUT_DIR_NAME)
            .join(format!("sub-{}", task_id.0));
        runtime = runtime.with_tool_output_truncate(
            sub_run_dir,
            subagent_trunc_cfg.enabled,
            subagent_trunc_cfg.max_bytes,
            subagent_trunc_cfg.max_lines,
        );
    }

    // Pin the subagent's filesystem-sandbox boundary at the project root
    // (#945). Subagents share the parent's single-root model — they
    // operate on the same project directory their parent does. This is
    // the expected v2 behaviour and matches how Claude Code's "isolation:
    // worktree" subagents are explicitly opt-in (the worktree case is
    // out-of-scope for this issue and lands in #946).
    //
    // When `project_root` is None (unit-test paths that don't drive the
    // gateway) we fall back to whatever sandbox the subagent's
    // `AgentConfig` resolved at construction time — same as the pre-#945
    // behaviour for those paths. Must come AFTER the spill builders so
    // the accumulated `extra_fs_read_roots` are reflected.
    //
    // Operator escape hatch (#947): named subagents whose name appears
    // on `[security].allow_full_os_access` opt out of the project-root
    // sandbox the same way HTTP-triggered runs do. Ephemeral subagents
    // (no `subagent_name`) cannot be on the list by construction, so
    // they always pick up the project-root pin. A run-start `WARN`
    // fires for parity with the HTTP / Telegram paths.
    let sub_full_os_access = request
        .subagent_name
        .as_deref()
        .map(|n| security_config.is_full_os_access_agent(n))
        .unwrap_or(false);
    if sub_full_os_access {
        let name = request.subagent_name.as_deref().unwrap_or("");
        warn!(
            target: "alms.security",
            agent_name = %name,
            task_id = %task_id.0,
            allow_full_os_access = true,
            "Subagent run starting for agent '{}' WITHOUT project-root filesystem \
             sandbox (allow_full_os_access). shell_permissions and the \
             destructive-command classifier still apply.",
            name,
        );
        runtime = runtime.with_unrestricted_filesystem();
    } else if let Some(root) = project_root {
        runtime = runtime.with_project_root(root.to_path_buf());
    }

    // Attach workspace to register `workspace_write` and ensure the
    // metadata directory exists.
    //
    // Named subagents:    {workspace_dir}/{name}/   (now `<project>/.alms/agents/<name>/`)
    // Ephemeral subagents: {workspace_dir}/.ephemeral/{task_id}/
    //
    // After #945, `with_workspace` no longer changes the sandbox root —
    // the project-root pin above already did. Ephemeral subagents
    // therefore share the project-root sandbox; their `.ephemeral/`
    // directory only exists so the workspace_write tool has a stable
    // metadata path that the parent can clean up after the subagent
    // terminates.
    if attach_workspace && let Some(ws_dir) = workspace_dir {
        let subagent_ws_dir = if let Some(name) = &request.subagent_name {
            ws_dir.join(name)
        } else {
            ws_dir.join(".ephemeral").join(task_id.0.to_string())
        };
        let workspace = alms_runtime::AgentWorkspace::with_dir(subagent_ws_dir);
        runtime = runtime.with_workspace(workspace);
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
                    // SubagentStarted events from inside a subagent are
                    // suppressed — a (future) sub-subagent's SubagentBar
                    // entry belongs on the subagent's own session stream,
                    // not the top-level parent's. The coordinator already
                    // emits a SubagentStarted onto the original parent's
                    // stream from `spawn_subagent` for this subagent at
                    // the moment its session is created (#1105); that
                    // event is what the UI SubagentBar resolver
                    // consumes. Recursive subagent spawning is not yet
                    // wired up (see `docs/autonomous-subagents-design.md`),
                    // so in practice this arm is dead code today, but
                    // the explicit suppression makes the match exhaustive
                    // and documents the intended behaviour for when
                    // sub-subagents land.
                    RuntimeEvent::SubagentStarted { .. } => continue,
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

    fn test_parent_agent_id() -> AgentId {
        AgentId::new()
    }

    // -- (a) dispatch foreground — success path returns response text -----------

    #[tokio::test]
    async fn test_dispatch_foreground_success() {
        let coord = test_coordinator();
        let result = coord
            .dispatch(
                "Say hello".to_string(),
                test_session_id(),
                test_parent_agent_id(),
                None,
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
                test_parent_agent_id(),
                None,
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

    // -- (c) #1105 -- spawn_subagent emits SubagentStarted onto parent's stream

    /// One captured `forward_subagent_started` call:
    /// `(tool_invocation_id, subagent_name, subagent_session_id, background)`.
    /// The `background` flag (#1125, A1-1) is `false` for foreground
    /// (`dispatch`) and `true` for background (`dispatch_background`).
    type CapturedStart = (uuid::Uuid, Option<String>, uuid::Uuid, bool);

    /// A mock EventForwarder that records every `forward_subagent_started`
    /// call. The default impl is a no-op, so we explicitly override it here
    /// to capture the args and assert on them.
    #[derive(Debug, Default)]
    struct CapturingEventForwarder {
        started: parking_lot::Mutex<Vec<CapturedStart>>,
    }

    impl alms_tools::EventForwarder for CapturingEventForwarder {
        fn forward_tool_start(
            &self,
            _invocation_id: uuid::Uuid,
            _tool: String,
            _params: serde_json::Value,
            _source_agent: Option<String>,
            _task_id: Option<String>,
        ) {
        }
        fn forward_tool_end(
            &self,
            _invocation_id: uuid::Uuid,
            _ok: bool,
            _result: serde_json::Value,
            _source_agent: Option<String>,
            _task_id: Option<String>,
        ) {
        }
        fn forward_token_delta(&self, _delta: String, _source_agent: Option<String>) {}
        fn forward_status(&self, _phase: String, _detail: Option<String>) {}
        fn forward_warning(&self, _code: String, _message: String, _source_agent: Option<String>) {}
        fn forward_subagent_started(
            &self,
            tool_invocation_id: uuid::Uuid,
            subagent_name: Option<String>,
            subagent_session_id: uuid::Uuid,
            background: bool,
        ) {
            self.started.lock().push((
                tool_invocation_id,
                subagent_name,
                subagent_session_id,
                background,
            ));
        }
    }

    /// #1105 — `spawn_subagent` must emit `forward_subagent_started` on the
    /// parent's event forwarder the moment the subagent's session row is
    /// created, carrying the parent's `tool_invocation_id` and the new
    /// session id. This is what the gateway turns into the
    /// `subagent_started` SSE event so the UI's SubagentBar can render the
    /// "View session" button live during a foreground `invoke_agent` run.
    #[tokio::test]
    async fn test_spawn_emits_subagent_started_when_invocation_id_set() {
        let coord = test_coordinator();
        let capture = Arc::new(CapturingEventForwarder::default());
        let fwd: Arc<dyn alms_tools::EventForwarder> = capture.clone();
        let parent_inv_id = uuid::Uuid::new_v4();

        let request = SubagentRequest {
            task: "test".to_string(),
            timeout: Duration::from_secs(300),
            parent_session: test_session_id(),
            parent_agent_id: test_parent_agent_id(),
            parent_run_id: None,
            subagent_name: Some("reviewer".to_string()),
            parent_tool_invocation_id: Some(parent_inv_id),
        };
        let (_task_id, sub_session_id) = coord
            .spawn_subagent(request, Some(fwd), false, None)
            .await
            .expect("spawn_subagent should succeed");

        // The event must be queued synchronously from inside `spawn_subagent`
        // — no async polling needed. Lock the capture, drain, and assert.
        let started = capture.started.lock();
        assert_eq!(
            started.len(),
            1,
            "spawn_subagent must emit exactly one SubagentStarted event"
        );
        let (got_inv_id, got_name, got_session_id, got_background) = started[0].clone();
        assert_eq!(
            got_inv_id, parent_inv_id,
            "SubagentStarted must carry the parent's invocation_id verbatim"
        );
        assert_eq!(
            got_name.as_deref(),
            Some("reviewer"),
            "SubagentStarted must carry the registered subagent name"
        );
        assert_eq!(
            got_session_id, sub_session_id.0,
            "SubagentStarted's session_id must match the value returned by \
             spawn_subagent (the row where the subagent persists messages)"
        );
        assert!(
            !got_background,
            "foreground spawn_subagent must emit background=false so the \
             gateway persists the #1125 (A1-1) subagent_started marker"
        );
    }

    /// Ephemeral (unnamed) subagents: `subagent_name` arrives as `None` on
    /// the wire — the frontend resolver falls back to
    /// `findSubagentByToolInvocationId`. The invocation id is the
    /// disambiguator for concurrent unnamed runs, so it must still flow
    /// through.
    #[tokio::test]
    async fn test_spawn_emits_subagent_started_for_ephemeral_subagent() {
        let coord = test_coordinator();
        let capture = Arc::new(CapturingEventForwarder::default());
        let fwd: Arc<dyn alms_tools::EventForwarder> = capture.clone();
        let parent_inv_id = uuid::Uuid::new_v4();

        let request = SubagentRequest {
            task: "ephemeral".to_string(),
            timeout: Duration::from_secs(300),
            parent_session: test_session_id(),
            parent_agent_id: test_parent_agent_id(),
            parent_run_id: None,
            subagent_name: None, // unnamed / ephemeral
            parent_tool_invocation_id: Some(parent_inv_id),
        };
        let (_task_id, sub_session_id) = coord
            .spawn_subagent(request, Some(fwd), false, None)
            .await
            .expect("spawn_subagent should succeed");

        let started = capture.started.lock();
        assert_eq!(started.len(), 1);
        let (got_inv_id, got_name, got_session_id, got_background) = started[0].clone();
        assert_eq!(got_inv_id, parent_inv_id);
        assert!(
            got_name.is_none(),
            "unnamed subagent must carry subagent_name = None so the frontend \
             resolves via tool_invocation_id"
        );
        assert_eq!(got_session_id, sub_session_id.0);
        assert!(
            !got_background,
            "foreground path must emit background=false"
        );
    }

    /// #1125 (A1-1) — the background (`dispatch_background`) path must emit
    /// `forward_subagent_started` with `background = true` so the gateway's
    /// `forward_runtime_events` arm SKIPS the durable `subagent_started`
    /// marker for background subagents (which are already reload-safe via
    /// their persisted `{task_id, session_id}` tool result). This is the
    /// coordinator-side guarantee that the foreground-only marker gating in
    /// the gateway has a correct `background` discriminator to key on.
    #[tokio::test]
    async fn test_spawn_emits_subagent_started_background_true_for_bg_path() {
        let coord = test_coordinator();
        let capture = Arc::new(CapturingEventForwarder::default());
        let fwd: Arc<dyn alms_tools::EventForwarder> = capture.clone();
        let parent_inv_id = uuid::Uuid::new_v4();

        let request = SubagentRequest {
            task: "bg".to_string(),
            timeout: Duration::from_secs(300),
            parent_session: test_session_id(),
            parent_agent_id: test_parent_agent_id(),
            parent_run_id: None,
            subagent_name: Some("worker".to_string()),
            parent_tool_invocation_id: Some(parent_inv_id),
        };
        // is_background = true — the only difference from the foreground tests.
        let (_task_id, _sub_session_id) = coord
            .spawn_subagent(request, Some(fwd), true, None)
            .await
            .expect("spawn_subagent should succeed");

        let started = capture.started.lock();
        assert_eq!(started.len(), 1);
        let (_got_inv_id, _got_name, _got_session_id, got_background) = started[0].clone();
        assert!(
            got_background,
            "background spawn_subagent must emit background=true so the \
             gateway suppresses the #1125 (A1-1) subagent_started marker"
        );
    }

    /// Legacy callers / tests that don't supply `parent_tool_invocation_id`
    /// must NOT see a `SubagentStarted` event — the frontend resolver
    /// needs either `subagent_name` or `tool_invocation_id` to attach the
    /// session id to a SubagentBar entry, and emitting without either
    /// would warn-and-no-op (per Iris's resolver tightening in #1113).
    /// Skipping the emit keeps the wire shape clean.
    #[tokio::test]
    async fn test_spawn_skips_subagent_started_when_invocation_id_absent() {
        let coord = test_coordinator();
        let capture = Arc::new(CapturingEventForwarder::default());
        let fwd: Arc<dyn alms_tools::EventForwarder> = capture.clone();

        let request = SubagentRequest {
            task: "legacy".to_string(),
            timeout: Duration::from_secs(300),
            parent_session: test_session_id(),
            parent_agent_id: test_parent_agent_id(),
            parent_run_id: None,
            subagent_name: Some("reviewer".to_string()),
            parent_tool_invocation_id: None,
        };
        let _ = coord
            .spawn_subagent(request, Some(fwd), false, None)
            .await
            .expect("spawn_subagent should succeed");

        assert!(
            capture.started.lock().is_empty(),
            "no SubagentStarted should fire when parent_tool_invocation_id is None"
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
            parent_agent_id: test_parent_agent_id(),
            parent_run_id: None,
            subagent_name: None,
            parent_tool_invocation_id: None,
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
            parent_agent_id: test_parent_agent_id(),
            parent_run_id: None,
            subagent_name: None,
            parent_tool_invocation_id: None,
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
            parent_agent_id: test_parent_agent_id(),
            parent_run_id: None,
            subagent_name: None,
            parent_tool_invocation_id: None,
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
            parent_agent_id: test_parent_agent_id(),
            parent_run_id: None,
            subagent_name: None,
            parent_tool_invocation_id: None,
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
            max_tokens: 9999,
            // #869: `recent_window` / `summary_interval` are gone; the
            // compact strategy uses threshold-based knobs. Set
            // `compact_trigger_pct` to a non-default value to verify the
            // override propagates through subagent inheritance.
            context_config: alms_core::config::ContextConfig {
                strategy: "compact".into(),
                max_input_tokens: 50_000,
                compact_trigger_pct: 0.75,
                summary_model: Some("cheap-model".into()),
                summary_provider: Some("openrouter".into()),
                ..Default::default()
            },
            posture: alms_runtime::Posture::Guarded,
            sandbox_root: "/sandbox".into(),
            shell_policy: "unrestricted".into(),
            shell_spill: alms_core::config::ShellSpillConfig {
                enabled: false,
                retention_days: 42,
            },
            tool_output_truncate: alms_core::config::ToolOutputTruncateConfig {
                enabled: true,
                max_bytes: 16_384,
                max_lines: 1500,
                retention_days: 14,
            },
            enabled_tools: vec!["echo".into(), "math".into()],
            ..AgentConfig::default()
        };

        // Ephemeral subagent (no registry record)
        let (config, model, _provider) = agent_config_for_subagent(None, &parent);
        assert!(model.is_none());
        // Should inherit runtime settings from parent
        assert_eq!(config.max_tokens, 9999);
        assert_eq!(config.context_config.strategy, "compact");
        assert_eq!(config.context_config.max_input_tokens, 50_000);
        assert_eq!(config.context_config.compact_trigger_pct, 0.75);
        // Should inherit sandbox settings
        assert_eq!(config.sandbox_root, "/sandbox");
        assert_eq!(config.shell_policy, "unrestricted");
        assert_eq!(config.enabled_tools, vec!["echo", "math"]);
        // Should inherit the shell spill policy (issue #756 subagent inheritance)
        assert!(!config.shell_spill.enabled);
        assert_eq!(config.shell_spill.retention_days, 42);
        // Should inherit the in-loop tool-output truncation policy
        // (issue #851 subagent inheritance).
        assert!(config.tool_output_truncate.enabled);
        assert_eq!(config.tool_output_truncate.max_bytes, 16_384);
        assert_eq!(config.tool_output_truncate.max_lines, 1500);
        assert_eq!(config.tool_output_truncate.retention_days, 14);
        // system_prompt should be the default subagent prompt, not the parent's
        assert_eq!(config.system_prompt, DEFAULT_SUBAGENT_PROMPT);

        // Named subagent with registry overrides
        let record = SubagentRecordConfig {
            model: Some("gpt-5".into()),
            posture: Some("guarded".into()),
            provider: Some("anthropic".into()),
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
        };
        let (config2, model2, _provider2) = agent_config_for_subagent(Some(record), &parent);
        assert_eq!(model2.as_deref(), Some("gpt-5"));
        // system_prompt is always the default subagent prompt (not overridable per-agent)
        assert_eq!(config2.system_prompt, DEFAULT_SUBAGENT_PROMPT);
        assert_eq!(config2.posture, alms_runtime::Posture::Guarded);
        // Should still inherit runtime settings from parent
        assert_eq!(config2.max_tokens, 9999);
        assert_eq!(config2.context_config.max_input_tokens, 50_000);
        // Shell spill policy still inherited through the registry-override path
        assert!(!config2.shell_spill.enabled);
        assert_eq!(config2.shell_spill.retention_days, 42);
        // Tool-output truncation policy still inherited through the
        // registry-override path (issue #851 subagent inheritance).
        assert!(config2.tool_output_truncate.enabled);
        assert_eq!(config2.tool_output_truncate.max_bytes, 16_384);
        assert_eq!(config2.tool_output_truncate.max_lines, 1500);
    }

    // -- per-named-subagent summary provider/model overlay (issue #872) --------

    /// When the parent has summary_provider / summary_model set on its
    /// effective context config (per-agent ?? server-level resolved at
    /// the gateway), the subagent inherits those values verbatim through
    /// the `base.context_config.clone()` path. The subagent's own
    /// registry record can then OVERRIDE them by setting both fields on
    /// `SubagentRecordConfig`. `None` on the subagent record falls
    /// through to the parent's effective values — which is what the
    /// issue calls out as "subagents inherit the parent's effective
    /// summary config (not parent's primary provider)".
    #[test]
    fn test_subagent_inherits_parent_effective_summary_config() {
        let parent = AgentConfig {
            context_config: alms_core::config::ContextConfig {
                summary_provider: Some("openrouter".into()),
                summary_model: Some("minimax/minimax-m2.7".into()),
                ..Default::default()
            },
            ..AgentConfig::default()
        };

        // Ephemeral subagent: inherits the parent's effective summary
        // config wholesale. No registry record means no override.
        let (config, _, _) = agent_config_for_subagent(None, &parent);
        assert_eq!(
            config.context_config.summary_provider.as_deref(),
            Some("openrouter")
        );
        assert_eq!(
            config.context_config.summary_model.as_deref(),
            Some("minimax/minimax-m2.7")
        );

        // Named subagent with no per-agent summary fields: same
        // inheritance as ephemeral.
        let record = SubagentRecordConfig {
            model: None,
            posture: None,
            provider: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
        };
        let (config2, _, _) = agent_config_for_subagent(Some(record), &parent);
        assert_eq!(
            config2.context_config.summary_provider.as_deref(),
            Some("openrouter"),
            "subagent without per-agent summary config inherits parent's effective summary_provider"
        );
        assert_eq!(
            config2.context_config.summary_model.as_deref(),
            Some("minimax/minimax-m2.7"),
            "subagent without per-agent summary config inherits parent's effective summary_model"
        );
    }

    /// When the subagent's registry record carries its own
    /// summary_provider / summary_model pair, those override the
    /// parent's effective config. Pair-only invariant means both fields
    /// are guaranteed symmetric by the time they reach the coordinator.
    #[test]
    fn test_subagent_summary_override_wins_over_parent_inherit() {
        let parent = AgentConfig {
            context_config: alms_core::config::ContextConfig {
                summary_provider: Some("openrouter".into()),
                summary_model: Some("minimax/minimax-m2.7".into()),
                ..Default::default()
            },
            ..AgentConfig::default()
        };

        let record = SubagentRecordConfig {
            model: None,
            posture: None,
            provider: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: Some("anthropic".into()),
            summary_model: Some("claude-haiku-4".into()),
        };
        let (config, _, _) = agent_config_for_subagent(Some(record), &parent);
        assert_eq!(
            config.context_config.summary_provider.as_deref(),
            Some("anthropic"),
            "subagent's per-agent summary_provider must win over parent's effective value"
        );
        assert_eq!(
            config.context_config.summary_model.as_deref(),
            Some("claude-haiku-4"),
            "subagent's per-agent summary_model must win over parent's effective value"
        );
    }

    /// Both layers None: subagent inherits the both-None state. The
    /// runtime-side `build_summary_client` short-circuits on
    /// `summary_provider = None` to a clone of the agent's main LLM
    /// client (the back-compat path), so no separate summary task runs.
    /// This is the "neither set" path from the issue: clean inheritance,
    /// no inadvertent provider switch.
    #[test]
    fn test_subagent_summary_all_none_stays_none() {
        let parent = AgentConfig {
            context_config: alms_core::config::ContextConfig {
                summary_provider: None,
                summary_model: None,
                ..Default::default()
            },
            ..AgentConfig::default()
        };

        let (config, _, _) = agent_config_for_subagent(None, &parent);
        assert!(config.context_config.summary_provider.is_none());
        assert!(config.context_config.summary_model.is_none());
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
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
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

    // -- (j-pre-3) subagent thinking-budget override precedence (Tim S1) --------
    //
    // Subagent precedence: subagent-record override > parent-effective config.
    // (Parent-effective is itself resolved from per-agent > server-default on
    // the gateway side; per-run overrides were removed in #941.) A named
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
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
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
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
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
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
        };
        let (sub_none, _, _) = agent_config_for_subagent(Some(record_none), &parent);
        assert_eq!(
            sub_none.anthropic_thinking_budget, 4096,
            "named subagent with no override inherits the parent's budget"
        );
    }

    // -- (j-pre-4) subagent reasoning-effort override precedence (#768) ---------
    //
    // Mirrors the Anthropic path above (subagent-record > parent-effective;
    // parent-effective itself is per-agent > server-default after #941): a
    // named subagent registered with `reasoning_effort = Some(Low)` must
    // honour its own registry override and override the parent's
    // `Some(High)`. Ephemeral subagents (record = None) still inherit the
    // parent's effort.
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
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
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
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
        };
        let (sub_none, _, _) = agent_config_for_subagent(Some(record_none), &parent);
        assert_eq!(
            sub_none.openai_reasoning_effort,
            Some(ReasoningEffort::High),
            "named subagent with None override inherits the parent's effort"
        );
    }

    /// Issue #766: Anthropic prompt caching is a server-level toggle
    /// inherited verbatim by subagents. A parent with caching disabled
    /// produces subagents with caching disabled, and vice versa — no
    /// per-subagent override path.
    #[test]
    fn test_subagent_inherits_anthropic_prompt_cache_enabled() {
        // Parent has caching DISABLED.
        let parent = AgentConfig {
            anthropic_prompt_cache_enabled: false,
            ..AgentConfig::default()
        };
        let (ephemeral, _, _) = agent_config_for_subagent(None, &parent);
        assert!(
            !ephemeral.anthropic_prompt_cache_enabled,
            "ephemeral subagents must inherit parent's disabled caching"
        );

        let record = SubagentRecordConfig {
            model: None,
            posture: None,
            provider: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
        };
        let (named, _, _) = agent_config_for_subagent(Some(record), &parent);
        assert!(
            !named.anthropic_prompt_cache_enabled,
            "named subagents must inherit parent's disabled caching"
        );

        // Parent has caching ENABLED (the default).
        let enabled_parent = AgentConfig {
            anthropic_prompt_cache_enabled: true,
            ..AgentConfig::default()
        };
        let (sub, _, _) = agent_config_for_subagent(None, &enabled_parent);
        assert!(
            sub.anthropic_prompt_cache_enabled,
            "subagents inherit enabled caching"
        );
    }

    /// Issue #769: Gemini context caching + thinking budget inherit
    /// from the parent the same way Anthropic prompt caching does.
    /// Caching is server-level only; the thinking budget gained a
    /// per-named-subagent override in #794 (covered by the separate
    /// `test_subagent_gemini_thinking_budget_override` below).
    #[test]
    fn test_subagent_inherits_gemini_cache_and_thinking_budget() {
        // Parent disables caching and sets a thinking budget.
        let parent = AgentConfig {
            gemini_cache_enabled: false,
            gemini_cache_ttl_seconds: 1800,
            gemini_thinking_budget: Some(8192),
            ..AgentConfig::default()
        };
        let (ephemeral, _, _) = agent_config_for_subagent(None, &parent);
        assert!(
            !ephemeral.gemini_cache_enabled,
            "ephemeral subagents inherit parent's disabled Gemini caching"
        );
        assert_eq!(ephemeral.gemini_cache_ttl_seconds, 1800);
        assert_eq!(ephemeral.gemini_thinking_budget, Some(8192));

        // Named subagent with `None` override inherits the parent's
        // effective budget and caching state verbatim (#794 propagation).
        let record = SubagentRecordConfig {
            model: None,
            posture: None,
            provider: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
        };
        let (named, _, _) = agent_config_for_subagent(Some(record), &parent);
        assert!(!named.gemini_cache_enabled);
        assert_eq!(named.gemini_cache_ttl_seconds, 1800);
        assert_eq!(named.gemini_thinking_budget, Some(8192));

        // Parent with caching enabled (the default) → subagent also
        // enabled. `None` thinking budget on parent passes through.
        let enabled_parent = AgentConfig {
            gemini_cache_enabled: true,
            gemini_cache_ttl_seconds: 300,
            gemini_thinking_budget: None,
            ..AgentConfig::default()
        };
        let (sub, _, _) = agent_config_for_subagent(None, &enabled_parent);
        assert!(sub.gemini_cache_enabled);
        assert_eq!(sub.gemini_cache_ttl_seconds, 300);
        assert_eq!(sub.gemini_thinking_budget, None);
    }

    /// Issue #794: per-named-subagent Gemini thinking budget override
    /// wins over the parent's effective budget, mirroring how the
    /// Anthropic `thinking_budget_tokens` and OpenAI `reasoning_effort`
    /// overrides work for named subagents. `Some(0)` is an explicit
    /// disable; `None` falls through to the parent.
    #[test]
    fn test_subagent_gemini_thinking_budget_override() {
        // Parent has Gemini thinking enabled at 4096.
        let parent = AgentConfig {
            gemini_thinking_budget: Some(4096),
            ..AgentConfig::default()
        };

        // Named subagent explicitly sets Some(16384): must win.
        let record_high = SubagentRecordConfig {
            model: None,
            posture: None,
            provider: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: Some(16384),
            summary_provider: None,
            summary_model: None,
        };
        let (sub_high, _, _) = agent_config_for_subagent(Some(record_high), &parent);
        assert_eq!(
            sub_high.gemini_thinking_budget,
            Some(16384),
            "named subagent Some(16384) must override parent's Some(4096)"
        );

        // Named subagent explicitly sets Some(0): must disable even
        // though the parent would enable.
        let record_zero = SubagentRecordConfig {
            model: None,
            posture: None,
            provider: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: Some(0),
            summary_provider: None,
            summary_model: None,
        };
        let (sub_zero, _, _) = agent_config_for_subagent(Some(record_zero), &parent);
        assert_eq!(
            sub_zero.gemini_thinking_budget,
            Some(0),
            "named subagent Some(0) must disable Gemini thinking even when parent enables"
        );

        // Named subagent with None: inherit parent's Some(4096).
        let record_none = SubagentRecordConfig {
            model: None,
            posture: None,
            provider: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
        };
        let (sub_none, _, _) = agent_config_for_subagent(Some(record_none), &parent);
        assert_eq!(sub_none.gemini_thinking_budget, Some(4096));

        // Parent with thinking disabled (None), subagent opts in with
        // Some(8192): subagent wins.
        let disabled_parent = AgentConfig {
            gemini_thinking_budget: None,
            ..AgentConfig::default()
        };
        let record_optin = SubagentRecordConfig {
            model: None,
            posture: None,
            provider: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: Some(8192),
            summary_provider: None,
            summary_model: None,
        };
        let (sub_optin, _, _) = agent_config_for_subagent(Some(record_optin), &disabled_parent);
        assert_eq!(sub_optin.gemini_thinking_budget, Some(8192));
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
        let parent_agent_id = test_parent_agent_id();

        // First invocation with name "reviewer"
        let (r1, sub_sid_1) = coord
            .dispatch(
                "First task".to_string(),
                parent_session,
                parent_agent_id,
                None,
                None,
                Some("reviewer".to_string()),
                None,
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
                parent_agent_id,
                None,
                None,
                Some("reviewer".to_string()),
                None,
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

        // Verify session was reused under the post-#1051 keying:
        // `(parent_agent_id, name)`, not `(parent_session, name)`.
        let stable_id = AgentId::deterministic(parent_agent_id, "reviewer");
        let stable_ctx = format!("subagent_{}_{}", parent_agent_id.0, "reviewer");
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

    // -- (k2) parent can read subagent session after invocation (#1042 repro) ----

    /// Repro for #1042: after a parent agent invokes a named subagent via the
    /// real coordinator path, the parent (using the same `parent_session_id`)
    /// must be able to read the subagent's session via `ReadSubagentSessionTool`.
    ///
    /// This is the round-trip the existing tests in `read_subagent_session.rs`
    /// don't cover — they populate the session manually with the same
    /// derivation the read tool uses, so they can't catch a divergence between
    /// the invoke-side and read-side keys.
    #[tokio::test]
    async fn test_parent_can_read_named_subagent_session_after_dispatch() {
        use alms_sandbox::Tool;
        use alms_tools::ReadSubagentSessionTool;

        let workspace_tmp = tempfile::TempDir::new().unwrap();
        let coord = test_coordinator().with_workspace_dir(workspace_tmp.path().to_path_buf());
        let parent_session = test_session_id();
        let parent_agent_id = test_parent_agent_id();

        // Parent invokes the named subagent.
        let (_response, sub_session_id) = coord
            .dispatch(
                "Investigate topic X".to_string(),
                parent_session,
                parent_agent_id,
                None,
                None,
                Some("researcher".to_string()),
                None,
                None,
            )
            .await
            .expect("dispatch should succeed");

        // Parent reads the subagent session using its agent identity and the
        // subagent's name. This is the exact path the runtime wires when it
        // registers `ReadSubagentSessionTool` for the parent run.
        let read_tool =
            ReadSubagentSessionTool::new(coord.session_manager.clone(), parent_agent_id);
        let result = read_tool
            .execute(serde_json::json!({ "name": "researcher" }))
            .await
            .expect("read_subagent_session should not error");

        // Diagnostic dump so failures are debuggable from CI logs.
        debug!(?result, "parent read of named subagent session");

        assert!(
            result.get("error").is_none(),
            "parent should be able to read its own subagent's session, got error: {result}"
        );
        assert_eq!(result["subagent"], "researcher");
        assert!(
            result["message_count"].as_u64().unwrap_or(0) > 0,
            "subagent session should have at least one message after dispatch"
        );

        // Sanity-check: the subagent session ID returned by dispatch matches
        // the session ID the read tool resolved to (via the deterministic
        // (parent_agent_id, name) key — #1051 / #1068).
        let derived_id = AgentId::deterministic(parent_agent_id, "researcher");
        let derived_ctx = format!("subagent_{}_{}", parent_agent_id.0, "researcher");
        let session = coord
            .session_manager
            .get_or_create(derived_id, &derived_ctx);
        assert_eq!(
            session.id, sub_session_id,
            "derived session ID should match the one returned by dispatch"
        );
    }

    /// Repro for #1042 / persistence path: after a daemon restart
    /// (`SessionManager` reloaded from disk), the parent can still read
    /// the subagent session it spawned in a previous run.
    #[tokio::test]
    async fn test_parent_can_read_named_subagent_session_after_reload() {
        use alms_sandbox::Tool;
        use alms_session::SessionConfig;
        use alms_tools::ReadSubagentSessionTool;

        let workspace_tmp = tempfile::TempDir::new().unwrap();
        let db_tmp = tempfile::TempDir::new().unwrap();
        let db_path = db_tmp.path().join("alms.db");

        // First daemon lifetime: parent invokes a named subagent against a
        // SQLite-backed session manager.
        let parent_session = test_session_id();
        let parent_agent_id = test_parent_agent_id();
        let sub_session_id_first;
        {
            let session_manager = Arc::new(
                SessionManager::with_sqlite(SessionConfig::default(), db_path.to_str().unwrap())
                    .expect("open SQLite session manager"),
            );
            let llm_config = LlmConfig {
                mock: true,
                ..LlmConfig::default()
            };
            let llm = LlmClient::new(llm_config).unwrap();
            let coord = Coordinator::new(AgentId::new(), session_manager.clone(), llm)
                .with_workspace_dir(workspace_tmp.path().to_path_buf());

            let (_response, sub_session_id) = coord
                .dispatch(
                    "Investigate topic X".to_string(),
                    parent_session,
                    parent_agent_id,
                    None,
                    None,
                    Some("researcher".to_string()),
                    None,
                    None,
                )
                .await
                .expect("dispatch should succeed");
            sub_session_id_first = sub_session_id;

            // Flush WAL so the second SessionManager sees the writes.
            session_manager.flush_wal().ok();
        }

        // Second daemon lifetime: fresh SessionManager loads from disk.
        // The parent's read_subagent_session must still find the subagent's
        // session — proves the link survives restart.
        {
            let session_manager = Arc::new(
                SessionManager::with_sqlite(SessionConfig::default(), db_path.to_str().unwrap())
                    .expect("reopen SQLite session manager"),
            );

            let read_tool = ReadSubagentSessionTool::new(session_manager.clone(), parent_agent_id);
            let result = read_tool
                .execute(serde_json::json!({ "name": "researcher" }))
                .await
                .expect("read_subagent_session should not error after reload");

            debug!(
                ?result,
                "parent read of named subagent session after reload"
            );

            assert!(
                result.get("error").is_none(),
                "after reload, parent should still be able to read subagent session, got: {result}"
            );
            assert_eq!(result["subagent"], "researcher");

            // The session ID resolved by the read tool must match the one
            // dispatch returned in the first daemon lifetime — keyed on
            // `(parent_agent_id, name)` (#1051 / #1068).
            let derived_id = AgentId::deterministic(parent_agent_id, "researcher");
            let derived_ctx = format!("subagent_{}_{}", parent_agent_id.0, "researcher");
            let session = session_manager.get_or_create(derived_id, &derived_ctx);
            assert_eq!(
                session.id, sub_session_id_first,
                "derived session ID should match the one dispatched in the first lifetime"
            );
        }
    }

    /// Repro for #1042 / unauthorized-third-party path: an unrelated agent
    /// (different `parent_agent_id`) calling `read_subagent_session` against
    /// the same subagent name must NOT see the original parent's subagent
    /// session. Post-#1068, named subagent sessions are keyed on
    /// `(parent_agent_id, name)` — the unrelated agent's derived
    /// `(agent_id, context_id)` key is different, so `has_session` returns
    /// false.
    #[tokio::test]
    async fn test_unrelated_agent_cannot_read_subagent_session() {
        use alms_sandbox::Tool;
        use alms_tools::ReadSubagentSessionTool;

        let workspace_tmp = tempfile::TempDir::new().unwrap();
        let coord = test_coordinator().with_workspace_dir(workspace_tmp.path().to_path_buf());

        let parent_session = test_session_id();
        let parent_agent_id = test_parent_agent_id();
        coord
            .dispatch(
                "Investigate topic X".to_string(),
                parent_session,
                parent_agent_id,
                None,
                None,
                Some("researcher".to_string()),
                None,
                None,
            )
            .await
            .expect("dispatch should succeed");

        // Unrelated agent: different parent_agent_id, attempts to read.
        let unrelated_agent_id = AgentId::new();
        assert_ne!(unrelated_agent_id, parent_agent_id);
        let read_tool =
            ReadSubagentSessionTool::new(coord.session_manager.clone(), unrelated_agent_id);
        let result = read_tool
            .execute(serde_json::json!({ "name": "researcher" }))
            .await
            .expect("read_subagent_session should not panic for unrelated agent");

        assert!(
            result["error"]
                .as_str()
                .unwrap_or("")
                .contains("No session found"),
            "unrelated agent must see 'No session found', got: {result}"
        );
    }

    // -- (l) concurrent named subagent invocations are rejected -----------------

    #[tokio::test]
    async fn test_concurrent_named_subagent_rejected() {
        let coord = test_coordinator();
        let session_id = test_session_id();
        let parent_agent_id = test_parent_agent_id();

        // Spawn a named subagent with a long timeout so it stays active
        let request = SubagentRequest {
            task: "Long task".to_string(),
            timeout: Duration::from_secs(300),
            parent_session: session_id,
            parent_agent_id,
            parent_run_id: None,
            subagent_name: Some("researcher".to_string()),
            parent_tool_invocation_id: None,
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
            parent_agent_id,
            parent_run_id: None,
            subagent_name: Some("researcher".to_string()),
            parent_tool_invocation_id: None,
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
            parent_agent_id,
            parent_run_id: None,
            subagent_name: Some("coder".to_string()),
            parent_tool_invocation_id: None,
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
        let parent_agent_id = test_parent_agent_id();

        // Two invocations without name — each should get a fresh session
        let (_r1, sub_sid_1) = coord
            .dispatch(
                "Task one".to_string(),
                parent_session,
                parent_agent_id,
                None,
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
                parent_agent_id,
                None,
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

    // -- (m) #1051 — named subagent identity is keyed on parent_agent_id ---------

    /// Unit-level regression for #1051.
    ///
    /// `derive_subagent_identity` must produce the same `(stable_id,
    /// stable_ctx)` for two requests that share `(parent_agent_id, name)`
    /// even when their `parent_session` differs. Pre-#1051 the derivation
    /// was keyed on `parent_session`, so this assertion would fail.
    #[test]
    fn test_named_subagent_identity_invariant_across_parent_sessions() {
        let parent_agent_id = AgentId::new();
        let session_a = SessionId::new();
        let session_b = SessionId::new();
        assert_ne!(
            session_a, session_b,
            "test setup: parent sessions must differ"
        );

        let req_a = SubagentRequest {
            task: "task A".into(),
            timeout: Duration::from_secs(1),
            parent_session: session_a,
            parent_agent_id,
            parent_run_id: None,
            subagent_name: Some("reviewer".into()),
            parent_tool_invocation_id: None,
        };
        let req_b = SubagentRequest {
            task: "task B".into(),
            timeout: Duration::from_secs(1),
            parent_session: session_b,
            parent_agent_id,
            parent_run_id: None,
            subagent_name: Some("reviewer".into()),
            parent_tool_invocation_id: None,
        };

        let (id_a, ctx_a) = derive_subagent_identity(TaskId::new(), &req_a);
        let (id_b, ctx_b) = derive_subagent_identity(TaskId::new(), &req_b);

        assert_eq!(
            id_a, id_b,
            "named subagent stable_id must be identical for the same \
             (parent_agent_id, name) across different parent sessions"
        );
        assert_eq!(
            ctx_a, ctx_b,
            "named subagent stable_ctx must be identical for the same \
             (parent_agent_id, name) across different parent sessions"
        );
        // Sanity: the ctx is keyed on parent_agent_id, not parent_session.
        assert!(
            ctx_a.contains(&parent_agent_id.0.to_string()),
            "stable_ctx should embed parent_agent_id, got {ctx_a}"
        );
        assert!(
            !ctx_a.contains(&session_a.0.to_string()) && !ctx_a.contains(&session_b.0.to_string()),
            "stable_ctx must not embed parent_session (#1051), got {ctx_a}"
        );
    }

    /// Different parent agents must NOT collide on the same subagent name.
    #[test]
    fn test_named_subagent_identity_differs_across_parent_agents() {
        let session = SessionId::new();
        let parent_a = AgentId::new();
        let parent_b = AgentId::new();
        assert_ne!(parent_a, parent_b);

        let mk = |parent_agent_id: AgentId| SubagentRequest {
            task: "t".into(),
            timeout: Duration::from_secs(1),
            parent_session: session,
            parent_agent_id,
            parent_run_id: None,
            subagent_name: Some("reviewer".into()),
            parent_tool_invocation_id: None,
        };

        let (id_a, ctx_a) = derive_subagent_identity(TaskId::new(), &mk(parent_a));
        let (id_b, ctx_b) = derive_subagent_identity(TaskId::new(), &mk(parent_b));

        assert_ne!(
            id_a, id_b,
            "different parent agents must yield different subagent stable_ids"
        );
        assert_ne!(
            ctx_a, ctx_b,
            "different parent agents must yield different subagent stable_ctxs"
        );
    }

    /// Integration-level regression for #1051 — mirrors
    /// `test_named_subagent_persistent_session` but spans TWO parent chat
    /// sessions. The second dispatch (from session B) must land in the
    /// same subagent session as the first (from session A) and see the
    /// full prior history.
    #[tokio::test]
    async fn test_named_subagent_session_reuse_across_parent_sessions() {
        let workspace_tmp = tempfile::TempDir::new().unwrap();
        let workspace_dir = workspace_tmp.path().to_path_buf();
        let coord = test_coordinator().with_workspace_dir(workspace_dir);
        let parent_agent_id = test_parent_agent_id();
        let session_a = test_session_id();
        let session_b = test_session_id();
        assert_ne!(
            session_a, session_b,
            "test setup: parent chat sessions must differ"
        );

        // First dispatch from chat session A.
        let (_r1, sub_sid_1) = coord
            .dispatch(
                "First task from chat A".to_string(),
                session_a,
                parent_agent_id,
                None,
                None,
                Some("reviewer".to_string()),
                None,
                None,
            )
            .await
            .expect("first dispatch should succeed");

        // Second dispatch from chat session B (different parent_session,
        // SAME parent_agent_id). Must reuse the same subagent session.
        let (_r2, sub_sid_2) = coord
            .dispatch(
                "Follow up from chat B".to_string(),
                session_b,
                parent_agent_id,
                None,
                None,
                Some("reviewer".to_string()),
                None,
                None,
            )
            .await
            .expect("second dispatch should succeed");

        assert_eq!(
            sub_sid_1, sub_sid_2,
            "Named subagent must reuse the same session across different \
             parent chat sessions sharing the same parent_agent_id (#1051)"
        );

        // Both turns landed in the same subagent session — total 4 messages.
        let stable_id = AgentId::deterministic(parent_agent_id, "reviewer");
        let stable_ctx = format!("subagent_{}_{}", parent_agent_id.0, "reviewer");
        let session = coord.session_manager.get_or_create(stable_id, &stable_ctx);
        let messages = coord.session_manager.get_history(session.id).unwrap();
        assert_eq!(
            messages.len(),
            4,
            "Cross-session reuse should accumulate both turns in one subagent \
             session (4 messages: 2× user/assistant), got {}",
            messages.len()
        );
    }

    // -- (n) #1068 — subagent_prompts cache cleanup uses the right key ----------

    /// Regression for the silent cache leak that shipped in #1051: the
    /// `subagent_prompts` cleanup was keyed on `parent_session.0` while the
    /// insert (post-#1051) was keyed on `parent_agent_id.0`, so `remove`
    /// became a permanent no-op and the cache grew without bound.
    ///
    /// This test dispatches a named subagent and asserts the cache shrinks
    /// back to its prior size after the TTL grace window — proving
    /// insert/remove keys agree.
    #[tokio::test]
    async fn test_subagent_prompts_cache_cleanup_after_dispatch() {
        // The cleanup waits SUBAGENT_TTL_SECS before pulling the handle and
        // running the cache `remove`, so we can't realistically wait it out.
        // Instead, drive `dispatch` to completion and then directly drain the
        // cache by computing the same key shape `derive_subagent_identity`
        // produces — proving that the insert key matches what cleanup would
        // remove if the TTL elapsed.
        let workspace_tmp = tempfile::TempDir::new().unwrap();
        let workspace_dir = workspace_tmp.path().to_path_buf();
        let coord = test_coordinator().with_workspace_dir(workspace_dir);
        let parent_session = test_session_id();
        let parent_agent_id = test_parent_agent_id();

        assert_eq!(coord.subagent_prompts.len(), 0, "Cache should start empty");

        coord
            .dispatch(
                "First task".into(),
                parent_session,
                parent_agent_id,
                None,
                None,
                Some("reviewer".to_string()),
                None,
                None,
            )
            .await
            .expect("dispatch should succeed");

        // After dispatch, the cache should hold exactly one entry under the
        // new `(parent_agent_id, name)` key shape — NOT the old
        // `(parent_session, name)` shape.
        assert_eq!(
            coord.subagent_prompts.len(),
            1,
            "Cache should hold one entry after a named dispatch"
        );

        let new_key = format!("subagent_{}_{}", parent_agent_id.0, "reviewer");
        let old_key = format!("subagent_{}_{}", parent_session.0, "reviewer");
        assert!(
            coord.subagent_prompts.contains_key(&new_key),
            "Cache must be keyed on parent_agent_id (#1051), key was: {}",
            coord
                .subagent_prompts
                .iter()
                .map(|e| e.key().clone())
                .collect::<Vec<_>>()
                .join(", ")
        );
        assert!(
            !coord.subagent_prompts.contains_key(&old_key),
            "Cache must not still use the pre-#1051 parent_session key shape"
        );

        // Now simulate the cleanup that fires after SUBAGENT_TTL_SECS by
        // removing under the same key the production cleanup uses. If the
        // production cleanup were still keyed on parent_session, this would
        // be a no-op and the cache would leak.
        coord.subagent_prompts.remove(&new_key);
        assert_eq!(
            coord.subagent_prompts.len(),
            0,
            "Cache must shrink back to empty after cleanup — insert and \
             remove key shapes must agree (#1068)"
        );
    }

    // -- (o) #1068 — active_named guard scopes by parent_agent_id ----------------

    /// Regression for the over-broad concurrency guard that shipped in
    /// #1051: `active_named` was keyed on bare `name`, so two different
    /// parent agents trying to spawn a same-named subagent concurrently
    /// would incorrectly collide even though their sessions are disjoint
    /// under Option C.
    ///
    /// This test does NOT cover *true* concurrent dispatch (that needs
    /// timing control we don't have with the mock LLM) — instead it inserts
    /// the guard key for parent A directly and confirms parent B's dispatch
    /// is NOT rejected by the active-set check.
    #[tokio::test]
    async fn test_active_named_does_not_collide_across_parent_agents() {
        let workspace_tmp = tempfile::TempDir::new().unwrap();
        let coord = test_coordinator().with_workspace_dir(workspace_tmp.path().to_path_buf());
        let parent_a = test_parent_agent_id();
        let parent_b = test_parent_agent_id();
        assert_ne!(parent_a, parent_b);

        // Manually take the guard slot for parent_a's "reviewer".
        coord
            .active_named
            .insert((parent_a, "reviewer".to_string()));

        // parent_b dispatching "reviewer" must succeed despite parent_a's
        // slot being held — Option C says their sessions are disjoint.
        let result = coord
            .dispatch(
                "from parent B".to_string(),
                test_session_id(),
                parent_b,
                None,
                None,
                Some("reviewer".to_string()),
                None,
                None,
            )
            .await;
        assert!(
            result.is_ok(),
            "parent_b's named dispatch must not be blocked by parent_a's \
             active guard slot (#1068): {result:?}"
        );

        // Sanity: parent_a's slot is still held — we never released it.
        assert!(
            coord
                .active_named
                .contains(&(parent_a, "reviewer".to_string())),
            "parent_a's guard slot should still be held"
        );
    }

    // -- (p) #1068 / S2 — cross-parent-agent dispatch yields disjoint sessions --

    /// End-to-end S2 contract test from #1068: dispatching the same
    /// subagent name from two different parent agents must produce two
    /// DISTINCT subagent sessions (Option C scoping).
    #[tokio::test]
    async fn test_named_subagent_disjoint_across_parent_agents() {
        let workspace_tmp = tempfile::TempDir::new().unwrap();
        let coord = test_coordinator().with_workspace_dir(workspace_tmp.path().to_path_buf());
        let parent_a = test_parent_agent_id();
        let parent_b = test_parent_agent_id();
        assert_ne!(parent_a, parent_b);

        let (_r1, sub_sid_a) = coord
            .dispatch(
                "from parent A".to_string(),
                test_session_id(),
                parent_a,
                None,
                None,
                Some("reviewer".to_string()),
                None,
                None,
            )
            .await
            .expect("parent_a dispatch should succeed");

        let (_r2, sub_sid_b) = coord
            .dispatch(
                "from parent B".to_string(),
                test_session_id(),
                parent_b,
                None,
                None,
                Some("reviewer".to_string()),
                None,
                None,
            )
            .await
            .expect("parent_b dispatch should succeed");

        assert_ne!(
            sub_sid_a, sub_sid_b,
            "Different parent agents spawning the same name must land in \
             DISJOINT subagent sessions (Option C, #1051)"
        );

        // Each parent's session sees only its own turn (2 messages: user+assistant).
        let ctx_a = format!("subagent_{}_{}", parent_a.0, "reviewer");
        let ctx_b = format!("subagent_{}_{}", parent_b.0, "reviewer");
        let session_a = coord
            .session_manager
            .get_or_create(AgentId::deterministic(parent_a, "reviewer"), &ctx_a);
        let session_b = coord
            .session_manager
            .get_or_create(AgentId::deterministic(parent_b, "reviewer"), &ctx_b);
        assert_ne!(session_a.id, session_b.id);
        assert_eq!(
            coord
                .session_manager
                .get_history(session_a.id)
                .unwrap()
                .len(),
            2,
            "parent_a's reviewer should hold exactly its own turn"
        );
        assert_eq!(
            coord
                .session_manager
                .get_history(session_b.id)
                .unwrap()
                .len(),
            2,
            "parent_b's reviewer should hold exactly its own turn"
        );
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

    // -- (#921 review fix #5) subagent end-to-end truncation --------------------
    //
    // The unit-level test `test_subagent_inherits_parent_config` already
    // pins the policy inheritance (max_bytes / max_lines / retention_days
    // propagate from parent to subagent). This test exercises the *runtime*
    // wiring: a subagent-shaped `AgentRuntime` constructed with the same
    // builder calls used by `run_subagent` must (a) write spill files
    // under `{data_dir}/tool-output/sub-{task_id}/`, (b) cap the in-loop
    // preview to the configured byte budget, and (c) emit a bounded
    // `truncate_for_emit` value for the audit log + ToolEnd SSE paths.
    //
    // Driving this through the full `Coordinator::dispatch` path is hard
    // because the mock LLM can't synthesize tool calls, so we instead build
    // the subagent runtime in the same shape `run_subagent` does and
    // exercise `process_tool_results` + `truncate_for_emit` directly.

    #[tokio::test]
    async fn test_subagent_runtime_truncates_oversized_tool_output_to_sub_dir() {
        use alms_core::{AgentId, ToolCallRecord};
        use alms_runtime::AgentRuntime;
        use alms_runtime::llm_types::{LlmConfig, LlmMessage, ToolCall};
        use alms_session::{SessionConfig, SessionManager};

        // Per-test data dir — gateway puts subagent spills under
        // `{data_dir}/tool-output/sub-{task_id}/` so we mimic that layout.
        let data_dir = tempfile::tempdir().unwrap();
        let task_id = TaskId::new();

        // Build the subagent's tool-output spill dir exactly the way
        // `run_subagent` does at line ~1244-1252.
        let sub_run_dir = data_dir
            .path()
            .join(alms_runtime::tool_output_truncate::TOOL_OUTPUT_DIR_NAME)
            .join(format!("sub-{}", task_id.0));

        // Build a subagent-shaped AgentConfig with truncation enabled and
        // small caps so the test stays fast.
        let config = AgentConfig {
            tool_output_truncate: alms_core::config::ToolOutputTruncateConfig {
                enabled: true,
                max_bytes: 4 * 1024,
                max_lines: 100,
                retention_days: 7,
            },
            ..AgentConfig::default()
        };

        let llm_config = LlmConfig {
            mock: true,
            ..LlmConfig::default()
        };
        let llm = LlmClient::new(llm_config).unwrap();

        // Same builder sequence as `run_subagent`:
        //   AgentRuntime::new -> with_tool_output_truncate -> (no workspace
        //   in this test, mirrors ephemeral subagent path with `data_dir`
        //   set but `attach_workspace` false).
        let runtime = AgentRuntime::new(AgentId::new(), config, llm)
            .unwrap()
            .with_tool_output_truncate(
                sub_run_dir.clone(),
                /* enabled */ true,
                /* max_bytes */ 4 * 1024,
                /* max_lines */ 100,
            );

        // Drive `process_tool_results` with a 100 KB tool result.
        let session_manager = SessionManager::new(SessionConfig::default());
        let session = session_manager.get_or_create(AgentId::new(), "subagent-test");

        let tool_call = ToolCall::new("call_subagent_huge", "fake_tool", "{}");
        let raw = "x".repeat(100 * 1024);
        let result_value = serde_json::Value::String(raw.clone());

        let mut messages: Vec<LlmMessage> = Vec::new();
        let mut records: Vec<ToolCallRecord> = Vec::new();
        let mut seq: u32 = 0;
        let invocation_ids = vec![Uuid::new_v4()];

        runtime.process_tool_results(
            std::slice::from_ref(&tool_call),
            vec![Ok(result_value.clone())],
            &invocation_ids,
            &mut messages,
            &mut records,
            &mut seq,
            &session_manager,
            session.id,
            /* is_dm */ false,
        );

        // (a) The spill file must land under `tool-output/sub-{task_id}/`,
        //     NOT directly under `tool-output/` and NOT under any other
        //     per-run subdir. This is the key distinguishing layout for
        //     subagents — the parent's gateway-startup retention sweep
        //     walks every child of `tool-output/` so subagent spills get
        //     swept the same way parent spills do.
        let spill_path = sub_run_dir.join("tool_call_subagent_huge.txt");
        assert!(
            spill_path.exists(),
            "subagent spill must land under tool-output/sub-{}/: {}",
            task_id.0,
            spill_path.display()
        );
        let spilled = std::fs::read_to_string(&spill_path).unwrap();
        assert_eq!(
            spilled,
            result_value.to_string(),
            "spill bytes must match the original JSON-stringified result"
        );

        // (b) The subagent's in-loop preview must be bounded by max_bytes
        //     + the marker budget (4 KB cap + ~1 KB hint), not the original
        //     100 KB.
        assert_eq!(messages.len(), 1);
        let preview = messages[0].content_str();
        assert!(
            preview.len() < 8 * 1024,
            "subagent in-loop preview must be capped: {} bytes (cap = {} + hint)",
            preview.len(),
            4 * 1024
        );

        // (c) `truncate_for_emit` (used by audit log + ToolEnd SSE) must
        //     also see a bounded value. This is the parent's view of the
        //     subagent's tool result via the SSE forward / audit chain.
        let emit_value = runtime.truncate_for_emit(&tool_call.id, &result_value);
        let emit_str = emit_value
            .as_str()
            .expect("truncated emit value must be a JSON string");
        assert!(
            emit_str.len() < 8 * 1024,
            "subagent truncate_for_emit must be capped: {} bytes",
            emit_str.len()
        );

        // (d) The persisted session row must carry `truncated_in_loop:
        //     true` so a follow-up subagent run on the same session sees
        //     the truncated bytes (not the legacy 2000-byte re-truncated
        //     ones) when the context builder reconstructs history.
        let history = session_manager.get_history(session.id).unwrap();
        let tool_msg = history
            .iter()
            .find(|m| matches!(m.role, alms_session::Role::Tool))
            .expect("subagent tool result must be persisted");
        let meta = tool_msg.metadata.as_ref().expect("metadata must be set");
        assert_eq!(
            meta.get("truncated_in_loop").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert!(
            meta.get("spill_path")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.contains("sub-")),
            "persisted spill_path must reference the subagent dir: {meta:?}"
        );
    }

    // -- #1075 — one session row per subagent invocation -----------------------

    /// Count sessions whose `context_id` starts with `subagent_`. Used by the
    /// #1075 regression tests below — distinguishes subagent rows from the
    /// parent's own session row (which uses a different context_id).
    fn count_subagent_sessions(session_manager: &SessionManager) -> usize {
        session_manager
            .list_all()
            .into_iter()
            .filter(|s| s.context_id.starts_with("subagent_"))
            .count()
    }

    /// #1075 — Ephemeral foreground dispatch must produce exactly ONE session
    /// row, and the `session_id` returned to the parent (which flows into the
    /// `invoke_agent` tool result) must match the row where messages were
    /// actually written.
    ///
    /// Pre-fix: `derive_subagent_identity` was called twice — once in
    /// `spawn_subagent` and once in `run_subagent` — minting a fresh
    /// `AgentId::new()` each time for ephemeral subagents. That produced two
    /// `(agent_id, context_id)` keys on the session map, two `get_or_create`
    /// insertions, and two session rows. The handle held the first id (empty
    /// orphan); messages landed on the second.
    #[tokio::test]
    async fn test_1075_ephemeral_foreground_produces_one_session_row() {
        let coord = test_coordinator();
        let (response, sub_session_id) = coord
            .dispatch(
                "Say hello".to_string(),
                test_session_id(),
                test_parent_agent_id(),
                None,
                None,
                None, // ephemeral — the regression vector
                None,
                None,
            )
            .await
            .expect("dispatch should succeed");
        assert!(response.contains("mock"));

        // Exactly one subagent session row exists.
        assert_eq!(
            count_subagent_sessions(&coord.session_manager),
            1,
            "ephemeral foreground dispatch must create exactly one session row"
        );

        // The returned session_id (which the `invoke_agent` tool hands back
        // to the parent) is the row where messages were actually written.
        let history = coord
            .session_manager
            .get_history(sub_session_id)
            .expect("returned session_id must resolve to a real row");
        assert!(
            !history.is_empty(),
            "subagent session must have at least one message — \
             pre-fix this id pointed at an empty orphan row (#1075)"
        );
    }

    /// #1075 — Ephemeral background dispatch must also produce exactly ONE
    /// session row. This is the original repro path (`is_background=true`)
    /// from Atlas's diagnosis.
    #[tokio::test]
    async fn test_1075_ephemeral_background_produces_one_session_row() {
        let coord = test_coordinator();
        let (task_uuid, sub_session_id) = coord
            .dispatch_background(
                "Background work".to_string(),
                test_session_id(),
                test_parent_agent_id(),
                None,
                None,
                None, // ephemeral — the regression vector
                None,
                None,
            )
            .await
            .expect("dispatch_background should succeed");

        // Wait for the background task to finish so its messages land in
        // the session before we count rows.
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
            "background subagent should reach terminal state"
        );

        assert_eq!(
            count_subagent_sessions(&coord.session_manager),
            1,
            "ephemeral background dispatch must create exactly one session row"
        );

        let history = coord
            .session_manager
            .get_history(sub_session_id)
            .expect("returned session_id must resolve to a real row");
        assert!(
            !history.is_empty(),
            "background subagent session must have at least one message — \
             pre-fix this id pointed at an empty orphan row (#1075)"
        );
    }

    /// #1075 — Named subagent dispatch (foreground) must also produce exactly
    /// ONE session row. Named subagents were not affected by the original bug
    /// because `derive_subagent_identity` uses `AgentId::deterministic(...)`
    /// in that branch, so both call sites produced the same key — but the
    /// post-fix invariant must hold for both branches.
    #[tokio::test]
    async fn test_1075_named_foreground_produces_one_session_row() {
        let workspace_tmp = tempfile::TempDir::new().unwrap();
        let coord = test_coordinator().with_workspace_dir(workspace_tmp.path().to_path_buf());
        let (_response, sub_session_id) = coord
            .dispatch(
                "Investigate X".to_string(),
                test_session_id(),
                test_parent_agent_id(),
                None,
                None,
                Some("researcher".to_string()),
                None,
                None,
            )
            .await
            .expect("dispatch should succeed");

        assert_eq!(
            count_subagent_sessions(&coord.session_manager),
            1,
            "named foreground dispatch must create exactly one session row"
        );

        let history = coord
            .session_manager
            .get_history(sub_session_id)
            .expect("returned session_id must resolve to a real row");
        assert!(
            !history.is_empty(),
            "named subagent session must have at least one message"
        );
    }
}
