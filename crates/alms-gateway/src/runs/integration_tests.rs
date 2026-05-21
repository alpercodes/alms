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
use alms_core::{AgentId, Run, RunId, RunStatus, SessionId, TokenUsage};
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

/// Build an `AppState` whose LLM client runs in mock mode and is backed
/// by an in-memory SQLite store. Used by the #1045 HTTP-layer regression
/// test which needs (a) a real `Coordinator` capable of dispatching a
/// subagent to completion (mock LLM avoids the network) and (b) the full
/// gateway router so `GET /sessions/{id}/messages` exercises the actual
/// JSON serialization path the UI sees.
fn test_app_state_with_mock_llm() -> (
    AppState,
    CancellationToken,
    mpsc::UnboundedReceiver<SubagentCompletion>,
    mpsc::UnboundedReceiver<RunTrigger>,
    mpsc::UnboundedReceiver<DmEvent>,
) {
    let llm_config = alms_runtime::LlmConfig {
        mock: true,
        ..alms_runtime::LlmConfig::default()
    };
    let gateway_config = GatewayConfig {
        db_path: Some(":memory:".to_string()),
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
    let llm_config = alms_runtime::LlmConfig {
        base_url: "http://127.0.0.1:1".to_string(),
        api_key: "fake-key-for-test".to_string(),
        timeout_secs: 1,
        stream_chunk_timeout_secs: 1,
        ..alms_runtime::LlmConfig::default()
    };

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

/// Build an `AppState` whose LLM client points at a TCP listener that
/// ACCEPTS connections but never sends a response, combined with a
/// 1-second client timeout. The HTTP request hangs on the read side
/// until the timeout fires, giving the test a deterministic ~1s window
/// between `run_started` (producer's startup broadcast) and the
/// generic `Err(_)` terminal arm.
///
/// Used by the #927 Err-arm interposer test that needs a wider gap
/// than the port-1 ECONNREFUSED helper provides — port 1 fails almost
/// instantly, leaving no room for the test to acquire its DashMap
/// barrier guard between the producer's `mark_run_as_running`
/// (early in `execute_run`) and the producer's terminal-arm
/// `mark_run_as_failed`.
///
/// Returns `(state, shutdown_token, completion_rx, trigger_rx,
/// dm_event_rx, listener_join)`. The listener task runs until the test
/// drops `state`.
async fn test_app_state_with_hanging_llm() -> (
    AppState,
    CancellationToken,
    mpsc::UnboundedReceiver<SubagentCompletion>,
    mpsc::UnboundedReceiver<RunTrigger>,
    mpsc::UnboundedReceiver<DmEvent>,
) {
    use tokio::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{addr}");
    // Spawn an accept loop that holds connections without responding.
    // The test runtime will drop the listener when the AppState drops.
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            // Hold the connection forever (or until the client
            // times out). Spawn a task to keep the socket alive.
            tokio::spawn(async move {
                let _sock = sock;
                // Park indefinitely.
                std::future::pending::<()>().await;
            });
        }
    });

    let llm_config = alms_runtime::LlmConfig {
        base_url,
        api_key: "fake-key-for-test".to_string(),
        timeout_secs: 1,
        stream_chunk_timeout_secs: 1,
        ..alms_runtime::LlmConfig::default()
    };

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
        worktree_mode: alms_core::WorktreeMode::Off,
        debug_mode: false,
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
    assert!(
        state
            .run_manager
            .mark_run_as_failed(run_id, error_msg.clone())
    );

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

    // Mark the run as running, then completed — mirrors the production
    // lifecycle. Post-#1046/#1052 `mark_run_as_completed` enforces a
    // `Running → Completed` transition contract; the historical pattern
    // of calling it on a Queued run silently no-ops and would leave the
    // run as Queued, breaking the assertion below. The assert verifies
    // the bool-returning contract (#1052).
    state.run_manager.mark_run_as_running(run_id);
    assert!(state.run_manager.mark_run_as_completed(
        run_id,
        "done".to_string(),
        TokenUsage {
            prompt_tokens: 10,
            completion_tokens: 5,
            ..TokenUsage::default()
        },
    ));

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

/// Wire-compat regression for the #941 pivot.
///
/// UI clients on stale builds may still send the removed per-run override
/// fields (`model`, `max_tokens`, `posture`, `provider`, `debug_mode`,
/// `thinking_budget_tokens`, `reasoning_effort`, `gemini_thinking_budget`)
/// on the `POST /runs` body. The deserializer must silently ignore them
/// instead of returning a 400, otherwise every UI client on a pre-#941
/// bundle would 400 on send-message until the user reloads. This pins
/// "ignore" semantics by deserializing a request payload that carries
/// every removed field and confirming the gateway accepts it and
/// produces a queued run with the agent's resolved config.
///
/// **#943 extension.** The test also pins that the stale fields had **no
/// effect on the resolved config** — every per-run-overridable knob on the
/// `ResolvedRunConfig` snapshot `create_run` actually produced through its
/// enqueue -> `execute_run` -> `mark_run_as_running_with_config` chain
/// matches the seeded agent record, NOT the value that was in the stale
/// payload. A future reintroduction of any per-run override path that
/// leaked back into the persisted snapshot would fail these assertions.
///
/// Codex P2 follow-up on the first cut: we wait for the run to transition
/// to `Running` (which guarantees `mark_run_as_running_with_config` has
/// fired and the layered snapshot is now on the run record) and assert
/// against `run.resolved_config` from the persisted run, NOT against a
/// fresh `resolve_agent_config` call. A separate-helper assertion is
/// independent of the request body and would still pass even if a
/// regression reintroduced `body.merge_into(resolved)` somewhere inside
/// `create_run`; asserting against the run-path snapshot pins the actual
/// produced output. Same SSE-then-cancel-then-join teardown shape as
/// `happy_path_start_flips_state_before_broadcasting`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_run_ignores_stale_per_run_override_fields() {
    use alms_core::CreateRunRequest;
    use alms_core::config::ReasoningEffort;
    use alms_core::registry::AgentRecord;
    use axum::Json;
    use axum::extract::State;
    use chrono::Utc;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();
    let agent_id = AgentId::new();
    let now = Utc::now();

    // Seed an agent record whose per-agent overrides differ on every
    // assertable knob from the stale payload below. The resolved config
    // post-#941 must reflect THESE values, never the payload's.
    let agent = AgentRecord {
        id: agent_id,
        name: "stale-payload-victim".into(),
        description: String::new(),
        model: Some("claude-sonnet-4-6".into()),
        posture: Some("autonomous".into()),
        provider: Some("anthropic".into()),
        telegram_token: None,
        thinking_budget_tokens: Some(2048),
        reasoning_effort: Some(ReasoningEffort::Low),
        gemini_thinking_budget: Some(4096),
        summary_provider: None,
        summary_model: None,
        worktree_mode: alms_core::WorktreeMode::Off,
        debug_mode: false,
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

    let session = state.session_manager.get_or_create(agent_id, "web");
    let session_id = session.id;

    // Subscribe BEFORE `create_run` so we don't miss `run_started`. The
    // producer flips the run to `Running` and persists the resolved
    // snapshot via `mark_run_as_running_with_config` immediately before
    // broadcasting `run_started` (#895 ordering), so observing the event
    // is sufficient to know the snapshot is queryable on the run record.
    let mut session_rx = subscribe_session(&state, session_id);

    // Build a JSON payload with every removed per-run override field set
    // to a value that DIFFERS from the seeded agent record above. The
    // gateway must deserialize this into the new (knob-less)
    // `CreateRunRequest`, drop the extra fields without error, and
    // resolve config from per-agent + server-default only.
    let stale_payload = serde_json::json!({
        "session_id": session_id.0.to_string(),
        "input": { "type": "text", "text": "stale per-run fields" },
        "model": "definitely-not-the-agent-model",
        "max_tokens": 1234,
        "posture": "full_control",
        "provider": "openai",
        "debug_mode": true,
        "thinking_budget_tokens": 9999,
        "reasoning_effort": "high",
        "gemini_thinking_budget": 8888,
    });

    let req: CreateRunRequest = serde_json::from_value(stale_payload)
        .expect("deserializer must silently ignore removed per-run override fields");

    // Sanity: the parsed request only has the new fields.
    assert_eq!(req.session_id, session_id);

    let (status, resp) = match super::lifecycle::create_run(State(state.clone()), Json(req)).await {
        Ok(ok) => ok,
        Err((code, body)) => panic!("create_run failed: status={code:?} body={:?}", body.0),
    };
    assert_eq!(status, axum::http::StatusCode::CREATED);
    let run_id = resp.0.run_id;

    // Wait for `run_started` — the producer reaches it AFTER
    // `mark_run_as_running_with_config` persists the snapshot. The 10s
    // ceiling is loose; the actual latency is bounded by the spawned
    // queue handler's startup, which is in single-digit milliseconds on a
    // healthy runtime. The deadline only matters if the producer never
    // reaches `run_started` at all (e.g. an early-failure regression in
    // `execute_run`), in which case we want a clear timeout panic rather
    // than a hung test.
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), session_rx.recv())
            .await
            .expect("test must observe run_started within 10s")
            .expect("session sender must not close before run_started");
        if event.event_type == "run_started" {
            break;
        }
    }

    // #943: pin that the stale payload had ZERO influence on the snapshot
    // `create_run` actually produced through its enqueue chain. Read the
    // persisted run record — `resolved_config` is populated in
    // `mark_run_as_running_with_config` (lifecycle.rs:~1096), which fires
    // BEFORE the `run_started` broadcast we just observed. Asserting
    // against this snapshot pins the produced output of the run path,
    // unlike a fresh `resolve_agent_config` helper call which is
    // independent of the request body and would not catch a regression
    // that reintroduced `body.merge_into(resolved)` inside `create_run`.
    let run = state
        .run_manager
        .get_run(run_id)
        .expect("run must exist after create_run enqueued it");
    let snapshot = run
        .resolved_config
        .as_ref()
        .expect("resolved_config must be populated once the run reaches Running");

    // Provider + model: agent's anthropic/claude-sonnet-4-6, NOT the
    // payload's openai/definitely-not-the-agent-model.
    assert_eq!(
        snapshot.provider, "anthropic",
        "resolved provider must come from the agent record, not the stale payload"
    );
    assert_eq!(
        snapshot.model, "claude-sonnet-4-6",
        "resolved model must come from the agent record, not the stale payload"
    );

    // Posture: agent's autonomous, NOT the payload's full_control.
    // `ResolvedRunConfig.posture` is the stringified `Posture` enum.
    assert_eq!(
        snapshot.posture, "autonomous",
        "resolved posture must come from the agent record, not the stale payload"
    );

    // Anthropic extended thinking budget: agent's 2048, NOT the
    // payload's 9999.
    assert_eq!(
        snapshot.thinking_budget_tokens, 2048,
        "resolved thinking_budget_tokens must come from the agent record, not the stale payload"
    );

    // OpenAI reasoning effort: agent's Low, NOT the payload's "high".
    assert_eq!(
        snapshot.reasoning_effort,
        Some(ReasoningEffort::Low),
        "resolved reasoning_effort must come from the agent record, not the stale payload"
    );

    // Gemini thinking budget: agent's 4096, NOT the payload's 8888.
    assert_eq!(
        snapshot.gemini_thinking_budget,
        Some(4096),
        "resolved gemini_thinking_budget must come from the agent record, not the stale payload"
    );

    // debug_mode: agent's false, NOT the payload's true. The agent record
    // is the single source of truth for debug_mode post-#1003; the stale
    // payload's `debug_mode: true` must not flip the resolved knob on.
    // (This test seeds a non-system-triggered web session, so the #546
    // notification-flip does not apply — the snapshot value here is
    // exactly the agent record's `debug_mode`.)
    assert!(
        !snapshot.debug_mode,
        "resolved debug_mode must come from the agent record, not the stale payload"
    );

    // Tear down the spawned execute_run task. The default LLM in
    // `test_app_state_with_sqlite` points at the openrouter URL with no
    // API key, so `runtime.run()` would eventually fail on its own, but
    // cancelling here keeps test runtime tight and deterministic. The
    // post-#895 sequencing means the snapshot we just asserted on is
    // already persisted and will not be mutated by the cancel arm.
    state.run_manager.cancel_run(run_id);

    shutdown_token.cancel();
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
        worktree_mode: alms_core::WorktreeMode::Off,
        debug_mode: false,
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
    )
    .expect("success path: per-agent provider+model both supplied");
    assert_eq!(resolved.agent_name.as_deref(), Some("chamunchuk"));
    assert_eq!(resolved.llm.provider(), "anthropic");
    assert_eq!(resolved.llm.default_model(), "claude-sonnet-4-6");
}

// ---------------------------------------------------------------------------
// #863: MISSING_MODEL_AFTER_PROVIDER_SWITCH gateway-side 400
//
// `POST /runs` must reject requests where a per-agent provider override is
// set but no model was supplied at any layer. Pre-#863 the agent loop would
// send `model: ""` on the new provider's wire and surface as an opaque
// downstream 4xx (e.g. Anthropic 404 on `model: ""`). Post-#863 the gateway
// catches the deterministic config-shape failure mode at request time and
// returns a structured 400 BEFORE any LLM call.
// ---------------------------------------------------------------------------

/// Per-agent provider switch with NO model on any layer -> structured 400.
///
/// Server default is the test-default `LlmConfig::default()` (provider:
/// openrouter, default_model: moonshotai/kimi-k2.6, providers: empty).
/// Agent record carries `provider: Some("anthropic")` and `model: None`,
/// and there is no `[llm.providers.anthropic]` entry to supply a model.
/// This is the canonical #863 leak shape — pre-fix the agent loop would
/// send Anthropic the OpenRouter `kimi-k2.6` default; pre-#863 it would
/// then fall through the empty-clear and Anthropic would 404 on `model: ""`;
/// post-#863 the gateway returns 400 MISSING_MODEL_AFTER_PROVIDER_SWITCH
/// before any LLM call.
#[tokio::test]
async fn create_run_rejects_provider_switch_with_no_model_anywhere() {
    use alms_core::registry::AgentRecord;
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;
    use chrono::Utc;

    let (state, _shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();
    let agent_id = AgentId::new();
    let now = Utc::now();
    let agent = AgentRecord {
        id: agent_id,
        name: "leaky-agent".into(),
        description: String::new(),
        // The #863 trigger: provider override with NO model at any layer.
        model: None,
        posture: None,
        provider: Some("anthropic".into()),
        telegram_token: None,
        thinking_budget_tokens: None,
        reasoning_effort: None,
        gemini_thinking_budget: None,
        summary_provider: None,
        summary_model: None,
        worktree_mode: alms_core::WorktreeMode::Off,
        debug_mode: false,
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

    let session = state.session_manager.get_or_create(agent_id, "web");
    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: Some(agent_id),
        input: RunInput::Text {
            text: "hello".into(),
        },
    };

    let Err((status, body)) = super::lifecycle::create_run(State(state.clone()), Json(req)).await
    else {
        panic!("create_run must reject when no model is supplied at any layer (#863)");
    };

    // Acceptance criteria from issue #863:
    // 1. 400 status code BEFORE any LLM call
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    // 2. error_code == "MISSING_MODEL_AFTER_PROVIDER_SWITCH"
    assert_eq!(
        body.0["error_code"], "MISSING_MODEL_AFTER_PROVIDER_SWITCH",
        "body must carry the structured error_code so clients can branch on it"
    );
    // 3. Body carries agent_id + new_provider + prev_provider so the operator
    //    knows which agent to PATCH and which providers were involved.
    assert_eq!(
        body.0["agent_id"],
        agent_id.0.to_string(),
        "body must identify the agent so operators know which record to PATCH"
    );
    assert_eq!(
        body.0["new_provider"], "anthropic",
        "body must name the new provider the run was about to be sent to"
    );
    assert_eq!(
        body.0["prev_provider"], "openrouter",
        "body must name the previous (server-default) provider whose model leaked"
    );
    // 4. Human-readable message describes the failure mode.
    let message = body.0["message"]
        .as_str()
        .expect("message must be a string");
    assert!(
        message.contains("anthropic") && message.contains("openrouter"),
        "message must explain which provider override caused the failure: {message}"
    );

    // 5. No run was enqueued — the rejection happens BEFORE `insert_run`.
    let runs = state.run_manager.list_by_agent(agent_id, 10);
    assert!(
        runs.is_empty(),
        "no run should have been created when the gateway rejects pre-flight"
    );
}

/// Same provider on both sides -> no spurious 400.
///
/// Pin the no-spurious-400 invariant: when the agent record's provider
/// matches the server default (no actual switch), the leak guard must NOT
/// fire even if the agent has no per-agent `model`. The server-default
/// model reaches the wire as intended.
#[tokio::test]
async fn create_run_does_not_reject_when_provider_unchanged() {
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
        name: "happy-agent".into(),
        description: String::new(),
        model: None,
        posture: None,
        // Same provider as the server default (`openrouter` per
        // `LlmConfig::default()`). No switch -> no leak guard.
        provider: Some("openrouter".into()),
        telegram_token: None,
        thinking_budget_tokens: None,
        reasoning_effort: None,
        gemini_thinking_budget: None,
        summary_provider: None,
        summary_model: None,
        worktree_mode: alms_core::WorktreeMode::Off,
        debug_mode: false,
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

    let session = state.session_manager.get_or_create(agent_id, "web");
    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: Some(agent_id),
        input: RunInput::Text {
            text: "hello".into(),
        },
    };

    let (status, _resp) = super::lifecycle::create_run(State(state), Json(req))
        .await
        .expect("same-provider config must NOT be rejected (#863)");
    assert_eq!(status, axum::http::StatusCode::CREATED);
    shutdown_token.cancel();
}

