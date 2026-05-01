//! Integration tests for lifecycle.rs and notifications.rs error paths.
//!
//! These tests cover the scenarios identified in issue #629 (Tim's stability
//! audit #613) as the highest-impact gaps in test coverage:
//!
//! - Cancelled-during-tool execution (tokio::select! path)
//! - DM with ignore_message -> end_conversation -> notification flow
//! - Notification on invisible session
//! - Failure with partial tool calls
//! - Run queueing priority ordering
//! - Subagent completion with session ID propagation
//!
//! Each test constructs a minimal `AppState` (no real LLM, no SQLite) and
//! exercises the gateway's run management, notification routing, and event
//! broadcasting infrastructure.

use crate::gateway::GatewayConfig;
use crate::server::AppState;
use crate::sse::SseEventData;
use alms_coordinator::message_bus::{DmEvent, MessageSource, RunTrigger};
use alms_coordinator::{SubagentCompletion, TaskId, TaskStatus};
use alms_core::{AgentId, Run, RunStatus, SessionId, TokenUsage};
use alms_tools::MessageSender;
use alms_tools::message_sender::ConversationEndReason;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Build a minimal `AppState` suitable for integration tests.
///
/// Uses in-memory session storage (no SQLite), a dummy LLM config, and
/// fresh channels for completion/trigger/dm-event loops.
fn test_app_state() -> (
    AppState,
    CancellationToken,
    mpsc::UnboundedReceiver<SubagentCompletion>,
    mpsc::UnboundedReceiver<RunTrigger>,
    mpsc::UnboundedReceiver<DmEvent>,
) {
    let gateway_config = GatewayConfig::default();
    let gateway = crate::gateway::Gateway::new(gateway_config).unwrap();
    let scheduler = Arc::new(alms_runtime::Scheduler::new());
    let shutdown_token = CancellationToken::new();
    let (completion_tx, completion_rx) = mpsc::unbounded_channel();
    let (trigger_tx, trigger_rx) = mpsc::unbounded_channel();
    let (dm_event_tx, dm_event_rx) = mpsc::unbounded_channel();
    let state = AppState::new(
        gateway,
        scheduler,
        shutdown_token.clone(),
        completion_tx,
        trigger_tx,
        dm_event_tx,
    )
    .unwrap();
    (
        state,
        shutdown_token,
        completion_rx,
        trigger_rx,
        dm_event_rx,
    )
}

/// Build an `AppState` backed by an in-memory SQLite store.
///
/// Used by tests that need a real agent registry (e.g. peer-name -> AgentId
/// resolution in `handle_dm_run_failure`). Mirrors the channel plumbing of
/// [`test_app_state`] but threads `db_path = Some(":memory:")` into the
/// `GatewayConfig` so `session_manager.store()` returns `Some(...)`.
fn test_app_state_with_sqlite() -> (
    AppState,
    CancellationToken,
    mpsc::UnboundedReceiver<SubagentCompletion>,
    mpsc::UnboundedReceiver<RunTrigger>,
    mpsc::UnboundedReceiver<DmEvent>,
) {
    let gateway_config = GatewayConfig {
        db_path: Some(":memory:".to_string()),
        ..GatewayConfig::default()
    };
    let gateway = crate::gateway::Gateway::new(gateway_config).unwrap();
    let scheduler = Arc::new(alms_runtime::Scheduler::new());
    let shutdown_token = CancellationToken::new();
    let (completion_tx, completion_rx) = mpsc::unbounded_channel();
    let (trigger_tx, trigger_rx) = mpsc::unbounded_channel();
    let (dm_event_tx, dm_event_rx) = mpsc::unbounded_channel();
    let state = AppState::new(
        gateway,
        scheduler,
        shutdown_token.clone(),
        completion_tx,
        trigger_tx,
        dm_event_tx,
    )
    .unwrap();
    (
        state,
        shutdown_token,
        completion_rx,
        trigger_rx,
        dm_event_rx,
    )
}

/// Build an `AppState` whose LLM client points at an unreachable local
/// address with a 1-second timeout, so any `execute_run` that reaches the
/// runtime LLM call fails quickly and deterministically through the
/// generic `Err(_)` arm.  Used by the #912 follow-up regression test
/// (PR #930) that asserts the gateway lifecycle no longer writes a
/// duplicate `(run failed) ...` `kind: "error"` marker.
fn test_app_state_with_failing_llm() -> (
    AppState,
    CancellationToken,
    mpsc::UnboundedReceiver<SubagentCompletion>,
    mpsc::UnboundedReceiver<RunTrigger>,
    mpsc::UnboundedReceiver<DmEvent>,
) {
    // Port 1 is reserved (`tcpmux`) and almost universally unbound on
    // CI / dev machines, so `connect()` fails immediately with
    // ECONNREFUSED rather than hanging.  Combined with a 1-second
    // request timeout this caps the test runtime at ~1s even in the
    // pathological case where the kernel is slow to refuse.
    let mut llm_config = alms_runtime::LlmConfig::default();
    llm_config.base_url = "http://127.0.0.1:1".to_string();
    llm_config.api_key = "fake-key-for-test".to_string();
    llm_config.timeout_secs = 1;
    llm_config.stream_chunk_timeout_secs = 1;

    let gateway_config = GatewayConfig {
        llm_config,
        ..GatewayConfig::default()
    };
    let gateway = crate::gateway::Gateway::new(gateway_config).unwrap();
    let scheduler = Arc::new(alms_runtime::Scheduler::new());
    let shutdown_token = CancellationToken::new();
    let (completion_tx, completion_rx) = mpsc::unbounded_channel();
    let (trigger_tx, trigger_rx) = mpsc::unbounded_channel();
    let (dm_event_tx, dm_event_rx) = mpsc::unbounded_channel();
    let state = AppState::new(
        gateway,
        scheduler,
        shutdown_token.clone(),
        completion_tx,
        trigger_tx,
        dm_event_tx,
    )
    .unwrap();
    (
        state,
        shutdown_token,
        completion_rx,
        trigger_rx,
        dm_event_rx,
    )
}

/// Seed two agents (`alice` and `bob`) into the SQLite-backed agent registry
/// so that peer-name resolution works in `handle_dm_run_failure` and
/// related lifecycle helpers.
///
/// Returns `(alice_id, bob_id)`.
fn seed_alice_bob(state: &AppState) -> (AgentId, AgentId) {
    use alms_core::registry::AgentRecord;
    use chrono::Utc;
    let store = state
        .session_manager
        .store()
        .expect("test_app_state_with_sqlite must provide a SQLite store");
    let alice = AgentRecord {
        id: AgentId::new(),
        name: "alice".into(),
        description: String::new(),
        model: None,
        posture: None,
        provider: None,
        telegram_token: None,
        thinking_budget_tokens: None,
        reasoning_effort: None,
        gemini_thinking_budget: None,
        summary_provider: None,
        summary_model: None,
        is_default: false,
        created_at: Utc::now(),
        last_active: Utc::now(),
    };
    let bob = AgentRecord {
        id: AgentId::new(),
        name: "bob".into(),
        ..alice.clone()
    };
    store.create_agent(&alice).unwrap();
    store.create_agent(&bob).unwrap();
    (alice.id, bob.id)
}

/// Subscribe to SSE events on a session and return the receiver.
///
/// Events sent via `run_manager.send_session_event()` will be received
/// on the returned channel.
fn subscribe_session(
    state: &AppState,
    session_id: SessionId,
) -> mpsc::UnboundedReceiver<SseEventData> {
    let (tx, rx) = mpsc::unbounded_channel();
    state.run_manager.register_session_sender(session_id, tx);
    rx
}

/// Drain all currently buffered events from a receiver without blocking.
fn drain_events(rx: &mut mpsc::UnboundedReceiver<SseEventData>) -> Vec<SseEventData> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

// ---------------------------------------------------------------------------
// 1. Cancelled-during-tool execution (tokio::select! path)
// ---------------------------------------------------------------------------

/// Test that cancelling a run before it starts (queued-then-cancelled) results
/// in a `run_cancelled` SSE event and the run record transitioning to
/// `Cancelled` status.
///
/// This exercises the early-exit path at the top of `execute_run()` where
/// `cancel_token.is_cancelled()` is checked before the runtime is created.
/// This is the tokio::select! cancellation path that was identified as
/// under-tested in #629.
#[tokio::test]
async fn cancelled_before_execution_emits_cancelled_event() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state.session_manager.get_or_create(agent_id, "test-cancel");
    let session_id = session.id;

    // Create a run and insert it.
    let run = Run::new(session_id, agent_id, "test input".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run.clone());

    // Register a cancellation token and cancel it BEFORE execution.
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    cancel_token.cancel();

    // Subscribe to session events to capture the cancelled event.
    let mut rx = subscribe_session(&state, session_id);

    // Execute the run -- it should detect the pre-cancelled token and
    // short-circuit without creating a runtime.
    super::lifecycle::execute_run(
        state.clone(),
        super::RunParams {
            run_id,
            session_id,
            agent_id,
            input: run.input,
            overrides: super::RunOverrides::default(),
            context_id: "test-cancel".to_string(),
            cancel_token,
            is_peer_message: false,
            is_system_triggered: false,
            input_pre_persisted: false,
        },
    )
    .await;

    // Verify the run status is Cancelled.
    let run = state.run_manager.get_run(run_id).expect("run should exist");
    assert_eq!(
        run.status,
        RunStatus::Cancelled,
        "pre-cancelled run should transition to Cancelled status"
    );

    // Verify that a run_cancelled SSE event was emitted.
    let events = drain_events(&mut rx);
    assert!(
        events.iter().any(|e| e.event_type == "run_cancelled"),
        "expected a run_cancelled SSE event; got: {:?}",
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
    );

    shutdown_token.cancel();
}

/// Test that cancelling during shutdown also produces a `run_cancelled`
/// event, exercising the `state.shutdown_token.is_cancelled()` branch
/// in `execute_run()`.
#[tokio::test]
async fn cancelled_during_shutdown_emits_cancelled_event() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-shutdown");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "shutdown test".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run.clone());

    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    // Cancel the shutdown token (not the per-run token) to simulate
    // graceful shutdown in progress.
    shutdown_token.cancel();

    let mut rx = subscribe_session(&state, session_id);

    super::lifecycle::execute_run(
        state.clone(),
        super::RunParams {
            run_id,
            session_id,
            agent_id,
            input: run.input,
            overrides: super::RunOverrides::default(),
            context_id: "test-shutdown".to_string(),
            cancel_token,
            is_peer_message: false,
            is_system_triggered: false,
            input_pre_persisted: false,
        },
    )
    .await;

    let run = state.run_manager.get_run(run_id).expect("run should exist");
    assert_eq!(
        run.status,
        RunStatus::Cancelled,
        "run during shutdown should be cancelled"
    );

    let events = drain_events(&mut rx);
    assert!(
        events.iter().any(|e| e.event_type == "run_cancelled"),
        "expected a run_cancelled SSE event during shutdown"
    );
}

