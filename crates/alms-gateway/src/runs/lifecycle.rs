// SPDX-License-Identifier: Apache-2.0

//! Run creation, execution, and completion — the core run lifecycle.

use super::tools::{RoutedBgEvent, RuntimeEventForwarder, forward_runtime_events, route_bg_event};
use super::{RunParams, is_internal_context_id};
use crate::api_error;
use crate::configuration::{ResolveAgentConfigError, build_resolved_config, resolve_agent_config};
use crate::server::AppState;
use crate::session_queue::AdmissionError;
use crate::sse::SseEventData;
use alms_core::{
    AgentId, AlmsError, CreateRunRequest, CreateRunResponse, Run, RunId, RunInput, RunStatus,
    SessionId, classify_session_type, sanitize_error_for_session,
};
use alms_runtime::RuntimeEvent;
use alms_tools::message_sender::ConversationEndReason;
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header::RETRY_AFTER},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use futures::FutureExt;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
#[cfg(test)]
use std::sync::LazyLock;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument, warn};

#[cfg(test)]
static START_TRANSITION_BARRIERS: LazyLock<dashmap::DashMap<RunId, Arc<tokio::sync::Barrier>>> =
    LazyLock::new(dashmap::DashMap::new);
#[cfg(test)]
static TERMINAL_TRANSITION_BARRIERS: LazyLock<dashmap::DashMap<RunId, Arc<tokio::sync::Barrier>>> =
    LazyLock::new(dashmap::DashMap::new);
#[cfg(test)]
static ADMISSION_PERSISTENCE_BARRIERS: LazyLock<
    dashmap::DashMap<SessionId, Arc<tokio::sync::Barrier>>,
> = LazyLock::new(dashmap::DashMap::new);

#[cfg(test)]
static ADMISSION_EVENT_BARRIERS: LazyLock<dashmap::DashMap<SessionId, Arc<tokio::sync::Barrier>>> =
    LazyLock::new(dashmap::DashMap::new);
#[cfg(test)]
static ADMISSION_EXECUTION_BARRIERS: LazyLock<
    dashmap::DashMap<SessionId, Arc<tokio::sync::Barrier>>,
> = LazyLock::new(dashmap::DashMap::new);

#[cfg(test)]
static ADMISSION_ACQUIRE_BARRIERS: LazyLock<
    dashmap::DashMap<SessionId, Arc<tokio::sync::Barrier>>,
> = LazyLock::new(dashmap::DashMap::new);

#[derive(Debug)]
pub(crate) struct RunAdmissionGate {
    mutex: Arc<tokio::sync::Mutex<()>>,
    leases: std::sync::atomic::AtomicUsize,
}

pub(crate) type RunAdmissionGates = Arc<dashmap::DashMap<SessionId, Arc<RunAdmissionGate>>>;

/// Pre-await ownership of an admission mutex.
///
/// This lease exists before `lock_owned().await`, so cancelling a waiter still
/// runs the same last-reference cleanup as dropping an acquired guard.
struct RunAdmissionLease {
    gates: RunAdmissionGates,
    session_id: SessionId,
    gate: Arc<RunAdmissionGate>,
}

impl Drop for RunAdmissionLease {
    fn drop(&mut self) {
        let previous = self
            .gate
            .leases
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        debug_assert!(previous > 0, "admission lease count underflow");
        if previous != 1 {
            return;
        }

        if let dashmap::mapref::entry::Entry::Occupied(entry) = self.gates.entry(self.session_id) {
            // Acquisition increments `leases` while holding this same map
            // entry. If a new user arrived after our 1 -> 0 transition, the
            // recheck observes it and preserves the entry; otherwise removal
            // linearizes before a later acquirer creates a replacement gate.
            if Arc::ptr_eq(entry.get(), &self.gate)
                && self.gate.leases.load(std::sync::atomic::Ordering::Acquire) == 0
            {
                entry.remove();
            }
        }
    }
}

/// Owns one acquired session admission mutex.
pub(crate) struct RunAdmissionGuard {
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    lease: Option<RunAdmissionLease>,
}

impl Drop for RunAdmissionGuard {
    fn drop(&mut self) {
        drop(self.guard.take());
        drop(self.lease.take());
    }
}

/// Acquire the per-session admission boundary with cancellation-safe ownership.
pub(crate) async fn acquire_run_admission_guard(
    gates: &RunAdmissionGates,
    session_id: SessionId,
) -> RunAdmissionGuard {
    let gate = match gates.entry(session_id) {
        dashmap::mapref::entry::Entry::Occupied(entry) => {
            let gate = entry.get().clone();
            gate.leases
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            gate
        }
        dashmap::mapref::entry::Entry::Vacant(entry) => {
            let gate = Arc::new(RunAdmissionGate {
                mutex: Arc::new(tokio::sync::Mutex::new(())),
                leases: std::sync::atomic::AtomicUsize::new(1),
            });
            entry.insert(gate.clone());
            gate
        }
    };
    let lease = RunAdmissionLease {
        gates: gates.clone(),
        session_id,
        gate,
    };

    #[cfg(test)]
    pause_during_admission_acquire(session_id).await;

    let guard = lease.gate.mutex.clone().lock_owned().await;
    RunAdmissionGuard {
        guard: Some(guard),
        lease: Some(lease),
    }
}

#[cfg(test)]
pub(super) fn install_start_transition_barrier(run_id: RunId) -> Arc<tokio::sync::Barrier> {
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    START_TRANSITION_BARRIERS.insert(run_id, barrier.clone());
    barrier
}

#[cfg(test)]
async fn pause_before_start_transition(run_id: RunId) {
    let barrier = START_TRANSITION_BARRIERS
        .get(&run_id)
        .map(|entry| entry.value().clone());
    if let Some(barrier) = barrier {
        barrier.wait().await;
        barrier.wait().await;
        START_TRANSITION_BARRIERS.remove(&run_id);
    }
}

#[cfg(test)]
pub(super) fn install_terminal_transition_barrier(run_id: RunId) -> Arc<tokio::sync::Barrier> {
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    TERMINAL_TRANSITION_BARRIERS.insert(run_id, barrier.clone());
    barrier
}

#[cfg(test)]
async fn pause_before_terminal_transition(run_id: RunId) {
    let barrier = TERMINAL_TRANSITION_BARRIERS
        .get(&run_id)
        .map(|entry| entry.value().clone());
    if let Some(barrier) = barrier {
        barrier.wait().await;
        barrier.wait().await;
        TERMINAL_TRANSITION_BARRIERS.remove(&run_id);
    }
}

#[cfg(test)]
pub(super) fn install_admission_persistence_barrier(
    session_id: SessionId,
) -> Arc<tokio::sync::Barrier> {
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    ADMISSION_PERSISTENCE_BARRIERS.insert(session_id, barrier.clone());
    barrier
}

#[cfg(test)]
async fn pause_after_admission_persistence(session_id: SessionId) {
    let barrier = ADMISSION_PERSISTENCE_BARRIERS
        .remove(&session_id)
        .map(|(_, barrier)| barrier);
    if let Some(barrier) = barrier {
        barrier.wait().await;
        barrier.wait().await;
    }
}

#[cfg(test)]
pub(super) fn install_admission_event_barrier(session_id: SessionId) -> Arc<tokio::sync::Barrier> {
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    ADMISSION_EVENT_BARRIERS.insert(session_id, barrier.clone());
    barrier
}

#[cfg(test)]
async fn pause_before_admission_event(session_id: SessionId) {
    let barrier = ADMISSION_EVENT_BARRIERS
        .remove(&session_id)
        .map(|(_, barrier)| barrier);
    if let Some(barrier) = barrier {
        barrier.wait().await;
        barrier.wait().await;
    }
}

#[cfg(test)]
pub(super) fn install_admission_execution_barrier(
    session_id: SessionId,
) -> Arc<tokio::sync::Barrier> {
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    ADMISSION_EXECUTION_BARRIERS.insert(session_id, barrier.clone());
    barrier
}

#[cfg(test)]
async fn pause_before_admission_execution(session_id: SessionId) {
    let barrier = ADMISSION_EXECUTION_BARRIERS
        .remove(&session_id)
        .map(|(_, barrier)| barrier);
    if let Some(barrier) = barrier {
        barrier.wait().await;
        barrier.wait().await;
    }
}

#[cfg(test)]
pub(super) fn install_admission_acquire_barrier(
    session_id: SessionId,
) -> Arc<tokio::sync::Barrier> {
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    ADMISSION_ACQUIRE_BARRIERS.insert(session_id, barrier.clone());
    barrier
}

#[cfg(test)]
async fn pause_during_admission_acquire(session_id: SessionId) {
    let barrier = ADMISSION_ACQUIRE_BARRIERS
        .remove(&session_id)
        .map(|(_, barrier)| barrier);
    if let Some(barrier) = barrier {
        barrier.wait().await;
        barrier.wait().await;
    }
}

/// POST /runs/{run_id}/cancel — cancel a running or queued run.
#[instrument(level = "info", skip(state), fields(run_id = %run_id.0))]
pub async fn cancel_run(
    State(state): State<AppState>,
    Path(run_id): Path<RunId>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let already_finished = || {
        api_error(
            StatusCode::CONFLICT,
            "ALREADY_FINISHED",
            "Run already finished",
        )
    };

    let run = state
        .run_manager
        .get_run(run_id)
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "NOT_FOUND", "Run not found"))?;

    match run.status() {
        RunStatus::Queued | RunStatus::Running => {}
        _ => {
            return Err(already_finished());
        }
    }

    if !state.run_manager.has_cancel_token(run_id) {
        return Err(already_finished());
    }

    // #1046: flip the run state to `Cancelled` and broadcast `run_cancelled`
    // SYNCHRONOUSLY from the HTTP handler so the user-visible cancel is
    // authoritative within a single HTTP round trip. Pre-#1046, the state
    // flip only happened in `execute_run`'s terminal arm AFTER `drop(runtime)`
    // and `forwarder_handle.await` had completed (typically several seconds
    // of cleanup latency on Windows with an in-flight LLM HTTP connection),
    // during which `GET /runs/{id}` still reported `Running` and the SSE
    // feed had not emitted `run_cancelled`. Operators perceived this as
    // "cancel didn't work — agent kept running" because the UI's
    // "Cancel" button had no immediate observable effect.
    //
    // The `mark_run_as_cancelled` return-value contract guarantees that
    // the SSE event fires exactly once per run: whichever caller wins the
    // race (this HTTP handler or `execute_run`'s terminal arm) does the
    // broadcast, the loser sees `false` and skips. The race is rare in
    // practice because the cleanup window where both can fire is bounded
    // by `forwarder_handle.await` (~milliseconds without network LLM
    // calls, seconds with) — but the contract closes the window entirely.
    //
    // The state flip also closes a #895-class consistency window: between
    // this point and `execute_run`'s terminal arm, any concurrent
    // `GET /sessions` snapshot or `GET /runs/{id}` query now sees
    // `Cancelled` instead of `Running`, matching the SSE feed.
    match state.run_manager.try_mark_run_as_cancelled(run_id) {
        Ok(true) => {
            let _ = state.run_manager.cancel_run(run_id);
            state
                .run_manager
                .send_event(run_id, run.session_id, SseEventData::run_cancelled(run_id))
                .await;
        }
        Ok(false) => {
            let authoritative = state.run_manager.get_run(run_id);
            if !authoritative.is_some_and(|run| run.status() == RunStatus::Cancelled) {
                return Err(already_finished());
            }
        }
        Err(error) => {
            let message = format!("Run cancellation could not be committed: {error}");
            // The failed durable attempt quarantines the in-memory run as
            // Failed/persistence_failed. Stop the real worker and publish the
            // same terminal boundary immediately so execution cannot continue
            // behind a terminal status surface.
            let _ = state.run_manager.cancel_run(run_id);
            state
                .run_manager
                .send_event(
                    run_id,
                    run.session_id,
                    SseEventData::run_error(run_id, &message),
                )
                .await;
            state
                .run_manager
                .send_agent_event(
                    run.agent_id,
                    run_id,
                    run.session_id,
                    SseEventData::session_activity_ended(run.session_id, run_id, run.agent_id),
                )
                .await;
            return Err(api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "LIFECYCLE_PERSISTENCE_FAILED",
                message,
            ));
        }
    }

    info!("Cancel requested for run {}", run_id.0);

    Ok(Json(serde_json::json!({
        "run_id": run_id.0.to_string(),
        "status": "cancelled",
    })))
}

/// POST /sessions/{session_id}/subagent/cancel — cancel the live subagent
/// running on the given SUBAGENT session.
///
/// Session-keyed on purpose: the UI's subagent surfaces (the status-bar
/// chips and the drilled-down subagent session view) carry the subagent's
/// SESSION id (via `subagent_started`, the `invoke_agent` result and the
/// reload-rehydration path) but not its run id — and the subagent's own run
/// id has no entry in `RunManager::cancel_tokens` anyway (`register_run`
/// via the `RunRegistrar` trait only inserts the run record), so run-keyed
/// `POST /runs/{run_id}/cancel` returns 409 for subagent runs without
/// cancelling anything. This endpoint goes through the coordinator's
/// `SubagentHandle` instead, firing the same child cancellation token the
/// subagent's `select!` waits on.
///
/// Everything downstream is the EXISTING cancellation path: the
/// coordinator's terminal arm flips the task to `Cancelled`, updates the
/// run record, seals the subagent's own session with `run_cancelled`, and
/// (for background subagents) emits the `subagent_completed` notification
/// with status `cancelled` that renders the parent's status-bar chip as
/// *Cancelled*. Note the foreground asymmetry: cancelling a FOREGROUND
/// subagent makes the parent's blocked `invoke_agent` call return an error
/// (`"Subagent was cancelled"`), so the parent continues with a failed tool
/// call and its chip renders *Failed* rather than *Cancelled* — the parent
/// run itself is NOT cancelled either way.
///
/// Returns 200 `{"status":"cancelling"}` when a live subagent was found and
/// its token fired (cancellation completes asynchronously — the terminal
/// events above follow on the streams), or 404 `NO_LIVE_SUBAGENT` when the
/// session has no live subagent (unknown session, or the subagent already
/// reached a terminal state — e.g. a double-click racing natural
/// completion).
#[instrument(level = "info", skip(state), fields(session_id = %session_id.0))]
pub async fn cancel_subagent(
    State(state): State<AppState>,
    Path(session_id): Path<SessionId>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if !state.coordinator.cancel_subagent_by_session(session_id) {
        return Err(api_error(
            StatusCode::NOT_FOUND,
            "NO_LIVE_SUBAGENT",
            "No live subagent for this session",
        ));
    }

    info!("Subagent cancel requested for session {}", session_id.0);

    Ok(Json(serde_json::json!({
        "session_id": session_id.0.to_string(),
        "status": "cancelling",
    })))
}

/// Cross-validate the resolved per-run `(provider, model, max_input_tokens,
/// max_tokens)` quadruple against the provider's published context window
/// (#919).
///
/// Returns:
/// - `Ok(())` when the budget fits, when the table doesn't know the
///   `(provider, model)` pair, when the agent is on the mock LLM, or when
///   the validator is in `warn` mode (in which case the overshoot is
///   downgraded to a structured WARN log).
/// - `Err(TokenBudgetError)` when strict mode is active and the
///   table-known cap is exceeded. The caller (HTTP `create_run` or non-HTTP
///   `execute_run`) maps the structured error onto its own surface — a
///   400 JSON body for the synchronous request path or a `run_error` SSE
///   event + `mark_run_as_failed` for the queued path.
///
/// Centralising the mock-skip, env-mode dispatch, and structured-WARN log
/// in one helper means every run-creation path — HTTP `POST /runs`,
/// scheduler triggers, peer DMs, notification runs, and subagent
/// completion runs — sees the same enforcement contract. The Codex P2
/// follow-up that motivated the `execute_run` call site flagged that the
/// HTTP-only guard left the non-HTTP paths exposed to the same opaque
/// downstream 4xx the validator is meant to prevent.
fn evaluate_pre_flight_token_budget(
    agent_id: AgentId,
    agent_config: &alms_runtime::AgentConfig,
    llm: &alms_runtime::LlmClient,
    rejection_log_message: &str,
) -> Result<(), alms_core::config::TokenBudgetError> {
    // Mock mode: never enforce the cap. Mirrors `AlmsConfig::validate`'s
    // boot-time skip — the mock client does not enforce a real provider's
    // cap, so refusing an intentionally-overshoot budget here would block
    // local/dev mock runs that pass at boot. Same policy in both skip
    // paths (Codex P2 #1 follow-up on PR #1020).
    if llm.is_mock() {
        return Ok(());
    }

    let provider = llm.provider();
    let model = llm.default_model();
    let max_input_tokens = agent_config.context_config.max_input_tokens;
    let max_tokens = agent_config.max_tokens;

    let Err(err) =
        alms_core::config::validate_token_budget(provider, model, max_input_tokens, max_tokens)
    else {
        return Ok(());
    };

    match alms_core::config::ValidationMode::from_env() {
        alms_core::config::ValidationMode::Strict => {
            warn!(
                target: "alms.config",
                agent_id = %agent_id,
                provider = %err.provider,
                model = %err.model,
                max_input_tokens = err.max_input_tokens,
                max_tokens = err.max_tokens,
                effective_total = err.effective_total,
                provider_cap = err.provider_cap,
                "{}",
                rejection_log_message,
            );
            Err(err)
        }
        alms_core::config::ValidationMode::Warn => {
            warn!(
                target: "alms.config",
                agent_id = %agent_id,
                provider = %err.provider,
                model = %err.model,
                max_input_tokens = err.max_input_tokens,
                max_tokens = err.max_tokens,
                effective_total = err.effective_total,
                provider_cap = err.provider_cap,
                "{}",
                err.message()
            );
            Ok(())
        }
    }
}

/// HTTP-shaped wrapper around [`evaluate_pre_flight_token_budget`].
///
/// Mirrors the `MissingModelAfterProviderSwitch` 400 error envelope so
/// every gateway-side budget rejection lands with a consistent shape on
/// the wire. Used only by the synchronous `POST /runs` handler; the
/// queued / non-HTTP path runs the same `evaluate_pre_flight_token_budget`
/// check inline inside `execute_run` (peer-DM, scheduler, notification,
/// subagent-completion runs, and HTTP runs whose effective budget was
/// mutated by `PATCH /settings` or `PATCH /agents` while they sat in the
/// queue) and emits a `run_error` SSE event with the same
/// `INVALID_TOKEN_BUDGET_FOR_PROVIDER` code instead of a synchronous 400,
/// then marks the run `Failed` and broadcasts queue advance — same shape
/// as the `MissingModelAfterProviderSwitch` failure arm immediately above
/// it in `execute_run`.
fn pre_flight_token_budget(
    agent_id: AgentId,
    agent_config: &alms_runtime::AgentConfig,
    llm: &alms_runtime::LlmClient,
) -> Result<(), (StatusCode, axum::Json<serde_json::Value>)> {
    match evaluate_pre_flight_token_budget(
        agent_id,
        agent_config,
        llm,
        "Rejecting POST /runs with INVALID_TOKEN_BUDGET_FOR_PROVIDER (#919)",
    ) {
        Ok(()) => Ok(()),
        Err(err) => Err((
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({
                "error_code": "INVALID_TOKEN_BUDGET_FOR_PROVIDER",
                "message": err.message(),
                "agent_id": agent_id.0.to_string(),
                "provider": err.provider,
                "model": err.model,
                "max_input_tokens": err.max_input_tokens,
                "max_tokens": err.max_tokens,
                "effective_total": err.effective_total,
                "provider_cap": err.provider_cap,
            })),
        )),
    }
}

#[derive(Debug)]
pub struct RunCreationErrorBody(pub serde_json::Value, Option<HeaderValue>);

impl From<Json<serde_json::Value>> for RunCreationErrorBody {
    fn from(body: Json<serde_json::Value>) -> Self {
        Self(body.0, None)
    }
}

impl IntoResponse for RunCreationErrorBody {
    fn into_response(self) -> Response {
        let mut response = Json(self.0).into_response();
        if let Some(retry_after) = self.1 {
            response.headers_mut().insert(RETRY_AFTER, retry_after);
        }
        response
    }
}

fn run_creation_api_error(
    status: StatusCode,
    code: &str,
    message: &str,
) -> (StatusCode, RunCreationErrorBody) {
    let (status, body) = api_error(status, code, message);
    (status, body.into())
}

pub(super) fn queue_admission_error(error: AdmissionError) -> (StatusCode, RunCreationErrorBody) {
    match error {
        AdmissionError::PerKeyFull => (
            StatusCode::TOO_MANY_REQUESTS,
            RunCreationErrorBody(
                serde_json::json!({
                    "error_code": "AGENT_QUEUE_FULL",
                    "message": "This agent has reached its pending run limit",
                    "retryable": true,
                    "retry_after_ms": 1000,
                }),
                Some(HeaderValue::from_static("1")),
            ),
        ),
        AdmissionError::GlobalFull => (
            StatusCode::TOO_MANY_REQUESTS,
            RunCreationErrorBody(
                serde_json::json!({
                    "error_code": "GATEWAY_QUEUE_FULL",
                    "message": "The gateway has reached its pending run limit",
                    "retryable": true,
                    "retry_after_ms": 1000,
                }),
                Some(HeaderValue::from_static("1")),
            ),
        ),
        AdmissionError::ShuttingDown | AdmissionError::DispatchClosed => run_creation_api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "QUEUE_UNAVAILABLE",
            "Run queue is unavailable while the gateway is shutting down",
        ),
    }
}

