// SPDX-License-Identifier: Apache-2.0

//! Consolidated DM post-run lifecycle handling.
//!
//! When a peer-triggered DM run completes, the **DM completion gate**
//! ([`handle_dm_run_completion`]) guarantees the run exits as exactly one of
//! three outcomes — never silence (#1154):
//!
//! - **Ended**: `ignore_message` was successfully called → write the
//!   `dm_ended` marker, reset depth counters, and emit `ConversationEnded`
//!   triggers + the `dm_conversation_ended` SSE event.
//! - **Delivered**: the run produced deliverable final assistant text → the
//!   text IS the reply (implicit replies, #1154). Deliver it to the peer via
//!   `MessageBus::send`, which persists it to the shared DM session and
//!   triggers the peer's run. No `send_message` call is required.
//! - **Errored**: no deliverable reply and no end signal (or delivery
//!   failed) → end the conversation with an `Errored` reason so the peer is
//!   notified instead of stranded.
//!
//! Run-level failures (cancel / error / pre-loop setup failures) route
//! through [`handle_dm_run_failure`] / [`notify_dm_peer_of_setup_failure`],
//! which produce the **errored** exit for those paths.
//!
//! The `ConversationEnded` trigger handling (depth-exceeded SSE events,
//! web-chat forwarding, notification formatting) remains in `notifications.rs`
//! because it serves the *incoming notification* side, not the post-run side.
//!
//! See #628 and Tim's stability audit (#613) for background; #1154 for the
//! implicit-reply redesign.

use crate::api_error;
use crate::server::AppState;
use crate::sse::SseEventData;
use alms_core::{AgentId, RunId, SessionId, ToolCallRecord, dm_participants};
use alms_tools::message_sender::{ConversationEndReason, MessageSender as _, SendError};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use tracing::{debug, info, warn};

use super::lifecycle::extract_peer_from_dm_context;

/// Context needed to evaluate and execute DM post-run lifecycle actions.
///
/// Constructed from the run parameters available at the completion site in
/// `execute_run()`.
pub(super) struct DmRunCompletionContext<'a> {
    pub state: &'a AppState,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub agent_name: Option<&'a str>,
    pub context_id: &'a str,
    pub is_peer_message: bool,
    pub tool_calls: &'a [ToolCallRecord],
    /// The run's final assistant text (`RunOutput::response`). Under
    /// implicit replies (#1154) this is the candidate message for the peer.
    pub response: &'a str,
    /// The final LLM turn's reasoning trace (`RunOutput::reasoning`). Used
    /// to detect the `[Thinking]`-only promotion fallback
    /// (`response == reasoning`, #1098) so a reasoning trace is never
    /// delivered to the peer as a reply.
    pub reasoning: Option<&'a str>,
}

/// Exit taken by the DM completion gate for a successfully-completed run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DmRunExit {
    /// Not a peer-triggered DM run — the gate does not apply.
    NotPeerDm,
    /// The final assistant text was delivered to the peer (peer triggered).
    Delivered,
    /// The conversation was ended (`ignore_message`, or the delivery hit
    /// the depth ceiling and the bus ended the conversation itself).
    Ended,
    /// The peer was notified that the run failed to produce a reply (or
    /// that delivery / end-signalling failed). Best-effort: covers every
    /// path where neither a delivery nor a clean end could be completed.
    Errored,
}