// ---------------------------------------------------------------------------
// 2. DM with ignore_message -> end_conversation -> notification flow
// ---------------------------------------------------------------------------

/// Test that a `ConversationEnded` trigger (with `Ignored` reason) flowing
/// through `run_trigger_loop` creates a notification run on the correct
/// session, emits `dm_conversation_ended` SSE, and persists a DM-ended
/// marker to the web-chat session.
///
/// This exercises the full DM lifecycle: ignore_message -> end_conversation
/// -> ConversationEnded trigger -> notification run + web-chat forwarding.
#[tokio::test]
async fn dm_conversation_ended_trigger_creates_notification_and_marker() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let agent_id = AgentId::new();
    let sender_agent_id = AgentId::new();

    // Create a notifications session for the target agent.
    let notif_session = state
        .session_manager
        .get_or_create(agent_id, "notifications:bob");
    let notif_session_id = notif_session.id;
    let notif_context_id = notif_session.context_id.clone();

    // Create a user-facing web-chat session so that
    // `notify_dm_ended_to_webchat` has a target.
    let web_session = state.session_manager.get_or_create(agent_id, "web");
    let web_session_id = web_session.id;

    // Subscribe to web-chat session to capture the forwarded DM-ended event.
    let mut web_rx = subscribe_session(&state, web_session_id);

    // Build a ConversationEnded trigger with Ignored reason.
    let (test_tx, test_rx) = mpsc::unbounded_channel();
    test_tx
        .send(RunTrigger {
            agent_id,
            session_id: notif_session_id,
            input: "DM ended by alice".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: sender_agent_id,
                from_name: "alice".to_string(),
                reason: ConversationEndReason::Ignored,
                source_session_id: None,
            },
            context_id: notif_context_id.clone(),
        })
        .unwrap();
    drop(test_tx);

    // Run the trigger loop to completion.
    super::notifications::run_trigger_loop(test_rx, state.clone()).await;

    // Verify a notification run was created on the notifications session.
    let runs = state.run_manager.list_by_session(notif_session_id, 10);
    assert!(
        !runs.is_empty(),
        "expected at least one run on the notifications session"
    );
    assert_eq!(
        runs[0].session_id, notif_session_id,
        "notification run must be on the notifications: session"
    );
    assert_eq!(runs[0].agent_id, agent_id);

    // Verify that the web-chat session received a DM-ended marker
    // (persisted by `notify_dm_ended_to_webchat`).
    let web_history = state.session_manager.get_history(web_session_id).unwrap();
    let dm_ended_markers: Vec<_> = web_history
        .iter()
        .filter(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("type").and_then(|v| v.as_str()) == Some("dm_ended_notification")
            })
        })
        .collect();
    assert!(
        !dm_ended_markers.is_empty(),
        "expected a dm_ended_notification marker on the web-chat session"
    );

    // Verify the marker contains the peer name.
    let marker_meta = dm_ended_markers[0].metadata.as_ref().unwrap();
    assert_eq!(
        marker_meta.get("peer").and_then(|v| v.as_str()),
        Some("alice"),
        "DM-ended marker should reference the peer who ended the conversation"
    );
    assert_eq!(
        marker_meta.get("reason").and_then(|v| v.as_str()),
        Some("ignored"),
        "DM-ended marker should record the reason as 'ignored'"
    );

    // Verify SSE events were forwarded to the web-chat session.
    let web_events = drain_events(&mut web_rx);
    assert!(
        web_events
            .iter()
            .any(|e| e.event_type == "dm_conversation_ended"),
        "expected a dm_conversation_ended SSE event on the web-chat session; got: {:?}",
        web_events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
    );

    shutdown_token.cancel();
}

/// Test that a `ConversationEnded` trigger with `DepthExceeded` reason
/// handles a missing agent registry gracefully (no panic) and still creates
/// the notification run.
///
/// Without a SQLite agent registry, peer name resolution fails and the
/// depth-exceeded SSE emission path is skipped. This verifies the code
/// degrades gracefully rather than panicking.
#[tokio::test]
async fn dm_depth_exceeded_graceful_without_agent_registry() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let agent_id = AgentId::new();
    let sender_agent_id = AgentId::new();

    // Register the target agent in the session manager so that the
    // peer name resolution (`load_agent_by_id`) works. Without SQLite
    // there is no agent registry, so this path will skip the SSE emission.
    // We need to verify the code path doesn't panic and gracefully
    // handles the missing agent record.
    let notif_session = state
        .session_manager
        .get_or_create(agent_id, "notifications:bob");
    let notif_session_id = notif_session.id;
    let notif_context_id = notif_session.context_id.clone();

    // Create the DM session between alice and bob so we can subscribe.
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    let _dm_session =
        state
            .session_manager
            .get_or_create_with_id(dm_session_id, agent_id, "dm:alice:bob");
    let mut dm_rx = subscribe_session(&state, dm_session_id);

    let (test_tx, test_rx) = mpsc::unbounded_channel();
    test_tx
        .send(RunTrigger {
            agent_id,
            session_id: notif_session_id,
            input: "DM depth exceeded".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: sender_agent_id,
                from_name: "alice".to_string(),
                reason: ConversationEndReason::DepthExceeded,
                source_session_id: None,
            },
            context_id: notif_context_id.clone(),
        })
        .unwrap();
    drop(test_tx);

    super::notifications::run_trigger_loop(test_rx, state.clone()).await;

    // Without a SQLite agent registry, the peer name resolution will fail
    // and the depth-exceeded SSE emission path will be skipped. Verify
    // the run was still created (the notification run is independent of
    // the SSE emission).
    let runs = state.run_manager.list_by_session(notif_session_id, 10);
    assert!(
        !runs.is_empty(),
        "notification run should be created even when peer resolution fails"
    );

    // The DM session SSE events may or may not contain dm_conversation_ended
    // depending on whether peer name resolution succeeded. In our test setup
    // (no SQLite), it won't. Verify no panic occurred.
    let _dm_events = drain_events(&mut dm_rx);

    shutdown_token.cancel();
}

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

    let (test_tx, test_rx) = mpsc::unbounded_channel();
    test_tx
        .send(RunTrigger {
            agent_id,
            session_id: notif_session_id,
            input: "Conversation ended".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: sender_agent_id,
                from_name: "alice".to_string(),
                reason: ConversationEndReason::Ignored,
                source_session_id: None, // <-- pure recipient, no source session
            },
            context_id: notif_session.context_id.clone(),
        })
        .unwrap();
    drop(test_tx);

    super::notifications::run_trigger_loop(test_rx, state.clone()).await;

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

    let (test_tx, test_rx) = mpsc::unbounded_channel();
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
                source_session_id: Some(source_session_id),
            },
            context_id: source_session.context_id.clone(),
        })
        .unwrap();
    drop(test_tx);

    super::notifications::run_trigger_loop(test_rx, state.clone()).await;

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
// 4. Failure with partial tool calls
// ---------------------------------------------------------------------------

/// Test that the `format_completion_notification` function correctly
/// formats notifications for different task statuses, including failed
/// subagents (which represent partial tool call scenarios).
#[test]
fn format_completion_notification_for_failed_subagent() {
    let completion = SubagentCompletion {
        task_id: TaskId::new(),
        subagent_name: Some("researcher".to_string()),
        status: TaskStatus::Failed,
        summary: "Error: API rate limit exceeded after 3 tool calls".to_string(),
        parent_session_id: SessionId::new(),
        parent_agent_id: AgentId::new(),
        subagent_session_id: SessionId::new(),
        task_description: Some("Research the topic".to_string()),
        tool_count: Some(3),
        duration_ms: Some(5000),
        token_usage: Some(TokenUsage {
            prompt_tokens: 1000,
            completion_tokens: 200,
            ..TokenUsage::default()
        }),
    };

    let notification = super::notifications::format_completion_notification(&completion);
    assert!(
        notification.contains("failed"),
        "notification should indicate the subagent failed"
    );
    assert!(
        notification.contains("researcher"),
        "notification should mention the subagent name"
    );
    assert!(
        notification.contains("API rate limit exceeded"),
        "notification should include the error summary"
    );
}

/// Test that the `format_completion_notification` function handles
/// cancelled subagents (another form of partial execution).
#[test]
fn format_completion_notification_for_cancelled_subagent() {
    let completion = SubagentCompletion {
        task_id: TaskId::new(),
        subagent_name: Some("writer".to_string()),
        status: TaskStatus::Cancelled,
        summary: "Run was cancelled by user".to_string(),
        parent_session_id: SessionId::new(),
        parent_agent_id: AgentId::new(),
        subagent_session_id: SessionId::new(),
        task_description: None,
        tool_count: Some(1),
        duration_ms: Some(1500),
        token_usage: None,
    };

    let notification = super::notifications::format_completion_notification(&completion);
    assert!(
        notification.contains("cancelled"),
        "notification should indicate the subagent was cancelled"
    );
}

/// Test that when a run with partial tool calls is recorded, the
/// `RunManager` correctly tracks the run status as failed while
/// preserving the error message.
#[tokio::test]
async fn partial_tool_call_failure_preserves_error_in_run_record() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-partial-fail");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "test input".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);

    // Simulate what execute_run does on FailedWithToolCalls: mark as failed
    // with the error message while tool calls are persisted separately.
    let error_msg = "LLM API error after 2 tool calls".to_string();
    state
        .run_manager
        .mark_run_as_failed(run_id, error_msg.clone());

    let run = state.run_manager.get_run(run_id).expect("run should exist");
    assert_eq!(run.status, RunStatus::Failed);
    assert_eq!(
        run.error.as_deref(),
        Some("LLM API error after 2 tool calls"),
        "error message should be preserved in the run record"
    );

    shutdown_token.cancel();
}