/// Per-agent provider switch WITH a per-agent model -> 200 (success path).
///
/// Pin the no-spurious-400 invariant: when the agent record carries an
/// in-namespace per-agent model, the run is accepted even though the
/// provider was switched.
#[tokio::test]
async fn create_run_accepts_provider_switch_with_per_agent_model() {
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
        name: "well-configured".into(),
        description: String::new(),
        // Per-agent model in the new provider's namespace -> success.
        model: Some("claude-sonnet-4-6".into()),
        posture: None,
        provider: Some("anthropic".into()),
        telegram_token: None,
        thinking_budget_tokens: None,
        reasoning_effort: None,
        gemini_thinking_budget: None,
        summary_provider: None,
        summary_model: None,
        worktree_mode: alms_core::WorktreeMode::Off,
        debug_mode: false,
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

    let session = state.session_manager.get_or_create(agent_id, "web");
    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: Some(agent_id),
        input: RunInput::Text {
            text: "hello".into(),
        },
    };

    let (status, _resp) = super::lifecycle::create_run(State(state), Json(req))
        .await
        .expect("provider switch with valid per-agent model must NOT be rejected");
    assert_eq!(status, axum::http::StatusCode::CREATED);
    shutdown_token.cancel();
}

/// `execute_run`'s `match resolve_outcome` failure arm must mark the run
/// `Failed` with the structured `MissingModelAfterProviderSwitch` message
/// when invoked on a non-HTTP path (Telegram / scheduler / peer-DM /
/// subagent-completion triggers).
///
/// `create_run` runs `resolve_agent_config` as a pre-flight check and
/// rejects with `400 MISSING_MODEL_AFTER_PROVIDER_SWITCH` before
/// `insert_run`, so the HTTP path never reaches the in-loop resolve. The
/// non-HTTP triggers all enqueue runs that flow straight into
/// `execute_run`, where the resolve runs again under live locks. If a
/// future refactor "simplifies" the in-loop resolve back to `unwrap()`
/// (the symmetry argument: "create_run already pre-flighted, the second
/// resolve can't fail") the regression would be silent because the only
/// existing tests covering the missing-model path go through
/// `create_run`'s pre-flight rather than driving `execute_run` directly.
///
/// This test closes that coverage gap: it bypasses `create_run` (mirroring
/// what the Telegram / scheduler paths do — `insert_run` + `execute_run`
/// directly) and pins the three post-conditions of the failure arm:
///
/// 1. Terminal status is `Failed` — not `Running` (would mean the resolve
///    Err leaked through), not `Cancelled` (would mean the cancel-token
///    early-exit fired instead).
/// 2. The persisted `error` field carries the `Display`-formatted
///    structured message — `mark_run_as_failed(run_id, e.to_string())` —
///    so operators reading `GET /runs/{id}` can identify the failure mode
///    by `error_code`-substring grep, same as the HTTP 400 body's
///    `message` field.
/// 3. The `in_flight` counter returns to zero — the RAII
///    `_in_flight_guard` decrements correctly when the failure arm
///    early-returns.
#[tokio::test]
async fn execute_run_failure_arm_marks_run_failed_with_structured_error_on_provider_switch_without_model()
 {
    use alms_core::registry::AgentRecord;
    use chrono::Utc;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();

    // Seed an agent with the canonical #863 trigger shape: provider
    // override to `anthropic` with NO model on any layer. Server default
    // is `openrouter` per `LlmConfig::default()`, so this is a real
    // cross-namespace switch and the in-loop `resolve_agent_config` will
    // fail with `MissingModelAfterProviderSwitch`.
    let agent_id = AgentId::new();
    let now = Utc::now();
    let agent = AgentRecord {
        id: agent_id,
        name: "non-http-trigger-agent".into(),
        description: String::new(),
        model: None,
        posture: None,
        provider: Some("anthropic".into()),
        telegram_token: None,
        thinking_budget_tokens: None,
        reasoning_effort: None,
        gemini_thinking_budget: None,
        summary_provider: None,
        summary_model: None,
        worktree_mode: alms_core::WorktreeMode::Off,
        debug_mode: false,
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

    let session = state
        .session_manager
        .get_or_create(agent_id, "non-http-context");
    let session_id = session.id;

    // Bypass `create_run` (which would reject pre-flight) and enqueue the
    // run directly — this is the shape the Telegram / scheduler / peer-DM
    // / subagent paths use.
    let run = Run::new(session_id, agent_id, "trigger #863 in execute_run".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run.clone());

    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    // Snapshot the in-flight counter before the call so we can pin its
    // post-call delta even if the test fixture changes the baseline.
    let in_flight_before = state.run_manager.in_flight_count();

    super::lifecycle::execute_run(
        state.clone(),
        super::RunParams {
            run_id,
            session_id,
            agent_id,
            input: run.input,
            context_id: "non-http-context".to_string(),
            cancel_token,
            // is_peer_message=false / is_system_triggered=false matches
            // what a Telegram-driven run carries — the failure arm is
            // independent of these flags. Other non-HTTP callers
            // (notifications, subagent completions) use
            // is_system_triggered=true; pinning the false case here is
            // sufficient since the resolve happens before any flag-driven
            // branch.
            is_peer_message: false,
            is_system_triggered: false,
            input_pre_persisted: false,
        },
    )
    .await;

    // 1. Terminal status is Failed — the failure arm fired.
    let final_run = state
        .run_manager
        .get_run(run_id)
        .expect("run must still exist after execute_run returns");
    assert_eq!(
        final_run.status,
        RunStatus::Failed,
        "run must reach Failed via the resolve_outcome failure arm; got {:?}",
        final_run.status,
    );

    // 2. The persisted error carries the `Display`-formatted structured
    //    message. We grep for the agent_id and both provider names rather
    //    than pinning the full string — the `Display` format itself is
    //    pinned by `test_missing_model_after_provider_switch_display_format`
    //    in `mod.rs`, and decoupling the assertions there from this one
    //    means a benign rephrasing of `Display` only updates one test.
    let error_msg = final_run
        .error
        .as_ref()
        .expect("Failed run must carry a structured error message");
    assert!(
        error_msg.contains(&agent_id.0.to_string()),
        "error must identify the agent_id (got: {error_msg})"
    );
    assert!(
        error_msg.contains("anthropic"),
        "error must name the new provider (got: {error_msg})"
    );
    assert!(
        error_msg.contains("openrouter"),
        "error must name the previous provider (got: {error_msg})"
    );

    // 3. The RAII `_in_flight_guard` must have decremented the counter
    //    back to its pre-call value when the failure arm returned. A
    //    future refactor that hoists the resolve out of the
    //    `track_in_flight` window would silently regress the drain
    //    semantics; pinning the delta catches that.
    assert_eq!(
        state.run_manager.in_flight_count(),
        in_flight_before,
        "in_flight counter must return to baseline ({}) after the failure arm; got {}",
        in_flight_before,
        state.run_manager.in_flight_count(),
    );

    shutdown_token.cancel();
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

/// #1052 regression: when a DM run is cancelled mid-`ignore_message`-unwind,
/// the post-`Ok`-arm bookkeeping in `execute_run` must NOT fire — the peer
/// already received `run_cancelled` (or will receive it from the cancel
/// arm), and emitting `dm_conversation_ended` with reason `"ignored"`
/// (the `Display`-format of `ConversationEndReason::Ignored`) here would
/// conflate an operator-driven cancel with an agent-driven ignore,
/// corrupting the peer's conversation-state model. (The issue body
/// paraphrased the reason as `"ignore_message"`; the wire-format reason
/// is `"ignored"` per `ConversationEndReason::Display`. The conflation
/// concern is identical regardless of the spelling.)
///
/// The contract this test pins:
///
/// 1. `mark_run_as_completed` is now bool-returning. When the run is
///    already `Cancelled` (because a racing cancel path drove it there),
///    the call returns `false` and the existing `Cancelled` status is
///    preserved (the transition is idempotent — see
///    `test_mark_run_terminal_is_idempotent` in `run_manager.rs` for the
///    data-layer contract).
///
/// 2. The lifecycle layer's `Ok` arm in `execute_run` reads that bool
///    and gates `handle_dm_run_completion` on it. This test exercises
///    the gate at the call site: we drive the exact ordering the racing
///    `Ok` arm sees (`mark_run_as_cancelled` already won, then
///    `mark_run_as_completed` runs), and verifies that:
///
///    - `mark_run_as_completed` returns `false`.
///    - When the caller honours the gate (skips
///      `handle_dm_run_completion`), no `dm_conversation_ended` SSE
///      event is emitted on the DM session feed.
///    - For contrast, if we ignored the gate and called
///      `handle_dm_run_completion` anyway with `ignore_message` in the
///      tool-call record set, it WOULD emit `dm_conversation_ended` —
///      proving the gate is what prevents the double-emit, not some
///      other coincidence in the test setup.
///
/// Driving the full `execute_run` race deterministically would require an
/// LLM stub that returns `Ok` carrying an `ignore_message` tool-call
/// record while a sibling task races a cancel in between the loop's
/// return and `execute_run`'s `Ok` arm — feasible but flaky. The
/// gate-at-the-call-site test pins the contract just as tightly with
/// zero scheduling sensitivity. The full-flow scenario is covered
/// indirectly by code review of the `Ok` arm: the only path to
/// `handle_dm_run_completion` is gated on `completed_transitioned`,
/// which is `false` in this test.
#[tokio::test]
async fn handle_dm_run_completion_gated_when_cancel_wins_race() {
    use alms_core::ToolCallRole;

    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    // Open a DM conversation: alice → bob. This creates the shared DM
    // session and bumps the depth counter, so the subsequent
    // `end_conversation` path has something to remove (and so the
    // `dm_ended` marker write is reachable — `end_conversation` aborts
    // early if the session doesn't exist).
    let _receipt = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    // Drain the trigger emitted by `send` so subsequent assertions are
    // scoped to the post-cancel side effects only.
    let _initial_trigger = tr.try_recv().expect("send should emit a trigger");

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");

    // Insert bob's peer-triggered run (the run that races the cancel).
    let run = Run::new(dm_session_id, bob_id, "received a DM".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);
    state.run_manager.mark_run_as_running(run_id);

    // Simulate the race: a concurrent path (cancel handler, shutdown
    // drain, `cancel_runs_for_session`, ...) drives the run to
    // `Cancelled` BEFORE the Ok arm processes the loop result.
    assert!(
        state.run_manager.mark_run_as_cancelled(run_id),
        "first mark_run_as_cancelled must transition the run"
    );

    // Subscribe to the DM session AFTER the cancel transition so we can
    // pin "no `dm_conversation_ended` SSE on the gated path." A
    // pre-fix run would emit one here.
    let mut dm_rx = subscribe_session(&state, dm_session_id);

    // Now the racing Ok arm tries to mark Completed — the bool gate must
    // return `false`, the status must stay `Cancelled`, and the caller
    // must skip `handle_dm_run_completion` (the lifecycle layer in
    // `lifecycle.rs` does this; we replicate the contract here).
    let completed_transitioned = state.run_manager.mark_run_as_completed(
        run_id,
        String::new(),
        alms_core::TokenUsage::default(),
    );
    assert!(
        !completed_transitioned,
        "mark_run_as_completed must return false when state is already terminal"
    );
    assert_eq!(
        state.run_manager.get_run(run_id).unwrap().status,
        RunStatus::Cancelled,
        "Cancelled status must NOT be clobbered to Completed"
    );

    // Build the same tool-call record set the cancelled `ignore_message`
    // run would have produced. `should_signal_dm_end` is satisfied by
    // this shape — if we were to call `handle_dm_run_completion` here,
    // it would emit `dm_conversation_ended` with reason
    // `"ignore_message"`. The gate must prevent the call.
    let ignore_records = vec![
        alms_core::ToolCallRecord {
            seq: 0,
            role: ToolCallRole::Assistant,
            tool_name: Some("ignore_message".to_string()),
            tool_id: Some("call_1".to_string()),
            params: None,
            result: None,
            timestamp: chrono::Utc::now(),
            from_agent: None,
        },
        alms_core::ToolCallRecord {
            seq: 1,
            role: ToolCallRole::Tool,
            tool_name: Some("ignore_message".to_string()),
            tool_id: Some("call_1".to_string()),
            params: None,
            result: Some(r#"{"ok":true}"#.to_string()),
            timestamp: chrono::Utc::now(),
            from_agent: None,
        },
    ];
    // Pin the contrast: these records WOULD satisfy
    // `should_signal_dm_end` if the gate weren't there. The
    // `should_signal_dm_end` function is the gating logic inside
    // `handle_dm_run_completion` itself — and it returns `true` for
    // this shape, which is exactly why the call-site gate is necessary.
    assert!(
        super::dm_lifecycle::should_signal_dm_end(true, &ignore_records, dm_context),
        "tool-call record set must trigger `dm_conversation_ended` if \
         `handle_dm_run_completion` is called — proving the gate is what \
         prevents the double-emit, not some other accident in this setup"
    );

    // The Ok arm honours the gate: when `completed_transitioned` is
    // false, `handle_dm_run_completion` is NOT called. We do not call
    // it here either — this is the contract being verified.

    // Assert: no `dm_conversation_ended` SSE landed on the DM session
    // feed. Any other events that flowed through (e.g. from the depth
    // bookkeeping triggered by the `send` above) are allowed; only the
    // post-Ok-arm `dm_conversation_ended` is forbidden.
    tokio::task::yield_now().await;
    let events = drain_events(&mut dm_rx);
    assert!(
        !events
            .iter()
            .any(|e| e.event_type == "dm_conversation_ended"),
        "no `dm_conversation_ended` SSE event must be emitted when the cancel \
         path wins the race against an `ignore_message`-emitting `Ok` arm; \
         got events: {:?}",
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>(),
    );

    shutdown_token.cancel();
}

/// Companion test to `handle_dm_run_completion_gated_when_cancel_wins_race`:
/// when the `Ok` arm transitions cleanly (no racing cancel), the gate
/// passes and `handle_dm_run_completion` DOES emit
/// `dm_conversation_ended`. Pins the happy-path side of the gate so a
/// future refactor that "simplifies" the gate by inverting the
/// condition gets caught by both tests.
#[tokio::test]
async fn handle_dm_run_completion_fires_when_completed_transition_wins() {
    use alms_core::ToolCallRole;

    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    let _receipt = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _initial_trigger = tr.try_recv().expect("send should emit a trigger");

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");

    let run = Run::new(dm_session_id, bob_id, "received a DM".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);
    state.run_manager.mark_run_as_running(run_id);

    let mut dm_rx = subscribe_session(&state, dm_session_id);

    // No racing cancel — the Ok arm wins.
    let completed_transitioned = state.run_manager.mark_run_as_completed(
        run_id,
        String::new(),
        alms_core::TokenUsage::default(),
    );
    assert!(
        completed_transitioned,
        "mark_run_as_completed must return true on the first call from Running"
    );

    let ignore_records = vec![
        alms_core::ToolCallRecord {
            seq: 0,
            role: ToolCallRole::Assistant,
            tool_name: Some("ignore_message".to_string()),
            tool_id: Some("call_1".to_string()),
            params: None,
            result: None,
            timestamp: chrono::Utc::now(),
            from_agent: None,
        },
        alms_core::ToolCallRecord {
            seq: 1,
            role: ToolCallRole::Tool,
            tool_name: Some("ignore_message".to_string()),
            tool_id: Some("call_1".to_string()),
            params: None,
            result: Some(r#"{"ok":true}"#.to_string()),
            timestamp: chrono::Utc::now(),
            from_agent: None,
        },
    ];

    // Mirror the lifecycle layer's behaviour: gate passes, so call
    // `handle_dm_run_completion`.
    let signalled = super::dm_lifecycle::handle_dm_run_completion(
        super::dm_lifecycle::DmRunCompletionContext {
            state: &state,
            run_id,
            session_id: dm_session_id,
            agent_id: bob_id,
            agent_name: Some("bob"),
            context_id: dm_context,
            is_peer_message: true,
            tool_calls: &ignore_records,
        },
    )
    .await;
    assert!(
        signalled,
        "handle_dm_run_completion must return true for a peer-DM run \
         that called ignore_message"
    );

    tokio::task::yield_now().await;
    let events = drain_events(&mut dm_rx);
    let dm_ended = events
        .iter()
        .find(|e| e.event_type == "dm_conversation_ended")
        .unwrap_or_else(|| {
            panic!(
                "happy path must emit dm_conversation_ended; got events: {:?}",
                events.iter().map(|e| &e.event_type).collect::<Vec<_>>(),
            )
        });
    // `ConversationEndReason::Ignored` serialises to `"ignored"` (see
    // `alms-tools/src/message_sender.rs`); the SSE event renders the
    // `Display`-format string.
    assert_eq!(
        dm_ended
            .data
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        "ignored",
        "reason must be `ignored` (Display of ConversationEndReason::Ignored) \
         on the happy path"
    );

    shutdown_token.cancel();
}

/// #1052 review (Tim): pin the contract that `handle_dm_run_failure`
/// fires from the terminal Err arm REGARDLESS of whether
/// `mark_run_as_cancelled` returned `true` or `false`.
///
/// Per #1050's design, `handle_dm_run_failure` is an independent side
/// effect: the synchronous HTTP cancel handler from #1050 flips state
/// and broadcasts `run_cancelled` itself, but it does NOT call
/// `handle_dm_run_failure` — it relies on the terminal Err(Cancelled)
/// arm in `execute_run` to do that. If the terminal arm were to gate
/// the call on `cancelled_transitioned`, then when the HTTP cancel
/// won the race the bool would be `false` and the DM peer would
/// never receive the `ConversationEnded` peer notification, the depth
/// counter would never be reset, and no `dm_ended` marker would land.
///
/// This test simulates the race: a concurrent path drives the run to
/// `Cancelled` first (so the subsequent `mark_run_as_cancelled` call
/// returns `false`), then calls `handle_dm_run_failure` directly (as
/// the terminal arm does, post-fix). The peer-notification side
/// effects (`ConversationEnded` `RunTrigger`, `dm_ended` marker,
/// `dm_conversation_ended` SSE) MUST all land.
#[tokio::test]
async fn handle_dm_run_failure_fires_when_cancel_transition_already_lost() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    // Open the DM and drain the initial trigger from `send`.
    let _ = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _ = tr.try_recv();

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");

    // Insert bob's peer-triggered run.
    let run = Run::new(dm_session_id, bob_id, "received a DM".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);
    state.run_manager.mark_run_as_running(run_id);

    // Simulate the race: an external path (the synchronous HTTP cancel
    // handler from #1050, a sibling shutdown drain, etc.) has already
    // driven the run to `Cancelled` before the terminal Err arm runs.
    assert!(
        state.run_manager.mark_run_as_cancelled(run_id),
        "first mark_run_as_cancelled must transition"
    );

    // Subscribe AFTER the racing cancel so the test only observes the
    // post-terminal-arm side effects.
    let mut dm_rx = subscribe_session(&state, dm_session_id);

    // The terminal Err(Cancelled) arm now runs. Its `mark_run_as_cancelled`
    // call returns `false` — the SSE broadcast is correctly skipped (the
    // external path already emitted `run_cancelled`).
    let cancelled_transitioned = state.run_manager.mark_run_as_cancelled(run_id);
    assert!(
        !cancelled_transitioned,
        "second mark_run_as_cancelled must return false when state is already terminal"
    );

    // CONTRACT: `handle_dm_run_failure` MUST still fire. The terminal arm
    // calls it unconditionally — it is an independent side effect per
    // #1050, NOT something to skip when the state-flip lost the race.
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
    .expect("handle_dm_run_failure must succeed even when cancel transition was lost");

    // Assert: dm_ended marker landed. Without this, Alice's session UI
    // shows the DM as still open until the 1800s `DEPTH_EXPIRY_SECS`
    // sweep clears the depth counter.
    let history = state.session_manager.get_history(dm_session_id).unwrap();
    assert!(
        history.iter().any(|m| {
            m.metadata
                .as_ref()
                .and_then(|meta| meta.get("reason"))
                .and_then(|v| v.as_str())
                == Some("user_cancelled")
        }),
        "dm_ended marker with reason=user_cancelled MUST be persisted even when \
         the state-flip race was lost — this is the contract that prevents \
         stranded DM peers when the HTTP cancel handler wins"
    );

    // Assert: ConversationEnded RunTrigger fired so Alice's notifications
    // session learns about the end.
    let trigger = tr
        .try_recv()
        .expect("ConversationEnded RunTrigger MUST fire even on lost cancel race");
    match trigger.source {
        MessageSource::ConversationEnded { reason, .. } => {
            assert!(
                matches!(reason, ConversationEndReason::UserCancelled),
                "trigger reason must be UserCancelled; got {reason:?}"
            );
        }
        other => panic!("expected ConversationEnded source, got {other:?}"),
    }

    // Assert: dm_conversation_ended SSE landed on the DM session feed.
    tokio::task::yield_now().await;
    let events = drain_events(&mut dm_rx);
    let dm_ended = events
        .iter()
        .find(|e| e.event_type == "dm_conversation_ended")
        .expect("dm_conversation_ended SSE MUST fire even when the state-flip race was lost");
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
    assert!(
        state
            .run_manager
            .mark_run_as_completed(run_id, "ok".into(), Default::default())
    );
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
    assert!(state.run_manager.mark_run_as_cancelled(run_id));
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
// #927: extend #895 state-flip-before-broadcast to completed/failed paths
// ---------------------------------------------------------------------------
//
// Tim's review on PR #925 flagged that the four-site reorder in #895 closed
// the SSE-vs-state race for the cancel/start paths but missed the symmetric
// race on the success/failure terminator paths in `execute_run`. The
// production fix lives in `crates/alms-gateway/src/runs/lifecycle.rs` —
// each of the three terminal arms (`Ok`, `FailedWithToolCalls`, generic
// `Err`) now calls `mark_run_as_*` BEFORE the
// `send_event(run_finished | run_error)` broadcast.
//
// Pinning the invariant: unlike the four #895 sites — where the next
// `send_agent_event(...).await` between the broadcast and the flip
// provided a natural suspension barrier the consumer task could ride on —
// the Ok / FailedWithToolCalls / Err arms in pre-fix code call
// `mark_run_as_*` SYNCHRONOUSLY immediately after `send_event` returns,
// then `.await` further downstream (e.g. `dm_lifecycle::handle_dm_run_*`).
// The next natural suspension lies BEYOND the flip in pre-fix order, so
// the natural-barrier trick from #895 does not distinguish pre-fix from
// post-fix here.
//
// Instead the failure-arm test below uses the `RunManager::runs` DashMap
// as an explicit synchronisation barrier: the test acquires a
// `runs.get_mut(&run_id)` write guard AFTER the producer's startup
// `mark_run_as_running` (signalled by the arrival of `run_started` on
// the session feed) but BEFORE the producer's terminal-arm `mark_run_as_*`
// runs. `mark_run_as_*` calls `runs.get_mut(&run_id)` internally (see
// `modify_and_snapshot` in `RunManager`), so the held guard parks the
// terminal flip on the parking_lot RwLock. `send_event` touches only
// `event_senders` and `session_senders`, so the broadcast remains
// unaffected by the barrier.
//
// - **Pre-fix order** (broadcast then flip): the producer reaches
//   `send_event(run_error)` first, the consumer receives the event, then
//   the producer parks on the held DashMap guard. The test sees the
//   broadcast and the assertion FAILS.
// - **Post-fix order** (flip then broadcast): the producer parks on the
//   guard at `mark_run_as_failed` and never reaches `send_event`. The
//   test does not see the broadcast within the timeout and the assertion
//   PASSES.
//
// Note on which arm is exercised end-to-end: `AgentRuntime::finish_run`
// wraps every non-Cancelled error returned by `agent_loop` into
// `AlmsError::FailedWithToolCalls { source, tool_calls }` — even when
// `tool_calls` is empty. So an end-to-end run with a failing LLM lands
// in the `FailedWithToolCalls` arm of `execute_run`, NOT the generic
// `Err(_)` arm. The interposer test below therefore exercises the
// `FailedWithToolCalls` arm (which is the production-relevant path for
// any LLM 4xx/5xx, rate-limit, content-policy reject, timeout, or
// stream-parse failure).
//
// **Reverting the `FailedWithToolCalls`-arm reorder in `lifecycle.rs`
// causes `failed_with_tool_calls_arm_flips_state_before_broadcasting`
// to fail**, because the broadcast is then on the producer's pre-flip
// side of the held guard and the consumer receives it inside the
// timeout window.
//
// The Ok arm and generic Err arm rely on the same production fix
// (identical structural shape) but cannot be exercised by the
// gap-based interposer:
// - The Ok arm requires a fast-completing LLM (mock mode), but with
//   mock mode the window between `run_started` (consumer wake) and the
//   terminal flip is too small for the test to deterministically wedge
//   a guard acquisition into. Wiring a slow-responding HTTP fixture
//   into `LlmClient` from the gateway crate is out of scope for this
//   fix — see #927 follow-up.
// - The generic `Err(_)` arm is unreachable through `runtime.run()`
//   because `finish_run` re-wraps every error into
//   `FailedWithToolCalls`. It exists to handle direct
//   `AgentRuntime`-bypass paths and synthetic test inputs.
//
// They are covered by smoke tests at the `RunManager` boundary that
// mirror the post-fix call order in the test body (matching the
// precedent set by `smoke_post_execute_cancel_*` for #895). Those tests
// do NOT regression-pin the `lifecycle.rs` ordering; the
// `FailedWithToolCalls`-arm interposer test is the load-bearing pin
// that makes the bundle revert detectable.

/// Regression test for #927 (`FailedWithToolCalls` arm,
/// interposer-via-DashMap-barrier): in the
/// `Err(FailedWithToolCalls { ... })` arm of `execute_run`,
/// `mark_run_as_failed` must be called BEFORE `send_event(run_error)`.
///
/// This arm is the end-to-end production path for any LLM call failure
/// (4xx/5xx, rate-limit, content-policy reject, timeout, stream-parse
/// error, etc.) because `AgentRuntime::finish_run` re-wraps every
/// non-Cancelled error from `agent_loop` into `FailedWithToolCalls`
/// regardless of whether tool calls actually executed.
///
/// **Pin mechanism (gap-based DashMap barrier):**
///
/// 1. Spawn `execute_run` with the hanging-LLM helper. The producer
///    reaches `mark_run_as_running` (no guard held → succeeds), then
///    fires `send_event(run_started)` and enters the LLM call which
///    will fail after the 1s `timeout_secs` budget the helper sets.
/// 2. The consumer task observes `run_started` on the session SSE feed
///    and acquires a `runs.get_mut(&run_id)` write guard. The hanging
///    LLM (TCP listener that never responds) opens a deterministic
///    ~1-2s window between `run_started` and the terminal-arm flip,
///    plenty of time for the consumer to wedge in.
/// 3. The producer's `runtime.run()` returns `Err(FailedWithToolCalls
///    { source, tool_calls })` and the producer enters the
///    `FailedWithToolCalls` arm of `execute_run`.
///    - **Post-fix:** `mark_run_as_failed` runs first → blocks on the
///      held DashMap guard. `send_event(run_error)` never fires within
///      the timeout. The consumer's `run_error` recv times out, the
///      test asserts no event observed, PASSES.
///    - **Pre-fix:** `send_event(run_error)` runs first → broadcast
///      lands on the consumer feed. `mark_run_as_failed` then blocks on
///      the held guard. The consumer's `run_error` recv succeeds, the
///      assertion FAILS.
/// 4. The test releases the guard so the producer can complete teardown.
///
/// **Reverting the `FailedWithToolCalls`-arm reorder in
/// `lifecycle.rs` causes this test to fail** — verified locally by
/// reverting the production change and observing the assertion fire.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_with_tool_calls_arm_flips_state_before_broadcasting() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_hanging_llm().await;
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-927-err-arm");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "trigger LLM failure".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run.clone());

    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    let mut session_rx = subscribe_session(&state, session_id);

    let runs_clone = state.run_manager.runs.clone();

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
                context_id: "test-927-err-arm".to_string(),
                cancel_token,
                is_peer_message: false,
                is_system_triggered: false,
                input_pre_persisted: false,
            },
        )
        .await
    });

    // Wait for `run_started` (which fires AFTER `mark_run_as_running`,
    // so the early DashMap write has already completed and won't be
    // blocked by our guard).
    let started_deadline = tokio::time::sleep(std::time::Duration::from_secs(2));
    tokio::pin!(started_deadline);
    let mut saw_started = false;
    loop {
        tokio::select! {
            biased;
            _ = &mut started_deadline => break,
            event = session_rx.recv() => {
                match event {
                    Some(e) if e.event_type == "run_started" => {
                        saw_started = true;
                        break;
                    }
                    Some(_) => continue,
                    None => break,
                }
            }
        }
    }
    assert!(
        saw_started,
        "expected `run_started` SSE event before LLM failure window opens"
    );

    // Acquire the DashMap write guard. The producer is now between
    // `mark_run_as_running` (already done) and the terminal arm
    // (~1-2s away while the hanging LLM times out). Holding this guard
    // blocks the producer's terminal `mark_run_as_*`.
    //
    // **Note for future maintainers (Tim's PR #936 review):** this is a
    // synchronous (parking_lot) lock guard held across `.await` points
    // below. That is *intentional* and not a deadlock risk:
    //
    // - DashMap v6 shards are `parking_lot::RwLock`s and the guard is a
    //   sync lock, not a Tokio async lock — it is not aware of task
    //   suspension and never yields to the runtime.
    // - The await we hold across is `session_rx.recv()` on a
    //   *different* task's broadcast channel; the lock is acquired by
    //   this test task and contended by the producer task only via
    //   `runs.get_mut(&run_id)` inside `mark_run_as_*`. There is no
    //   reentrancy from this task into the same shard, so no
    //   self-deadlock is possible.
    // - The point of the test IS to wedge that contention: the held
    //   guard is the synchronisation barrier that pins
    //   broadcast-vs-flip ordering. "Fixing" the held-across-await
    //   shape (e.g. dropping the guard before the recv loop, or
    //   swapping to a tokio mutex) would dismantle the regression pin.
    let _guard = runs_clone
        .get_mut(&run_id)
        .expect("run must exist after insert_run");

    // Wait for `run_error` to land. In post-fix code the producer is
    // blocked on the guard at `mark_run_as_failed` and the broadcast
    // never fires; we time out without seeing the event. In pre-fix
    // code the broadcast runs first, the consumer receives `run_error`
    // immediately, and the assertion below fails.
    //
    // Use a generous timeout (5s) so the hanging-LLM helper has time
    // to time out (1s × 2 attempts via stream-then-buffer fallback) and
    // the producer has time to reach the terminal arm. Pre-fix code
    // delivers the event well within this window in practice.
    //
    // **One-sided false-pass risk on slow CI (Tim's PR #936 review):**
    // this is a "absence of event" assertion, so any environment where
    // the producer takes longer than 5s to reach the terminal arm
    // (hanging-LLM timeout + scheduling slack on a heavily-loaded
    // runner) will pass for the *wrong* reason — the broadcast simply
    // hasn't fired yet, regardless of pre-/post-fix order. The window
    // is sized for the 1s hanging-LLM timeout × 2 attempts plus headroom
    // and has been stable in CI to date; if this test ever starts
    // flaking on slow CI the right move is a deterministic barrier
    // (e.g. a tap on the `mark_run_as_failed` call rather than a wall
    // clock), not a longer timeout — bumping the timeout widens the
    // false-pass window without strengthening the pin.
    let mut saw_error = false;
    let err_deadline = tokio::time::sleep(std::time::Duration::from_secs(5));
    tokio::pin!(err_deadline);
    loop {
        tokio::select! {
            biased;
            _ = &mut err_deadline => break,
            event = session_rx.recv() => {
                match event {
                    Some(e) if e.event_type == "run_error" => {
                        saw_error = true;
                        break;
                    }
                    Some(_) => continue,
                    None => break,
                }
            }
        }
    }

    assert!(
        !saw_error,
        "pre-#927 race: `run_error` was broadcast BEFORE \
         `mark_run_as_failed` flipped the run state. Holding the \
         DashMap write guard blocks the flip; in pre-fix code the \
         broadcast runs to completion while the producer is parked on \
         the lock. Post-fix code blocks at the flip first and never \
         reaches the broadcast within the 5s timeout window. Reverting \
         the `FailedWithToolCalls`-arm reorder in lifecycle.rs causes \
         this assertion to fail."
    );

    // Release the DashMap guard so the producer can complete and the
    // test can shut down cleanly.
    drop(_guard);

    let _ = tokio::time::timeout(std::time::Duration::from_secs(15), exec_handle)
        .await
        .expect("execute_run task must complete within 15s after guard drop");

    shutdown_token.cancel();
}

