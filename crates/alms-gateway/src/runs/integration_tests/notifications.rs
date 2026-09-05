// SPDX-License-Identifier: Apache-2.0

//! Notification routing and formatting, and the per-agent `debug_mode` toggle on notification runs.

use super::{drain_events, subscribe_session, test_app_state};
use crate::server::AppState;
use crate::sse::SseEventData;
use alms_coordinator::message_bus::{DmEvent, MessageSource, RunTrigger};
use alms_core::{AgentId, Run, RunStatus, SessionId};
use alms_tools::message_sender::ConversationEndReason;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// 3. Notification on invisible session
// ---------------------------------------------------------------------------

/// Test that when `source_session_id` is `None` (the agent was a pure DM
/// recipient), the notification run stays on the `notifications:` session
/// and is NOT rerouted to the user-facing web session.
///
/// This is a regression test for #513 (notification pollution in web-chat).
/// The invisible session path is critical for agents that never initiated
/// the DM conversation.
#[tokio::test]
async fn notification_stays_on_invisible_session_when_no_source() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let agent_id = AgentId::new();
    let sender_agent_id = AgentId::new();

    // Create the notifications session (invisible to user).
    let notif_session = state
        .session_manager
        .get_or_create(agent_id, "notifications:bob");
    let notif_session_id = notif_session.id;

    // Create a user-facing web session. If rerouting were happening,
    // the run would incorrectly land here.
    let web_session = state.session_manager.get_or_create(agent_id, "web");
    let web_session_id = web_session.id;

    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx
        .send(RunTrigger {
            agent_id,
            session_id: notif_session_id,
            input: "Conversation ended".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: sender_agent_id,
                from_name: "alice".to_string(),
                reason: ConversationEndReason::Ignored,
                self_notification: false,
                source_session_id: None, // <-- pure recipient, no source session
            },
            context_id: notif_session.context_id.clone(),
        })
        .await
        .unwrap();
    drop(test_tx);

    crate::runs::notifications::run_trigger_loop(test_rx, state.clone()).await;

    // Run MUST be on the notifications session.
    let notif_runs = state.run_manager.list_by_session(notif_session_id, 10);
    assert!(
        !notif_runs.is_empty(),
        "expected a run on the notifications session"
    );

    // No runs should appear on the web session.
    let web_runs = state.run_manager.list_by_session(web_session_id, 10);
    assert!(
        web_runs.is_empty(),
        "notification run must NOT be rerouted to the user-facing web session \
         when source_session_id is None (pure DM recipient)"
    );

    shutdown_token.cancel();
}

/// Test that when `source_session_id` is `Some(...)`, the trigger's
/// `session_id` is used as-is (the MessageBus already set it to the source
/// session). The run_trigger_loop does not override it.
#[tokio::test]
async fn notification_uses_source_session_when_present() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let agent_id = AgentId::new();
    let sender_agent_id = AgentId::new();

    // The source session is a user-facing session where the agent
    // initiated the DM from.
    let source_session = state.session_manager.get_or_create(agent_id, "my-chat");
    let source_session_id = source_session.id;

    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx
        .send(RunTrigger {
            agent_id,
            // MessageBus already set session_id to the source session.
            session_id: source_session_id,
            input: "Conversation ended".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: sender_agent_id,
                from_name: "alice".to_string(),
                reason: ConversationEndReason::Ignored,
                self_notification: false,
                source_session_id: Some(source_session_id),
            },
            context_id: source_session.context_id.clone(),
        })
        .await
        .unwrap();
    drop(test_tx);

    crate::runs::notifications::run_trigger_loop(test_rx, state.clone()).await;

    // Run should be on the source session.
    let runs = state.run_manager.list_by_session(source_session_id, 10);
    assert!(
        !runs.is_empty(),
        "notification run should be on the source session when source_session_id is present"
    );
    assert_eq!(runs[0].session_id, source_session_id);

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// Additional edge cases: DM conversation history formatting
// ---------------------------------------------------------------------------