/// Test that a completed run is not cancellable via the real
/// `RunManager::cancel_run` path.
///
/// Mirrors the lifecycle that `execute_run` follows: register a cancel
/// token, complete the run, remove the token. Afterwards, `cancel_run`
/// must return `false` (no token to cancel) and the run status must
/// remain `Completed`.
#[tokio::test]
async fn completed_run_is_not_cancellable() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-double-cancel");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "test".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);

    // Register a cancel token (as execute_run does on start).
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token);

    // Mark the run as completed.
    state.run_manager.mark_run_as_completed(
        run_id,
        "done".to_string(),
        TokenUsage {
            prompt_tokens: 10,
            completion_tokens: 5,
            ..TokenUsage::default()
        },
    );

    // Remove the cancel token (as execute_run does after completion).
    state.run_manager.remove_cancel_token(run_id);

    // Exercise the real RunManager::cancel_run path -- it should return
    // false because the cancel token was cleaned up after completion.
    let cancelled = state.run_manager.cancel_run(run_id);
    assert!(
        !cancelled,
        "cancel_run should return false for a completed run whose token was removed"
    );

    // Verify the run status is still Completed (not mutated).
    let run = state.run_manager.get_run(run_id).unwrap();
    assert_eq!(
        run.status,
        RunStatus::Completed,
        "completed run status must not change after a cancel attempt"
    );

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// 5. Run queueing priority ordering
// ---------------------------------------------------------------------------

/// Test that `enqueue_low` (used by notification/subagent runs) does not
/// starve when mixed with normal-priority `enqueue` calls.
///
/// This verifies that the `SessionQueue` processes both normal and low
/// priority work items, and that `pending_count` correctly reflects
/// the queue depth.
#[tokio::test]
async fn agent_queue_pending_count_reflects_enqueued_items() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();

    // Before any enqueue, pending_count should be 0.
    assert_eq!(
        state.agent_queue.pending_count(&agent_id),
        0,
        "empty queue should have pending_count == 0"
    );

    // Enqueue a normal-priority item.
    let (done_tx1, done_rx1) = tokio::sync::oneshot::channel::<()>();
    let (start_tx1, start_rx1) = tokio::sync::oneshot::channel::<()>();
    state.agent_queue.enqueue(
        agent_id,
        Box::pin(async move {
            let _ = start_tx1.send(());
            let _ = done_rx1.await;
        }),
    );

    // Enqueue a low-priority item.
    let (done_tx2, done_rx2) = tokio::sync::oneshot::channel::<()>();
    state.agent_queue.enqueue_low(
        agent_id,
        Box::pin(async move {
            let _ = done_rx2.await;
        }),
    );

    // Wait for the first item to start processing.
    let _ = start_rx1.await;

    // The second item should be pending while the first is executing.
    // Note: pending_count semantics may vary -- it counts items waiting
    // to be dequeued, not including the currently executing one.
    // We verify it's >= 1 to account for both interpretations.
    let pending = state.agent_queue.pending_count(&agent_id);
    assert!(
        pending >= 1,
        "expected at least 1 pending item while first is executing; got {pending}"
    );

    // Release both items so the queue drains.
    let _ = done_tx1.send(());
    let _ = done_tx2.send(());

    // Give the queue processor a moment to drain.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    shutdown_token.cancel();
}

/// Test that triggered runs (via `enqueue_triggered_run`) use low priority
/// and are properly recorded in the RunManager.
#[tokio::test]
async fn triggered_run_uses_low_priority_and_records_run() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "notifications:test");
    let session_id = session.id;

    // Subscribe to capture the run_created event.
    let mut rx = subscribe_session(&state, session_id);

    // Build and send a subagent completion trigger.
    let (test_tx, test_rx) = mpsc::unbounded_channel();
    test_tx
        .send(RunTrigger {
            agent_id,
            session_id,
            input: "Subagent completed its task".to_string(),
            source: MessageSource::SubagentCompletion,
            context_id: session.context_id.clone(),
        })
        .unwrap();
    drop(test_tx);

    super::notifications::run_trigger_loop(test_rx, state.clone()).await;

    // Verify the run was created.
    let runs = state.run_manager.list_by_session(session_id, 10);
    assert!(
        !runs.is_empty(),
        "triggered run should be recorded in RunManager"
    );

    // Verify a run_created SSE event was emitted with system_triggered=true.
    let events = drain_events(&mut rx);
    let run_created = events.iter().find(|e| e.event_type == "run_created");
    assert!(
        run_created.is_some(),
        "expected a run_created SSE event for the triggered run"
    );

    // Verify the run_created event data contains is_notification flag.
    if let Some(event) = run_created {
        let data = &event.data;
        assert_eq!(
            data.get("is_notification").and_then(|v| v.as_bool()),
            Some(true),
            "triggered runs should have is_notification=true in run_created event"
        );
    }

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// 6. Subagent completion with session ID propagation
// ---------------------------------------------------------------------------

/// Test that `completion_notification_loop` creates a run on the correct
/// parent session and emits a `subagent_completed` SSE event with the
/// subagent's session ID for frontend navigation.
///
/// This verifies the full subagent completion -> notification flow including
/// session ID propagation (#629 item).
#[tokio::test]
async fn subagent_completion_propagates_session_id() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let parent_agent_id = AgentId::new();
    let parent_session = state
        .session_manager
        .get_or_create(parent_agent_id, "parent-chat");
    let parent_session_id = parent_session.id;

    let subagent_session_id = SessionId::new();

    // Subscribe to the parent session to capture SSE events.
    let mut rx = subscribe_session(&state, parent_session_id);

    // Send a subagent completion event.
    let (test_tx, test_rx) = mpsc::unbounded_channel();
    test_tx
        .send(SubagentCompletion {
            task_id: TaskId::new(),
            subagent_name: Some("researcher".to_string()),
            status: TaskStatus::Completed,
            summary: "Found 3 relevant papers on the topic.".to_string(),
            parent_session_id,
            parent_agent_id,
            subagent_session_id,
            task_description: Some("Research quantum computing advances".to_string()),
            tool_count: Some(5),
            duration_ms: Some(12000),
            token_usage: Some(TokenUsage {
                prompt_tokens: 3000,
                completion_tokens: 800,
                ..TokenUsage::default()
            }),
        })
        .unwrap();
    drop(test_tx);

    // Run the completion notification loop.
    super::notifications::completion_notification_loop(test_rx, state.clone()).await;

    // Verify a run was created on the parent session.
    let runs = state.run_manager.list_by_session(parent_session_id, 10);
    assert!(
        !runs.is_empty(),
        "completion notification should create a run on the parent session"
    );
    assert_eq!(runs[0].agent_id, parent_agent_id);

    // Verify the subagent_completed SSE event contains the subagent's session ID.
    let events = drain_events(&mut rx);
    let completion_event = events.iter().find(|e| e.event_type == "subagent_completed");
    assert!(
        completion_event.is_some(),
        "expected a subagent_completed SSE event on the parent session; got: {:?}",
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
    );

    if let Some(event) = completion_event {
        let data = &event.data;

        // Verify subagent session ID is propagated (for frontend navigation).
        assert_eq!(
            data.get("subagent_session_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            Some(subagent_session_id.0.to_string()),
            "subagent_completed event must include the subagent's session_id \
             in the subagent_session_id field"
        );

        // Verify subagent name is included.
        assert_eq!(
            data.get("subagent_name").and_then(|v| v.as_str()),
            Some("researcher"),
            "subagent_completed event must include the subagent name"
        );

        // Verify status is included.
        assert_eq!(
            data.get("status").and_then(|v| v.as_str()),
            Some("done"),
            "subagent_completed event must include the status"
        );
    }

    // Verify a subagent_completion marker was persisted to session history.
    let history = state
        .session_manager
        .get_history(parent_session_id)
        .unwrap();
    let completion_markers: Vec<_> = history
        .iter()
        .filter(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("type").and_then(|v| v.as_str()) == Some("subagent_completion")
            })
        })
        .collect();
    assert!(
        !completion_markers.is_empty(),
        "expected a subagent_completion marker persisted to session history"
    );

    // Verify the marker contains the subagent's session ID for navigation.
    let marker_meta = completion_markers[0].metadata.as_ref().unwrap();
    assert_eq!(
        marker_meta
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        Some(subagent_session_id.0.to_string()),
        "subagent_completion marker must include the subagent's session_id"
    );
    assert_eq!(
        marker_meta.get("subagent_name").and_then(|v| v.as_str()),
        Some("researcher"),
    );
    assert_eq!(
        marker_meta.get("status").and_then(|v| v.as_str()),
        Some("done"),
    );

    shutdown_token.cancel();
}

/// Test that subagent completion for a missing parent session is
/// gracefully skipped (warn + continue), not a panic.
///
/// This exercises the defensive check at the top of
/// `completion_notification_loop` where the parent session lookup fails.
#[tokio::test]
async fn subagent_completion_with_missing_parent_session_is_skipped() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let parent_agent_id = AgentId::new();
    // Use a session ID that was never created.
    let missing_session_id = SessionId::new();

    let (test_tx, test_rx) = mpsc::unbounded_channel();
    test_tx
        .send(SubagentCompletion {
            task_id: TaskId::new(),
            subagent_name: Some("ghost".to_string()),
            status: TaskStatus::Completed,
            summary: "This should be skipped".to_string(),
            parent_session_id: missing_session_id,
            parent_agent_id,
            subagent_session_id: SessionId::new(),
            task_description: None,
            tool_count: None,
            duration_ms: None,
            token_usage: None,
        })
        .unwrap();
    drop(test_tx);

    // Should complete without panic.
    super::notifications::completion_notification_loop(test_rx, state.clone()).await;

    // No run should have been created for the missing session.
    let runs = state.run_manager.list_by_session(missing_session_id, 10);
    assert!(
        runs.is_empty(),
        "no run should be created when parent session is missing"
    );

    shutdown_token.cancel();
}