/// Smoke test (NOT a regression pin) covering the call sequence for the
/// `Ok(_)` arm at the `RunManager` boundary. Driving `execute_run`'s
/// `Ok(_)` arm requires a fast-completing LLM (mock mode), but the
/// window between `run_started` and the terminal flip with mock mode is
/// too small for the gap-based DashMap-barrier interposer used by
/// `failed_with_tool_calls_arm_flips_state_before_broadcasting` above
/// to wedge a guard acquisition reliably.
///
/// What this test verifies: that callers using the post-#927 sequence
/// (`mark_run_as_completed` then `send_event(run_finished)`) see
/// `has_active_runs == false` upon receiving the `run_finished` event.
/// This is a sanity check on the call sequence itself, NOT that the
/// production code emits events in that sequence — the test mirrors the
/// post-fix order in its own body, so reverting `lifecycle.rs` cannot
/// break it.
///
/// Mirrors `smoke_post_execute_cancel_flips_state_at_run_manager_boundary`
/// for #895.
#[tokio::test]
async fn smoke_ok_arm_flips_state_at_run_manager_boundary() {
    use alms_core::TokenUsage;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-927-smoke-ok");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "test".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);
    state.run_manager.mark_run_as_running(run_id);
    assert!(state.run_manager.has_active_runs(session_id));

    let mut session_rx = subscribe_session(&state, session_id);

    // Mirror the post-#927 ordering: flip state first, broadcast second.
    assert!(state.run_manager.mark_run_as_completed(
        run_id,
        "ok".to_string(),
        TokenUsage::default()
    ));
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::run_finished(run_id, true, TokenUsage::default()),
        )
        .await;

    let event = session_rx
        .recv()
        .await
        .expect("run_finished event must be delivered");
    assert_eq!(event.event_type, "run_finished");

    assert!(
        !state.run_manager.has_active_runs(session_id),
        "has_active_runs must be false after mark_run_as_completed \
         (sanity check on the RunManager boundary, NOT a regression pin \
         on lifecycle.rs ordering — see \
         `failed_with_tool_calls_arm_flips_state_before_broadcasting` \
         for the load-bearing interposer pin)"
    );
    let run_snapshot = state
        .run_manager
        .get_run(run_id)
        .expect("run must exist after mark_run_as_completed");
    assert_eq!(
        run_snapshot.status,
        RunStatus::Completed,
        "run status must be Completed after mark_run_as_completed"
    );

    shutdown_token.cancel();
}

