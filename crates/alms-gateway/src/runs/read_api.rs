// SPDX-License-Identifier: Apache-2.0

//! Read-only run HTTP endpoints and their presentation models.

use crate::api_error;
use crate::server::AppState;
use alms_core::{
    AgentId, Run, RunId, RunStatus, RunStatusResponse, SessionId, classify_session_type,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::instrument;

/// `GET /runs?session_id=<uuid>&limit=<n>` — list runs for a session (existing)
/// `GET /runs?agent_id=<uuid>&limit=<n>` — list runs across all sessions for an agent
///
/// Exactly one of `session_id` or `agent_id` must be provided. Providing both
/// returns 400 BAD_REQUEST.
/// When `agent_id` is provided, the response includes enriched run entries
/// with `session_type`, `trigger`, `context_id`, and `duration_ms` fields
/// for the agent run log panel.
#[instrument(level = "info", skip(state, params))]
pub async fn list_runs(
    State(state): State<AppState>,
    Query(params): Query<ListRunsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let limit = params.limit.unwrap_or(50);

    // Reject ambiguous requests that supply both filters.
    if params.session_id.is_some() && params.agent_id.is_some() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "AMBIGUOUS_FILTER",
            "Provide either `session_id` or `agent_id`, not both",
        ));
    }

    if let Some(agent_id) = params.agent_id {
        // Agent-level listing: cross-session runs with enriched metadata.
        let runs = state.run_manager.list_by_agent(agent_id, limit);
        let entries: Vec<AgentRunEntry> = runs
            .into_iter()
            .map(|run| enrich_run(&state, run))
            .collect();
        Ok(Json(serde_json::json!({ "runs": entries })))
    } else if let Some(session_id) = params.session_id {
        // Session-level listing: original behaviour (backwards-compatible).
        let runs = state.run_manager.list_by_session(session_id, limit);
        let responses: Vec<RunStatusResponse> =
            runs.into_iter().map(RunStatusResponse::from).collect();
        Ok(Json(serde_json::json!({ "runs": responses })))
    } else {
        Err(api_error(
            StatusCode::BAD_REQUEST,
            "MISSING_FILTER",
            "At least one of `session_id` or `agent_id` must be provided",
        ))
    }
}

#[derive(Debug, Deserialize)]
pub struct ListRunsQuery {
    pub session_id: Option<SessionId>,
    pub agent_id: Option<AgentId>,
    pub limit: Option<usize>,
}

/// Enriched run entry for the agent run log panel.
///
/// Extends `RunStatusResponse` with session type classification, trigger
/// derivation, context ID, and computed duration.
#[derive(Debug, Serialize)]
pub struct AgentRunEntry {
    pub run_id: RunId,
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub status: RunStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    pub usage: Option<alms_core::TokenUsage>,
    pub ts: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<alms_core::JobId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<RunId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_count: Option<u32>,
    /// Session type: "chat", "dm", "notification", "job", "subagent",
    /// "telegram", "episodic"
    pub session_type: String,
    /// Trigger derivation: "user", "scheduled", "subagent", "dm",
    /// "notification", "telegram"
    pub trigger: String,
    /// The session's context_id (for display and navigation).
    pub context_id: String,
    /// Run duration in milliseconds (None if run has not ended).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
}

pub(super) fn run_duration_ms(
    started_at: Option<chrono::DateTime<Utc>>,
    ended_at: Option<chrono::DateTime<Utc>>,
) -> Option<i64> {
    match (started_at, ended_at) {
        (Some(start), Some(end)) => Some((end - start).num_milliseconds().max(0)),
        _ => None,
    }
}