/// POST /runs - Create a new run
///
/// Per API spec: Returns 201 Created with { run_id, session_id, status: "queued", ts }
#[instrument(level = "info", skip(state, req), fields(session_id = %req.session_id.0))]
pub async fn create_run(
    State(state): State<AppState>,
    Json(req): Json<CreateRunRequest>,
) -> Result<(StatusCode, Json<CreateRunResponse>), (StatusCode, RunCreationErrorBody)> {
    let session = match state.session_manager.get(req.session_id) {
        Ok(session) => session,
        Err(_) => {
            return Err(run_creation_api_error(
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                "Session not found",
            ));
        }
    };

    let input_text = match req.input {
        RunInput::Text { text } => text,
    };

    let session_agent_id = session.agent_id;
    let is_shared_session = session_agent_id.is_nil();
    let agent_id = match req.agent_id {
        Some(requested) if requested != session_agent_id && !is_shared_session => {
            return Err(run_creation_api_error(
                StatusCode::BAD_REQUEST,
                "AGENT_SESSION_MISMATCH",
                "Session belongs to a different agent",
            ));
        }
        Some(requested) => requested,
        None if is_shared_session => {
            return Err(run_creation_api_error(
                StatusCode::BAD_REQUEST,
                "AGENT_ID_REQUIRED",
                "Shared sessions require agent_id so per-agent config can be resolved",
            ));
        }
        None => session_agent_id,
    };

    let run = Run::new(session.id, agent_id, input_text);
    let run_id = run.run_id;
    let session_id = run.session_id;
    let agent_id = run.agent_id;
    let context_id = session.context_id.clone();

    info!("Creating run {} for session {}", run_id.0, session_id.0);

    // Reject new runs during shutdown.
    if state.shutdown_token.is_cancelled() {
        return Err(run_creation_api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "SHUTTING_DOWN",
            "Server is shutting down",
        ));
    }

    // Option C (#1156 review follow-up on #1154): DM sessions are
    // agent-to-agent only — reject the operator path. Peer DM turns are
    // triggered exclusively by `MessageBus` -> `RunTrigger` ->
    // `run_trigger_loop` (notifications.rs), which enqueues with
    // `is_peer_message: true` and never goes through this handler. This
    // handler always enqueues with `is_peer_message: false`, so a
    // POST /runs on a `dm:` session would arm the implicit-reply
    // machinery (`dm_recipient.md` prompt + send_message peer-fold)
    // while the DM completion gate refuses delivery (`NotPeerDm`) — a
    // guaranteed silent drop. Closing the unintended operator path is
    // the structural fix; the RunTrigger path is unaffected by
    // construction.
    if context_id.starts_with("dm:") {
        warn!(
            session_id = %session_id.0,
            context_id = %context_id,
            "Rejecting POST /runs on a DM session with DM_SESSION_NOT_DIRECTLY_RUNNABLE (#1156)"
        );
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error_code": "DM_SESSION_NOT_DIRECTLY_RUNNABLE",
                "message": "DM sessions are agent-to-agent only; turns are \
                            triggered via send_message, not POST /runs.",
                "session_id": session_id.0.to_string(),
                "context_id": context_id.as_str(),
            }))
            .into(),
        ));
    }

    // #1289 item 3: subagent sessions are coordinator-driven only —
    // reject the operator path, same shape and same reasoning as the DM
    // guard above.
    //
    // A subagent turn is produced by `invoke_agent` ->
    // `SubagentDispatcher::dispatch` -> `run_subagent_loop`, which builds
    // its own `AgentRuntime`, records `Run::for_subagent` with the
    // parent's `parent_run_id`, and on completion returns the result to
    // the awaiting parent and emits `subagent_completed`. A `POST /runs`
    // reproduces none of that: no parent linkage, and no delivery — the
    // output lands in the transcript and nowhere else, while the parent
    // blocked in `invoke_agent` never sees it. It can also race a live
    // coordinator loop writing the same session, which is why #1288
    // withdrew the sidebar's delete control from these rows.
    //
    // This is a NEW guard, not a restored one. Before #1278 the id check
    // above rejected this incidentally, but only when the caller supplied
    // an `agent_id` that differed; with `agent_id` omitted the handler
    // already accepted the request and ran under the derived id. #1278
    // filed named subagent sessions under the invoked agent's registry
    // id, which made the supplied-`agent_id` case pass too — so the
    // question #1289 asks is not "put the accident back" but "should this
    // be rejected on principle". It should: the UI already treats these
    // sessions as read-only (`isInternalSession` includes `'subagent'`),
    // so permitting it would make the API contradict its only client,
    // and #1278's listing change now hands an operator the session id
    // (`alms session list --agent`) that `alms run create --session`
    // takes.
    //
    // Keyed on the `subagent_` prefix rather than on a successful parse,
    // so a context this binary cannot decompose (the legacy pre-#1185
    // `subagent_{task_id}` shape) is rejected too rather than falling
    // through the gap.
    if classify_session_type(&context_id) == "subagent" {
        warn!(
            session_id = %session_id.0,
            context_id = %context_id,
            "Rejecting POST /runs on a subagent session with \
             SUBAGENT_SESSION_NOT_DIRECTLY_RUNNABLE (#1289)"
        );
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error_code": "SUBAGENT_SESSION_NOT_DIRECTLY_RUNNABLE",
                "message": "Subagent sessions are coordinator-driven; turns are \
                            triggered via invoke_agent, not POST /runs.",
                "session_id": session_id.0.to_string(),
                "context_id": context_id.as_str(),
            }))
            .into(),
        ));
    }

    // #863: pre-flight per-agent config resolution so a per-agent provider
    // override with no model on any layer is rejected with a clean 400
    // BEFORE any LLM call rather than producing an opaque downstream 4xx
    // (e.g. Anthropic 404 on `model: ""`). The resolve is pure (no
    // side-effects) and `execute_run` will resolve again under the live
    // secrets / agent_config locks at run time — this pre-flight only
    // catches the deterministic config-shape failure mode.
    //
    // #919: when the resolve succeeds, also cross-validate the resulting
    // `(provider, model, max_input_tokens, max_tokens)` quadruple against
    // the provider's published context window. Per-agent overrides
    // (model, provider) can change the effective cap relative to the
    // load-time default, so re-running the validator here catches
    // configurations that pass at boot but blow the cap on a specific
    // agent's resolved shape.
    {
        let base_agent_config = state.agent_config.read().clone();
        // #1148: snapshot the LIVE server-default client, not a boot-time
        // clone, so a `PATCH /settings` model/provider switch is validated
        // (and, below, executed) against the pair the run will actually
        // send on. Bound to a local first so the read guard is released
        // before the match body rather than held across it.
        let server_llm = state.llm.read().clone();
        match resolve_agent_config(
            agent_id,
            &state.session_manager,
            &base_agent_config,
            &server_llm,
            Some(&state.secrets.read()),
        ) {
            Err(ResolveAgentConfigError::MissingModelAfterProviderSwitch {
                agent_id: ag,
                new_provider,
                prev_provider,
            }) => {
                warn!(
                    agent_id = %ag,
                    new_provider = %new_provider,
                    prev_provider = %prev_provider,
                    "Rejecting POST /runs with MISSING_MODEL_AFTER_PROVIDER_SWITCH (#863)"
                );
                return Err((
                    StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({
                        "error_code": "MISSING_MODEL_AFTER_PROVIDER_SWITCH",
                        "message": format!(
                            "agent {ag} overrides provider to {new_provider} but no \
                             model was supplied; previous provider {prev_provider}'s \
                             default cannot be reused"
                        ),
                        "agent_id": ag.0.to_string(),
                        "new_provider": new_provider,
                        "prev_provider": prev_provider,
                    }))
                    .into(),
                ));
            }
            Ok(resolved) => {
                // #919 per-run token-budget validation.
                pre_flight_token_budget(agent_id, &resolved.agent_config, &resolved.llm)
                    .map_err(|(status, body)| (status, body.into()))?;
            }
        }
    }

    // One same-session admission owns the complete durable-to-live boundary.
    // Holding the gate through `run_created` keeps database message order,
    // in-memory history, queue order, and the session feed consistent.
    let admission_guard = acquire_run_admission_guard(&state.run_admission_gates, session_id).await;

    // The session may have been deleted after the request's initial lookup but
    // before this handler acquired the deletion fence.
    state.session_manager.get(session_id).map_err(|_| {
        run_creation_api_error(
            StatusCode::NOT_FOUND,
            "SESSION_NOT_FOUND",
            "Session not found",
        )
    })?;

    // Queue reservation happens before any run, message, cancellation-token,
    // or SSE side effect inside the serialized admission boundary.
    let reservation = state
        .agent_queue
        .try_reserve(agent_id)
        .map_err(queue_admission_error)?;

    // The queued run and its visible user input are one durable admission
    // fact. The message carries `pending_input: true` so execution skips the
    // runtime's legacy input write and cannot duplicate it.
    let user_msg = alms_session::Message {
        id: uuid::Uuid::new_v4().to_string(),
        role: alms_session::Role::User,
        content: alms_session::Content::Text(run.input.clone()),
        timestamp: alms_core::Timestamp::now(),
        metadata: Some(serde_json::json!({
            "pending_input": true,
            "run_id": run_id.0.to_string(),
        })),
    };
    let persisted_touch = state
        .run_manager
        .persist_run_admission(&run, &user_msg)
        .map_err(|error| {
            run_creation_api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "LIFECYCLE_PERSISTENCE_FAILED",
                &format!("Failed to persist run admission: {error}"),
            )
        })?;

    #[cfg(test)]
    pause_after_admission_persistence(session_id).await;

    if let Some(touched_at) = persisted_touch {
        state
            .session_manager
            .append_persisted_message(session_id, user_msg, touched_at)
            .map_err(|error| {
                run_creation_api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "ADMISSION_PROJECTION_FAILED",
                    &format!("Failed to publish persisted run input: {error}"),
                )
            })?;
    } else {
        state
            .session_manager
            .append_message(session_id, user_msg)
            .map_err(|error| {
                run_creation_api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "ADMISSION_PROJECTION_FAILED",
                    &format!("Failed to publish run input: {error}"),
                )
            })?;
    }
    state
        .run_manager
        .insert_persisted_run(run.clone())
        .map_err(|error| {
            run_creation_api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "ADMISSION_PROJECTION_FAILED",
                &format!("Failed to publish persisted run: {error}"),
            )
        })?;
    let input_pre_persisted = true;

    // Dispatch before the first await after durable side effects. If the
    // request is dropped while publishing run_created, dropping start_tx
    // releases the queued work instead of stranding it.
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    let (start_tx, start_rx) = tokio::sync::oneshot::channel();
    let state_clone = state.clone();
    let receipt = match reservation.submit(Box::pin(async move {
        let _ = start_rx.await;
        execute_run_guarded(
            state_clone,
            RunParams {
                run_id,
                session_id,
                agent_id,
                input: run.input,
                context_id,
                cancel_token,
                is_peer_message: false,
                is_system_triggered: false,
                input_pre_persisted,
                dm_ended_peer: None,
            },
        )
        .await;
    })) {
        Ok(receipt) => receipt,
        Err(error) => {
            let persistence_error = state
                .run_manager
                .try_mark_run_as_failed(run_id, "Run queue closed before dispatch".to_string())
                .err();
            state.run_manager.remove_cancel_token(run_id);
            if let Some(persistence_error) = persistence_error {
                return Err(run_creation_api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "LIFECYCLE_PERSISTENCE_FAILED",
                    &format!("Run dispatch failure could not be persisted: {persistence_error}"),
                ));
            }
            return Err(queue_admission_error(error));
        }
    };
    let agent_running = state.run_manager.agent_has_running_run(agent_id);
    let queued_behind = receipt.queued_ahead() + usize::from(agent_running);

    // Notify session-level SSE subscribers that a new run was created.
    //
    // `queued_behind` tells the UI how many runs are ahead of this one so
    // it can show "Queued -- waiting for agent..." instead of a misleading
    // "Thinking...".
    //
    // The dispatch receipt counts submitted normal-priority work at the same
    // linearization point that orders the channel. Low-priority notification
    // work is excluded because this user run overtakes it. The active run has
    // already left the pending queue, so add it separately.
    //
    // Known residual race: there is a narrow sub-millisecond TOCTOU window
    // between `pending.fetch_sub(1)` inside the queue handler and
    // `mark_run_as_running` in `execute_run` (lifecycle.rs:~495). During
    // that window both `pending_count` and `agent_has_running_run` read
    // false, so a concurrent `create_run` can report `queued_behind = 0`
    // and render "Thinking" instead of "Queued". The window is bounded by
    // executor dispatch latency. Closing it would require a separate
    // in-flight counter inside `SessionQueue` that is incremented on
    // dequeue (before `work.await`) and decremented after the work
    // future resolves; considered low priority.
    // The detached task owns both event publication and the start gate.
    // Dropping the HTTP request cannot cancel run_created while allowing
    // the queued work to advance to run_started.
    let event_state = state.clone();
    let event_task = tokio::spawn(async move {
        let _admission_guard = admission_guard;
        #[cfg(test)]
        pause_before_admission_event(session_id).await;

        event_state
            .run_manager
            .send_session_event(
                session_id,
                run_id,
                SseEventData::run_created(
                    run_id,
                    session_id,
                    false,
                    Some("user".to_string()),
                    queued_behind,
                ),
            )
            .await;
        let _ = start_tx.send(());
    });
    let _ = event_task.await;

    let response = CreateRunResponse {
        run_id,
        session_id,
        status: RunStatus::Queued,
        ts: Utc::now(),
    };

    Ok((StatusCode::CREATED, Json(response)))
}

/// Resolve the effective posture for a run.
///
/// System-triggered runs (peer DMs, notification runs, subagent completions)
/// have no human in the loop, so Guarded posture would hang forever waiting
/// for approval. This function overrides Guarded to Autonomous for those
/// runs while leaving all other postures unchanged.
pub(super) fn resolve_posture_for_run(
    posture: alms_runtime::Posture,
    is_system_triggered: bool,
) -> alms_runtime::Posture {
    if is_system_triggered && posture == alms_runtime::Posture::Guarded {
        alms_runtime::Posture::Autonomous
    } else {
        posture
    }
}

/// Extract the peer agent name from a `dm:{name1}:{name2}` context ID.
///
/// Delegates to [`alms_core::dm_peer`].  Returns `None` if the context ID
/// does not match the expected format or neither name matches `agent_name`.
pub(super) fn extract_peer_from_dm_context(context_id: &str, agent_name: &str) -> Option<String> {
    alms_core::dm_peer(context_id, agent_name).map(|s| s.to_string())
}

/// Configure the one `send_message` recipient this run may not address.
///
/// Called once per run at the tool-registration site in [`execute_run`]. At
/// most one recipient is ever folded, and the two runs that fold one do so
/// for opposite reasons:
///
/// - **A peer-triggered DM turn** (`is_peer_message`) folds its current peer,
///   read off the `dm:{a}:{b}` context id — #1154 design default #2. The
///   agent's final assistant text IS the reply, delivered by the DM
///   completion gate, so a `send_message` at the peer would double-deliver.
///   Gated on `is_peer_message` (#1156 defense-in-depth) because the fold is
///   only correct when the gate actually delivers, and the gate is armed
///   exclusively for peer-triggered DM runs. Option C already rejects
///   non-peer runs on `dm:` sessions at `create_run`, so a `dm:` context
///   cannot reach the other arm today — the gate keeps that explicit should
///   a new non-peer `dm:` path ever appear (folding without delivery would
///   silently drop the message).
///
/// - **A `ConversationEnded` post-end turn** (#1299) folds the peer whose
///   conversation just ended, carried on the trigger. Here there is no
///   completion gate to double-deliver past: the send would simply re-open
///   the conversation the agent was just told had ended, at depth 1.
///   `MAX_DM_DEPTH` bounds one conversation and `end_conversation` resets it,
///   so nothing else stops a pair looping cap → end → re-open → cap forever.
///   These runs already had half this treatment — `notifications.rs`
///   withholds the DM addendum from them on exactly the "not a peer message"
///   reasoning — and the fold is the other half.
///
/// The ended-peer arm is keyed on the trigger, NOT on `context_id`, because
/// the post-end turn runs on the agent's web-chat, its `notifications:`
/// session, or — when the end resolved an open job episode (#1198 / #1205) —
/// a `job_*` session. None of those name the peer, and the job arm is both
/// the one that survives the #1258 interrupted-end suppression and the one
/// that re-opens with nobody watching.
///
/// The two arms are mutually exclusive by construction: `dm_ended_peer` is
/// set only by `run_trigger_loop`'s `ConversationEnded` arm, which enqueues
/// with `is_peer_message: false`.
///
/// Every other run folds nothing, and no run loses `send_message` for any
/// recipient other than the single folded name.
pub(super) fn apply_send_message_fold(
    tool: alms_tools::SendMessageTool,
    is_peer_message: bool,
    context_id: &str,
    agent_name: &str,
    dm_ended_peer: Option<&str>,
) -> alms_tools::SendMessageTool {
    if is_peer_message {
        tool.with_dm_peer(extract_peer_from_dm_context(context_id, agent_name))
    } else {
        tool.with_ended_dm_peer(dm_ended_peer.map(str::to_string))
    }
}

/// RAII guard that calls [`RunManager::untrack_in_flight`] on drop.
///
/// This ensures the in-flight counter is always decremented even when the
/// run task panics, preventing `wait_drain` from blocking indefinitely.
struct InFlightGuard {
    run_manager: crate::server::RunManager,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.run_manager.untrack_in_flight();
    }
}

/// Build the LLM client used for the per-run summary task (#866 + #871).
///
/// Thin wrapper around `alms_runtime::build_summary_client` that adds the
/// gateway-side contextual `info!` log (the runtime helper is shared with the
/// coordinator subagent path and stays log-agnostic so each caller can label
/// with its own ID — `agent_id` here, `task_id` in the coordinator). All the
/// leak-guard logic lives in the runtime helper so future drift is impossible.
///
/// See `alms_runtime::summary_client` for the full rule set.
fn build_summary_client(
    llm: &alms_runtime::LlmClient,
    summary_provider: Option<&str>,
    summary_model: Option<&str>,
    secrets: &alms_core::secrets::SecretsStore,
    agent_id: AgentId,
) -> alms_runtime::LlmClient {
    let switched =
        alms_runtime::build_summary_client(llm, summary_provider, summary_model, Some(secrets));

    if let Some(provider) = summary_provider {
        // #1191 made `Some(openrouter, gemma)` the compiled default, so
        // this branch now runs on every stock deployment's runs — only an
        // operator-configured pair is `info!`-worthy; the default pair is
        // routine and logs at `debug!` (PR #1194).
        if alms_core::config::ContextConfig::is_compiled_default_summary_pair(
            summary_provider,
            summary_model,
        ) {
            debug!(
                agent_id = %agent_id,
                agent_provider = %llm.provider(),
                summary_provider = %provider,
                summary_model = %summary_model.unwrap_or("<inherit>"),
                "Using dedicated provider for summary task (#866, compiled default pair)"
            );
        } else {
            info!(
                agent_id = %agent_id,
                agent_provider = %llm.provider(),
                summary_provider = %provider,
                summary_model = %summary_model.unwrap_or("<inherit>"),
                "Using dedicated provider for summary task (#866)"
            );
        }
    }

    switched
}

/// Maximum bytes of an `AlmsError` Display string to embed in the
/// `ConversationEndReason::Errored { message }` payload.
///
/// Bounds the size of the peer-side notification text so a verbose error
/// (e.g. an LLM JSON dump) does not balloon the `dm_ended` marker / SSE
/// frame. Truncation is done on a UTF-8 char boundary.
const PEER_ERROR_MESSAGE_MAX_LEN: usize = 300;

/// Build the human-readable error string carried in
/// `ConversationEndReason::Errored { message }` for the peer-side
/// notification.
///
/// Routes the error through [`sanitize_error_for_session`] first so any
/// raw provider details (URLs, API keys, response bodies) are collapsed
/// to a category label before reaching the peer agent's notification
/// context. This closes the same threat surface that #911 / #930 closed
/// on the failing agent's own session — see issue #931. Sanitisation is
/// inlined (rather than bolted onto the call sites) so future callers
/// of this helper cannot accidentally skip it.
///
/// After sanitisation the result is truncated to
/// [`PEER_ERROR_MESSAGE_MAX_LEN`] on a UTF-8 char boundary and an
/// ellipsis is appended when truncated. In practice the sanitiser
/// already collapses to a short fixed label, so the truncation step is
/// effectively a no-op — but it remains a safety net in case the
/// sanitiser's contract widens in the future.
pub(super) fn truncate_error_for_peer(err: &AlmsError) -> String {
    let s = sanitize_error_for_session(err);
    if s.len() <= PEER_ERROR_MESSAGE_MAX_LEN {
        return s;
    }
    // Walk back from PEER_ERROR_MESSAGE_MAX_LEN to a char boundary.
    let mut end = PEER_ERROR_MESSAGE_MAX_LEN;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut truncated = s[..end].to_string();
    truncated.push_str("...");
    truncated
}

/// Compose a peer-facing failure message from a self-authored `prefix` and a
/// foreign `error` string.
///
/// Some `Errored` ends describe *what* failed in our own words and only
/// interpolate a foreign error as the tail ("reply delivery failed: {e}",
/// "Run panic could not be persisted: {e}"). Those tails are the leak vector
/// — a `SendError::Internal` wraps storage errors that can carry database
/// paths — so only the tail goes through [`truncate_error_for_peer`].
///
/// The prefix is deliberately kept outside the sanitiser: it is our text, it
/// is secret-free, and it carries the useful "what failed" half. Routing the
/// whole composed string through `sanitize_error_for_session` instead would
/// match none of its keywords and collapse the lot to the bare label
/// `"Runtime error"`.
///
/// #1258 made this matter on two new surfaces: the peer-facing message is now
/// rendered in the browser's DM-ended banner (`detail`) and persisted as the
/// `dm_ended_notification` marker's own text, not just fed to a notification
/// run's LLM context.
pub(super) fn peer_error_with_prefix(prefix: &str, error: &str) -> String {
    format!(
        "{prefix}{}",
        truncate_error_for_peer(&AlmsError::Runtime(error.to_string()))
    )
}