/// Test that `format_dm_conversation_history` correctly truncates long
/// conversations by dropping the oldest messages and adding an omission note.
#[test]
fn dm_conversation_history_truncation_preserves_recent_messages() {
    use alms_session::{Content, Message, Role};

    // Generate enough messages to exceed DM_HISTORY_MAX_CHARS.
    let mut messages = Vec::new();
    for i in 0..200 {
        let sender = if i % 2 == 0 { "alice" } else { "bob" };
        messages.push(Message {
            id: format!("msg-{i}"),
            role: Role::User,
            content: Content::Text(format!(
                "Message number {i} with some padding text to fill up space"
            )),
            timestamp: alms_core::Timestamp::now(),
            metadata: Some(serde_json::json!({
                "from_agent": sender,
                "message_type": "dm",
            })),
        });
    }

    let formatted = crate::runs::notifications::format_dm_conversation_history(&messages);

    // Should be within the character limit.
    assert!(
        formatted.len() <= crate::runs::notifications::DM_HISTORY_MAX_CHARS,
        "formatted history ({} chars) should not exceed DM_HISTORY_MAX_CHARS ({})",
        formatted.len(),
        crate::runs::notifications::DM_HISTORY_MAX_CHARS,
    );

    // Should contain the omission note.
    assert!(
        formatted.contains("earlier message(s) omitted"),
        "truncated history should contain an omission note"
    );

    // The most recent messages should be present (they are kept).
    assert!(
        formatted.contains("Message number 199"),
        "the most recent message should be preserved after truncation"
    );
}

/// Test that `format_dm_conversation_history` returns empty string
/// for an empty message list.
#[test]
fn dm_conversation_history_empty_input_returns_empty() {
    let formatted = crate::runs::notifications::format_dm_conversation_history(&[]);
    assert!(formatted.is_empty());
}

/// Test that `format_dm_conversation_history` skips synthetic markers
/// and only includes actual text messages.
#[test]
fn dm_conversation_history_skips_synthetic_markers() {
    use alms_session::{Content, Message, Role};

    let messages = vec![
        Message {
            id: "msg-1".to_string(),
            role: Role::User,
            content: Content::Text("Hello from alice".to_string()),
            timestamp: alms_core::Timestamp::now(),
            metadata: Some(serde_json::json!({
                "from_agent": "alice",
                "message_type": "dm",
            })),
        },
        // Synthetic marker (should be skipped).
        Message {
            id: "msg-2".to_string(),
            role: Role::System,
            content: Content::Text("(run completed)".to_string()),
            timestamp: alms_core::Timestamp::now(),
            metadata: Some(serde_json::json!({
                "synthetic": true,
                "type": "run_boundary",
            })),
        },
        Message {
            id: "msg-3".to_string(),
            role: Role::User,
            content: Content::Text("Reply from bob".to_string()),
            timestamp: alms_core::Timestamp::now(),
            metadata: Some(serde_json::json!({
                "from_agent": "bob",
                "message_type": "dm",
            })),
        },
    ];

    let formatted = crate::runs::notifications::format_dm_conversation_history(&messages);

    assert!(
        formatted.contains("alice"),
        "should include alice's message"
    );
    assert!(formatted.contains("bob"), "should include bob's message");
    assert!(
        !formatted.contains("run completed"),
        "should NOT include synthetic marker text"
    );
}

// ---------------------------------------------------------------------------
// Additional edge cases: DM-ended notification formatting
// ---------------------------------------------------------------------------

/// Test that `format_dm_ended_notification` includes conversation history
/// when provided.
#[test]
fn dm_ended_notification_includes_history_when_available() {
    let history = "[10:00] alice: Hey Bob!\n[10:01] bob: Hello Alice!";
    let notification = crate::runs::notifications::format_dm_ended_notification(
        "alice",
        ConversationEndReason::Ignored,
        Some(history),
        false,
    );

    assert!(
        notification.contains("Hey Bob!"),
        "notification should include conversation history"
    );
    assert!(
        notification.contains("Hello Alice!"),
        "notification should include all messages from history"
    );
    assert!(
        notification.contains("alice"),
        "notification should mention the agent who ended the conversation"
    );
}

/// Test that `format_dm_ended_notification` falls back gracefully when
/// no conversation history is available.
#[test]
fn dm_ended_notification_fallback_without_history() {
    let notification = crate::runs::notifications::format_dm_ended_notification(
        "alice",
        ConversationEndReason::Ignored,
        None,
        false,
    );

    assert!(
        notification.contains("alice"),
        "fallback notification should mention the agent name"
    );
    // Should not be empty.
    assert!(
        !notification.is_empty(),
        "notification should not be empty even without history"
    );
}

/// Test that `format_dm_ended_notification` correctly differentiates
/// between `Ignored` and `DepthExceeded` reasons.
#[test]
fn dm_ended_notification_distinguishes_reasons() {
    let ignored = crate::runs::notifications::format_dm_ended_notification(
        "alice",
        ConversationEndReason::Ignored,
        None,
        false,
    );
    let depth = crate::runs::notifications::format_dm_ended_notification(
        "alice",
        ConversationEndReason::DepthExceeded,
        None,
        false,
    );

    // Both should mention alice.
    assert!(ignored.contains("alice"));
    assert!(depth.contains("alice"));

    // They should be different (different reason text).
    assert_ne!(
        ignored, depth,
        "ignored and depth_exceeded notifications should have different content"
    );

    // Depth exceeded should mention the depth/limit.
    assert!(
        depth.contains("depth") || depth.contains("limit") || depth.contains("maximum"),
        "depth exceeded notification should mention the message depth/limit"
    );
}