/// Enrich a `Run` with session type, trigger, context_id, and duration.
fn enrich_run(state: &AppState, run: Run) -> AgentRunEntry {
    // Look up the session to get context_id for classification.
    let (context_id, session_type) = match state.session_manager.get(run.session_id) {
        Ok(session) => {
            let st = classify_session_type(&session.context_id).to_string();
            (session.context_id.clone(), st)
        }
        Err(_) => ("unknown".to_string(), "chat".to_string()),
    };

    // Derive trigger from run metadata and session context.
    let trigger = derive_trigger(&run, &context_id);

    // Compute duration if both started_at and ended_at are present.
    let duration_ms = run_duration_ms(run.started_at, run.ended_at);

    let ts = run
        .ended_at
        .unwrap_or_else(|| run.started_at.unwrap_or(run.created_at));

    // Attach tool call count if SQLite is available.
    let tool_call_count = state
        .session_manager
        .store()
        .and_then(|store| store.count_tool_calls(run.run_id).ok());

    /// Max characters to include in the `response` field of listing entries.
    /// Full text is available via `GET /runs/{run_id}`.
    const RESPONSE_TRUNCATE_LEN: usize = 200;

    let status = run.status();
    let response = run.output.map(|s| {
        if s.len() > RESPONSE_TRUNCATE_LEN {
            let mut truncated = s[..RESPONSE_TRUNCATE_LEN].to_string();
            truncated.push_str("...");
            truncated
        } else {
            s
        }
    });

    AgentRunEntry {
        run_id: run.run_id,
        session_id: run.session_id,
        agent_id: run.agent_id,
        status,
        response,
        error: run.error,
        started_at: run.started_at,
        ended_at: run.ended_at,
        usage: run.usage,
        ts,
        job_id: run.job_id,
        parent_run_id: run.parent_run_id,
        tool_call_count,
        session_type,
        trigger,
        context_id,
        duration_ms,
    }
}

/// Derive the trigger type for a run based on its metadata and session context.
///
/// Priority:
/// 1. `job_id` present -> "scheduled"
/// 2. `parent_run_id` present -> "subagent"
/// 3. Delegate to [`classify_session_type`] for context_id-based classification,
///    mapping session types to trigger names (dm, notification, telegram).
/// 4. Default -> "user"
pub(super) fn derive_trigger(run: &Run, context_id: &str) -> String {
    if run.job_id.is_some() {
        return "scheduled".to_string();
    }
    if run.parent_run_id.is_some() {
        return "subagent".to_string();
    }
    // Delegate to the canonical session type classifier for context_id-based
    // triggers, avoiding duplicated prefix checks.
    match classify_session_type(context_id) {
        "dm" => "dm".to_string(),
        "notification" => "notification".to_string(),
        "telegram" => "telegram".to_string(),
        _ => "user".to_string(),
    }
}

/// GET /runs/{run_id}/tool-calls — list tool call records for a run.
#[instrument(level = "info", skip(state), fields(run_id = %run_id.0))]
pub async fn get_run_tool_calls(
    State(state): State<AppState>,
    Path(run_id): Path<RunId>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    // Verify the run exists.
    state
        .run_manager
        .get_run(run_id)
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "NOT_FOUND", "Run not found"))?;

    let records = state
        .session_manager
        .store()
        .map(|store| store.load_tool_calls(run_id))
        .transpose()
        .map_err(|e| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                format!("Failed to load tool calls: {e}"),
            )
        })?
        .unwrap_or_default();

    Ok(Json(serde_json::json!({
        "run_id": run_id.0.to_string(),
        "tool_calls": records,
    })))
}