/// DM completion gate (#1154): single entry point for DM post-run lifecycle
/// handling on the `Ok` arm of `execute_run()`.
///
/// Guarantees a peer-triggered DM run exits as exactly one of
/// **delivered | ended | errored** — see the module docs for the outcome
/// definitions. Returns [`DmRunExit::NotPeerDm`] without side effects for
/// runs the gate does not apply to.
///
/// # Decision order
///
/// 1. `ignore_message` succeeded (per tool-call records) → **ended**.
/// 2. The final text is a deliverable reply
///    ([`alms_core::deliverable_dm_reply`]) → deliver via
///    `MessageBus::send` → **delivered** (or **ended** when the send hits
///    the depth ceiling — the bus has already ended the conversation — or
///    **errored** when delivery fails outright).
/// 3. Otherwise (empty/whitespace final text, promoted reasoning trace,
///    failed `send_message`-only runs, …) → **errored**: the conversation
///    is ended with an `Errored` reason so the peer is notified. This
///    replaces the pre-#1154 silent `DM_TEXT_ONLY_DROPPED` outcome.
pub(super) async fn handle_dm_run_completion(ctx: DmRunCompletionContext<'_>) -> DmRunExit {
    if !(ctx.is_peer_message && ctx.context_id.starts_with("dm:")) {
        return DmRunExit::NotPeerDm;
    }

    // ---- Exit 1: ended (explicit ignore_message) ----
    if should_signal_dm_end(ctx.is_peer_message, ctx.tool_calls, ctx.context_id) {
        return signal_dm_end_on_completion(&ctx).await;
    }

    // ---- Exit 2: delivered (implicit reply) ----
    if let Some(reply) = alms_core::deliverable_dm_reply(ctx.response, ctx.reasoning) {
        return deliver_implicit_reply(&ctx, reply).await;
    }

    // ---- Exit 3: errored (no reply, no end signal) ----
    //
    // The runtime's bounded empty-reply nudge already gave the agent a
    // second chance; ending with an error here is what keeps the peer from
    // waiting forever (replaces the silent `DM_TEXT_ONLY_DROPPED` drop).
    warn!(
        run_id = %ctx.run_id.0,
        context_id = %ctx.context_id,
        "DM run completed without a deliverable reply or end signal — \
         ending conversation with an error so the peer is notified (#1154)"
    );
    if let Err(e) = handle_dm_run_failure(
        ctx.state,
        &ctx.run_id,
        &ctx.session_id,
        ctx.agent_id,
        ctx.agent_name,
        ctx.context_id,
        ctx.is_peer_message,
        // #1258: `interrupted: false` — the run *completed*, it just had
        // nothing deliverable on its LAST turn. Earlier turns of this DM may
        // have been delivered and exist only in the DM session, so the
        // notification run (and the #429 transcript it carries) is still the
        // only thing that brings them to the operator's chat.
        ConversationEndReason::Errored {
            message: "agent run completed without producing a reply".to_string(),
            interrupted: false,
        },
    )
    .await
    {
        warn!(
            error = %e,
            "Failed to signal no-reply conversation end — DM peer state may be stale"
        );
    }
    DmRunExit::Errored
}

/// Exit-1 helper: signal the `ignore_message`-driven conversation end.
///
/// Carries the pre-#1154 `handle_dm_run_completion` body: resolve the peer,
/// call `end_conversation(Ignored)` on the bus, and emit the
/// `dm_conversation_ended` SSE event on the DM session stream.
///
/// The sender's web-chat SSE marker is NOT emitted here -- it is handled by
/// the sender's self-notification run in `run_trigger_loop` (notifications.rs),
/// which calls `notify_dm_ended_to_webchat` for every `ConversationEnded`
/// trigger recipient. Emitting it here as well would cause duplicate markers.
/// See #556.
async fn signal_dm_end_on_completion(ctx: &DmRunCompletionContext<'_>) -> DmRunExit {
    let end_reason = ConversationEndReason::Ignored;
    let reason_label = "ignore_message";

    let Some((agent_name, peer_name, peer_id)) = resolve_dm_pair(ctx, reason_label) else {
        // Resolution failure means we cannot reach the bus at all — the
        // peer is not notified. Surface it as the errored exit so callers
        // and logs do not mistake it for a clean end.
        return DmRunExit::Errored;
    };

    info!(
        agent = %agent_name,
        peer = %peer_name,
        reason = reason_label,
        "DM run ended with {reason_label} — signalling conversation end"
    );

    match ctx
        .state
        .message_bus
        .end_conversation(
            agent_name,
            ctx.agent_id,
            &peer_name,
            peer_id,
            end_reason.clone(),
        )
        .await
    {
        Ok(()) => {
            // Emit dm_conversation_ended SSE event on the DM session stream
            // so the web UI can show a "conversation ended" indicator.
            // Phase 6 of #384.
            //
            // NOTE: This code path fires for the ignore_message reason. The
            // depth-exceeded reason emits this event from `run_trigger_loop`
            // when processing the `ConversationEnded` trigger (#419).
            //
            // NOTE: If both agents ignore simultaneously, end_conversation
            // returns Ok(()) for both callers (the second sees "already ended
            // by peer" and returns Ok). This means duplicate
            // dm_conversation_ended SSE events may be emitted for the same
            // session. The frontend should be prepared to handle duplicates.
            ctx.state
                .run_manager
                .send_session_event(
                    ctx.session_id,
                    ctx.run_id,
                    SseEventData::dm_conversation_ended(
                        ctx.session_id,
                        agent_name,
                        &peer_name,
                        &end_reason.to_string(),
                        ctx.context_id,
                    ),
                )
                .await;

            // NOTE: The sender's web-chat SSE marker is handled by the
            // sender's self-notification run in `run_trigger_loop`
            // (notifications.rs), which calls `notify_dm_ended_to_webchat`
            // for every ConversationEnded trigger recipient. Calling it here
            // as well would cause a duplicate marker for the sender. See #556.

            DmRunExit::Ended
        }
        Err(e) => {
            warn!(
                agent = %agent_name,
                peer = %peer_name,
                error = %e,
                "Failed to signal conversation end"
            );
            DmRunExit::Errored
        }
    }
}

