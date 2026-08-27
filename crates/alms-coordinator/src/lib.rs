pub mod message_bus;

use alms_core::{
    AgentId, AlmsError, AlmsResult, Run, RunId, RunRegistrar, SessionId, TokenUsage,
    truncate_to_char_boundary,
};
use alms_runtime::{AgentConfig, AgentRuntime, LlmClient, RunOutput};
use alms_session::SessionManager;
use alms_tools::event_forwarder::EventForwarder;
use alms_tools::subagent::SubagentDispatcher;
use alms_tools::subagent_self_sink::SubagentSelfEventSink;
use async_trait::async_trait;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// How long (in seconds) a subagent's result is kept in memory after the run
/// completes so the completion-notification system can poll it (background
/// dispatch) before the handle is reaped.
///
/// This is purely a post-completion retention window. It used to be the same
/// `SUBAGENT_TTL_SECS` constant that *also* served as a hard 5-minute
/// wall-clock kill on an actively-running subagent — but that overloaded
/// timer killed legitimately long subagents mid-work (#1150). The run-kill
/// arm was removed in #1150; a subagent now terminates via the
/// inherited in-loop phase-aware inactivity timer (#1150) + `max_iterations`,
/// or via cancellation. Only this retention concern remains here.
const RESULT_RETENTION_SECS: u64 = 300;

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

/// A subagent's most recent coarse activity signal, as emitted (post-dedup)
/// by the subagent→parent relay for the parent's Subagent status bar.
///
/// `kind` is one of [`alms_tools::subagent_activity_kind`]; `tool` is the tool
/// name (populated only for `tool_start`).
///
/// Recorded on the `SubagentHandle` so the CURRENT status stays queryable
/// after the live `subagent_activity` SSE signal has fired (#1189 follow-up):
/// the signal is ephemeral (never persisted/replayed) and deduplicated to one
/// emission per activity transition, so a session-stream subscriber that
/// attaches AFTER a transition — page reload, session switch back from the
/// subagent view, a second tab, an SSE reconnect — would otherwise have no way
/// to learn what the subagent is doing until the NEXT transition (which, in a
/// long reasoning/writing phase, can be never). The gateway replays this
/// snapshot to every newly-attached session stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentActivity {
    pub kind: String,
    pub tool: Option<String>,
    /// The subagent's tool invocation id (tool kinds only). Recorded so the
    /// attach-time snapshot replay carries the SAME id as the live signal —
    /// that identity is what lets the client recognise the replay instead of
    /// counting it as a new tool invocation (#1190), while parallel
    /// invocations of the same tool (distinct ids) each count.
    pub tool_invocation_id: Option<Uuid>,
    /// The PARENT invoke_agent tool-invocation-id — the chip-resolution
    /// correlator (#1190 Codex P2): the same id `subagent_started` carries,
    /// which unnamed subagent chips are keyed by. Recorded so the attach-time
    /// snapshot replay resolves to the RIGHT chip identity-exactly; the
    /// task-derived `source_agent` label alone forces a first-match fallback
    /// that can persistently cross-attach status between concurrent unnamed
    /// subagents.
    pub parent_tool_invocation_id: Option<Uuid>,
}

/// The frontend-facing label the relay tags a subagent's forwarded events
/// with (`source_agent`): the subagent's registered name, or
/// `subagent-{task_id_prefix}` for ephemeral/unnamed subagents. Single source
/// of truth — used by the relay in `run_agent_loop` AND stored on the
/// `SubagentHandle` so [`Coordinator::subagent_activity_snapshot`] reports
/// the exact label the frontend keys its status-bar chips by.
fn subagent_label(task_id: TaskId, subagent_name: Option<&str>) -> String {
    subagent_name
        .map(str::to_string)
        .unwrap_or_else(|| format!("subagent-{}", &task_id.0.to_string()[..8]))
}

/// One entry of [`Coordinator::subagent_activity_snapshot`]: the current
/// coarse activity of an in-flight subagent, keyed by the `source_agent`
/// label the frontend routes `subagent_activity` signals by.
#[derive(Debug, Clone)]
pub struct SubagentActivitySnapshot {
    /// The relay's `source_agent` label (see [`subagent_label`]).
    pub label: String,
    /// Latest activity kind ([`alms_tools::subagent_activity_kind`]).
    pub kind: String,
    /// Tool name (only when `kind == "tool_start"`).
    pub tool: Option<String>,
    /// The subagent's tool invocation id (tool kinds only) — replayed
    /// verbatim so the client's DISTINCT-id tool counting recognises the
    /// snapshot as the already-counted in-progress invocation (#1190).
    pub tool_invocation_id: Option<Uuid>,
    /// The PARENT invoke_agent tool-invocation-id — replayed verbatim so the
    /// client resolves the target chip identity-exactly (#1190 Codex P2).
    pub parent_tool_invocation_id: Option<Uuid>,
    /// The parent run the subagent was spawned from, when known.
    pub parent_run_id: Option<RunId>,
}

/// Handle to a running subagent
#[derive(Debug)]
struct SubagentHandle {
    task_id: TaskId,
    status: TaskStatus,
    /// The subagent's own cancellation token — the same child token
    /// `run_subagent`'s `select!` waits on. Derived in `spawn_subagent`
    /// (from the parent run's token when present) so it exists before the
    /// subagent task starts. Firing it cancels JUST this subagent through
    /// the exact same path as a parent-run cancel cascade, so all terminal
    /// bookkeeping (status flip, run record update, `run_cancelled` on the
    /// subagent's own session, `subagent_completed` notification for
    /// background subagents) runs unchanged. Powers
    /// [`Coordinator::cancel_subagent_by_session`] / the gateway's
    /// `POST /sessions/{id}/subagent/cancel` endpoint.
    cancel_token: CancellationToken,
    parent_run_id: Option<RunId>,
    parent_session_id: SessionId,
    parent_agent_id: AgentId,
    /// The subagent's own session ID (for frontend navigation).
    subagent_session_id: SessionId,
    /// Whether this was spawned via `dispatch_background` (triggers completion notification).
    is_background: bool,
    /// The relay's `source_agent` label for this subagent (see
    /// [`subagent_label`]) — the key the frontend's status-bar chips resolve
    /// forwarded signals by.
    label: String,
    /// The most recent coarse activity signal the relay emitted for this
    /// subagent (post-dedup), or `None` before the first signal. Read by
    /// [`Coordinator::subagent_activity_snapshot`] so a reattaching session
    /// stream can be brought up to date.
    latest_activity: Option<SubagentActivity>,
    /// Receiver for the final TaskResult — taken by `dispatch()` to await completion.
    result_rx: Option<oneshot::Receiver<TaskResult>>,
    /// Stored result for background tasks — set by `run_subagent` on completion
    /// so the completion notification system can access the result.
    completed_result: Option<TaskResult>,
    /// Receiver for the structured `AlmsError` produced by the subagent
    /// (when it failed) — taken by `dispatch()` so it can return the
    /// typed variant unchanged instead of stringifying-and-rewrapping
    /// `task_result.result["error"]` as `AlmsError::Runtime(...)`.
    /// `None` for completed / cancelled / timed-out runs. Issue #920.
    error_rx: Option<oneshot::Receiver<AlmsError>>,
}

/// Coordinator manages subagent lifecycle in a pure hierarchy.
///
/// Any agent can spawn subagents by calling `dispatch()`. Named subagents
/// must be pre-registered in the agent registry (`alms agent create`);
/// ephemeral (unnamed) subagents use default config.
/// Peer-to-peer messaging is provided independently by the message bus.
#[derive(Debug)]
pub struct Coordinator {
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
    /// Live server-default LLM client — cloned for each subagent runtime
    /// at spawn time.
    ///
    /// Behind an `Arc<RwLock>` (rather than held by value) so the
    /// gateway can share the *same* handle it hands to `AppState` and
    /// `PATCH /settings` can rebuild the server-default `(model,
    /// provider)` pair in place (#1148). Without the shared handle a
    /// live default switch would reach the parent run but not the
    /// subagents it spawns, splitting one run tree across two models.
    /// Constructors that take an `LlmClient` by value wrap it in a
    /// private handle nobody else can rebuild;
    /// [`Coordinator::with_agent_config_and_shared_llm`] takes the
    /// gateway's shared one instead.
    llm: Arc<parking_lot::RwLock<LlmClient>>,
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
    /// Optional sink (#1180) that mints a per-subagent [`EventForwarder`] bound
    /// to the subagent's OWN session — when set, each spawned subagent
    /// (foreground or background) mirrors its untagged run events to its own
    /// session's SSE event log so the fullscreen subagent-session view streams
    /// live and is replayable. The gateway provides the implementation.
    subagent_self_sink: Option<Arc<dyn SubagentSelfEventSink>>,
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
    pub fn new(session_manager: Arc<SessionManager>, llm: LlmClient) -> Self {
        Self {
            subagents: Arc::new(DashMap::new()),
            active_named: Arc::new(dashmap::DashSet::new()),
            session_manager,
            llm: Arc::new(parking_lot::RwLock::new(llm)),
            base_agent_config: Arc::new(parking_lot::RwLock::new(AgentConfig::default())),
            workspace_dir: None,
            data_dir: None,
            project_root: None,
            subagent_prompts: Arc::new(DashMap::new()),
            completion_tx: None,
            secrets: None,
            run_registrar: None,
            subagent_self_sink: None,
            security_config: alms_core::config::SecurityConfig::default(),
        }
    }

    /// Create a coordinator that inherits sandbox settings from the given config.
    ///
    /// The `Arc<RwLock<AgentConfig>>` is shared with the gateway's `AppState`
    /// so that PATCH /settings updates are visible to subsequently-spawned
    /// subagents without restarting the server.
    pub fn with_agent_config(
        session_manager: Arc<SessionManager>,
        llm: LlmClient,
        base_agent_config: Arc<parking_lot::RwLock<AgentConfig>>,
    ) -> Self {
        Self::with_agent_config_and_shared_llm(
            session_manager,
            Arc::new(parking_lot::RwLock::new(llm)),
            base_agent_config,
        )
    }

    /// Create a coordinator that shares BOTH live server-default handles
    /// with the gateway's `AppState` (#1148).
    ///
    /// `PATCH /settings` rebuilds the `LlmClient` behind `llm` in place
    /// whenever the server-default `(model, provider)` pair changes, and
    /// rewrites `base_agent_config` for the `context` / `session` /
    /// `tools` / `llm` sections. Taking both as shared handles is what
    /// keeps a parent run and the subagents it spawns on the same
    /// server-default layer: `AppState` reads its own copy of the very
    /// same `Arc`s, so a live switch cannot reach one and miss the other.
    ///
    /// Passing an owned client instead (see
    /// [`Coordinator::with_agent_config`]) is correct for callers with no
    /// gateway to share with — the coordinator then owns a handle nobody
    /// else can rebuild.
    pub fn with_agent_config_and_shared_llm(
        session_manager: Arc<SessionManager>,
        llm: Arc<parking_lot::RwLock<LlmClient>>,
        base_agent_config: Arc<parking_lot::RwLock<AgentConfig>>,
    ) -> Self {
        Self {
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
            subagent_self_sink: None,
            security_config: alms_core::config::SecurityConfig::default(),
        }
    }