/// GET /runs/{run_id}/reasoning — rehydrate accumulated extended-thinking
/// (reasoning) text for the **current in-flight turn** of a run (#1043, #1077).
///
/// Reasoning text is streamed as `reasoning_delta` SSE events and only
/// persisted to the session-messages store as `reasoning_blocks` metadata
/// **at end-of-turn** via `persist_assistant_tool_calls`. While a turn is
/// in flight, the only authoritative record lives in the per-session SSE
/// event log. If an operator reloads the page mid-turn, the messages GET
/// returns no reasoning yet for that turn, and the default SSE replay
/// cursor sits at the session HWM — past every `reasoning_delta` that has
/// already fired. The reasoning panel would therefore show only whatever
/// streams in after the reload, throwing away potentially the bulk of the
/// model's current thinking trace (see issue #1043 acceptance criteria).
///
/// This endpoint reads the per-session SSE event log, filters to
/// `reasoning_delta` events for the given run (skipping `source_agent`
/// entries, which the UI suppresses — they belong to subagent reasoning
/// panels rendered elsewhere), and concatenates the `text` fields in
/// event-id order. The response carries the maximum `event_id` of any
/// included event so the client can pass it as `?last_event_id=<n>` on
/// the subsequent SSE open call. Sampling the event-id during the same
/// snapshot the text is built from makes the rehydrate→reconnect handoff
/// race-free: every event reflected in the returned text has an id ≤ the
/// returned `last_event_id`, so the SSE stream replays only events that
/// were NOT included, and the per-delta append in the UI's
/// `reasoning_delta` handler appends to (not duplicates) the rehydrated
/// text.
///
/// **Per-turn scoping (#1077)**: a run can span multiple LLM turns, each
/// ending with one or more `tool_start` / `tool_end` events. Prior turns'
/// reasoning is already persisted to the message store as
/// `reasoning_blocks` metadata on the sealed assistant message, and the
/// UI rehydrates each turn's bubble from that source on reload. Including
/// past turns' deltas in this response too would double-render them — the
/// sealed bubble shows them once, then this concatenated blob shows them
/// again at the bottom in an unsealed bubble. We therefore compute the
/// latest **parent-agent** `tool_start` / `tool_end` boundary in this run
/// and drop every `reasoning_delta` whose `event_id` is at or before it.
/// Subagent tool events (with `source_agent` set) do **not** move the
/// boundary — the parent's turn frame is independent of subagent activity.
///
/// First-turn case: when no parent-agent tool event has fired yet, the
/// boundary is unset and every `reasoning_delta` in this run is included.
/// This preserves the original #1043 contract for the first turn of a run
/// (which is also the only turn for tool-less runs).
///
/// Returns an empty `text` and `last_event_id: null` when the run has no
/// recorded post-boundary reasoning yet — the client can safely call this
/// on every reload regardless of run state.
///
/// **Terminal runs (#1133):** when `run.status().is_terminal()`, the response
/// is forced to `{ text: "", last_event_id: null, terminal: true,
/// seal_event_id: <id|null> }`. Reasoning is sealed onto the run's assistant
/// message in history, so this endpoint must not re-seed it (double-render)
/// nor hand back a cursor that could overshoot the run's terminal SSE event
/// (stuck spinner). The additive `terminal` boolean is authoritative because
/// empty `text` alone is overloaded: a *live* run with no post-boundary
/// reasoning this turn also returns empty text + a null cursor. A live run
/// returns its accumulated `text` / cursor with `terminal: false`.
///
/// **`seal_event_id` (#1133 Codex #3 / sub-race B):** the session-event-log
/// id of this run's terminal SSE event (`run_finished` / `run_cancelled` /
/// `run_error`), or `null` if none has been logged yet. It is a SEPARATE
/// field from the `last_event_id` reasoning cursor (which stays `null` on
/// terminal to avoid overshoot): it is the *coverage anchor* the frontend
/// uses to decide whether the loaded history already contains this run's
/// sealed reasoning. The runtime appends the final assistant message
/// (carrying `reasoning_blocks`) STRICTLY BEFORE the gateway flips the run
/// terminal and broadcasts the terminal event (`append_message` →
/// `mark_run_as_completed` → `send_event(run_finished)` in `execute_run`),
/// with no history mutation in between, and the messages-GET samples its
/// high-water mark in the same id space just before reading history. So
/// `history_hwm >= seal_event_id` ⟺ the history read ran after the seal ⟺
/// the sealed reasoning is present. The frontend gates its load-time
/// `reasoning_delta` suppress-set on that comparison: deduped against the
/// sealed bubble when history covers it (sub-race A), left to replay-and-
/// render when it does not (sub-race B — messages-GET resolved before the
/// seal landed).
#[instrument(level = "info", skip(state), fields(run_id = %run_id.0))]
pub async fn get_run_reasoning(
    State(state): State<AppState>,
    Path(run_id): Path<RunId>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let run = state
        .run_manager
        .get_run(run_id)
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "NOT_FOUND", "Run not found"))?;

    let events = state
        .run_manager
        .session_events_from(run.session_id, 0)
        .await;

    // First pass — compute the parent-agent turn boundary for this run.
    //
    // A turn boundary is the latest `tool_start` / `tool_end` event in
    // this run whose `source_agent` is unset (i.e. emitted by the parent
    // agent itself, not by a subagent). Reasoning deltas at or before
    // this id belong to a turn whose assistant message has already been
    // sealed and persisted with `reasoning_blocks` metadata; the UI
    // rehydrates those from the message store and this endpoint must
    // NOT return them again, or the prior turn's reasoning renders twice
    // — once on the sealed bubble, once concatenated into a trailing
    // unsealed bubble (the #1077 symptom).
    //
    // `tool_start` without a matching `tool_end` (approval-paused, or
    // cancelled mid-call) still moves the boundary correctly — the
    // unfinished turn's reasoning is by definition older than the next
    // delta that would belong to a fresh turn.
    //
    // The same pass also records the run's seal anchor — the terminal SSE
    // event's id (see the `seal_event_id` doc-comment rationale). We take the
    // *minimum* terminal id, since the first terminal event is when the seal
    // landed; there is normally exactly one (#1046/#1052 gate the cancel-vs-
    // completion duplicate), so min == max in practice.
    //
    // #1162 sym-2: a `stream_reset` event is also a boundary. When the
    // streaming attempt painted a partial of THIS turn and then faulted, the
    // runtime emitted those `reasoning_delta`s (durable), fired a
    // `stream_reset`, and re-emitted the buffered full reasoning as a fresh
    // `reasoning_delta` after the reset. The abandoned partial deltas sit in
    // the log at ids ≤ the reset id; folding the reset into the boundary drops
    // them so this endpoint rehydrates only the re-emitted full reasoning —
    // exactly what the live UI shows after handling the reset.
    let mut latest_turn_boundary: Option<u64> = None;
    let mut seal_event_id: Option<u64> = None;
    for ev in &events {
        if ev.run_id != run_id {
            continue;
        }
        if matches!(
            ev.event_type.as_str(),
            "run_finished" | "run_cancelled" | "run_error"
        ) {
            seal_event_id = Some(
                seal_event_id
                    .map(|s| s.min(ev.event_id))
                    .unwrap_or(ev.event_id),
            );
            continue;
        }
        // A parent-agent `stream_reset` moves the boundary past the abandoned
        // partial reasoning. Subagent resets are suppressed upstream (their
        // deltas never reach the parent stream), but mirror the same
        // `source_agent` filter here for symmetry with the tool-boundary pass.
        if ev.event_type == "stream_reset" {
            let is_subagent = ev
                .data
                .get("source_agent")
                .map(|v| !v.is_null())
                .unwrap_or(false);
            if !is_subagent {
                latest_turn_boundary = Some(
                    latest_turn_boundary
                        .map(|b| b.max(ev.event_id))
                        .unwrap_or(ev.event_id),
                );
            }
            continue;
        }
        if ev.event_type != "tool_start" && ev.event_type != "tool_end" {
            continue;
        }
        let is_subagent = ev
            .data
            .get("source_agent")
            .map(|v| !v.is_null())
            .unwrap_or(false);
        if is_subagent {
            continue;
        }
        // Events are appended in event-id order, but compute max
        // explicitly rather than assuming iteration order so the contract
        // is stable against any future event_log reordering.
        latest_turn_boundary = Some(
            latest_turn_boundary
                .map(|b| b.max(ev.event_id))
                .unwrap_or(ev.event_id),
        );
    }

    let mut text = String::new();
    let mut last_event_id: Option<u64> = None;
    for ev in events {
        if ev.run_id != run_id {
            continue;
        }
        if ev.event_type != "reasoning_delta" {
            continue;
        }
        // Drop deltas from sealed prior turns — their reasoning is
        // already rehydrated by the messages GET's `reasoning_blocks`
        // metadata path. See doc comment above for the #1077 rationale.
        if let Some(boundary) = latest_turn_boundary
            && ev.event_id <= boundary
        {
            continue;
        }
        // Subagent reasoning is suppressed in the main panel — the UI's
        // `reasoning_delta` handler in `use-session-stream.js` early-returns
        // on `source_agent` set. Mirror that filter here so rehydration
        // doesn't surface text the live stream would have dropped.
        let is_subagent = ev
            .data
            .get("source_agent")
            .map(|v| !v.is_null())
            .unwrap_or(false);
        if is_subagent {
            continue;
        }
        if let Some(t) = ev.data.get("text").and_then(|v| v.as_str()) {
            text.push_str(t);
        }
        last_event_id = Some(ev.event_id);
    }

    // Terminal-run seal (#1133 / A3-1). `reasoning_delta` events are
    // non-ephemeral, so unlike `get_run_text` (whose in-memory buffer is
    // evicted on terminal transition) this endpoint would otherwise keep
    // serving a completed run's final-turn reasoning from the durable event
    // log, double-rendering it on top of the sealed assistant bubble; and any
    // non-null cursor risks the client overshooting the terminal event (on the
    // HTTP-cancel path a trailing delta can even outrank `run_cancelled`),
    // leaving the spinner stuck. So for a terminal run we blank the text and
    // null the cursor; see the doc comment for why `terminal` (not empty-text)
    // is the authoritative signal the frontend keys off.
    //
    // Re-fetch the run status AFTER the `session_events_from` await (#1133 C1).
    // The `run` snapshot was cloned before that in-memory await; if the run
    // terminalizes *during* the await window the stale snapshot still reads
    // `Running`, and we'd seal `terminal:false` with a live cursor — the exact
    // A3-1 double-render the seal prevents, inside a narrow race. `get_run` is
    // a cheap synchronous DashMap clone. If the run was evicted between the two
    // lookups (`None`), fall back to the pre-await snapshot's status — the
    // worst case is the same window we already tolerated before this fix.
    let terminal = state
        .run_manager
        .get_run(run_id)
        .map(|fresh| fresh.status())
        .unwrap_or(run.status())
        .is_terminal();
    if terminal {
        text.clear();
        last_event_id = None;
    } else {
        // The seal anchor is meaningful only for a terminal run; a live run is
        // never added to the frontend suppress-set, so do not advertise one.
        seal_event_id = None;
    }

    Ok(Json(serde_json::json!({
        "run_id": run_id.0.to_string(),
        "text": text,
        "last_event_id": last_event_id,
        "terminal": terminal,
        "seal_event_id": seal_event_id,
    })))
}