/// Exit-2 helper: deliver the run's final assistant text to the DM peer.
///
/// `MessageBus::send` persists the reply to the shared DM session (with
/// `from_agent` / `message_type: "dm"` metadata — the same shape the
/// `send_message` tool produced pre-#1154, so DM perspective mapping
/// applies unchanged) and triggers the peer's run.
async fn deliver_implicit_reply(ctx: &DmRunCompletionContext<'_>, reply: &str) -> DmRunExit {
    let Some((agent_name, peer_name, peer_id)) = resolve_dm_pair(ctx, "implicit_reply") else {
        return DmRunExit::Errored;
    };

    match ctx
        .state
        .message_bus
        .send(
            agent_name,
            ctx.agent_id,
            &peer_name,
            peer_id,
            reply,
            // The "source session" of an implicit reply is the DM session
            // itself, which `MessageBus::send` correctly rejects as a
            // source (a DM session cannot be its own source, #656) — so
            // the peer's original source-session entry is preserved.
            Some(ctx.session_id),
        )
        .await
    {
        Ok(receipt) => {
            info!(
                agent = %agent_name,
                peer = %peer_name,
                session_id = %receipt.session_id.0,
                run_id = %ctx.run_id.0,
                reply_len = reply.len(),
                "DM completion gate: final assistant text delivered to peer (#1154)"
            );
            DmRunExit::Delivered
        }
        Err(SendError::DepthExceeded) => {
            // The bus has already ended the conversation (marker write,
            // depth reset, ConversationEnded triggers) as part of its
            // depth-ceiling handling — this is a clean end, not an error.
            info!(
                agent = %agent_name,
                peer = %peer_name,
                "DM reply hit the depth ceiling — bus ended the conversation"
            );
            DmRunExit::Ended
        }
        Err(e) => {
            warn!(
                agent = %agent_name,
                peer = %peer_name,
                error = %e,
                "DM reply delivery failed — ending conversation with an error"
            );
            if let Err(end_err) = handle_dm_run_failure(
                ctx.state,
                &ctx.run_id,
                &ctx.session_id,
                ctx.agent_id,
                ctx.agent_name,
                ctx.context_id,
                ctx.is_peer_message,
                // #1258: `interrupted: false` — same shape as Exit 3. The run
                // completed and produced a reply; only the last delivery hop
                // failed, so the exchange up to that point is real and still
                // needs relaying.
                //
                // The `SendError` tail is sanitised (#911 / #930 / #931): its
                // `Internal` variant wraps storage errors that can carry
                // paths, and since #1258 this string reaches the browser (the
                // `dm_conversation_ended` `detail` field) and the persisted
                // marker text, not just the peer's run input.
                ConversationEndReason::Errored {
                    message: super::lifecycle::peer_error_with_prefix(
                        "reply delivery failed: ",
                        &e.to_string(),
                    ),
                    interrupted: false,
                },
            )
            .await
            {
                warn!(
                    error = %end_err,
                    "Failed to signal delivery-failure conversation end — \
                     DM peer state may be stale"
                );
            }
            DmRunExit::Errored
        }
    }
}