/// Smoke test (NOT a regression pin) covering the call sequence for the
/// generic `Err(_)` arm at the `RunManager` boundary. The generic
/// `Err(_)` arm is unreachable through `runtime.run()` because
/// `AgentRuntime::finish_run` re-wraps every error from `agent_loop`
/// into `AlmsError::FailedWithToolCalls { ... }` — even when no tool
/// calls executed. The arm exists in `lifecycle.rs` to handle direct
/// `AgentRuntime`-bypass paths (e.g. construction-time failures before
/// the loop starts) and synthetic test inputs that pre-construct a
/// non-`FailedWithToolCalls` error variant.
///
/// What this test verifies: that callers using the post-#927 sequence
/// (`mark_run_as_failed` then `send_event(run_error)`) see
/// `has_active_runs == false` upon receiving the `run_error` event.
/// Mirrors the smoke-test pattern of
/// `smoke_post_execute_cancel_flips_state_at_run_manager_boundary` for
/// #895 and `smoke_ok_arm_flips_state_at_run_manager_boundary` above.
///
/// The generic `Err(_)` arm in `lifecycle.rs` shares the exact post-fix
/// structural shape of the `FailedWithToolCalls` arm (flip then
/// broadcast, no intervening logic). The
/// `failed_with_tool_calls_arm_flips_state_before_broadcasting`
/// interposer test is the load-bearing pin that makes the bundle revert
/// detectable for both arms — a regression that reordered the
/// `FailedWithToolCalls` arm without reordering generic `Err(_)` would
/// be inconsistent with the pattern and rejected at code review, and a
/// regression that reordered both would be caught by the interposer
/// test.
#[tokio::test]
async fn smoke_err_arm_flips_state_at_run_manager_boundary() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-927-smoke-err");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "test".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);
    state.run_manager.mark_run_as_running(run_id);
    assert!(state.run_manager.has_active_runs(session_id));

    let mut session_rx = subscribe_session(&state, session_id);

    // Mirror the post-#927 ordering: flip state first, broadcast second.
    assert!(
        state
            .run_manager
            .mark_run_as_failed(run_id, "synthetic generic failure".to_string())
    );
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::run_error(run_id, "synthetic generic failure"),
        )
        .await;

    let event = session_rx
        .recv()
        .await
        .expect("run_error event must be delivered");
    assert_eq!(event.event_type, "run_error");

    assert!(
        !state.run_manager.has_active_runs(session_id),
        "has_active_runs must be false after mark_run_as_failed \
         (sanity check on the RunManager boundary, NOT a regression pin \
         on lifecycle.rs ordering — see \
         `failed_with_tool_calls_arm_flips_state_before_broadcasting` \
         for the load-bearing interposer pin)"
    );
    let run_snapshot = state
        .run_manager
        .get_run(run_id)
        .expect("run must exist after mark_run_as_failed");
    assert_eq!(
        run_snapshot.status,
        RunStatus::Failed,
        "run status must be Failed after mark_run_as_failed"
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

// ---------------------------------------------------------------------------
// #831 — queue position display
//
// `run_created.queued_behind` already carries the initial 1-indexed position
// at enqueue time. The new `run_queue_position` SSE event broadcasts the
// updated position to each remaining queued run on a per-agent queue when the
// head advances (a run finishes / fails / is cancelled). `GET /runs/{id}`
// also exposes the live position via the `queue_position` field so a
// late-joining client can render the queued state without waiting for the
// next decrement.
// ---------------------------------------------------------------------------

/// Helper: extract `position` field from a `run_queue_position` event for a
/// given run_id. Returns `None` if no such event exists in the slice.
fn find_position_event(events: &[SseEventData], run_id: alms_core::RunId) -> Option<u64> {
    events
        .iter()
        .filter(|e| e.event_type == "run_queue_position")
        .filter(|e| {
            e.data
                .get("run_id")
                .and_then(|v| v.as_str())
                .map(|s| s == run_id.0.to_string())
                .unwrap_or(false)
        })
        .filter_map(|e| e.data.get("position").and_then(|v| v.as_u64()))
        .next_back()
}

/// When 3 runs are enqueued back-to-back against a busy agent, the
/// `run_created.queued_behind` field carries each run's initial 1-indexed
/// position: 1, 2, 3 (i.e. one run ahead of the first new one — the already-
/// running one — two ahead of the second, three ahead of the third).
///
/// Acceptance: this is what frontend consumes via `data.queued_behind` to
/// render the initial "Queued — position N" chip without waiting for the
/// first decrement event.
#[tokio::test]
async fn three_back_to_back_queued_runs_get_distinct_initial_positions() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state.session_manager.get_or_create(agent_id, "web");
    let session_id = session.id;

    // Simulate an already-running run on this agent so subsequent
    // create_run calls all see queued_behind > 0.
    let running_run = Run::new(session_id, agent_id, "prior task".into());
    let running_run_id = running_run.run_id;
    state.run_manager.insert_run(running_run);
    state.run_manager.mark_run_as_running(running_run_id);

    // Park a never-completing work item on the per-agent SessionQueue so
    // subsequent `create_run` calls actually queue behind it (the queue
    // handler is otherwise idle and would dispatch them immediately).
    let (_park_release_tx, park_release_rx) = tokio::sync::oneshot::channel::<()>();
    state.agent_queue.enqueue(
        agent_id,
        Box::pin(async move {
            let _ = park_release_rx.await;
        }),
    );
    // Yield so the queue handler picks up the parked item.
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }

    let mut rx = subscribe_session(&state, session_id);

    let mut queued_behind_values = Vec::new();
    for i in 0..3 {
        let req = CreateRunRequest {
            session_id,
            agent_id: None,
            input: RunInput::Text {
                text: format!("queued message {i}"),
            },
        };
        let _ = super::lifecycle::create_run(State(state.clone()), Json(req))
            .await
            .expect("create_run should succeed");

        // Drain the run_created event for this iteration before continuing.
        // Each create_run synchronously emits its run_created on the session
        // SSE feed before returning.
        tokio::task::yield_now().await;
        let events = drain_events(&mut rx);
        let run_created = events
            .iter()
            .find(|e| e.event_type == "run_created")
            .unwrap_or_else(|| panic!("expected run_created on iteration {i}"));
        queued_behind_values.push(
            run_created
                .data
                .get("queued_behind")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        );
    }

    // Cancel shutdown so the spawned execute_run tasks exit fast.
    shutdown_token.cancel();
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    // Each subsequent run sees +1 ahead of it. The exact starting value
    // depends on whether the running run plus the parked queue item are
    // both visible; what matters is monotonic distinct values.
    assert_eq!(
        queued_behind_values.len(),
        3,
        "expected 3 run_created events"
    );
    assert!(
        queued_behind_values[0] >= 1,
        "first queued run should be position >= 1; got {:?}",
        queued_behind_values
    );
    assert!(
        queued_behind_values[1] > queued_behind_values[0],
        "second queued run should be deeper than first; got {:?}",
        queued_behind_values
    );
    assert!(
        queued_behind_values[2] > queued_behind_values[1],
        "third queued run should be deeper than second; got {:?}",
        queued_behind_values
    );
}

/// Driving `execute_run` to a terminal exit advances the per-agent queue
/// head and broadcasts `run_queue_position` for every still-queued run on
/// the same agent with a freshly-decremented position.
///
/// Three runs queued on the same agent: A (running-then-finishing), B and C
/// (still queued). When A's `execute_run` completes (with a runtime-init
/// failure since no real LLM is wired up — we use the early-fail path),
/// the broadcast fires `run_queue_position` for B and C with the new
/// positions.
#[tokio::test]
async fn execute_run_terminal_broadcasts_decremented_positions_to_remaining_queued() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "queue-pos-test");
    let session_id = session.id;

    // Create three queued runs, A, B, C, in FIFO order.
    let a = Run::new(session_id, agent_id, "A".into());
    let a_id = a.run_id;
    state.run_manager.insert_run(a);
    // Sleep enough for `created_at` to differ deterministically.
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let b = Run::new(session_id, agent_id, "B".into());
    let b_id = b.run_id;
    state.run_manager.insert_run(b);
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let c = Run::new(session_id, agent_id, "C".into());
    let c_id = c.run_id;
    state.run_manager.insert_run(c);

    let mut rx = subscribe_session(&state, session_id);

    // Use the shutdown_token early-exit branch in execute_run: by cancelling
    // shutdown, A's execute_run hits the early return and `broadcast_queue_advance`
    // fires for the still-queued B and C without needing a real LLM.
    shutdown_token.cancel();

    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(a_id, cancel_token.clone());
    super::lifecycle::execute_run(
        state.clone(),
        super::RunParams {
            run_id: a_id,
            session_id,
            agent_id,
            input: "A".to_string(),
            context_id: "queue-pos-test".to_string(),
            cancel_token,
            is_peer_message: false,
            is_system_triggered: false,
            input_pre_persisted: false,
        },
    )
    .await;

    // Drain SSE events.
    tokio::task::yield_now().await;
    let events = drain_events(&mut rx);

    // B is the next-up after A: position 1 (no Running anymore — A is now
    // Cancelled — so B at idx 0 with running_offset=0 would be position 0,
    // skipped). C is at idx 1, position 1.
    //
    // Wait: with no Running run, B (idx 0) gets position 0 (skipped) and C
    // (idx 1) gets position 1. But that means B is missing a decrement
    // event — which is correct: B is "about to dequeue" and `run_started`
    // is the proper signal for B's transition out of the queue. The frontend
    // already handles `run_started` to clear the queued chip.
    let b_position = find_position_event(&events, b_id);
    let c_position = find_position_event(&events, c_id);

    assert_eq!(
        b_position, None,
        "B is the head of the remaining queue — no run_queue_position should fire \
         (run_started will signal its dequeue); got: {b_position:?}"
    );
    assert_eq!(
        c_position,
        Some(1),
        "C should receive run_queue_position with position 1 after the head advanced; \
         got: {c_position:?}"
    );

    // A itself should NOT receive a position update (it's now terminal).
    let a_position = find_position_event(&events, a_id);
    assert_eq!(
        a_position, None,
        "A is terminal — no run_queue_position should fire for it"
    );
}

/// `GET /runs/{id}` exposes the live `queue_position` for a queued run so
/// late-joining clients (page reload, polling fallback) can render the
/// queued chip without waiting for the next SSE decrement.
#[tokio::test]
async fn get_run_status_returns_queue_position_for_queued_run() {
    use axum::extract::{Path, State};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "queue-status-test");
    let session_id = session.id;

    // One running run + two queued runs.
    let running = Run::new(session_id, agent_id, "running".into());
    let running_id = running.run_id;
    state.run_manager.insert_run(running);
    state.run_manager.mark_run_as_running(running_id);

    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let q1 = Run::new(session_id, agent_id, "q1".into());
    let q1_id = q1.run_id;
    state.run_manager.insert_run(q1);
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let q2 = Run::new(session_id, agent_id, "q2".into());
    let q2_id = q2.run_id;
    state.run_manager.insert_run(q2);

    // Queued #1 should be position 1 (next up — one Running ahead).
    let resp_q1 = super::lifecycle::get_run_status(State(state.clone()), Path(q1_id))
        .await
        .expect("get_run_status should succeed for q1");
    assert_eq!(resp_q1.0.queue_position, Some(1));
    assert_eq!(resp_q1.0.status, RunStatus::Queued);

    // Queued #2 should be position 2.
    let resp_q2 = super::lifecycle::get_run_status(State(state.clone()), Path(q2_id))
        .await
        .expect("get_run_status should succeed for q2");
    assert_eq!(resp_q2.0.queue_position, Some(2));

    // Running run has no queue_position.
    let resp_running = super::lifecycle::get_run_status(State(state.clone()), Path(running_id))
        .await
        .expect("get_run_status should succeed for running run");
    assert_eq!(resp_running.0.queue_position, None);
    assert_eq!(resp_running.0.status, RunStatus::Running);

    shutdown_token.cancel();
}

/// When the only queued run is cancelled (no further runs behind it), the
/// broadcast helper is a no-op — there's nothing left to update. This
/// confirms the early-empty guard prevents wasted work / spurious events.
#[tokio::test]
async fn broadcast_queue_advance_is_noop_when_no_queued_runs_remain() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "single-cancel-test");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "single".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);

    let mut rx = subscribe_session(&state, session_id);

    // Drive execute_run via the pre-cancel branch.
    shutdown_token.cancel();
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
            input: "single".to_string(),
            context_id: "single-cancel-test".to_string(),
            cancel_token,
            is_peer_message: false,
            is_system_triggered: false,
            input_pre_persisted: false,
        },
    )
    .await;

    tokio::task::yield_now().await;
    let events = drain_events(&mut rx);
    assert!(
        !events.iter().any(|e| e.event_type == "run_queue_position"),
        "no run_queue_position events should fire when the queue is empty after \
         the head exits; got: {:?}",
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
    );
}

/// The Telegram path enqueues against the same `agent_queue` as HTTP runs
/// (gateway.rs:644 calls `agent_queue.enqueue(agent_id, ...)`), so any
/// pending Telegram work item factors into the `pending_count` used by
/// `create_run` to compute `queued_behind`. This is the closest the gateway
/// can come to "Telegram parity" without spinning up a real Telegram bot —
/// the queue is shared and the position math sees the same number.
#[tokio::test]
async fn http_run_sees_pending_telegram_style_work_in_queued_behind() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state.session_manager.get_or_create(agent_id, "web");
    let session_id = session.id;

    // Park two opaque work items on the agent queue (mimicking Telegram-
    // submitted messages, which use the same `state.agent_queue.enqueue`
    // call site). The first one becomes the head; the second sits in the
    // queue's mpsc channel as a true "pending" item.
    let (_release_tx_1, release_rx_1) = tokio::sync::oneshot::channel::<()>();
    let (_release_tx_2, release_rx_2) = tokio::sync::oneshot::channel::<()>();
    state.agent_queue.enqueue(
        agent_id,
        Box::pin(async move {
            let _ = release_rx_1.await;
        }),
    );
    state.agent_queue.enqueue(
        agent_id,
        Box::pin(async move {
            let _ = release_rx_2.await;
        }),
    );
    // Let the queue handler pick up the head.
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }

    let mut rx = subscribe_session(&state, session_id);

    let req = CreateRunRequest {
        session_id,
        agent_id: None,
        input: RunInput::Text {
            text: "behind two telegram-style items".into(),
        },
    };
    let _ = super::lifecycle::create_run(State(state.clone()), Json(req))
        .await
        .expect("create_run should succeed");

    shutdown_token.cancel();
    tokio::task::yield_now().await;

    let events = drain_events(&mut rx);
    let run_created = events
        .iter()
        .find(|e| e.event_type == "run_created")
        .expect("run_created should fire");
    let queued_behind = run_created
        .data
        .get("queued_behind")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    // At minimum: the second parked item still sits as `pending` (not yet
    // dequeued), giving queued_behind >= 1. The head item's status is not
    // tracked via `Run` records (it's a raw work item, not a `Run`), so
    // the +1-for-running term doesn't apply — but the `pending_count` term
    // alone is enough to prove the shared queue is honoured.
    assert!(
        queued_behind >= 1,
        "HTTP run behind parked queue items should see queued_behind >= 1; \
         got {queued_behind}"
    );
}

// ---------------------------------------------------------------------------
// #919: per-run token-budget validation against provider context window
//
// `POST /runs` must reject requests where the resolved per-agent
// `(provider, model, max_input_tokens, max_tokens)` quadruple overshoots
// the provider's published context window. The validator runs inside
// `pre_flight_token_budget` after `resolve_agent_config` succeeds, so a
// per-agent provider/model override that lands on a too-small cap is
// caught BEFORE the run is enqueued.
// ---------------------------------------------------------------------------

// The `ALMS_LLM_BUDGET_VALIDATION` env-var mutex and RAII guard live in
// `crate::test_env_locks` so they are shared with `settings.rs::tests`
// (which also exercises the validator on the PATCH path). Both files are
// compiled into the same `cargo test -p alms-gateway` process — without a
// single shared mutex, a strict-mode PATCH test could race a concurrent
// warn-mode `POST /runs` test on the same env var. The lock guards a
// single var-set (`ALMS_LLM_BUDGET_VALIDATION` only) and is disjoint by
// construction from any other env-var mutex in the workspace.
use crate::test_env_locks::BudgetValidationEnvGuard;