/// GET /runs/{run_id}/text — rehydrate accumulated in-flight visible-reply
/// text for the **current parent-agent turn** of a run (#1107).
///
/// Mirror endpoint of [`get_run_reasoning`] for the visible reply channel.
/// Visible text streams as `token_delta` SSE events which are flagged
/// ephemeral in [`crate::server::RunManager::send_event`] and therefore
/// **not** written to either the per-run or per-session event log
/// ([`crate::event_log::SessionEventLogManager`] holds nothing for them). Visible text is
/// flushed to the message store only at end-of-turn via
/// `persist_assistant_tool_calls`. While a turn is in flight there is no
/// durable record of the partial assistant reply, so on a mid-turn
/// session switch the UI's `chatMessages` state is wiped and there is
/// nothing to repopulate it from — the messages GET returns no in-flight
/// assistant entry yet, and the SSE replay cursor sits past every
/// `token_delta` that already fired (and even if it didn't, replay
/// itself doesn't carry token_delta because the events are ephemeral).
///
/// The fix is the in-memory per-run accumulator
/// (`RunTextBuffer`) maintained inside `send_event`:
/// every parent-agent `token_delta` chunk appends to the buffer's `text`
/// field and stamps `last_session_event_id` with the session log HWM at
/// the moment of the append. This endpoint reads that snapshot and
/// returns it verbatim. The HWM is the SSE replay watermark — the
/// client bumps `lastEventId` up to it so subsequent SSE replay does not
/// double-emit any non-ephemeral events that the rehydrated text was
/// implicitly contemporaneous with.
///
/// Per-turn scoping (mirrors the #1077 contract on the reasoning side):
/// parent-agent `tool_start` / `tool_end` events clear the buffer because
/// the visible text emitted before a tool call has by then been sealed
/// onto the closing assistant message of the prior turn and persisted to
/// the message store. Returning that already-sealed text here would
/// produce the same double-render bug the reasoning side hit in #1077 —
/// once on the sealed bubble, once concatenated into a trailing unsealed
/// bubble. Subagent tool events do not clear the buffer (the parent's
/// turn frame is independent of subagent activity); subagent
/// `token_delta` itself is filtered at append time (`source_agent` set
/// is dropped), mirroring the UI's live `token_delta` handler.
///
/// Returns an empty `text` and `last_event_id: null` when the run has
/// no recorded post-boundary visible text yet — the client calls this
/// endpoint unconditionally on every reload that has an active run, so
/// an empty-result case must be well-formed rather than a 404.
///
/// DM sessions are explicitly out of scope: DM visible reply uses a
/// different surface (`dm_message` events and the
/// `groupDmReasoningBlocks` layout); rehydrating into the main chat
/// pane would surface a row that the DM view never renders. The
/// frontend gates the call on `session_type !== 'dm'` so this endpoint
/// itself does not need a DM branch — the backend remains uniform and
/// returns whatever the buffer holds. Note that DM runs do still flow
/// through `forward_runtime_events` → `send_event` → `token_delta`, so
/// the per-run buffer IS populated for DM runs; it is simply
/// populated-but-unread because the frontend never calls this endpoint
/// for DM sessions. The wasted memory is bounded (see "Memory profile"
/// below) and clears on terminal state like any other run. See the PR
/// body for the follow-up issue if/when DM rehydration becomes worth
/// the refactor.
///
/// **Memory profile.** The per-run buffer grows up to roughly the
/// model's `max_tokens` worth of UTF-8 per in-flight turn (cleared on
/// each parent-agent tool boundary, evicted on terminal state). For
/// typical Anthropic 4.x runs with `max_tokens ≈ 8192` that is on the
/// order of ~32 KB; long-form generations with `max_tokens` in the
/// 64K range can reach ~256 KB per active run. Across a fleet of `N`
/// concurrent active runs the total upper bound is roughly
/// `N * max_tokens * bytes_per_token`. No hard cap is enforced
/// today — `max_tokens` is the de-facto ceiling and active-run count
/// already caps fleet memory in practice; a future explicit cap can
/// be layered on if a deployment ever pushes that envelope.
#[instrument(level = "info", skip(state), fields(run_id = %run_id.0))]
pub async fn get_run_text(
    State(state): State<AppState>,
    Path(run_id): Path<RunId>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    // 404 on unknown run — matches the reasoning endpoint contract so the
    // client surfaces "this run does not exist" the same way regardless of
    // which rehydration surface it was probing.
    let _run = state
        .run_manager
        .get_run(run_id)
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "NOT_FOUND", "Run not found"))?;

    let snapshot = state.run_manager.run_text_buffer_snapshot(run_id);
    let (text, last_event_id) = match snapshot {
        Some(buf) => (buf.text, buf.last_session_event_id),
        None => (String::new(), None),
    };

    Ok(Json(serde_json::json!({
        "run_id": run_id.0.to_string(),
        "text": text,
        "last_event_id": last_event_id,
    })))
}