/// Resolve `(agent_name, peer_name, peer_id)` for a peer-triggered DM run.
///
/// Logs and returns `None` when the agent name is unset, the peer cannot be
/// extracted from the context ID, or the peer is not in the registry.
fn resolve_dm_pair<'a>(
    ctx: &'a DmRunCompletionContext<'_>,
    reason_label: &str,
) -> Option<(&'a str, String, AgentId)> {
    let Some(agent_name) = ctx.agent_name else {
        debug!(
            reason = reason_label,
            "DM outcome detected but agent name is not set — skipping"
        );
        return None;
    };

    let Some(peer_name) = extract_peer_from_dm_context(ctx.context_id, agent_name) else {
        debug!(
            context_id = %ctx.context_id,
            reason = reason_label,
            "DM outcome detected but could not extract peer from context_id"
        );
        return None;
    };

    let peer_agent_id = ctx
        .state
        .session_manager
        .store()
        .and_then(|store| store.load_agent_by_name(&peer_name).ok())
        .flatten()
        .map(|record| record.id);

    let Some(peer_id) = peer_agent_id else {
        warn!(
            peer = %peer_name,
            reason = reason_label,
            "Cannot complete DM outcome — peer agent not found in registry"
        );
        return None;
    };

    Some((agent_name, peer_name, peer_id))
}

/// Notify the DM peer when a peer-triggered DM run fails BEFORE the agent
/// loop starts (#1154 / B4).
///
/// `execute_run()` has three pre-loop failure arms — `resolve_agent_config`
/// error, token-budget rejection, and `AgentRuntime::new` failure — that
/// historically returned early without any DM bookkeeping, stranding the
/// peer until the depth-expiry sweep. This helper mirrors the post-loop
/// failure arms by routing into [`handle_dm_run_failure`] with an `Errored`
/// reason.
///
/// `agent_name` may be `None` on the earliest arm (`resolve_agent_config`
/// failed before the registry record was loaded); in that case the name is
/// re-resolved from the registry by `agent_id` so the peer extraction in
/// `handle_dm_run_failure` can still succeed.
#[allow(clippy::too_many_arguments)]
pub(super) async fn notify_dm_peer_of_setup_failure(
    state: &AppState,
    run_id: &RunId,
    session_id: &SessionId,
    agent_id: AgentId,
    agent_name: Option<&str>,
    context_id: &str,
    is_peer_message: bool,
    error_message: String,
) {
    notify_dm_peer_of_setup_outcome(
        state,
        run_id,
        session_id,
        agent_id,
        agent_name,
        context_id,
        is_peer_message,
        // #1258: `interrupted: true` — the run never started its loop, so no
        // turn of this DM ever completed on this side.
        ConversationEndReason::Errored {
            message: error_message,
            interrupted: true,
        },
    )
    .await;
}

/// Notify the DM peer when a peer-triggered DM run is **cancelled before the
/// agent loop starts** (#1154 / S1).
///
/// A run cancelled while still `Queued` — HTTP `POST /runs/{id}/cancel`, the
/// #1109 approval-deny cascade, `cancel_runs_for_session`, or graceful
/// shutdown — reaches `execute_run`'s pre-loop early-exit and returns there
/// without any DM bookkeeping. Neither the synchronous `cancel_run` handler
/// (it only flips state + emits `run_cancelled`) nor the early-exit signalled
/// the peer, so the peer was stranded on "Chatting with…" until the
/// `DEPTH_EXPIRY_SECS` sweep. This helper routes the early-exit into
/// [`handle_dm_run_failure`] with [`ConversationEndReason::UserCancelled`] so
/// the peer is told the conversation ended.
///
/// Like [`notify_dm_peer_of_setup_failure`], `agent_name` may be `None` (the
/// early-exit fires before the registry record is loaded); the name is
/// re-resolved from the registry by `agent_id`.
pub(super) async fn notify_dm_peer_of_setup_cancellation(
    state: &AppState,
    run_id: &RunId,
    session_id: &SessionId,
    agent_id: AgentId,
    agent_name: Option<&str>,
    context_id: &str,
    is_peer_message: bool,
) {
    notify_dm_peer_of_setup_outcome(
        state,
        run_id,
        session_id,
        agent_id,
        agent_name,
        context_id,
        is_peer_message,
        ConversationEndReason::UserCancelled,
    )
    .await;
}