/// Resolve cancellation/completion persistence provenance from the
/// authoritative run snapshot, then sanitize it for DM history.
///
/// A caller may observe `Ok(false)` after an earlier HTTP/approval path
/// already quarantined the run. In that case the local transition carries no
/// error, but `terminal_reason = persistence_failed` preserves the cause.
pub(super) fn lifecycle_persistence_error_for_peer(
    state: &AppState,
    run_id: RunId,
    observed_error: Option<String>,
) -> Option<String> {
    let authoritative_error = state.run_manager.get_run(run_id).and_then(|run| {
        (run.terminal_reason() == Some("persistence_failed")).then(|| {
            run.error
                .unwrap_or_else(|| "lifecycle persistence failed".to_string())
        })
    });
    authoritative_error
        .or(observed_error)
        .map(|message| truncate_error_for_peer(&AlmsError::Runtime(message)))
}

/// Persist a synthetic notification input before allowing the runtime to
/// consume it.
///
/// `SessionManager::append_message` intentionally treats SQLite writes as
/// best-effort and only logs a persistence failure. That contract is useful
/// for ordinary transient chat history, but it is unsafe for notification
/// runs: `run_on_session` assumes this synthetic user turn is already durable
/// and therefore will not persist the input itself. Persist explicitly first
/// so a storage failure stops the run instead of producing an assistant reply
/// whose triggering input disappears after restart.
fn persist_notification_input(
    session_manager: &alms_session::SessionManager,
    session_id: SessionId,
    message: alms_session::Message,
) -> alms_core::AlmsResult<()> {
    if let Some(store) = session_manager.store() {
        let touched_at = message.timestamp;
        store.save_message(session_id, &message)?;
        session_manager.append_persisted_message(session_id, message, touched_at)?;

        // Preserve append_message's last-activity write-through semantics.
        // The notification input itself is already durable at this point, so
        // a timestamp-only persistence failure remains best-effort.
        if let Ok(session) = session_manager.get(session_id)
            && let Err(error) = store.save_session(&session)
        {
            warn!(
                session_id = %session_id.0,
                %error,
                "Failed to persist notification session activity timestamp"
            );
        }
        Ok(())
    } else {
        session_manager.append_message(session_id, message)
    }
}

/// Broadcast a `run_queue_position` SSE event for every still-queued run on
/// the given agent (#831).
///
/// Called when the head of the per-agent queue is about to advance — i.e. at
/// every terminal exit of [`execute_run`]. For each remaining `Queued` run
/// (FIFO-sorted), assigns a 1-indexed position matching the same semantic as
/// `run_created.queued_behind` and fans the event out on both the per-run and
/// per-session SSE feeds.
///
/// Position numbering: if any `Running` run still exists for the agent, the
/// first queued run is position 1 (next up); otherwise the first queued run
/// is position 0 and is **skipped** — `run_started` will fire for it shortly.
/// Subsequent queued runs are numbered sequentially.
///
/// The broadcast also tolerates the narrow TOCTOU window between
/// `pending.fetch_sub(1)` inside the queue handler and `mark_run_as_running`:
/// in either ordering the FIFO-sorted Queued runs still produce monotonically
/// decremented positions matching what a fresh `create_run` would compute via
/// `pending_count + agent_has_running_run`.
async fn broadcast_queue_advance(state: &AppState, agent_id: AgentId) {
    let queued = state.run_manager.list_queued_for_agent(agent_id);
    if queued.is_empty() {
        return;
    }
    let running_offset = usize::from(state.run_manager.agent_has_running_run(agent_id));
    for (idx, run) in queued.iter().enumerate() {
        let position = idx + running_offset;
        if position == 0 {
            // First queued run is about to be picked up by the queue handler;
            // `run_started` will fire for it shortly, so we don't emit a
            // misleading position-zero event.
            continue;
        }
        state
            .run_manager
            .send_event(
                run.run_id,
                run.session_id,
                SseEventData::run_queue_position(run.run_id, run.session_id, agent_id, position),
            )
            .await;
    }
}