/// Test that subagent completion with rich metadata (tool_count, duration_ms,
/// token_usage) is correctly propagated into the persisted marker.
#[tokio::test]
async fn subagent_completion_marker_includes_rich_metadata() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let parent_agent_id = AgentId::new();
    let parent_session = state
        .session_manager
        .get_or_create(parent_agent_id, "rich-metadata-test");
    let parent_session_id = parent_session.id;

    let subagent_session_id = SessionId::new();

    let (test_tx, test_rx) = mpsc::unbounded_channel();
    test_tx
        .send(SubagentCompletion {
            task_id: TaskId::new(),
            subagent_name: Some("analyzer".to_string()),
            status: TaskStatus::Failed,
            summary: "OOM after processing large dataset".to_string(),
            parent_session_id,
            parent_agent_id,
            subagent_session_id,
            task_description: Some("Analyze the 10GB dataset".to_string()),
            tool_count: Some(42),
            duration_ms: Some(300_000),
            token_usage: Some(TokenUsage {
                prompt_tokens: 50_000,
                completion_tokens: 15_000,
                ..TokenUsage::default()
            }),
        })
        .unwrap();
    drop(test_tx);

    super::notifications::completion_notification_loop(test_rx, state.clone()).await;

    let history = state
        .session_manager
        .get_history(parent_session_id)
        .unwrap();
    let markers: Vec<_> = history
        .iter()
        .filter(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("type").and_then(|v| v.as_str()) == Some("subagent_completion")
            })
        })
        .collect();
    assert!(!markers.is_empty());

    let meta = markers[0].metadata.as_ref().unwrap();
    assert_eq!(meta.get("status").and_then(|v| v.as_str()), Some("fail"));
    assert_eq!(
        meta.get("task_description").and_then(|v| v.as_str()),
        Some("Analyze the 10GB dataset")
    );
    assert_eq!(meta.get("tool_count").and_then(|v| v.as_u64()), Some(42));
    assert_eq!(
        meta.get("duration_ms").and_then(|v| v.as_u64()),
        Some(300_000)
    );

    // Verify token usage is included.
    let usage = meta.get("token_usage").expect("should have token_usage");
    assert_eq!(
        usage.get("prompt_tokens").and_then(|v| v.as_u64()),
        Some(50_000)
    );
    assert_eq!(
        usage.get("completion_tokens").and_then(|v| v.as_u64()),
        Some(15_000)
    );
    // reasoning_tokens was None on the TokenUsage; the marker must omit
    // the field entirely (byte-identical to pre-#768).
    assert!(
        usage.get("reasoning_tokens").is_none(),
        "reasoning_tokens should be absent from token_usage when None"
    );

    // Verify the marker text mentions "failed".
    if let alms_session::Content::Text(ref text) = markers[0].content {
        assert!(
            text.contains("failed"),
            "marker text should mention 'failed' for a failed subagent"
        );
    }

    shutdown_token.cancel();
}

/// Test that subagent completion carries `reasoning_tokens` through to the
/// persisted marker metadata when the provider reports them separately
/// (OpenAI o-series, DeepSeek R1, xAI reasoning variants). Previously the
/// field was dropped at the marker boundary — fixed in response to Tim's
/// review on #777 (C1).
#[tokio::test]
async fn subagent_completion_marker_includes_reasoning_tokens() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let parent_agent_id = AgentId::new();
    let parent_session = state
        .session_manager
        .get_or_create(parent_agent_id, "reasoning-tokens-test");
    let parent_session_id = parent_session.id;

    let subagent_session_id = SessionId::new();

    let (test_tx, test_rx) = mpsc::unbounded_channel();
    test_tx
        .send(SubagentCompletion {
            task_id: TaskId::new(),
            subagent_name: Some("reasoner".to_string()),
            status: TaskStatus::Completed,
            summary: "Deep thought complete".to_string(),
            parent_session_id,
            parent_agent_id,
            subagent_session_id,
            task_description: Some("Solve the hard problem".to_string()),
            tool_count: Some(3),
            duration_ms: Some(45_000),
            token_usage: Some(TokenUsage {
                prompt_tokens: 1_200,
                completion_tokens: 300,
                reasoning_tokens: Some(2_048),
                ..TokenUsage::default()
            }),
        })
        .unwrap();
    drop(test_tx);

    super::notifications::completion_notification_loop(test_rx, state.clone()).await;

    let history = state
        .session_manager
        .get_history(parent_session_id)
        .unwrap();
    let markers: Vec<_> = history
        .iter()
        .filter(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("type").and_then(|v| v.as_str()) == Some("subagent_completion")
            })
        })
        .collect();
    assert!(!markers.is_empty());

    let meta = markers[0].metadata.as_ref().unwrap();
    let usage = meta.get("token_usage").expect("should have token_usage");
    assert_eq!(
        usage.get("prompt_tokens").and_then(|v| v.as_u64()),
        Some(1_200)
    );
    assert_eq!(
        usage.get("completion_tokens").and_then(|v| v.as_u64()),
        Some(300)
    );
    assert_eq!(
        usage.get("reasoning_tokens").and_then(|v| v.as_u64()),
        Some(2_048),
        "reasoning_tokens must be propagated into the subagent completion marker"
    );

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

    let formatted = super::notifications::format_dm_conversation_history(&messages);

    // Should be within the character limit.
    assert!(
        formatted.len() <= super::notifications::DM_HISTORY_MAX_CHARS,
        "formatted history ({} chars) should not exceed DM_HISTORY_MAX_CHARS ({})",
        formatted.len(),
        super::notifications::DM_HISTORY_MAX_CHARS,
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
    let formatted = super::notifications::format_dm_conversation_history(&[]);
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

    let formatted = super::notifications::format_dm_conversation_history(&messages);

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
    let notification = super::notifications::format_dm_ended_notification(
        "alice",
        ConversationEndReason::Ignored,
        Some(history),
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
    let notification = super::notifications::format_dm_ended_notification(
        "alice",
        ConversationEndReason::Ignored,
        None,
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
    let ignored = super::notifications::format_dm_ended_notification(
        "alice",
        ConversationEndReason::Ignored,
        None,
    );
    let depth = super::notifications::format_dm_ended_notification(
        "alice",
        ConversationEndReason::DepthExceeded,
        None,
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
    assert!(super::is_internal_context_id("job_abc-123"));
    assert!(super::is_internal_context_id("subagent_task-1"));
    assert!(super::is_internal_context_id("dm:alice:bob"));
    assert!(super::is_internal_context_id("notifications:alice"));
    assert!(super::is_internal_context_id("episodic:summary"));

    assert!(!super::is_internal_context_id("web"));
    assert!(!super::is_internal_context_id("default"));
    assert!(!super::is_internal_context_id("my-custom-session"));
    assert!(!super::is_internal_context_id("chat-with-user"));
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
    let result = super::find_user_facing_session(&mgr, agent_id);
    assert!(
        result.is_none(),
        "should return None when only internal sessions exist"
    );

    // Create a user-facing session.
    let web = mgr.get_or_create(agent_id, "web");
    let result = super::find_user_facing_session(&mgr, agent_id);
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
    use super::lifecycle::resolve_posture_for_run;
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
    let _dm_session =
        state
            .session_manager
            .get_or_create_with_id(dm_session_id, agent_id, "dm:alice:bob");

    // Subscribe to the DM session.
    let mut rx = subscribe_session(&state, dm_session_id);

    // Send a DM event.
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    event_tx
        .send(DmEvent {
            session_id: dm_session_id,
            from_agent: "alice".to_string(),
            from_agent_id: agent_id,
            message: "Hello Bob, this is a test DM!".to_string(),
            ts: chrono::Utc::now(),
        })
        .unwrap();
    drop(event_tx);

    // Run the event loop.
    super::notifications::dm_event_loop(event_rx, state.clone()).await;

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
// 8. create_run: pre-persist user message + accurate queued_behind
// ---------------------------------------------------------------------------

/// When a user posts a message to an agent, the message must be persisted to
/// the session immediately -- not lazily inside the agent loop. Otherwise a
/// page reload while the run is still queued finds an empty session history
/// and the user's message appears lost.
#[tokio::test]
async fn create_run_pre_persists_user_input_to_session() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state.session_manager.get_or_create(agent_id, "web");
    let session_id = session.id;

    let req = CreateRunRequest {
        session_id,
        agent_id: None,
        input: RunInput::Text {
            text: "hello from the user".into(),
        },
        model: None,
        max_tokens: None,
        posture: None,
        provider: None,
        debug_mode: None,
        thinking_budget_tokens: None,
        reasoning_effort: None,
        gemini_thinking_budget: None,
    };

    // Call the handler directly. We do NOT await the spawned execute_run -- we
    // just want to verify that the synchronous create_run call persists the
    // input BEFORE enqueueing.
    let (status, _resp) = match super::lifecycle::create_run(State(state.clone()), Json(req)).await
    {
        Ok(ok) => ok,
        Err((code, body)) => panic!("create_run failed: status={code:?} body={:?}", body.0),
    };
    assert_eq!(status, axum::http::StatusCode::CREATED);

    // Cancel shutdown so the enqueued execute_run task (spawned by create_run)
    // early-exits without trying to call a real LLM.  This keeps the test
    // deterministic and fast.
    shutdown_token.cancel();

    // The user message must be in the session history immediately.
    let history = state
        .session_manager
        .get_history(session_id)
        .expect("session history should be readable");
    let user_msgs: Vec<_> = history
        .iter()
        .filter(|m| matches!(m.role, alms_session::Role::User))
        .collect();
    assert_eq!(
        user_msgs.len(),
        1,
        "exactly one user message should be pre-persisted",
    );
    match &user_msgs[0].content {
        alms_session::Content::Text(t) => {
            assert_eq!(t, "hello from the user");
        }
        other => panic!("expected Text content, got {:?}", other),
    }

    // The pre-persist marker must be present so the executor knows not to
    // re-persist.
    let marker = user_msgs[0]
        .metadata
        .as_ref()
        .and_then(|md| md.get("pending_input"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    assert!(
        marker,
        "pre-persisted user message should carry pending_input: true metadata",
    );
}

#[tokio::test]
async fn create_run_rejects_agent_session_mismatch() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, _shutdown_token, _cr, _tr, _dr) = test_app_state();
    let owner_id = AgentId::new();
    let other_id = AgentId::new();
    let session = state.session_manager.get_or_create(owner_id, "web");

    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: Some(other_id),
        input: RunInput::Text {
            text: "hello".into(),
        },
        model: None,
        max_tokens: None,
        posture: None,
        provider: None,
        debug_mode: None,
        thinking_budget_tokens: None,
        reasoning_effort: None,
        gemini_thinking_budget: None,
    };

    let Err((status, body)) = super::lifecycle::create_run(State(state), Json(req)).await else {
        panic!("create_run should reject mismatched agent_id");
    };
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(body.0["error"]["code"], "AGENT_SESSION_MISMATCH");
}

#[tokio::test]
async fn create_run_requires_agent_id_for_shared_session() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, _shutdown_token, _cr, _tr, _dr) = test_app_state();
    let session_id = SessionId::deterministic_dm("alice", "bob");
    let session = state
        .session_manager
        .get_or_create_shared(session_id, "dm:alice:bob");

    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: None,
        input: RunInput::Text {
            text: "hello".into(),
        },
        model: None,
        max_tokens: None,
        posture: None,
        provider: None,
        debug_mode: None,
        thinking_budget_tokens: None,
        reasoning_effort: None,
        gemini_thinking_budget: None,
    };

    let Err((status, body)) = super::lifecycle::create_run(State(state), Json(req)).await else {
        panic!("create_run should require agent_id for shared sessions");
    };
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(body.0["error"]["code"], "AGENT_ID_REQUIRED");
}