/// Shared body for the pre-loop DM peer-notification helpers
/// ([`notify_dm_peer_of_setup_failure`] /
/// [`notify_dm_peer_of_setup_cancellation`]). Gates on peer-DM, re-resolves
/// the agent name by ID when the caller did not have it yet, and routes the
/// supplied `reason` into [`handle_dm_run_failure`].
#[allow(clippy::too_many_arguments)]
async fn notify_dm_peer_of_setup_outcome(
    state: &AppState,
    run_id: &RunId,
    session_id: &SessionId,
    agent_id: AgentId,
    agent_name: Option<&str>,
    context_id: &str,
    is_peer_message: bool,
    reason: ConversationEndReason,
) {
    if !(is_peer_message && context_id.starts_with("dm:")) {
        return;
    }

    // Re-resolve the agent name by ID when the caller did not have it yet.
    let resolved_name: Option<String> = match agent_name {
        Some(name) => Some(name.to_string()),
        None => state
            .session_manager
            .store()
            .and_then(|store| store.load_agent_by_id(agent_id).ok())
            .flatten()
            .map(|record| record.name),
    };

    if let Err(e) = handle_dm_run_failure(
        state,
        run_id,
        session_id,
        agent_id,
        resolved_name.as_deref(),
        context_id,
        is_peer_message,
        reason,
    )
    .await
    {
        warn!(
            run_id = %run_id.0,
            error = %e,
            "Pre-loop DM peer notification failed — peer state may be stale"
        );
    }
}