/// Execute a run in background, forwarding runtime events to SSE.
#[instrument(level = "info", skip(state, params), fields(run_id = %params.run_id.0, session_id = %params.session_id.0))]
pub(super) async fn execute_run(state: AppState, params: RunParams) {
    let RunParams {
        run_id,
        session_id,
        agent_id,
        input,
        context_id,
        cancel_token,
        is_peer_message,
        is_system_triggered,
        input_pre_persisted,
        dm_ended_peer,
    } = params;
    // Track this run for graceful shutdown drain.  The guard ensures the
    // counter is decremented even if this function panics.
    state.run_manager.track_in_flight();
    let _in_flight_guard = InFlightGuard {
        run_manager: state.run_manager.clone(),
    };

    // #1198: job-stamped runs (turn-1 `Run::for_job` and episode
    // continuation runs stamped by `enqueue_triggered_run`) feed the job
    // episode tracker at EVERY exit of this function — the five sites are
    // the pre-cancel early exit, the resolve-failure exit, the token-budget
    // rejection exit, the runtime-construction failure exit, and the common
    // terminal tail. `finish_episode_run` is a no-op for `None`.
    let episode_job_id = state.run_manager.get_run(run_id).and_then(|r| r.job_id);

    if input_pre_persisted {
        #[cfg(test)]
        pause_before_admission_execution(session_id).await;

        // Settle the accepted prompt as soon as its queue slot begins. This
        // deliberately precedes the queued-cancellation/shutdown exit as well
        // as config resolution, budget checks, start persistence, and runtime
        // construction: every terminal accepted run must leave its prompt
        // available to later context, while later queue slots stay hidden.
        if let Err(error) = state
            .session_manager
            .claim_pending_input(session_id, run_id)
        {
            let mut failure_message = format!("Failed to claim accepted run input: {error}");
            let emit_failure = match state
                .run_manager
                .try_mark_run_as_failed(run_id, failure_message.clone())
            {
                Ok(transitioned) => transitioned,
                Err(persistence_error) => {
                    failure_message = format!(
                        "Run input claim failure could not be persisted: {persistence_error}"
                    );
                    true
                }
            };
            if emit_failure {
                state
                    .run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::run_error(run_id, &failure_message),
                    )
                    .await;
            }
            state
                .run_manager
                .send_agent_event(
                    agent_id,
                    run_id,
                    session_id,
                    SseEventData::session_activity_ended(session_id, run_id, agent_id),
                )
                .await;
            state.run_manager.remove_senders(run_id);
            state.run_manager.remove_cancel_token(run_id);
            state.approval_store.clear_for_run(run_id);
            super::notifications::finish_episode_run(&state, episode_job_id, run_id, &[]).await;
            broadcast_queue_advance(&state, agent_id).await;
            return;
        }
    }

    // Early exit if already cancelled (queued-then-cancelled before execution
    // started) or if the server is shutting down.  The shutdown_token check
    // prevents the SessionQueue drain from starting NEW runs during graceful
    // shutdown -- they would increment the in-flight counter and potentially
    // outlive the drain timeout.
    if cancel_token.is_cancelled() || state.shutdown_token.is_cancelled() {
        // #895: flip the run state BEFORE broadcasting the SSE event.
        // `RunManager::get_session_runs()` uses `RunState` membership to
        // compute `has_active_run` for `GET /sessions` (#890 / #892). If we
        // broadcast first and flip second, a concurrent client opening
        // `GET /sessions` between the broadcast and the state flip observes
        // `has_active_run: true` while the SSE feed has already moved past
        // the `ended` event — a subsequent `last_event_id`-based reconnect
        // won't replay it, and the sidebar's "active" indicator stays stuck
        // until the next unrelated event clears it. Flipping first makes
        // "a client that sees `has_active_run: true`" a strict prefix of
        // "a client that has the started event but not the ended event,"
        // closing the race. The change is internal to the gateway lock
        // window — the SSE event ordering visible to clients is identical.
        //
        // #1046 / #1052: gate the broadcast on the transition bool. The
        // HTTP `cancel_run` handler (#1046) may have already flipped the
        // state and emitted the SSE event before this `execute_run` task
        // was dispatched (queued-then-cancelled-via-HTTP). Likewise, a
        // concurrent path (graceful shutdown's `cancel_all_in_flight`,
        // `cancel_runs_for_session`, ...) may have already driven the run
        // terminal (#1052). In either case the state is already
        // `Cancelled` so `mark_run_as_cancelled` returns false and we
        // skip the duplicate broadcast. The synthetic
        // `session_activity_ended` below still fires because it serves a
        // separate sidebar-indicator-clearing purpose.
        let (transitioned, persistence_error) =
            match state.run_manager.try_mark_run_as_cancelled(run_id) {
                Ok(transitioned) => (transitioned, None),
                Err(error) => {
                    let message = format!("Run cancellation could not be persisted: {error}");
                    state
                        .run_manager
                        .send_event(
                            run_id,
                            session_id,
                            SseEventData::run_error(run_id, &message),
                        )
                        .await;
                    (false, Some(message))
                }
            };
        if transitioned {
            state
                .run_manager
                .send_event(run_id, session_id, SseEventData::run_cancelled(run_id))
                .await;
        }
        // Emit a synthetic `session_activity_ended` on the agent-scoped
        // feed (#888). The pre-cancel branch never emits a paired
        // `session_activity_started`, so this is asymmetric — but the run
        // was visible as `Queued` between insertion and cancellation, so
        // any concurrent `GET /sessions` snapshot will have observed
        // `has_active_run: true` and lit the sidebar's "active" indicator.
        // Without this `ended` event, that indicator stays stuck until the
        // page reloads. The consumer treats the snapshot as the source of
        // truth for "indicator on" and `ended` as the universal "indicator
        // off" signal, so the missing `started` is harmless: clients with
        // no indicator simply ignore the event.
        state
            .run_manager
            .send_agent_event(
                agent_id,
                run_id,
                session_id,
                SseEventData::session_activity_ended(session_id, run_id, agent_id),
            )
            .await;
        state.run_manager.remove_cancel_token(run_id);
        state.run_manager.remove_senders(run_id);

        // S1 (#1154): if this was a peer-triggered DM run cancelled while
        // still `Queued` (HTTP cancel, #1109 deny cascade,
        // `cancel_runs_for_session`, or shutdown), signal the peer that the
        // conversation ended with `UserCancelled`. This is the single
        // chokepoint every queued-then-cancelled run passes through — the
        // synchronous `cancel_run` handler only flips state + emits
        // `run_cancelled`, and the `Ok`/`Cancelled` arms below never run for
        // a run cancelled before the loop started. Without this the peer was
        // stranded until the `DEPTH_EXPIRY_SECS` sweep. `agent_name` is not
        // resolved yet here (this exit precedes `resolve_agent_config`), so
        // the helper re-resolves it from the registry by `agent_id`. The
        // helper is a no-op for non-peer / non-`dm:` runs.
        let persistence_peer_error =
            lifecycle_persistence_error_for_peer(&state, run_id, persistence_error);
        if let Some(message) = persistence_peer_error {
            super::dm_lifecycle::notify_dm_peer_of_setup_failure(
                &state,
                &run_id,
                &session_id,
                agent_id,
                None,
                &context_id,
                is_peer_message,
                message,
            )
            .await;
        } else {
            super::dm_lifecycle::notify_dm_peer_of_setup_cancellation(
                &state,
                &run_id,
                &session_id,
                agent_id,
                None,
                &context_id,
                is_peer_message,
            )
            .await;
        }

        info!("Run {} was cancelled before starting", run_id.0);
        // #1198 exit 1/5: a queued-then-cancelled episode run never
        // executed, so it opened no async work — feed the tracker with an
        // empty record set so its in-flight reservation is released (a
        // missed release stalls the episode until the deadline sweep).
        super::notifications::finish_episode_run(&state, episode_job_id, run_id, &[]).await;
        // The queue head is advancing — fan out updated positions to any
        // remaining queued runs on this agent (#831).
        broadcast_queue_advance(&state, agent_id).await;
        return;
    }

    // Events are persisted to the event log regardless of whether an SSE
    // client is connected. The SSE client registers its own sender when it
    // calls GET /runs/{id}/events.

    // ---------------------------------------------------------------------
    // Resolve the layered run config UP FRONT (#837).
    //
    // The per-agent > server-default merge — including the bootstrap-prompt
    // swap and the system-triggered posture override — happens here,
    // **before** the `Queued` → `Running` transition fires. Two reasons:
    //
    // 1. Triage: the resolved snapshot is persisted alongside the
    //    `Running` state and broadcast on the `run_started` SSE so live
    //    observers and post-hoc `GET /runs/{id}` calls can confirm the
    //    provider / model / posture / budgets the run actually committed
    //    to. This makes "I set model X but Y was used" reports falsifiable
    //    from stored data alone (#833).
    // 2. Atomicity: marking the run `Running` and attaching its snapshot
    //    in a single `mark_run_as_running_with_config` call means SQLite
    //    never observes a torn state where status is `Running` but the
    //    snapshot is absent.
    //
    // Per-run config overrides were removed in the #941 pivot — the run
    // config is determined entirely by `resolve_agent_config` (per-agent
    // > server default). Operators change agent config via `PATCH
    // /agents/{id}` (or server defaults via `PATCH /settings`) before
    // starting the run; `POST /runs` carries no config knobs. Removing
    // the per-run path closes the leak family at #833 / #860 / #863 /
    // #939 by deleting the pathway that produced them.
    // ---------------------------------------------------------------------

    // Resolve per-agent config (model, posture, provider, reasoning
    // budgets, summary provider/model) from the agent registry on top of
    // the server default. The returned `LlmClient` already carries the
    // per-agent provider switch (with secrets re-resolved) and any
    // per-agent model override.
    //
    // `resolve_agent_config` can fail with #863 `MissingModelAfterProviderSwitch`
    // when a per-agent provider override is set with no model on any layer.
    // The HTTP `create_run` handler runs the same resolve up-front and
    // rejects with `400 MISSING_MODEL_AFTER_PROVIDER_SWITCH`, so this path
    // is only reachable for non-HTTP triggers (Telegram / scheduler / peer
    // DMs / subagent completions). Mark the run as failed with the same
    // structured message so the run record carries an actionable error.
    let base_agent_config = state.agent_config.read().clone();
    // #1148: the server-default `(model, provider)` pair is live-mutable
    // via `PATCH /settings`, so read the shared client here rather than a
    // boot-time clone. Agents carrying a per-agent override still win —
    // `resolve_agent_config` layers those on top of whatever base it is
    // handed. In-flight runs are unaffected: each run resolves once, here,
    // and holds the resulting client by value for its duration.
    let server_llm = state.llm.read().clone();
    let resolve_outcome = {
        let secrets_guard = state.secrets.read();
        resolve_agent_config(
            agent_id,
            &state.session_manager,
            &base_agent_config,
            &server_llm,
            Some(&secrets_guard),
        )
    };
    let resolved = match resolve_outcome {
        Ok(resolved) => resolved,
        Err(e) => {
            error!("Run {} failed to resolve agent config: {}", run_id.0, e);
            // #1046: flip state FIRST (matching the #895 ordering on
            // all started/ended boundaries) and gate the `run_error`
            // SSE broadcast on the transition bool. The HTTP
            // `cancel_run` handler may have flipped the queued run to
            // `Cancelled` while `execute_run` was sitting in the agent
            // queue; in that case `mark_run_as_failed` returns false
            // and we skip the duplicate `run_error` event — the cancel
            // handler's `run_cancelled` already filled the terminal
            // slot.
            //
            // The `session_activity_ended` agent-feed fan-out fires
            // unconditionally here. This branch is reached BEFORE
            // `mark_run_as_running_with_config` and so no
            // `session_activity_started` has been emitted yet — the
            // extra `ended` is harmless (consumers ignore ended events
            // for sessions whose indicator was never lit) and matches
            // the pre-#1046 behaviour.
            let mut failure_message = e.to_string();
            let (_failed_transitioned, emit_failure) = match state
                .run_manager
                .try_mark_run_as_failed(run_id, failure_message.clone())
            {
                Ok(transitioned) => (transitioned, transitioned),
                Err(error) => {
                    failure_message = format!("Run failure could not be persisted: {error}");
                    (false, true)
                }
            };

            if emit_failure {
                state
                    .run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::run_error(run_id, &failure_message),
                    )
                    .await;
            }
            state
                .run_manager
                .send_agent_event(
                    agent_id,
                    run_id,
                    session_id,
                    SseEventData::session_activity_ended(session_id, run_id, agent_id),
                )
                .await;
            // B4 (#1154): a peer-triggered DM run that dies on config
            // resolution must still notify the DM peer — otherwise the
            // peer's depth entry stays live until the 1800s sweep and the
            // peer waits on a reply that will never come. `agent_name` is
            // not resolved yet on this arm; the helper re-resolves it from
            // the registry by `agent_id`.
            super::dm_lifecycle::notify_dm_peer_of_setup_failure(
                &state,
                &run_id,
                &session_id,
                agent_id,
                None,
                &context_id,
                is_peer_message,
                truncate_error_for_peer(&alms_core::AlmsError::Runtime(failure_message)),
            )
            .await;

            state.run_manager.remove_senders(run_id);
            state.run_manager.remove_cancel_token(run_id);
            state.approval_store.clear_for_run(run_id);
            // #1198 exit 2/5: release the episode reservation (no tool
            // calls — the run died before the loop).
            super::notifications::finish_episode_run(&state, episode_job_id, run_id, &[]).await;
            broadcast_queue_advance(&state, agent_id).await;
            return;
        }
    };
    let agent_name = resolved.agent_name;
    if state.workspace_dir.is_some() && agent_name.is_none() {
        warn!(
            "Agent {} has no registry record — workspace and bootstrap skipped",
            agent_id.0
        );
    }

    let mut agent_config = resolved.agent_config;
    let llm = resolved.llm;

    // #919 per-run token-budget validation — non-HTTP path mirror of the
    // `create_run` pre-flight check above (Codex P2 follow-up on PR #1020).
    // The HTTP path validates at `POST /runs` time and returns a structured
    // 400 to the caller. Runs enqueued via `enqueue_triggered_run` (peer
    // DMs, scheduler triggers, notification runs, subagent completion runs)
    // skip that path entirely and land here. We also re-validate HTTP runs
    // that were created earlier and sat in the queue while `PATCH /settings`
    // or `PATCH /agents/{id}` mutated their effective budget — the
    // create-time pre-flight is no longer authoritative by the time
    // `execute_run` resolves the live snapshot, and without this second
    // check we would still reach the provider with an over-budget request
    // (the exact opaque-downstream-4xx symptom #919 is meant to prevent).
    //
    // On a strict-mode reject we emit a structured `run_error` SSE event
    // with the same `INVALID_TOKEN_BUDGET_FOR_PROVIDER` code the HTTP 400
    // body carries (clients branch on the same code regardless of which
    // surface delivered the failure), mark the run as `Failed` with the
    // human-readable message, and broadcast queue advance so any
    // queued-behind runs see their positions decrement. The `Queued` →
    // `Failed` transition fires here BEFORE `mark_run_as_running_with_config`,
    // so the run never enters the running set — same shape as the
    // `MissingModelAfterProviderSwitch` failure arm immediately above.
    if let Err(budget_err) = evaluate_pre_flight_token_budget(
        agent_id,
        &agent_config,
        &llm,
        "Failing queued run with INVALID_TOKEN_BUDGET_FOR_PROVIDER (#919)",
    ) {
        let message = budget_err.message();
        error!(
            "Run {} rejected by token-budget validator before LLM call: {}",
            run_id.0, message
        );
        // Flip the run before publishing terminal session activity so
        // `send_agent_event` snapshots the authoritative post-transition
        // `has_active_run` value. Keep `run_error` first: clients rely on
        // that pre-existing rejection-message wire ordering (#919).
        // #1046 / #1052: bool intentionally ignored - this pre-flight arm
        // runs before worker dispatch. If cancellation won concurrently,
        // the terminal state is already absorbing and the activity snapshot
        // below is still false for this run.
        let persistence_error = state
            .run_manager
            .try_mark_run_as_failed(run_id, message.clone())
            .err()
            .map(|error| format!("Run failure could not be persisted: {error}"));
        let peer_message = persistence_error
            .as_deref()
            .unwrap_or(message.as_str())
            .to_string();
        let failure_event = if let Some(error) = persistence_error.as_deref() {
            SseEventData::run_error(run_id, error)
        } else {
            SseEventData::run_error_with_code(run_id, "INVALID_TOKEN_BUDGET_FOR_PROVIDER", &message)
        };
        state
            .run_manager
            .send_event(run_id, session_id, failure_event)
            .await;
        state
            .run_manager
            .send_agent_event(
                agent_id,
                run_id,
                session_id,
                SseEventData::session_activity_ended(session_id, run_id, agent_id),
            )
            .await;
        // #1046 / #1052: bool intentionally ignored — this is the
        // pre-flight rejection path before worker dispatch. The state flip
        // now happens above the terminal activity emission (#1220 review).

        // B4 (#1154): notify the DM peer when a peer-triggered DM run is
        // rejected by the token-budget validator — same rationale as the
        // resolve-failure arm above. `agent_name` IS resolved on this arm.
        super::dm_lifecycle::notify_dm_peer_of_setup_failure(
            &state,
            &run_id,
            &session_id,
            agent_id,
            agent_name.as_deref(),
            &context_id,
            is_peer_message,
            truncate_error_for_peer(&alms_core::AlmsError::Runtime(peer_message)),
        )
        .await;

        state.run_manager.remove_senders(run_id);
        state.run_manager.remove_cancel_token(run_id);
        state.approval_store.clear_for_run(run_id);
        // #1198 exit 3/5: release the episode reservation.
        super::notifications::finish_episode_run(&state, episode_job_id, run_id, &[]).await;
        broadcast_queue_advance(&state, agent_id).await;
        return;
    }

    // System-triggered runs (peer DMs, notifications, subagent completions)
    // have no human in the loop, so Guarded posture would hang forever
    // waiting for approval.  Force Autonomous posture for these runs.
    let posture_resolved = resolve_posture_for_run(agent_config.posture, is_system_triggered);
    if posture_resolved != agent_config.posture {
        info!(
            "Run {} is system-triggered — overriding {:?} posture to {:?}",
            run_id.0, agent_config.posture, posture_resolved
        );
    }
    agent_config.posture = posture_resolved;

    // Override system prompt with bootstrap prompt for first-time agents.
    // Must come after per-agent overrides so bootstrap takes precedence.
    // Only mutates `system_prompt`; the layered fields the snapshot tracks
    // (provider/model/posture/budgets/debug) are unaffected.
    let agent_config =
        if let (Some(workspace_dir), Some(name)) = (&state.workspace_dir, &agent_name) {
            let workspace = alms_runtime::AgentWorkspace::new(workspace_dir, name);
            if workspace.needs_bootstrap() {
                info!(
                    "Agent {} ({}) has no personality.md — using bootstrap prompt",
                    name, agent_id.0
                );
                let mut cfg = agent_config;
                cfg.system_prompt = alms_runtime::AgentWorkspace::bootstrap_prompt().to_string();
                cfg
            } else {
                agent_config
            }
        } else {
            agent_config
        };

    // NOTE on debug_mode for notification runs: a #546-era convenience used
    // to force `debug_mode = true` here for system-triggered non-peer runs
    // landing on a user-facing session (subagent-completion and DM-ended
    // notification runs — job completions never hit this path because
    // `notify_job_completion` emits SSE + a history marker without creating
    // a run), because pre-#1003 there was no operator-facing way to
    // enable the context-debug view for those runs at all. Post-#1003 the
    // per-agent `debug_mode` toggle is the single source of truth (merged in
    // `resolve_agent_config`), and the flip silently overrode a toggle the
    // user had set to off — the "Context sent to LLM" row appeared on the
    // parent's subagent-completion turn with debug mode disabled. The flip
    // was removed so notification runs honor the same per-agent gate as
    // every other run; operators who want the notification-run context can
    // enable Debug mode on the agent in Settings.

    // Snapshot the fully-layered config now that all post-resolution
    // transforms have settled. Fed into both the persisted `Run` row and
    // the `run_started` SSE payload below (#837).
    let resolved_config = build_resolved_config(&agent_config, &llm);

    #[cfg(test)]
    pause_before_start_transition(run_id).await;

    // #895: flip the run state BEFORE broadcasting `run_started`. See the
    // pre-cancel branch above for the full rationale — broadcasting first
    // leaves a narrow window where a concurrent `GET /sessions` observes
    // `has_active_run: false` (the run hasn't been added to the running
    // set yet) while the `started` event has already been delivered, so
    // the sidebar misses the activity. Flipping first guarantees that
    // any `has_active_run: true` observation is followed by an `ended`
    // event the client will see.
    //
    // Atomic-with-snapshot variant (#837): the layered config is stored
    // alongside the status flip in a single SQLite upsert.
    let started = match state
        .run_manager
        .try_mark_run_as_running_with_config(run_id, resolved_config.clone())
    {
        Ok(started) => started,
        Err(error) => {
            let message = format!("Run start could not be persisted: {error}");
            error!(run_id = %run_id.0, "{message}");
            state
                .run_manager
                .send_event(
                    run_id,
                    session_id,
                    SseEventData::run_error(run_id, &message),
                )
                .await;
            state
                .run_manager
                .send_agent_event(
                    agent_id,
                    run_id,
                    session_id,
                    SseEventData::session_activity_ended(session_id, run_id, agent_id),
                )
                .await;
            state.run_manager.remove_cancel_token(run_id);
            state.run_manager.remove_senders(run_id);
            state.approval_store.clear_for_run(run_id);
            super::dm_lifecycle::notify_dm_peer_of_setup_failure(
                &state,
                &run_id,
                &session_id,
                agent_id,
                agent_name.as_deref(),
                &context_id,
                is_peer_message,
                truncate_error_for_peer(&alms_core::AlmsError::Runtime(message)),
            )
            .await;
            super::notifications::finish_episode_run(&state, episode_job_id, run_id, &[]).await;
            broadcast_queue_advance(&state, agent_id).await;
            return;
        }
    };
    if !started {
        // Cancellation may win after the early pre-cancel check while config
        // and runtime setup are in progress. The state machine rejects the
        // stale start, so do not emit run_started or execute the agent loop.
        state
            .run_manager
            .send_agent_event(
                agent_id,
                run_id,
                session_id,
                SseEventData::session_activity_ended(session_id, run_id, agent_id),
            )
            .await;
        state.run_manager.remove_cancel_token(run_id);
        state.run_manager.remove_senders(run_id);
        state.approval_store.clear_for_run(run_id);
        if let Some(message) = lifecycle_persistence_error_for_peer(&state, run_id, None) {
            super::dm_lifecycle::notify_dm_peer_of_setup_failure(
                &state,
                &run_id,
                &session_id,
                agent_id,
                agent_name.as_deref(),
                &context_id,
                is_peer_message,
                message,
            )
            .await;
        } else {
            super::dm_lifecycle::notify_dm_peer_of_setup_cancellation(
                &state,
                &run_id,
                &session_id,
                agent_id,
                agent_name.as_deref(),
                &context_id,
                is_peer_message,
            )
            .await;
        }
        super::notifications::finish_episode_run(&state, episode_job_id, run_id, &[]).await;
        broadcast_queue_advance(&state, agent_id).await;
        return;
    }

    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::run_started_with_config(run_id, session_id, resolved_config),
        )
        .await;

    // Mirror onto the agent-scoped session-activity feed (#856) so the
    // web UI sidebar can surface activity on sessions other than the
    // currently-viewed one. Covers both regular runs and DM runs because
    // every run goes through this point.
    state
        .run_manager
        .send_agent_event(
            agent_id,
            run_id,
            session_id,
            SseEventData::session_activity_started(session_id, run_id, agent_id),
        )
        .await;

    // Create a runtime event channel so we can forward tool events to SSE.
    // A second sender (`invoke_agent_tx`) is created for the InvokeAgentTool
    // so subagent events are forwarded into the same SSE stream.  It is moved
    // directly into the tool (not cloned) so no orphaned sender lingers in
    // this scope -- when the runtime drops its sender and the tool drops its
    // sender, the channel closes and the forwarder task completes.
    let (runtime_tx, runtime_rx) = mpsc::unbounded_channel::<RuntimeEvent>();
    let invoke_agent_fwd: std::sync::Arc<dyn alms_tools::EventForwarder> =
        std::sync::Arc::new(RuntimeEventForwarder::new(runtime_tx.clone()));

    // Capture summary config before agent_config and llm are consumed.
    // C1 fix: resolve the summary model *from the per-agent LLM client* so
    // that when `summary_model` is None we fall back to the agent's configured
    // model, not the server default.  After this line `llm` is consumed by
    // `AgentRuntime::new` and no longer available.
    //
    // #871 leak guard (mirror of #860/#861 on the summary path): when
    // `summary_provider` is configured AND `summary_model` is None, the
    // agent's model name belongs to the AGENT's provider, not the summary
    // provider. Falling back to it would leak the agent's model slug onto
    // the summary provider's wire and produce a confusing 404 (the symptom
    // #861 was meant to prevent, on the summary task this time). Pass `None`
    // through to `generate_and_persist_summary` instead so it falls back to
    // `llm_for_summary.default_model()` — which the leak guard below clears
    // to "" so the wire fails fast with a clean missing-model error.
    let run_summary_mode = agent_config.context_config.run_summary_mode.clone();
    let summary_max_tokens = agent_config.context_config.summary_max_tokens;
    let summary_provider_cfg = agent_config.context_config.summary_provider.clone();
    let summary_model_cfg = agent_config.context_config.summary_model.clone();
    let summary_model_resolved = if summary_provider_cfg.is_some() {
        // Dedicated summary provider: only use an explicit summary_model.
        // Never inherit the agent's resolved model — it belongs to a
        // different provider's namespace. None falls through to the
        // summary client's default_model in `generate_session_summary`,
        // which the leak guard below has already cleared to "".
        summary_model_cfg.clone()
    } else {
        // No provider switch: agent and summary share a wire, so the
        // agent's resolved model is a safe fallback (pre-#871 behaviour).
        summary_model_cfg
            .clone()
            .or_else(|| Some(llm.default_model().to_string()))
    };

    // S6 fix: clone the per-agent resolved LLM client *before* AgentRuntime::new
    // consumes it.  The summary task needs the agent's provider/base_url/api_key,
    // not the server-default `state.llm`.
    //
    // #866: when `summary_provider` is configured, re-target the summary
    // client at that provider via `with_provider_and_secrets`. This lets the
    // summary task hit a different provider than the agent (e.g. agent on
    // Anthropic, summary on OpenRouter). When `summary_provider` is None the
    // client is byte-identical to the agent's `llm`, preserving pre-#866
    // behaviour.
    //
    // The construction logic (including the #871 leak guard) lives in
    // `build_summary_client` so it can be unit-tested without spinning up a
    // full AppState. Here we just borrow `state.secrets` and forward it.
    let llm_for_summary = build_summary_client(
        &llm,
        summary_provider_cfg.as_deref(),
        summary_model_cfg.as_deref(),
        &state.secrets.read(),
        agent_id,
    );

    let mut runtime = match alms_runtime::AgentRuntime::new(agent_id, agent_config, llm) {
        Ok(rt) => rt,
        Err(e) => {
            error!("Run {} failed to create runtime: {}", run_id.0, e);
            // #1046: flip state FIRST and gate the `run_error` SSE
            // broadcast on the transition bool. By this point the run
            // is already `Running` (mark_run_as_running_with_config
            // fired above), so the HTTP `cancel_run` handler may have
            // raced and flipped the state to `Cancelled` between then
            // and this point — in that case `mark_run_as_failed`
            // returns false and we skip the duplicate `run_error`
            // event, since the cancel handler's `run_cancelled`
            // already filled the terminal slot.
            //
            // The `session_activity_ended` agent-feed fan-out fires
            // unconditionally to pair the earlier
            // `session_activity_started` (#856), regardless of who
            // won the cancel-vs-init-fail race. The HTTP cancel handler
            // does NOT emit `session_activity_ended`, so suppressing
            // it here would leave the agent feed with an unpaired
            // `started` and a sidebar indicator stuck on until the
            // next unrelated event clears it.
            let mut failure_message = e.to_string();
            let (_failed_transitioned, emit_failure) = match state
                .run_manager
                .try_mark_run_as_failed(run_id, failure_message.clone())
            {
                Ok(transitioned) => (transitioned, transitioned),
                Err(error) => {
                    failure_message = format!("Run failure could not be persisted: {error}");
                    (false, true)
                }
            };

            if emit_failure {
                state
                    .run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::run_error(run_id, &failure_message),
                    )
                    .await;
            }
            state
                .run_manager
                .send_agent_event(
                    agent_id,
                    run_id,
                    session_id,
                    SseEventData::session_activity_ended(session_id, run_id, agent_id),
                )
                .await;

            // Persist the runtime-construction failure so a follow-up
            // turn ("why did that fail?") gives the agent the error in
            // context (#874). Skipped for internal context IDs to mirror
            // the run-boundary marker policy.
            //
            // The error text is run through `sanitize_error_for_session`
            // before persistence so raw provider response bodies — which
            // can contain API keys, internal hostnames, or request URLs —
            // never reach session history or the LLM context on follow-up
            // turns (#911). The same sanitiser already guards the
            // runtime-layer fallback at `alms-runtime::agent::mod`.
            if !is_internal_context_id(&context_id) {
                let safe_reason = sanitize_error_for_session(&e);
                super::markers::persist_error_marker(
                    &state.session_manager,
                    session_id,
                    "runtime_init_error",
                    format!("(runtime initialization failed) {safe_reason}"),
                    serde_json::json!({
                        "run_id": run_id.0.to_string(),
                        "status": "failed",
                        "error": safe_reason,
                        "error_kind": "runtime_init",
                    }),
                );
            }

            // B4 (#1154): notify the DM peer when a peer-triggered DM run
            // dies on runtime construction — same rationale as the
            // resolve-failure arm above.
            super::dm_lifecycle::notify_dm_peer_of_setup_failure(
                &state,
                &run_id,
                &session_id,
                agent_id,
                agent_name.as_deref(),
                &context_id,
                is_peer_message,
                truncate_error_for_peer(&alms_core::AlmsError::Runtime(failure_message)),
            )
            .await;

            state.run_manager.remove_senders(run_id);
            state.run_manager.remove_cancel_token(run_id);
            state.approval_store.clear_for_run(run_id);
            // #1198 exit 4/5: release the episode reservation.
            super::notifications::finish_episode_run(&state, episode_job_id, run_id, &[]).await;
            // The queue head is advancing — fan out updated positions to any
            // remaining queued runs on this agent (#831).
            broadcast_queue_advance(&state, agent_id).await;
            return;
        }
    }
    .with_event_sender(runtime_tx)
    .with_run_id(run_id)
    .with_cancel_token(cancel_token.clone());

    // Set agent name for perspective mapping in DM sessions.
    if let Some(ref name) = agent_name {
        runtime = runtime.with_agent_name(name.clone());
    }

    // Implicit DM reply mode (#1154): for peer-triggered DM runs the
    // final assistant text IS the reply, delivered by the DM completion
    // gate after the run completes. The flag arms the runtime's bounded
    // empty-reply nudge and tells `finish_run` to leave the final-text
    // persistence to `MessageBus::send` (avoiding a double-render in the
    // DM session).
    if is_peer_message && context_id.starts_with("dm:") {
        runtime = runtime.with_dm_implicit_reply();
    }

    // #866: when a separate summary provider is configured, wire the
    // re-targeted client into the runtime so the in-loop compact-strategy
    // path (`maybe_summarize`, formerly known as sliding-summary; renamed
    // in #869) hits the configured provider rather than the agent's. When
    // `summary_provider` is None we leave the runtime's `summary_llm` as
    // None — the summarizer transparently falls back to `self.llm`
    // (pre-#866 behaviour).
    if summary_provider_cfg.is_some() {
        runtime = runtime.with_summary_llm(llm_for_summary.clone());
    }

    // Inject ALMS_DATA_DIR and ALMS_WORKSPACE_DIR into shell_exec processes
    // so that CLI commands invoked by agents find the correct database and
    // workspace regardless of the sandboxed cwd.
    {
        let shell_env = alms_core::build_shell_default_env(
            Some(&state.data_dir),
            state.workspace_dir.as_deref(),
        );
        if !shell_env.is_empty() {
            runtime = runtime.with_shell_default_env(shell_env);
        }
    }

    // Wire the shell-output spill policy (issue #756) before
    // `with_project_root` so the per-run spill directory is included in
    // the agent's `fs_read`/`fs_list`/`fs_grep`/`fs_glob`
    // extra_read_roots at run start.
    //
    // This said "before `with_workspace`" until #1260. That is a strictly
    // weaker rule than the real one and obeying it is not sufficient:
    // `with_project_root` runs earlier, it is the registration that
    // composes the accumulated extras, and `with_workspace` has
    // registered no fs_* tool since #945 — so a reordering that satisfied
    // the old comment could drop every extra read root silently. The
    // `with_tool_output_truncate` block below always stated the
    // constraint correctly; the two are meant to be identical.
    // The directory itself is created lazily on first spill. Reads use the
    // live `tools_config.shell_spill` so operators can tweak the TOML and
    // restart, but values are NOT PATCH-mutable (consistent with
    // shell_permissions).
    {
        let spill_cfg = state.tools_config.read().shell_spill.clone();
        let run_dir = state
            .data_dir
            .join(alms_runtime::spill::SPILL_DIR_NAME)
            .join(run_id.0.to_string());
        runtime = runtime.with_shell_spill(run_dir, spill_cfg.enabled);
    }

    // Wire the shared in-loop tool-output truncation policy (issue #851).
    // Mirrors `with_shell_spill` above — same lifecycle, same retention
    // model, same fs_* read-root widening, but applied to *every* tool's
    // output (not just shell). Must come before `with_project_root` so
    // the per-run spill dir is included in the agent's extra_read_roots.
    {
        let trunc_cfg = state.tools_config.read().tool_output_truncate.clone();
        let run_dir = state
            .data_dir
            .join(alms_runtime::tool_output_truncate::TOOL_OUTPUT_DIR_NAME)
            .join(run_id.0.to_string());
        runtime = runtime.with_tool_output_truncate(
            run_dir,
            trunc_cfg.enabled,
            trunc_cfg.max_bytes,
            trunc_cfg.max_lines,
        );
    }

    // Pin the agent's filesystem-sandbox boundary at the project root
    // (#945) — or, when `worktree_mode = "git"` (#946), at the agent's
    // dedicated worktree under `<project>/.alms/worktrees/<name>/`. After
    // this call `fs_*` and `shell` enforce against the same root and the
    // shell's persistent cwd defaults to that root. Must come AFTER the
    // spill / tool-output-truncate builders so the accumulated
    // `extra_fs_read_roots` are reflected in the read-family fs_* tool
    // registrations.
    //
    // Precedence (highest wins):
    // 1. `[security].allow_full_os_access` (#947) — when the agent's
    //    name is on the list, skip the sandbox pin entirely and call
    //    `with_unrestricted_filesystem`. Worktree mode is silently
    //    ignored at runtime per the documented precedence; the
    //    worktree itself stays on disk because the operator may flip
    //    the security knob off later.
    // 2. `worktree_mode = "git"` (#946) — pin the sandbox at the
    //    worktree path. Push `<project>/.alms/agents/` onto the
    //    extra-read-roots list FIRST so the parent agent can still
    //    `fs_read('.alms/agents/<sibling>/personality.md')` from
    //    outside its worktree (matches the issue's sibling-read
    //    acceptance criterion).
    // 3. Default — `with_project_root(state.project_root)` as #945
    //    intended.
    //
    // `shell_permissions` (#717) and the destructive-command
    // classifier (#745) apply at every level — they are independent
    // operator policy, not part of the sandbox.
    let full_os_access = agent_name
        .as_deref()
        .map(|n| state.security_config.is_full_os_access_agent(n))
        .unwrap_or(false);
    let resolved_worktree_mode = resolved.worktree_mode;
    if full_os_access {
        // Defensive `unwrap` is fine — `full_os_access` is only true when
        // `agent_name` is `Some`.
        let name = agent_name.as_deref().unwrap_or("");
        warn!(
            target: "alms.security",
            agent_name = %name,
            run_id = %run_id.0,
            allow_full_os_access = true,
            worktree_mode = %resolved_worktree_mode.as_wire_str(),
            "Run starting for agent '{}' WITHOUT project-root filesystem sandbox \
             (allow_full_os_access). shell_permissions and the destructive-command \
             classifier still apply. Worktree-mode is silently ignored at runtime \
             when the agent is on the security list.",
            name,
        );
        runtime = runtime.with_unrestricted_filesystem();
    } else if resolved_worktree_mode == alms_core::WorktreeMode::Git
        && let Some(ref name) = agent_name
    {
        // Worktree-mode-git path. The worktree was provisioned at
        // agent-create time (or on a `mode: off → git` PATCH); we
        // just compute the path again here and pin the sandbox at
        // it. If the directory has been removed externally,
        // `with_project_root` will fall back to the as-is path
        // with a WARN — the run continues with whatever fs sandbox
        // semantics the missing directory produces.
        let worktree_dir = alms_core::worktree::worktree_path(&state.project_root, name);
        // Sibling-read root: the project's `.alms/agents/` tree,
        // read-only. This lets a parent agent in worktree mode still
        // read sibling personality metadata from outside its
        // worktree. Scoped tightly — NOT the entire project root,
        // which would defeat the worktree's filesystem isolation.
        let sibling_read_root = state.project_root.join(".alms").join("agents");
        runtime = runtime
            .with_extra_fs_read_root(sibling_read_root)
            .with_project_root(worktree_dir.clone());
        info!(
            target: "alms.worktree",
            agent_name = %name,
            run_id = %run_id.0,
            worktree_dir = %worktree_dir.display(),
            "Run starting under per-agent git worktree (#946)"
        );
    } else {
        runtime = runtime.with_project_root(state.project_root.clone());
    }

    // Attach workspace if configured — registers the workspace_write tool for this run.
    // After #945 this no longer changes the sandbox root or default cwd —
    // it only ensures the agent's metadata directory exists and binds the
    // `workspace_write` tool. The metadata lives at
    // `<project_root>/.alms/agents/<name>/`, naturally inside the
    // project-root sandbox.
    if let (Some(workspace_dir), Some(name)) = (&state.workspace_dir, &agent_name) {
        let workspace = alms_runtime::AgentWorkspace::new(workspace_dir, name);
        runtime = runtime.with_workspace(workspace);
    }

    // Register invoke_agent tool.
    // Subagent events are forwarded into this run's SSE stream.
    // The cancel_token is passed to InvokeAgentTool so that cancelling the
    // parent run propagates to all subagents spawned during this run.
    {
        let dispatcher: std::sync::Arc<dyn alms_tools::SubagentDispatcher> =
            state.coordinator.clone();
        // Separate channel for background subagent events -> session stream.
        // This is independent of the parent's runtime_tx, so it doesn't
        // block the parent run from finishing.
        // Note: bg_run_id uses the parent's run_id. These events may arrive
        // after the parent run has finished. The frontend uses source_agent
        // (not run_id) for SubagentBar routing, so this is acceptable.
        let (bg_event_tx, bg_event_rx) = mpsc::unbounded_channel::<alms_runtime::RuntimeEvent>();
        let bg_state = state.clone();
        let bg_session_id = session_id;
        let bg_run_id = run_id;
        // #1105 — preserve the documented ordering invariant for the
        // background path. The parent's `tool_start (invoke_agent)` is
        // queued on `runtime_tx` by the agent loop, but `SubagentStarted`
        // for a background subagent is enqueued on the SEPARATE
        // `bg_event_tx` by `spawn_subagent`. The two channels are drained
        // by independent tasks, so converting `SubagentStarted` to SSE
        // directly here can race the parent's `tool_start` and reach the
        // client first — which leaves the frontend resolver
        // (`setSubagentSessionId`) without an `activeSubagents` entry to
        // attach the session id to (the bg path also explicitly skips
        // `setSubagentSessionId` in the `tool_end` handler, so the only
        // fallback is `subagent_completed` at the end of the bg subagent's
        // lifetime). We forward `SubagentStarted` back onto the runtime
        // channel via a clone of `invoke_agent_fwd` so the existing
        // `forward_runtime_events` task picks it up in strict FIFO order
        // after the parent's `tool_start` (which the agent loop enqueues
        // BEFORE calling `tool.execute()`, i.e. before this bg task
        // observes the event at all).
        // #1115 deadlock fix: hold the parent-channel forwarder as a *Weak*
        // reference, NOT a strong `Arc` clone.
        //
        // `bg_runtime_fwd` exists solely to reroute the single
        // `SubagentStarted` event onto the parent's `runtime_tx` so it lands
        // in FIFO order behind the parent's `tool_start (invoke_agent)` (the
        // #1105 ordering invariant). That event is emitted by
        // `Coordinator::spawn_subagent` *synchronously, before
        // `dispatch_background` returns* — i.e. while the parent's
        // `runtime.run()` is still in flight and the strong `invoke_agent_fwd`
        // (held by `InvokeAgentTool` inside the runtime) is still alive.
        //
        // The `Weak::upgrade` for that event succeeds while the parent run is
        // in flight, but it is NOT strictly guaranteed — and crucially the
        // parent can ALSO finish entirely before a bg subagent that was spawned
        // near the end of the turn emits its `SubagentStarted` at all. The
        // event is *enqueued* onto `bg_event_tx` and *drained* by the separate
        // task spawned below; nothing synchronizes that task's `upgrade()` call
        // against the parent run completing and dropping `runtime` (and with it
        // `invoke_agent_fwd`). When the upgrade misses, `route_bg_event` now
        // FALLS BACK to delivering the `subagent_started` event on the parent's
        // *session* stream via `send_session_event` (#1125 A1-3) instead of
        // dropping it, so the #1105 "View session during the run" surface is
        // preserved even for a bg subagent that starts after the parent turn
        // ends. (The `send_session_event` fallback is keyed by session, not by
        // the parent's `runtime_tx` channel lifetime, so it is robust to the
        // parent finishing; the subagent `session_id` is also still in the
        // `invoke_agent` tool result as a backstop either way.)
        //
        // A *strong* clone here is a deadlock: the bg event forwarder task
        // lives as long as `bg_event_rx` has any sender, and `bg_event_tx`
        // (inside `bg_event_fwd`) is cloned into the coordinator's
        // subagent→parent relay task, which only drops it when the background
        // subagent's loop finishes. So a strong `bg_runtime_fwd` keeps a
        // `runtime_tx` sender alive for the entire background-subagent
        // lifetime, and `forward_runtime_events` (awaited at
        // `forwarder_handle.await` below) never observes all senders close.
        // The parent run then hangs in `Running` until the bg subagent
        // completes or its TTL expires — exactly the regression #1115
        // introduced (pre-#1115 the bg task held no `runtime_tx` sender at
        // all). A `Weak` does not contribute to the sender count, so
        // `drop(runtime)` closes `runtime_tx` promptly regardless of how long
        // the detached bg subagent keeps the bg channel open.
        let bg_runtime_fwd = std::sync::Arc::downgrade(&invoke_agent_fwd);
        tokio::spawn(async move {
            let mut rx = bg_event_rx;
            while let Some(event) = rx.recv().await {
                // Upgrade only for the duration of routing a single event.
                // While the parent run is alive the upgrade succeeds and
                // `route_bg_event` reroutes `SubagentStarted` back onto the
                // parent's `runtime_tx` (the #1105 FIFO-ordering invariant),
                // returning `None` so we do not also emit on the session
                // stream.
                //
                // After the parent run finishes and drops `runtime` (and with
                // it `invoke_agent_fwd`), the upgrade returns `None`. The bg
                // subagent's `subagent_activity` status signals still convert
                // to SSE directly via `route_bg_event` and reach the session
                // stream regardless of the parent (the Subagent status bar
                // must keep updating after the parent turn ends). For
                // `SubagentStarted`, `route_bg_event` FALLS BACK to returning
                // a `Persist(subagent_started)` (rather than dropping it),
                // which we deliver below via `send_session_event` — recovering
                // the #1105 surface for a bg subagent that started after the
                // parent turn ended (#1125 A1-3). Both delivery paths are
                // independent of `runtime_tx`, so this does not reintroduce
                // the #1124 deadlock.
                let upgraded = bg_runtime_fwd.upgrade();
                match route_bg_event(event, upgraded.as_deref(), bg_run_id, bg_session_id) {
                    // Durable events (dead-parent `subagent_started` fallback):
                    // persist to the session event log, then fan out.
                    Some(RoutedBgEvent::Persist(sse)) => {
                        bg_state
                            .run_manager
                            .send_session_event(bg_session_id, bg_run_id, sse)
                            .await;
                    }
                    // Ephemeral `subagent_activity` status signals: live
                    // fan-out only — never logged, so a subagent's activity
                    // cannot bloat the parent's persisted session log.
                    Some(RoutedBgEvent::Transient(sse)) => {
                        bg_state
                            .run_manager
                            .send_transient_session_event(bg_session_id, sse);
                    }
                    None => {}
                }
            }
        });

        let bg_event_fwd: std::sync::Arc<dyn alms_tools::EventForwarder> =
            std::sync::Arc::new(RuntimeEventForwarder::new(bg_event_tx));
        let invoke_tool = alms_tools::InvokeAgentTool::new(
            dispatcher,
            session_id,
            agent_id,
            Some(run_id),
            Some(invoke_agent_fwd),
        )
        .with_cancel_token(cancel_token)
        .with_background_event_fwd(bg_event_fwd);
        let read_session_tool =
            alms_tools::ReadSubagentSessionTool::new(state.session_manager.clone(), agent_id);
        runtime.register_tool(std::sync::Arc::new(invoke_tool));
        runtime.register_tool(std::sync::Arc::new(read_session_tool));
    }

    // Register read_session tool (on-demand session recall for the agent's own sessions).
    {
        let read_own_session_tool = alms_tools::ReadSessionTool::new(
            state.session_manager.clone(),
            agent_id,
            agent_name.clone(),
        );
        runtime.register_tool(std::sync::Arc::new(read_own_session_tool));
    }

    // Register peer messaging tools (Layer 2) when agent name is known.
    if let Some(ref name) = agent_name {
        let sender: std::sync::Arc<dyn alms_tools::MessageSender> = state.message_bus.clone();
        let send_tool = apply_send_message_fold(
            alms_tools::SendMessageTool::new(
                sender,
                agent_id,
                name.clone(),
                state.session_manager.clone(),
                session_id,
            ),
            is_peer_message,
            &context_id,
            name,
            dm_ended_peer.as_deref(),
        );
        let list_tool =
            alms_tools::ListAgentsTool::new(state.session_manager.clone(), name.clone());
        let read_tool =
            alms_tools::ReadMessagesTool::new(state.session_manager.clone(), name.clone());
        let list_sessions_tool = alms_tools::ListMySessionsTool::new(
            state.session_manager.clone(),
            agent_id,
            session_id,
            name.clone(),
        );
        runtime.register_tool(std::sync::Arc::new(send_tool));
        runtime.register_tool(std::sync::Arc::new(list_tool));
        runtime.register_tool(std::sync::Arc::new(read_tool));
        runtime.register_tool(std::sync::Arc::new(list_sessions_tool));

        // Only register `ignore_message` in DM sessions -- the tool is
        // meaningless outside DM context and would confuse the LLM into
        // calling it in web-chat or job runs.  The runtime guard in
        // IgnoreMessageTool::execute() remains as defense-in-depth.
        if context_id.starts_with("dm:") {
            let ignore_tool = alms_tools::IgnoreMessageTool::new(context_id.clone());
            runtime.register_tool(std::sync::Arc::new(ignore_tool));
        }
    }

    // Spawn forwarder: converts RuntimeEvents -> SseEventData (and stores approvals).
    // We keep the handle so we can await it after the runtime finishes, ensuring
    // all tool events are flushed before we send run_finished.
    let forwarder_state = state.clone();

    // Build cross-session DM info when the run is on a DM session so that
    // status phases are echoed to the agent's webchat stream (#651, #688).
    let dm_peer_for_webchat = agent_name
        .as_deref()
        .and_then(|name| extract_peer_from_dm_context(&context_id, name));

    let dm_cross_session =
        dm_peer_for_webchat
            .as_deref()
            .map(|peer_name| super::tools::DmCrossSessionInfo {
                agent_id,
                peer_name: peer_name.to_string(),
            });

    let forwarder_handle = tokio::spawn(forward_runtime_events(
        runtime_rx,
        run_id,
        session_id,
        forwarder_state.run_manager.clone(),
        forwarder_state.approval_store.clone(),
        forwarder_state.session_manager.clone(),
        context_id.clone(),
        dm_cross_session,
    ));

    // Save input for episodic summary generation (input is consumed by run()).
    // S3 optimisation: only clone when summary mode is enabled AND the session
    // type is eligible (not a subagent or episodic session).
    // DM sessions are included — the agent_name is needed to derive the peer.
    let agent_name_for_summary = agent_name.clone().unwrap_or_default();
    let should_summarize = run_summary_mode != alms_core::config::RunSummaryMode::Off
        && alms_core::derive_source_label(&context_id, &agent_name_for_summary).is_some();

    let run_input_for_summary = if should_summarize {
        Some(input.clone())
    } else {
        None
    };

    // System-triggered notification runs (subagent completions, DM-ended)
    // that land on a user-facing session pre-persist the notification as
    // a `Role::User` message with `notification_input: true` metadata.
    // The `notification_input` flag drives `get_session_messages` filtering
    // so the internal prompt never shows as a "user" bubble on reload.
    //
    // The `Role::User` choice is now a consequence of the canonical
    // message-shape invariant enforced in `ContextBuilder::normalize_for_llm`
    // (see #586): the invariant guarantees a trailing user turn regardless
    // of this message's presence, so synthesising a placeholder would be
    // redundant. Persisting as user keeps the real payload in history.
    // Applies to Anthropic direct and OpenRouter-to-Claude proxying, which
    // performs the same system-message extraction as the Anthropic API.
    let is_notification_on_user_session =
        is_system_triggered && !is_peer_message && !is_internal_context_id(&context_id);

    let result = if is_peer_message {
        // Peer-triggered run: the input message is already in the shared
        // session (written by MessageBus with from_agent metadata).
        // Use run_on_session to look up the session by ID directly and
        // skip re-persisting the input (fixes C1 session split + C2 double-write).
        runtime
            .run_on_session(&state.session_manager, session_id, &context_id, &input)
            .await
    } else if input_pre_persisted {
        // The prompt was durably claimed when this queue slot began. Reuse
        // that session history without persisting the user message twice.
        runtime
            .run_on_session(&state.session_manager, session_id, &context_id, &input)
            .await
    } else if is_notification_on_user_session {
        // Notification run on a user-facing session: pre-persist the
        // input as Role::User + `notification_input` metadata (see the
        // comment above `is_notification_on_user_session` for why), then
        // use `run_on_session` to skip `runtime.run()`'s default
        // Role::User persistence and avoid a double-write.
        let notif_msg = alms_session::Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: alms_session::Role::User,
            content: alms_session::Content::Text(input.clone()),
            timestamp: alms_core::Timestamp::now(),
            metadata: Some(serde_json::json!({
                "notification_input": true,
            })),
        };
        match persist_notification_input(&state.session_manager, session_id, notif_msg) {
            Ok(()) => {
                runtime
                    .run_on_session(&state.session_manager, session_id, &context_id, &input)
                    .await
            }
            Err(error) => Err(error),
        }
    } else {
        // Pre-create the session with the trigger's session_id so that
        // `runtime.run()` -> `get_or_create(agent_id, context_id)` finds
        // the existing session instead of generating a new random UUID.
        //
        // Without this, system-triggered runs on internal sessions (e.g.
        // `notifications:{agent}`) hit a session ID mismatch: the Run
        // record carries the deterministic SessionId from the trigger, but
        // `get_or_create` inside `runtime.run()` creates a session with a
        // random UUID because no session with that `(agent_id, context_id)`
        // key exists yet.  Fixes #585.
        match if is_system_triggered {
            state
                .session_manager
                .get_or_create_with_id(session_id, agent_id, &context_id)
                .map(|_| ())
        } else {
            Ok(())
        } {
            Ok(()) => {
                runtime
                    .run(&state.session_manager, &context_id, input)
                    .await
            }
            Err(error) => Err(error),
        }
    };

    // Drop `runtime` to close the last STRONG sender on `runtime_tx`.
    // The `invoke_agent_fwd` Arc was moved into `InvokeAgentTool`, which
    // lives inside the runtime's tool registry, so dropping the runtime
    // also drops that strong copy of the forwarder — and the runtime's own
    // `RuntimeEventSender`. Once those close, `forward_runtime_events`
    // observes `rx.recv()` return `None` and `forwarder_handle` completes.
    //
    // The bg event forwarder task holds only a *Weak* reference to
    // `invoke_agent_fwd` (the #1115 deadlock fix), so it does NOT keep a
    // `runtime_tx` sender alive. This matters because that bg task — and the
    // `bg_event_tx` feeding it — can outlive this run by the full lifetime of
    // a detached background subagent: the coordinator's subagent→parent relay
    // task clones `bg_event_tx` and only drops it when the bg subagent's loop
    // finishes. Were `bg_runtime_fwd` a strong `Arc`, `forwarder_handle.await`
    // here would block until the bg subagent completed (or its TTL expired),
    // hanging the parent run in `Running` — the regression #1115 introduced.
    drop(runtime);
    forwarder_handle.await.ok();

    // Helper: persist tool call records (used by all outcome branches).
    // `session_id` is stored on each row (B9(b), #1154) so the records stay
    // attributable to this session even if the `runs` row is later removed.
    let persist_tool_calls = |records: &[alms_core::ToolCallRecord]| {
        if !records.is_empty()
            && let Some(store) = state.session_manager.store()
            && let Err(e) = store.save_tool_calls(run_id, session_id, records)
        {
            warn!(
                "Failed to persist {} tool call records for run {}: {}",
                records.len(),
                run_id.0,
                e
            );
        }
    };

    // #1198: tool-call records captured per-arm for the episode tail hook
    // (exit 5/5 below) — the pending-work scan needs the records, and each
    // arm owns a different vec. Cloned only for job-stamped runs.
    let mut episode_tool_calls: Vec<alms_core::ToolCallRecord> = Vec::new();

    #[cfg(test)]
    pause_before_terminal_transition(run_id).await;

    match result {
        Ok(output) => {
            persist_tool_calls(&output.tool_calls);
            if episode_job_id.is_some() {
                episode_tool_calls = output.tool_calls.clone();
            }

            // Persist a run-boundary marker so page reloads show "(run
            // completed)" separators. Only for user-facing sessions to
            // avoid cluttering internal sessions (jobs, subagents, DMs,
            // notifications).
            //
            // Note: this writes to the session store, not to the SSE feed,
            // so its position relative to the state-flip / broadcast
            // ordering below is not load-bearing. Kept above
            // `mark_run_as_completed` for the same reason as before — it's
            // a synchronous SQLite write that sequences naturally before
            // both branches.
            if !is_internal_context_id(&context_id) {
                super::markers::persist_lifecycle_marker(
                    &state.session_manager,
                    session_id,
                    "run_boundary",
                    "(run completed)".to_string(),
                    serde_json::json!({
                        "run_id": run_id.0.to_string(),
                        "status": "completed",
                    }),
                );
            }

            // Compute the summary text BEFORE `mark_run_as_completed`
            // consumes `output.response` (#927 reorder preserves the
            // original consume-then-clone shape, just rolls it earlier).
            //
            // Under implicit DM replies (#1154), `output.response` IS the
            // outbound DM reply — the pre-#1154 lookup that read the last
            // `send_message`-written outbound message from the DM session
            // (#421 / #434 Bug 1) is obsolete: the reply is only persisted
            // (via `MessageBus::send`) AFTER the completion gate below
            // delivers it, so the run's own response field is the
            // authoritative content for episodic summaries. `ignore_message`
            // runs keep the pre-#1154 behaviour (empty response → empty
            // summary output).
            let run_output_for_summary = run_input_for_summary
                .as_ref()
                .map(|_| output.response.clone());

            // #1098: capture the extended-thinking trace from the final
            // LLM turn so the summarizer path can strip it from the
            // assistant output before feeding it to the heuristic /
            // summarizer LLM. In the `[Thinking]`-only reasoning-promotion
            // fallback (`finalize_content_and_reasoning`), `output.response`
            // IS the reasoning trace; we need the sideband copy so the
            // summarizer can identify and drop it. In normal `[Thinking,
            // Text]` turns the visible response and the reasoning are
            // distinct strings and the summarizer's prefix-strip is a
            // no-op — so threading reasoning through is safe in both
            // cases.
            let run_reasoning_for_summary = output.reasoning.clone();

            // Capture the final text + reasoning for the DM completion gate
            // (#1154) before `mark_run_as_completed` consumes
            // `output.response`. Under implicit replies the response is the
            // candidate message for the DM peer; the reasoning copy lets
            // the gate detect the `[Thinking]`-only promotion fallback
            // (`response == reasoning`) so a reasoning trace is never
            // delivered as a reply.
            let dm_response = output.response.clone();
            let dm_reasoning = output.reasoning.clone();

            // #927 (extends #895): flip the run state to `Completed`
            // BEFORE broadcasting `run_finished`. Pre-fix, a concurrent
            // `GET /sessions` snapshot taken between the broadcast and
            // the flip could observe `has_active_run: true` while the SSE
            // feed had already moved past the terminal event —
            // `last_event_id`-based reconnect would never replay it,
            // leaving the sidebar's activity indicator stuck.
            //
            // The usage payload is `Copy` (TokenUsage is a small struct
            // of u32/u64), so the broadcast can read it after the flip
            // consumes the original via `mark_run_as_completed`. We
            // capture it here for clarity and to avoid relying on a
            // post-move value.
            //
            // #1046 (symmetric to the cancel-arm gate): gate the
            // broadcast on the transition bool. The HTTP `cancel_run`
            // handler now flips the run state to `Cancelled` and
            // broadcasts `run_cancelled` synchronously, which races
            // against natural completion in a narrow window: if the
            // agent loop returned `Ok(_)` just before the cancel arrived,
            // `mark_run_as_completed` would silently regress the state
            // from `Cancelled` to `Completed` and emit `run_finished`
            // on top of the already-delivered `run_cancelled`. The
            // first-writer-wins contract on `Run::mark_completed`
            // (Running-only, idempotent) makes the second flip a no-op
            // and the bool gate suppresses the duplicate terminal SSE
            // event. The post-completion DM lifecycle / episodic
            // summary fan-out below still fires — they are independent
            // side effects, and the run's stored output / usage was
            // never persisted (the cancel won the race), so those
            // helpers operate on the in-flight result they were handed
            // by the runtime regardless of the final stored state.
            let usage_for_broadcast = output.usage;

            // #1052: `mark_run_as_completed` returns `bool` indicating
            // whether the transition actually happened. The previous
            // contract unconditionally overwrote the `status` field, so a
            // run that had already been driven terminal by a racing path
            // (cancel token, shutdown drain, cancel_runs_for_session,
            // synchronous HTTP cancel handler from #1050) would silently
            // flip back to `Completed` here — and the `run_finished`
            // broadcast / episodic summary spawn / DM lifecycle handler
            // below would all fire as if the run had completed naturally.
            // With the bool gate in place, every post-flip side effect in
            // this Ok arm is conditional on `completed_transitioned`: if a
            // cancel won the race, the canonical `run_cancelled` SSE was
            // already emitted (by the `Cancelled` arm, the synchronous HTTP
            // `cancel_run` handler, or the pre-cancel early-exit above) and
            // the DM peer was already signalled — by the `Cancelled` arm's
            // `handle_dm_run_failure(UserCancelled)` when the loop ran, or by
            // the pre-cancel early-exit's
            // `notify_dm_peer_of_setup_cancellation` (S1, #1154) when the run
            // was cancelled before the loop started. Either way we stay
            // silent here. Note this differs from the `Cancelled` / `Failed`
            // Err arms below where `handle_dm_run_failure` is intentionally
            // unconditional — see those arms for the rationale.
            let (completed_transitioned, completion_persistence_error) = match state
                .run_manager
                .try_mark_run_as_completed(run_id, output.response, output.usage)
            {
                Ok(transitioned) => (transitioned, None),
                Err(error) => {
                    let message = format!("Run completion could not be persisted: {error}");
                    state
                        .run_manager
                        .send_event(
                            run_id,
                            session_id,
                            SseEventData::run_error(run_id, &message),
                        )
                        .await;
                    (false, Some(message))
                }
            };

            if completed_transitioned {
                // token_delta events already emitted during streaming in the agent loop
                state
                    .run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::run_finished(run_id, true, usage_for_broadcast),
                    )
                    .await;

                // Fire-and-forget episodic summary generation.
                // Runs in a separate task so it never blocks the SSE cleanup path.
                // S4: tracked by `in_flight` so graceful shutdown waits for it.
                if let (Some(run_input), Some(run_output)) =
                    (run_input_for_summary, run_output_for_summary)
                {
                    let sm = state.session_manager.clone();
                    let llm_clone = llm_for_summary.clone();
                    let ctx_id = context_id.clone();
                    let run_mgr = state.run_manager.clone();
                    let req = alms_runtime::episodic::PersistSummaryRequest {
                        mode: run_summary_mode.clone(),
                        agent_id,
                        session_id,
                        run_id,
                        run_input,
                        run_output,
                        run_reasoning: run_reasoning_for_summary,
                        context_id: ctx_id,
                        summary_model: summary_model_resolved.clone(),
                        agent_name: agent_name_for_summary.clone(),
                        summary_max_tokens,
                    };
                    run_mgr.track_in_flight();
                    tokio::spawn(async move {
                        let _guard = InFlightGuard {
                            run_manager: run_mgr,
                        };
                        alms_runtime::episodic::generate_and_persist_summary(&sm, &llm_clone, req)
                            .await;
                    });
                }

                // -- DM completion gate (consolidated in #628, redesigned
                //    for implicit replies in #1154) --
                //
                // Every peer-triggered DM run exits as exactly one of
                // delivered | ended | errored — never silence:
                //
                // - `ignore_message` succeeded → ended (marker + peer
                //   notification + `dm_conversation_ended` SSE).
                // - Deliverable final text → delivered to the peer via
                //   `MessageBus::send` (persisted to the shared DM session
                //   + peer run triggered). The final text IS the reply; no
                //   `send_message` call is involved.
                // - Neither → errored: the conversation is ended with an
                //   `Errored` reason so the peer is notified.
                //
                // All logic lives in `dm_lifecycle::handle_dm_run_completion()`.
                //
                // #1052: gated on `completed_transitioned` together with
                // the `run_finished` broadcast and episodic-summary spawn.
                // Pre-fix, this fired unconditionally — when a cancel
                // raced an `ignore_message`-emitting run, the peer
                // received both `run_cancelled` (from the cancel path)
                // AND `dm_conversation_ended` with reason
                // `"ignore_message"` (from this call), conflating an
                // operator-driven cancel with an agent-driven ignore.
                //
                // Cancel-side conversation bookkeeping is signalled by
                // whichever path actually drove the run terminal — NOT
                // necessarily this Ok arm's sibling `Cancelled` arm. There
                // are two cancel paths and each owns the peer signal:
                //   1. Cancelled *during* the loop → the `Cancelled` /
                //      `CancelledWithToolCalls` arms call
                //      `handle_dm_run_failure(UserCancelled)` (unconditional
                //      per #1050, fires regardless of who wins the
                //      state-flip race).
                //   2. Cancelled *before* the loop started (queued-then-
                //      cancelled) → the pre-cancel early-exit at the top of
                //      `execute_run` calls
                //      `notify_dm_peer_of_setup_cancellation` (S1, #1154);
                //      this Ok arm is never reached on that path.
                // Either way the peer is signalled exactly once, so skipping
                // `handle_dm_run_completion` here when the cancel won is
                // correct — and the `MessageBus::end_conversation`
                // depth-remove in both paths is the atomicity guard, so it
                // cannot strand depth-counter state.
                let dm_exit = super::dm_lifecycle::handle_dm_run_completion(
                    super::dm_lifecycle::DmRunCompletionContext {
                        state: &state,
                        run_id,
                        session_id,
                        agent_id,
                        agent_name: agent_name.as_deref(),
                        context_id: &context_id,
                        is_peer_message,
                        tool_calls: &output.tool_calls,
                        response: &dm_response,
                        reasoning: dm_reasoning.as_deref(),
                    },
                )
                .await;
                if dm_exit != super::dm_lifecycle::DmRunExit::NotPeerDm {
                    info!(
                        run_id = %run_id.0,
                        dm_exit = ?dm_exit,
                        "DM completion gate exit"
                    );
                }

                info!("Run {} completed successfully", run_id.0);
            } else {
                // A cancel / shutdown path won the state-flip race while the
                // loop was producing this `Ok`. The `run_cancelled` SSE was
                // already emitted by whichever path flipped the state.
                //
                // S1 (#1154): but NOT every winning path signals the DM peer.
                // The synchronous HTTP `cancel_run` handler and
                // `cancel_runs_for_session` only flip state + emit
                // `run_cancelled` — they do not call `handle_dm_run_failure`.
                // And because the loop returned `Ok` (not
                // `Err(Cancelled)`), the `Cancelled` arm below — which would
                // have signalled the peer — never runs. So in the
                // Ok-arm-vs-HTTP-cancel interleaving the peer would be left
                // stranded. Signal it here with `UserCancelled`. This is
                // safe to call even if the peer was already signalled by a
                // racing path: `MessageBus::end_conversation`'s
                // `depths.remove()` / tombstone guard makes a second call a
                // no-op (no duplicate marker or trigger). Best-effort — a
                // returned error is logged, not propagated.
                let completion_peer_error = lifecycle_persistence_error_for_peer(
                    &state,
                    run_id,
                    completion_persistence_error,
                );
                // #1258: `interrupted: true` either way — see the `Cancelled`
                // arm below.
                let conversation_reason =
                    completion_peer_error.map_or(ConversationEndReason::UserCancelled, |message| {
                        ConversationEndReason::Errored {
                            message,
                            interrupted: true,
                        }
                    });
                if let Err(e) = super::dm_lifecycle::handle_dm_run_failure(
                    &state,
                    &run_id,
                    &session_id,
                    agent_id,
                    agent_name.as_deref(),
                    &context_id,
                    is_peer_message,
                    conversation_reason,
                )
                .await
                {
                    warn!(
                        error = %e,
                        "handle_dm_run_failure emitted error on Ok-but-cancelled \
                         path — DM peer state may be stale"
                    );
                }

                // The partial tool-call records and the run-boundary marker
                // were already persisted above (diagnostic and idempotent).
                info!(
                    "Run {} was already terminal when its loop returned Ok — the path \
                     that won the race (cancel / shutdown) already emitted the terminal \
                     SSE, so this arm stays silent to keep the event exactly-once \
                     (#1046). Not a dropped event. Terminal-arm bookkeeping skipped",
                    run_id.0
                );
            }
        }
        Err(alms_core::AlmsError::Cancelled) => {
            // #895: flip the run state BEFORE broadcasting `run_cancelled`.
            // See the pre-cancel branch at the top of this function for the
            // full rationale; the same race applies to every started/ended
            // boundary that uses `mark_run_as_*` to drive `has_active_run`.
            //
            // #1046 / #1052: gate the broadcast on the
            // `mark_run_as_cancelled` return value. When the HTTP
            // `cancel_run` handler (now synchronous post-#1050) has
            // already flipped the state and fired the SSE event (the
            // common case for user-initiated cancels — see the rationale
            // on the handler), this match arm sees `false` and skips the
            // duplicate broadcast. The same gate also closes the race on
            // shutdown-drain / `cancel_runs_for_session` cleanup paths.
            // The downstream `handle_dm_run_failure` and
            // `session_activity_ended` emissions still fire — they are
            // NOT duplicated by the HTTP handler (#1052 Tim review).
            let (cancelled_transitioned, cancellation_persistence_error) =
                match state.run_manager.try_mark_run_as_cancelled(run_id) {
                    Ok(transitioned) => (transitioned, None),
                    Err(error) => {
                        let message = format!("Run cancellation could not be persisted: {error}");
                        state
                            .run_manager
                            .send_event(
                                run_id,
                                session_id,
                                SseEventData::run_error(run_id, &message),
                            )
                            .await;
                        (false, Some(message))
                    }
                };

            if cancelled_transitioned {
                state
                    .run_manager
                    .send_event(run_id, session_id, SseEventData::run_cancelled(run_id))
                    .await;
            }

            // Issue #912 — DO NOT persist a lifecycle-layer
            // `(run cancelled)` error marker here.  The runtime layer
            // (`alms_runtime::AgentRuntime::finish_run`) already wrote a
            // `[Run cancelled by user]` text message at
            // `Role::Assistant` (or `Role::User` with `from_agent`
            // metadata in DM sessions).  That runtime-layer record is
            // the canonical source of truth: it survives
            // `strip_mid_history_system_markers` natively and reaches
            // the next-turn LLM context as a regular conversation turn,
            // satisfying the #874 follow-up-visibility requirement.
            // Persisting a second `kind: "error"` marker here would
            // duplicate the same conceptual event in both chat history
            // and LLM context — see Atlas + Alper's decision recorded
            // on issue #912.

            // Best-effort: notify the DM peer that the conversation ended
            // because this run was cancelled mid-flight. Without this, the
            // peer thinks the DM is still open until the 1800s
            // `DEPTH_EXPIRY_SECS` sweep clears the depth counter.
            //
            // #1052 review (Tim): NOT gated on `cancelled_transitioned`.
            // Per #1050's explicit design, `handle_dm_run_failure` fires
            // from the terminal arm as an independent side effect — the
            // synchronous HTTP cancel handler in #1050 does NOT call it
            // itself, and gating here would strand the DM peer whenever
            // the HTTP cancel wins the race (Alice never receives the
            // `ConversationEnded` peer notification, the depth counter
            // for `dm:alice:bob` is never reset, no `dm_ended` marker is
            // written). The depth-remove inside
            // `MessageBus::end_conversation` is idempotent (see
            // `handle_dm_run_failure_double_end_is_idempotent`), so a
            // duplicate call after some other path already ended the
            // conversation is safe.
            let cancellation_peer_error = lifecycle_persistence_error_for_peer(
                &state,
                run_id,
                cancellation_persistence_error,
            );
            // #1258: `interrupted: true` either way — the run was cancelled,
            // so no turn of this DM completed; the `Errored` upgrade only
            // swaps in the teardown-persistence failure as the reason text.
            let conversation_reason =
                cancellation_peer_error.map_or(ConversationEndReason::UserCancelled, |message| {
                    ConversationEndReason::Errored {
                        message,
                        interrupted: true,
                    }
                });
            if let Err(e) = super::dm_lifecycle::handle_dm_run_failure(
                &state,
                &run_id,
                &session_id,
                agent_id,
                agent_name.as_deref(),
                &context_id,
                is_peer_message,
                conversation_reason,
            )
            .await
            {
                warn!(
                    error = %e,
                    "handle_dm_run_failure emitted error — DM peer state may be stale"
                );
            }

            if cancelled_transitioned {
                info!("Run {} cancelled", run_id.0);
            } else {
                info!(
                    "Run {} was already terminal when its loop returned Cancelled — \
                     the path that won the race already emitted the terminal SSE, so \
                     this arm stays silent to keep the event exactly-once (#1046). \
                     Not a dropped event. DM peer notification fired unconditionally",
                    run_id.0
                );
            }
        }
        Err(alms_core::AlmsError::CancelledWithToolCalls { tool_calls }) => {
            // Persist partial tool call records even though the run was cancelled.
            // Persisted regardless of who wins the race — these are diagnostic
            // and the per-run `run_tool_calls` table is keyed on `run_id`, so a
            // double-write is a no-op overwrite of the same rows.
            persist_tool_calls(&tool_calls);
            if episode_job_id.is_some() {
                episode_tool_calls = tool_calls.clone();
            }

            // #895 / #1046 / #1052: see the `Cancelled` arm above for
            // the rationale on gating the SSE broadcast on the transition
            // bool (and on `handle_dm_run_failure` being intentionally
            // unconditional).
            let (cancelled_transitioned, cancellation_persistence_error) =
                match state.run_manager.try_mark_run_as_cancelled(run_id) {
                    Ok(transitioned) => (transitioned, None),
                    Err(error) => {
                        let message = format!("Run cancellation could not be persisted: {error}");
                        state
                            .run_manager
                            .send_event(
                                run_id,
                                session_id,
                                SseEventData::run_error(run_id, &message),
                            )
                            .await;
                        (false, Some(message))
                    }
                };

            if cancelled_transitioned {
                state
                    .run_manager
                    .send_event(run_id, session_id, SseEventData::run_cancelled(run_id))
                    .await;
            }

            // Issue #912 — see the `Cancelled` arm above.  The
            // runtime-layer `[Run cancelled by user]` write is the
            // canonical record; no lifecycle-layer marker is persisted.

            // Best-effort: see comment in the `Cancelled` arm. NOT gated
            // on `cancelled_transitioned` — `handle_dm_run_failure` is an
            // independent side effect per #1050's design, and gating it
            // would strand the DM peer when an external path (e.g. the
            // synchronous HTTP cancel handler from #1050) won the
            // state-flip race.
            let cancellation_peer_error = lifecycle_persistence_error_for_peer(
                &state,
                run_id,
                cancellation_persistence_error,
            );
            // #1258: `interrupted: true` either way — the run was cancelled,
            // so no turn of this DM completed; the `Errored` upgrade only
            // swaps in the teardown-persistence failure as the reason text.
            let conversation_reason =
                cancellation_peer_error.map_or(ConversationEndReason::UserCancelled, |message| {
                    ConversationEndReason::Errored {
                        message,
                        interrupted: true,
                    }
                });
            if let Err(e) = super::dm_lifecycle::handle_dm_run_failure(
                &state,
                &run_id,
                &session_id,
                agent_id,
                agent_name.as_deref(),
                &context_id,
                is_peer_message,
                conversation_reason,
            )
            .await
            {
                warn!(
                    error = %e,
                    "handle_dm_run_failure emitted error — DM peer state may be stale"
                );
            }

            if cancelled_transitioned {
                info!(
                    "Run {} cancelled ({} tool calls persisted)",
                    run_id.0,
                    tool_calls.len()
                );
            } else {
                info!(
                    "Run {} was already terminal when its loop returned \
                     CancelledWithToolCalls — the path that won the race already \
                     emitted the terminal SSE, so this arm stays silent to keep the \
                     event exactly-once (#1046). Not a dropped event. \
                     {} tool calls persisted; DM peer notification fired unconditionally",
                    run_id.0,
                    tool_calls.len()
                );
            }
        }
        Err(alms_core::AlmsError::FailedWithToolCalls { source, tool_calls }) => {
            // Persist partial tool call records even though the run failed.
            persist_tool_calls(&tool_calls);
            if episode_job_id.is_some() {
                episode_tool_calls = tool_calls.clone();
            }

            // #927 (extends #895): flip the run state to `Failed` BEFORE
            // broadcasting `run_error`. See the `Ok` arm above and the
            // pre-cancel branch at the top of this function for the full
            // rationale; the same race applies to every started/ended
            // boundary that uses `mark_run_as_*` to drive `has_active_run`.
            //
            // #1046 / #1052: gate the SSE broadcast on the transition
            // bool. The synchronous HTTP `cancel_run` handler (#1050) may
            // have already flipped the state to `Cancelled` and emitted
            // `run_cancelled` in the window between `agent_loop` returning
            // `Err(FailedWithToolCalls { ... })` (which can happen for
            // non-cancel reasons — `AgentRuntime::finish_run` wraps any
            // non-Cancelled error in this variant) and this terminal arm
            // running. The first-writer-wins contract on
            // `Run::mark_failed` (Queued/Running only, idempotent) makes
            // the second flip a no-op and the bool gate suppresses the
            // duplicate terminal SSE event. `handle_dm_run_failure` below
            // remains UNCONDITIONAL per #1050 design (see the `Cancelled`
            // arm above for the rationale).
            let mut failure_message = source.to_string();
            let (failed_transitioned, emit_failure, persistence_failed) = match state
                .run_manager
                .try_mark_run_as_failed(run_id, failure_message.clone())
            {
                Ok(transitioned) => (transitioned, transitioned, false),
                Err(error) => {
                    failure_message = format!("Run failure could not be persisted: {error}");
                    (false, true, true)
                }
            };

            if emit_failure {
                state
                    .run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::run_error(run_id, &failure_message),
                    )
                    .await;
            }

            // Issue #912 — DO NOT persist a lifecycle-layer
            // `(run failed) ...` error marker here.  The runtime layer
            // (`alms_runtime::AgentRuntime::finish_run`) already wrote a
            // `[Run failed: <safe_reason>]` text message at
            // `Role::Assistant` (or `Role::User` with `from_agent`
            // metadata in DM sessions), already passed through
            // `sanitize_error_for_session` (#911).  That runtime-layer
            // record is the canonical source of truth: it survives
            // `strip_mid_history_system_markers` natively and reaches
            // the next-turn LLM context as a regular conversation turn,
            // satisfying the #874 follow-up-visibility requirement.
            // Persisting a second `kind: "error"` marker here would
            // duplicate the same conceptual event in both chat history
            // and LLM context — see Atlas + Alper's decision recorded
            // on issue #912.

            // Best-effort: notify the DM peer that the conversation ended
            // due to a runtime error. The truncated `source` string is
            // surfaced in the DM-ended notification so the human user sees a
            // useful reason instead of a stale "in-flight" indicator until
            // the 1800s sweep.
            //
            // #1258: for this arm the peer AGENT no longer sees it — a died
            // run is an interrupted end, which starts no notification run,
            // and the `dm_ended_notification` marker is synthetic and
            // stripped from LLM context. The human still gets it, on the
            // DM-ended banner's `detail` line and in the persisted marker.
            //
            // NOT gated on `failed_transitioned` — `handle_dm_run_failure`
            // is an independent side effect per #1050's design (see the
            // `Cancelled` arm above for the full rationale).
            if let Err(e) = super::dm_lifecycle::handle_dm_run_failure(
                &state,
                &run_id,
                &session_id,
                agent_id,
                agent_name.as_deref(),
                &context_id,
                is_peer_message,
                // #1258: `interrupted: true` — the run died mid-turn, so
                // whatever this DM turn was going to produce does not exist.
                ConversationEndReason::Errored {
                    message: truncate_error_for_peer(&alms_core::AlmsError::Runtime(
                        failure_message.clone(),
                    )),
                    interrupted: true,
                },
            )
            .await
            {
                warn!(
                    error = %e,
                    "handle_dm_run_failure emitted error — DM peer state may be stale"
                );
            }

            if failed_transitioned {
                error!(
                    "Run {} failed ({} tool calls persisted): {}",
                    run_id.0,
                    tool_calls.len(),
                    source
                );
            } else if persistence_failed {
                error!(
                    "Run {} failure was quarantined after persistence failed ({} tool calls persisted): {}",
                    run_id.0,
                    tool_calls.len(),
                    failure_message
                );
            } else {
                error!(
                    "Run {} was already terminal when its loop returned \
                     FailedWithToolCalls — the path that won the race already emitted \
                     the terminal SSE, so this arm stays silent to keep the event \
                     exactly-once (#1046). Not a dropped event. \
                     {} tool calls persisted; DM peer notification fired \
                     unconditionally: {}",
                    run_id.0,
                    tool_calls.len(),
                    source
                );
            }
        }
        Err(e) => {
            // #927 (extends #895): flip the run state to `Failed` BEFORE
            // broadcasting `run_error`. See the `Ok` and
            // `FailedWithToolCalls` arms for the full rationale.
            //
            // #1046 / #1052: gated on the transition bool — see the
            // `FailedWithToolCalls` arm above for the full rationale on
            // first-writer-wins semantics between this arm and the
            // synchronous HTTP `cancel_run` handler from #1050. This
            // generic arm is unreachable through `runtime.run()` in
            // practice (`AgentRuntime::finish_run` re-wraps every
            // non-Cancelled error into `FailedWithToolCalls`), but is
            // kept defensively for synthetic test inputs and
            // direct-runtime-bypass paths.
            let mut failure_message = e.to_string();
            let (failed_transitioned, emit_failure, persistence_failed) = match state
                .run_manager
                .try_mark_run_as_failed(run_id, failure_message.clone())
            {
                Ok(transitioned) => (transitioned, transitioned, false),
                Err(error) => {
                    failure_message = format!("Run failure could not be persisted: {error}");
                    (false, true, true)
                }
            };

            if emit_failure {
                state
                    .run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::run_error(run_id, &failure_message),
                    )
                    .await;
            }

            // Issue #912 — see the `FailedWithToolCalls` arm above.
            // This generic-error arm covers LLM 4xx/5xx, rate-limit,
            // content-policy reject, and runtime errors (context budget
            // exceeded, summary generation failed) — all of which the
            // runtime layer has already persisted as a sanitised
            // `[Run failed: <safe_reason>]` text message via
            // `finish_run`'s `Err(e)` branch.  The lifecycle-layer
            // marker would be a duplicate.

            // Best-effort: see comment in the `FailedWithToolCalls` arm.
            // NOT gated on `failed_transitioned` — `handle_dm_run_failure`
            // is an independent side effect per #1050's design.
            if let Err(end_err) = super::dm_lifecycle::handle_dm_run_failure(
                &state,
                &run_id,
                &session_id,
                agent_id,
                agent_name.as_deref(),
                &context_id,
                is_peer_message,
                // #1258: `interrupted: true` — the run died mid-turn, so
                // whatever this DM turn was going to produce does not exist.
                ConversationEndReason::Errored {
                    message: truncate_error_for_peer(&alms_core::AlmsError::Runtime(
                        failure_message.clone(),
                    )),
                    interrupted: true,
                },
            )
            .await
            {
                warn!(
                    error = %end_err,
                    "handle_dm_run_failure emitted error — DM peer state may be stale"
                );
            }

            if failed_transitioned {
                error!("Run {} failed: {}", run_id.0, e);
            } else if persistence_failed {
                error!(
                    "Run {} failure was quarantined after persistence failed: {}",
                    run_id.0, failure_message
                );
            } else {
                error!(
                    "Run {} was already terminal when its loop returned Err — the path \
                     that won the race already emitted the terminal SSE, so this arm \
                     stays silent to keep the event exactly-once (#1046). Not a \
                     dropped event. DM peer notification fired unconditionally: {}",
                    run_id.0, e
                );
            }
        }
    }

    // Mirror the run's terminal transition onto the agent-scoped
    // session-activity feed (#856) — paired with the
    // `session_activity_started` emitted before the run executed. Fires
    // exactly once per run regardless of which terminal arm the result
    // took (Ok / Cancelled / CancelledWithToolCalls / FailedWithToolCalls
    // / generic Err). Pre-execution cancellation and runtime-construction
    // failure paths handle their own emissions inline above and return
    // early, so this site is reached only when the run actually started.
    state
        .run_manager
        .send_agent_event(
            agent_id,
            run_id,
            session_id,
            SseEventData::session_activity_ended(session_id, run_id, agent_id),
        )
        .await;

    // Forward a `dm_activity_ended` event to the agent's webchat session
    // so the frontend can update the status bar (#688).  This is distinct
    // from `dm_conversation_ended` (which signals the entire DM conversation
    // is over) — `dm_activity_ended` signals that a single DM run finished,
    // allowing the frontend to keep "Chatting with..." visible if more DM
    // runs are expected.
    if let Some(ref peer_name) = dm_peer_for_webchat
        && let Some(target) = super::find_user_facing_session(&state.session_manager, agent_id)
    {
        let dummy_run_id = RunId::new();
        state
            .run_manager
            .send_session_event(
                target.id,
                dummy_run_id,
                SseEventData::dm_activity_ended(target.id, peer_name),
            )
            .await;
    }

    // Update last_active on the agent record (non-fatal).
    if let Some(store) = state.session_manager.store()
        && let Err(e) = store.touch_agent(agent_id)
    {
        warn!("Failed to update last_active for agent {}: {}", agent_id, e);
    }

    state.run_manager.remove_senders(run_id);
    // Defense-in-depth: sweep any other orphaned sender entries for runs
    // that reached terminal state (covers the TOCTOU window in #149).
    state.run_manager.purge_terminal_senders();
    state.run_manager.remove_cancel_token(run_id);
    // Clean up any stale pending approvals for this run
    state.approval_store.clear_for_run(run_id);
    // #1198 exit 5/5: the common terminal tail — reached by every match arm
    // (Ok / Cancelled / CancelledWithToolCalls / FailedWithToolCalls /
    // generic Err) exactly once per run. Feeds the episode tracker with the
    // run's tool records (pending-work scan) and, on quiescence, drives
    // `close_episode` (completion card + record_run + re-arm). Fires AFTER
    // the run's terminal state was persisted so the card reads the final
    // status/output.
    super::notifications::finish_episode_run(&state, episode_job_id, run_id, &episode_tool_calls)
        .await;

    // The queue head is advancing — fan out updated positions to any
    // remaining queued runs on this agent (#831). Emitted once per run
    // regardless of which terminal arm was taken (Ok / Cancelled /
    // CancelledWithToolCalls / FailedWithToolCalls / generic Err).
    broadcast_queue_advance(&state, agent_id).await;
    // `_in_flight_guard` dropped here — signals drain waiters that this run is done.
}

