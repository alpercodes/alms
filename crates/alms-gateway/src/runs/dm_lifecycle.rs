//! Consolidated DM post-run lifecycle handling.
//!
//! When a peer-triggered DM run completes, several steps must happen:
//!
//! 1. Detect whether `ignore_message` was successfully called
//! 2. Resolve the peer agent from the `dm:` context ID
//! 3. Call `end_conversation` on the `MessageBus` to reset depth counters
//!    and emit `ConversationEnded` triggers to both agents
//! 4. Emit a `dm_conversation_ended` SSE event on the DM session stream
//!
//! Previously this logic was inlined in `execute_run()` (lifecycle.rs lines
//! 1085-1180). This module consolidates it into a single entry point so there
//! is exactly one code path for ignore-message-driven conversation endings.
//!
//! The `ConversationEnded` trigger handling (depth-exceeded SSE events,
//! web-chat forwarding, notification formatting) remains in `notifications.rs`
//! because it serves the *incoming notification* side, not the post-run side.
//!
//! See #628 and Tim's stability audit (#613) for background.

use crate::server::AppState;
use crate::sse::SseEventData;
use alms_core::{AgentId, RunId, SessionId, ToolCallRecord};
use alms_tools::message_sender::{ConversationEndReason, MessageSender as _};
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
}

/// Single entry point for DM post-run lifecycle handling.
///
/// Called from `execute_run()` after a run completes successfully. Evaluates
/// whether the run was a peer-triggered DM that ended with `ignore_message`,
/// and if so, signals conversation end to the `MessageBus` and emits the
/// appropriate SSE event.
///
/// Returns `true` if a conversation end was signalled, `false` otherwise
/// (including when conditions are not met or an error occurs).
///
/// # Conditions (all must be true)
///
/// 1. `is_peer_message` is true (the run was triggered by a peer DM)
/// 2. `context_id` starts with `"dm:"` (it is a DM session)
/// 3. `ignore_message` was successfully called during the run (verified via
///    tool call records, requiring a matching non-error `Tool`-role result)
///
/// # Actions (in order)
///
/// 1. Extract the peer agent name from the `dm:` context ID
/// 2. Resolve the peer's `AgentId` from the agent registry
/// 3. Call `end_conversation()` on the `MessageBus` with reason `Ignored`
/// 4. Emit `dm_conversation_ended` SSE event on the DM session stream
///
/// The sender's web-chat SSE marker is NOT emitted here -- it is handled by
/// the sender's self-notification run in `run_trigger_loop` (notifications.rs),
/// which calls `notify_dm_ended_to_webchat` for every `ConversationEnded`
/// trigger recipient. Emitting it here as well would cause duplicate markers.
/// See #556.
pub(super) async fn handle_dm_run_completion(ctx: DmRunCompletionContext<'_>) -> bool {
    // Gate: only peer-triggered DM runs with a successful ignore_message call.
    if !should_signal_dm_end(ctx.is_peer_message, ctx.tool_calls, ctx.context_id) {
        return false;
    }

    let Some(agent_name) = ctx.agent_name else {
        debug!("DM ignore_message detected but agent name is not set — skipping conversation end");
        return false;
    };

    let Some(peer_name) = extract_peer_from_dm_context(ctx.context_id, agent_name) else {
        debug!(
            context_id = %ctx.context_id,
            "DM ignore_message detected but could not extract peer from context_id"
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
        "DM run ended with ignore_message — signalling conversation end"
    );

    let end_reason = ConversationEndReason::Ignored;

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
            // NOTE: This code path only fires for the ignore_message reason.
            // The depth-exceeded reason emits this event from `run_trigger_loop`
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
}