/// Signal conversation end when a peer-triggered DM run failed or was
/// cancelled mid-flight.
///
/// Mirrors [`handle_dm_run_completion`] for the four error arms in
/// `execute_run()`:
///
/// - `AlmsError::Cancelled`
/// - `AlmsError::CancelledWithToolCalls`
/// - `AlmsError::FailedWithToolCalls`
/// - generic `Err(_)`
///
/// Without this, a peer-triggered DM run that fails for any reason (LLM 5xx,
/// rate-limit, tool panic, posture trip, `POST /runs/{id}/cancel` mid-flight)
/// leaves the depth counter unreset, no `dm_ended` marker, and no
/// `ConversationEnded` peer notification — the peer thinks the DM is still
/// open until the 1800s `DEPTH_EXPIRY_SECS` sweep eventually clears it.
///
/// # End condition
///
/// A conversation end is signalled when the run is a peer-triggered DM
/// (`is_peer_message` and `context_id` starts with `"dm:"`). Tool call
/// records are NOT consulted here — we end the conversation purely on the
/// run-level outcome, regardless of whether `ignore_message` was called.
///
/// # Reason mapping
///
/// Callers pass the reason directly:
///
/// - cancel arms (`Cancelled`, `CancelledWithToolCalls`) →
///   [`ConversationEndReason::UserCancelled`]
/// - failure arms (`FailedWithToolCalls`, generic `Err(_)`) →
///   [`ConversationEndReason::Errored`] with the truncated `AlmsError`
///   `Display` string
///
/// # Atomicity
///
/// `MessageBus::end_conversation()` uses `depths.remove()` as an atomicity
/// guard (see `bus.rs:359`), so if the peer also independently calls
/// `end_conversation` (e.g. their own happy-path `ignore_message`), only the
/// first caller writes the `dm_ended` marker and emits the
/// `ConversationEnded` trigger. Best-effort: a returned `Err` is logged but
/// does not abort the surrounding error-arm bookkeeping.
///
/// Returns `Ok(())` on success or skip; returns `Err` only when the
/// underlying `MessageBus::end_conversation` fails (the caller logs and
/// continues).
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_dm_run_failure(
    state: &AppState,
    run_id: &RunId,
    session_id: &SessionId,
    agent_id: AgentId,
    agent_name: Option<&str>,
    context_id: &str,
    is_peer_message: bool,
    reason: ConversationEndReason,
) -> Result<(), alms_core::AlmsError> {
    // Same gating as the happy path: only DM peer-triggered runs.
    if !(is_peer_message && context_id.starts_with("dm:")) {
        return Ok(());
    }

    let reason_label = match &reason {
        ConversationEndReason::Ignored => "ignore_message",
        ConversationEndReason::DepthExceeded => "depth_exceeded",
        ConversationEndReason::UserCancelled => "user_cancelled",
        ConversationEndReason::Errored { .. } => "errored",
    };

    let Some(agent_name) = agent_name else {
        debug!(
            reason = reason_label,
            "DM run failure detected but agent name is not set — skipping conversation end"
        );
        return Ok(());
    };

    let Some(peer_name) = extract_peer_from_dm_context(context_id, agent_name) else {
        debug!(
            context_id = %context_id,
            reason = reason_label,
            "DM run failure detected but could not extract peer from context_id"
        );
        return Ok(());
    };

    // Resolve the peer's AgentId from the agent registry.
    let peer_agent_id = state
        .session_manager
        .store()
        .and_then(|store| store.load_agent_by_name(&peer_name).ok())
        .flatten()
        .map(|record| record.id);

    let Some(peer_id) = peer_agent_id else {
        warn!(
            peer = %peer_name,
            "Cannot signal conversation end on failure — peer agent not found in registry"
        );
        return Ok(());
    };

    info!(
        agent = %agent_name,
        peer = %peer_name,
        reason = reason_label,
        run_id = %run_id.0,
        "DM run failed/cancelled — signalling conversation end ({reason_label})"
    );

    state
        .message_bus
        .end_conversation(agent_name, agent_id, &peer_name, peer_id, reason.clone())
        .await
        .map_err(|e| alms_core::AlmsError::Runtime(e.to_string()))?;

    // Emit dm_conversation_ended SSE on the DM session stream so the web UI
    // shows the "conversation ended" indicator regardless of run outcome.
    //
    // Atomicity: `MessageBus::end_conversation()` returns `Ok(())` even when
    // the depth-counter remove was a no-op (already ended by peer). That
    // means the peer-side helper may race in too. Frontend already tolerates
    // duplicate `dm_conversation_ended` events — see the comment in
    // `handle_dm_run_completion`.
    state
        .run_manager
        .send_session_event(
            *session_id,
            *run_id,
            SseEventData::dm_conversation_ended(
                *session_id,
                agent_name,
                &peer_name,
                &reason.to_string(),
                context_id,
            ),
        )
        .await;

    Ok(())
}

/// Evaluate the three-way condition for ignore_message detection.
///
/// Returns `true` when all conditions are met:
/// 1. The run was triggered by a peer message (`is_peer_message`)
/// 2. The context ID is a DM session (`dm:` prefix)
/// 3. `ignore_message` was successfully called (verified via tool call records)
///
/// This is the same condition previously inlined in `execute_run()` and
/// duplicated as `should_signal_ignore()` in the test helpers. Now it lives
/// here as the single source of truth.
pub(super) fn should_signal_dm_end(
    is_peer_message: bool,
    tool_calls: &[ToolCallRecord],
    context_id: &str,
) -> bool {
    is_peer_message
        && alms_core::ran_ignore_message_successfully(tool_calls)
        && context_id.starts_with("dm:")
}