/// Execute one admitted run behind a domain-level panic boundary.
///
/// The keyed queue also catches panics so one bad item cannot kill its worker,
/// but only this layer has enough context to reconcile the run's durable and
/// observable state. Keeping the cleanup here prevents a caught panic from
/// leaving a run, cancellation token, or activity indicator permanently live.
pub(super) async fn execute_run_guarded(state: AppState, params: RunParams) {
    let execution = execute_run(state.clone(), params.clone());
    execute_run_guarded_future(state, params, execution).await;
}

pub(super) async fn execute_run_guarded_future<F>(state: AppState, params: RunParams, execution: F)
where
    F: std::future::Future<Output = ()>,
{
    let run_id = params.run_id;
    let session_id = params.session_id;
    let agent_id = params.agent_id;
    let context_id = params.context_id.clone();
    let is_peer_message = params.is_peer_message;
    let episode_job_id = state.run_manager.get_run(run_id).and_then(|run| run.job_id);

    if AssertUnwindSafe(execution).catch_unwind().await.is_ok() {
        return;
    }

    const PANIC_REASON: &str = "Run panicked during execution";
    error!(run_id = %run_id.0, %session_id, "{PANIC_REASON}");

    // `failure_message` is the operator-facing text (run record + `run_error`
    // SSE on the run's own session, where the raw storage error is the useful
    // thing and the operator owns the run). `peer_failure_message` is the same
    // fact with the storage error sanitised (#911 / #930 / #931): since #1258
    // the DM-peer copy is rendered in the browser's DM-ended banner and
    // persisted as the marker's own text, so a raw storage error — which can
    // carry a database path — must not ride along.
    let (transitioned, failure_message, peer_failure_message, persistence_failed) = match state
        .run_manager
        .try_mark_run_as_failed(run_id, PANIC_REASON.to_string())
    {
        Ok(transitioned) => (
            transitioned,
            PANIC_REASON.to_string(),
            PANIC_REASON.to_string(),
            false,
        ),
        Err(error) => (
            false,
            format!("Run panic could not be persisted: {error}"),
            peer_error_with_prefix("Run panic could not be persisted: ", &error.to_string()),
            true,
        ),
    };
    if transitioned || persistence_failed {
        state
            .run_manager
            .send_event(
                run_id,
                session_id,
                SseEventData::run_error(run_id, &failure_message),
            )
            .await;

        if let Err(end_error) = super::dm_lifecycle::handle_dm_run_failure(
            &state,
            &run_id,
            &session_id,
            agent_id,
            None,
            &context_id,
            is_peer_message,
            // #1258: `interrupted: true` — a panic is the strongest form of
            // "the turn never finished".
            ConversationEndReason::Errored {
                message: peer_failure_message,
                interrupted: true,
            },
        )
        .await
        {
            warn!(
                error = %end_error,
                run_id = %run_id.0,
                "Failed to reconcile DM after run panic"
            );
        }
    }

    // This is intentionally unconditional. A panic can land after the
    // terminal state transition but before the normal activity-ended
    // publication; ended events are run-idempotent for consumers.
    state
        .run_manager
        .send_agent_event(
            agent_id,
            run_id,
            session_id,
            SseEventData::session_activity_ended(session_id, run_id, agent_id),
        )
        .await;

    state.run_manager.remove_senders(run_id);
    state.run_manager.purge_terminal_senders();
    state.run_manager.remove_cancel_token(run_id);
    state.approval_store.clear_for_run(run_id);
    super::notifications::finish_episode_run(&state, episode_job_id, run_id, &[]).await;
    broadcast_queue_advance(&state, agent_id).await;
}