// ---------------------------------------------------------------------------
// Edge case: is_internal_context_id classification
// ---------------------------------------------------------------------------

/// Test the context ID classification used to determine whether a session
/// is internal (invisible to the user) or user-facing.
///
/// This is critical for notification routing and marker persistence
/// decisions throughout lifecycle.rs.
#[test]
fn internal_context_id_classification() {
    assert!(crate::runs::is_internal_context_id("job_abc-123"));
    assert!(crate::runs::is_internal_context_id("subagent_task-1"));
    assert!(crate::runs::is_internal_context_id("dm:alice:bob"));
    assert!(crate::runs::is_internal_context_id("notifications:alice"));
    assert!(crate::runs::is_internal_context_id("episodic:summary"));

    assert!(!crate::runs::is_internal_context_id("web"));
    assert!(!crate::runs::is_internal_context_id("default"));
    assert!(!crate::runs::is_internal_context_id("my-custom-session"));
    assert!(!crate::runs::is_internal_context_id("chat-with-user"));
}

/// Test `find_user_facing_session` returns the correct session and
/// excludes internal sessions.
#[test]
fn find_user_facing_session_excludes_internal() {
    let mgr = alms_session::SessionManager::new(alms_session::SessionConfig::default());
    let mgr = std::sync::Arc::new(mgr);
    let agent_id = AgentId::new();

    // Create internal sessions.
    mgr.get_or_create(agent_id, "dm:alice:bob");
    mgr.get_or_create(agent_id, "notifications:alice");
    mgr.get_or_create(agent_id, "job_cron-123");

    // No user-facing session yet.
    let result = crate::runs::find_user_facing_session(&mgr, agent_id);
    assert!(
        result.is_none(),
        "should return None when only internal sessions exist"
    );

    // Create a user-facing session.
    let web = mgr.get_or_create(agent_id, "web");
    let result = crate::runs::find_user_facing_session(&mgr, agent_id);
    assert!(result.is_some());
    assert_eq!(result.unwrap().id, web.id);
}

// ---------------------------------------------------------------------------
// Edge case: resolve_posture_for_run with all posture variants
// ---------------------------------------------------------------------------

/// Test that system-triggered runs override ONLY Guarded posture, leaving
/// all other postures unchanged. This is a comprehensive test covering
/// all posture variants.
#[test]
fn resolve_posture_comprehensive() {
    use crate::runs::lifecycle::resolve_posture_for_run;
    use alms_runtime::Posture;

    // System-triggered: only Guarded changes.
    assert_eq!(
        resolve_posture_for_run(Posture::Guarded, true),
        Posture::Autonomous,
    );
    assert_eq!(
        resolve_posture_for_run(Posture::Autonomous, true),
        Posture::Autonomous,
    );
    assert_eq!(
        resolve_posture_for_run(Posture::FullControl, true),
        Posture::FullControl,
    );

    // User-initiated: nothing changes.
    assert_eq!(
        resolve_posture_for_run(Posture::Guarded, false),
        Posture::Guarded,
    );
    assert_eq!(
        resolve_posture_for_run(Posture::Autonomous, false),
        Posture::Autonomous,
    );
    assert_eq!(
        resolve_posture_for_run(Posture::FullControl, false),
        Posture::FullControl,
    );
}

// ---------------------------------------------------------------------------
// Edge case: DM event loop
// ---------------------------------------------------------------------------

