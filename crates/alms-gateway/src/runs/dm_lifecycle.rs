//! Consolidated DM post-run lifecycle handling.
//!
//! When a peer-triggered DM run completes, several steps must happen:
//!
//! 1. Detect whether `ignore_message` was called or the agent hit its
//!    max-iterations limit without replying
//! 2. Resolve the peer agent from the `dm:` context ID
//! 3. Call `end_conversation` on the `MessageBus` to reset depth counters
//!    and emit `ConversationEnded` triggers to both agents
//! 4. Emit a `dm_conversation_ended` SSE event on the DM session stream
//!
//! Previously this logic was inlined in `execute_run()` (lifecycle.rs lines
//! 1085-1180). This module consolidates it into a single entry point so there
//! is exactly one code path for ignore-message-driven and
//! max-iterations-driven conversation endings.
//!
//! The `ConversationEnded` trigger handling (depth-exceeded SSE events,
//! web-chat forwarding, notification formatting) remains in `notifications.rs`
//! because it serves the *incoming notification* side, not the post-run side.
//!
//! See #628 and Tim's stability audit (#613) for background.

use crate::api_error;
use crate::server::AppState;
use crate::sse::SseEventData;
use alms_core::{AgentId, RunId, SessionId, ToolCallRecord, dm_participants};
use alms_tools::message_sender::{ConversationEndReason, MessageSender as _};
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
    /// Whether the run hit the agent loop's max-iterations limit.
    /// Set to `true` when `output.response == MAX_ITERATIONS_SENTINEL`.
    pub hit_max_iterations: bool,
}

/// Single entry point for DM post-run lifecycle handling.
///
/// Called from `execute_run()` after a run completes successfully. Evaluates
/// whether the run was a peer-triggered DM that should signal conversation
/// end, and if so, signals it to the `MessageBus` and emits the appropriate
/// SSE event.
///
/// Returns `true` if a conversation end was signalled, `false` otherwise
/// (including when conditions are not met or an error occurs).
///
/// # End conditions (checked in order)
///
/// A conversation end is signalled when the run is a peer-triggered DM
/// (`is_peer_message` and `context_id` starts with `"dm:"`) AND one of:
///
/// 1. `ignore_message` was successfully called during the run (verified via
///    tool call records) -- reason: `Ignored`
/// 2. The agent loop hit its max-iterations limit (`MAX_ITERATIONS_SENTINEL`
///    response) -- reason: `MaxIterations`
///
/// # Actions (in order)
///
/// 1. Extract the peer agent name from the `dm:` context ID
/// 2. Resolve the peer's `AgentId` from the agent registry
/// 3. Call `end_conversation()` on the `MessageBus` with the determined reason
/// 4. Emit `dm_conversation_ended` SSE event on the DM session stream
///
/// The sender's web-chat SSE marker is NOT emitted here -- it is handled by
/// the sender's self-notification run in `run_trigger_loop` (notifications.rs),
/// which calls `notify_dm_ended_to_webchat` for every `ConversationEnded`
/// trigger recipient. Emitting it here as well would cause duplicate markers.
/// See #556.
pub(super) async fn handle_dm_run_completion(ctx: DmRunCompletionContext<'_>) -> bool {
    // Determine the end reason, if any. Check ignore_message first, then
    // max-iterations as a fallback.
    let end_reason = if should_signal_dm_end(ctx.is_peer_message, ctx.tool_calls, ctx.context_id) {
        ConversationEndReason::Ignored
    } else if should_signal_dm_end_max_iterations(
        ctx.is_peer_message,
        ctx.context_id,
        ctx.hit_max_iterations,
    ) {
        ConversationEndReason::MaxIterations
    } else {
        return false;
    };

    let reason_label = match end_reason {
        ConversationEndReason::Ignored => "ignore_message",
        ConversationEndReason::MaxIterations => "max_iterations",
        ConversationEndReason::DepthExceeded => "depth_exceeded",
        ConversationEndReason::UserCancelled => "user_cancelled",
    };

    let Some(agent_name) = ctx.agent_name else {
        debug!(
            reason = reason_label,
            "DM end detected but agent name is not set — skipping conversation end"
        );
        return false;
    };

    let Some(peer_name) = extract_peer_from_dm_context(ctx.context_id, agent_name) else {
        debug!(
            context_id = %ctx.context_id,
            reason = reason_label,
            "DM end detected but could not extract peer from context_id"
        );
        return false;
    };

    // Resolve the peer's AgentId from the agent registry.
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
            "Cannot signal conversation end — peer agent not found in registry"
        );
        return false;
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
        .end_conversation(agent_name, ctx.agent_id, &peer_name, peer_id, end_reason)
        .await
    {
        Ok(()) => {
            // Emit dm_conversation_ended SSE event on the DM session stream
            // so the web UI can show a "conversation ended" indicator.
            // Phase 6 of #384.
            //
            // NOTE: This code path fires for both ignore_message and
            // max_iterations reasons. The depth-exceeded reason emits this
            // event from `run_trigger_loop` when processing the
            // `ConversationEnded` trigger (#419).
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

            true
        }
        Err(e) => {
            warn!(
                agent = %agent_name,
                peer = %peer_name,
                error = %e,
                "Failed to signal conversation end"
            );
            false
        }
    }
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

/// Detect whether a peer-triggered DM run ended because the agent loop hit
/// its max-iterations limit without calling `send_message` or `ignore_message`.
///
/// Returns `true` when all conditions are met:
/// 1. The run was triggered by a peer message (`is_peer_message`)
/// 2. The context ID is a DM session (`dm:` prefix)
/// 3. The run hit the max-iterations limit (`hit_max_iterations`)
///
/// When this returns `true`, the caller should signal conversation end with
/// reason `MaxIterations` so the peer receives the `dm_ended` notification.
pub(super) fn should_signal_dm_end_max_iterations(
    is_peer_message: bool,
    context_id: &str,
    hit_max_iterations: bool,
) -> bool {
    is_peer_message && context_id.starts_with("dm:") && hit_max_iterations
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
            params: None,
            result: result.map(String::from),
            timestamp: chrono::Utc::now(),
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

    // -- Tests for should_signal_dm_end_max_iterations --

    #[test]
    fn max_iterations_in_dm_peer_signals_end() {
        assert!(should_signal_dm_end_max_iterations(
            true,
            "dm:alice:bob",
            true,
        ));
    }

    #[test]
    fn max_iterations_non_peer_does_not_signal() {
        assert!(!should_signal_dm_end_max_iterations(
            false,
            "dm:alice:bob",
            true,
        ));
    }

    #[test]
    fn max_iterations_non_dm_context_does_not_signal() {
        assert!(!should_signal_dm_end_max_iterations(
            true,
            "session:abc123",
            true,
        ));
    }

    #[test]
    fn no_max_iterations_does_not_signal() {
        assert!(!should_signal_dm_end_max_iterations(
            true,
            "dm:alice:bob",
            false,
        ));
    }

    #[test]
    fn max_iterations_in_notification_context_does_not_signal() {
        assert!(!should_signal_dm_end_max_iterations(
            true,
            "notifications:bob",
            true,
        ));
    }

    #[test]
    fn max_iterations_in_job_context_does_not_signal() {
        assert!(!should_signal_dm_end_max_iterations(
            true,
            "job_some-uuid",
            true,
        ));
    }
}