#[cfg(test)]
mod tests {
    use super::super::read_api::{derive_trigger, run_duration_ms};
    use super::*;
    use alms_core::{AgentId, SessionId, job::JobId};

    /// Helper to create a basic run for testing.
    fn test_run() -> Run {
        Run::new(SessionId::new(), AgentId::new(), "test".into())
    }

    #[test]
    fn duration_is_never_negative_when_wall_clock_moves_backwards() {
        let start = Utc::now();
        let earlier_end = start - chrono::Duration::milliseconds(25);

        assert_eq!(run_duration_ms(Some(start), Some(earlier_end)), Some(0));
        assert_eq!(run_duration_ms(Some(start), Some(start)), Some(0));
        assert_eq!(run_duration_ms(Some(start), None), None);
    }

    #[test]
    fn test_derive_trigger_user() {
        let run = test_run();
        assert_eq!(derive_trigger(&run, "web"), "user");
        assert_eq!(derive_trigger(&run, "default"), "user");
        assert_eq!(derive_trigger(&run, "my-context"), "user");
    }

    #[test]
    fn test_derive_trigger_scheduled() {
        let run = Run::for_job(
            SessionId::new(),
            AgentId::new(),
            "job task".into(),
            JobId::new(),
        );
        assert_eq!(derive_trigger(&run, "job_abc"), "scheduled");
        // job_id takes priority even if context_id looks like DM
        assert_eq!(derive_trigger(&run, "dm:alice:bob"), "scheduled");
    }