    /// Snapshot the server-default client the way `spawn_subagent` does.
    ///
    /// Test-only mirror of the single production read site so a test can
    /// assert the shared-handle contract from #1148 without driving a
    /// full subagent spawn.
    #[cfg(test)]
    fn llm_snapshot(&self) -> LlmClient {
        self.llm.read().clone()
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

    /// Set a subagent self-event sink (#1180) so each spawned subagent mirrors
    /// its own (untagged) run events to its own session's SSE event log. This
    /// powers the fullscreen subagent-session live view (foreground and
    /// background alike). When unset, subagent events still reach the parent's
    /// stream as before; only the subagent's own-session live SSE is skipped.
    pub fn with_subagent_self_sink(mut self, sink: Arc<dyn SubagentSelfEventSink>) -> Self {
        self.subagent_self_sink = Some(sink);
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
        fields(parent_session = %request.parent_session.0)
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

        // Derive the subagent's cancellation token HERE (not inside
        // `run_subagent`) so it can be stored on the handle before the
        // subagent task starts: `cancel_subagent_by_session` must be able
        // to cancel a subagent that is still in its Pending window. When a
        // parent run token exists this is a child token, so the parent-run
        // cancel cascade is preserved exactly; a user cancel via the handle
        // fires the same token the `select!` in `run_subagent` waits on.
        let child_cancel_token = parent_cancel_token
            .as_ref()
            .map(|p| p.child_token())
            .unwrap_or_default();

        // The handle is created and inserted BEFORE `subagent_started` is
        // emitted below (Tim S2, PR #1192): the moment the UI learns the
        // subagent's session id, a session-keyed cancel must find a live
        // handle — emitting first would open a window where an immediate
        // cancel click 404s spuriously because the handle isn't in the map
        // yet.
        let handle = SubagentHandle {
            task_id,
            status: TaskStatus::Pending,
            cancel_token: child_cancel_token.clone(),
            parent_run_id,
            parent_session_id,
            parent_agent_id,
            subagent_session_id: sub_session_id,
            is_background,
            label: subagent_label(task_id, request.subagent_name.as_deref()),
            latest_activity: None,
            result_rx: Some(result_rx),
            completed_result: None,
            error_rx: Some(error_rx),
        };

        self.subagents.insert(task_id, handle);

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
        //      FIFO of `runtime_tx`, and AFTER the cancellable handle
        //      was inserted into `subagents` above (Tim S2, PR #1192 —
        //      "UI knows the session id ⇒ handle exists");
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

        info!(
            target: "coordinator::subagent_spawned",
            task_id = %task_id.0,
            parent_session = %request.parent_session.0,
            "Subagent spawned"
        );

        let subagents = self.subagents.clone();
        let active_named = self.active_named.clone();
        let session_manager = self.session_manager.clone();
        // Snapshot the live server-default client under the lock so a
        // `PATCH /settings` model/provider switch (#1148) is reflected in
        // subsequently-spawned subagents, exactly like `base_agent_config`
        // below.
        let llm = self.llm.read().clone();
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
        let subagent_self_sink = self.subagent_self_sink.clone();
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
                    child_cancel_token,
                    secrets,
                    run_registrar,
                    subagent_self_sink,
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

    /// The current coarse activity of every IN-FLIGHT subagent whose parent
    /// session is `parent_session` (#1189 follow-up).
    ///
    /// The live `subagent_activity` SSE signal is ephemeral and deduplicated
    /// to one emission per activity transition, so it only reaches the
    /// session-stream subscribers attached at the instant of the transition.
    /// The gateway calls this on every NEW session-stream attach and replays
    /// the snapshot as synthetic `subagent_activity` events, so a client that
    /// reattaches mid-phase (page reload, session switch back from the
    /// subagent view, second tab, SSE reconnect) sees the subagent's current
    /// status instead of a chip stuck on "Starting…" until the next
    /// transition.
    ///
    /// Only Pending/Running subagents with at least one recorded signal are
    /// returned — completed ones already surfaced their terminal status via
    /// `subagent_completed`, and a subagent that has not emitted yet is
    /// legitimately "Starting…".
    pub fn subagent_activity_snapshot(
        &self,
        parent_session: SessionId,
    ) -> Vec<SubagentActivitySnapshot> {
        self.subagents
            .iter()
            .filter(|h| {
                h.parent_session_id == parent_session
                    && matches!(h.status, TaskStatus::Pending | TaskStatus::Running)
            })
            .filter_map(|h| {
                h.latest_activity
                    .as_ref()
                    .map(|activity| SubagentActivitySnapshot {
                        label: h.label.clone(),
                        kind: activity.kind.clone(),
                        tool: activity.tool.clone(),
                        tool_invocation_id: activity.tool_invocation_id,
                        parent_tool_invocation_id: activity.parent_tool_invocation_id,
                        parent_run_id: h.parent_run_id,
                    })
            })
            .collect()
    }

    /// Cancel the live subagent running on the given SUBAGENT session.
    ///
    /// Session-keyed cancel surface for the gateway's
    /// `POST /sessions/{session_id}/subagent/cancel` endpoint: the UI's
    /// subagent chips / subagent-session view know the subagent's session id
    /// (it is carried by `subagent_started`, the `invoke_agent` result and
    /// the reload-rehydration path) but not its run id, and the subagent's
    /// own run id has no entry in the gateway's `cancel_tokens` map — so
    /// run-keyed `POST /runs/{id}/cancel` cannot cancel a subagent directly.
    ///
    /// Fires the handle's `cancel_token` — the same child
    /// token `run_subagent`'s `select!` waits on — so cancellation flows
    /// through the exact same path as a parent-run cancel cascade: the
    /// handle stays in the map and `run_subagent`'s terminal arm performs
    /// ALL the usual bookkeeping (status → `Cancelled`, run record update
    /// via the registrar, `run_cancelled` on the subagent's own session
    /// stream, and — for background subagents — the `subagent_completed`
    /// completion notification that renders the parent's chip as
    /// *Cancelled*).
    ///
    /// Only Pending/Running handles match: a session id is reused across
    /// invocations of a NAMED subagent, but at most one live invocation per
    /// session exists at a time (`active_named` rejects concurrent
    /// same-named spawns; unnamed subagents get a fresh session per
    /// dispatch), so the first live match is the only live match.
    ///
    /// Returns `true` when a live subagent was found and its token fired;
    /// `false` when no live subagent exists for that session (unknown
    /// session, or the subagent already reached a terminal state). Callers
    /// map `false` to an HTTP 404.
    pub fn cancel_subagent_by_session(&self, subagent_session_id: SessionId) -> bool {
        for entry in self.subagents.iter() {
            let h = entry.value();
            if h.subagent_session_id == subagent_session_id
                && matches!(h.status, TaskStatus::Pending | TaskStatus::Running)
            {
                h.cancel_token.cancel();
                info!(
                    target: "coordinator::subagent_cancelled_by_session",
                    task_id = %h.task_id.0,
                    subagent_session = %subagent_session_id.0,
                    "Subagent cancellation requested by session id"
                );
                return true;
            }
        }
        false
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

/// Drop-armed completion guard for background subagents (#1198 step 7).
///
/// `run_subagent` emits exactly one [`SubagentCompletion`] on every
/// *non-panic* exit path (Completed / Failed / Cancelled — see the emission
/// block near the end of the function). A panic anywhere before that
/// emission point unwinds the spawned task WITHOUT a completion. Pre-#1198
/// that stranded the parent's "running" chip forever; under the #1198
/// job-episode model it would additionally stall a job episode's pending
/// `Subagent` entry until the episode's 4-hour deadline. This guard closes
/// the hole: it is armed at the top of `run_subagent` for background tasks
/// and disarmed immediately after the normal emission block, so its `Drop`
/// fires the fallback `Failed` completion only on an unwind.
struct BackgroundCompletionGuard {
    inner: Option<(
        mpsc::UnboundedSender<SubagentCompletion>,
        Box<SubagentCompletion>,
    )>,
}

impl BackgroundCompletionGuard {
    /// Arm the guard with the minimal fallback completion. A `None` tx
    /// produces a disarmed guard whose `Drop` is a no-op (mirrors the
    /// `completion_tx` optionality on the normal emission path).
    fn armed(
        tx: Option<mpsc::UnboundedSender<SubagentCompletion>>,
        completion: SubagentCompletion,
    ) -> Self {
        Self {
            inner: tx.map(|tx| (tx, Box::new(completion))),
        }
    }

    /// A guard that never fires (foreground tasks).
    fn disarmed() -> Self {
        Self { inner: None }
    }

    /// Normal exit reached — the real completion (when applicable) was
    /// emitted; make `Drop` a no-op.
    fn disarm(&mut self) {
        self.inner = None;
    }
}

impl Drop for BackgroundCompletionGuard {
    fn drop(&mut self) {
        if let Some((tx, completion)) = self.inner.take() {
            tracing::warn!(
                task_id = %completion.task_id.0,
                "run_subagent unwound without emitting a completion — \
                 sending panic-fallback Failed completion (#1198 step 7)"
            );
            let _ = tx.send(*completion);
        }
    }
}

// ---------------------------------------------------------------------------
// Internal subagent runner
// ---------------------------------------------------------------------------

/// Classify an agent-loop error from `run_subagent`'s run branch into the
/// task status it must surface as.
///
/// `AlmsError::Cancelled` / `AlmsError::CancelledWithToolCalls` are produced
/// exclusively by the loop observing ITS OWN `CancellationToken` at a
/// checkpoint (`loop_impl.rs` — iteration boundary, LLM call, tool
/// execution, approval wait; `agent/mod.rs` wraps the partial-tool-calls
/// variant) — and the token the subagent's loop holds is exactly
/// `child_cancel_token`. So a loop error of these variants IS a cancel of
/// this subagent and must be labelled `TaskStatus::Cancelled`, identically
/// to the `select!`'s token arm. This makes the cancel labelling
/// independent of which arm observes the fired token first (Tim, PR #1192
/// review — the `biased` select alone left a vanishingly-narrow
/// multi-threaded window where the run branch could win with
/// `Err(Cancelled)` and mislabel the user cancel as `Failed`).
///
/// Every other error is a genuine failure.
fn subagent_error_status(e: &AlmsError) -> TaskStatus {
    match e {
        AlmsError::Cancelled | AlmsError::CancelledWithToolCalls { .. } => TaskStatus::Cancelled,
        _ => TaskStatus::Failed,
    }
}

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
    // The subagent's own cancellation token, derived by `spawn_subagent`
    // (child of the parent run's token when present) and shared with the
    // `SubagentHandle` so `cancel_subagent_by_session` can fire it.
    child_cancel_token: CancellationToken,
    secrets: Option<Arc<parking_lot::RwLock<alms_core::secrets::SecretsStore>>>,
    run_registrar: Option<Arc<dyn RunRegistrar>>,
    subagent_self_sink: Option<Arc<dyn SubagentSelfEventSink>>,
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

    // #1198 step 7: arm the panic-completion guard for background tasks.
    // Foreground callers get their result via the `result_tx` oneshot and do
    // not consume the completion channel, so the guard stays disarmed there.
    // The fallback carries the same identity fields as the normal emission
    // (task_id / parent ids / invocation id) so the gateway's completion
    // loop and any #1198 episode resolve can correlate it identically.
    let mut completion_guard = if is_background {
        let task_desc = {
            let truncated =
                truncate_to_char_boundary(&request.task, NOTIFICATION_SUMMARY_MAX_CHARS);
            if truncated.len() == request.task.len() {
                request.task.clone()
            } else {
                format!("{}…[truncated]", truncated)
            }
        };
        BackgroundCompletionGuard::armed(
            completion_tx.clone(),
            SubagentCompletion {
                task_id,
                subagent_name: request.subagent_name.clone(),
                status: TaskStatus::Failed,
                summary: "subagent task panicked before emitting a completion".to_string(),
                parent_session_id: request.parent_session,
                parent_agent_id: request.parent_agent_id,
                subagent_session_id: sub_session_id,
                task_description: Some(task_desc),
                tool_count: None,
                duration_ms: None,
                token_usage: None,
                parent_tool_invocation_id: request.parent_tool_invocation_id,
            },
        )
    } else {
        BackgroundCompletionGuard::disarmed()
    };

    if let Some(mut handle) = subagents.get_mut(&task_id) {
        handle.status = TaskStatus::Running;
    }

    info!(
        target: "subagent::started",
        task_id = %task_id.0,
        task = %request.task,
        "Subagent execution started"
    );

    // `child_cancel_token` (derived in `spawn_subagent`, shared with the
    // SubagentHandle) fires when either:
    //   1. The parent run's CancellationToken is cancelled (it is a child
    //      token of the parent's when one exists), or
    //   2. `cancel_subagent_by_session()` or the test-only `cancel_subagent()`
    //      fires the handle's stored clone.
    // The same token is attached to the subagent's AgentRuntime.

    // Identity and session are already resolved by `spawn_subagent` and
    // passed in as parameters (#1075). Re-deriving here would mint a fresh
    // `AgentId::new()` for ephemeral subagents and create a duplicate row.

    // Register the subagent run with the RunRegistrar (if available) so it
    // appears in GET /runs, the UI sidebar, and CLI `alms run list`.
    let (subagent_run, registration_error) = if let Some(ref registrar) = run_registrar {
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
        match registrar.register_run(run.clone()).await {
            Ok(()) => (Some(run), None),
            Err(error) => {
                let message = format!("Failed to persist subagent run registration: {error}");
                tracing::error!(
                    target: "subagent::error",
                    task_id = %task_id.0,
                    error = %error,
                    "Subagent run registration failed"
                );
                (None, Some(AlmsError::Runtime(message)))
            }
        }
    } else {
        (None, None)
    };

    // #1180: build a forwarder bound to the subagent's OWN session so its
    // (untagged) run events stream to that session's SSE event log — powering
    // the fullscreen subagent-session live view, foreground and background
    // alike. Uses the subagent's registered run id when present (so
    // `GET /runs/{id}/events` lines up) and a fresh id otherwise. `None` when no
    // sink is wired (tests / non-gateway callers), leaving the parent-stream
    // forward unchanged.
    let self_event_fwd = subagent_self_sink.as_ref().map(|sink| {
        let self_run_id = subagent_run
            .as_ref()
            .map(|r| r.run_id)
            .unwrap_or_else(RunId::new);
        sink.forwarder_for(sub_session_id, self_run_id)
    });
    // Keep a clone so we can emit the terminal event on the subagent's OWN
    // session once `new_status` is known (#1180). `self_event_fwd` itself is
    // moved into `run_agent_loop` for the content relay; the sink's two-channel
    // drain orders the terminal after all content regardless of call timing.
    let self_event_fwd_for_terminal = self_event_fwd.clone();

    // The select returns the task status, a JSON result value (for the
    // TaskResult / completion notification), and optionally the full
    // RunOutput so we can record accurate token usage in the run record.
    //
    // #1150: the implicit 5-minute wall-clock kill arm
    // (`tokio::time::sleep(request.timeout)`) was removed. It killed
    // legitimately long subagents mid-work — especially during reasoning
    // phases — with a generic `Timeout`. A subagent now terminates the same
    // way a top-level run does: via the inherited in-loop phase-aware
    // inactivity timer (#1150) + `max_iterations`, which surface as a normal
    // `Err` on the completion arm, or via cancellation. Only the cancel and
    // completion arms remain.
    //
    // #920: the completion arm also returns the structured `AlmsError` (when
    // the agent loop produced a FAILURE) so the caller can forward it down
    // the typed-error oneshot. A cancellation has no typed error and uses
    // `None` — `dispatch()` falls back to the JSON path for it — regardless
    // of which arm observed it (the token arm, or the run branch returning
    // the loop's own `Err(Cancelled)` / `Err(CancelledWithToolCalls)` from
    // a checkpoint, which `subagent_error_status` classifies as `Cancelled`
    // rather than `Failed` so the labelling is poll-order-independent).
    let (mut new_status, mut result_value, tokens_used, run_output, mut typed_error) = if let Some(
        error,
    ) =
        registration_error
    {
        let message = error.to_string();
        (
            TaskStatus::Failed,
            serde_json::json!({"error": message}),
            None,
            None,
            Some(error),
        )
    } else {
        tokio::select! {
            // `biased`: poll the cancellation arm FIRST on every wake instead of
            // in random order, so a cancel requested before (or at) the poll
            // takes the cheap token arm without running the loop at all. This
            // is a fast path, not the correctness mechanism: even when the run
            // branch wins the both-ready race (or the loop observes the token
            // at one of its own checkpoints mid-run), the `Err` handler below
            // classifies the loop's `Cancelled` / `CancelledWithToolCalls`
            // errors as `TaskStatus::Cancelled`, so a user cancel is never
            // mislabelled as a failure in either order.
            biased;
            _ = child_cancel_token.cancelled() => {
                info!(
                    target: "subagent::cancelled",
                    task_id = %task_id.0,
                    "Subagent cancelled"
                );
                (TaskStatus::Cancelled, serde_json::json!({"cancelled": true}), None, None, None)
            }
            output = run_agent_loop(task_id, &request, sub_agent_id, &sub_context_id, &session_manager, &llm, parent_event_tx, self_event_fwd, &base_agent_config, workspace_dir.as_deref(), data_dir.as_deref(), project_root.as_deref(), &subagent_prompts, child_cancel_token.clone(), secrets.as_ref(), &security_config, is_background, subagents.clone()) => {
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
                    Err(e) if subagent_error_status(&e) == TaskStatus::Cancelled => {
                        // The loop observed the fired token at one of its own
                        // checkpoints before the token arm above was polled.
                        // Same outcome shape as the token arm: this is a user
                        // cancel, not a failure — no typed error (matching the
                        // token arm's contract; `dispatch()` maps Cancelled to
                        // its dedicated "Subagent was cancelled" error without
                        // consulting the typed channel).
                        info!(
                            target: "subagent::cancelled",
                            task_id = %task_id.0,
                            "Subagent cancelled (observed at a loop checkpoint)"
                        );
                        (
                            TaskStatus::Cancelled,
                            serde_json::json!({"cancelled": true}),
                            None,
                            None,
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
        }
    };

    // Release any remaining waiters after the subagent reaches a terminal state.
    child_cancel_token.cancel();

    // Commit the coordinator-owned run snapshot before publishing terminal
    // events or completion notifications. A persistence failure is a task
    // failure, not a successful lifecycle transition.
    if let (Some(registrar), Some(mut run)) = (&run_registrar, subagent_run) {
        match new_status {
            TaskStatus::Completed => {
                if let Some(ref output) = run_output {
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
                let _ = run.mark_failed(error);
            }
            TaskStatus::Cancelled => {
                let _ = run.mark_cancelled();
            }
            _ => {}
        }
        if let Err(error) = registrar.update_run(run) {
            tracing::error!(
                target: "subagent::error",
                task_id = %task_id.0,
                error = %error,
                "Failed to persist subagent lifecycle transition"
            );
            let message = format!("Failed to persist subagent lifecycle transition: {error}");
            new_status = TaskStatus::Failed;
            result_value = serde_json::json!({ "error": message });
            typed_error = Some(AlmsError::Runtime(message));
        }
    }

    // #1180: seal the subagent's OWN session — emit its terminal SSE
    // (run_finished / run_error / run_cancelled) and evict its text buffer via
    // the self-sink. Only the subagent's own session: the parent already gets
    // `subagent_completed` via the relay, and the #1046 guard keeps the
    // coordinator silent on the parent path (no double-broadcast). The sink's
    // two-channel drain orders this after all of the subagent's content.
    if let Some(self_fwd) = self_event_fwd_for_terminal {
        let outcome = match new_status {
            TaskStatus::Completed => alms_tools::SubagentRunOutcome::Completed {
                usage: run_output
                    .as_ref()
                    .map(|o| alms_core::TokenUsage {
                        prompt_tokens: o.usage.prompt_tokens,
                        completion_tokens: o.usage.completion_tokens,
                        reasoning_tokens: o.usage.reasoning_tokens,
                        cache_creation_input_tokens: o.usage.cache_creation_input_tokens,
                        cache_read_input_tokens: o.usage.cache_read_input_tokens,
                    })
                    .unwrap_or_default(),
            },
            TaskStatus::Failed => alms_tools::SubagentRunOutcome::Failed {
                error: result_value["error"]
                    .as_str()
                    .unwrap_or("unknown error")
                    .to_string(),
            },
            _ => alms_tools::SubagentRunOutcome::Cancelled,
        };
        self_fwd.forward_run_terminal(outcome);
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

    // #1198 step 7: normal exit reached — the completion (when applicable)
    // was emitted above, so the panic guard must not fire on the drop at
    // function end. Disarmed unconditionally: a background task whose handle
    // vanished (background_info == None) intentionally emitted nothing, and
    // the guard must not turn that into a spurious Failed completion.
    completion_guard.disarm();

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
    // This post-completion retention window is independent of the run
    // lifetime — the run-kill timer that used to share this constant was
    // removed in #1150.
    tokio::time::sleep(Duration::from_secs(RESULT_RETENTION_SECS)).await;
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
        // Agent-loop hard caps (#987 / B3 / #1150) — inherited verbatim so a
        // subagent that wedges in a tool loop terminates on the same caps its
        // parent would: iteration cap, absolute wall-clock backstop, and the
        // phase-aware inactivity budgets. As of #1150 this in-loop phase timer
        // (not the coordinator's old 5-minute wall-clock kill, now removed) is
        // what bounds a subagent run.
        max_iterations: base.max_iterations,
        max_run_duration_secs: base.max_run_duration_secs,
        between_iterations_secs: base.between_iterations_secs,
        tool_phase_ceiling_secs: base.tool_phase_ceiling_secs,
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
///
/// Ephemeral (unnamed) subagent contexts embed the parent agent id too —
/// `subagent_{parent_agent_id}_{task_id}` — so `read_subagent_session`'s
/// by-`session_id` readback (#1181) can enforce parent ownership from the
/// context alone, exactly like the named shape. Pre-hardening the ephemeral
/// context was `subagent_{task_id}`, which forced the readback to treat the
/// session UUID as a bearer capability — but that UUID leaks beyond the
/// spawning parent (parent-visible `invoke_agent` result / completion text,
/// and the shared DM `parent_session` for DM-triggered invocations), so any
/// agent that learned it could read the transcript (Tim / Codex on PR #1185).
/// The context format is coordinator-reserved — see
/// `docs/security-model.md` § subagent session readback.
fn derive_subagent_identity(task_id: TaskId, request: &SubagentRequest) -> (AgentId, String) {
    if let Some(ref name) = request.subagent_name {
        let stable_id = AgentId::deterministic(request.parent_agent_id, name);
        let stable_ctx = format!("subagent_{}_{}", request.parent_agent_id.0, name);
        (stable_id, stable_ctx)
    } else {
        (
            AgentId::new(),
            format!("subagent_{}_{}", request.parent_agent_id.0, task_id.0),
        )
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
    self_event_tx: Option<Arc<dyn EventForwarder>>,
    base_agent_config: &AgentConfig,
    workspace_dir: Option<&std::path::Path>,
    data_dir: Option<&std::path::Path>,
    project_root: Option<&std::path::Path>,
    subagent_prompts: &DashMap<String, String>,
    cancel_token: CancellationToken,
    secrets: Option<&Arc<parking_lot::RwLock<alms_core::secrets::SecretsStore>>>,
    security_config: &alms_core::config::SecurityConfig,
    is_background: bool,
    // Shared handle map — the relay records each emitted (post-dedup)
    // activity signal on this subagent's handle so
    // `Coordinator::subagent_activity_snapshot` can replay the CURRENT status
    // to session streams that attach after the signal fired (#1189 follow-up).
    subagents: Arc<DashMap<TaskId, SubagentHandle>>,
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
            // #1191 made `Some(openrouter, gemma)` the compiled default,
            // so this branch now runs on every stock deployment's subagent
            // spawns — the default pair is routine and logs at `debug!`;
            // only an operator-configured pair stays `info!` (PR #1194,
            // mirroring the gateway run path in `runs/lifecycle.rs`).
            if alms_core::config::ContextConfig::is_compiled_default_summary_pair(
                Some(provider),
                subagent_summary_model.as_deref(),
            ) {
                debug!(
                    task_id = %task_id.0,
                    agent_provider = %subagent_llm.provider(),
                    summary_provider = %provider,
                    summary_model = %subagent_summary_model.as_deref().unwrap_or("<inherit>"),
                    "Subagent inheriting parent summary_provider config (#871, compiled default pair)"
                );
            } else {
                info!(
                    task_id = %task_id.0,
                    agent_provider = %subagent_llm.provider(),
                    summary_provider = %provider,
                    summary_model = %subagent_summary_model.as_deref().unwrap_or("<inherit>"),
                    "Subagent inheriting parent summary_provider config (#871)"
                );
            }
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

    // Forward subagent events to two independent sinks: an UNTAGGED copy of the
    // full content to the subagent's OWN session log (#1180 — powers the
    // fullscreen subagent view) and a TAGGED (`source_agent`) coarse STATUS
    // signal to the parent stream (the UI's Subagent status bar).
    //
    // The parent deliberately does NOT receive the subagent's reasoning/token
    // text or tool params/results anymore: the bar only renders a status label
    // ("Reasoning…", "Using {tool}", "Writing…"), and forwarding the full
    // content bloated the parent's stream — and, worse, its persisted session
    // event log — for no visible benefit (token efficiency is a first-class
    // concern). The full content belongs on the subagent's own session, where
    // the fullscreen view subscribes (#1184). This also closes #1186 by
    // construction: the bar renders no reasoning text, so a buffered-fallback
    // re-emit has no painted partial to duplicate and the subagent's
    // `StreamReset` needs no parent-side retraction.
    if parent_event_tx.is_some() || self_event_tx.is_some() {
        let label = subagent_label(task_id, request.subagent_name.as_deref());
        // Chip-resolution correlator (#1190 Codex P2): the parent's
        // invoke_agent invocation id, attached to every status signal (and
        // the recorded snapshot) so the UI resolves the target chip
        // identity-exactly — the task-derived label alone cross-attaches
        // status between concurrent unnamed subagents.
        let parent_inv = request.parent_tool_invocation_id;
        let parent_fwd = parent_event_tx;
        let self_fwd = self_event_tx;
        let relay_subagents = subagents.clone();
        tokio::spawn(async move {
            use alms_runtime::RuntimeEvent;
            let mut rx = sub_rx;
            // Dedup state for `forward_status_to_parent` — the last activity
            // kind forwarded to the parent. Per-subagent by construction: this
            // task is the only consumer of this subagent's channel.
            let mut last_activity_kind: Option<&'static str> = None;
            while let Some(event) = rx.recv().await {
                // Untagged full-content copy to the subagent's own session.
                // Borrows `event` so the match below can still move it
                // (RuntimeEvent isn't Clone — ApprovalRequired holds a oneshot).
                if let Some(self_fwd) = self_fwd.as_deref() {
                    forward_event_to_self(self_fwd, &event);
                }

                // Tagged status signal to the parent stream; skipped when no
                // parent sink.
                let Some(parent_fwd) = parent_fwd.as_ref() else {
                    continue;
                };
                match event {
                    // ApprovalRequired cannot be forwarded through EventForwarder
                    // (it requires a oneshot channel).  Background subagents
                    // should already have Guarded overridden to Autonomous, so
                    // this path is a fallback for FullControl subagents (which
                    // intentionally keep their posture).  Auto-deny the tool
                    // call immediately so the subagent doesn't hang. Handled
                    // here (not in the helper) because sending on the oneshot
                    // consumes `decision_tx`, which requires moving the event.
                    RuntimeEvent::ApprovalRequired {
                        tool, decision_tx, ..
                    } => {
                        warn!(
                            tool = %tool,
                            "Subagent requested approval — auto-denying (approval not routable)"
                        );
                        let _ = decision_tx.send(false);
                    }
                    // Everything else reduces to (at most) a deduped
                    // `subagent_activity` status signal or a tagged warning —
                    // see `forward_status_to_parent` for the full mapping and
                    // the suppression rationale per variant. The record
                    // callback stores the signal on the handle so a session
                    // stream that attaches AFTER this (deduped, ephemeral)
                    // emission can still learn the subagent's current status
                    // via `subagent_activity_snapshot` (#1189 follow-up).
                    // Once per transition — same rate as the signal itself.
                    other => forward_status_to_parent(
                        parent_fwd.as_ref(),
                        &other,
                        &label,
                        parent_inv,
                        &mut last_activity_kind,
                        &mut |activity| {
                            if let Some(mut handle) = relay_subagents.get_mut(&task_id) {
                                handle.latest_activity = Some(activity.clone());
                            }
                        },
                    ),
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

/// Mirror a subagent's own runtime event UNTAGGED (`source_agent = None`) onto
/// its own session's forwarder (#1180) — on a session's OWN stream the frontend
/// renders untagged events as that session's main agent. Borrows `event` so the
/// caller can still move it into the tagged parent forward (`RuntimeEvent` isn't
/// `Clone`). `StreamReset` IS mirrored (unlike the parent relay, which drops it
/// after suppressing subagent deltas): the self stream paints partials, so the
/// buffered-fallback re-emit must be able to retract them (#1162 sym-2).
/// Approval / context-debug / nested subagent-started are skipped.
fn forward_event_to_self(self_fwd: &dyn EventForwarder, event: &alms_runtime::RuntimeEvent) {
    use alms_runtime::RuntimeEvent;
    match event {
        RuntimeEvent::ToolStart {
            invocation_id,
            tool,
            params,
            ..
        } => {
            self_fwd.forward_tool_start(*invocation_id, tool.clone(), params.clone(), None, None);
        }
        RuntimeEvent::ToolEnd {
            invocation_id,
            ok,
            result,
            ..
        } => {
            self_fwd.forward_tool_end(*invocation_id, *ok, result.clone(), None, None);
        }
        RuntimeEvent::TokenDelta { delta, .. } => {
            self_fwd.forward_token_delta(delta.clone(), None);
        }
        RuntimeEvent::ReasoningDelta { text, .. } => {
            self_fwd.forward_reasoning_delta(text.clone(), None);
        }
        RuntimeEvent::Status { phase, detail } => {
            self_fwd.forward_status(phase.clone(), detail.clone());
        }
        RuntimeEvent::Warning { code, message, .. } => {
            self_fwd.forward_warning(code.clone(), message.clone(), None);
        }
        RuntimeEvent::StreamReset { .. } => self_fwd.forward_stream_reset(),
        _ => {}
    }
}

/// Reduce a subagent's runtime event to (at most) one TAGGED status signal for
/// the parent's **Subagent status bar**.
///
/// This is the parent half of the subagent→parent relay. The bar renders a
/// concise status label per subagent ("Reasoning…", "Using {tool}",
/// "Writing…"), so the parent only needs to know *what kind* of activity is
/// happening — never the reasoning/token TEXT or tool params/results. Those
/// used to be forwarded verbatim (and the reasoning deltas + tool events were
/// PERSISTED to the parent's session event log), bloating the parent stream
/// with content the UI deliberately hid; the full content streams to the
/// subagent's own session instead (#1184).
///
/// Mapping:
/// - `ToolStart`      → `subagent_activity(kind=tool_start, tool=<name>)`
/// - `ToolEnd`        → `subagent_activity(kind=tool_end)`
/// - `ReasoningDelta` → `subagent_activity(kind=reasoning)` (deduped)
/// - `TokenDelta`     → `subagent_activity(kind=writing)` (deduped)
/// - `Warning`        → `forward_warning(..)` tagged with the subagent label
///   (unchanged from the pre-status-bar relay)
/// - everything else  → suppressed:
///   * `Status` — would overwrite the parent's thinking indicator with the
///     subagent's phase; the operator doesn't need the subagent's
///     "building context" on the parent surface.
///   * `StreamReset` — retracts a painted partial (#1162 sym-2), but the
///     parent paints no subagent content at all now, so there is nothing to
///     retract (this is what closes #1186). The subagent's OWN stream mirrors
///     it via `forward_event_to_self`.
///   * `ContextDebug` — only meaningful for the top-level agent's context.
///   * `SubagentStarted` — a (future) sub-subagent's chip belongs on the
///     subagent's own session stream; the coordinator already emits the
///     `SubagentStarted` for THIS subagent from `spawn_subagent` (#1105).
///   * `ApprovalRequired` — handled (auto-denied) by the caller, which owns
///     the event by value; it can never reach this borrowing helper in
///     production, and ignoring it here keeps the helper total.
///
/// `parent_tool_invocation_id` (the parent invoke_agent invocation id, a
/// per-subagent constant) is attached to EVERY emitted signal and to the
/// recorded snapshot: it is the chip-resolution correlator the UI matches
/// identity-exactly (#1190 Codex P2) — without it, concurrent unnamed
/// subagents resolve by a first-match label fallback that can persistently
/// cross-attach one subagent's status to another's chip.
///
/// Dedup (`last_activity_kind`): reasoning/token deltas arrive per-chunk, at
/// high frequency. The bar only changes when the activity *kind* changes, so
/// consecutive deltas of the same kind collapse into a single signal — the
/// parent stream sees one event per phase transition instead of one per
/// chunk. Tool boundaries always emit (they carry / clear the tool name) and
/// also update the state, so a delta after a tool boundary re-emits its kind.
///
/// `record_before_emit` receives each activity signal this function is about
/// to emit (never suppressed / deduped / warning events), and is invoked
/// STRICTLY BEFORE the wire emission. The relay uses it to store the signal
/// on the `SubagentHandle` for [`Coordinator::subagent_activity_snapshot`]
/// (#1189 follow-up) — the live signal itself is ephemeral and fires at most
/// once per transition, so it alone cannot serve subscribers that attach
/// later. The record-then-emit ordering matters (Tim on #1190): if the handle
/// were updated AFTER the emission, a `subagent_activity_snapshot` read racing
/// a fresh emission could return the PREVIOUS kind while the live event had
/// already reached a newly-attached subscriber — the mirror of the reattach
/// bug, with no correcting re-emit until the next transition. Recording first
/// makes the snapshot at-least-as-new as anything on the wire.
fn forward_status_to_parent(
    parent_fwd: &dyn EventForwarder,
    event: &alms_runtime::RuntimeEvent,
    label: &str,
    parent_tool_invocation_id: Option<Uuid>,
    last_activity_kind: &mut Option<&'static str>,
    record_before_emit: &mut dyn FnMut(&SubagentActivity),
) {
    use alms_runtime::RuntimeEvent;
    use alms_tools::subagent_activity_kind as kind;

    let (activity_kind, tool, tool_invocation_id) = match event {
        RuntimeEvent::ToolStart {
            invocation_id,
            tool,
            ..
        } => (kind::TOOL_START, Some(tool.clone()), Some(*invocation_id)),
        RuntimeEvent::ToolEnd { invocation_id, .. } => (kind::TOOL_END, None, Some(*invocation_id)),
        RuntimeEvent::ReasoningDelta { .. } => (kind::REASONING, None, None),
        RuntimeEvent::TokenDelta { .. } => (kind::WRITING, None, None),
        RuntimeEvent::Warning { code, message, .. } => {
            parent_fwd.forward_warning(code.clone(), message.clone(), Some(label.to_string()));
            return;
        }
        _ => return,
    };

    // Collapse consecutive same-kind delta signals. Only the delta kinds are
    // deduped: tool boundaries are low-frequency and must always fire so the
    // bar picks up the (possibly repeated) tool name.
    if (activity_kind == kind::REASONING || activity_kind == kind::WRITING)
        && *last_activity_kind == Some(activity_kind)
    {
        return;
    }
    *last_activity_kind = Some(activity_kind);
    let activity = SubagentActivity {
        kind: activity_kind.to_string(),
        tool,
        tool_invocation_id,
        parent_tool_invocation_id,
    };
    // Record BEFORE emitting — see the doc comment above for why the order
    // is load-bearing.
    record_before_emit(&activity);
    parent_fwd.forward_subagent_activity(
        activity.kind.clone(),
        activity.tool.clone(),
        activity.tool_invocation_id,
        activity.parent_tool_invocation_id,
        label.to_string(),
    );
}

#[cfg(test)]
impl Coordinator {
    /// Get the completed result for a finished background task (test-only).
    pub fn get_completed_result(&self, task_id: TaskId) -> Option<TaskResult> {
        self.subagents.get(&task_id)?.completed_result.clone()
    }

    /// Get the latest recorded activity signal for a task (test-only) —
    /// recorded by the relay regardless of task status, unlike
    /// `subagent_activity_snapshot` which filters to in-flight tasks.
    pub fn latest_activity_for(&self, task_id: TaskId) -> Option<SubagentActivity> {
        self.subagents.get(&task_id)?.latest_activity.clone()
    }

    /// Cancel a running subagent (test-only).
    pub fn cancel_subagent(&self, task_id: TaskId) -> AlmsResult<()> {
        if let Some((_, handle)) = self.subagents.remove(&task_id) {
            handle.cancel_token.cancel();
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
        Coordinator::new(session_manager, llm)
    }

    // -- #1148: live server-default LLM handle -------------------------------

    /// A coordinator built with
    /// [`Coordinator::with_agent_config_and_shared_llm`] must read the
    /// shared client at *use* time, so a `PATCH /settings` rebuild the
    /// gateway performs after construction is visible to
    /// subsequently-spawned subagents.
    ///
    /// Pre-#1148 the coordinator held the client by value, so a live
    /// server-default switch reached the parent run (which reads
    /// `AppState`'s handle) but not the subagents it spawned — one run
    /// tree, two models, no operator-visible signal. `llm_snapshot()` is
    /// the test-only mirror of the single read site in `spawn_subagent`.
    #[test]
    fn shared_llm_handle_is_read_at_use_time_not_at_construction() {
        let session_manager = Arc::new(SessionManager::new(alms_session::SessionConfig::default()));
        let shared = Arc::new(parking_lot::RwLock::new(
            LlmClient::new(LlmConfig {
                mock: true,
                provider: "openrouter".into(),
                default_model: "z-ai/glm-5.2".into(),
                ..LlmConfig::default()
            })
            .unwrap(),
        ));
        let coord = Coordinator::with_agent_config_and_shared_llm(
            session_manager,
            Arc::clone(&shared),
            Arc::new(parking_lot::RwLock::new(AgentConfig::default())),
        );

        assert_eq!(
            coord.llm_snapshot().default_model(),
            "z-ai/glm-5.2",
            "baseline: the coordinator starts on the pair it was built with"
        );

        // Simulate the gateway's `AppState::refresh_llm_from_server_default`
        // rebuilding the shared client after a `PATCH /settings`.
        {
            let mut guard = shared.write();
            *guard = guard.clone().with_model("moonshotai/kimi-k2.5");
        }

        assert_eq!(
            coord.llm_snapshot().default_model(),
            "moonshotai/kimi-k2.5",
            "the coordinator must read the SHARED handle at use time — a \
             by-value copy taken at construction would still report the \
             boot model and split the run tree across two models"
        );
    }

    /// A coordinator built from an owned client keeps a private handle:
    /// mutating an unrelated one must not affect it. Pins that the sharing
    /// above comes from the explicit constructor rather than from some
    /// incidental aliasing.
    #[test]
    fn coordinator_built_from_an_owned_client_keeps_its_own_handle() {
        let session_manager = Arc::new(SessionManager::new(alms_session::SessionConfig::default()));
        let original = LlmClient::new(LlmConfig {
            mock: true,
            default_model: "z-ai/glm-5.2".into(),
            ..LlmConfig::default()
        })
        .unwrap();
        let unrelated = Arc::new(parking_lot::RwLock::new(original.clone()));
        let coord = Coordinator::new(session_manager, original);

        {
            let mut guard = unrelated.write();
            *guard = guard.clone().with_model("moonshotai/kimi-k2.5");
        }

        assert_eq!(
            coord.llm_snapshot().default_model(),
            "z-ai/glm-5.2",
            "a coordinator that was never handed the shared handle must keep \
             the client it was constructed with"
        );
    }

    fn test_session_id() -> SessionId {
        SessionId::new()
    }

    fn test_parent_agent_id() -> AgentId {
        AgentId::new()
    }

    // -- BackgroundCompletionGuard (#1198 step 7) -------------------------------

    fn guard_fallback_completion() -> SubagentCompletion {
        SubagentCompletion {
            task_id: TaskId::new(),
            subagent_name: Some("researcher".to_string()),
            status: TaskStatus::Failed,
            summary: "subagent task panicked before emitting a completion".to_string(),
            parent_session_id: SessionId::new(),
            parent_agent_id: AgentId::new(),
            subagent_session_id: SessionId::new(),
            task_description: Some("investigate the thing".to_string()),
            tool_count: None,
            duration_ms: None,
            token_usage: None,
            parent_tool_invocation_id: None,
        }
    }

    /// An armed guard that is dropped without `disarm()` (the panic-unwind
    /// shape) must emit exactly one `Failed` completion.
    #[test]
    fn completion_guard_emits_failed_on_undisarmed_drop() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let completion = guard_fallback_completion();
        let expected_task = completion.task_id;

        let guard = BackgroundCompletionGuard::armed(Some(tx), completion);
        drop(guard);

        let got = rx
            .try_recv()
            .expect("armed guard must emit a completion on drop");
        assert_eq!(got.status, TaskStatus::Failed);
        assert_eq!(got.task_id, expected_task);
        assert!(
            rx.try_recv().is_err(),
            "guard must emit exactly one completion"
        );
    }

    /// `disarm()` (the normal-exit path, called right after the real
    /// emission block in `run_subagent`) must suppress the fallback — no
    /// double completion on a healthy run.
    #[test]
    fn completion_guard_disarm_suppresses_emission() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut guard = BackgroundCompletionGuard::armed(Some(tx), guard_fallback_completion());
        guard.disarm();
        drop(guard);
        assert!(
            rx.try_recv().is_err(),
            "disarmed guard must not emit a completion"
        );
    }

    /// Foreground tasks (`disarmed()`) and a missing completion channel
    /// (`None` tx) both produce a no-op guard.
    #[test]
    fn completion_guard_disarmed_and_no_tx_are_noops() {
        drop(BackgroundCompletionGuard::disarmed());
        // None tx: armed() degrades to a no-op guard — must not panic.
        drop(BackgroundCompletionGuard::armed(
            None,
            guard_fallback_completion(),
        ));
    }

    /// The real failure shape this guard exists for: the task PANICS while
    /// the guard is armed. The unwind must still deliver the `Failed`
    /// completion to the receiver (this is what turns "job episode stalls
    /// until the 4h deadline" into "pending entry resolves in seconds").
    #[tokio::test]
    async fn completion_guard_fires_across_panic_unwind() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let completion = guard_fallback_completion();
        let expected_task = completion.task_id;

        let handle = tokio::spawn(async move {
            let _guard = BackgroundCompletionGuard::armed(Some(tx), completion);
            panic!("simulated run_subagent panic before the emission point");
        });
        assert!(handle.await.is_err(), "task must have panicked");

        let got = rx
            .recv()
            .await
            .expect("panic unwind must deliver the fallback completion");
        assert_eq!(got.status, TaskStatus::Failed);
        assert_eq!(got.task_id, expected_task);
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

    // -- #1180: subagent's OWN-session live SSE (foreground + background) -------

    /// Capturing [`SubagentSelfEventSink`] (#1180): records each `forwarder_for`
    /// binding and every event forwarded through the minted forwarders, so the
    /// dispatch tests can assert both paths bind to the subagent's OWN session
    /// and stream its untagged events in.
    #[derive(Debug, Default)]
    struct CapturingSelfSink {
        bound: parking_lot::Mutex<Vec<(SessionId, RunId)>>,
        events: Arc<parking_lot::Mutex<std::collections::HashMap<SessionId, Vec<String>>>>,
    }

    impl SubagentSelfEventSink for CapturingSelfSink {
        fn forwarder_for(&self, session_id: SessionId, run_id: RunId) -> Arc<dyn EventForwarder> {
            self.bound.lock().push((session_id, run_id));
            Arc::new(CapturingSelfForwarder {
                session_id,
                events: self.events.clone(),
            })
        }
    }

    #[derive(Debug)]
    struct CapturingSelfForwarder {
        session_id: SessionId,
        events: Arc<parking_lot::Mutex<std::collections::HashMap<SessionId, Vec<String>>>>,
    }

    impl CapturingSelfForwarder {
        fn record(&self, kind: &str, source_agent: &Option<String>) {
            // #1180: a subagent's own-session events must be untagged.
            assert!(
                source_agent.is_none(),
                "subagent own-session event '{kind}' must be untagged, got {source_agent:?}"
            );
            self.events
                .lock()
                .entry(self.session_id)
                .or_default()
                .push(kind.to_string());
        }
    }

    impl EventForwarder for CapturingSelfForwarder {
        fn forward_tool_start(
            &self,
            _: uuid::Uuid,
            _: String,
            _: serde_json::Value,
            source_agent: Option<String>,
            _: Option<String>,
        ) {
            self.record("tool_start", &source_agent);
        }
        fn forward_tool_end(
            &self,
            _: uuid::Uuid,
            _: bool,
            _: serde_json::Value,
            source_agent: Option<String>,
            _: Option<String>,
        ) {
            self.record("tool_end", &source_agent);
        }
        fn forward_token_delta(&self, _: String, source_agent: Option<String>) {
            self.record("token_delta", &source_agent);
        }
        fn forward_reasoning_delta(&self, _: String, source_agent: Option<String>) {
            self.record("reasoning_delta", &source_agent);
        }
        fn forward_stream_reset(&self) {
            self.events
                .lock()
                .entry(self.session_id)
                .or_default()
                .push("stream_reset".to_string());
        }
        fn forward_status(&self, _: String, _: Option<String>) {
            // `status` carries no `source_agent` in the EventForwarder API.
            self.events
                .lock()
                .entry(self.session_id)
                .or_default()
                .push("status".to_string());
        }
        fn forward_warning(&self, _: String, _: String, source_agent: Option<String>) {
            self.record("warning", &source_agent);
        }
        fn forward_run_terminal(&self, outcome: alms_tools::SubagentRunOutcome) {
            let kind = match outcome {
                alms_tools::SubagentRunOutcome::Completed { .. } => "run_finished",
                alms_tools::SubagentRunOutcome::Failed { .. } => "run_error",
                alms_tools::SubagentRunOutcome::Cancelled => "run_cancelled",
            };
            self.events
                .lock()
                .entry(self.session_id)
                .or_default()
                .push(kind.to_string());
        }
    }

    /// Poll the sink until the session has forwarded `target` (or the budget
    /// expires). The coordinator's content relay and the terminal emit run on
    /// separate tasks, so events can land just after `dispatch` returns.
    async fn poll_self_events(
        sink: &CapturingSelfSink,
        sid: SessionId,
        target: &str,
    ) -> Vec<String> {
        for _ in 0..100 {
            let got = sink.events.lock().get(&sid).cloned().unwrap_or_default();
            if got.iter().any(|e| e == target) {
                return got;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        sink.events.lock().get(&sid).cloned().unwrap_or_default()
    }

    /// `forward_event_to_self` must strip the `source_agent` tag — events arrive
    /// tagged for the parent relay, but the subagent's own session is untagged.
    #[test]
    fn test_forward_event_to_self_strips_source_agent_tag() {
        let sink = CapturingSelfForwarder {
            session_id: SessionId::new(),
            events: Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new())),
        };
        let sid = sink.session_id;
        // Events arrive TAGGED (as the coordinator's parent relay sees them)...
        forward_event_to_self(
            &sink,
            &alms_runtime::RuntimeEvent::ReasoningDelta {
                text: "secret".to_string(),
                source_agent: Some("reviewer".to_string()),
            },
        );
        forward_event_to_self(
            &sink,
            &alms_runtime::RuntimeEvent::ToolStart {
                invocation_id: uuid::Uuid::new_v4(),
                tool: "echo".to_string(),
                params: serde_json::json!({}),
                source_agent: Some("reviewer".to_string()),
                task_id: Some("t".to_string()),
            },
        );
        // ...and `record` (above) asserts each arrived untagged. A regression
        // that forwarded the tag would trip those asserts.
        let got = sink.events.lock().get(&sid).cloned().unwrap_or_default();
        assert_eq!(
            got,
            vec!["reasoning_delta".to_string(), "tool_start".to_string()]
        );
    }

    /// #1180 / #1162 sym-2: `forward_event_to_self` mirrors `StreamReset` onto
    /// the subagent's own stream. The self stream paints partials (token /
    /// reasoning deltas are forwarded), so dropping the reset would leave the
    /// buffered re-emit stacked on an un-retracted partial — the double-render
    /// Tim caught. (The parent relay can drop it; the self stream cannot.)
    #[test]
    fn test_forward_event_to_self_mirrors_stream_reset() {
        let sink = CapturingSelfForwarder {
            session_id: SessionId::new(),
            events: Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new())),
        };
        let sid = sink.session_id;
        forward_event_to_self(
            &sink,
            &alms_runtime::RuntimeEvent::StreamReset {
                source_agent: Some("reviewer".to_string()),
            },
        );
        let got = sink.events.lock().get(&sid).cloned().unwrap_or_default();
        assert_eq!(got, vec!["stream_reset".to_string()]);
    }

    // -- Subagent status bar: `forward_status_to_parent` (#1180 follow-up) --

    /// One captured `forward_subagent_activity` call:
    /// (kind, tool, tool_invocation_id, parent_tool_invocation_id,
    /// source_agent).
    type CapturedActivity = (
        String,
        Option<String>,
        Option<uuid::Uuid>,
        Option<uuid::Uuid>,
        String,
    );

    /// Records every `forward_subagent_activity` / `forward_warning` call and
    /// asserts (via panics) that NO content-carrying forward method is ever
    /// invoked — the parent relay must reduce subagent events to status
    /// signals, never forward reasoning/token text or tool params/results.
    #[derive(Debug, Default)]
    struct StatusCapturingForwarder {
        activity: parking_lot::Mutex<Vec<CapturedActivity>>,
        /// (code, source_agent) per forwarded warning.
        warnings: parking_lot::Mutex<Vec<(String, Option<String>)>>,
    }

    impl EventForwarder for StatusCapturingForwarder {
        fn forward_tool_start(
            &self,
            _: uuid::Uuid,
            _: String,
            _: serde_json::Value,
            _: Option<String>,
            _: Option<String>,
        ) {
            panic!("parent relay must not forward tool_start content (params) to the parent");
        }
        fn forward_tool_end(
            &self,
            _: uuid::Uuid,
            _: bool,
            _: serde_json::Value,
            _: Option<String>,
            _: Option<String>,
        ) {
            panic!("parent relay must not forward tool_end content (results) to the parent");
        }
        fn forward_token_delta(&self, _: String, _: Option<String>) {
            panic!("parent relay must not forward token text to the parent");
        }
        fn forward_reasoning_delta(&self, _: String, _: Option<String>) {
            panic!("parent relay must not forward reasoning text to the parent");
        }
        fn forward_subagent_activity(
            &self,
            kind: String,
            tool: Option<String>,
            tool_invocation_id: Option<uuid::Uuid>,
            parent_tool_invocation_id: Option<uuid::Uuid>,
            source_agent: String,
        ) {
            self.activity.lock().push((
                kind,
                tool,
                tool_invocation_id,
                parent_tool_invocation_id,
                source_agent,
            ));
        }
        fn forward_stream_reset(&self) {
            panic!("parent relay must not forward subagent stream resets (#1186)");
        }
        fn forward_status(&self, _: String, _: Option<String>) {
            panic!("parent relay must not forward subagent phase status to the parent");
        }
        fn forward_warning(&self, code: String, _: String, source_agent: Option<String>) {
            self.warnings.lock().push((code, source_agent));
        }
        fn forward_run_terminal(&self, _: alms_tools::SubagentRunOutcome) {}
    }

    fn reasoning_event() -> alms_runtime::RuntimeEvent {
        alms_runtime::RuntimeEvent::ReasoningDelta {
            text: "secret chain of thought".to_string(),
            source_agent: None,
        }
    }

    fn token_event() -> alms_runtime::RuntimeEvent {
        alms_runtime::RuntimeEvent::TokenDelta {
            delta: "visible output".to_string(),
            source_agent: None,
        }
    }

    fn tool_start_event(tool: &str, invocation_id: uuid::Uuid) -> alms_runtime::RuntimeEvent {
        alms_runtime::RuntimeEvent::ToolStart {
            invocation_id,
            tool: tool.to_string(),
            params: serde_json::json!({"huge": "payload"}),
            source_agent: None,
            task_id: None,
        }
    }

    fn tool_end_event(invocation_id: uuid::Uuid) -> alms_runtime::RuntimeEvent {
        alms_runtime::RuntimeEvent::ToolEnd {
            invocation_id,
            ok: true,
            result: serde_json::json!({"huge": "result"}),
            source_agent: None,
            task_id: None,
        }
    }

    /// The relay reduces subagent content events to tagged status signals and
    /// dedups consecutive same-kind deltas: a realistic run (many reasoning
    /// deltas → tool → many token deltas) produces one signal per activity
    /// TRANSITION, with the tool name on `tool_start` and the label on every
    /// signal. Any content-carrying forward panics via the capturing sink.
    #[test]
    fn test_forward_status_to_parent_dedups_and_strips_content() {
        let fwd = StatusCapturingForwarder::default();
        let mut last: Option<&'static str> = None;
        let shell_inv = uuid::Uuid::new_v4();
        // The parent invoke_agent invocation id — a per-subagent constant the
        // relay attaches to EVERY signal for identity-exact chip resolution
        // (#1190 Codex P2).
        let parent_inv = Some(uuid::Uuid::new_v4());

        // Burst of reasoning deltas -> ONE `reasoning` signal.
        for _ in 0..5 {
            forward_status_to_parent(
                &fwd,
                &reasoning_event(),
                "reviewer",
                parent_inv,
                &mut last,
                &mut |_| {},
            );
        }
        // Tool boundary -> `tool_start` (with name) then `tool_end`, both
        // carrying the tool's invocation id (#1190 — the UI's toolsUsed
        // idempotency key).
        forward_status_to_parent(
            &fwd,
            &tool_start_event("shell", shell_inv),
            "reviewer",
            parent_inv,
            &mut last,
            &mut |_| {},
        );
        forward_status_to_parent(
            &fwd,
            &tool_end_event(shell_inv),
            "reviewer",
            parent_inv,
            &mut last,
            &mut |_| {},
        );
        // Reasoning resumes after the tool -> a fresh `reasoning` signal
        // (the tool boundary reset the dedup state).
        for _ in 0..3 {
            forward_status_to_parent(
                &fwd,
                &reasoning_event(),
                "reviewer",
                parent_inv,
                &mut last,
                &mut |_| {},
            );
        }
        // Then the answer streams -> ONE `writing` signal.
        for _ in 0..4 {
            forward_status_to_parent(
                &fwd,
                &token_event(),
                "reviewer",
                parent_inv,
                &mut last,
                &mut |_| {},
            );
        }

        let got = fwd.activity.lock().clone();
        let label = "reviewer".to_string();
        assert_eq!(
            got,
            vec![
                (
                    "reasoning".to_string(),
                    None,
                    None,
                    parent_inv,
                    label.clone()
                ),
                (
                    "tool_start".to_string(),
                    Some("shell".to_string()),
                    Some(shell_inv),
                    parent_inv,
                    label.clone()
                ),
                (
                    "tool_end".to_string(),
                    None,
                    Some(shell_inv),
                    parent_inv,
                    label.clone()
                ),
                (
                    "reasoning".to_string(),
                    None,
                    None,
                    parent_inv,
                    label.clone()
                ),
                ("writing".to_string(), None, None, parent_inv, label.clone()),
            ],
            "expected exactly one tagged signal per activity transition, with \
             the child invocation id on the tool kinds and the parent \
             correlator on EVERY signal"
        );
    }

    /// Consecutive tool boundaries always emit (never deduped) — the bar must
    /// pick up each tool name, including the same tool run twice in a row.
    #[test]
    fn test_forward_status_to_parent_tool_boundaries_never_deduped() {
        let fwd = StatusCapturingForwarder::default();
        let mut last: Option<&'static str> = None;

        let first_inv = uuid::Uuid::new_v4();
        let second_inv = uuid::Uuid::new_v4();
        forward_status_to_parent(
            &fwd,
            &tool_start_event("shell", first_inv),
            "worker",
            None,
            &mut last,
            &mut |_| {},
        );
        forward_status_to_parent(
            &fwd,
            &tool_end_event(first_inv),
            "worker",
            None,
            &mut last,
            &mut |_| {},
        );
        forward_status_to_parent(
            &fwd,
            &tool_start_event("shell", second_inv),
            "worker",
            None,
            &mut last,
            &mut |_| {},
        );
        forward_status_to_parent(
            &fwd,
            &tool_end_event(second_inv),
            "worker",
            None,
            &mut last,
            &mut |_| {},
        );

        let got = fwd.activity.lock().clone();
        let kinds: Vec<String> = got.iter().map(|(k, _, _, _, _)| k.clone()).collect();
        assert_eq!(
            kinds,
            vec!["tool_start", "tool_end", "tool_start", "tool_end"]
        );
        // The two same-tool invocations carry DISTINCT invocation ids — the
        // identity the UI counts by (#1190): a re-run (or parallel sibling)
        // of the same tool is a new invocation, not a replay.
        assert_eq!(got[0].2, Some(first_inv));
        assert_eq!(got[2].2, Some(second_inv));
        assert_ne!(got[0].2, got[2].2);
    }

    /// Warnings still pass through, tagged with the subagent label; the
    /// suppressed variants (`Status`, `StreamReset`, `ContextDebug`,
    /// `SubagentStarted`) produce nothing — asserted falsifiably because the
    /// capturing sink PANICS on any content/status/reset forward.
    #[test]
    fn test_forward_status_to_parent_warnings_tagged_and_noise_suppressed() {
        let fwd = StatusCapturingForwarder::default();
        let mut last: Option<&'static str> = None;

        forward_status_to_parent(
            &fwd,
            &alms_runtime::RuntimeEvent::Warning {
                code: "SPILL".to_string(),
                message: "tool output spilled".to_string(),
                source_agent: None,
            },
            "reviewer",
            None,
            &mut last,
            &mut |_| {},
        );
        forward_status_to_parent(
            &fwd,
            &alms_runtime::RuntimeEvent::Status {
                phase: "calling_llm".to_string(),
                detail: None,
            },
            "reviewer",
            None,
            &mut last,
            &mut |_| {},
        );
        forward_status_to_parent(
            &fwd,
            &alms_runtime::RuntimeEvent::StreamReset {
                source_agent: Some("reviewer".to_string()),
            },
            "reviewer",
            None,
            &mut last,
            &mut |_| {},
        );
        forward_status_to_parent(
            &fwd,
            &alms_runtime::RuntimeEvent::SubagentStarted {
                tool_invocation_id: uuid::Uuid::new_v4(),
                subagent_name: None,
                subagent_session_id: SessionId::new(),
                background: false,
            },
            "reviewer",
            None,
            &mut last,
            &mut |_| {},
        );

        assert_eq!(
            fwd.warnings.lock().clone(),
            vec![("SPILL".to_string(), Some("reviewer".to_string()))],
            "warnings must be forwarded tagged with the subagent label"
        );
        assert!(
            fwd.activity.lock().is_empty(),
            "suppressed variants must not synthesise activity signals"
        );
        // A StreamReset must not clear the dedup state either: the next
        // same-kind delta after a buffered-fallback re-emit stays deduped
        // (the bar's label is already correct).
        forward_status_to_parent(
            &fwd,
            &reasoning_event(),
            "reviewer",
            None,
            &mut last,
            &mut |_| {},
        );
        forward_status_to_parent(
            &fwd,
            &reasoning_event(),
            "reviewer",
            None,
            &mut last,
            &mut |_| {},
        );
        assert_eq!(fwd.activity.lock().len(), 1);
    }

    /// The snapshot recording must land BEFORE the wire emission (Tim on
    /// #1190): if the handle were updated after the fact, a
    /// `subagent_activity_snapshot` read racing a fresh emission could return
    /// the PREVIOUS kind while the live event had already reached a
    /// newly-attached subscriber — the mirror of the reattach bug, with no
    /// correcting re-emit until the next transition.
    #[test]
    fn test_forward_status_to_parent_records_before_emitting() {
        /// Forwarder that appends "emit" to a shared order log on the wire
        /// call; the record callback appends "record".
        #[derive(Debug)]
        struct OrderLoggingForwarder {
            log: Arc<parking_lot::Mutex<Vec<&'static str>>>,
        }
        impl EventForwarder for OrderLoggingForwarder {
            fn forward_tool_start(
                &self,
                _: uuid::Uuid,
                _: String,
                _: serde_json::Value,
                _: Option<String>,
                _: Option<String>,
            ) {
            }
            fn forward_tool_end(
                &self,
                _: uuid::Uuid,
                _: bool,
                _: serde_json::Value,
                _: Option<String>,
                _: Option<String>,
            ) {
            }
            fn forward_token_delta(&self, _: String, _: Option<String>) {}
            fn forward_subagent_activity(
                &self,
                _: String,
                _: Option<String>,
                _: Option<uuid::Uuid>,
                _: Option<uuid::Uuid>,
                _: String,
            ) {
                self.log.lock().push("emit");
            }
            fn forward_stream_reset(&self) {}
            fn forward_status(&self, _: String, _: Option<String>) {}
            fn forward_warning(&self, _: String, _: String, _: Option<String>) {}
            fn forward_run_terminal(&self, _: alms_tools::SubagentRunOutcome) {}
        }

        let log = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let fwd = OrderLoggingForwarder { log: log.clone() };
        let mut last: Option<&'static str> = None;

        forward_status_to_parent(
            &fwd,
            &token_event(),
            "reviewer",
            None,
            &mut last,
            &mut |activity| {
                assert_eq!(activity.kind, "writing");
                log.lock().push("record");
            },
        );

        assert_eq!(
            *log.lock(),
            vec!["record", "emit"],
            "the handle recording must strictly precede the wire emission so \
             a snapshot read can never lag a live signal"
        );
    }

    /// #1180 (foreground): a foreground subagent run streams its (untagged) run
    /// events into a self-forwarder bound to the subagent's OWN session.
    #[tokio::test]
    async fn test_dispatch_foreground_streams_to_own_session_sink() {
        let sink = Arc::new(CapturingSelfSink::default());
        let coord = test_coordinator().with_subagent_self_sink(sink.clone());

        let (_resp, sub_session_id) = coord
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
            .await
            .expect("dispatch should succeed");

        // The self-forwarder was built exactly once, bound to the subagent's OWN
        // session (not the parent's).
        let bound = sink.bound.lock().clone();
        assert_eq!(bound.len(), 1, "one self-forwarder per subagent");
        assert_eq!(
            bound[0].0, sub_session_id,
            "self-forwarder must be bound to the subagent's OWN session"
        );

        // The run's content + terminal streamed into the self-sink (mock LLM
        // completes); `record` already pinned the content was untagged.
        let got = poll_self_events(&sink, sub_session_id, "run_finished").await;
        assert!(
            got.contains(&"run_finished".to_string()),
            "foreground subagent must stream content + a terminal (run_finished) \
             to its OWN session sink, got {got:?}"
        );
    }

    /// #1180 (background): same guarantee for a background subagent run.
    #[tokio::test]
    async fn test_dispatch_background_streams_to_own_session_sink() {
        let sink = Arc::new(CapturingSelfSink::default());
        let coord = test_coordinator().with_subagent_self_sink(sink.clone());

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

        // Wait for the background subagent to reach a terminal state.
        let tid = TaskId(task_uuid);
        for _ in 0..100 {
            match coord.get_status(tid) {
                Some(TaskStatus::Completed) | Some(TaskStatus::Failed) => break,
                _ => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }

        let bound = sink.bound.lock().clone();
        assert_eq!(bound.len(), 1, "one self-forwarder per subagent");
        assert_eq!(
            bound[0].0, sub_session_id,
            "self-forwarder must be bound to the subagent's OWN session"
        );

        let got = poll_self_events(&sink, sub_session_id, "run_finished").await;
        assert!(
            got.contains(&"run_finished".to_string()),
            "background subagent must stream content + a terminal (run_finished) \
             to its OWN session sink, got {got:?}"
        );
    }

    // -- Subagent status snapshot (#1189 follow-up) ------------------------------

    /// END-TO-END emission + recording for a REAL background subagent run:
    /// the mock LLM streams the response word-by-word, the runtime emits
    /// per-chunk `TokenDelta`s on the subagent's event channel, and the REAL
    /// relay (spawned by `run_agent_loop`) must (a) reduce them to exactly ONE
    /// tagged `writing` signal on the parent forwarder and (b) record that
    /// signal on the SubagentHandle so `subagent_activity_snapshot` can
    /// replay it to session streams that attach later.
    ///
    /// (b) is the regression target for the "chip stuck on Starting…" bug:
    /// the live signal fires at most once per activity transition and is
    /// never persisted, so without the handle recording there is nothing left
    /// for a reattaching client to consume.
    #[tokio::test]
    async fn test_bg_dispatch_relay_emits_and_records_latest_activity() {
        let coord = test_coordinator();
        let fwd = Arc::new(StatusCapturingForwarder::default());
        let parent_session = test_session_id();
        // The parent's invoke_agent invocation id (what InvokeAgentTool
        // supplies in production) — must ride EVERY status signal and the
        // recorded snapshot as the chip-resolution correlator (#1190).
        let parent_inv = uuid::Uuid::new_v4();

        let (task_uuid, _sub_session_id) = coord
            .dispatch_background(
                "Background work".to_string(),
                parent_session,
                test_parent_agent_id(),
                None,
                Some(fwd.clone() as Arc<dyn EventForwarder>),
                None,
                None,
                Some(parent_inv),
            )
            .await
            .expect("dispatch_background should succeed");
        let tid = TaskId(task_uuid);

        // Wait until the relay has drained the run's deltas: at least one
        // activity signal captured AND the recording landed on the handle.
        let mut recorded = None;
        for _ in 0..100 {
            recorded = coord.latest_activity_for(tid);
            if recorded.is_some() && !fwd.activity.lock().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // (a) The relay emitted exactly one deduplicated `writing` signal
        // (the mock LLM streams visible tokens only), tagged with the
        // backend label the frontend keys chips by.
        let expected_label = format!("subagent-{}", &task_uuid.to_string()[..8]);
        let got = fwd.activity.lock().clone();
        assert_eq!(
            got,
            vec![(
                "writing".to_string(),
                None,
                None,
                Some(parent_inv),
                expected_label.clone()
            )],
            "a real streamed run must reduce to one tagged `writing` signal \
             carrying the parent invoke_agent correlator"
        );

        // (b) The same signal was recorded on the handle for snapshot replay.
        assert_eq!(
            recorded,
            Some(SubagentActivity {
                kind: "writing".to_string(),
                tool: None,
                tool_invocation_id: None,
                parent_tool_invocation_id: Some(parent_inv),
            }),
            "the relay must record its emitted signal on the SubagentHandle — \
             without it, a session stream attaching after the (ephemeral, \
             deduplicated) live signal has no way to learn the subagent's \
             current status and the chip sticks on 'Starting…'"
        );

        // The handle's label matches what the signals were tagged with, so
        // the snapshot resolves to the same chip the live signal targeted.
        let handle_label = coord.subagents.get(&tid).map(|h| h.label.clone());
        assert_eq!(handle_label, Some(expected_label));
    }

    /// `subagent_activity_snapshot` returns the current activity of in-flight
    /// subagents for the requested parent session ONLY: terminal tasks are
    /// excluded (their chips already got `subagent_completed`), tasks with no
    /// recorded signal are excluded (legitimately "Starting…"), and other
    /// sessions' subagents never leak in.
    #[tokio::test]
    async fn test_subagent_activity_snapshot_filters_running_and_session() {
        let coord = test_coordinator();
        let session_a = test_session_id();
        let session_b = test_session_id();
        let parent_agent = test_parent_agent_id();

        let mk_handle = |status: TaskStatus,
                         session: SessionId,
                         label: &str,
                         activity: Option<SubagentActivity>| {
            let task_id = TaskId::new();
            SubagentHandle {
                task_id,
                status,
                cancel_token: CancellationToken::new(),
                parent_run_id: None,
                parent_session_id: session,
                parent_agent_id: parent_agent,
                subagent_session_id: SessionId::new(),
                is_background: true,
                label: label.to_string(),
                latest_activity: activity,
                result_rx: None,
                completed_result: None,
                error_rx: None,
            }
        };

        let shell_inv = uuid::Uuid::new_v4();
        let parent_inv = uuid::Uuid::new_v4();
        let writing = SubagentActivity {
            kind: "writing".to_string(),
            tool: None,
            tool_invocation_id: None,
            parent_tool_invocation_id: None,
        };
        let using_shell = SubagentActivity {
            kind: "tool_start".to_string(),
            tool: Some("shell".to_string()),
            tool_invocation_id: Some(shell_inv),
            parent_tool_invocation_id: Some(parent_inv),
        };

        // In-flight on session A with a recorded signal — the one hit.
        let running = mk_handle(
            TaskStatus::Running,
            session_a,
            "subagent-aaaaaaaa",
            Some(writing.clone()),
        );
        // In-flight on session A but nothing recorded yet — excluded.
        let starting = mk_handle(TaskStatus::Running, session_a, "subagent-bbbbbbbb", None);
        // Terminal on session A — excluded even with a recorded signal.
        let done = mk_handle(
            TaskStatus::Completed,
            session_a,
            "subagent-cccccccc",
            Some(writing.clone()),
        );
        // In-flight on session B — excluded from session A's snapshot.
        let other_session = mk_handle(
            TaskStatus::Running,
            session_b,
            "reviewer",
            Some(using_shell.clone()),
        );

        for h in [running, starting, done, other_session] {
            coord.subagents.insert(h.task_id, h);
        }

        let snap = coord.subagent_activity_snapshot(session_a);
        assert_eq!(
            snap.len(),
            1,
            "only the in-flight session-A subagent with a recorded signal \
             belongs in session A's snapshot, got {snap:?}"
        );
        assert_eq!(snap[0].label, "subagent-aaaaaaaa");
        assert_eq!(snap[0].kind, "writing");
        assert_eq!(snap[0].tool, None);

        let snap_b = coord.subagent_activity_snapshot(session_b);
        assert_eq!(snap_b.len(), 1);
        assert_eq!(snap_b[0].label, "reviewer");
        assert_eq!(snap_b[0].kind, "tool_start");
        assert_eq!(snap_b[0].tool.as_deref(), Some("shell"));
        // The recorded ids round-trip into the snapshot verbatim — the
        // attach-time replay must carry the SAME child id as the live signal
        // (distinct-id tool counting) and the SAME parent correlator
        // (identity-exact chip resolution for concurrent unnamed subagents,
        // #1190).
        assert_eq!(snap_b[0].tool_invocation_id, Some(shell_inv));
        assert_eq!(snap_b[0].parent_tool_invocation_id, Some(parent_inv));
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
        fn forward_stream_reset(&self) {}
        fn forward_status(&self, _phase: String, _detail: Option<String>) {}
        fn forward_warning(&self, _code: String, _message: String, _source_agent: Option<String>) {}
        fn forward_run_terminal(&self, _outcome: alms_tools::SubagentRunOutcome) {}
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

    // -- (d) cancel_subagent cancels token and removes handle ------------------
    //
    // A synthetic handle makes both effects deterministic without racing the
    // synchronously completing mock LLM.

    #[test]
    fn test_cancel_subagent_cancels_token_and_removes_handle() {
        let coord = test_coordinator();
        let task_id = TaskId::new();
        let (handle, token) = synthetic_handle(task_id, SessionId::new(), TaskStatus::Pending);
        coord.subagents.insert(task_id, handle);

        let cancel_result = coord.cancel_subagent(task_id);
        assert!(cancel_result.is_ok(), "cancel should succeed");
        assert!(token.is_cancelled(), "cancel should fire the stored token");
        assert!(
            coord.get_status(task_id).is_none(),
            "handle should be removed after cancel"
        );
    }

    // -- (d2) cancel_subagent_by_session — session-keyed user cancel -----------
    //
    // The production cancel surface behind `POST /sessions/{id}/subagent/
    // cancel`. The mock LLM completes synchronously, so instead of racing a
    // real spawn these tests insert a synthetic handle directly (the tests
    // module can reach the private `subagents` map) and assert on the
    // token, which is deterministic. The handle must NOT be removed — the
    // spawned `run_subagent` task owns the terminal bookkeeping (status
    // flip, completion notification) and needs the handle to do it.

    /// Build a synthetic SubagentHandle for cancel-by-session tests.
    /// Returns the handle plus a clone of its cancellation token so the
    /// test can observe whether `cancel_subagent_by_session` fired it.
    fn synthetic_handle(
        task_id: TaskId,
        subagent_session_id: SessionId,
        status: TaskStatus,
    ) -> (SubagentHandle, CancellationToken) {
        let token = CancellationToken::new();
        let handle = SubagentHandle {
            task_id,
            status,
            cancel_token: token.clone(),
            parent_run_id: None,
            parent_session_id: SessionId::new(),
            parent_agent_id: AgentId::new(),
            subagent_session_id,
            is_background: true,
            label: format!("subagent-{}", &task_id.0.to_string()[..8]),
            latest_activity: None,
            result_rx: None,
            completed_result: None,
            error_rx: None,
        };
        (handle, token)
    }

    #[tokio::test]
    async fn test_cancel_by_session_fires_live_token_and_keeps_handle() {
        let coord = test_coordinator();
        let task_id = TaskId::new();
        let sub_session = SessionId::new();
        let (handle, token) = synthetic_handle(task_id, sub_session, TaskStatus::Running);
        coord.subagents.insert(task_id, handle);

        assert!(
            coord.cancel_subagent_by_session(sub_session),
            "cancel_subagent_by_session must report true for a live subagent"
        );
        assert!(
            token.is_cancelled(),
            "the handle's cancellation token must be fired"
        );
        // The handle must remain in the map: run_subagent's terminal arm
        // needs it to flip status and emit the completion notification
        // (removing it would silently drop the `subagent_completed` /
        // 'cancelled' path the UI chip relies on).
        assert!(
            coord.get_status(task_id).is_some(),
            "the handle must NOT be removed by a session-keyed cancel"
        );
    }

    #[tokio::test]
    async fn test_cancel_by_session_pending_handle_is_cancellable() {
        // A subagent can be cancelled during its Pending window (spawned,
        // `run_subagent` not yet marked it Running) — the token exists from
        // `spawn_subagent` time precisely for this.
        let coord = test_coordinator();
        let task_id = TaskId::new();
        let sub_session = SessionId::new();
        let (handle, token) = synthetic_handle(task_id, sub_session, TaskStatus::Pending);
        coord.subagents.insert(task_id, handle);

        assert!(coord.cancel_subagent_by_session(sub_session));
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn test_cancel_by_session_unknown_session_returns_false() {
        let coord = test_coordinator();
        assert!(
            !coord.cancel_subagent_by_session(SessionId::new()),
            "an unknown session must report false (mapped to HTTP 404)"
        );
    }

    #[tokio::test]
    async fn test_cancel_by_session_terminal_handle_returns_false() {
        // A terminal handle for the session must not match: named subagents
        // reuse the session across invocations, and firing a finished
        // invocation's token would be a no-op at best and misleading (200
        // for a cancel that cancelled nothing) at worst.
        let coord = test_coordinator();
        let task_id = TaskId::new();
        let sub_session = SessionId::new();
        let (handle, token) = synthetic_handle(task_id, sub_session, TaskStatus::Completed);
        coord.subagents.insert(task_id, handle);

        assert!(
            !coord.cancel_subagent_by_session(sub_session),
            "a terminal subagent must report false"
        );
        assert!(
            !token.is_cancelled(),
            "a terminal handle's token must not be fired"
        );
    }

    #[tokio::test]
    async fn test_cancel_by_session_after_real_completion_returns_false() {
        // End-to-end variant of the terminal case: a REAL spawn against the
        // mock LLM, awaited to completion (status flips before the result
        // is delivered, so this is deterministic), then a session-keyed
        // cancel must find no live subagent.
        let coord = test_coordinator();
        let request = SubagentRequest {
            task: "Complete then try to cancel".to_string(),
            parent_session: test_session_id(),
            parent_agent_id: test_parent_agent_id(),
            parent_run_id: None,
            subagent_name: None,
            parent_tool_invocation_id: None,
        };
        let (task_id, sub_session_id) = coord
            .spawn_subagent(request, None, false, None)
            .await
            .unwrap();
        let result_rx = coord.take_result_rx(task_id).unwrap();
        let task_result = result_rx.await.expect("should receive result");
        assert_eq!(task_result.status, TaskStatus::Completed);

        assert!(
            !coord.cancel_subagent_by_session(sub_session_id),
            "a completed subagent's session must report false"
        );
    }

    // -- (d3) cancel labelling is poll-order-independent (Tim S1, PR #1192) -----
    //
    // `run_subagent`'s run branch can observe a fired token BEFORE the
    // select's token arm does (the loop's own checkpoints return
    // `Err(Cancelled)` / `Err(CancelledWithToolCalls)`). Those variants are
    // produced exclusively by token observation, so the Err handler must
    // label them `Cancelled`, not `Failed` — otherwise a user cancel's
    // label would depend on which arm won the race.

    #[test]
    fn test_subagent_error_status_classifies_cancellation_variants() {
        assert_eq!(
            subagent_error_status(&AlmsError::Cancelled),
            TaskStatus::Cancelled,
            "a loop-checkpoint Cancelled must label the task Cancelled"
        );
        assert_eq!(
            subagent_error_status(&AlmsError::CancelledWithToolCalls { tool_calls: vec![] }),
            TaskStatus::Cancelled,
            "a mid-batch cancel (partial tool calls preserved) is still a cancel"
        );
        assert_eq!(
            subagent_error_status(&AlmsError::Runtime("boom".to_string())),
            TaskStatus::Failed,
            "a genuine runtime failure must stay Failed"
        );
        assert_eq!(
            subagent_error_status(&AlmsError::AgentNotFound("x".to_string())),
            TaskStatus::Failed,
            "non-cancellation errors must stay Failed"
        );
    }

    /// A token that is already fired when the LOOP runs (bypassing
    /// `run_subagent`'s select entirely by calling `run_agent_loop`
    /// directly) surfaces as exactly the cancellation variants
    /// `subagent_error_status` classifies — together the two pin the
    /// end-to-end property "a token-fired cancel observed by the loop
    /// yields `TaskStatus::Cancelled`", without racing the instant mock
    /// LLM mid-run.
    #[tokio::test]
    async fn test_loop_observed_cancel_yields_cancelled_status() {
        let session_manager = Arc::new(SessionManager::new(alms_session::SessionConfig::default()));
        let llm = LlmClient::new(LlmConfig {
            mock: true,
            ..LlmConfig::default()
        })
        .unwrap();
        let task_id = TaskId::new();
        let request = SubagentRequest {
            task: "cancelled before the loop starts".to_string(),
            parent_session: test_session_id(),
            parent_agent_id: test_parent_agent_id(),
            parent_run_id: None,
            subagent_name: None,
            parent_tool_invocation_id: None,
        };
        let (sub_agent_id, sub_context_id) = derive_subagent_identity(task_id, &request);
        let cancel_token = CancellationToken::new();
        cancel_token.cancel();
        let prompts = DashMap::new();

        let result = run_agent_loop(
            task_id,
            &request,
            sub_agent_id,
            &sub_context_id,
            &session_manager,
            &llm,
            None,
            None,
            &AgentConfig::default(),
            None,
            None,
            None,
            &prompts,
            cancel_token,
            None,
            &alms_core::config::SecurityConfig::default(),
            false,
            Arc::new(DashMap::new()),
        )
        .await;

        let err = result.expect_err("a pre-cancelled token must abort the loop");
        assert!(
            matches!(
                err,
                AlmsError::Cancelled | AlmsError::CancelledWithToolCalls { .. }
            ),
            "the loop's cancellation checkpoints must surface the token as a \
             Cancelled-class error (got {err:?}) — these are the variants \
             `subagent_error_status` maps to TaskStatus::Cancelled"
        );
        assert_eq!(
            subagent_error_status(&err),
            TaskStatus::Cancelled,
            "run_subagent's Err handler must label this outcome Cancelled, not Failed"
        );
    }

    // -- (e) #1150 — no implicit wall-clock run-kill arm ------------------------
    //
    // Regression for #1150: the dispatch `select!` no longer has a
    // `tokio::time::sleep(request.timeout)` arm that fails a subagent with a
    // generic `Timeout`. A subagent that completes normally (here, against the
    // mock LLM) must surface `Completed` — never `Failed {"error":"Timeout"}`
    // — proving the implicit 5-minute kill is gone and a long-but-productive
    // subagent is bounded only by the inherited in-loop phase timer (#1150),
    // `max_iterations`, or cancellation.

    #[tokio::test]
    async fn test_no_implicit_run_kill_completes_normally() {
        let coord = test_coordinator();
        let session_id = test_session_id();

        let request = SubagentRequest {
            task: "Run to completion".to_string(),
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
        assert_eq!(
            task_result.status,
            TaskStatus::Completed,
            "with the run-kill arm removed (#1150) a normally-completing \
             subagent must be Completed, not timed out; got: {:?}",
            task_result.status
        );
        // And specifically not the old generic timeout failure shape.
        assert_ne!(
            task_result.result.get("error").and_then(|e| e.as_str()),
            Some("Timeout"),
            "the removed run-kill arm's {{\"error\":\"Timeout\"}} result must \
             never be produced (#1150)"
        );

        // Retention still works (#1150): dropping the run-kill `select!`
        // arm must NOT have disturbed the *post-completion* retention window.
        // Once the result has been delivered the handle is kept in the map for
        // `RESULT_RETENTION_SECS` (well beyond this test) so the
        // completion-notification poller can still read it — so the handle is
        // present here, not reaped the instant the run finished.
        assert!(
            coord.get_status(task_id).is_some(),
            "the completed subagent handle must be retained for polling after \
             the run finishes (RESULT_RETENTION_SECS window, #1150)"
        );
    }

    // -- (g) list_active shows spawned subagents --------------------------------

    #[tokio::test]
    async fn test_list_active_includes_spawned() {
        let coord = test_coordinator();
        let session_id = test_session_id();

        let request = SubagentRequest {
            task: "List test".to_string(),
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
            // Agent-loop hard caps (#987 / B3 / #1150) — set to non-default
            // values so inheritance is proven, not just a default match. The
            // phase-aware inactivity budgets (#1150) must propagate to a
            // subagent verbatim so it terminates on the same progress-aware
            // ceilings its parent would (the coordinator's old 5-minute
            // wall-clock kill having been removed in #1150).
            max_iterations: 123,
            max_run_duration_secs: 99_999,
            between_iterations_secs: 222,
            tool_phase_ceiling_secs: 333,
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
        // Should inherit the agent-loop hard caps, including the #1150
        // phase-aware inactivity budgets, verbatim.
        assert_eq!(config.max_iterations, 123);
        assert_eq!(config.max_run_duration_secs, 99_999);
        assert_eq!(config.between_iterations_secs, 222);
        assert_eq!(config.tool_phase_ceiling_secs, 333);
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
        // The agent-loop hard caps (incl. the #1150 inactivity budgets) still
        // inherit through the registry-override path.
        assert_eq!(config2.max_iterations, 123);
        assert_eq!(config2.max_run_duration_secs, 99_999);
        assert_eq!(config2.between_iterations_secs, 222);
        assert_eq!(config2.tool_phase_ceiling_secs, 333);
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

    /// Repro for #1181: after a parent invokes an EPHEMERAL / unnamed
    /// subagent via the real coordinator path, the parent can read the
    /// subagent's persisted transcript by `session_id` — the id dispatch
    /// returns (and that `subagent_started` / the completion notification
    /// surface). Pre-#1181 `read_subagent_session` only resolved by name,
    /// so ephemeral transcripts were unreachable even though fully
    /// persisted.
    ///
    /// Like the named test above, this drives the REAL
    /// `derive_subagent_identity` context shape
    /// (`subagent_{parent_agent_id}_{task_id}`, parent id embedded per the
    /// PR #1185 ownership hardening) against the tool's access check — the
    /// unit tests in `read_subagent_session.rs` construct that context by
    /// hand, so they cannot catch the coordinator and the tool drifting
    /// apart. Also pins the denial half: a NON-parent agent supplying the
    /// same session UUID (which leaks into parent-visible text and shared DM
    /// sessions) must be rejected — the UUID is not a bearer capability.
    #[tokio::test]
    async fn test_parent_can_read_ephemeral_subagent_session_by_id() {
        use alms_sandbox::Tool;
        use alms_tools::ReadSubagentSessionTool;

        let workspace_tmp = tempfile::TempDir::new().unwrap();
        let coord = test_coordinator().with_workspace_dir(workspace_tmp.path().to_path_buf());
        let parent_session = test_session_id();
        let parent_agent_id = test_parent_agent_id();

        // Unnamed dispatch — fresh random session, context `subagent_{task_id}`.
        let (_response, sub_session_id) = coord
            .dispatch(
                "Investigate topic X".to_string(),
                parent_session,
                parent_agent_id,
                None,
                None,
                None, // unnamed → ephemeral
                None,
                None,
            )
            .await
            .expect("dispatch should succeed");

        let read_tool =
            ReadSubagentSessionTool::new(coord.session_manager.clone(), parent_agent_id);
        let result = read_tool
            .execute(serde_json::json!({ "session_id": sub_session_id.0.to_string() }))
            .await
            .expect("read_subagent_session should not error");

        debug!(?result, "parent read of ephemeral subagent session by id");

        assert!(
            result.get("error").is_none(),
            "parent must be able to read its ephemeral subagent's session by id, got: {result}"
        );
        assert_eq!(result["session_id"], sub_session_id.0.to_string());
        // Ephemeral subagents have no name — label is null.
        assert!(result["subagent"].is_null());
        assert!(
            result["message_count"].as_u64().unwrap_or(0) > 0,
            "ephemeral subagent session should have at least one message after dispatch"
        );

        // PR #1185 hardening: a DIFFERENT agent (e.g. a DM peer that saw the
        // session id on the shared DM session) supplying the same UUID must
        // be denied — ownership comes from the parent id embedded in the
        // context by `derive_subagent_identity`, not from knowing the UUID.
        let non_parent = AgentId::new();
        assert_ne!(non_parent, parent_agent_id);
        let peer_tool = ReadSubagentSessionTool::new(coord.session_manager.clone(), non_parent);
        let denied = peer_tool
            .execute(serde_json::json!({ "session_id": sub_session_id.0.to_string() }))
            .await
            .expect("read_subagent_session should not panic for non-parent");
        assert!(
            denied["error"]
                .as_str()
                .unwrap_or("")
                .contains("another agent's subagent"),
            "non-parent must be denied the ephemeral transcript, got: {denied}"
        );
        assert!(denied.get("messages").is_none());
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
            let coord = Coordinator::new(session_manager.clone(), llm)
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
            parent_session: session_a,
            parent_agent_id,
            parent_run_id: None,
            subagent_name: Some("reviewer".into()),
            parent_tool_invocation_id: None,
        };
        let req_b = SubagentRequest {
            task: "task B".into(),
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
        // The cleanup waits RESULT_RETENTION_SECS before pulling the handle
        // and running the cache `remove`, so we can't realistically wait it
        // out.
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

        // Now simulate the cleanup that fires after RESULT_RETENTION_SECS by
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

    // -- #1150 regression: a blocking foreground `invoke_agent` that outruns the
    //    parent's P3 tool-phase ceiling must NOT stall-fail the parent --------

    /// Read one full HTTP request (headers + Content-Length body) from a socket
    /// so the scripted LLM server consumes the agent's request before
    /// responding. Mirrors the helper in the runtime's agent-loop integration
    /// tests.
    async fn read_full_http_request(sock: &mut tokio::net::TcpStream) -> String {
        use tokio::io::AsyncReadExt;
        let mut buf: Vec<u8> = Vec::new();
        let mut tmp = [0u8; 8192];
        loop {
            let n = match sock.read(&mut tmp).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            buf.extend_from_slice(&tmp[..n]);
            let text = String::from_utf8_lossy(&buf);
            if let Some(header_end) = text.find("\r\n\r\n") {
                let content_length = text[..header_end]
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if buf.len() >= header_end + 4 + content_length {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// A `SubagentDispatcher` whose foreground `dispatch` blocks for a fixed
    /// duration — standing in for a long-but-productive subagent — then returns
    /// a successful response. Records the call count so the test can confirm
    /// the subagent actually ran (and blocked) rather than being short-circuited.
    #[derive(Debug)]
    struct SleepyForegroundDispatcher {
        block: Duration,
        response: String,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl SubagentDispatcher for SleepyForegroundDispatcher {
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
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Block past the parent's P3 ceiling with no progress signal
            // reaching the parent's ActivityClock — exactly the foreground
            // subagent case #1150 must not kill the parent for.
            tokio::time::sleep(self.block).await;
            Ok((self.response.clone(), SessionId::new()))
        }
    }

    /// #1150 regression (Tim's review): a **foreground** `invoke_agent` whose
    /// subagent runs *past* the parent's `tool_phase_ceiling_secs` (P3) must
    /// NOT stall-fail the parent. The parent blocks on `dispatch().await` for
    /// the subagent's whole runtime, and the subagent's progress never touches
    /// the parent's stack-local `ActivityClock` — so before the fix the parent
    /// tripped P3 at the very next checkpoint and discarded the subagent's
    /// completed work (re-creating, for the foreground path, the exact failure
    /// #1150 set out to fix). The fix runs a blocking-`invoke_agent` batch under
    /// the unbounded `ExecutingBlockingSubagent` phase, so the parent receives
    /// the subagent's result and continues.
    ///
    /// Real foreground path: a real `alms_tools::InvokeAgentTool` over a real
    /// parent `AgentRuntime` (driven via `run`), scripted to call `invoke_agent`
    /// (turn 1) then answer with final text (turn 2). The subagent is a
    /// `SleepyForegroundDispatcher` that blocks ~1.2s — well past the 1s P3
    /// ceiling — then returns successfully. The earlier `SleepTool` +
    /// background-dispatch tests do not exercise this interaction.
    #[tokio::test]
    async fn foreground_invoke_agent_past_p3_does_not_stall_parent() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        // Turn 1: request a single FOREGROUND `invoke_agent` (no `background`
        // flag) and no final text, so the loop must run the subagent then
        // iterate. Turn 2: a plain text reply that ends the run.
        let turn1_body = concat!(
            "data: {\"id\":\"t1\",\"object\":\"chat.completion.chunk\",\"created\":1,",
            "\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",",
            "\"tool_calls\":[{\"index\":0,\"id\":\"call_inv\",\"type\":\"function\",",
            "\"function\":{\"name\":\"invoke_agent\",\"arguments\":\"{\\\"task\\\":\\\"do work\\\"}\"}}]},",
            "\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let turn2_body = concat!(
            "data: {\"id\":\"t2\",\"object\":\"chat.completion.chunk\",\"created\":2,",
            "\"model\":\"test-model\",\"choices\":[{\"index\":0,",
            "\"delta\":{\"role\":\"assistant\",\"content\":\"all done\"},",
            "\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());

        // Serve exactly the two scripted turns, counting calls so we can assert
        // the parent made both (the subagent turn AND the final-text turn).
        let call_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let call_count_writer = call_count.clone();
        tokio::spawn(async move {
            for body in [turn1_body, turn2_body] {
                let (mut sock, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let _ = read_full_http_request(&mut sock).await;
                call_count_writer.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });

        let llm_config = LlmConfig {
            base_url,
            api_key: "test-key".to_string(),
            default_model: "test-model".to_string(),
            timeout_secs: 5,
            stream_chunk_timeout_secs: 5,
            ..LlmConfig::default()
        };
        let agent_config = AgentConfig {
            sandbox_root: "".into(),
            // `invoke_agent` is not auto-approved; Autonomous runs tools without
            // an approval round-trip (the realistic posture for a
            // subagent-spawning run).
            posture: alms_runtime::Posture::Autonomous,
            // Isolate P3: a 1s tool-phase ceiling with every other cap disabled.
            // The subagent blocks ~1.2s, so a progress-blind P3 would trip.
            max_iterations: 1000,
            max_run_duration_secs: 0,
            between_iterations_secs: 0,
            tool_phase_ceiling_secs: 1,
            ..AgentConfig::default()
        };
        let runtime = AgentRuntime::new(
            AgentId::new(),
            agent_config,
            LlmClient::new(llm_config).unwrap(),
        )
        .unwrap()
        .with_agent_name("parent".to_string());

        // Register the REAL foreground invoke_agent tool, wired to a dispatcher
        // that blocks ~1.2s then returns a successful response.
        let dispatcher = Arc::new(SleepyForegroundDispatcher {
            block: Duration::from_millis(1200),
            response: "subagent finished its long task".to_string(),
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let invoke_tool = alms_tools::InvokeAgentTool::new(
            dispatcher.clone(),
            SessionId::new(),
            AgentId::new(),
            None,
            None,
        );
        runtime.register_tool(Arc::new(invoke_tool));

        let session_manager = SessionManager::new(alms_session::SessionConfig::default());
        let result = runtime
            .run(&session_manager, "normal-context", "delegate the long task")
            .await;

        // The regression: pre-fix the parent tripped P3 the instant the ~1.2s
        // subagent returned (idle > the 1s ceiling) and failed with a stalled
        // error, discarding the subagent's work. With the blocking-invoke_agent
        // exclusion the parent stays unbounded for that batch and continues.
        let output = match result {
            Ok(o) => o,
            Err(e) => panic!(
                "parent must NOT stall-fail on a foreground invoke_agent that \
                 outruns the P3 ceiling; got error: {e}"
            ),
        };
        assert_eq!(
            output.response, "all done",
            "the parent must continue past the blocking subagent to its final reply"
        );
        assert_eq!(
            dispatcher.calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the foreground subagent must have actually run (and blocked) once"
        );
        assert_eq!(
            call_count.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the parent must make two LLM calls: the invoke_agent turn and the final-text turn"
        );
        // The subagent's result is carried into the parent run (not discarded
        // by a stall trip).
        assert!(
            output
                .tool_calls
                .iter()
                .any(|r| r.tool_name.as_deref() == Some("invoke_agent")),
            "the invoke_agent call/result must be recorded in the parent run"
        );
    }
}