/// Test that `dm_event_loop` correctly forwards DM messages as SSE events
/// to the DM session's subscribers.
#[tokio::test]
async fn dm_event_loop_forwards_messages_to_session_subscribers() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let agent_id = AgentId::new();
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    let _dm_session = state
        .session_manager
        .get_or_create_with_id(dm_session_id, agent_id, "dm:alice:bob")
        .unwrap();

    // Subscribe to the DM session.
    let mut rx = subscribe_session(&state, dm_session_id);

    // Send a DM event.
    let (event_tx, event_rx) = mpsc::channel(8);
    event_tx
        .send(DmEvent {
            session_id: dm_session_id,
            from_agent: "alice".to_string(),
            from_agent_id: agent_id,
            message: "Hello Bob, this is a test DM!".to_string(),
            ts: chrono::Utc::now(),
        })
        .await
        .unwrap();
    drop(event_tx);

    // Run the event loop.
    crate::runs::notifications::dm_event_loop(event_rx, state.clone()).await;

    // Verify a dm_message SSE event was emitted.
    let events = drain_events(&mut rx);
    let dm_event = events.iter().find(|e| e.event_type == "dm_message");
    assert!(
        dm_event.is_some(),
        "expected a dm_message SSE event; got: {:?}",
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
    );

    if let Some(event) = dm_event {
        assert_eq!(
            event.data.get("from_agent").and_then(|v| v.as_str()),
            Some("alice"),
        );
        assert!(
            event
                .data
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .contains("Hello Bob"),
        );
    }

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// Edge case: RunManager in-flight tracking
// ---------------------------------------------------------------------------

/// Test that the in-flight counter correctly tracks and drains runs.
///
/// This exercises the graceful shutdown path where `wait_drain` blocks
/// until all in-flight runs complete.
#[tokio::test]
async fn run_manager_in_flight_tracking() {
    let rm = crate::server::RunManager::new();

    assert_eq!(rm.in_flight_count(), 0);

    rm.track_in_flight();
    assert_eq!(rm.in_flight_count(), 1);

    rm.track_in_flight();
    assert_eq!(rm.in_flight_count(), 2);

    rm.untrack_in_flight();
    assert_eq!(rm.in_flight_count(), 1);

    rm.untrack_in_flight();
    assert_eq!(rm.in_flight_count(), 0);

    // wait_drain should return immediately when no runs are in flight.
    let drained = rm.wait_drain(std::time::Duration::from_millis(10)).await;
    assert!(
        drained,
        "wait_drain should return true when no runs in flight"
    );
}

/// Test that `wait_drain` times out when runs are still in flight.
#[tokio::test]
async fn run_manager_wait_drain_timeout() {
    let rm = crate::server::RunManager::new();

    rm.track_in_flight();
    // Do NOT untrack -- simulate a stuck run.

    let drained = rm.wait_drain(std::time::Duration::from_millis(50)).await;
    assert!(
        !drained,
        "wait_drain should return false (timeout) when runs are still in flight"
    );

    // Clean up.
    rm.untrack_in_flight();
}

// ---------------------------------------------------------------------------
// Notification runs honor the per-agent debug_mode toggle (#546 flip removal)
//
// A #546-era convenience in `lifecycle::execute_run` used to force
// `debug_mode = true` for system-triggered non-peer runs landing on a
// user-facing session (the shape `enqueue_triggered_run` produces for
// subagent-completion and DM-ended notifications; job completions never
// create a run — `notify_job_completion` is SSE + marker only). Post-#1003
// the per-agent `debug_mode` toggle is the single source of truth, and the
// flip silently overrode a toggle the user had set to off: after a background
// `invoke_agent` subagent completed, the parent's notification run emitted a
// `context_debug` SSE event and the UI showed the "Context sent to LLM" row
// with debug mode disabled. These two tests pin the corrected contract from
// both sides: debug off ⇒ no context_debug on the notification run; debug on
// ⇒ the notification run still emits it (the #546 capability, now opt-in).
// ---------------------------------------------------------------------------