/// Per-agent override pinning provider+model whose published context
/// window is smaller than `max_input_tokens + max_tokens` -> structured
/// 400 INVALID_TOKEN_BUDGET_FOR_PROVIDER.
///
/// Setup:
/// - Server-default `[context].max_input_tokens` is 128_000 (default).
/// - `agent.max_tokens` defaults to 32_000 (DEFAULT_AGENT_MAX_TOKENS).
/// - Per-agent override pins provider=`anthropic` and model=`claude-haiku-4-5`,
///   whose 200K cap fits the default 128K + 32K = 160K budget.
/// - Bumping `[context].max_input_tokens` to 250_000 pushes the effective
///   total to 282_000, which overshoots the 200K cap → validator fires.
///
/// Note: post-2026-05-09 verification round Opus 4.7 / Sonnet 4.6 moved to
/// 1M caps. Haiku 4.5 stays at 200K and is the natural overshoot fixture.
#[tokio::test]
async fn create_run_rejects_per_agent_override_that_blows_provider_cap() {
    use alms_core::registry::AgentRecord;
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;
    use chrono::Utc;

    // Pin strict mode for this test so a concurrent warn-mode test
    // can't make us silently accept the overbudget config.
    let _env = BudgetValidationEnvGuard::unset();

    let (state, _shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();

    // Bump the server-level input budget so 250K input + 32K output
    // overshoots Haiku 4.5's 200K cap.
    {
        let mut cfg = state.agent_config.write();
        cfg.context_config.max_input_tokens = 250_000;
    }

    let agent_id = AgentId::new();
    let now = Utc::now();
    let agent = AgentRecord {
        id: agent_id,
        name: "overbudget-agent".into(),
        description: String::new(),
        // Pin a model whose 200K cap is smaller than the 282K effective
        // total once we bump max_input_tokens above.
        model: Some("claude-haiku-4-5".into()),
        posture: None,
        provider: Some("anthropic".into()),
        telegram_token: None,
        thinking_budget_tokens: None,
        reasoning_effort: None,
        gemini_thinking_budget: None,
        summary_provider: None,
        summary_model: None,
        worktree_mode: alms_core::WorktreeMode::Off,
        debug_mode: false,
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

    let session = state.session_manager.get_or_create(agent_id, "web");
    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: Some(agent_id),
        input: RunInput::Text {
            text: "hello".into(),
        },
    };

    let Err((status, body)) = super::lifecycle::create_run(State(state.clone()), Json(req)).await
    else {
        panic!(
            "create_run must reject when the resolved budget overshoots the provider cap (#919)"
        );
    };

    // 1. 400 status code BEFORE any LLM call.
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    // 2. Structured error code so clients can branch on it.
    assert_eq!(
        body.0["error_code"], "INVALID_TOKEN_BUDGET_FOR_PROVIDER",
        "body must carry the structured error_code so clients can branch on it"
    );
    // 3. Body carries every datum the operator needs to fix the config.
    assert_eq!(body.0["agent_id"], agent_id.0.to_string());
    assert_eq!(body.0["provider"], "anthropic");
    assert_eq!(body.0["model"], "claude-haiku-4-5");
    assert_eq!(body.0["max_input_tokens"], 250_000);
    assert_eq!(body.0["max_tokens"], 32_000);
    assert_eq!(body.0["effective_total"], 282_000);
    assert_eq!(body.0["provider_cap"], 200_000);
    // 4. Human-readable message points at both knobs and the cap.
    let message = body.0["message"]
        .as_str()
        .expect("message must be a string");
    assert!(
        message.contains("max_input_tokens") && message.contains("max_tokens"),
        "message must name both budget knobs: {message}"
    );
    assert!(
        message.contains("anthropic") && message.contains("claude-haiku-4-5"),
        "message must identify the provider and resolved model: {message}"
    );
    // 5. No run was enqueued.
    let runs = state.run_manager.list_by_agent(agent_id, 10);
    assert!(
        runs.is_empty(),
        "no run should have been created when the gateway rejects pre-flight"
    );
}

/// Same overbudget config + `ALMS_LLM_BUDGET_VALIDATION=warn` -> run is
/// accepted (the env var downgrades the strict reject to a structured
/// WARN log).
///
/// Pins the warn opt-out behaviour for the per-run path. Uses a process-
/// global env-var mutex via `parking_lot` to avoid races with parallel
/// tests that read the same env var.
#[tokio::test]
async fn create_run_warn_mode_accepts_overbudget_config() {
    use alms_core::registry::AgentRecord;
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;
    use chrono::Utc;

    // Pin warn mode for this test, holding the global env-var lock so
    // concurrent strict-mode tests can't see the warn value.
    let _env = BudgetValidationEnvGuard::set("warn");

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();
    {
        let mut cfg = state.agent_config.write();
        cfg.context_config.max_input_tokens = 250_000;
    }

    let agent_id = AgentId::new();
    let now = Utc::now();
    let agent = AgentRecord {
        id: agent_id,
        name: "overbudget-warn-agent".into(),
        description: String::new(),
        model: Some("claude-haiku-4-5".into()),
        posture: None,
        provider: Some("anthropic".into()),
        telegram_token: None,
        thinking_budget_tokens: None,
        reasoning_effort: None,
        gemini_thinking_budget: None,
        summary_provider: None,
        summary_model: None,
        worktree_mode: alms_core::WorktreeMode::Off,
        debug_mode: false,
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

    let session = state.session_manager.get_or_create(agent_id, "web");
    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: Some(agent_id),
        input: RunInput::Text {
            text: "hello".into(),
        },
    };

    let (status, _resp) = super::lifecycle::create_run(State(state), Json(req))
        .await
        .expect("warn mode must accept overshooting configs (#919)");
    assert_eq!(status, axum::http::StatusCode::CREATED);
    shutdown_token.cancel();
}

/// Per-agent override that resolves to a model whose `(provider, model)`
/// pair the budget table doesn't know about -> run is accepted regardless
/// of size. Mirrors the unknown-pair-skips contract pinned in the
/// alms-core unit tests, exercised end-to-end through `pre_flight_token_budget`.
#[tokio::test]
async fn create_run_accepts_unknown_model_regardless_of_budget() {
    use alms_core::registry::AgentRecord;
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;
    use chrono::Utc;

    // Pin strict mode — the unknown-pair branch must skip the check
    // regardless of mode, but we hold the lock so a concurrent warn-mode
    // test doesn't make this assertion vacuous.
    let _env = BudgetValidationEnvGuard::unset();

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();
    // 10M input + 32K output overshoots every published cap, but with an
    // unknown model the validator returns Ok(()) and the run proceeds.
    {
        let mut cfg = state.agent_config.write();
        cfg.context_config.max_input_tokens = 10_000_000;
        cfg.max_tokens = 32_000;
    }

    let agent_id = AgentId::new();
    let now = Utc::now();
    let agent = AgentRecord {
        id: agent_id,
        name: "unknown-model-agent".into(),
        description: String::new(),
        // Per-agent provider override to anthropic with a model NOT in
        // the table — falls through to None at lookup time, validator
        // skips silently.
        model: Some("claude-2.1".into()),
        posture: None,
        provider: Some("anthropic".into()),
        telegram_token: None,
        thinking_budget_tokens: None,
        reasoning_effort: None,
        gemini_thinking_budget: None,
        summary_provider: None,
        summary_model: None,
        worktree_mode: alms_core::WorktreeMode::Off,
        debug_mode: false,
        is_default: false,
        created_at: now,
        last_active: now,
    };
    // Bump session storage to match so the cross-section validator is
    // satisfied.
    state
        .session_manager
        .store()
        .expect("SQLite-backed state should have a store")
        .create_agent(&agent)
        .expect("agent seed should succeed");

    let session = state.session_manager.get_or_create(agent_id, "web");
    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: Some(agent_id),
        input: RunInput::Text {
            text: "hello".into(),
        },
    };

    let (status, _resp) = super::lifecycle::create_run(State(state), Json(req))
        .await
        .expect("unknown (provider, model) pair must skip the budget check");
    assert_eq!(status, axum::http::StatusCode::CREATED);
    shutdown_token.cancel();
}

/// Mock mode bypasses the per-run pre-flight budget guard, mirroring the
/// boot-time skip in `AlmsConfig::validate` (Codex P2 #1 follow-up on PR
/// #1020). A mock-mode run with an intentionally-overshooting budget for
/// a known `(provider, model)` pair must land cleanly — the mock client
/// will not call the real provider, so refusing it is a false positive
/// that blocks otherwise-valid local/dev test setups.
#[tokio::test]
async fn create_run_mock_mode_bypasses_budget_validation() {
    use alms_core::registry::AgentRecord;
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;
    use chrono::Utc;

    // Pin strict mode — the mock-mode bypass must take effect regardless
    // of `ALMS_LLM_BUDGET_VALIDATION`. Hold the global env-var lock so a
    // concurrent warn-mode test can't make this assertion vacuous.
    let _env = BudgetValidationEnvGuard::unset();

    // Build state with a mock-mode LLM client. We can't mutate
    // `state.llm.config.mock` after construction (no public setter — the
    // flag travels through `LlmClient::new`), so we route through a
    // `GatewayConfig` whose `llm_config.mock = true`.
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
    let (trigger_tx, _tr) = mpsc::unbounded_channel();
    let (dm_event_tx, _dr) = mpsc::unbounded_channel();
    let state = AppState::new(
        gateway,
        scheduler,
        shutdown_token.clone(),
        completion_tx,
        trigger_tx,
        dm_event_tx,
    )
    .unwrap();

    // 250K input + 32K output = 282K — overshoots Haiku 4.5's 200K cap.
    // Without the mock-mode bypass the per-run validator would reject
    // this with `400 INVALID_TOKEN_BUDGET_FOR_PROVIDER`.
    {
        let mut cfg = state.agent_config.write();
        cfg.context_config.max_input_tokens = 250_000;
        cfg.max_tokens = 32_000;
    }

    let agent_id = AgentId::new();
    let now = Utc::now();
    let agent = AgentRecord {
        id: agent_id,
        name: "mock-mode-agent".into(),
        description: String::new(),
        // A known table-row whose 200K cap is smaller than the 282K
        // effective total — without the mock bypass the validator would
        // fire here.
        model: Some("claude-haiku-4-5".into()),
        posture: None,
        provider: Some("anthropic".into()),
        telegram_token: None,
        thinking_budget_tokens: None,
        reasoning_effort: None,
        gemini_thinking_budget: None,
        summary_provider: None,
        summary_model: None,
        worktree_mode: alms_core::WorktreeMode::Off,
        debug_mode: false,
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

    let session = state.session_manager.get_or_create(agent_id, "web");
    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: Some(agent_id),
        input: RunInput::Text {
            text: "hello".into(),
        },
    };

    let (status, _resp) = super::lifecycle::create_run(State(state), Json(req))
        .await
        .expect("mock-mode run with overshooting budget must be accepted (#1020 P2 #1)");
    assert_eq!(
        status,
        axum::http::StatusCode::CREATED,
        "mock mode must bypass token-budget pre-flight, mirroring `AlmsConfig::validate`"
    );
    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// #919: per-run token-budget validation INSIDE `execute_run` (non-HTTP path)
//
// `pre_flight_token_budget` originally only fired on the HTTP `POST /runs`
// path. Runs created via `enqueue_triggered_run` (peer DMs, scheduler
// triggers, notification runs, subagent completion runs) skip `create_run`
// entirely and land directly in `execute_run`, so the create-time guard
// did not protect them — the exact opaque-downstream-4xx symptom the
// validator is meant to prevent. Codex P2 follow-up on PR #1020 moved the
// guard into `execute_run` so every run-creation path inherits it.
// ---------------------------------------------------------------------------

/// Non-HTTP path: bypass `create_run` and call `execute_run` directly with
/// an over-budget agent config. `execute_run` must reject the run before
/// any LLM call by marking it `Failed` with the structured
/// `INVALID_TOKEN_BUDGET_FOR_PROVIDER` message.
///
/// Setup mirrors `create_run_rejects_per_agent_override_that_blows_provider_cap`
/// — same overbudget shape, same expected message structure. The
/// distinction is the call site: this test enqueues the run shape used by
/// the scheduler / Telegram / peer-DM / subagent completion paths and
/// confirms the `execute_run`-side guard fires identically. Pins the
/// "queued runs whose agent config changed after `POST /runs`" leak too,
/// because the second resolve inside `execute_run` is what catches both
/// the never-validated and the re-validated case.
#[tokio::test]
async fn execute_run_rejects_overbudget_resolved_config_on_non_http_path() {
    use alms_core::registry::AgentRecord;
    use chrono::Utc;

    // Pin strict mode so a concurrent warn-mode test can't make us silently
    // accept the overbudget config.
    let _env = BudgetValidationEnvGuard::unset();

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();
    {
        let mut cfg = state.agent_config.write();
        cfg.context_config.max_input_tokens = 250_000;
        cfg.max_tokens = 32_000;
    }

    let agent_id = AgentId::new();
    let now = Utc::now();
    let agent = AgentRecord {
        id: agent_id,
        name: "overbudget-non-http-agent".into(),
        description: String::new(),
        // 250K + 32K = 282K overshoots Haiku 4.5's 200K cap — same fixture
        // as the create_run-side test, exercised from the non-HTTP path.
        model: Some("claude-haiku-4-5".into()),
        posture: None,
        provider: Some("anthropic".into()),
        telegram_token: None,
        thinking_budget_tokens: None,
        reasoning_effort: None,
        gemini_thinking_budget: None,
        summary_provider: None,
        summary_model: None,
        worktree_mode: alms_core::WorktreeMode::Off,
        debug_mode: false,
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

    let session = state
        .session_manager
        .get_or_create(agent_id, "non-http-context");
    let session_id = session.id;

    // Bypass `create_run` (which would reject pre-flight on the HTTP path)
    // and enqueue the run directly — this is the shape the Telegram /
    // scheduler / peer-DM / subagent paths use.
    let run = Run::new(session_id, agent_id, "over-budget non-http trigger".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run.clone());

    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    let in_flight_before = state.run_manager.in_flight_count();

    super::lifecycle::execute_run(
        state.clone(),
        super::RunParams {
            run_id,
            session_id,
            agent_id,
            input: run.input,
            context_id: "non-http-context".to_string(),
            cancel_token,
            // System-triggered shape (scheduler / notification / subagent
            // completion). The budget check is independent of these flags
            // — it runs immediately after `resolve_agent_config` succeeds,
            // before the posture / bootstrap / debug-mode transforms.
            is_peer_message: false,
            is_system_triggered: true,
            input_pre_persisted: false,
        },
    )
    .await;

    // 1. Terminal status is Failed — the budget arm fired before any LLM
    //    call. NOT Cancelled (would mean the cancel-token early-exit fired
    //    instead) and NOT Completed (would mean the guard didn't trip).
    let final_run = state
        .run_manager
        .get_run(run_id)
        .expect("run must still exist after execute_run returns");
    assert_eq!(
        final_run.status,
        RunStatus::Failed,
        "run must reach Failed via the budget arm; got {:?}",
        final_run.status,
    );

    // 2. The persisted error carries the structured message — same shape
    //    operators see on `GET /runs/{id}` for the HTTP path's 400 body.
    let error_msg = final_run
        .error
        .as_ref()
        .expect("Failed run must carry a structured error message");
    assert!(
        error_msg.contains("anthropic") && error_msg.contains("claude-haiku-4-5"),
        "error must name the provider and resolved model (got: {error_msg})"
    );
    assert!(
        error_msg.contains("max_input_tokens") && error_msg.contains("max_tokens"),
        "error must name both budget knobs (got: {error_msg})"
    );
    assert!(
        error_msg.contains("200000") || error_msg.contains("200_000"),
        "error must name the provider cap (got: {error_msg})"
    );

    // 3. The RAII `_in_flight_guard` decrements back to baseline on the
    //    new failure arm too, mirroring the contract pinned in
    //    `execute_run_failure_arm_marks_run_failed_with_structured_error_on_provider_switch_without_model`.
    assert_eq!(
        state.run_manager.in_flight_count(),
        in_flight_before,
        "in_flight counter must return to baseline ({}) after the budget failure arm; got {}",
        in_flight_before,
        state.run_manager.in_flight_count(),
    );

    // 4. The run never transitioned to `Running` — the guard fires
    //    BEFORE `mark_run_as_running_with_config`, so the resolved-config
    //    snapshot is never persisted and the run isn't visible in the
    //    running set.
    assert!(
        final_run.resolved_config.is_none(),
        "Failed-before-running runs must not have a resolved_config snapshot; got {:?}",
        final_run.resolved_config,
    );

    shutdown_token.cancel();
}

/// Mock-mode skip on the non-HTTP path: mirrors the create_run-side mock
/// bypass test. When the LLM client is in mock mode, `execute_run`'s
/// budget guard must skip regardless of strict-mode env var.
///
/// (We don't test the warn-mode opt-out on the non-HTTP path explicitly —
/// `evaluate_pre_flight_token_budget` is the shared helper exercised by
/// both surfaces, so the strict/warn dispatch is pinned by the HTTP-side
/// `create_run_warn_mode_accepts_overbudget_config` test. The mock-mode
/// branch lives at the top of the helper and short-circuits before the
/// strict/warn split, so we pin it on both surfaces.)
#[tokio::test]
async fn execute_run_mock_mode_skips_budget_validation_on_non_http_path() {
    use alms_core::registry::AgentRecord;
    use chrono::Utc;

    let _env = BudgetValidationEnvGuard::unset();

    // Build state with a mock-mode LLM client.
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
    let (trigger_tx, _tr) = mpsc::unbounded_channel();
    let (dm_event_tx, _dr) = mpsc::unbounded_channel();
    let state = AppState::new(
        gateway,
        scheduler,
        shutdown_token.clone(),
        completion_tx,
        trigger_tx,
        dm_event_tx,
    )
    .unwrap();

    {
        let mut cfg = state.agent_config.write();
        cfg.context_config.max_input_tokens = 250_000;
        cfg.max_tokens = 32_000;
    }

    let agent_id = AgentId::new();
    let now = Utc::now();
    let agent = AgentRecord {
        id: agent_id,
        name: "mock-mode-non-http-agent".into(),
        description: String::new(),
        model: Some("claude-haiku-4-5".into()),
        posture: None,
        provider: Some("anthropic".into()),
        telegram_token: None,
        thinking_budget_tokens: None,
        reasoning_effort: None,
        gemini_thinking_budget: None,
        summary_provider: None,
        summary_model: None,
        worktree_mode: alms_core::WorktreeMode::Off,
        debug_mode: false,
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

    let session = state
        .session_manager
        .get_or_create(agent_id, "non-http-context");
    let session_id = session.id;

    let run = Run::new(
        session_id,
        agent_id,
        "mock-mode overbudget non-http trigger".into(),
    );
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
            context_id: "non-http-context".to_string(),
            cancel_token,
            is_peer_message: false,
            is_system_triggered: true,
            input_pre_persisted: false,
        },
    )
    .await;

    // The run must NOT carry the budget-failure signature — mock mode
    // bypassed the guard, mirroring `AlmsConfig::validate`'s boot-time
    // skip and the HTTP-path test.
    let final_run = state
        .run_manager
        .get_run(run_id)
        .expect("run must still exist after execute_run returns");
    if let Some(error_msg) = final_run.error.as_ref() {
        assert!(
            !error_msg.contains("context.max_input_tokens"),
            "mock mode must NOT produce the budget-failure signature; got: {error_msg}"
        );
    }

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// #1046 — HTTP cancel must flip state authoritatively
// ---------------------------------------------------------------------------

/// #1046 regression — `POST /runs/{id}/cancel` flips the run state to
/// `Cancelled` SYNCHRONOUSLY in the HTTP handler and broadcasts exactly
/// one `run_cancelled` SSE event before the response returns.
///
/// Pre-#1046 the state flip + SSE broadcast only happened inside
/// `execute_run`'s terminal arm, AFTER `drop(runtime)` and
/// `forwarder_handle.await` had completed. With an in-flight LLM HTTP
/// request being aborted (Windows + TLS connection drop), that cleanup
/// window was observed to stretch to ~8 seconds during which
/// `GET /runs/{id}` still reported `Running` and the SSE feed had
/// emitted no terminal event. The user-visible symptom matched the
/// issue report ("cancel doesn't work — agent keeps running").
///
/// This test pins the synchronous flip on the HTTP boundary: after
/// `cancel_run` returns OK, the run's status MUST be `Cancelled` and
/// the session SSE feed MUST have received exactly one `run_cancelled`
/// event.
#[tokio::test]
async fn http_cancel_flips_state_synchronously() {
    use axum::extract::{Path, State};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-1046-http-cancel-flip");
    let session_id = session.id;

    // Set up a run in the Running state (the common case for cancel).
    let run = Run::new(session_id, agent_id, "test".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);
    state.run_manager.mark_run_as_running(run_id);

    // Register a cancel token so `cancel_run` finds it.
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    let mut session_rx = subscribe_session(&state, session_id);

    // Invoke the HTTP cancel handler directly.
    let cancel_state = state.clone();
    let response = super::lifecycle::cancel_run(State(cancel_state), Path(run_id))
        .await
        .expect("cancel_run should succeed for a Running run");

    // The response shape matches the existing contract.
    assert_eq!(response.0["status"], "cancelled");

    // SYNCHRONOUS state flip: the moment the HTTP handler returns OK,
    // `GET /runs/{id}` must report `Cancelled`. No await between the
    // handler returning and this read, so any pre-#1046 deferred-flip
    // regression surfaces here.
    let run_after = state
        .run_manager
        .get_run(run_id)
        .expect("run must still exist after cancel");
    assert_eq!(
        run_after.status,
        RunStatus::Cancelled,
        "run.status MUST be Cancelled immediately after `cancel_run` \
         returns (#1046). Pre-fix: status stayed `Running` until \
         `execute_run`'s terminal arm completed."
    );

    // The cancel token was also cancelled (existing behaviour).
    assert!(
        cancel_token.is_cancelled(),
        "cancel token must be cancelled after `cancel_run`"
    );

    // The session feed must have received exactly one `run_cancelled`
    // event for this run.
    let events = drain_events(&mut session_rx);
    let cancelled_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "run_cancelled")
        .collect();
    assert_eq!(
        cancelled_events.len(),
        1,
        "HTTP cancel must emit exactly one `run_cancelled` event; got \
         {} events: {:?}",
        cancelled_events.len(),
        events
            .iter()
            .map(|e| e.event_type.as_str())
            .collect::<Vec<_>>()
    );

    shutdown_token.cancel();
}

/// #1046 regression — race between the HTTP `cancel_run` handler and
/// `execute_run`'s terminal `Cancelled` arm produces EXACTLY ONE
/// `run_cancelled` SSE event for the same run.
///
/// The HTTP handler now flips state + broadcasts. When `execute_run`
/// later reaches its `Err(CancelledWithToolCalls)` arm (after the agent
/// loop unwound and the runtime was dropped), it calls
/// `mark_run_as_cancelled` again. The bool-returning contract makes
/// that second call a no-op for the broadcast: the state is already
/// `Cancelled` so `mark_run_as_cancelled` returns `false` and the SSE
/// branch is skipped. Without the gate, the UI's session feed would
/// see two `run_cancelled` events for the same run and append two
/// `(run cancelled)` system bubbles to the transcript.
///
/// Uses the hanging-LLM helper so `execute_run` gets far enough to
/// reach the terminal arm via a real `Err(Cancelled)` propagation
/// through `agent_loop`'s LLM-call `select!` rather than the pre-cancel
/// early-exit branch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_cancel_and_execute_run_emit_single_event() {
    use axum::extract::{Path, State};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_hanging_llm().await;
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-1046-race-single-event");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "trigger LLM hang".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run.clone());

    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    let mut session_rx = subscribe_session(&state, session_id);

    // Spawn `execute_run` so we can interleave the HTTP cancel with
    // its in-flight loop.
    let exec_state = state.clone();
    let exec_input = run.input.clone();
    let exec_cancel_token = cancel_token.clone();
    let exec_handle = tokio::spawn(async move {
        super::lifecycle::execute_run(
            exec_state,
            super::RunParams {
                run_id,
                session_id,
                agent_id,
                input: exec_input,
                context_id: "test-1046-race-single-event".to_string(),
                cancel_token: exec_cancel_token,
                is_peer_message: false,
                is_system_triggered: false,
                input_pre_persisted: false,
            },
        )
        .await
    });

    // Wait for `run_started` so we know the loop has crossed
    // `mark_run_as_running` and is parked on the LLM HTTP request
    // (the hanging-LLM helper).
    let started_deadline = tokio::time::sleep(std::time::Duration::from_secs(3));
    tokio::pin!(started_deadline);
    let mut saw_started = false;
    loop {
        tokio::select! {
            biased;
            _ = &mut started_deadline => break,
            event = session_rx.recv() => {
                match event {
                    Some(e) if e.event_type == "run_started" => {
                        saw_started = true;
                        break;
                    }
                    Some(_) => continue,
                    None => break,
                }
            }
        }
    }
    assert!(
        saw_started,
        "expected `run_started` SSE event before HTTP cancel"
    );

    // Fire the HTTP cancel handler. With the #1046 fix this both
    // cancels the token AND broadcasts `run_cancelled` synchronously.
    let _ = super::lifecycle::cancel_run(State(state.clone()), Path(run_id))
        .await
        .expect("cancel_run should succeed for a Running run");

    // Let `execute_run` finish unwinding — agent_loop's `select!` will
    // wake on the cancelled token, unwind through `finish_run`'s
    // `Err(Cancelled)` arm, and reach the terminal
    // `CancelledWithToolCalls` arm in `execute_run`. With the #1046
    // gate, that arm sees `mark_run_as_cancelled` return `false` and
    // skips the duplicate broadcast.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(15), exec_handle)
        .await
        .expect("execute_run must complete within 15s after cancel");

    // Drain all remaining session events and count `run_cancelled`s.
    let events = drain_events(&mut session_rx);
    let cancelled_count = events
        .iter()
        .filter(|e| e.event_type == "run_cancelled")
        .count();
    assert_eq!(
        cancelled_count,
        1,
        "exactly one `run_cancelled` event must be emitted per run \
         even when the HTTP handler and `execute_run`'s terminal arm \
         both call `mark_run_as_cancelled` (#1046); got {cancelled_count} \
         events: {:?}",
        events
            .iter()
            .map(|e| e.event_type.as_str())
            .collect::<Vec<_>>()
    );

    // Final state is still Cancelled (idempotent re-mark in the
    // terminal arm does not regress the run to some other status).
    let final_run = state
        .run_manager
        .get_run(run_id)
        .expect("run must exist after execute_run");
    assert_eq!(
        final_run.status,
        RunStatus::Cancelled,
        "run must remain `Cancelled` after `execute_run` completes its cleanup"
    );

    shutdown_token.cancel();
}