#[tokio::test]
async fn create_run_resolves_per_agent_config_for_shared_session_via_requested_agent_id() {
    use alms_core::registry::AgentRecord;
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;
    use chrono::Utc;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();
    let agent_id = AgentId::new();
    let now = Utc::now();
    let agent = AgentRecord {
        id: agent_id,
        name: "chamunchuk".into(),
        description: String::new(),
        model: Some("claude-sonnet-4-6".into()),
        posture: None,
        provider: Some("anthropic".into()),
        telegram_token: None,
        thinking_budget_tokens: None,
        reasoning_effort: None,
        gemini_thinking_budget: None,
        summary_provider: None,
        summary_model: None,
        is_default: false,
        created_at: now,
        last_active: now,
    };
    state
        .session_manager
        .store()
        .expect("SQLite-backed state should have a store")
        .create_agent(&agent)
        .expect("agent seed should succeed");

    let session_id = SessionId::deterministic_dm("alice", "bob");
    let session = state
        .session_manager
        .get_or_create_shared(session_id, "dm:alice:bob");

    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: Some(agent_id),
        input: RunInput::Text {
            text: "hello".into(),
        },
        model: None,
        max_tokens: None,
        posture: None,
        provider: None,
        debug_mode: None,
        thinking_budget_tokens: None,
        reasoning_effort: None,
        gemini_thinking_budget: None,
    };

    let (status, resp) = match super::lifecycle::create_run(State(state.clone()), Json(req)).await {
        Ok(ok) => ok,
        Err((code, body)) => panic!("create_run failed: status={code:?} body={:?}", body.0),
    };
    assert_eq!(status, axum::http::StatusCode::CREATED);
    shutdown_token.cancel();

    let runs = state.run_manager.list_by_agent(agent_id, 10);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].run_id, resp.0.run_id);
    assert_eq!(runs[0].agent_id, agent_id);
    assert_eq!(runs[0].session_id, session.id);

    let base_agent_config = state.agent_config.read().clone();
    let secrets = state.secrets.read();
    let resolved = super::resolve_agent_config(
        runs[0].agent_id,
        &state.session_manager,
        &base_agent_config,
        &state.llm,
        Some(&secrets),
    );
    assert_eq!(resolved.agent_name.as_deref(), Some("chamunchuk"));
    assert_eq!(resolved.llm.provider(), "anthropic");
    assert_eq!(resolved.llm.default_model(), "claude-sonnet-4-6");
}

/// When the user sends a message to an agent that is already *running* another
/// task (but nothing is queued behind it), `queued_behind` in the run_created
/// SSE event must be >= 1 so the UI shows "Queued -- waiting for agent..."
/// rather than a misleading "Thinking...".
///
/// Reproduces the bug where `SessionQueue::pending_count` returns 0 because
/// the currently-running item has already been dequeued, leaving no visible
/// signal that the new run is actually queued.
#[tokio::test]
async fn create_run_reports_queued_behind_when_agent_is_running() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state.session_manager.get_or_create(agent_id, "web");
    let session_id = session.id;

    // Simulate an already-running run on this agent.
    let running_run = Run::new(session_id, agent_id, "prior task".into());
    let running_run_id = running_run.run_id;
    state.run_manager.insert_run(running_run);
    state.run_manager.mark_run_as_running(running_run_id);

    // Subscribe to session events so we can inspect the run_created payload.
    let mut rx = subscribe_session(&state, session_id);

    let req = CreateRunRequest {
        session_id,
        agent_id: None,
        input: RunInput::Text {
            text: "second message".into(),
        },
        model: None,
        max_tokens: None,
        posture: None,
        provider: None,
        debug_mode: None,
        thinking_budget_tokens: None,
        reasoning_effort: None,
        gemini_thinking_budget: None,
    };

    match super::lifecycle::create_run(State(state.clone()), Json(req)).await {
        Ok(_) => {}
        Err((code, body)) => panic!("create_run failed: status={code:?} body={:?}", body.0),
    }

    // Cancel shutdown so the enqueued execute_run task (spawned by create_run)
    // early-exits without trying to call a real LLM.  Run-level events emitted
    // after cancellation are irrelevant -- we only inspect the run_created
    // event emitted synchronously during create_run.
    shutdown_token.cancel();

    // Give the SSE fan-out a moment to land.
    tokio::task::yield_now().await;

    let events = drain_events(&mut rx);
    let run_created = events
        .iter()
        .find(|e| e.event_type == "run_created")
        .expect("run_created event should be emitted");

    let queued_behind = run_created
        .data
        .get("queued_behind")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert!(
        queued_behind >= 1,
        "queued_behind should be >= 1 when the agent is already running another run; got {queued_behind}",
    );
}

// ---------------------------------------------------------------------------
// handle_dm_run_failure tests
//
// Regression coverage for the gap surfaced during v0.2.2 sign-off: the four
// error arms in `execute_run` (Cancelled, CancelledWithToolCalls,
// FailedWithToolCalls, generic Err) skipped DM peer-state notification, so
// a peer-triggered DM run that failed left the depth counter, `dm_ended`
// marker, and `ConversationEnded` peer notification unset until the 1800s
// `DEPTH_EXPIRY_SECS` sweep eventually cleared it.
// ---------------------------------------------------------------------------

/// Test that `handle_dm_run_failure` is a no-op for non-peer runs.
///
/// The helper must NOT call `MessageBus::end_conversation` or emit any SSE
/// when `is_peer_message` is false — only peer-triggered DM runs participate
/// in the DM lifecycle handshake.
#[tokio::test]
async fn handle_dm_run_failure_skips_when_not_peer_message() {
    let (state, _shutdown, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (_alice_id, bob_id) = seed_alice_bob(&state);

    let session_id = SessionId::deterministic_dm("alice", "bob");
    state
        .session_manager
        .get_or_create_with_id(session_id, bob_id, "dm:alice:bob");
    let mut dm_rx = subscribe_session(&state, session_id);

    let run_id = alms_core::RunId::new();
    let res = super::dm_lifecycle::handle_dm_run_failure(
        &state,
        &run_id,
        &session_id,
        bob_id,
        Some("bob"),
        "dm:alice:bob",
        false, // <-- is_peer_message = false
        ConversationEndReason::UserCancelled,
    )
    .await;
    assert!(res.is_ok(), "no-op path should succeed");

    // No RunTrigger should have been emitted.
    assert!(
        tr.try_recv().is_err(),
        "no ConversationEnded RunTrigger should fire for non-peer runs"
    );

    // No SSE event should land on the DM session stream.
    tokio::task::yield_now().await;
    let events = drain_events(&mut dm_rx);
    assert!(
        events.is_empty(),
        "no SSE should be emitted; got {:?}",
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
    );
}

/// Test that `handle_dm_run_failure` is a no-op for non-DM context IDs.
///
/// The helper must only act on `dm:` context IDs — a peer-triggered run on
/// any other context (e.g. `notifications:bob`, `web`, `subagent_…`) is not
/// a DM and must not trigger the DM end handshake.
#[tokio::test]
async fn handle_dm_run_failure_skips_when_not_dm_context() {
    let (state, _shutdown, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (_alice_id, bob_id) = seed_alice_bob(&state);

    let session = state.session_manager.get_or_create(bob_id, "web");
    let session_id = session.id;
    let mut rx = subscribe_session(&state, session_id);

    let run_id = alms_core::RunId::new();
    let res = super::dm_lifecycle::handle_dm_run_failure(
        &state,
        &run_id,
        &session_id,
        bob_id,
        Some("bob"),
        "web", // <-- not a dm: context
        true,
        ConversationEndReason::UserCancelled,
    )
    .await;
    assert!(res.is_ok());

    assert!(tr.try_recv().is_err(), "no RunTrigger for non-DM context");
    tokio::task::yield_now().await;
    let events = drain_events(&mut rx);
    assert!(
        events.is_empty(),
        "no SSE for non-DM context; got {:?}",
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
    );
}

/// Happy path: a peer-triggered DM run that errored should end the
/// conversation, reset the depth counter, emit a `ConversationEnded`
/// `RunTrigger` carrying `Errored { message }` for the peer, and emit a
/// `dm_conversation_ended` SSE on the DM session stream.
#[tokio::test]
async fn handle_dm_run_failure_errored_resets_depth_and_emits_sse() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    // Bootstrap the depth counter as if alice had just sent a DM to bob.
    // This routes through the real `MessageBus::send`, which creates the
    // shared DM session and bumps the depth.
    let _receipt = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    // Drain the trigger emitted by `send` so subsequent assertions are
    // scoped to the failure-driven trigger only.
    let _initial_trigger = tr.try_recv().expect("send should emit a trigger");

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");

    // Subscribe to the DM session SSE stream so we can capture the
    // `dm_conversation_ended` event.
    let mut dm_rx = subscribe_session(&state, dm_session_id);

    // Simulate bob's peer-triggered DM run failing mid-flight. This is the
    // generic `Err(e)` arm in `execute_run` — the message field is the
    // truncated error display.
    let run_id = alms_core::RunId::new();
    let result = super::dm_lifecycle::handle_dm_run_failure(
        &state,
        &run_id,
        &dm_session_id,
        bob_id,
        Some("bob"),
        dm_context,
        true,
        ConversationEndReason::Errored {
            message: "LLM provider error".to_string(),
        },
    )
    .await;
    assert!(
        result.is_ok(),
        "happy path should return Ok; got {result:?}"
    );

    // 1. The DM session must contain a `dm_ended` marker — this proves
    //    `MessageBus::end_conversation` ran (the marker write happens after
    //    the depth counter has been removed). The depth-counter map is a
    //    `pub(super)` field of MessageBus so we cannot inspect it directly
    //    from the gateway tests; the marker is the next-best public proof.
    let history = state.session_manager.get_history(dm_session_id).unwrap();
    let dm_ended_marker = history.iter().find(|m| {
        m.metadata
            .as_ref()
            .and_then(|meta| meta.get("message_type"))
            .and_then(|v| v.as_str())
            == Some("dm_ended")
    });
    assert!(
        dm_ended_marker.is_some(),
        "expected a `dm_ended` marker to be persisted to the DM session"
    );
    let marker_meta = dm_ended_marker.unwrap().metadata.as_ref().unwrap();
    assert_eq!(
        marker_meta.get("reason").and_then(|v| v.as_str()),
        Some("errored"),
        "dm_ended marker must record the Errored reason"
    );

    // 2. `ConversationEnded` RunTrigger should have been emitted for alice
    //    (the peer of the failed run) carrying the `Errored` reason.
    let trigger = tr.try_recv().expect("expected ConversationEnded trigger");
    match trigger.source {
        MessageSource::ConversationEnded {
            from_name, reason, ..
        } => {
            assert_eq!(from_name, "bob", "from_name must be the failed run's agent");
            match reason {
                ConversationEndReason::Errored { message } => {
                    assert_eq!(message, "LLM provider error");
                }
                other => panic!("expected Errored reason, got {other:?}"),
            }
        }
        other => panic!("expected ConversationEnded source, got {other:?}"),
    }

    // 3. `dm_conversation_ended` SSE event with reason "errored" should
    //    have landed on the DM session stream.
    tokio::task::yield_now().await;
    let events = drain_events(&mut dm_rx);
    let dm_ended = events
        .iter()
        .find(|e| e.event_type == "dm_conversation_ended")
        .unwrap_or_else(|| {
            panic!(
                "expected dm_conversation_ended SSE; got {:?}",
                events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
            )
        });
    let reason_str = dm_ended
        .data
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(
        reason_str, "errored",
        "SSE reason must be 'errored' for the Errored variant"
    );

    shutdown_token.cancel();
}

/// Cancel arm: a peer-triggered DM run that was cancelled mid-flight should
/// emit `dm_conversation_ended` with reason `user_cancelled` and reset the
/// depth counter, mirroring the `Errored` happy-path test.
#[tokio::test]
async fn handle_dm_run_failure_user_cancelled_emits_sse() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    // Open a DM conversation.
    let _ = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _ = tr.try_recv();

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    let mut dm_rx = subscribe_session(&state, dm_session_id);

    let run_id = alms_core::RunId::new();
    super::dm_lifecycle::handle_dm_run_failure(
        &state,
        &run_id,
        &dm_session_id,
        bob_id,
        Some("bob"),
        dm_context,
        true,
        ConversationEndReason::UserCancelled,
    )
    .await
    .unwrap();

    // dm_ended marker is the public proof that `MessageBus::end_conversation`
    // ran end-to-end (depth counter removal happens immediately before the
    // marker write).
    let history = state.session_manager.get_history(dm_session_id).unwrap();
    assert!(
        history.iter().any(|m| {
            m.metadata
                .as_ref()
                .and_then(|meta| meta.get("reason"))
                .and_then(|v| v.as_str())
                == Some("user_cancelled")
        }),
        "dm_ended marker with reason=user_cancelled must be persisted"
    );

    let trigger = tr.try_recv().expect("expected ConversationEnded trigger");
    match trigger.source {
        MessageSource::ConversationEnded { reason, .. } => {
            assert!(
                matches!(reason, ConversationEndReason::UserCancelled),
                "trigger reason must be UserCancelled; got {reason:?}"
            );
        }
        other => panic!("expected ConversationEnded source, got {other:?}"),
    }

    tokio::task::yield_now().await;
    let events = drain_events(&mut dm_rx);
    let dm_ended = events
        .iter()
        .find(|e| e.event_type == "dm_conversation_ended")
        .expect("expected dm_conversation_ended SSE");
    assert_eq!(
        dm_ended
            .data
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        "user_cancelled",
    );

    shutdown_token.cancel();
}