    #[test]
    fn test_derive_trigger_subagent() {
        let run = Run::for_subagent(
            SessionId::new(),
            AgentId::new(),
            "subtask".into(),
            RunId::new(),
        );
        assert_eq!(derive_trigger(&run, "subagent_task_1"), "subagent");
        // parent_run_id takes priority over context_id prefix
        assert_eq!(derive_trigger(&run, "web"), "subagent");
    }

    #[test]
    fn test_derive_trigger_dm() {
        let run = test_run();
        assert_eq!(derive_trigger(&run, "dm:alice:bob"), "dm");
    }

    #[test]
    fn test_derive_trigger_notification() {
        let run = test_run();
        assert_eq!(derive_trigger(&run, "notifications:alice"), "notification");
    }

    #[test]
    fn test_derive_trigger_telegram() {
        let run = test_run();
        assert_eq!(derive_trigger(&run, "telegram_123"), "telegram");
    }

    #[test]
    fn test_derive_trigger_priority_job_over_context() {
        // job_id should win over parent_run_id and context_id
        let mut run = Run::for_job(
            SessionId::new(),
            AgentId::new(),
            "job+sub".into(),
            JobId::new(),
        );
        run.parent_run_id = Some(RunId::new());
        assert_eq!(derive_trigger(&run, "dm:a:b"), "scheduled");
    }

    #[test]
    fn test_derive_trigger_priority_subagent_over_context() {
        // parent_run_id should win over context_id prefix
        let run = Run::for_subagent(SessionId::new(), AgentId::new(), "sub".into(), RunId::new());
        assert_eq!(derive_trigger(&run, "dm:a:b"), "subagent");
    }

    #[test]
    fn test_truncate_error_for_peer_short_label_unchanged() {
        // The sanitiser collapses Runtime errors to short fixed category
        // labels well under PEER_ERROR_MESSAGE_MAX_LEN — the helper passes
        // them through with no truncation and no ellipsis.
        let err = AlmsError::Runtime("429 Too Many Requests".into());
        let out = truncate_error_for_peer(&err);
        assert_eq!(out, "LLM rate limit exceeded");
        assert!(!out.ends_with("..."));
        assert!(out.len() <= PEER_ERROR_MESSAGE_MAX_LEN);
    }

    #[test]
    fn test_truncate_error_for_peer_cancelled_passes_through() {
        // The Cancelled label is also bounded and passes through unchanged.
        let err = AlmsError::Cancelled;
        let out = truncate_error_for_peer(&err);
        assert_eq!(out, "Run cancelled by user");
    }

    /// #1258 / Tim's S3: the two `Errored` sites that interpolate a foreign
    /// error into self-authored text must sanitise the TAIL only. Since #1258
    /// that string is rendered in the browser's DM-ended banner and persisted
    /// as the marker's own text, so a storage error's database path cannot be
    /// allowed to ride along.
    #[test]
    fn peer_error_with_prefix_sanitises_the_tail_only() {
        let out = peer_error_with_prefix(
            "reply delivery failed: ",
            "Delivery failed: sqlite error at C:\\dev\\alms\\.alms\\alms.db: disk I/O error",
        );
        assert!(
            out.starts_with("reply delivery failed: "),
            "the self-authored prefix says WHAT failed and must survive; got {out:?}"
        );
        assert!(
            !out.contains("alms.db") && !out.contains("C:\\"),
            "the foreign tail must be collapsed by the sanitiser, not passed \
             through; got {out:?}"
        );
    }

    /// The prefix must stay OUTSIDE the sanitiser. Routing the composed
    /// string through `sanitize_error_for_session` instead matches none of
    /// its keywords and collapses the lot to `"Runtime error"`, which is
    /// strictly less useful than what the peer had before.
    #[test]
    fn peer_error_with_prefix_does_not_collapse_the_whole_message() {
        let out = peer_error_with_prefix("Run panic could not be persisted: ", "429 rate limited");
        assert_eq!(
            out,
            "Run panic could not be persisted: LLM rate limit exceeded"
        );
    }

    /// Regression test for #931: a `Runtime` error whose Display string
    /// embeds an API key, hostname, header, or bearer token must NOT
    /// reach the peer agent's notification context. Before the fix,
    /// `truncate_error_for_peer` was a length-only UTF-8 truncator and
    /// the raw provider error would land verbatim in the peer's
    /// `dm_ended` notification body.
    ///
    /// Mirrors the `sanitize_runtime_auth_strips_url_and_keys` test in
    /// `alms-core/src/error.rs` but at the gateway-layer helper boundary
    /// — so a future change that swaps the sanitiser call out (or wires
    /// the helper to a non-sanitising path) is caught here.
    #[test]
    fn test_truncate_error_for_peer_strips_secrets_for_dm_peer() {
        let err = AlmsError::Runtime(
            "HTTP 401 Unauthorized at https://api.example.com (authorization: Bearer sk-test-12345)"
                .into(),
        );
        let out = truncate_error_for_peer(&err);
        // The sanitiser collapses 401/403 Runtime errors to a fixed label.
        assert_eq!(out, "LLM authentication error");
        for needle in [
            "sk-test-",
            "api.example.com",
            "Bearer",
            "authorization",
            "Unauthorized",
        ] {
            assert!(
                !out.contains(needle),
                "DM peer notification text must not contain {needle:?}, got {out:?}"
            );
        }
    }