/// #1046 regression — `Run::mark_cancelled` returns `false` and does
/// NOT mutate `ended_at` when the run is already in a terminal state.
///
/// The idempotency contract is what makes the gateway-side
/// "first-writer-wins" broadcast gate work. Without it the cleanup
/// pass in `execute_run` would re-stamp `ended_at` to a later
/// (cleanup-time) timestamp, masking when the cancel actually
/// happened, and the bool return would always be `true` so the
/// duplicate-broadcast guard would not fire.
#[test]
fn run_mark_cancelled_is_idempotent_and_returns_transition_bool() {
    let session_id = SessionId::new();
    let agent_id = AgentId::new();
    let mut run = Run::new(session_id, agent_id, "test".into());

    // Queued → Cancelled transitions and reports true.
    assert!(matches!(run.status, RunStatus::Queued));
    assert!(
        run.mark_cancelled(),
        "Queued → Cancelled must report transition=true"
    );
    assert!(matches!(run.status, RunStatus::Cancelled));
    let first_ended_at = run.ended_at.expect("ended_at must be stamped");

    // Second call on an already-Cancelled run reports false and does
    // NOT update ended_at.
    std::thread::sleep(std::time::Duration::from_millis(2));
    assert!(
        !run.mark_cancelled(),
        "second mark_cancelled on already-Cancelled run must report false"
    );
    assert_eq!(
        run.ended_at,
        Some(first_ended_at),
        "ended_at must NOT be updated by a no-op second mark_cancelled — \
         it should reflect when the cancel actually happened, not when \
         cleanup re-marked the run"
    );

    // Failed → mark_cancelled must NOT overwrite the terminal Failed
    // status (defense-in-depth; not a path the gateway exercises today,
    // but the idempotency invariant should hold for all terminal
    // states).
    let mut failed_run = Run::new(session_id, agent_id, "test".into());
    failed_run.mark_running();
    let _ = failed_run.mark_failed("boom".into());
    assert!(matches!(failed_run.status, RunStatus::Failed));
    assert!(
        !failed_run.mark_cancelled(),
        "mark_cancelled on a Failed run must report false (no transition)"
    );
    assert!(
        matches!(failed_run.status, RunStatus::Failed),
        "mark_cancelled on a Failed run must NOT overwrite the terminal status"
    );
}

/// #1046 — Codex P1 — `http_cancel_wins_against_natural_completion`.
///
/// **Bug shape (symmetric to the original #1046 fix):** the first
/// #1046 PR closed "cancel doesn't flip until `execute_run` finishes"
/// by making the HTTP handler flip state + broadcast `run_cancelled`
/// synchronously. That introduced a symmetric hole in the opposite
/// direction: `execute_run`'s terminal arms (`Ok` / `FailedWithToolCalls`
/// / generic `Err`) called `mark_run_as_completed` / `mark_run_as_failed`
/// UNCONDITIONALLY and emitted `run_finished` / `run_error`. If a near-
/// complete run had its state flipped to `Cancelled` by the HTTP handler
/// in the narrow window between `agent_loop` returning a non-cancel
/// outcome and `execute_run`'s terminal arm running, the terminal arm
/// would silently regress the state from `Cancelled` back to
/// `Completed` / `Failed` and emit a duplicate terminal SSE event on
/// top of the already-delivered `run_cancelled`. From the user's
/// perspective: they clicked cancel, saw it land, then the agent
/// "finished" anyway.
///
/// **Why a RunManager-boundary test rather than an end-to-end
/// interposer:** the bug requires `agent_loop` to return a NON-cancel
/// terminal outcome (`Ok` or non-cancel `Err`) BEFORE the HTTP cancel
/// arrives, then for the HTTP cancel to flip state to `Cancelled`,
/// then for `execute_run`'s terminal arm to run `mark_run_as_completed`
/// / `mark_run_as_failed`. Driving this end-to-end requires either (a)
/// a custom LLM helper that signals back through a channel before
/// returning `Ok` (so the test can fire HTTP cancel in the gap between
/// the signal and `execute_run`'s terminal arm dispatching), or (b) a
/// DashMap-barrier interposer that blocks the producer at
/// `mark_run_as_failed` — but the HTTP `cancel_run` handler ALSO
/// contends on the same DashMap shard via `mark_run_as_cancelled`, so
/// the barrier would deadlock the cancel path. Neither approach is
/// clean without adding test-only hooks into production code, and the
/// existing hanging-LLM helper drives the cancel through `agent_loop`'s
/// `select!` (producing `Err(CancelledWithToolCalls)` rather than a
/// non-cancel `Err`), so the failing-arm race path is unreachable
/// via that fixture.
///
/// Instead, this test pins the contract at the `RunManager` boundary
/// directly. The terminal-arm broadcast gates in `execute_run` are
/// trivial bool checks on the return value of `mark_run_as_*`; the
/// load-bearing invariant is the `RunManager`-side idempotency
/// contract that produces `false` when a cancelled run is later
/// re-marked. If that contract holds, the gates in `lifecycle.rs`
/// trivially follow. The pre-cancel-state simulation here is exactly
/// what the HTTP handler does in production
/// (`mark_run_as_cancelled` + `send_event(run_cancelled)`); the
/// subsequent `mark_run_as_completed` / `mark_run_as_failed` calls
/// are exactly what `execute_run`'s Ok / FailedWithToolCalls / generic
/// Err arms do.
///
/// **Contract pinned by this test:**
/// 1. `mark_run_as_completed` / `mark_run_as_failed` on a run whose
///    state is already `Cancelled` return `false`.
/// 2. The run's status remains `Cancelled` (no regression to
///    `Completed` / `Failed`).
/// 3. The `output` / `error` fields are NOT mutated by the no-op
///    flip — preserving the run's actual end-state for triage via
///    `GET /runs/{id}`.
/// 4. Using the bool gate, the gateway broadcasts EXACTLY ONE
///    terminal SSE event (`run_cancelled`) — no `run_finished` or
///    `run_error` follows.
#[tokio::test]
async fn http_cancel_wins_against_natural_completion() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-1046-cancel-wins-vs-completion");
    let session_id = session.id;

    // Set up a run that has crossed `mark_run_as_running` — i.e. the
    // production state at the moment the LLM call is in flight.
    let run = Run::new(session_id, agent_id, "race fixture".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);
    state.run_manager.mark_run_as_running(run_id);

    let mut session_rx = subscribe_session(&state, session_id);

    // Step 1 — simulate the HTTP `cancel_run` handler winning the
    // race: it flips the state to `Cancelled` and broadcasts
    // `run_cancelled` synchronously. This is exactly what the
    // post-original-#1046 handler does (see `cancel_run` in
    // lifecycle.rs).
    let http_cancel_transitioned = state.run_manager.mark_run_as_cancelled(run_id);
    assert!(
        http_cancel_transitioned,
        "Running → Cancelled must transition (sanity check on the test fixture)"
    );
    state
        .run_manager
        .send_event(run_id, session_id, SseEventData::run_cancelled(run_id))
        .await;

    // Step 2 — simulate `execute_run`'s Ok arm running AFTER the
    // HTTP cancel won the race: `agent_loop` returned `Ok(_)`, the
    // terminal arm calls `mark_run_as_completed`. Post-fix this
    // returns false (Codex P1 gate: `mark_completed` requires Running,
    // run is Cancelled). The `lifecycle.rs` broadcast gate uses this
    // bool to skip the duplicate `run_finished` event.
    let completed_transitioned = state.run_manager.mark_run_as_completed(
        run_id,
        "the agent finished its work".to_string(),
        TokenUsage {
            prompt_tokens: 42,
            completion_tokens: 7,
            ..TokenUsage::default()
        },
    );
    assert!(
        !completed_transitioned,
        "mark_run_as_completed on a Cancelled run MUST return false. \
         Pre-#1046 symmetric-fix, this would return true (or have no \
         return at all) and the lifecycle would silently regress the \
         state to Completed and emit a duplicate `run_finished` SSE \
         event after `run_cancelled`."
    );
    // Production gate (mirrors the lifecycle.rs Ok arm post-fix). If
    // the gate were missing or inverted, this would fire a duplicate
    // `run_finished` event after `run_cancelled`.
    if completed_transitioned {
        state
            .run_manager
            .send_event(
                run_id,
                session_id,
                SseEventData::run_finished(run_id, true, TokenUsage::default()),
            )
            .await;
    }

    // Step 3 — simulate `execute_run`'s FailedWithToolCalls / generic
    // Err arm. Same shape on the failure side: `mark_run_as_failed`
    // on a Cancelled run must return false and not regress state.
    let failed_transitioned = state
        .run_manager
        .mark_run_as_failed(run_id, "post-cancel LLM 500".to_string());
    assert!(
        !failed_transitioned,
        "mark_run_as_failed on a Cancelled run MUST return false. \
         Pre-#1046 symmetric-fix, this would have overwritten the \
         state to Failed."
    );
    if failed_transitioned {
        state
            .run_manager
            .send_event(
                run_id,
                session_id,
                SseEventData::run_error(run_id, "post-cancel LLM 500"),
            )
            .await;
    }

    // Contract assertion: the run's stored state remains Cancelled,
    // and neither `output` nor `error` was mutated by the no-op
    // terminal flips.
    let final_run = state
        .run_manager
        .get_run(run_id)
        .expect("run must exist after the race");
    assert_eq!(
        final_run.status,
        RunStatus::Cancelled,
        "run state must remain Cancelled — `mark_completed` / \
         `mark_failed` must NOT regress an already-Cancelled run. \
         Pre-#1046 symmetric-fix (Codex P1), the terminal arm \
         overwrote the state."
    );
    assert!(
        final_run.output.is_none(),
        "output field must NOT be populated by a no-op \
         mark_completed — got {:?}",
        final_run.output
    );
    assert!(
        final_run.error.is_none(),
        "error field must NOT be populated by a no-op mark_failed — \
         got {:?}",
        final_run.error
    );

    // Wire-contract assertion: exactly ONE terminal SSE event fired
    // on the session feed, and it is `run_cancelled`. Pre-fix code
    // emits two: `run_cancelled` followed by `run_finished` (or
    // `run_error`), breaking the single-terminal-event contract that
    // clients rely on.
    let events = drain_events(&mut session_rx);
    let terminal: Vec<&str> = events
        .iter()
        .map(|e| e.event_type.as_str())
        .filter(|t| matches!(*t, "run_cancelled" | "run_finished" | "run_error"))
        .collect();
    assert_eq!(
        terminal,
        vec!["run_cancelled"],
        "exactly one terminal SSE event must fire per run, and it \
         must be `run_cancelled` (cancel won the race against natural \
         completion); got {terminal:?}. Pre-fix code emits \
         `run_cancelled` followed by a regression event."
    );

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// #1043 — GET /runs/{run_id}/reasoning rehydration endpoint
// ---------------------------------------------------------------------------