/// Shared driver: seeds a mock-mode state + agent with the given
/// `debug_mode`, executes a system-triggered non-peer run on a user-facing
/// session (the subagent-completion notification shape), and returns the
/// final run record plus the SSE events observed on the session stream.
async fn drive_notification_run_with_debug_mode(
    debug_mode: bool,
) -> (alms_core::Run, Vec<SseEventData>) {
    use alms_core::registry::AgentRecord;

    // Mock-mode LLM: the run completes deterministically without a provider,
    // and `finish_run`'s `if self.config.debug_mode` gate is still exercised
    // (the ContextDebug emission happens after build_context, before the
    // LLM call).
    let llm_config = alms_runtime::LlmConfig {
        mock: true,
        ..alms_runtime::LlmConfig::default()
    };
    let gateway_config = crate::gateway::GatewayConfig {
        db_path: Some(":memory:".to_string()),
        llm_config,
        ..crate::gateway::GatewayConfig::default()
    };
    let gateway = crate::gateway::Gateway::new(gateway_config).unwrap();
    let scheduler = std::sync::Arc::new(alms_runtime::Scheduler::new());
    let shutdown_token = CancellationToken::new();
    let (completion_tx, _cr) = mpsc::unbounded_channel();
    let (trigger_tx, _tr) = mpsc::channel(8);
    let (dm_event_tx, _dr) = mpsc::channel(8);
    let state = AppState::new(
        gateway,
        scheduler,
        shutdown_token.clone(),
        completion_tx,
        trigger_tx,
        dm_event_tx,
    )
    .unwrap();

    // No per-agent model/provider overrides — the run uses the server-default
    // mock client unchanged, so nothing can fail before the runtime starts.
    let agent_id = AgentId::new();
    let agent = AgentRecord {
        id: agent_id,
        debug_mode,
        ..AgentRecord::for_test("notify-debug-gate-agent")
    };
    state
        .session_manager
        .store()
        .expect("SQLite-backed state should have a store")
        .create_agent(&agent)
        .expect("agent seed should succeed");

    // User-facing context (no internal prefix) — the exact surface the old
    // flip targeted (`is_system_triggered && !is_peer_message &&
    // !is_internal_context_id`).
    let context_id = "web-notify-debug-gate";
    let session = state.session_manager.get_or_create(agent_id, context_id);
    let session_id = session.id;

    // Subscribe BEFORE the run so every SSE event (run_started,
    // context_debug, ...) is captured. `execute_run` awaits its event
    // forwarder before returning, so draining afterwards is deterministic.
    let mut session_rx = subscribe_session(&state, session_id);

    let run = Run::new(session_id, agent_id, "Subagent 'worker' completed.".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    crate::runs::lifecycle::execute_run(
        state.clone(),
        crate::runs::RunParams {
            run_id,
            session_id,
            agent_id,
            input: run.input,
            context_id: context_id.to_string(),
            cancel_token,
            // The subagent-completion notification shape produced by
            // `enqueue_triggered_run`.
            is_peer_message: false,
            is_system_triggered: true,
            input_pre_persisted: false,
            dm_ended_peer: None,
        },
    )
    .await;

    let final_run = state
        .run_manager
        .get_run(run_id)
        .expect("run must still exist after execute_run returns");
    let events = drain_events(&mut session_rx);

    shutdown_token.cancel();
    (final_run, events)
}

/// Regression: with the agent's `debug_mode` OFF, a system-triggered
/// notification run (subagent completion landing on the parent's user-facing
/// session) must NOT emit `context_debug` — the old #546 flip forced it on,
/// which is exactly the "Context sent to LLM row appears with debug mode
/// disabled" bug.
#[tokio::test]
async fn notification_run_with_debug_mode_off_does_not_emit_context_debug() {
    let (final_run, events) = drive_notification_run_with_debug_mode(false).await;

    // Sanity: the run actually executed (mock mode completes) and the
    // subscription observed its lifecycle — guards against a vacuous pass
    // where the run failed before the runtime ever built a context.
    assert_eq!(
        final_run.status(),
        RunStatus::Completed,
        "mock-mode notification run must complete; got {:?} (error: {:?})",
        final_run.status(),
        final_run.error,
    );
    assert!(
        events.iter().any(|e| e.event_type == "run_started"),
        "session subscription must observe run_started; got: {:?}",
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>(),
    );

    // The resolved snapshot honors the agent record — no force-flip.
    let snapshot = final_run
        .resolved_config()
        .expect("running run must have persisted a resolved-config snapshot");
    assert!(
        !snapshot.debug_mode,
        "system-triggered notification runs must not force debug_mode on \
         (the removed #546 flip); the agent record is the single source of truth"
    );

    // And the wire never carried the context snapshot.
    assert!(
        !events.iter().any(|e| e.event_type == "context_debug"),
        "no context_debug SSE event may be emitted when the agent's \
         debug_mode is off, including on the notification path"
    );
}

/// Companion opt-in leg: with the agent's `debug_mode` ON, the same
/// notification run DOES emit `context_debug` — the capability #546 wanted
/// is preserved, gated behind the per-agent toggle like every other run.
#[tokio::test]
async fn notification_run_with_debug_mode_on_emits_context_debug() {
    let (final_run, events) = drive_notification_run_with_debug_mode(true).await;

    assert_eq!(
        final_run.status(),
        RunStatus::Completed,
        "mock-mode notification run must complete; got {:?} (error: {:?})",
        final_run.status(),
        final_run.error,
    );

    let snapshot = final_run
        .resolved_config()
        .expect("running run must have persisted a resolved-config snapshot");
    assert!(
        snapshot.debug_mode,
        "per-agent debug_mode = true must reach the notification run's snapshot"
    );

    assert!(
        events.iter().any(|e| e.event_type == "context_debug"),
        "notification runs must still emit context_debug when the agent \
         opts in via debug_mode; got: {:?}",
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>(),
    );
}