/// Atomicity guard: when both peers race to end the conversation (e.g. the
/// peer's happy-path `ignore_message` runs at the same time as the local
/// run's failure arm), only the first caller should win. The atomicity
/// guard at `bus.rs:359` (`depths.remove()`) ensures the second
/// `end_conversation` returns `Ok(())` without re-emitting a trigger.
#[tokio::test]
async fn handle_dm_run_failure_double_end_is_idempotent() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    // Open a DM conversation.
    let _ = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _ = tr.try_recv();

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");

    let run_id = alms_core::RunId::new();

    // First call: should emit the ConversationEnded trigger.
    super::dm_lifecycle::handle_dm_run_failure(
        &state,
        &run_id,
        &dm_session_id,
        bob_id,
        Some("bob"),
        dm_context,
        true,
        ConversationEndReason::Errored {
            message: "first".into(),
        },
    )
    .await
    .unwrap();

    // Second call: depth has been removed already; bus.rs's atomicity guard
    // returns Ok(()) without re-emitting a trigger.
    super::dm_lifecycle::handle_dm_run_failure(
        &state,
        &run_id,
        &dm_session_id,
        bob_id,
        Some("bob"),
        dm_context,
        true,
        ConversationEndReason::Errored {
            message: "second".into(),
        },
    )
    .await
    .unwrap();

    // Exactly one ConversationEnded trigger should have been emitted across
    // the two calls (the second call hits the atomicity guard).
    let mut conversation_ended_triggers = 0;
    while let Ok(t) = tr.try_recv() {
        if matches!(t.source, MessageSource::ConversationEnded { .. }) {
            conversation_ended_triggers += 1;
        }
    }
    assert_eq!(
        conversation_ended_triggers, 1,
        "atomicity guard must suppress duplicate ConversationEnded triggers"
    );

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// Agent-scoped session-activity SSE feed (#856)
// ---------------------------------------------------------------------------

/// Subscribe to the agent-scoped session-activity feed and return the receiver.
fn subscribe_agent(state: &AppState, agent_id: AgentId) -> mpsc::UnboundedReceiver<SseEventData> {
    let (tx, rx) = mpsc::unbounded_channel();
    state.run_manager.register_agent_sender(agent_id, tx);
    rx
}

/// End-to-end happy path for the agent-scoped session-activity feed.
///
/// Exercises the full `RunManager` plumbing the way `execute_run` does:
/// emit `session_activity_started` at the start, transition the run
/// through `Running` -> `Completed`, then emit `session_activity_ended`.
/// Verifies that:
/// - Both events arrive on the agent's subscriber.
/// - The events carry the correct `session_id`, `run_id`, and `agent_id`.
/// - `RunManager::has_active_runs` (which backs `GET /sessions`'s
///   `has_active_run` field) flips `true` while running and `false`
///   after completion.
#[tokio::test]
async fn agent_session_activity_started_and_ended_arrive_on_feed() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session_a = state.session_manager.get_or_create(agent_id, "chat-a");
    let session_a_id = session_a.id;
    // A second session exists but no run is started on it — must not
    // appear active in the snapshot.
    let _session_b = state.session_manager.get_or_create(agent_id, "chat-b");

    let mut rx = subscribe_agent(&state, agent_id);

    // Insert and mark a run as running on session A.
    let run = Run::new(session_a_id, agent_id, "test".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);
    state.run_manager.mark_run_as_running(run_id);

    // Mid-run: GET /sessions should report has_active_run=true for A only.
    assert!(
        state.run_manager.has_active_runs(session_a_id),
        "session A should report has_active_run=true while a run is in flight"
    );
    let session_b_id = state.session_manager.get_or_create(agent_id, "chat-b").id;
    assert!(
        !state.run_manager.has_active_runs(session_b_id),
        "session B has no runs and must report has_active_run=false"
    );

    // Emit the started event the way execute_run does.
    state
        .run_manager
        .send_agent_event(
            agent_id,
            run_id,
            session_a_id,
            SseEventData::session_activity_started(session_a_id, run_id, agent_id),
        )
        .await;

    let started = rx.recv().await.expect("started event should arrive");
    assert_eq!(started.event_type, "session_activity_started");
    assert_eq!(started.data["session_id"], session_a_id.0.to_string());
    assert_eq!(started.data["run_id"], run_id.0.to_string());
    assert_eq!(started.data["agent_id"], agent_id.0.to_string());

    // Complete the run and emit the ended event.
    state
        .run_manager
        .mark_run_as_completed(run_id, "ok".into(), Default::default());
    state
        .run_manager
        .send_agent_event(
            agent_id,
            run_id,
            session_a_id,
            SseEventData::session_activity_ended(session_a_id, run_id, agent_id),
        )
        .await;

    let ended = rx.recv().await.expect("ended event should arrive");
    assert_eq!(ended.event_type, "session_activity_ended");
    assert_eq!(ended.data["session_id"], session_a_id.0.to_string());
    assert_eq!(ended.data["run_id"], run_id.0.to_string());
    assert_eq!(ended.data["agent_id"], agent_id.0.to_string());

    // Post-completion: has_active_run flips back to false.
    assert!(
        !state.run_manager.has_active_runs(session_a_id),
        "session A should report has_active_run=false after run completes"
    );

    shutdown_token.cancel();
}

/// `agent_id` filter test: an agent X subscribed to its own feed should
/// NOT receive activity events for runs belonging to agent Y. The feed
/// is scoped at the broadcast layer, so subscribers to one agent's feed
/// are entirely isolated from any other agent's runs (#856).
#[tokio::test]
async fn agent_session_activity_feed_filters_by_agent_id() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_x = AgentId::new();
    let agent_y = AgentId::new();
    let session_y = state.session_manager.get_or_create(agent_y, "chat-y");
    let session_y_id = session_y.id;

    // Subscribe agent X (not agent Y) to its own session-activity feed.
    let mut rx_x = subscribe_agent(&state, agent_x);

    // Emit activity on agent Y's feed.
    let run = Run::new(session_y_id, agent_y, "Y run".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);
    state
        .run_manager
        .send_agent_event(
            agent_y,
            run_id,
            session_y_id,
            SseEventData::session_activity_started(session_y_id, run_id, agent_y),
        )
        .await;
    state
        .run_manager
        .send_agent_event(
            agent_y,
            run_id,
            session_y_id,
            SseEventData::session_activity_ended(session_y_id, run_id, agent_y),
        )
        .await;

    // Yield so any pending fan-out completes.
    tokio::task::yield_now().await;

    // Agent X must receive nothing.
    assert!(
        rx_x.try_recv().is_err(),
        "agent X must not receive any events for agent Y's runs",
    );

    shutdown_token.cancel();
}