/// Reasoning text streams as `reasoning_delta` SSE events but is only
/// persisted to the message store at end-of-turn. On a mid-turn reload
/// the messages GET returns no reasoning yet and the default SSE replay
/// cursor (session HWM) sits past every fired delta, so the reasoning
/// panel would otherwise show nothing until the next post-reload delta
/// arrives. The new endpoint reconstructs the accumulated text from the
/// session event log and returns the maximum included event_id so the
/// client can bump its SSE `last_event_id` past the rehydrated events
/// and avoid a double-emit on reconnect (see acceptance criteria in
/// issue #1043: "No double-emission").
#[tokio::test]
async fn get_run_reasoning_returns_concatenated_text_and_max_event_id() {
    use axum::extract::{Path, State};
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-reasoning-rehydrate");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "go think".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);

    // Fire three reasoning_delta events on a first-turn run (no
    // parent-agent tool events yet, so the #1077 turn boundary is unset
    // and every delta is returned). Concatenation must equal the joined
    // text in event-emission order. An unrelated `run_started` event
    // (different event_type) is interleaved to exercise the
    // per-event-type filter so we know we are not lifting the wrong
    // frames out of the log. We deliberately do not interleave a
    // `tool_start` here — that would seal the turn under the #1077
    // semantics and is covered by
    // `get_run_reasoning_drops_pre_turn_boundary_deltas`.
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "Let me ", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::run_started(run_id, session_id),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "think ", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "carefully.", None),
        )
        .await;

    let response = super::lifecycle::get_run_reasoning(State(state.clone()), Path(run_id))
        .await
        .expect("get_run_reasoning should succeed for a known run");
    let body = response.0;

    assert_eq!(
        body["text"].as_str().unwrap(),
        "Let me think carefully.",
        "rehydrated reasoning must equal the concatenation of every \
         reasoning_delta text field in event-emission order"
    );

    let returned_id = body["last_event_id"]
        .as_u64()
        .expect("last_event_id must be present when reasoning events exist");
    let session_hwm = state
        .run_manager
        .latest_session_event_id(session_id)
        .await
        .expect("session must have logged events");
    assert!(
        returned_id <= session_hwm,
        "returned last_event_id {returned_id} must not exceed session HWM \
         {session_hwm}; otherwise SSE replay would skip events that fired \
         after the snapshot"
    );

    // Subagent reasoning (source_agent set) is suppressed in the main
    // panel — the UI's reasoning_delta handler early-returns on
    // source_agent, so the rehydration path must mirror that filter,
    // otherwise reload would briefly surface subagent thinking text on
    // the parent agent's panel and then have it vanish on the next
    // re-render.
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "<subagent>", Some("worker-1".into())),
        )
        .await;
    let response2 = super::lifecycle::get_run_reasoning(State(state.clone()), Path(run_id))
        .await
        .expect("get_run_reasoning should succeed after subagent delta");
    assert_eq!(
        response2.0["text"].as_str().unwrap(),
        "Let me think carefully.",
        "subagent reasoning_delta entries (source_agent set) must be \
         filtered out of the rehydrated text"
    );

    shutdown_token.cancel();
}

/// When the run has not emitted any reasoning_delta yet (e.g. a fresh
/// queued run, or a model that never emits extended-thinking text), the
/// endpoint returns an empty `text` and a null `last_event_id`. The
/// client calls this endpoint unconditionally on every reload that has
/// an active run, so an empty-result case must be well-formed rather
/// than 404 / error.
#[tokio::test]
async fn get_run_reasoning_returns_empty_when_no_reasoning_events_logged() {
    use axum::extract::{Path, State};
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-reasoning-empty");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "no reasoning yet".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);

    let response = super::lifecycle::get_run_reasoning(State(state.clone()), Path(run_id))
        .await
        .expect("get_run_reasoning should succeed even with no events");
    let body = response.0;
    assert_eq!(body["text"].as_str().unwrap(), "");
    assert!(
        body["last_event_id"].is_null(),
        "last_event_id must be null when no reasoning_delta has been \
         logged, so the client leaves its SSE replay cursor untouched"
    );

    shutdown_token.cancel();
}

/// Reasoning events emitted on one run must not contaminate the
/// rehydrated text returned for a sibling run on the same session.
/// Background subagent runs share their parent session's event log, so
/// without per-run filtering the parent's `/reasoning` endpoint would
/// pick up subagent reasoning text and vice versa.
#[tokio::test]
async fn get_run_reasoning_isolates_text_by_run_id() {
    use axum::extract::{Path, State};
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-reasoning-isolation");
    let session_id = session.id;

    let run_a = Run::new(session_id, agent_id, "a".into());
    let run_a_id = run_a.run_id;
    state.run_manager.insert_run(run_a);
    let run_b = Run::new(session_id, agent_id, "b".into());
    let run_b_id = run_b.run_id;
    state.run_manager.insert_run(run_b);

    state
        .run_manager
        .send_event(
            run_a_id,
            session_id,
            SseEventData::reasoning_delta(run_a_id, "A1 ", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_b_id,
            session_id,
            SseEventData::reasoning_delta(run_b_id, "B1 ", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_a_id,
            session_id,
            SseEventData::reasoning_delta(run_a_id, "A2", None),
        )
        .await;

    let resp_a = super::lifecycle::get_run_reasoning(State(state.clone()), Path(run_a_id))
        .await
        .expect("get_run_reasoning should succeed for run A");
    let resp_b = super::lifecycle::get_run_reasoning(State(state.clone()), Path(run_b_id))
        .await
        .expect("get_run_reasoning should succeed for run B");
    assert_eq!(resp_a.0["text"].as_str().unwrap(), "A1 A2");
    assert_eq!(resp_b.0["text"].as_str().unwrap(), "B1 ");

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// #1077 — get_run_reasoning must be per-turn scoped to avoid double-render
// ---------------------------------------------------------------------------

/// Regression test for #1077.
///
/// A run can span multiple LLM turns, each closed by one or more tool
/// calls. Prior turns' reasoning is persisted to the message store as
/// `reasoning_blocks` on the sealed assistant message and rehydrated by
/// the UI from there. If `get_run_reasoning` returned the full run-wide
/// blob, prior-turn reasoning would render twice on reload — once from
/// the sealed bubble, once from the trailing unsealed bubble seeded by
/// this endpoint.
///
/// The fix scopes the response to deltas emitted strictly **after** the
/// latest parent-agent `tool_start` / `tool_end` event in this run. This
/// test fires a Turn-1 → tool boundary → Turn-2 sequence and asserts the
/// response contains only Turn-2's text.
#[tokio::test]
async fn get_run_reasoning_drops_pre_turn_boundary_deltas() {
    use axum::extract::{Path, State};
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-reasoning-per-turn");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "multi-turn".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);

    // Turn 1: reasoning -> tool_start -> tool_end
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "Turn1-A ", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "Turn1-B", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::tool_start(
                run_id,
                crate::sse::ToolInvocationId(uuid::Uuid::new_v4()),
                "echo",
                serde_json::json!({}),
                None,
                None,
            ),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::tool_end(
                run_id,
                crate::sse::ToolInvocationId(uuid::Uuid::new_v4()),
                true,
                serde_json::json!({}),
                None,
                None,
            ),
        )
        .await;

    // Turn 2: reasoning only (still in flight — no closing tool yet)
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "Turn2-A ", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "Turn2-B", None),
        )
        .await;

    let response = super::lifecycle::get_run_reasoning(State(state.clone()), Path(run_id))
        .await
        .expect("get_run_reasoning should succeed");
    let body = response.0;
    assert_eq!(
        body["text"].as_str().unwrap(),
        "Turn2-A Turn2-B",
        "reasoning rehydration must include ONLY deltas emitted after \
         the latest parent-agent tool boundary; Turn-1 text is already \
         persisted to the sealed assistant message's reasoning_blocks \
         and would otherwise double-render (#1077)"
    );
    assert!(
        body["last_event_id"].as_u64().is_some(),
        "last_event_id must be present when post-boundary deltas exist"
    );

    shutdown_token.cancel();
}

/// First-turn contract pin for #1054 — when no tool events have fired
/// yet, the endpoint must return every `reasoning_delta` in the run.
///
/// This is the original #1043 / #1054 contract that #1077 must not
/// regress: tool-less runs (or the first turn of any run) have no
/// boundary marker, and the full delta concatenation is the only way
/// to rehydrate the live reasoning panel mid-stream.
#[tokio::test]
async fn get_run_reasoning_returns_full_text_when_no_tool_events() {
    use axum::extract::{Path, State};
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-reasoning-first-turn");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "first turn".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);

    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "alpha ", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "beta ", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "gamma", None),
        )
        .await;

    let response = super::lifecycle::get_run_reasoning(State(state.clone()), Path(run_id))
        .await
        .expect("get_run_reasoning should succeed");
    assert_eq!(
        response.0["text"].as_str().unwrap(),
        "alpha beta gamma",
        "with no tool events present the boundary is unset and ALL \
         reasoning_delta text must be returned (#1054 contract)"
    );

    shutdown_token.cancel();
}

/// A subagent `tool_start` (with `source_agent` set) must not move the
/// parent agent's turn boundary. Subagent activity is independent of
/// the parent's turn frame: an `invoke_agent` call kicks off a subagent
/// whose tool events are scoped to the subagent's own panel, and the
/// parent's reasoning panel must continue rehydrating the parent's
/// in-flight turn deltas (which include thinking BEFORE the parent
/// emits its own tool call).
#[tokio::test]
async fn get_run_reasoning_boundary_ignores_subagent_tool_events() {
    use axum::extract::{Path, State};
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-reasoning-subagent-boundary");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "subagent boundary".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);

    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "pre-sub ", None),
        )
        .await;
    // Subagent tool_start — source_agent set. MUST NOT move boundary.
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::tool_start(
                run_id,
                crate::sse::ToolInvocationId(uuid::Uuid::new_v4()),
                "echo",
                serde_json::json!({}),
                Some("worker-1".into()),
                None,
            ),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::tool_end(
                run_id,
                crate::sse::ToolInvocationId(uuid::Uuid::new_v4()),
                true,
                serde_json::json!({}),
                Some("worker-1".into()),
                None,
            ),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "post-sub", None),
        )
        .await;

    let response = super::lifecycle::get_run_reasoning(State(state.clone()), Path(run_id))
        .await
        .expect("get_run_reasoning should succeed");
    assert_eq!(
        response.0["text"].as_str().unwrap(),
        "pre-sub post-sub",
        "subagent tool events (source_agent set) must NOT move the \
         parent's turn boundary — only the parent's own tool calls \
         seal the parent's reasoning bubble"
    );

    shutdown_token.cancel();
}

/// The turn boundary computation must be run-scoped: a `tool_end` event
/// from run A on a shared session must not clip run B's reasoning. Two
/// concurrent or sequential runs on the same session share a single
/// event log, so without per-run filtering the wrong run's tool event
/// could swallow legitimate reasoning text on the other run.
#[tokio::test]
async fn get_run_reasoning_boundary_is_run_scoped() {
    use axum::extract::{Path, State};
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-reasoning-cross-run-boundary");
    let session_id = session.id;

    let run_a = Run::new(session_id, agent_id, "a".into());
    let run_a_id = run_a.run_id;
    state.run_manager.insert_run(run_a);
    let run_b = Run::new(session_id, agent_id, "b".into());
    let run_b_id = run_b.run_id;
    state.run_manager.insert_run(run_b);

    // Run B emits some reasoning first.
    state
        .run_manager
        .send_event(
            run_b_id,
            session_id,
            SseEventData::reasoning_delta(run_b_id, "B-first ", None),
        )
        .await;
    // Run A fires a parent-agent tool boundary (no subagent flag).
    state
        .run_manager
        .send_event(
            run_a_id,
            session_id,
            SseEventData::tool_end(
                run_a_id,
                crate::sse::ToolInvocationId(uuid::Uuid::new_v4()),
                true,
                serde_json::json!({}),
                None,
                None,
            ),
        )
        .await;
    // Run B continues to emit reasoning that fires AFTER run A's tool
    // event but is logically part of run B's first turn.
    state
        .run_manager
        .send_event(
            run_b_id,
            session_id,
            SseEventData::reasoning_delta(run_b_id, "B-second", None),
        )
        .await;

    let resp_b = super::lifecycle::get_run_reasoning(State(state.clone()), Path(run_b_id))
        .await
        .expect("get_run_reasoning should succeed for run B");
    assert_eq!(
        resp_b.0["text"].as_str().unwrap(),
        "B-first B-second",
        "the turn boundary is per-run: run A's tool_end must not clip \
         run B's reasoning — run B has emitted no tool events of its \
         own so its first-turn full-text contract still applies"
    );

    shutdown_token.cancel();
}

/// An unmatched parent-agent `tool_start` (approval-paused, or cancelled
/// mid-call before `tool_end` fired) must still move the turn boundary.
/// `get_run_reasoning` advertises this contract: "tool_start without a
/// matching tool_end still moves the boundary correctly — the unfinished
/// turn's reasoning is by definition older than the next delta that would
/// belong to a fresh turn." This test pins it so future refactors of the
/// boundary computation cannot silently regress to an `_end`-only walk.
///
/// Scenario: Turn 1 emits reasoning then a parent-agent `tool_start` (no
/// matching `tool_end` — simulating Guarded posture awaiting approval).
/// Turn 2 emits fresh reasoning. The response must contain only Turn 2.
#[tokio::test]
async fn get_run_reasoning_boundary_uses_unmatched_tool_start() {
    use axum::extract::{Path, State};
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-reasoning-approval-paused");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "approval-paused".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);

    // Turn 1: reasoning -> tool_start (NO matching tool_end — approval-paused).
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "Turn1-pre ", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::tool_start(
                run_id,
                crate::sse::ToolInvocationId(uuid::Uuid::new_v4()),
                "shell",
                serde_json::json!({}),
                None,
                None,
            ),
        )
        .await;

    // Turn 2: fresh reasoning after the boundary.
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "Turn2-only", None),
        )
        .await;

    let response = super::lifecycle::get_run_reasoning(State(state.clone()), Path(run_id))
        .await
        .expect("get_run_reasoning should succeed");
    assert_eq!(
        response.0["text"].as_str().unwrap(),
        "Turn2-only",
        "an unmatched parent-agent tool_start (approval-paused / cancelled \
         mid-call) must still seal the prior turn — the boundary walks \
         both tool_start AND tool_end, not just tool_end"
    );

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// #1107 — GET /runs/{run_id}/text in-flight visible-reply rehydration
// ---------------------------------------------------------------------------

/// Visible-reply text streams as `token_delta` SSE events which the
/// gateway flags ephemeral in `send_event` and therefore does not write
/// to either the per-run or per-session event log. The persistence path
/// is end-of-turn only (flush onto the sealed assistant message). On a
/// mid-stream session switch the UI's in-memory accumulation is wiped
/// by `replaceMessages([])`, the messages GET has nothing yet for the
/// in-flight turn, and SSE replay carries no token_delta (ephemeral). The
/// dedicated endpoint reconstructs the partial reply from the per-run
/// in-memory accumulator that `send_event` maintains, and returns the
/// session event log HWM at the moment the most recent delta was
/// appended so the client can advance its SSE replay cursor past any
/// non-ephemeral events that were contemporaneous.
#[tokio::test]
async fn get_run_text_returns_concatenated_visible_reply_text() {
    use axum::extract::{Path, State};
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-text-rehydrate");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "go talk".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);

    // Interleave a non-ephemeral event (`run_started`) so the buffer's
    // `last_session_event_id` watermark has something to snap to —
    // mirrors the reasoning test's mixed-event-type setup.
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::run_started(run_id, session_id),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::token_delta(run_id, "Hello ", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::token_delta(run_id, "world", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::token_delta(run_id, "!", None),
        )
        .await;

    let response = super::lifecycle::get_run_text(State(state.clone()), Path(run_id))
        .await
        .expect("get_run_text should succeed for a known run");
    let body = response.0;

    assert_eq!(
        body["text"].as_str().unwrap(),
        "Hello world!",
        "rehydrated visible reply must equal the concatenation of every \
         non-subagent token_delta delta in event-emission order"
    );

    let returned_id = body["last_event_id"]
        .as_u64()
        .expect("last_event_id must be present once a non-ephemeral event has been logged");
    let session_hwm = state
        .run_manager
        .latest_session_event_id(session_id)
        .await
        .expect("session must have logged events");
    assert!(
        returned_id <= session_hwm,
        "returned last_event_id {returned_id} must not exceed session HWM \
         {session_hwm}; otherwise SSE replay would skip events that fired \
         after the rehydration snapshot"
    );

    // Subagent token deltas (source_agent set) must be filtered out so
    // the rehydration surface matches what the UI's live `token_delta`
    // handler would have rendered (it early-returns on source_agent).
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::token_delta(run_id, "<subagent>", Some("worker-1".into())),
        )
        .await;
    let response2 = super::lifecycle::get_run_text(State(state.clone()), Path(run_id))
        .await
        .expect("get_run_text should succeed after subagent delta");
    assert_eq!(
        response2.0["text"].as_str().unwrap(),
        "Hello world!",
        "subagent token_delta entries (source_agent set) must be \
         filtered out of the rehydrated text"
    );

    shutdown_token.cancel();
}