/// POST /sessions/{session_id}/cancel-dm — cancel an active DM conversation.
///
/// This endpoint gives the user direct control over ending a DM conversation
/// between two agents. It performs these steps:
///
/// 1. Validates the session exists and is a DM session (`dm:` prefix)
/// 2. Resolves both participant agent IDs from the registry
/// 3. Cancels any active (queued/running) runs on the DM session
/// 4. Calls `end_conversation` on the `MessageBus` with `UserCancelled` reason
/// 5. Emits `dm_conversation_ended` SSE event so the frontend knows the DM ended
///
/// Returns 200 with cancellation details, or an error if the session is not a DM
/// or agents cannot be resolved.
///
/// See issue #705.
#[tracing::instrument(level = "info", skip(state), fields(session_id = %session_id.0))]
pub async fn cancel_dm(
    State(state): State<AppState>,
    Path(session_id): Path<SessionId>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    // 1. Look up the session.
    let session = state
        .session_manager
        .get(session_id)
        .map_err(|_| api_error(StatusCode::NOT_FOUND, "NOT_FOUND", "Session not found"))?;

    // 2. Verify it is a DM session.
    let (name_a, name_b) = dm_participants(&session.context_id).ok_or_else(|| {
        api_error(
            StatusCode::BAD_REQUEST,
            "NOT_DM_SESSION",
            "Session is not a DM conversation",
        )
    })?;

    // 3. Resolve both agent IDs from the registry.
    let store = state.session_manager.store().ok_or_else(|| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "NO_STORE",
            "No agent registry available",
        )
    })?;

    let agent_a = store
        .load_agent_by_name(name_a)
        .map_err(|e| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "REGISTRY_ERROR",
                format!("Failed to look up agent '{name_a}': {e}"),
            )
        })?
        .ok_or_else(|| {
            api_error(
                StatusCode::NOT_FOUND,
                "AGENT_NOT_FOUND",
                format!("Agent '{name_a}' not found in registry"),
            )
        })?;

    let agent_b = store
        .load_agent_by_name(name_b)
        .map_err(|e| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "REGISTRY_ERROR",
                format!("Failed to look up agent '{name_b}': {e}"),
            )
        })?
        .ok_or_else(|| {
            api_error(
                StatusCode::NOT_FOUND,
                "AGENT_NOT_FOUND",
                format!("Agent '{name_b}' not found in registry"),
            )
        })?;

    // 4. Cancel any active runs on this DM session.
    let runs_cancelled = state.run_manager.cancel_runs_for_session(session_id);

    info!(
        runs_cancelled = runs_cancelled,
        agent_a = %name_a,
        agent_b = %name_b,
        "Cancelling DM conversation by user request"
    );

    // 5. Signal conversation end via the MessageBus.
    //
    // Use agent_a as the "sender" — the direction does not matter for
    // UserCancelled since the user (not an agent) initiated the cancellation.
    // Both agents will receive notifications.
    let end_result = state
        .message_bus
        .end_conversation(
            name_a,
            agent_a.id,
            name_b,
            agent_b.id,
            ConversationEndReason::UserCancelled,
        )
        .await;

    if let Err(ref e) = end_result {
        warn!(
            error = %e,
            "end_conversation returned error during user cancellation — \
             continuing (runs already cancelled)"
        );
    }

    // 6. Emit dm_conversation_ended SSE event on the DM session stream.
    //
    // Use a synthetic RunId since this cancellation is not associated with
    // any particular run — it is a user-initiated action.
    let synthetic_run_id = RunId::new();
    state
        .run_manager
        .send_session_event(
            session_id,
            synthetic_run_id,
            SseEventData::dm_conversation_ended(
                session_id,
                "user",
                &format!("{name_a}, {name_b}"),
                &ConversationEndReason::UserCancelled.to_string(),
                &session.context_id,
            ),
        )
        .await;

    Ok(Json(serde_json::json!({
        "ok": true,
        "session_id": session_id.0.to_string(),
        "context_id": session.context_id,
        "participants": [name_a, name_b],
        "runs_cancelled": runs_cancelled,
        "reason": "user_cancelled",
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alms_core::ToolCallRole;

    /// Build a minimal `ToolCallRecord` for testing.
    fn make_record(
        role: ToolCallRole,
        tool_name: &str,
        tool_id: &str,
        result: Option<&str>,
    ) -> ToolCallRecord {
        ToolCallRecord {
            seq: 0,
            role,
            tool_name: Some(tool_name.to_string()),
            tool_id: Some(tool_id.to_string()),
            tool_invocation_id: None,
            params: None,
            result: result.map(String::from),
            timestamp: chrono::Utc::now(),
            from_agent: None,
        }
    }

    #[test]
    fn ignore_in_dm_context_signals_end() {
        let records = vec![
            make_record(ToolCallRole::Assistant, "ignore_message", "c1", None),
            make_record(
                ToolCallRole::Tool,
                "ignore_message",
                "c1",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(should_signal_dm_end(true, &records, "dm:alice:bob"));
    }

    #[test]
    fn ignore_in_non_dm_context_does_not_signal() {
        let records = vec![
            make_record(ToolCallRole::Assistant, "ignore_message", "c1", None),
            make_record(
                ToolCallRole::Tool,
                "ignore_message",
                "c1",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(!should_signal_dm_end(true, &records, "session:abc123"));
    }

    #[test]
    fn send_message_does_not_signal() {
        let records = vec![
            make_record(ToolCallRole::Assistant, "send_message", "c1", None),
            make_record(
                ToolCallRole::Tool,
                "send_message",
                "c1",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(!should_signal_dm_end(true, &records, "dm:alice:bob"));
    }

    #[test]
    fn non_peer_message_does_not_signal() {
        let records = vec![
            make_record(ToolCallRole::Assistant, "ignore_message", "c1", None),
            make_record(
                ToolCallRole::Tool,
                "ignore_message",
                "c1",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(!should_signal_dm_end(false, &records, "dm:alice:bob"));
    }

    #[test]
    fn empty_tool_calls_does_not_signal() {
        assert!(!should_signal_dm_end(true, &[], "dm:alice:bob"));
    }

    #[test]
    fn notification_context_does_not_signal() {
        let records = vec![
            make_record(ToolCallRole::Assistant, "ignore_message", "c1", None),
            make_record(
                ToolCallRole::Tool,
                "ignore_message",
                "c1",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(!should_signal_dm_end(false, &records, "notifications:bob"));
    }

    #[test]
    fn job_context_does_not_signal() {
        let records = vec![
            make_record(ToolCallRole::Assistant, "ignore_message", "c1", None),
            make_record(
                ToolCallRole::Tool,
                "ignore_message",
                "c1",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(!should_signal_dm_end(false, &records, "job_some-uuid"));
    }

    #[test]
    fn tool_role_result_only_does_not_signal() {
        let records = vec![make_record(
            ToolCallRole::Tool,
            "ignore_message",
            "c1",
            Some(r#"{"ok":true}"#),
        )];
        assert!(!should_signal_dm_end(true, &records, "dm:alice:bob"));
    }

    #[test]
    fn conflict_batch_then_send_does_not_signal() {
        let conflict_error = format!("Error: {}", alms_core::DM_CONFLICT_MSG);
        let records = vec![
            make_record(ToolCallRole::Assistant, "send_message", "s1", None),
            make_record(ToolCallRole::Assistant, "ignore_message", "i1", None),
            make_record(
                ToolCallRole::Tool,
                "send_message",
                "s1",
                Some(&conflict_error),
            ),
            make_record(
                ToolCallRole::Tool,
                "ignore_message",
                "i1",
                Some(&conflict_error),
            ),
            make_record(ToolCallRole::Assistant, "send_message", "s2", None),
            make_record(
                ToolCallRole::Tool,
                "send_message",
                "s2",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(!should_signal_dm_end(true, &records, "dm:alice:bob"));
    }

    #[test]
    fn conflict_batch_then_clean_ignore_signals() {
        let conflict_error = format!("Error: {}", alms_core::DM_CONFLICT_MSG);
        let records = vec![
            make_record(ToolCallRole::Assistant, "send_message", "s1", None),
            make_record(ToolCallRole::Assistant, "ignore_message", "i1", None),
            make_record(
                ToolCallRole::Tool,
                "send_message",
                "s1",
                Some(&conflict_error),
            ),
            make_record(
                ToolCallRole::Tool,
                "ignore_message",
                "i1",
                Some(&conflict_error),
            ),
            make_record(ToolCallRole::Assistant, "ignore_message", "i2", None),
            make_record(
                ToolCallRole::Tool,
                "ignore_message",
                "i2",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(should_signal_dm_end(true, &records, "dm:alice:bob"));
    }

    #[test]
    fn ignore_without_tool_result_does_not_signal() {
        let records = vec![make_record(
            ToolCallRole::Assistant,
            "ignore_message",
            "c1",
            None,
        )];
        assert!(!should_signal_dm_end(true, &records, "dm:alice:bob"));
    }
}