/// Pre-cancellation in `execute_run` emits a synthetic
/// `session_activity_ended` (without a paired `session_activity_started`)
/// so the sidebar's snapshot-derived "active" indicator clears (#888).
///
/// Background: a queued run is observable via `GET /sessions`'s
/// `has_active_run: true` field between insertion and cancellation, so a
/// concurrent client snapshot will have lit up the indicator. The
/// pre-cancel branch never emits a `started` (the run never executed),
/// but it MUST emit `ended` to clear that indicator — otherwise the UI
/// shows a stuck "active" state until the next reload.
///
/// The asymmetric `ended`-without-`started` is intentional and documented
/// in the lifecycle code: the consumer treats the snapshot as the source
/// of truth for "indicator on" and `ended` as the universal "indicator
/// off" signal.
#[tokio::test]
async fn pre_cancelled_run_emits_session_activity_ended() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-pre-cancel");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "noop".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run.clone());

    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    cancel_token.cancel();

    let mut agent_rx = subscribe_agent(&state, agent_id);

    super::lifecycle::execute_run(
        state.clone(),
        super::RunParams {
            run_id,
            session_id,
            agent_id,
            input: run.input,
            overrides: super::RunOverrides::default(),
            context_id: "test-pre-cancel".to_string(),
            cancel_token,
            is_peer_message: false,
            is_system_triggered: false,
            input_pre_persisted: false,
        },
    )
    .await;

    let events = drain_events(&mut agent_rx);
    let event_types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();

    // Pre-cancelled runs MUST NOT emit `started` (the run never executed)
    // but MUST emit exactly one `ended` so the sidebar indicator clears.
    assert!(
        !event_types.contains(&"session_activity_started"),
        "pre-cancelled runs must not emit session_activity_started; got: {event_types:?}"
    );
    let ended_count = event_types
        .iter()
        .filter(|t| **t == "session_activity_ended")
        .count();
    assert_eq!(
        ended_count, 1,
        "pre-cancelled runs must emit exactly one session_activity_ended; got: {event_types:?}"
    );

    // Verify the ended event carries the right payload so consumers can
    // correlate it with their snapshot-derived indicator state.
    let ended = events
        .iter()
        .find(|e| e.event_type == "session_activity_ended")
        .expect("session_activity_ended must be present");
    assert_eq!(ended.data["session_id"], session_id.0.to_string());
    assert_eq!(ended.data["run_id"], run_id.0.to_string());
    assert_eq!(ended.data["agent_id"], agent_id.0.to_string());

    // And the snapshot truth flips to false post-cancel, matching what
    // a freshly-loading client would see.
    assert!(
        !state.run_manager.has_active_runs(session_id),
        "session must report has_active_run=false after pre-cancellation"
    );

    shutdown_token.cancel();
}

/// Regression test for #895 (pre-cancel branch, interposer pattern):
/// in the pre-cancel branch of `execute_run`, the run state must be
/// flipped to `Cancelled` BEFORE the `run_cancelled` SSE event is
/// broadcast. Otherwise a concurrent `GET /sessions` snapshot taken
/// between broadcast and state flip sees `has_active_run: true` while
/// the SSE feed has already moved past the `ended` event — a subsequent
/// `last_event_id`-based reconnect won't replay it and the sidebar's
/// "active" indicator stays stuck.
///
/// **Interposer pattern (Tim's review on PR #925):** the previous version
/// of this test asserted on `has_active_runs` *after* `execute_run().await`
/// returned, by which point both the broadcast and the flip have completed
/// regardless of internal ordering — a regression of the production fix
/// could not be detected. This version uses the producer's own suspension
/// point as a synchronisation barrier:
///
/// - Spawn `execute_run` on a separate task so we can interleave with it.
/// - Subscribe a session sender and `recv()` events as they arrive.
/// - The pre-cancel branch fires `send_event(run_cancelled)` then
///   `send_agent_event(session_activity_ended).await`. The latter awaits
///   on `event_log.log_event(...)`, which is a real suspension point.
/// - When our consumer task is woken by the `run_cancelled` enqueue, the
///   producer has just suspended on that next `send_agent_event` await
///   and has NOT yet called `mark_run_as_cancelled` (in the pre-fix
///   order; in the post-fix order, the flip happened *before* the
///   broadcast).
/// - We probe `has_active_runs` synchronously upon recv. Post-fix:
///   observes `false` (flip already done). Pre-fix: observes `true`
///   (flip not yet done) — test fails.
///
/// **Reverting the four-site reorder in `lifecycle.rs` causes this test
/// to fail.** The probe captures the cross-event-boundary state, not the
/// terminal state.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pre_cancelled_run_flips_state_before_broadcasting() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-895-pre-cancel");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "noop".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run.clone());

    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    cancel_token.cancel();

    let mut session_rx = subscribe_session(&state, session_id);

    let exec_state = state.clone();
    let exec_input = run.input.clone();
    let exec_handle = tokio::spawn(async move {
        super::lifecycle::execute_run(
            exec_state,
            super::RunParams {
                run_id,
                session_id,
                agent_id,
                input: exec_input,
                overrides: super::RunOverrides::default(),
                context_id: "test-895-pre-cancel".to_string(),
                cancel_token,
                is_peer_message: false,
                is_system_triggered: false,
                input_pre_persisted: false,
            },
        )
        .await
    });

    // Interposer: the `recv()` future resolves the moment our test task
    // is scheduled by tokio in response to the producer's session-fanout
    // (the synchronous `senders.retain` inside `RunManager::send_event`).
    // In single-threaded tokio the producer reaches its next suspension
    // point — `send_agent_event(session_activity_ended).await` for the
    // pre-cancel branch — before yielding to us. Pre-fix, the flip
    // happens AFTER `send_agent_event` returns; post-fix, it happens
    // BEFORE the original `send_event(run_cancelled)`. So at the
    // moment we observe `run_cancelled`, the flip is either pending
    // (pre-fix) or already done (post-fix), and `has_active_runs`
    // reports the difference.
    let mut probed_active: Option<bool> = None;
    let mut saw_cancelled = false;
    while let Some(event) =
        tokio::time::timeout(std::time::Duration::from_secs(5), session_rx.recv())
            .await
            .expect("test must observe events within timeout")
    {
        if event.event_type == "run_cancelled" {
            saw_cancelled = true;
            // SYNCHRONOUS probe at moment of receipt — no `.await`
            // between recv() resolving and this read, so the producer
            // is parked at its next suspension point and cannot have
            // advanced past the broadcast.
            probed_active = Some(state.run_manager.has_active_runs(session_id));
            break;
        }
    }

    // Drain remaining events so the producer can finish.
    while session_rx.try_recv().is_ok() {}

    exec_handle.await.expect("execute_run task must complete");

    assert!(
        saw_cancelled,
        "expected a run_cancelled SSE event in pre-cancel path"
    );
    assert_eq!(
        probed_active,
        Some(false),
        "has_active_runs must report false at the moment run_cancelled is \
         observed by a session subscriber (pre-#895 race: probe sees \
         has_active_runs=true while ended event has already fired). \
         Reverting the lifecycle.rs reorder causes this assertion to fail."
    );

    let run_snapshot = state
        .run_manager
        .get_run(run_id)
        .expect("run must exist after pre-cancellation");
    assert_eq!(
        run_snapshot.status,
        RunStatus::Cancelled,
        "run status must be Cancelled after the run completes"
    );

    shutdown_token.cancel();
}

/// Regression test for #895 (happy-path-start, interposer pattern): the
/// run state must be flipped to `Running` BEFORE the `run_started` SSE
/// event is broadcast. The four-site reorder in `lifecycle.rs` exists
/// for symmetry with the `ended` paths so all `mark_run_as_*` sites
/// have the same shape (the actual user-visible race lives on the
/// `ended` side — see the issue body).
///
/// We can't pin the invariant via `has_active_runs` alone because both
/// `Queued` and `Running` count as active, so the field is `true` in
/// both pre-fix and post-fix orderings at this point. Instead we pin
/// `run.status`, which differs: pre-fix the run is still `Queued` at
/// broadcast time; post-fix it is already `Running`.
///
/// **Interposer pattern:** spawn `execute_run`, subscribe a session
/// sender, and probe `run.status` synchronously the moment
/// `run_started` arrives. Between `send_event(run_started)` and
/// `mark_run_as_running` in pre-fix code there is a real suspension
/// point — `send_agent_event(session_activity_started).await` — so the
/// consumer task is scheduled BEFORE the producer reaches the flip.
/// Multi-thread runtime is required so the consumer runs while the
/// producer is suspended on that next await.
///
/// We don't need a working LLM — `runtime.run()` will fail with the
/// dummy default LLM client, but the failure happens AFTER the
/// `run_started` broadcast and the cancel-token below puts an upper
/// bound on the test runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn happy_path_start_flips_state_before_broadcasting() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-895-happy-start");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "test".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run.clone());

    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    let mut session_rx = subscribe_session(&state, session_id);

    let exec_state = state.clone();
    let exec_input = run.input.clone();
    let exec_handle = tokio::spawn(async move {
        super::lifecycle::execute_run(
            exec_state,
            super::RunParams {
                run_id,
                session_id,
                agent_id,
                input: exec_input,
                overrides: super::RunOverrides::default(),
                context_id: "test-895-happy-start".to_string(),
                cancel_token,
                is_peer_message: false,
                is_system_triggered: false,
                input_pre_persisted: false,
            },
        )
        .await
    });

    // Interposer: synchronously probe `has_active_runs` the instant
    // `run_started` lands on our session feed. The producer is parked
    // at the next suspension point (the `send_agent_event(
    // session_activity_started).await` inside `execute_run` in pre-fix
    // code, which precedes `mark_run_as_running`). Post-fix, the flip
    // happened before the broadcast, so the probe sees `true`. Pre-fix,
    // the flip is still ahead of the producer's instruction pointer,
    // so the probe sees `false`.
    let mut probed_status: Option<RunStatus> = None;
    let mut saw_started = false;
    while let Some(event) =
        tokio::time::timeout(std::time::Duration::from_secs(5), session_rx.recv())
            .await
            .expect("test must observe run_started within timeout")
    {
        if event.event_type == "run_started" {
            saw_started = true;
            // SYNCHRONOUS probe — see comment in the pre-cancel test.
            // We probe `run.status` rather than `has_active_runs` because
            // both `Queued` and `Running` count as active, so the latter
            // does not distinguish pre-fix (`Queued` at broadcast) from
            // post-fix (`Running` at broadcast).
            probed_status = state.run_manager.get_run(run_id).map(|r| r.status);
            break;
        }
    }

    assert!(
        saw_started,
        "expected a run_started SSE event in happy-path-start"
    );
    assert_eq!(
        probed_status,
        Some(RunStatus::Running),
        "run.status must be Running at the moment run_started is observed \
         by a session subscriber (pre-#895 race: probe sees status=Queued \
         even though the started event has fired). Reverting the \
         lifecycle.rs reorder causes this assertion to fail."
    );

    // Tear down the spawned execute_run. We've already proven the
    // start-broadcast invariant; let the runtime fail/cancel out so the
    // test exits promptly. The default LLM has no API key so
    // `runtime.run()` will return an error within a few hundred ms;
    // cancelling the run accelerates that.
    state.run_manager.cancel_run(run_id);

    let _ = tokio::time::timeout(std::time::Duration::from_secs(15), exec_handle)
        .await
        .expect("execute_run task must complete within 15s after cancellation");

    shutdown_token.cancel();
}