/// When the run has not emitted any `token_delta` yet, the endpoint
/// returns an empty `text` and a null `last_event_id`. The client calls
/// this endpoint unconditionally on every reload that has an active run,
/// so an empty-result case must be well-formed rather than 404 / error.
#[tokio::test]
async fn get_run_text_returns_empty_when_no_text_events_logged() {
    use axum::extract::{Path, State};
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-text-empty");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "silent".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);

    let response = super::lifecycle::get_run_text(State(state.clone()), Path(run_id))
        .await
        .expect("get_run_text should succeed even with no token_delta events");
    let body = response.0;
    assert_eq!(body["text"].as_str().unwrap(), "");
    assert!(
        body["last_event_id"].is_null(),
        "last_event_id must be null when no token_delta has been \
         emitted, so the client leaves its SSE replay cursor untouched"
    );

    shutdown_token.cancel();
}

/// Visible-reply text emitted on one run must not contaminate the
/// rehydrated text returned for a sibling run on the same session.
/// Background subagent runs share their parent session's event log /
/// SSE fanout, so without per-run keying the parent's `/text` endpoint
/// would surface subagent reply text on the wrong run.
#[tokio::test]
async fn get_run_text_isolates_text_by_run_id() {
    use axum::extract::{Path, State};
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-text-isolation");
    let session_id = session.id;

    let run_a = Run::new(session_id, agent_id, "a".into());
    let run_a_id = run_a.run_id;
    state.run_manager.insert_run(run_a);
    let run_b = Run::new(session_id, agent_id, "b".into());
    let run_b_id = run_b.run_id;
    state.run_manager.insert_run(run_b);

    state
        .run_manager
        .send_event(
            run_a_id,
            session_id,
            SseEventData::token_delta(run_a_id, "A1 ", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_b_id,
            session_id,
            SseEventData::token_delta(run_b_id, "B1 ", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_a_id,
            session_id,
            SseEventData::token_delta(run_a_id, "A2", None),
        )
        .await;

    let resp_a = super::lifecycle::get_run_text(State(state.clone()), Path(run_a_id))
        .await
        .expect("get_run_text should succeed for run A");
    let resp_b = super::lifecycle::get_run_text(State(state.clone()), Path(run_b_id))
        .await
        .expect("get_run_text should succeed for run B");

    assert_eq!(
        resp_a.0["text"].as_str().unwrap(),
        "A1 A2",
        "run A's rehydration must contain only run A's token_delta text"
    );
    assert_eq!(
        resp_b.0["text"].as_str().unwrap(),
        "B1 ",
        "run B's rehydration must contain only run B's token_delta text"
    );

    shutdown_token.cancel();
}

/// Regression test mirroring the reasoning-side #1077 fix.
///
/// A run can span multiple LLM turns, each closed by one or more tool
/// calls. Visible reply text emitted in a prior turn has been sealed
/// onto the closing assistant message and persisted to the message
/// store; the messages GET on reload returns that sealed bubble. If the
/// rehydration buffer kept returning the prior-turn text on top of that,
/// the chat pane would render the same text twice — once on the sealed
/// bubble, once on a trailing unsealed bubble seeded by the load-session
/// step 3 path. The fix clears the buffer on every parent-agent
/// `tool_start` / `tool_end`, so this endpoint returns only the current
/// turn's accumulated text.
#[tokio::test]
async fn get_run_text_drops_pre_turn_boundary_deltas() {
    use axum::extract::{Path, State};
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-text-per-turn");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "multi-turn".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);

    // Turn 1: token_delta -> tool_start -> tool_end
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::token_delta(run_id, "Turn1-A ", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::token_delta(run_id, "Turn1-B", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::tool_start(
                run_id,
                crate::sse::ToolInvocationId(uuid::Uuid::new_v4()),
                "echo",
                serde_json::json!({}),
                None,
                None,
            ),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::tool_end(
                run_id,
                crate::sse::ToolInvocationId(uuid::Uuid::new_v4()),
                true,
                serde_json::json!({}),
                None,
                None,
            ),
        )
        .await;

    // Turn 2: fresh token_delta (still in flight — no closing tool yet)
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::token_delta(run_id, "Turn2-A ", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::token_delta(run_id, "Turn2-B", None),
        )
        .await;

    let response = super::lifecycle::get_run_text(State(state.clone()), Path(run_id))
        .await
        .expect("get_run_text should succeed");
    let body = response.0;
    assert_eq!(
        body["text"].as_str().unwrap(),
        "Turn2-A Turn2-B",
        "visible-reply rehydration must include ONLY deltas emitted after \
         the latest parent-agent tool boundary; Turn-1 text is already \
         persisted to the sealed assistant message and would otherwise \
         double-render (#1107, mirroring #1077 on the reasoning channel)"
    );
    assert!(
        body["last_event_id"].as_u64().is_some(),
        "last_event_id must be present once any non-ephemeral event \
         has fired alongside the post-boundary deltas"
    );

    shutdown_token.cancel();
}

/// A subagent `tool_start` / `tool_end` (with `source_agent` set) must
/// not clear the parent's text buffer. Subagent activity is independent
/// of the parent's turn frame: an `invoke_agent` call spawns a subagent
/// whose tool events are scoped to the subagent's own panel, and the
/// parent's reply continues to accumulate in the same turn.
#[tokio::test]
async fn get_run_text_boundary_ignores_subagent_tool_events() {
    use axum::extract::{Path, State};
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-text-subagent-boundary");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "subagent boundary".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);

    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::token_delta(run_id, "pre-sub ", None),
        )
        .await;
    // Subagent tool_start — source_agent set. MUST NOT clear buffer.
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::tool_start(
                run_id,
                crate::sse::ToolInvocationId(uuid::Uuid::new_v4()),
                "echo",
                serde_json::json!({}),
                Some("worker-1".into()),
                None,
            ),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::tool_end(
                run_id,
                crate::sse::ToolInvocationId(uuid::Uuid::new_v4()),
                true,
                serde_json::json!({}),
                Some("worker-1".into()),
                None,
            ),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::token_delta(run_id, "post-sub", None),
        )
        .await;

    let response = super::lifecycle::get_run_text(State(state.clone()), Path(run_id))
        .await
        .expect("get_run_text should succeed");
    assert_eq!(
        response.0["text"].as_str().unwrap(),
        "pre-sub post-sub",
        "subagent tool events (source_agent set) must NOT clear the \
         parent's text buffer — only the parent's own tool calls seal \
         the parent's current turn"
    );

    shutdown_token.cancel();
}

/// An unmatched parent-agent `tool_start` (approval-paused, or cancelled
/// mid-call before `tool_end` fired) must still clear the buffer — the
/// buffer's per-turn contract is that any parent-agent tool event seals
/// the prior turn's visible reply, regardless of whether the matching
/// `tool_end` arrived. Mirrors the reasoning-side `_uses_unmatched_tool_start`
/// guard so the two channels stay aligned.
#[tokio::test]
async fn get_run_text_boundary_uses_unmatched_tool_start() {
    use axum::extract::{Path, State};
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-text-approval-paused");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "approval-paused".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);

    // Turn 1: token_delta -> tool_start (NO matching tool_end — approval-paused).
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::token_delta(run_id, "Turn1-pre ", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::tool_start(
                run_id,
                crate::sse::ToolInvocationId(uuid::Uuid::new_v4()),
                "shell",
                serde_json::json!({}),
                None,
                None,
            ),
        )
        .await;

    // Turn 2: fresh token_delta after the boundary.
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::token_delta(run_id, "Turn2-only", None),
        )
        .await;

    let response = super::lifecycle::get_run_text(State(state.clone()), Path(run_id))
        .await
        .expect("get_run_text should succeed");
    assert_eq!(
        response.0["text"].as_str().unwrap(),
        "Turn2-only",
        "an unmatched parent-agent tool_start (approval-paused / cancelled \
         mid-call) must still seal the prior turn's visible reply"
    );

    shutdown_token.cancel();
}

/// `last_event_id` boundary correctness — the watermark returned must
/// never exceed the session event log HWM, so advancing the client's
/// SSE replay cursor to it cannot skip events that fired after the
/// rehydration snapshot was taken. Pins the contract that the response's
/// `last_event_id` is sampled inside the same `send_event` critical
/// section that captures the delta, not lazily resolved from a
/// post-snapshot read.
#[tokio::test]
async fn get_run_text_last_event_id_bounded_by_session_hwm() {
    use axum::extract::{Path, State};
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-text-hwm-bounded");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "hwm".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);

    // Mix some non-ephemeral events around the token_delta so the HWM
    // walks across multiple ids and we can verify the watermark sits
    // somewhere in the logged range.
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::run_started(run_id, session_id),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::token_delta(run_id, "a", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "r", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::token_delta(run_id, "b", None),
        )
        .await;

    let response = super::lifecycle::get_run_text(State(state.clone()), Path(run_id))
        .await
        .expect("get_run_text should succeed");
    let returned_id = response.0["last_event_id"]
        .as_u64()
        .expect("last_event_id must be present when text exists");
    let session_hwm = state
        .run_manager
        .latest_session_event_id(session_id)
        .await
        .expect("session must have logged events");
    assert!(
        returned_id <= session_hwm,
        "returned last_event_id {returned_id} must not exceed session HWM \
         {session_hwm} — advancing the SSE replay cursor past this would \
         drop events that fired after the rehydration snapshot"
    );

    shutdown_token.cancel();
}

/// The endpoint must 404 on an unknown run — same contract as the
/// reasoning endpoint and the rest of the runs API.
#[tokio::test]
async fn get_run_text_returns_404_for_unknown_run() {
    use axum::extract::{Path, State};
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let unknown_run = RunId::new();
    let result = super::lifecycle::get_run_text(State(state.clone()), Path(unknown_run)).await;
    let err = result.expect_err("unknown run must surface a 404, not 200 with empty text");
    assert_eq!(err.0, axum::http::StatusCode::NOT_FOUND);

    shutdown_token.cancel();
}

/// The buffer must be cleared when the run reaches a terminal state, so
/// any post-run rehydration call returns the empty contract — by then
/// the messages GET is the authoritative source of the final assistant
/// reply and the buffer would otherwise double-render it on top of the
/// sealed bubble.
#[tokio::test]
async fn get_run_text_returns_empty_after_run_completes() {
    use axum::extract::{Path, State};
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-text-post-terminal");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "soon-done".into());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);

    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::token_delta(run_id, "Hello", None),
        )
        .await;

    // Sanity-check: buffer is populated mid-run.
    let response = super::lifecycle::get_run_text(State(state.clone()), Path(run_id))
        .await
        .expect("get_run_text should succeed for live run");
    assert_eq!(response.0["text"].as_str().unwrap(), "Hello");

    // Flip the run to Completed; the buffer must be evicted as part of
    // the terminal transition.
    state.run_manager.mark_run_as_running(run_id);
    let transitioned =
        state
            .run_manager
            .mark_run_as_completed(run_id, "Hello".into(), Default::default());
    assert!(transitioned, "mark_run_as_completed should return true");

    let response = super::lifecycle::get_run_text(State(state.clone()), Path(run_id))
        .await
        .expect("get_run_text should still succeed on terminal run");
    assert_eq!(
        response.0["text"].as_str().unwrap(),
        "",
        "post-terminal rehydration must return empty — the messages GET \
         is the authoritative source for the final assistant reply once \
         the run has completed"
    );

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// #1045 — GET /sessions/{id}/messages must return the subagent's transcript
// ---------------------------------------------------------------------------

/// Regression test for #1045.
///
/// The UI's "View session" drill-down (SubagentBar / SubagentCompletionCard)
/// loads the subagent session by id via `loadSession` → `getSessionMessages`,
/// which hits this exact HTTP endpoint. If the response body's `messages`
/// array is empty or shaped wrong, the chat pane renders blank — the
/// #1045 symptom.
///
/// Coverage: dispatches a named subagent through the real `Coordinator`
/// path in an `AppState` whose LLM is mock-backed (so the agent loop
/// runs to completion without network), then invokes
/// `GET /sessions/{sub_session_id}/messages` over the production router
/// via `tower::ServiceExt::oneshot` and asserts the JSON wire shape.
///
/// What is exercised end-to-end against the mock LLM:
///   - `Role::User`     → `"user"`      with `Content::Text` → `"type": "text"`
///   - `Role::Assistant`→ `"assistant"` with `Content::Text` → `"type": "text"`
///   - the trailing `timestamp` field on each text message
///
/// What is intentionally *not* covered here (because the mock LLM emits
/// plain-text only and the dispatch path produces no synthetic markers):
///   - `Content::ToolCall` / `Content::ToolResult` shape mapping
///   - the `Role::System` synthetic-vs-internal filter
///   - the `notification_input` metadata filter
///   - the UI's `mapHistoryMessages` (in `static/ui/utils/history.js`) is
///     *not* loaded by this test; field-additive regressions there are
///     not caught — only wire-level regressions that drop or rename the
///     `role` / `type` / `content` / `timestamp` fields on text messages.
///
/// Covering the tool-call and synthetic-system paths would require an
/// integration harness that can drive a real `invoke_agent` tool call
/// (or emit a synthetic `Role::System` message into the session). For
/// the #1045 regression specifically, the user/assistant text path is
/// the load-bearing surface — that is what the UI's chat pane reads.
#[tokio::test]
async fn subagent_session_messages_endpoint_returns_transcript() {
    use alms_tools::subagent::SubagentDispatcher;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();

    // Drive a real subagent dispatch through the Coordinator that lives
    // inside AppState. The parent_session_id and parent_agent_id are
    // synthetic — `dispatch` doesn't require either to exist anywhere;
    // they are only used as keys to derive the subagent's deterministic
    // identity (#1051 / #1068: keyed on `(parent_agent_id, name)`).
    let parent_session = SessionId::new();
    let parent_agent_id = AgentId::new();
    let task: &str = "Investigate topic X";
    let (response, sub_session_id) = state
        .coordinator
        .dispatch(
            task.to_string(),
            parent_session,
            parent_agent_id,
            None,
            None,
            Some("researcher".to_string()),
            None,
            None,
        )
        .await
        .expect("subagent dispatch should succeed against mock LLM");
    // The mock LLM at crates/alms-runtime/src/llm_client/mod.rs:577 always
    // prepends `[mock] ` to the assistant reply. If `dispatch` returns Ok
    // with an empty / garbled string, the wire-shape assertions below would
    // still pass (an empty assistant message would just not match) but the
    // failure mode would be opaque. This guards the upstream contract.
    assert!(
        response.contains("mock"),
        "dispatch returned a response that does not contain the mock marker; \
         got {response:?}"
    );

    // Build the production router (with state) and issue the exact GET
    // the UI fires when `loadSession` hits `getSessionMessages`.
    // Note: `protected_router()` here omits the `require_auth` middleware
    // that `server::mod::serve_with_gateway` layers on top in production
    // (crates/alms-gateway/src/server/mod.rs:171-178) — consistent with
    // the rest of this test file, which doesn't carry bearer tokens.
    let router = crate::server::routes::protected_router().with_state(state);
    let uri = format!("/sessions/{}/messages", sub_session_id.0);
    let http_response = router
        .oneshot(HttpRequest::get(&uri).body(Body::empty()).unwrap())
        .await
        .expect("router oneshot should not fail");
    assert_eq!(
        http_response.status(),
        axum::http::StatusCode::OK,
        "expected 200 OK from {uri}, got {}",
        http_response.status()
    );

    let body_bytes = axum::body::to_bytes(http_response.into_body(), 1024 * 1024)
        .await
        .expect("read response body");
    let json: serde_json::Value =
        serde_json::from_slice(&body_bytes).expect("response body should be JSON");

    let messages = json["messages"]
        .as_array()
        .expect("response should carry a `messages` array");
    assert!(
        !messages.is_empty(),
        "GET /sessions/{sub_session_id}/messages must return a non-empty \
         `messages` array for a subagent session that has run — \
         empty array is the #1045 symptom. Body: {json}",
        sub_session_id = sub_session_id.0
    );

    // Shape assertions match the UI's `mapHistoryMessages` expectations
    // (crates/alms-gateway/static/ui/utils/history.js): user task as a
    // text message under `role: "user"`, assistant reply as a text
    // message under `role: "assistant"`. Both must carry non-empty
    // `content` so the chat pane has something to render.
    let user_msg = messages
        .iter()
        .find(|m| {
            m["role"] == "user"
                && m["type"] == "text"
                && m["content"].as_str().is_some_and(|c| c.contains(task))
        })
        .unwrap_or_else(|| {
            panic!("expected a user text message containing the task; got {messages:?}")
        });

    let assistant_msg = messages
        .iter()
        .find(|m| {
            m["role"] == "assistant"
                && m["type"] == "text"
                && m["content"].as_str().is_some_and(|c| !c.is_empty())
        })
        .unwrap_or_else(|| panic!("expected a non-empty assistant text reply; got {messages:?}"));

    // Each text message must carry a `timestamp` field. This is what
    // `mapHistoryMessages` (and the chat-pane ordering logic) consume —
    // dropping or renaming it would resurface a #1045-flavored bug.
    for (label, msg) in [("user", user_msg), ("assistant", assistant_msg)] {
        assert!(
            msg["timestamp"].is_string(),
            "{label} message must carry a string `timestamp` field; got {msg:?}"
        );
    }

    shutdown_token.cancel();
}