    /// Regression test for #931: also exercise the
    /// `AlmsError::FailedWithToolCalls` shape used by one of the two
    /// production call sites — the inner `source` is what reaches
    /// `truncate_error_for_peer` after a `&Box<AlmsError>` deref-coerce.
    /// A raw 500-internal-error body containing leaked secrets must not
    /// survive sanitisation through that path either.
    #[test]
    fn test_truncate_error_for_peer_failed_with_tool_calls_path_strips_secrets() {
        let inner = AlmsError::Runtime(
            "provider 500 internal error: secret-key=sk-test-12345 leaked in body".into(),
        );
        // Mirror the production call site shape: `&source` where
        // `source: Box<AlmsError>` deref-coerces to `&AlmsError`.
        let source: Box<AlmsError> = Box::new(inner);
        let out = truncate_error_for_peer(&source);
        assert_eq!(out, "Runtime error");
        assert!(
            !out.contains("sk-test-12345"),
            "API key from inner Runtime body must not survive sanitisation: got {out:?}"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Truncation + UTF-8 char-boundary walkback path (#931 / #936 follow-up)
    //
    // The sanitiser collapses every error variant *except* `ToolExecution`
    // to a short fixed-length category label, so in practice the truncation
    // step is a no-op for those variants. `ToolExecution` is the one
    // variant whose sanitised output length scales with caller input
    // (`format!("Tool execution failed: {}", msg.split(':').next())`), so
    // it is the only path through which the truncation + char-boundary
    // walkback can fire on production data. These tests pin that path so a
    // future "this code is dead, remove it" refactor either preserves the
    // behaviour or notices it has live callers.
    // ─────────────────────────────────────────────────────────────────────

    /// Pins the truncation arm on the only sanitiser output that can
    /// exceed `PEER_ERROR_MESSAGE_MAX_LEN`: a `ToolExecution` error whose
    /// pre-`:` segment is large enough that `"Tool execution failed: <name>"`
    /// overflows the cap. The truncated body must end with `"..."` and the
    /// total length must be exactly `PEER_ERROR_MESSAGE_MAX_LEN + 3`
    /// because the input is pure ASCII (no walkback required).
    #[test]
    fn test_truncate_error_for_peer_oversize_ascii_tool_truncates_with_ellipsis() {
        // 400 ASCII chars with no ':' means the sanitiser keeps the full
        // tool name, producing `"Tool execution failed: aaaa..."` of total
        // length 22 + 400 = 422 > PEER_ERROR_MESSAGE_MAX_LEN (300).
        let long_name = "a".repeat(400);
        let err = AlmsError::ToolExecution(long_name);
        let out = truncate_error_for_peer(&err);
        assert!(
            out.ends_with("..."),
            "oversize sanitiser output must be truncated and ellipsis-terminated, got {out:?}"
        );
        // Body length is exactly PEER_ERROR_MESSAGE_MAX_LEN for pure ASCII
        // (char-boundary walkback is a no-op); total = max + 3 ("...").
        assert_eq!(out.len(), PEER_ERROR_MESSAGE_MAX_LEN + 3);
        assert!(out.starts_with("Tool execution failed: "));
    }

    /// Pins the UTF-8 char-boundary walkback inside the truncation arm.
    /// Constructs a `ToolExecution` error whose sanitised output places a
    /// multibyte codepoint *across* byte offset `PEER_ERROR_MESSAGE_MAX_LEN`,
    /// so a naive `&s[..PEER_ERROR_MESSAGE_MAX_LEN]` would panic. The
    /// helper must walk back to the nearest char boundary at or below the
    /// cap and append `"..."`.
    ///
    /// Without the walkback, slicing `String` on a non-boundary panics —
    /// so this test would fail by panicking, not by producing wrong output.
    #[test]
    fn test_truncate_error_for_peer_multibyte_walks_back_to_char_boundary() {
        // The sanitiser prefix `"Tool execution failed: "` is 23 ASCII
        // bytes. Pad the start of the tool name so that the cap (300)
        // lands inside a 3-byte codepoint — concretely: 23 prefix bytes
        // + N filler ASCII bytes + a run of 3-byte CJK chars, with N
        // chosen so that 300 - (23 + N) is not a multiple of 3.
        //
        // Use N = 1 ("x"): prefix runs 23..24, then "あ" (0xE3 0x81 0x82,
        // 3 bytes each) starts at byte 24. Codepoint boundaries after
        // byte 24 are at 24 + 3k. 300 - 24 = 276, which IS a multiple
        // of 3, so 300 is on a boundary. Use N = 2 instead: 23 + 2 = 25,
        // boundaries at 25 + 3k, 300 - 25 = 275, not a multiple of 3.
        // Walkback must land at the nearest boundary <= 300, which is
        // 25 + 3*91 = 298.
        let mut tool_name = "xx".to_string();
        tool_name.push_str(&"あ".repeat(200)); // 600 bytes; total >> 300
        let err = AlmsError::ToolExecution(tool_name);
        let out = truncate_error_for_peer(&err);
        assert!(out.ends_with("..."));
        let body = out.trim_end_matches("...");
        // Body must end at a UTF-8 char boundary (otherwise `&s[..end]`
        // panics inside the helper, never reaching this assertion).
        assert!(
            body.is_char_boundary(body.len()),
            "truncated body must end on a UTF-8 char boundary"
        );
        assert!(body.len() <= PEER_ERROR_MESSAGE_MAX_LEN);
        // The walkback should land ON the largest valid boundary <= 300,
        // not arbitrarily earlier. With our padding, that boundary is 298.
        assert_eq!(body.len(), 298);
    }

    // ─────────────────────────────────────────────────────────────────────
    // build_summary_client (#866 + #871) — mirror of #860/#861 leak-guard
    // tests in `runs/mod.rs::tests` but on the summary path.
    // ─────────────────────────────────────────────────────────────────────

    /// Build an `LlmConfig` whose `[llm.providers.<name>]` map contains
    /// (anthropic, openrouter) and an in-memory `SecretsStore` with keys
    /// for both. Mirrors the fixture shape used by the #860/#861 tests in
    /// `runs/mod.rs::tests`.
    fn summary_test_fixtures(
        anthropic_entry_model: Option<&str>,
        openrouter_entry_model: Option<&str>,
    ) -> (alms_runtime::LlmClient, alms_core::secrets::SecretsStore) {
        use alms_runtime::llm_types::LlmConfig;
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "openrouter".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::OpenAiCompatible,
                base_url: "https://openrouter.ai/api/v1".into(),
                api_key_env: None,
                api_key: None,
                model: openrouter_entry_model.map(str::to_string),
                auth_scheme: alms_core::config::AuthScheme::Bearer,
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        providers.insert(
            "anthropic".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: None,
                model: anthropic_entry_model.map(str::to_string),
                auth_scheme: alms_core::config::AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let cfg = LlmConfig {
            // Agent's resolved client is on Anthropic with a sonnet model.
            provider: "anthropic".into(),
            api_key: "sk-ant-runtime".into(),
            base_url: "https://api.anthropic.com/v1".into(),
            default_model: "claude-sonnet-4-20250514".into(),
            providers,
            ..LlmConfig::default()
        };
        let llm = alms_runtime::LlmClient::new(cfg).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mut secrets = alms_core::secrets::SecretsStore::load(dir.path().join("secrets.json"))
            .unwrap_or_else(|_| alms_core::secrets::SecretsStore::empty());
        secrets.set_key("anthropic", "sk-ant-runtime").unwrap();
        secrets.set_key("openrouter", "sk-or-runtime").unwrap();
        std::mem::forget(dir);
        (llm, secrets)
    }

    /// Pre-#866 behaviour: when `summary_provider` is `None`, the helper
    /// returns a clone of the agent's `llm` byte-identical to pre-#866
    /// behaviour. Provider, default_model, and base_url all match.
    #[test]
    fn build_summary_client_no_provider_returns_agent_llm_clone() {
        let (llm, secrets) = summary_test_fixtures(None, None);
        let summary = build_summary_client(&llm, None, None, &secrets, AgentId::new());
        assert_eq!(summary.provider(), llm.provider());
        assert_eq!(summary.default_model(), llm.default_model());
    }

    /// #866 happy path: provider AND model both set. Both reach the wire
    /// regardless of any provider-entry model field.
    #[test]
    fn build_summary_client_provider_and_model_set_both_apply() {
        let (llm, secrets) = summary_test_fixtures(None, None);
        let summary = build_summary_client(
            &llm,
            Some("openrouter"),
            Some("minimax/minimax-m2.7"),
            &secrets,
            AgentId::new(),
        );
        assert_eq!(summary.provider(), "openrouter");
        assert_eq!(summary.default_model(), "minimax/minimax-m2.7");
    }

    /// #866 + #871: provider set, no model, AND the new provider entry has
    /// its own `model` field. `apply_provider` rewrites `default_model` to
    /// the entry's model — that's the (provider, model) pair the user
    /// configured, so the leak guard does NOT clear it. This shape is
    /// rejected by the PATCH validator (`SUMMARY_PROVIDER_REQUIRES_MODEL`
    /// fires when `summary_model` is None) but a hand-edited TOML can still
    /// reach this branch — keep the property pinned.
    #[test]
    fn build_summary_client_provider_only_with_entry_model_uses_entry_model() {
        let (llm, secrets) = summary_test_fixtures(None, Some("provider-default-model"));
        let summary =
            build_summary_client(&llm, Some("openrouter"), None, &secrets, AgentId::new());
        assert_eq!(summary.provider(), "openrouter");
        assert_eq!(
            summary.default_model(),
            "provider-default-model",
            "provider-entry model should reach the wire when no \
             summary_model is configured (#866)"
        );
    }

    /// #871 leak guard (the bug Tim flagged): provider set, no model, AND
    /// the new provider entry has NO `model` field. `apply_provider` leaves
    /// `default_model` unchanged, so the cloned client carries the AGENT's
    /// model slug ("claude-sonnet-4-...") into the new (openrouter) wire.
    /// Pre-fix: the agent's slug reaches openrouter and produces a 404 (the
    /// exact symptom #861 was meant to prevent on the agent path). Post-fix:
    /// the leak guard clears `default_model` so the wire request fails fast
    /// with a missing-model error rather than 404'ing on the wrong slug.
    #[test]
    fn build_summary_client_provider_only_no_entry_model_clears_to_fail_fast() {
        let (llm, secrets) = summary_test_fixtures(None, None);
        let agent_model = llm.default_model().to_string();
        let summary =
            build_summary_client(&llm, Some("openrouter"), None, &secrets, AgentId::new());
        assert_eq!(summary.provider(), "openrouter");
        assert_ne!(
            summary.default_model(),
            agent_model,
            "summary_provider switch must NOT leak the agent's model slug \
             onto the summary provider's wire (#871, mirror of #860/#861)"
        );
        assert_eq!(
            summary.default_model(),
            "",
            "leak guard clears default_model when summary_provider is set, \
             summary_model is None, and the provider entry has no model \
             field — wire fails fast with a missing-model error (#871)"
        );
    }

    /// #866 + #871: provider set, model set, and the new provider entry
    /// also has its own `model` field. The explicit `summary_model`
    /// override must win — same property as the per-agent #861 test
    /// (`test_per_agent_provider_with_entry_model_does_not_drop_per_agent_model`).
    #[test]
    fn build_summary_client_explicit_model_wins_over_entry_model() {
        let (llm, secrets) = summary_test_fixtures(None, Some("provider-default-model"));
        let summary = build_summary_client(
            &llm,
            Some("openrouter"),
            Some("user-chosen-summary-model"),
            &secrets,
            AgentId::new(),
        );
        assert_eq!(summary.provider(), "openrouter");
        assert_eq!(
            summary.default_model(),
            "user-chosen-summary-model",
            "explicit summary_model must reach the wire even when the \
             provider entry has its own model field"
        );
    }

    /// Back-compat: when both fields are unset and the agent has a normal
    /// resolved client, the helper short-circuits to a clone with the same
    /// provider+model+base_url. This is the byte-identical-to-pre-#866
    /// guarantee.
    #[test]
    fn build_summary_client_back_compat_when_provider_and_model_both_none() {
        let (llm, secrets) = summary_test_fixtures(None, None);
        let summary = build_summary_client(&llm, None, None, &secrets, AgentId::new());
        assert_eq!(summary.provider(), "anthropic");
        assert_eq!(summary.default_model(), "claude-sonnet-4-20250514");
    }

    /// #1260 — what a run's tool-registry churn is allowed to log.
    ///
    /// These live here, rather than next to the registry in
    /// `alms-sandbox`, because the claim is about the *builder chain
    /// `execute_run` runs above*, and because this crate is the one with
    /// the interest-cache-safe `tracing` capture harness (#1221). Putting
    /// a third copy of that harness in `alms-sandbox` is exactly what
    /// #1282 exists to prevent.
    ///
    /// # What the "a normal run logs no re-registration warning" claim
    /// ranges over
    ///
    /// Every write into a run's tool map goes through one of two checked
    /// entry points — `ToolRegistry::register` and
    /// `ToolRegistry::register_as`. (#1260 also routed the one raw
    /// `DashMap::insert` that bypassed both, the `shell_exec` alias in
    /// `register_builtin_tools_sandboxed`, through `register_as`.) The
    /// callers that reach them during a run are:
    ///
    /// | caller | what it replaces |
    /// |---|---|
    /// | `register_builtin_tools_sandboxed` | nothing — first write of 11 builtins + the `shell_exec` alias |
    /// | `runtime::ToolRegistry::attach_fs_cache_to_registry` | `fs_read`, `fs_write`, `fs_edit` |
    /// | `apply_shell_permissions`, from `AgentRuntime::new` | `shell`, `shell_exec` |
    /// | `with_shell_default_env` | `shell`, `shell_exec` |
    /// | `with_shell_spill` | `shell`, `shell_exec`, six read-family `fs_*` |
    /// | `with_tool_output_truncate` | six read-family `fs_*` — **no shell** |
    /// | `with_extra_fs_read_root` | six read-family `fs_*` |
    /// | `with_project_root` / `with_unrestricted_filesystem` | six `fs_*`, `shell`, `shell_exec` |
    /// | `with_workspace` | nothing — first write of `workspace_read` / `workspace_write` |
    /// | `AgentRuntime::register_tool`, from `runs::tools` | nothing — first write of each of the 8 agent tools |
    ///
    /// The third and sixth rows are a correction (#1317 review). The table
    /// first credited the shell pair to `with_tool_output_truncate` and
    /// omitted `apply_shell_permissions` — two errors that cancel, so
    /// `shell`'s total of five was right for the wrong reasons and the
    /// assertions below still passed. Worth stating rather than silently
    /// swapping: a total that survives a wrong decomposition is exactly
    /// the failure the counts are supposed to catch, and it survived
    /// because the count and its explanation were derived together.
    ///
    /// The default path replaces 29 names per run, which is the shape of
    /// the issue's histogram: four apiece for `fs_read` / `fs_write` /
    /// `fs_edit` (they carry the extra `attach_fs_cache_to_registry`
    /// pass), three apiece for `fs_list` / `fs_grep` / `fs_glob`, four
    /// apiece for `shell` and `shell_exec` — the 76:57 ratio the issue
    /// counted, over 19 runs. The table is not merely consistent with
    /// that histogram, it is the only model consistent with it: the
    /// rival — in which `with_workspace` re-registers the fs_* tools as
    /// well, which the doc on `refresh_fs_tools_for_extras` asserted
    /// until #1260 corrected it — predicts 5:4, and 76 is not divisible
    /// by 5. `expected_registrations` below turns the table into
    /// assertions so it cannot drift back into prose.
    ///
    /// The last two rows are the only ones these tests do not build,
    /// because they need an `AppState`. They cannot contribute to the
    /// claim: each of those ten tools is the sole writer of its name, so
    /// they hit `register` with no existing entry to compare against.
    /// `alms-coordinator`'s subagent chain is a strict subset of the
    /// builders below and adds no shape of its own.
    ///
    /// # How these rows were graded
    ///
    /// **A row is only pinned by a mutation it is the sole killer of.**
    /// "Every mutant was killed" grades the suite; it says nothing about
    /// any individual row, because a row that never kills alone is
    /// carried by its neighbours and can be deleted without the table
    /// noticing. Adding a row to a suite whose mutants already all die
    /// is therefore not evidence the row works — it is the shape a
    /// redundant row takes.
    ///
    /// #1260 ran into that twice. The `impl_type_id` guard in
    /// `alms-sandbox` could not fail at all, and the reason it looked
    /// fine was that every discriminator mutation was killed by the
    /// impostor rows instead — a negative result that reads as a pass.
    /// The `shell` composition below was wrong in two ways that
    /// cancelled, and the reason it survived was that the assertion and
    /// its justification were derived together, so agreement between
    /// them was agreement between two copies of one derivation.
    ///
    /// The check is free once per-row killers are printed: does each row
    /// appear *alone* in some kill set? The two that do not are declared
    /// in the PR rather than left to look like coverage.
    mod tool_registration_logging {
        use super::*;
        use alms_test_support::capture_logs;

        /// The sandbox branch `execute_run` picks between. All three are
        /// exercised: they register different tools and the middle one
        /// adds an extra `with_extra_fs_read_root` pass.
        #[derive(Clone, Copy)]
        enum SandboxBranch {
            /// The default — `with_project_root(state.project_root)`.
            ProjectRoot,
            /// `worktree_mode = "git"` (#946).
            Worktree,
            /// `[security].allow_full_os_access` (#947).
            Unrestricted,
        }

        /// The registration sequence `execute_run` performs, in its
        /// order. Mirrors the builder chain above; when a builder is
        /// added there it belongs here too, or these tests quietly stop
        /// covering it.
        fn build_runtime_like_a_run(branch: SandboxBranch, root: &std::path::Path) {
            let project_root = root.join("project");
            let data_dir = root.join("data");
            let workspace_dir = root.join("workspace");
            std::fs::create_dir_all(&project_root).unwrap();
            std::fs::create_dir_all(&data_dir).unwrap();
            std::fs::create_dir_all(&workspace_dir).unwrap();

            let llm = test_llm();
            let run_id = RunId::new();
            let config = alms_runtime::AgentConfig {
                sandbox_root: project_root.display().to_string(),
                ..Default::default()
            };

            let (runtime_tx, _rx) = mpsc::unbounded_channel::<RuntimeEvent>();
            let mut runtime = alms_runtime::AgentRuntime::new(AgentId::new(), config, llm)
                .expect("runtime construction must succeed")
                .with_event_sender(runtime_tx)
                .with_run_id(run_id)
                .with_cancel_token(CancellationToken::new())
                .with_agent_name("registry-noise-probe".to_string());

            let shell_env =
                alms_core::build_shell_default_env(Some(&data_dir), Some(&workspace_dir));
            runtime = runtime.with_shell_default_env(shell_env);
            runtime = runtime.with_shell_spill(
                data_dir
                    .join(alms_runtime::spill::SPILL_DIR_NAME)
                    .join(run_id.0.to_string()),
                true,
            );
            runtime = runtime.with_tool_output_truncate(
                data_dir
                    .join(alms_runtime::tool_output_truncate::TOOL_OUTPUT_DIR_NAME)
                    .join(run_id.0.to_string()),
                true,
                8_000,
                200,
            );

            runtime = match branch {
                SandboxBranch::ProjectRoot => runtime.with_project_root(project_root.clone()),
                SandboxBranch::Worktree => {
                    let worktree = project_root
                        .join(".alms")
                        .join("worktrees")
                        .join("registry-noise-probe");
                    std::fs::create_dir_all(&worktree).unwrap();
                    runtime
                        .with_extra_fs_read_root(project_root.join(".alms").join("agents"))
                        .with_project_root(worktree)
                }
                SandboxBranch::Unrestricted => runtime.with_unrestricted_filesystem(),
            };

            let workspace =
                alms_runtime::AgentWorkspace::new(&workspace_dir, "registry-noise-probe");
            let _runtime = runtime.with_workspace(workspace);
        }

        /// A minimal LLM client — deliberately *not*
        /// `summary_test_fixtures`, whose `SecretsStore` emits three
        /// unrelated WARNs of its own and would put the volume row
        /// below over budget for reasons that have nothing to do with
        /// tool registration.
        fn test_llm() -> alms_runtime::LlmClient {
            alms_runtime::LlmClient::new(alms_runtime::LlmConfig {
                provider: "anthropic".into(),
                api_key: "sk-ant-test".into(),
                base_url: "https://api.anthropic.com/v1".into(),
                default_model: "claude-sonnet-4-20250514".into(),
                ..Default::default()
            })
            .expect("test LLM client must build")
        }

        /// Counts the lines of a capture, ignoring the trailing newline.
        fn line_count(captured: &str) -> usize {
            captured.lines().filter(|l| !l.trim().is_empty()).count()
        }

        /// How many times the chain registered `name` under its primary
        /// name. Matched to end-of-line so `shell` cannot absorb a
        /// hypothetical `shell_*`; the alias uses a different message and
        /// is counted by [`alias_registrations`].
        fn registrations(captured: &str, name: &str) -> usize {
            captured
                .matches(&format!("Registering tool: {name}\n"))
                .count()
        }

        /// How many times the chain pointed `shell_exec` at a tool.
        fn alias_registrations(captured: &str) -> usize {
            captured
                .matches("Registering tool alias: 'shell_exec'")
                .count()
        }

        /// The registration count each name should reach, derived from
        /// the caller table in the module doc rather than read off a run.
        /// Registrations, not replacements — the first one is the insert,
        /// so the issue's histogram numbers are these minus one.
        ///
        /// `fs_read` / `fs_write` / `fs_edit` sit one above the other
        /// three because `attach_fs_cache_to_registry` covers exactly the
        /// cache-aware family while `register_fs_tools` covers all six.
        /// That asymmetry is the whole of the 4:3 ratio, and it is why
        /// the stale doc on `refresh_fs_tools_for_extras` mattered: the
        /// model it described — `with_workspace` re-registering the fs_*
        /// tools as well — predicts 5:4, and 76 is not divisible by 5.
        fn expected_registrations(branch: SandboxBranch) -> Vec<(&'static str, usize)> {
            // The worktree branch pushes a sibling-read root, and
            // `with_extra_fs_read_root` refreshes the read-family tools —
            // so it costs the six fs_* one extra pass and leaves `shell`
            // alone.
            let extra = match branch {
                SandboxBranch::Worktree => 1,
                SandboxBranch::ProjectRoot | SandboxBranch::Unrestricted => 0,
            };
            vec![
                // builtins + attach_fs_cache + spill + tool-output + root
                ("fs_read", 5 + extra),
                ("fs_write", 5 + extra),
                ("fs_edit", 5 + extra),
                // builtins + spill + tool-output + root
                ("fs_list", 4 + extra),
                ("fs_grep", 4 + extra),
                ("fs_glob", 4 + extra),
                // builtins + apply_shell_permissions + default_env
                // + spill + root. NOT `with_tool_output_truncate`, whose
                // only registration effect is `refresh_fs_tools_for_extras`
                // — which is also why `extra` above does not apply here.
                ("shell", 5),
            ]
        }

        /// The load-bearing row, run over each sandbox branch.
        ///
        /// The capture is taken at DEBUG so one buffer carries both
        /// halves: a "no warnings" assertion over a chain that registered
        /// nothing would be satisfied vacuously, and the counts are what
        /// rule that out.
        ///
        /// The counts are also the enumeration itself. The claim this
        /// module rests on — that the caller table in its doc comment is
        /// complete rather than sampled — was prose, checked against the
        /// issue's histogram by hand. Asserting the per-name numbers
        /// makes the 4:3 ratio an artifact of the build rather than a
        /// claim about it, and turns "a builder was added to
        /// `execute_run` and nobody mirrored it here" from silent drift
        /// into a failing test.
        #[test]
        fn a_runs_builder_chain_reregisters_tools_without_warning() {
            for (label, branch) in [
                ("project_root", SandboxBranch::ProjectRoot),
                ("worktree", SandboxBranch::Worktree),
                ("unrestricted", SandboxBranch::Unrestricted),
            ] {
                let dir = tempfile::tempdir().unwrap();
                let captured = capture_logs(tracing::Level::DEBUG, || {
                    build_runtime_like_a_run(branch, dir.path());
                });

                // Collected rather than asserted one at a time: which
                // names moved *together* is the diagnostic. A builder that
                // registers only fs_* shifts six counts and leaves `shell`
                // alone, and that contrast is what distinguishes the real
                // contributors from the plausible ones — the distinction
                // the caller table got wrong for `with_tool_output_truncate`
                // until the #1317 review. Failing on the first mismatch
                // hides exactly that shape.
                let mismatches: Vec<String> = expected_registrations(branch)
                    .into_iter()
                    .filter_map(|(name, expected)| {
                        let actual = registrations(&captured, name);
                        (actual != expected)
                            .then(|| format!("{name}: expected {expected}, saw {actual}"))
                    })
                    .collect();
                assert!(
                    mismatches.is_empty(),
                    "[{label}] registration counts moved: {}. The builder \
                     sequence changed and the caller table in this module's \
                     doc comment no longer enumerates it — check which names \
                     moved together before editing the numbers.",
                    mismatches.join("; ")
                );
                assert_eq!(
                    alias_registrations(&captured),
                    5,
                    "[{label}] every site that registers `shell` registers \
                     the alias in the same breath, so the two counts move \
                     together — a divergence means one of the five sites \
                     grew or lost its `register_arc_as` call"
                );

                assert!(
                    !captured.contains(alms_runtime::TOOL_COLLISION_WARNING),
                    "[{label}] re-registering the same tool for a new run is \
                     the normal lifecycle and must not warn. Captured:\n{captured}"
                );
            }
        }

        /// The complement. Without it, deleting both warnings outright
        /// would satisfy the row above just as well — and a
        /// discriminator that never fires re-hides the class of bug the
        /// noise was concealing in the first place.
        ///
        /// Also the reason the negative assertion above cannot go stale:
        /// both rows look for `TOOL_COLLISION_WARNING`, which is the
        /// registry's own constant rather than a copy of its text, and
        /// this row fails the moment it stops being emitted.
        #[test]
        fn a_different_implementation_taking_an_established_name_still_warns() {
            let dir = tempfile::tempdir().unwrap();
            let project_root = dir.path().join("project");
            std::fs::create_dir_all(&project_root).unwrap();

            let llm = test_llm();
            let config = alms_runtime::AgentConfig {
                sandbox_root: project_root.display().to_string(),
                ..Default::default()
            };
            let runtime = alms_runtime::AgentRuntime::new(AgentId::new(), config, llm).unwrap();

            let captured = capture_logs(tracing::Level::WARN, || {
                runtime.register_tool(std::sync::Arc::new(ImpostorFsRead));
            });

            assert!(
                captured.contains(alms_runtime::TOOL_COLLISION_WARNING),
                "a foreign implementation claiming the `fs_read` name is the \
                 one case worth a WARN. Captured:\n{captured}"
            );
            assert!(
                captured.contains("fs_read"),
                "the warning must name the tool that was displaced. \
                 Captured:\n{captured}"
            );
        }

        /// `register_as` has its own warning and its own branch, so it
        /// needs its own complement — the builder-chain row above pins
        /// only that the alias's *benign* re-registration is silent, and
        /// silence is equally satisfied by an alias path that can never
        /// speak.
        ///
        /// Re-pointing `shell_exec` at a tool that calls itself
        /// something else is the case the alias check exists for: the
        /// map key stays put while the thing it resolves to changes
        /// underneath it.
        #[test]
        fn an_alias_repointed_at_a_different_tool_still_warns() {
            let registry = alms_runtime::tools::ToolRegistry::with_builtins();

            let captured = capture_logs(tracing::Level::WARN, || {
                registry.register_arc_as("shell_exec", std::sync::Arc::new(ImpostorFsRead));
            });

            assert!(
                captured.contains(alms_runtime::TOOL_COLLISION_WARNING),
                "an alias now resolving to a different tool is worth saying \
                 out loud. Captured:\n{captured}"
            );
            assert!(
                captured.contains("shell_exec"),
                "the warning must name the alias. Captured:\n{captured}"
            );
        }

        /// Total log volume over a run's registration churn, at WARN and
        /// at INFO. The issue's third acceptance criterion is about
        /// volume rather than about any particular message, so this row
        /// is phrased the same way and survives rewording of either
        /// warning.
        ///
        /// Budgets, not measurements. Today the chain emits **1** WARN
        /// (the non-Linux shell-sandbox notice; on Linux that one is an
        /// INFO and the count is 0) and **5** lines at INFO — the two
        /// LLM-client lines, "Filesystem sandbox active", the
        /// shell-sandbox notice and one "Registered built-in tools"
        /// summary. The slack above those is there so an ordinary new
        /// log line does not fail the build, while the regressions this
        /// row exists for stay far outside it: putting the
        /// per-registration `warn!` back on the happy path takes WARN to
        /// ~30, and putting the per-registration `info!` back takes INFO
        /// to ~40.
        #[test]
        fn a_runs_registration_churn_is_quiet_at_warn_and_info() {
            let dir = tempfile::tempdir().unwrap();
            let warns = capture_logs(tracing::Level::WARN, || {
                build_runtime_like_a_run(SandboxBranch::ProjectRoot, dir.path());
            });
            assert!(
                line_count(&warns) <= 3,
                "a run's tool-registry churn must not produce \
                 readable-log-destroying WARN volume; got {} line(s):\n{warns}",
                line_count(&warns)
            );

            let dir = tempfile::tempdir().unwrap();
            let infos = capture_logs(tracing::Level::INFO, || {
                build_runtime_like_a_run(SandboxBranch::ProjectRoot, dir.path());
            });
            assert!(
                line_count(&infos) <= 10,
                "the same applies one level down — INFO carried a line per \
                 registration, which is the same non-event the `debug!` \
                 immediately above it already reports; got {} line(s):\n{infos}",
                line_count(&infos)
            );
        }

        /// A `Tool` that is not `FsReadTool` but claims the `fs_read`
        /// name — the collision the registry should still surface.
        #[derive(Debug)]
        struct ImpostorFsRead;

        #[async_trait::async_trait]
        impl alms_tools::Tool for ImpostorFsRead {
            fn name(&self) -> &str {
                "fs_read"
            }
            fn description(&self) -> &str {
                "not the real fs_read"
            }
            async fn execute(
                &self,
                _params: serde_json::Value,
            ) -> alms_tools::SandboxResult<serde_json::Value> {
                Ok(serde_json::Value::Null)
            }
        }
    }
}