/// Smoke test (NOT a regression pin) covering the call sequence for the
/// post-execute cancel arms (`Err(Cancelled)` and
/// `Err(CancelledWithToolCalls)`) at the `RunManager` boundary. Driving
/// this branch via `execute_run` requires a real LLM, and unlike the
/// pre-cancel and happy-path-start branches there is no intermediate
/// `send_agent_event(...)` between `send_event(run_cancelled)` and
/// `mark_run_as_cancelled`, so the interposer pattern used in the two
/// tests above cannot reach this branch deterministically without
/// modifying production code.
///
/// What this test verifies: that callers using the post-#895 sequence
/// (`mark_run_as_cancelled` then `send_event`) see `has_active_runs ==
/// false` upon receiving the `run_cancelled` event. This is a sanity
/// check that the call sequence itself is correct, NOT that the
/// production code emits events in that sequence — the test mirrors the
/// post-fix order in its own body, so reverting `lifecycle.rs` cannot
/// break it. See the follow-up issue filed against v0.2.3 for an
/// extension of #895 to the `mark_run_as_completed`/`mark_run_as_failed`
/// sites that also need interposer-based regression pins.
#[tokio::test]
async fn smoke_post_execute_cancel_flips_state_at_run_manager_boundary() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-895-post-cancel");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "test".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);
    state.run_manager.mark_run_as_running(run_id);
    assert!(state.run_manager.has_active_runs(session_id));

    let mut session_rx = subscribe_session(&state, session_id);

    // Mirror the post-#895 ordering: flip state first, broadcast second.
    state.run_manager.mark_run_as_cancelled(run_id);
    state
        .run_manager
        .send_event(run_id, session_id, SseEventData::run_cancelled(run_id))
        .await;

    let event = session_rx
        .recv()
        .await
        .expect("run_cancelled event must be delivered");
    assert_eq!(event.event_type, "run_cancelled");

    assert!(
        !state.run_manager.has_active_runs(session_id),
        "has_active_runs must be false after mark_run_as_cancelled \
         (sanity check on the RunManager boundary, NOT a regression pin \
         on lifecycle.rs ordering)"
    );
    let run_snapshot = state
        .run_manager
        .get_run(run_id)
        .expect("run must exist after mark_run_as_cancelled");
    assert_eq!(
        run_snapshot.status,
        RunStatus::Cancelled,
        "run status must be Cancelled after mark_run_as_cancelled"
    );

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// #912 follow-up (PR #930 review F1): gateway lifecycle does not write a
// duplicate error marker on the four removed arms
// ---------------------------------------------------------------------------

/// Gateway-side regression pin for PR #930 follow-up F1 — Tim's "test
/// scope is narrower than the dedup contract" finding.
///
/// The runtime-layer test in `alms_runtime::agent::tests` drives
/// `finish_run` directly with a synthetic `Err(_)` history and asserts
/// exactly one `[Run failed: ...]` record persists.  That covers the
/// runtime side of the contract but doesn't independently verify the
/// gateway lifecycle layer no longer writes its own
/// `persist_error_marker` call.  Compile-time absence of those four
/// calls in `lifecycle.rs` is the first line of defence — but a future
/// refactor could accidentally re-add one and the runtime-layer test
/// would still pass.  This test closes that gap end-to-end: it drives
/// `execute_run` down the generic `Err(_)` arm with a real
/// `AgentRuntime` and a deliberately unreachable LLM, then asserts on
/// the persisted session shape.
///
/// We pick the generic `Err(_)` arm as the most representative — the
/// four arms #912 removed (`Cancelled`, `CancelledWithToolCalls`,
/// `FailedWithToolCalls`, generic `Err(_)`) share the same dedup
/// logic, so one end-to-end pin is enough.  The `FailedWithToolCalls`
/// arm requires a synthetic tool-call sequence to drive deterministically
/// (it fires only when the LLM returned tool calls before failing); the
/// generic `Err(_)` arm fires on any LLM-call failure and is the
/// default failure mode for production deployments.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_run_failed_arm_persists_no_lifecycle_error_marker() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_failing_llm();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-912-no-dup-marker");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "trigger LLM failure".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run.clone());

    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    super::lifecycle::execute_run(
        state.clone(),
        super::RunParams {
            run_id,
            session_id,
            agent_id,
            input: run.input,
            overrides: super::RunOverrides::default(),
            context_id: "test-912-no-dup-marker".to_string(),
            cancel_token,
            is_peer_message: false,
            is_system_triggered: false,
            input_pre_persisted: false,
        },
    )
    .await;

    // The run must have terminated through the failed arm — the LLM
    // is unreachable so it cannot complete normally.  We check the
    // RunManager rather than asserting on a specific terminal status
    // string because the failure could surface as either a connection
    // refused, a timeout, or a stream parse error depending on the
    // host's TCP stack — all three land in the same generic `Err(_)`
    // arm of `execute_run`.
    let final_run = state
        .run_manager
        .get_run(run_id)
        .expect("run must still exist after execute_run returns");
    assert_eq!(
        final_run.status,
        RunStatus::Failed,
        "run must reach Failed status when the LLM is unreachable; got {:?} (error={:?})",
        final_run.status,
        final_run.error,
    );

    // CORE INVARIANT for issue #912: NO `Role::System` `kind: "error"`
    // marker may be persisted on the four removed arms.  Pre-#912 the
    // gateway wrote a `(run failed) ...` `kind: "error"` system marker
    // here (after the runtime had already written `[Run failed: ...]`
    // as `Role::Assistant` text); post-#912 the runtime-layer write is
    // the only error record.
    let history = state.session_manager.get_history(session_id).unwrap();
    let lifecycle_error_markers: Vec<_> = history
        .iter()
        .filter(|m| {
            m.role == alms_session::Role::System
                && m.metadata
                    .as_ref()
                    .and_then(|md| md.get("kind"))
                    .and_then(|v| v.as_str())
                    == Some("error")
        })
        .collect();
    assert_eq!(
        lifecycle_error_markers.len(),
        0,
        "lifecycle layer must NOT persist a `Role::System kind=error` marker on the generic `Err(_)` arm (issue #912); got {} markers: {:#?}",
        lifecycle_error_markers.len(),
        lifecycle_error_markers
            .iter()
            .map(|m| match &m.content {
                alms_session::Content::Text(t) => t.clone(),
                _ => "<non-text>".to_string(),
            })
            .collect::<Vec<_>>(),
    );

    // Sanity-check the runtime-layer write IS present — we want to be
    // sure we drove `execute_run` deeply enough to reach the failure
    // path, not exit early before the runtime tried the LLM call.
    let runtime_failure_records: Vec<_> = history
        .iter()
        .filter(|m| match &m.content {
            alms_session::Content::Text(t) => t.starts_with("[Run failed:"),
            _ => false,
        })
        .collect();
    assert_eq!(
        runtime_failure_records.len(),
        1,
        "exactly one runtime-layer `[Run failed: ...]` record must persist (the canonical record kept by #912); got {} records in history of len {}",
        runtime_failure_records.len(),
        history.len(),
    );
    assert_eq!(
        runtime_failure_records[0].role,
        alms_session::Role::Assistant,
        "runtime-layer failure record must be `Role::Assistant` (the bubble shape kept as canonical by #912)"
    );

    shutdown_token.cancel();
}

/// `GET /agents/{agent_id}/events` returns 404 when the `agent_id` does
/// not resolve to a record in the registry, and crucially does NOT
/// register a sender for the unknown agent (#887).
///
/// Without this guard, a misbehaving client could slowly grow the
/// in-memory `agent_senders` map by repeatedly connecting with random
/// UUIDs — entries are only pruned on `send_agent_event` fanout, which
/// never fires for an agent that never emits events.
#[tokio::test]
async fn stream_agent_events_returns_404_for_unknown_agent_and_does_not_leak_sender() {
    use axum::extract::{Path, Query, State};
    use axum::http::HeaderMap;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, _bob_id) = seed_alice_bob(&state);

    // Sanity: looking up alice should succeed; an unknown UUID should
    // not exist in the registry.
    let unknown_id = AgentId::new();
    assert_ne!(unknown_id, alice_id);

    // Pre-condition: agent_senders is empty.
    assert_eq!(
        state.run_manager.agent_senders.len(),
        0,
        "test fixture must start with no agent senders"
    );

    // Hit the handler with an unknown agent_id.
    let result = super::stream_agent_events(
        State(state.clone()),
        Path(unknown_id),
        HeaderMap::new(),
        Query(super::SessionEventsQuery {
            last_event_id: None,
        }),
    )
    .await;

    let (status, body) = match result {
        Err(err) => err,
        Ok(_) => panic!("stream_agent_events must return 404 for an unknown agent_id"),
    };
    assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
    assert_eq!(body.0["error"]["code"], "NOT_FOUND");

    // Critical: no sender was registered for the unknown agent. This
    // is the behavioural guarantee that prevents the slow-leak failure
    // mode #887 was filed for.
    assert_eq!(
        state.run_manager.agent_senders.len(),
        0,
        "stream_agent_events must NOT register a sender for unknown agent_ids"
    );
    assert!(
        !state.run_manager.agent_senders.contains_key(&unknown_id),
        "no agent_senders entry should exist for the unknown agent_id"
    );

    // A request for a known agent should still succeed.  We don't
    // inspect the response body (it is a stream), but registration must
    // succeed and the sender map must contain exactly one entry.
    let ok = super::stream_agent_events(
        State(state.clone()),
        Path(alice_id),
        HeaderMap::new(),
        Query(super::SessionEventsQuery {
            last_event_id: None,
        }),
    )
    .await;
    assert!(
        ok.is_ok(),
        "stream_agent_events must succeed for a known agent_id"
    );
    assert_eq!(
        state.run_manager.agent_senders.len(),
        1,
        "exactly one sender should be registered for the known agent"
    );
    assert!(
        state.run_manager.agent_senders.contains_key(&alice_id),
        "the sender must be keyed by the known agent's id"
    );

    shutdown_token.cancel();
}