/// GET /runs/{run_id} - Get run status
pub async fn get_run_status(
    State(state): State<AppState>,
    Path(run_id): Path<RunId>,
) -> Result<Json<RunStatusResponse>, (StatusCode, Json<serde_json::Value>)> {
    match state.run_manager.get_run(run_id) {
        Some(run) => {
            let agent_id = run.agent_id;
            let is_queued = matches!(run.status(), alms_core::RunStatus::Queued);
            let mut resp = RunStatusResponse::from(run);
            // Attach tool call count if SQLite is available.
            if let Some(store) = state.session_manager.store() {
                resp.tool_call_count = store.count_tool_calls(run_id).ok();
            }
            // Attach the live 1-indexed queue position so a late-joining
            // client (page reload, polling fallback) can render "Queued —
            // position N" without waiting for the next SSE decrement (#831).
            //
            // Position uses the same FIFO-rank-among-Queued + running-offset
            // formulation as the SSE `run_queue_position` broadcast, so the
            // two surfaces always agree. `None` is returned when the run is
            // running/terminal, or when the run somehow isn't in the queued
            // set (defensive: shouldn't happen for status == Queued but the
            // FIFO sort tolerates it gracefully).
            if is_queued {
                let queued = state.run_manager.list_queued_for_agent(agent_id);
                let running_offset = usize::from(state.run_manager.agent_has_running_run(agent_id));
                if let Some(idx) = queued.iter().position(|r| r.run_id == run_id) {
                    let pos = idx + running_offset;
                    if pos > 0 {
                        resp.queue_position = Some(pos);
                    }
                }
            }
            Ok(Json(resp))
        }
        None => Err(api_error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "Run not found",
        )),
    }
}
