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
use alms_tools::SubagentDispatcher;
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
    mpsc::Receiver<RunTrigger>,
    mpsc::Receiver<DmEvent>,
) {
    let gateway_config = GatewayConfig::default();
    let gateway = crate::gateway::Gateway::new(gateway_config).unwrap();
    let scheduler = Arc::new(alms_runtime::Scheduler::new());
    let shutdown_token = CancellationToken::new();
    let (completion_tx, completion_rx) = mpsc::unbounded_channel();
    let (trigger_tx, trigger_rx) = mpsc::channel(64);
    let (dm_event_tx, dm_event_rx) = mpsc::channel(64);
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
    mpsc::Receiver<RunTrigger>,
    mpsc::Receiver<DmEvent>,
) {
    let gateway_config = GatewayConfig {
        db_path: Some(":memory:".to_string()),
        ..GatewayConfig::default()
    };
    let gateway = crate::gateway::Gateway::new(gateway_config).unwrap();
    let scheduler = Arc::new(alms_runtime::Scheduler::new());
    let shutdown_token = CancellationToken::new();
    let (completion_tx, completion_rx) = mpsc::unbounded_channel();
    let (trigger_tx, trigger_rx) = mpsc::channel(64);
    let (dm_event_tx, dm_event_rx) = mpsc::channel(64);
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
    mpsc::Receiver<RunTrigger>,
    mpsc::Receiver<DmEvent>,
) {
    test_app_state_with_mock_llm_at(":memory:")
}

/// File-backed variant of [`test_app_state_with_mock_llm`] for tests that
/// need to reopen the SQLite database and verify restart-visible state.
fn test_app_state_with_mock_llm_at(
    db_path: &str,
) -> (
    AppState,
    CancellationToken,
    mpsc::UnboundedReceiver<SubagentCompletion>,
    mpsc::Receiver<RunTrigger>,
    mpsc::Receiver<DmEvent>,
) {
    let llm_config = alms_runtime::LlmConfig {
        mock: true,
        ..alms_runtime::LlmConfig::default()
    };
    let gateway_config = GatewayConfig {
        db_path: Some(db_path.to_string()),
        llm_config,
        ..GatewayConfig::default()
    };
    let gateway = crate::gateway::Gateway::new(gateway_config).unwrap();
    let scheduler = Arc::new(alms_runtime::Scheduler::new());
    let shutdown_token = CancellationToken::new();
    let (completion_tx, completion_rx) = mpsc::unbounded_channel();
    let (trigger_tx, trigger_rx) = mpsc::channel(64);
    let (dm_event_tx, dm_event_rx) = mpsc::channel(64);
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
    mpsc::Receiver<RunTrigger>,
    mpsc::Receiver<DmEvent>,
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
    let (trigger_tx, trigger_rx) = mpsc::channel(64);
    let (dm_event_tx, dm_event_rx) = mpsc::channel(64);
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
    mpsc::Receiver<RunTrigger>,
    mpsc::Receiver<DmEvent>,
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
    let (trigger_tx, trigger_rx) = mpsc::channel(64);
    let (dm_event_tx, dm_event_rx) = mpsc::channel(64);
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

/// Seed one more agent into the SQLite-backed registry.
///
/// [`seed_alice_bob`] covers the pair; the #1299 fold tests also need a
/// third party to show the fold removes exactly one recipient.
fn seed_agent(state: &AppState, name: &str) -> AgentId {
    use alms_core::registry::AgentRecord;
    use chrono::Utc;
    let store = state
        .session_manager
        .store()
        .expect("test_app_state_with_sqlite must provide a SQLite store");
    let record = AgentRecord {
        id: AgentId::new(),
        name: name.into(),
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
    store.create_agent(&record).unwrap();
    record.id
}

/// Subscribe to SSE events on a session and return the receiver.
///
/// Events sent via `run_manager.send_session_event()` will be received
/// on the returned channel.
fn subscribe_session(
    state: &AppState,
    session_id: SessionId,
) -> crate::server::ManagedSubscription<SessionId> {
    state.run_manager.subscribe_session(session_id)
}

/// Drain all currently buffered events from a receiver without blocking.
fn drain_events<K>(rx: &mut crate::server::ManagedSubscription<K>) -> Vec<SseEventData>
where
    K: Eq + std::hash::Hash + Clone,
{
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
    let _ = state.run_manager.insert_run(run.clone());
    state
        .session_manager
        .append_message(
            session_id,
            alms_session::Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: alms_session::Role::User,
                content: alms_session::Content::Text(run.input.clone()),
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({
                    "pending_input": true,
                    "run_id": run_id.0.to_string(),
                })),
            },
        )
        .unwrap();

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
            input_pre_persisted: true,
            dm_ended_peer: None,
        },
    )
    .await;

    // Verify the run status is Cancelled.
    let run = state.run_manager.get_run(run_id).expect("run should exist");
    assert_eq!(
        run.status(),
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

    let history = state.session_manager.get_history(session_id).unwrap();
    let run_id_string = run_id.0.to_string();
    let claimed_input = history
        .iter()
        .find(|message| {
            message
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("run_id"))
                .and_then(serde_json::Value::as_str)
                == Some(run_id_string.as_str())
        })
        .expect("pre-cancelled accepted prompt must remain durable");
    let metadata = claimed_input.metadata.as_ref().unwrap();
    assert_eq!(metadata["pending_input"], false);
    assert!(metadata["input_claimed_at"].is_string());
    assert!(
        state
            .session_manager
            .get_context_history(session_id)
            .unwrap()
            .iter()
            .any(|message| matches!(
                &message.content,
                alms_session::Content::Text(text) if text == "test input"
            )),
        "a pre-cancelled accepted prompt must remain context-visible"
    );

    shutdown_token.cancel();
}

#[tokio::test]
async fn cancellation_between_precheck_and_start_transition_cleans_up_without_starting() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-cancel-during-start");
    let session_id = session.id;
    let job_id = alms_core::JobId::new();
    let run = Run::for_job(
        session_id,
        agent_id,
        "must never reach the agent loop".into(),
        job_id,
    );
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());
    state
        .job_episodes
        .open(job_id, session_id, agent_id, run_id);

    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    let _run_subscription = state.run_manager.subscribe_run(run_id);

    let approval_id = uuid::Uuid::new_v4();
    let (decision_tx, _decision_rx) = tokio::sync::oneshot::channel();
    state
        .approval_store
        .insert(crate::approvals::PendingApproval {
            approval_id,
            run_id,
            tool: "test".to_string(),
            params: serde_json::json!({}),
            requested_at: chrono::Utc::now(),
            decision_tx,
        });

    let barrier = super::lifecycle::install_start_transition_barrier(run_id);
    let execute_state = state.clone();
    let execute_cancel_token = cancel_token.clone();
    let handle = tokio::spawn(async move {
        super::lifecycle::execute_run(
            execute_state,
            super::RunParams {
                run_id,
                session_id,
                agent_id,
                input: run.input,
                context_id: "test-cancel-during-start".to_string(),
                cancel_token: execute_cancel_token,
                is_peer_message: false,
                is_system_triggered: false,
                input_pre_persisted: false,
                dm_ended_peer: None,
            },
        )
        .await;
    });

    barrier.wait().await;
    cancel_token.cancel();
    assert!(state.run_manager.mark_run_as_cancelled(run_id));
    barrier.wait().await;
    tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("execute_run should leave the rejected-start branch")
        .expect("execute_run task should not panic");

    let run = state.run_manager.get_run(run_id).unwrap();
    assert_eq!(run.status(), RunStatus::Cancelled);
    let event_types: Vec<_> = state
        .run_manager
        .events_from(run_id, 0)
        .await
        .into_iter()
        .map(|event| event.event_type)
        .collect();
    assert!(!event_types.iter().any(|kind| kind == "run_started"));
    assert!(!event_types.iter().any(|kind| kind == "run_finished"));
    assert!(!state.run_manager.cancel_run(run_id));
    assert!(!state.run_manager.event_senders.contains_key(&run_id));
    assert!(
        state
            .approval_store
            .list_pending()
            .iter()
            .all(|approval| approval.run_id != run_id)
    );
    assert!(state.job_episodes.snapshot(job_id).is_none());
    shutdown_token.cancel();
}

#[tokio::test]
async fn pre_persisted_input_survives_start_persistence_failure() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-start-persistence-failure");
    let run = Run::new(session.id, agent_id, "must not start".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());
    state
        .session_manager
        .append_message(
            session.id,
            alms_session::Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: alms_session::Role::User,
                content: alms_session::Content::Text(run.input.clone()),
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({
                    "pending_input": true,
                    "run_id": run_id.0.to_string(),
                })),
            },
        )
        .unwrap();
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    let _run_subscription = state.run_manager.subscribe_run(run_id);
    state.run_manager.inject_next_persistence_failure();

    super::lifecycle::execute_run(
        state.clone(),
        super::RunParams {
            run_id,
            session_id: session.id,
            agent_id,
            input: run.input,
            context_id: "test-start-persistence-failure".to_string(),
            cancel_token,
            is_peer_message: false,
            is_system_triggered: false,
            input_pre_persisted: true,
            dm_ended_peer: None,
        },
    )
    .await;

    let run = state.run_manager.get_run(run_id).unwrap();
    assert_eq!(run.status(), RunStatus::Failed);
    assert_eq!(run.terminal_reason(), Some("persistence_failed"));
    let event_types: Vec<_> = state
        .run_manager
        .events_from(run_id, 0)
        .await
        .into_iter()
        .map(|event| event.event_type)
        .collect();
    assert!(event_types.iter().any(|kind| kind == "run_error"));
    assert!(!event_types.iter().any(|kind| kind == "run_started"));
    assert!(!state.run_manager.cancel_run(run_id));
    assert!(!state.run_manager.event_senders.contains_key(&run_id));

    let history = state.session_manager.get_history(session.id).unwrap();
    let run_id_string = run_id.0.to_string();
    let claimed_input = history
        .iter()
        .find(|message| {
            message
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("run_id"))
                .and_then(serde_json::Value::as_str)
                == Some(run_id_string.as_str())
        })
        .expect("accepted prompt must remain in session history");
    let metadata = claimed_input.metadata.as_ref().unwrap();
    assert_eq!(metadata["pending_input"], false);
    assert!(metadata["input_claimed_at"].as_str().is_some());
    assert!(
        state
            .session_manager
            .get_context_history(session.id)
            .unwrap()
            .iter()
            .any(|message| matches!(
                &message.content,
                alms_session::Content::Text(text) if text == "must not start"
            ))
    );
    shutdown_token.cancel();
}

#[tokio::test]
async fn terminal_persistence_failure_is_reported_and_quarantined() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-terminal-persistence-failure");
    let run = Run::new(session.id, agent_id, "complete in mock mode".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    let _run_subscription = state.run_manager.subscribe_run(run_id);
    let barrier = super::lifecycle::install_terminal_transition_barrier(run_id);
    let execute_state = state.clone();
    let handle = tokio::spawn(async move {
        super::lifecycle::execute_run(
            execute_state,
            super::RunParams {
                run_id,
                session_id: session.id,
                agent_id,
                input: run.input,
                context_id: "test-terminal-persistence-failure".to_string(),
                cancel_token,
                is_peer_message: false,
                is_system_triggered: false,
                input_pre_persisted: false,
                dm_ended_peer: None,
            },
        )
        .await;
    });

    barrier.wait().await;
    assert_eq!(
        state.run_manager.get_run(run_id).unwrap().status(),
        RunStatus::Running
    );
    state.run_manager.inject_next_persistence_failure();
    barrier.wait().await;
    tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("execute_run should leave the terminal transition")
        .expect("execute_run task should not panic");

    let run = state.run_manager.get_run(run_id).unwrap();
    assert_eq!(run.status(), RunStatus::Failed);
    assert_eq!(run.terminal_reason(), Some("persistence_failed"));
    let event_types: Vec<_> = state
        .run_manager
        .events_from(run_id, 0)
        .await
        .into_iter()
        .map(|event| event.event_type)
        .collect();
    assert!(event_types.iter().any(|kind| kind == "run_started"));
    assert!(event_types.iter().any(|kind| kind == "run_error"));
    assert!(!event_types.iter().any(|kind| kind == "run_finished"));
    assert!(!state.run_manager.cancel_run(run_id));
    assert!(!state.run_manager.event_senders.contains_key(&run_id));
    shutdown_token.cancel();
}

#[tokio::test]
async fn create_run_registration_failure_aborts_before_message_or_queue_side_effects() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::{Json, extract::State, http::StatusCode};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-registration-persistence-failure");
    state.run_manager.inject_next_persistence_failure();

    let request = CreateRunRequest {
        session_id: session.id,
        agent_id: Some(agent_id),
        input: RunInput::Text {
            text: "must not be admitted".to_string(),
        },
    };
    let Err((status, body)) =
        super::lifecycle::create_run(State(state.clone()), Json(request)).await
    else {
        panic!("registration persistence failure must reject POST /runs");
    };

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body.0["error"]["code"], "LIFECYCLE_PERSISTENCE_FAILED");
    assert!(state.run_manager.runs.is_empty());
    assert!(
        state
            .session_manager
            .get_history(session.id)
            .unwrap()
            .is_empty(),
        "the user message must not be persisted after registration fails"
    );
    assert_eq!(state.agent_queue.pending_count(&agent_id), 0);
    shutdown_token.cancel();
}
#[tokio::test]
async fn concurrent_same_session_admissions_preserve_durable_and_live_order() {
    use alms_core::{CreateRunRequest, RunInput};
    use alms_session::{Content, Role};
    use axum::{Json, extract::State, http::StatusCode};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-concurrent-admission-order");
    let barrier = super::lifecycle::install_admission_persistence_barrier(session.id);

    let first_state = state.clone();
    let first_session_id = session.id;
    let first = tokio::spawn(async move {
        super::lifecycle::create_run(
            State(first_state),
            Json(CreateRunRequest {
                session_id: first_session_id,
                agent_id: Some(agent_id),
                input: RunInput::Text {
                    text: "first admission".to_string(),
                },
            }),
        )
        .await
    });

    barrier.wait().await;
    let sqlite = state
        .session_manager
        .store()
        .expect("SQLite-backed test state");
    assert_eq!(sqlite.load_messages(session.id).unwrap().len(), 1);

    let second_state = state.clone();
    let second_session_id = session.id;
    let mut second = tokio::spawn(async move {
        super::lifecycle::create_run(
            State(second_state),
            Json(CreateRunRequest {
                session_id: second_session_id,
                agent_id: Some(agent_id),
                input: RunInput::Text {
                    text: "second admission".to_string(),
                },
            }),
        )
        .await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut second)
            .await
            .is_err(),
        "the second same-session admission must wait for the first projection"
    );
    assert_eq!(
        sqlite.load_messages(session.id).unwrap().len(),
        1,
        "the second durable commit must wait behind the session gate"
    );

    barrier.wait().await;
    let (first_status, _) = tokio::time::timeout(std::time::Duration::from_secs(5), first)
        .await
        .expect("first admission timed out")
        .expect("first admission task panicked")
        .expect("first admission failed");
    let (second_status, _) = tokio::time::timeout(std::time::Duration::from_secs(5), second)
        .await
        .expect("second admission timed out")
        .expect("second admission task panicked")
        .expect("second admission failed");
    assert_eq!(first_status, StatusCode::CREATED);
    assert_eq!(second_status, StatusCode::CREATED);

    let user_texts = |messages: Vec<alms_session::Message>| {
        messages
            .into_iter()
            .filter_map(|message| match (message.role, message.content) {
                (Role::User, Content::Text(text))
                    if message
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.get("run_id"))
                        .and_then(serde_json::Value::as_str)
                        .is_some() =>
                {
                    Some(text)
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let expected = vec![
        "first admission".to_string(),
        "second admission".to_string(),
    ];
    assert_eq!(
        user_texts(sqlite.load_messages(session.id).unwrap()),
        expected
    );
    assert_eq!(
        user_texts(state.session_manager.get_history(session.id).unwrap()),
        expected
    );
    assert!(
        state.run_admission_gates.is_empty(),
        "idle admission gates must remove their weak registry entry"
    );
    shutdown_token.cancel();
}

#[tokio::test]
async fn queued_later_prompt_is_not_visible_to_the_first_run() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::{Json, extract::State};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-run-context-boundary");
    state.agent_config.write().debug_mode = true;
    let mut session_events = subscribe_session(&state, session.id);
    let execution_barrier = super::lifecycle::install_admission_execution_barrier(session.id);

    let create = |text: &str| CreateRunRequest {
        session_id: session.id,
        agent_id: Some(agent_id),
        input: RunInput::Text {
            text: text.to_string(),
        },
    };
    let (_, first) =
        super::lifecycle::create_run(State(state.clone()), Json(create("first prompt")))
            .await
            .expect("first admission failed");
    let (_, second) =
        super::lifecycle::create_run(State(state.clone()), Json(create("second prompt")))
            .await
            .expect("second admission failed");

    let before_claim = state
        .session_manager
        .get_context_history(session.id)
        .unwrap();
    assert!(
        before_claim.is_empty(),
        "neither queued prompt is eligible before the first run claims its input"
    );
    execution_barrier.wait().await;
    execution_barrier.wait().await;

    for _ in 0..500 {
        let both_terminal = [first.0.run_id, second.0.run_id].into_iter().all(|run_id| {
            state.run_manager.get_run(run_id).is_some_and(|run| {
                matches!(
                    run.status(),
                    RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
                )
            })
        });
        if both_terminal {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let first_run = state
        .run_manager
        .get_run(first.0.run_id)
        .expect("first run disappeared");
    let second_run = state
        .run_manager
        .get_run(second.0.run_id)
        .expect("second run disappeared");
    assert_eq!(first_run.status(), RunStatus::Completed);
    assert_eq!(second_run.status(), RunStatus::Completed);
    assert_eq!(first_run.output.as_deref(), Some("[mock] first prompt"));
    assert_eq!(second_run.output.as_deref(), Some("[mock] second prompt"));

    let events = drain_events(&mut session_events);
    let context_messages = |run_id: RunId| {
        events
            .iter()
            .find(|event| {
                event.event_type == "context_debug" && event.data["run_id"] == run_id.0.to_string()
            })
            .and_then(|event| event.data["messages"].as_array())
            .cloned()
            .unwrap_or_else(|| panic!("missing context_debug for run {run_id:?}"))
    };
    let text_turns = |messages: Vec<serde_json::Value>| {
        messages
            .into_iter()
            .filter_map(|message| {
                let role = message["role"].as_str()?;
                if role == "system" {
                    return None;
                }
                Some((role.to_string(), message["content"].as_str()?.to_string()))
            })
            .collect::<Vec<_>>()
    };
    let first_turns = text_turns(context_messages(first.0.run_id));
    assert_eq!(
        first_turns,
        vec![("user".to_string(), "first prompt".to_string())]
    );
    assert!(
        first_turns
            .iter()
            .all(|(_, content)| !content.contains("second prompt")),
        "the actual first LLM context must not contain the later prompt"
    );
    assert_eq!(
        text_turns(context_messages(second.0.run_id)),
        vec![
            ("user".to_string(), "first prompt".to_string()),
            ("assistant".to_string(), "[mock] first prompt".to_string()),
            ("user".to_string(), "second prompt".to_string()),
        ]
    );
    shutdown_token.cancel();
}

#[tokio::test]
async fn cancelled_create_request_keeps_event_order_and_gate_ownership() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::{Json, extract::State};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-cancelled-admission-publication");
    let event_barrier = super::lifecycle::install_admission_event_barrier(session.id);

    let first_state = state.clone();
    let first_session_id = session.id;
    let first = tokio::spawn(async move {
        super::lifecycle::create_run(
            State(first_state),
            Json(CreateRunRequest {
                session_id: first_session_id,
                agent_id: Some(agent_id),
                input: RunInput::Text {
                    text: "first publication".to_string(),
                },
            }),
        )
        .await
    });
    event_barrier.wait().await;
    let first_run_id = state.run_manager.list_by_session(session.id, 10)[0].run_id;
    first.abort();
    let _ = first.await;

    let second_state = state.clone();
    let second_session_id = session.id;
    let mut second = tokio::spawn(async move {
        super::lifecycle::create_run(
            State(second_state),
            Json(CreateRunRequest {
                session_id: second_session_id,
                agent_id: Some(agent_id),
                input: RunInput::Text {
                    text: "second publication".to_string(),
                },
            }),
        )
        .await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut second)
            .await
            .is_err(),
        "the second admission must remain behind the detached first publisher"
    );
    assert!(
        state
            .run_manager
            .session_events_from(session.id, 0)
            .await
            .iter()
            .all(|event| event.event_type != "run_created")
    );

    event_barrier.wait().await;
    let (_, second_response) = tokio::time::timeout(std::time::Duration::from_secs(5), second)
        .await
        .expect("second admission timed out")
        .expect("second admission task panicked")
        .expect("second admission failed");
    let created_run_ids = state
        .run_manager
        .session_events_from(session.id, 0)
        .await
        .into_iter()
        .filter(|event| event.event_type == "run_created")
        .filter_map(|event| {
            event
                .data
                .get("run_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| uuid::Uuid::parse_str(value).ok())
                .map(RunId)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        created_run_ids,
        vec![first_run_id, second_response.0.run_id]
    );
    assert!(state.run_admission_gates.is_empty());
    shutdown_token.cancel();
}

#[tokio::test]
async fn admission_gate_registry_does_not_retain_idle_sessions() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    for _ in 0..100 {
        let guard = super::lifecycle::acquire_run_admission_guard(
            &state.run_admission_gates,
            SessionId::new(),
        )
        .await;
        drop(guard);
    }
    assert!(state.run_admission_gates.is_empty());
    shutdown_token.cancel();
}

#[tokio::test]
async fn simultaneous_final_admission_lease_drops_remove_registry_entry() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    for _ in 0..100 {
        let session_id = SessionId::new();
        let owner =
            super::lifecycle::acquire_run_admission_guard(&state.run_admission_gates, session_id)
                .await;
        let acquire_barrier = super::lifecycle::install_admission_acquire_barrier(session_id);
        let gates = state.run_admission_gates.clone();
        let waiter = tokio::spawn(async move {
            super::lifecycle::acquire_run_admission_guard(&gates, session_id).await
        });

        acquire_barrier.wait().await;
        let release = Arc::new(tokio::sync::Barrier::new(3));
        let drop_release = release.clone();
        let drop_task = tokio::spawn(async move {
            drop_release.wait().await;
            drop(owner);
        });
        let abort_release = release.clone();
        let abort_waiter = waiter.abort_handle();
        let abort_task = tokio::spawn(async move {
            abort_release.wait().await;
            abort_waiter.abort();
        });

        release.wait().await;
        let _ = tokio::join!(drop_task, abort_task);
        let _ = waiter.await;
        assert!(
            state.run_admission_gates.is_empty(),
            "simultaneous final lease drops must remove the registry entry"
        );
    }
    shutdown_token.cancel();
}

#[tokio::test]
async fn deleted_job_session_cannot_gain_an_orphan_scheduled_run() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let job = state
        .job_store
        .create(alms_core::CreateJobRequest {
            agent_id,
            prompt: "scheduled work".to_string(),
            schedule: alms_core::JobSchedule::Once {
                run_at: chrono::Utc::now() + chrono::Duration::hours(1),
            },
        })
        .unwrap();
    let context_id = format!("job_{}", job.id.0);
    let session = state.session_manager.get_or_create(agent_id, &context_id);
    let owner =
        super::lifecycle::acquire_run_admission_guard(&state.run_admission_gates, session.id).await;
    let acquire_barrier = super::lifecycle::install_admission_acquire_barrier(session.id);
    let fire_state = state.clone();
    let fire =
        tokio::spawn(async move { super::notifications::fire_job_run(fire_state, job.id).await });

    acquire_barrier.wait().await;
    state.session_manager.delete(agent_id, &context_id).unwrap();
    acquire_barrier.wait().await;
    drop(owner);

    let result = fire.await.expect("scheduled producer panicked");
    assert!(result.is_err(), "deleted target must reject registration");
    assert!(state.run_manager.list_by_session(session.id, 10).is_empty());
    assert!(!state.session_manager.has_session(&(agent_id, context_id)));
    shutdown_token.cancel();
}

#[tokio::test]
async fn deleted_session_cannot_gain_an_orphan_triggered_run() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let context_id = "deleted-trigger-target".to_string();
    let session = state.session_manager.get_or_create(agent_id, &context_id);
    let session_id = session.id;
    let owner =
        super::lifecycle::acquire_run_admission_guard(&state.run_admission_gates, session_id).await;
    let acquire_barrier = super::lifecycle::install_admission_acquire_barrier(session_id);
    let trigger_state = state.clone();
    let trigger_context = context_id.clone();
    let trigger = tokio::spawn(async move {
        super::notifications::enqueue_triggered_run(
            &trigger_state,
            agent_id,
            session_id,
            "late notification".to_string(),
            trigger_context,
            "test".to_string(),
            false,
            None,
            None,
        )
        .await
    });

    acquire_barrier.wait().await;
    state.session_manager.delete(agent_id, &context_id).unwrap();
    acquire_barrier.wait().await;
    drop(owner);

    assert_eq!(
        trigger.await.expect("triggered producer panicked"),
        None,
        "deleted target must suppress registration"
    );
    assert!(state.run_manager.list_by_session(session_id, 10).is_empty());
    assert!(!state.session_manager.has_session(&(agent_id, context_id)));
    shutdown_token.cancel();
}

#[tokio::test]
async fn deleted_episode_target_releases_reserved_continuation() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let job_id = create_recurring_job(&state, agent_id, "episode cleanup");
    let context_id = format!("job_{}", job_id.0);
    let session = state.session_manager.get_or_create(agent_id, &context_id);
    let turn1 = RunId::new();
    let task_id = uuid::Uuid::new_v4();
    state.job_episodes.open(job_id, session.id, agent_id, turn1);
    assert!(matches!(
        state.job_episodes.on_run_complete(
            job_id,
            turn1,
            vec![],
            vec![(task_id, SessionId::new())]
        ),
        super::job_episode::RunCompletion::Open
    ));
    let route = state
        .job_episodes
        .resolve_subagent(task_id)
        .expect("terminal signal must reserve its continuation");
    state.session_manager.delete(agent_id, &context_id).unwrap();

    let result = super::notifications::enqueue_triggered_run(
        &state,
        agent_id,
        route.job_session_id,
        "late continuation".to_string(),
        route.context_id,
        "subagent".to_string(),
        false,
        Some(route.job_id),
        None,
    )
    .await;

    assert_eq!(result, None);
    assert!(
        state.job_episodes.snapshot(job_id).is_none(),
        "failed admission must release the final continuation reservation"
    );
    assert!(state.run_manager.list_by_session(session.id, 10).is_empty());
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
    let _ = state.run_manager.insert_run(run.clone());

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
            dm_ended_peer: None,
        },
    )
    .await;

    let run = state.run_manager.get_run(run_id).expect("run should exist");
    assert_eq!(
        run.status(),
        RunStatus::Cancelled,
        "run during shutdown should be cancelled"
    );

    let events = drain_events(&mut rx);
    assert!(
        events.iter().any(|e| e.event_type == "run_cancelled"),
        "expected a run_cancelled SSE event during shutdown"
    );
}

/// Regression test for #1109: denying a tool approval must cancel runs
/// still queued on the same session, BEFORE the decision reaches the
/// runtime — they must NOT auto-start when the per-agent queue advances.
///
/// Drives the deny through the real `resolve_approval` handler. The
/// denied run's own token must stay uncancelled (the runtime's deny
/// branch owns it — see the #816 race note in `resolve_approval`), and
/// queued runs on other sessions must be untouched.
#[tokio::test]
async fn deny_cancels_queued_runs_in_same_session() {
    use crate::approvals::{PendingApproval, ResolveApprovalRequest};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-deny-queue");
    let session_id = session.id;

    // Run A: Running, with a pending approval (the run being denied).
    let mut run_a = Run::new(session_id, agent_id, "guarded run".into());
    run_a.mark_running();
    let run_a_id = run_a.run_id;
    let _ = state.run_manager.insert_run(run_a);
    let token_a = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_a_id, token_a.clone());

    // Run B: Queued behind A on the SAME session.
    let run_b = Run::new(session_id, agent_id, "queued behind".into());
    let run_b_id = run_b.run_id;
    let _ = state.run_manager.insert_run(run_b.clone());
    let token_b = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_b_id, token_b.clone());

    // Run C: Queued on a DIFFERENT session of the same agent.
    let other_session = state
        .session_manager
        .get_or_create(agent_id, "test-deny-queue-other");
    let run_c = Run::new(other_session.id, agent_id, "other session".into());
    let run_c_id = run_c.run_id;
    let _ = state.run_manager.insert_run(run_c);
    let token_c = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_c_id, token_c.clone());

    // Pending approval on run A, as the runtime's approval gate would
    // have registered it.
    let approval_id = uuid::Uuid::new_v4();
    let (decision_tx, decision_rx) = tokio::sync::oneshot::channel();
    state.approval_store.insert(PendingApproval {
        approval_id,
        run_id: run_a_id,
        tool: "math".to_string(),
        params: serde_json::json!({"operation": "add", "a": 1, "b": 2}),
        requested_at: chrono::Utc::now(),
        decision_tx,
    });

    let mut rx = subscribe_session(&state, session_id);

    // Deny through the real HTTP handler.
    let response = crate::approvals::resolve_approval(
        axum::extract::State(state.clone()),
        axum::extract::Path(approval_id),
        axum::Json(ResolveApprovalRequest {
            decision: "deny".to_string(),
        }),
    )
    .await;
    assert!(response.is_ok(), "deny resolution must succeed");

    // The runtime received `false` — and only after the queued-run
    // cancellation above had already been applied.
    assert!(
        !decision_rx.await.expect("decision channel must resolve"),
        "runtime must receive the deny decision"
    );

    // (1) Queued same-session run: token cancelled + state flipped.
    assert!(
        token_b.is_cancelled(),
        "queued same-session run's token must be cancelled on deny"
    );
    assert_eq!(
        state.run_manager.get_run(run_b_id).unwrap().status(),
        RunStatus::Cancelled,
        "queued same-session run must flip to Cancelled on deny"
    );

    // (2) The denied run is left to the runtime's deny branch.
    assert!(
        !token_a.is_cancelled(),
        "handler must NOT cancel the denied run's token — the runtime's \
         deny branch owns that after emitting the user_denied result"
    );

    // (3) Different-session queued run: untouched.
    assert!(
        !token_c.is_cancelled(),
        "queued run on another session must not be cancelled"
    );
    assert_eq!(
        state.run_manager.get_run(run_c_id).unwrap().status(),
        RunStatus::Queued,
        "queued run on another session must stay Queued"
    );

    // run_cancelled (for B) and approval_resolved (for A) on the session feed.
    let events = drain_events(&mut rx);
    assert!(
        events.iter().any(|e| e.event_type == "run_cancelled"),
        "expected run_cancelled SSE for the queued run; got: {:?}",
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
    );
    assert!(
        events.iter().any(|e| e.event_type == "approval_resolved"),
        "expected approval_resolved SSE; got: {:?}",
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
    );

    // (4) When the per-agent queue later dequeues B's work item, the
    // early-exit at the top of `execute_run` fires — B never runs.
    super::lifecycle::execute_run(
        state.clone(),
        super::RunParams {
            run_id: run_b_id,
            session_id,
            agent_id,
            input: run_b.input,
            context_id: "test-deny-queue".to_string(),
            cancel_token: token_b,
            is_peer_message: false,
            is_system_triggered: false,
            input_pre_persisted: false,
            dm_ended_peer: None,
        },
    )
    .await;
    assert_eq!(
        state.run_manager.get_run(run_b_id).unwrap().status(),
        RunStatus::Cancelled,
        "queued run must never auto-start after a deny"
    );

    shutdown_token.cancel();
}

/// Regression pin for the #1139 review's C1 race: a same-session `Queued`
/// run whose cancel token is NOT yet registered (the `create_run`
/// insert-to-register window, lifecycle.rs ~:879-961) must be SKIPPED by
/// the deny sweep — left `Queued`, never marked `Cancelled`, no
/// `run_cancelled` broadcast. Marking it would let `create_run` register
/// a fresh token and the unconditional `mark_running` resurrect a run
/// that already announced `run_cancelled`. The structural fix (register
/// token before `insert_run`) is tracked in #1142.
#[tokio::test]
async fn deny_skips_queued_run_without_registered_cancel_token() {
    use crate::approvals::{PendingApproval, ResolveApprovalRequest};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-deny-no-token");
    let session_id = session.id;

    // Run A: Running, with a pending approval (the run being denied).
    let mut run_a = Run::new(session_id, agent_id, "guarded run".into());
    run_a.mark_running();
    let run_a_id = run_a.run_id;
    let _ = state.run_manager.insert_run(run_a);
    let token_a = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_a_id, token_a.clone());

    // Run B: Queued on the SAME session, but with NO registered cancel
    // token — visible to `list_queued_for_session` exactly as it is
    // inside `create_run`'s insert-to-register window.
    let run_b = Run::new(session_id, agent_id, "queued, token pending".into());
    let run_b_id = run_b.run_id;
    let _ = state.run_manager.insert_run(run_b);

    let approval_id = uuid::Uuid::new_v4();
    let (decision_tx, decision_rx) = tokio::sync::oneshot::channel();
    state.approval_store.insert(PendingApproval {
        approval_id,
        run_id: run_a_id,
        tool: "math".to_string(),
        params: serde_json::json!({"operation": "add", "a": 1, "b": 2}),
        requested_at: chrono::Utc::now(),
        decision_tx,
    });

    let mut rx = subscribe_session(&state, session_id);

    // Deny through the real HTTP handler.
    let response = crate::approvals::resolve_approval(
        axum::extract::State(state.clone()),
        axum::extract::Path(approval_id),
        axum::Json(ResolveApprovalRequest {
            decision: "deny".to_string(),
        }),
    )
    .await;
    assert!(response.is_ok(), "deny resolution must succeed");
    assert!(
        !decision_rx.await.expect("decision channel must resolve"),
        "runtime must receive the deny decision"
    );

    // The token-less queued run escaped the sweep: still Queued, never
    // marked Cancelled (degraded pre-#1109 behavior, not corruption).
    assert_eq!(
        state.run_manager.get_run(run_b_id).unwrap().status(),
        RunStatus::Queued,
        "queued run without a registered cancel token must be left Queued \
         by the deny sweep — marking it Cancelled would race create_run's \
         token registration and resurrect via mark_running"
    );

    // No run_cancelled was broadcast for it either.
    let events = drain_events(&mut rx);
    assert!(
        !events.iter().any(|e| e.event_type == "run_cancelled"),
        "no run_cancelled SSE may be emitted for a skipped token-less run; \
         got: {:?}",
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
    );
    assert!(
        events.iter().any(|e| e.event_type == "approval_resolved"),
        "approval_resolved must still fire; got: {:?}",
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
    );

    shutdown_token.cancel();
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
    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx
        .send(RunTrigger {
            agent_id,
            session_id: notif_session_id,
            input: "DM ended by alice".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: sender_agent_id,
                from_name: "alice".to_string(),
                reason: ConversationEndReason::Ignored,
                self_notification: false,
                source_session_id: None,
            },
            context_id: notif_context_id.clone(),
        })
        .await
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
    let ended = web_events
        .iter()
        .find(|e| e.event_type == "dm_conversation_ended")
        .unwrap_or_else(|| {
            panic!(
                "expected a dm_conversation_ended SSE event on the web-chat session; got: {:?}",
                web_events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
            )
        });
    // A pure-recipient forward persists the marker (the run is on the invisible
    // notifications: session), so it must NOT suppress the live banner either.
    assert_ne!(
        ended.data.get("suppress_banner").and_then(|v| v.as_bool()),
        Some(true),
        "pure-recipient forward must not set suppress_banner (the banner is the \
         one visible indicator)"
    );

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// #1258 — an interrupted DM end is delivered as a marker, not as a run
// ---------------------------------------------------------------------------

/// What the operator's session ended up with after ONE `ConversationEnded`
/// trigger was driven through `run_trigger_loop`.
struct DmEndDelivery {
    /// Runs created on the operator's web-chat session.
    runs: Vec<Run>,
    /// Persisted `dm_ended_notification` markers on that session.
    markers: Vec<alms_session::Message>,
    /// `dm_conversation_ended` SSE frames delivered to that session.
    events: Vec<SseEventData>,
}

/// Drive one `ConversationEnded` trigger whose target IS the agent's
/// user-facing web-chat — the shape of the #1258 incident, where the
/// notification landed on the very session the operator had just cancelled a
/// run on (`source_session_id = Some(web-chat)`, the #556 initiator-ends
/// routing).
///
/// Every arm of the delivery is captured, so a caller asserting "no run" is
/// also forced to say what the operator DOES get instead — the assertion
/// cannot pass by the trigger having been silently dropped.
async fn drive_dm_end_on_operator_session(reason: ConversationEndReason) -> DmEndDelivery {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let agent_id = AgentId::new();
    let peer_agent_id = AgentId::new();

    // The operator's web-chat: both the trigger's target and the answer
    // `find_user_facing_session` gives the marker forward.
    let web_session = state.session_manager.get_or_create(agent_id, "web");
    let web_session_id = web_session.id;
    let mut web_rx = subscribe_session(&state, web_session_id);

    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx
        .send(RunTrigger {
            agent_id,
            session_id: web_session_id,
            input: "DM ended".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: peer_agent_id,
                from_name: "scout".to_string(),
                reason,
                self_notification: true,
                source_session_id: Some(web_session_id),
            },
            context_id: web_session.context_id.clone(),
        })
        .await
        .unwrap();
    drop(test_tx);

    super::notifications::run_trigger_loop(test_rx, state.clone()).await;

    let runs = state.run_manager.list_by_session(web_session_id, 10);
    let markers: Vec<alms_session::Message> = state
        .session_manager
        .get_history(web_session_id)
        .expect("web-chat session must exist")
        .into_iter()
        .filter(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("type").and_then(|v| v.as_str()) == Some("dm_ended_notification")
            })
        })
        .collect();
    let events: Vec<SseEventData> = drain_events(&mut web_rx)
        .into_iter()
        .filter(|e| e.event_type == "dm_conversation_ended")
        .collect();

    shutdown_token.cancel();

    DmEndDelivery {
        runs,
        markers,
        events,
    }
}

/// #1258, the reported incident: a DM run dies on an upstream 429, and 470ms
/// after the operator cancelled a run on their web-chat the DM-ended
/// notification puts a NEW run on that same session — work they did not ask
/// for, about a conversation they were not watching, indistinguishable from
/// the cancel having been ignored.
///
/// The run that was going to produce this DM's outcome died mid-turn, so it
/// must cost no further turn: the operator learns about it from the persisted
/// banner marker instead, which also has to carry the failure text now that no
/// prose turn explains it.
#[tokio::test]
async fn errored_dm_end_delivers_marker_and_starts_no_run() {
    let delivered = drive_dm_end_on_operator_session(ConversationEndReason::Errored {
        message: "LLM rate limit exceeded".to_string(),
        interrupted: true,
    })
    .await;

    assert!(
        delivered.runs.is_empty(),
        "an errored DM end whose run died must not start a run on the \
         operator's session; got {:?}",
        delivered.runs
    );

    assert_eq!(
        delivered.markers.len(),
        1,
        "the operator must still learn the DM ended — exactly one reloadable \
         marker; got {:?}",
        delivered.markers
    );
    let meta = delivered.markers[0]
        .metadata
        .as_ref()
        .expect("marker carries metadata");
    assert_eq!(meta.get("reason").and_then(|v| v.as_str()), Some("errored"));
    assert_eq!(
        meta.get("detail").and_then(|v| v.as_str()),
        Some("LLM rate limit exceeded"),
        "the marker must carry the failure text — with no run to narrate it, \
         this is the only place the operator can read WHY"
    );
    let alms_session::Content::Text(ref content) = delivered.markers[0].content else {
        panic!("marker content must be text");
    };
    assert!(
        content.contains("run failed") && content.contains("LLM rate limit exceeded"),
        "marker text must read as an explanation, not a raw reason code; got {content:?}"
    );

    let ended = delivered
        .events
        .first()
        .expect("the phase-clear SSE is unconditional");
    assert_ne!(
        ended.data.get("suppress_banner").and_then(|v| v.as_bool()),
        Some(true),
        "with no run standing in for it, the live banner must render"
    );
    assert_eq!(
        ended.data.get("detail").and_then(|v| v.as_str()),
        Some("LLM rate limit exceeded"),
        "the live banner must carry the same failure text as the marker"
    );
}

/// #1258, the other half of "interrupted": the operator cancelled. A cancel is
/// the strongest possible *stop doing things here* signal, so the end that
/// follows it must not reappear as a fresh run on the same session.
#[tokio::test]
async fn user_cancelled_dm_end_delivers_marker_and_starts_no_run() {
    let delivered = drive_dm_end_on_operator_session(ConversationEndReason::UserCancelled).await;

    assert!(
        delivered.runs.is_empty(),
        "a user-cancelled DM end must not start a run on the operator's \
         session; got {:?}",
        delivered.runs
    );
    assert_eq!(
        delivered.markers.len(),
        1,
        "the cancel must still be recorded for reload; got {:?}",
        delivered.markers
    );
    let meta = delivered.markers[0]
        .metadata
        .as_ref()
        .expect("marker carries metadata");
    assert_eq!(
        meta.get("reason").and_then(|v| v.as_str()),
        Some("user_cancelled")
    );
    assert!(
        meta.get("detail").is_none(),
        "a cancel carries no failure text — `detail` must stay off non-errored \
         markers so their shape is unchanged"
    );
    let alms_session::Content::Text(ref content) = delivered.markers[0].content else {
        panic!("marker content must be text");
    };
    assert!(
        content.contains("cancelled by user"),
        "marker text must read as an explanation, not a raw reason code; got {content:?}"
    );
    assert!(
        !delivered.events.is_empty(),
        "the phase-clear SSE is unconditional — otherwise the web-chat is \
         stuck showing 'Chatting with scout'"
    );
}

/// The control for the two tests above, on byte-identical wiring: a
/// *concluded* end still gets its notification run. Only the reason differs,
/// so "no run was created" above cannot be an artefact of the harness — this
/// same setup does create one.
///
/// It also pins the thing the #1258 suppression must not break: a DM that ran
/// its course carries a transcript the agent has to relay (the #429 history
/// embedding), and that relaying is the run.
#[tokio::test]
async fn concluded_dm_end_still_starts_its_notification_run() {
    let delivered = drive_dm_end_on_operator_session(ConversationEndReason::Ignored).await;

    assert_eq!(
        delivered.runs.len(),
        1,
        "a concluded DM end must still relay its outcome as a run; got {:?}",
        delivered.runs
    );
    assert!(
        delivered.markers.is_empty(),
        "the run IS the visible notification here, so the marker stays \
         suppressed (#1215 'initiator gets both'); got {:?}",
        delivered.markers
    );
    let ended = delivered
        .events
        .first()
        .expect("the phase-clear SSE is unconditional");
    assert_eq!(
        ended.data.get("suppress_banner").and_then(|v| v.as_bool()),
        Some(true),
        "the run is the notification, so the live banner is suppressed"
    );
}

/// #1258 / Tim's review of PR #1267 — the edge that keeps the suppression
/// from becoming the very bug it was written to avoid.
///
/// `Errored` covers two materially different things. `dm_lifecycle`'s Exit 3
/// (#1154, "agent run completed without producing a reply") and its
/// delivery-failure sibling both describe a run that **completed** — it just
/// had nothing usable on its LAST turn, possibly after several delivered
/// ones. Those earlier turns exist only in the `dm:` session; the
/// notification run and its #429 transcript are what carry them to the
/// operator's chat.
///
/// So this end must keep its run even though the reason string is `errored`.
/// Suppressing it would trade a spurious spinner for a silently dropped
/// answer — which is exactly why blanket "no run for a DM-ended
/// notification" was rejected in the first place.
///
/// Same harness as the two suppression tests above and as the concluded
/// control: only `interrupted` differs.
#[tokio::test]
async fn errored_dm_end_from_a_completed_run_still_starts_its_notification_run() {
    let delivered = drive_dm_end_on_operator_session(ConversationEndReason::Errored {
        message: "agent run completed without producing a reply".to_string(),
        interrupted: false,
    })
    .await;

    assert_eq!(
        delivered.runs.len(),
        1,
        "an `errored` end whose run COMPLETED still has a transcript to \
         relay, so it must keep its notification run; got {:?}",
        delivered.runs
    );
    assert!(
        delivered.markers.is_empty(),
        "the run IS the visible notification here, so the marker stays \
         suppressed (#1215 'initiator gets both'); got {:?}",
        delivered.markers
    );
    let ended = delivered
        .events
        .first()
        .expect("the phase-clear SSE is unconditional");
    assert_eq!(
        ended.data.get("reason").and_then(|v| v.as_str()),
        Some("errored"),
        "the `interrupted` split is a routing input, not a wire change — both \
         `Errored` shapes stay `errored` on the SSE"
    );
    assert_eq!(
        ended.data.get("suppress_banner").and_then(|v| v.as_bool()),
        Some(true),
        "the run is the notification, so the live banner is suppressed"
    );
}

/// #1258 / Tim's review of PR #1267 — why the suppression keys on "was the
/// turn cut short", not on "is the transcript empty".
///
/// The tempting predicate is the transcript: a notification run is
/// load-bearing precisely when it carries DM content the operator's web-chat
/// has never seen. It does not work as a predicate, because a DM that is
/// eligible to end always has content. `MessageBus::end_conversation`
/// refuses to run unless the DM session exists, and the session exists only
/// because a `send_message` persisted a message into it — so the initiating
/// message alone makes the history non-empty.
///
/// That is not a corner case, it is the reported incident: the ended run was
/// the *recipient's*, so the peer's opening message was already in the
/// transcript. A transcript-gated suppression would have let #1258's run
/// through.
#[tokio::test]
async fn an_interrupted_dm_end_still_has_a_transcript() {
    let (state, shutdown_token, _cr, mut trigger_rx, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    // Alice opens the DM. This is the ONLY message in it — the shape of the
    // #1258 incident at the moment bob's run died.
    let receipt = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "please review X", None)
        .await
        .expect("send must succeed");
    let _ = trigger_rx.try_recv(); // drain bob's DM-delivery trigger

    // Bob's DM run dies on an upstream 429 before he says anything.
    state
        .message_bus
        .end_conversation(
            "bob",
            bob_id,
            "alice",
            alice_id,
            ConversationEndReason::Errored {
                message: "LLM rate limit exceeded".to_string(),
                interrupted: true,
            },
        )
        .await
        .expect("end must succeed");

    let messages = state
        .session_manager
        .get_history(receipt.session_id)
        .expect("DM session must exist");
    let transcript = super::notifications::format_dm_conversation_history(&messages);
    assert!(
        !transcript.is_empty(),
        "an interrupted DM still has a transcript, so `conversation_history` \
         cannot be the suppression predicate; got {transcript:?}"
    );
    assert!(
        transcript.contains("please review X"),
        "the initiating message is what makes it non-empty; got {transcript:?}"
    );

    shutdown_token.cancel();
}

/// #1215: when a `ConversationEnded` trigger carries a `source_session_id`
/// (the agent initiated/ended from a user-facing session), the notification
/// RUN is routed to that source session and is itself the visible
/// notification there. The gateway must uphold BOTH:
///
/// 1. The redundant persisted `dm_ended_notification` banner marker is
///    SKIPPED — otherwise the DM-end renders twice ("initiator gets both").
/// 2. The lightweight `dm_conversation_ended` SSE is STILL forwarded to the
///    web-chat so the "Chatting with {peer}" status clears (Tim's C1 on
///    #1218). It is the ONLY path that reaches the web-chat, and the
///    notification run re-asserts the DM phase (#688) rather than clearing
///    it — so dropping the SSE with the marker would strand the status bar.
#[tokio::test]
async fn dm_conversation_ended_with_source_session_clears_phase_but_skips_marker() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let agent_id = AgentId::new();
    let sender_agent_id = AgentId::new();

    // The agent has a user-facing source session (its web-chat). The
    // MessageBus routes the notification run here and sets source_session_id.
    let source_session = state.session_manager.get_or_create(agent_id, "web");
    let source_session_id = source_session.id;
    let source_context_id = source_session.context_id.clone();

    // Subscribe to the source session so we can observe the phase-clear SSE.
    let mut src_rx = subscribe_session(&state, source_session_id);

    // Build a ConversationEnded trigger WITH a source session, mirroring how
    // the MessageBus emits the initiator/self notification.
    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx
        .send(RunTrigger {
            agent_id,
            session_id: source_session_id,
            input: "DM ended by alice".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: sender_agent_id,
                from_name: "alice".to_string(),
                reason: ConversationEndReason::Ignored,
                self_notification: false,
                source_session_id: Some(source_session_id),
            },
            context_id: source_context_id,
        })
        .await
        .unwrap();
    drop(test_tx);

    super::notifications::run_trigger_loop(test_rx, state.clone()).await;

    // The notification run still fires on the source session (#556 preserved).
    let runs = state.run_manager.list_by_session(source_session_id, 10);
    assert!(
        !runs.is_empty(),
        "the #556 self-notification run must still be created on the source session"
    );
    assert_eq!(runs[0].agent_id, agent_id);

    // (1) NO redundant dm_ended_notification banner marker is persisted to
    // the source session — the run is the single visible notification there.
    let history = state
        .session_manager
        .get_history(source_session_id)
        .unwrap_or_default();
    let banner_markers = history
        .iter()
        .filter(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("type").and_then(|v| v.as_str()) == Some("dm_ended_notification")
            })
        })
        .count();
    assert_eq!(
        banner_markers, 0,
        "no dm_ended_notification banner should be persisted when the run \
         already lands in the user-facing source session (#1215 initiator-gets-both)"
    );

    // (2) But the dm_conversation_ended SSE IS forwarded to the source session
    // so the web-chat clears the "Chatting with {peer}" phase (Tim's C1). The
    // reason here is `Ignored` and there is no agent registry, so the only
    // possible source of this event on the source session is the phase-clear
    // forward inside notify_dm_ended_to_webchat.
    let src_events = drain_events(&mut src_rx);
    let ended = src_events
        .iter()
        .find(|e| e.event_type == "dm_conversation_ended")
        .unwrap_or_else(|| {
            panic!(
                "the phase-clear dm_conversation_ended SSE must still reach the source \
                 session even when the marker is suppressed; got: {:?}",
                src_events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
            )
        });
    // ...and it carries suppress_banner=true: suppressing the marker must also
    // suppress the LIVE banner, so a live viewer sees only the notification run
    // (the live half of "initiator gets both", #1215).
    assert_eq!(
        ended.data.get("suppress_banner").and_then(|v| v.as_bool()),
        Some(true),
        "suppressing the reloadable marker must also suppress the live banner"
    );

    shutdown_token.cancel();
}

/// #1218 P2 (the #1202 job-as-DM-source interaction): when a scheduled job is
/// the DM source, `source_session_id = Some(job_session_id)` but the job
/// session is INTERNAL (`job_*`), so the notification run is routed to the job
/// session, NOT the agent's web-chat. The operator watching the web-chat must
/// therefore STILL get the persisted `dm_ended_notification` banner — there is
/// no visible run there to stand in for it. The marker is suppressed ONLY when
/// the source session IS the marker target; a `job_*` source is never the
/// user-facing target, so the banner persists.
#[tokio::test]
async fn dm_conversation_ended_job_source_persists_webchat_marker() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let agent_id = AgentId::new();
    let sender_agent_id = AgentId::new();

    // The DM source is the job's (internal) session — where the notification
    // run is routed, NOT the web-chat.
    let job_session = state
        .session_manager
        .get_or_create(agent_id, "job_550e8400-e29b-41d4-a716-446655440000");
    let job_session_id = job_session.id;
    let job_context_id = job_session.context_id.clone();

    // The agent also has a user-facing web-chat that the operator is watching.
    let web_session = state.session_manager.get_or_create(agent_id, "web");
    let web_session_id = web_session.id;

    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx
        .send(RunTrigger {
            agent_id,
            session_id: job_session_id,
            input: "DM ended by alice".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: sender_agent_id,
                from_name: "alice".to_string(),
                reason: ConversationEndReason::Ignored,
                self_notification: false,
                source_session_id: Some(job_session_id),
            },
            context_id: job_context_id,
        })
        .await
        .unwrap();
    drop(test_tx);

    super::notifications::run_trigger_loop(test_rx, state.clone()).await;

    // The DM-ended banner MUST be persisted to the web-chat: the notification
    // run went to the internal job session, so the web-chat has no visible run
    // to replace the banner. Pre-fix (persist_marker = source.is_none()) this
    // was suppressed -> operator saw a transient phase-clear but nothing
    // reloadable and no run (#1218 P2).
    let web_history = state
        .session_manager
        .get_history(web_session_id)
        .unwrap_or_default();
    let banner_markers = web_history
        .iter()
        .filter(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("type").and_then(|v| v.as_str()) == Some("dm_ended_notification")
            })
        })
        .count();
    assert_eq!(
        banner_markers, 1,
        "a job-as-DM-source DM-end must persist the web-chat banner (the run is \
         routed to the internal job session, not the web-chat) — #1218 P2"
    );

    shutdown_token.cancel();
}

/// #1218 P2 (second): `notify_dm_ended_to_webchat` targets the agent's
/// MOST-RECENT user-facing session, which can differ from the source session
/// the run is routed to when the agent has more than one user-facing chat.
/// The marker must still be persisted to the target chat — otherwise that
/// chat gets neither the run (it is in the source chat) nor a reloadable
/// banner, only a transient SSE. The marker is suppressed ONLY when the
/// source session IS the exact marker target (`source_session_id == target`),
/// which is the identity check that subsumes the earlier internal-source case.
#[tokio::test]
async fn dm_conversation_ended_source_differs_from_marker_target_persists_marker() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let agent_id = AgentId::new();
    let sender_agent_id = AgentId::new();

    // The agent has TWO user-facing sessions.
    let web_a = state.session_manager.get_or_create(agent_id, "web-a");
    let web_b = state.session_manager.get_or_create(agent_id, "web-b");

    // Discover the actual marker target (the most-recent user-facing session)
    // and pick a DIFFERENT user-facing session as the DM source, so the check
    // is deterministic regardless of last-activity ordering.
    let target = super::find_user_facing_session(&state.session_manager, agent_id)
        .expect("agent has user-facing sessions");
    let target_id = target.id;
    let (source_id, source_ctx) = if target_id == web_a.id {
        (web_b.id, "web-b")
    } else {
        (web_a.id, "web-a")
    };
    assert_ne!(
        source_id, target_id,
        "source must differ from the marker target"
    );

    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx
        .send(RunTrigger {
            agent_id,
            session_id: source_id,
            input: "DM ended by alice".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: sender_agent_id,
                from_name: "alice".to_string(),
                reason: ConversationEndReason::Ignored,
                self_notification: false,
                source_session_id: Some(source_id),
            },
            context_id: source_ctx.to_string(),
        })
        .await
        .unwrap();
    drop(test_tx);

    super::notifications::run_trigger_loop(test_rx, state.clone()).await;

    // The banner MUST land on the marker target: the run is in the source
    // chat (a different session), so the target chat has no visible run to
    // replace the banner. Pre-fix (marker suppressed whenever the source was
    // user-facing) the target chat got only a transient SSE (#1218 P2).
    let target_history = state
        .session_manager
        .get_history(target_id)
        .unwrap_or_default();
    let banner_markers = target_history
        .iter()
        .filter(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("type").and_then(|v| v.as_str()) == Some("dm_ended_notification")
            })
        })
        .count();
    assert_eq!(
        banner_markers, 1,
        "marker must persist to the most-recent user-facing session when the DM \
         source is a DIFFERENT user-facing chat than the target (#1218 P2)"
    );

    shutdown_token.cancel();
}

/// #1218 P2 (#3, the ordering / #1205 episode-routing case): when an open job
/// episode awaits the SAME DM that a user-facing web-chat also sourced, the
/// #1205 episode override REROUTES the notification run to the job session.
/// The web-chat is then not a run target, so its reloadable banner must
/// persist — otherwise the operator watching the web-chat gets neither the run
/// (it went to the job session) nor a marker, only a transient SSE. This is
/// why the web-chat forward is deferred until the FINAL `run_targets` are
/// known: keying persistence on `source_session_id` alone (pre-fix) suppressed
/// the marker here because source == the web-chat target.
#[tokio::test]
async fn dm_conversation_ended_episode_rerouted_run_persists_webchat_marker() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();

    // Register alice + bob so peer-name resolution (bob) works and resolve_dm
    // can compute the deterministic DM session id.
    let (alice_id, bob_id) = seed_alice_bob(&state);

    // bob has a user-facing web-chat (the DM source AND the marker target) ...
    let web_session = state.session_manager.get_or_create(bob_id, "web");
    let web_session_id = web_session.id;

    // ... and an open job episode awaiting the alice<->bob DM.
    let job_id = alms_core::JobId::new();
    let job_ctx = format!("job_{}", job_id.0);
    let job_session = state.session_manager.get_or_create(bob_id, &job_ctx);
    let job_session_id = job_session.id;
    let turn1 = alms_core::RunId::new();
    state
        .job_episodes
        .open(job_id, job_session_id, bob_id, turn1);
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    let _ = state
        .job_episodes
        .on_run_complete(job_id, turn1, vec![dm_session_id], vec![]);

    // ConversationEnded for bob, sourced from bob's web-chat. Episode routing
    // supersedes the web-chat target and sends the run to the job session.
    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx
        .send(RunTrigger {
            agent_id: bob_id,
            session_id: web_session_id,
            input: "DM ended by alice".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: alice_id,
                from_name: "alice".to_string(),
                reason: ConversationEndReason::Ignored,
                self_notification: false,
                source_session_id: Some(web_session_id),
            },
            context_id: "web".to_string(),
        })
        .await
        .unwrap();
    drop(test_tx);

    super::notifications::run_trigger_loop(test_rx, state.clone()).await;

    // The continuation run went to the JOB session (episode routing fired) ...
    let job_runs = state.run_manager.list_by_session(job_session_id, 10);
    assert!(
        !job_runs.is_empty(),
        "episode routing must send the continuation run to the job session"
    );
    // ... and NOT to the web-chat.
    let web_runs = state.run_manager.list_by_session(web_session_id, 10);
    assert!(
        web_runs.is_empty(),
        "the run must NOT land on the web-chat when episode routing reroutes it"
    );

    // So the web-chat still needs the reloadable banner: it must be persisted
    // even though the DM source == the web-chat target (#1218 P2 ordering).
    let web_history = state
        .session_manager
        .get_history(web_session_id)
        .unwrap_or_default();
    let banner_markers = web_history
        .iter()
        .filter(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("type").and_then(|v| v.as_str()) == Some("dm_ended_notification")
            })
        })
        .count();
    assert_eq!(
        banner_markers, 1,
        "marker must persist to the web-chat when the run is rerouted to a job \
         session by #1205 episode routing (#1218 P2 ordering)"
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
    let _dm_session = state
        .session_manager
        .get_or_create_with_id(dm_session_id, agent_id, "dm:alice:bob")
        .unwrap();
    let mut dm_rx = subscribe_session(&state, dm_session_id);

    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx
        .send(RunTrigger {
            agent_id,
            session_id: notif_session_id,
            input: "DM depth exceeded".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: sender_agent_id,
                from_name: "alice".to_string(),
                reason: ConversationEndReason::DepthExceeded,
                self_notification: false,
                source_session_id: None,
            },
            context_id: notif_context_id.clone(),
        })
        .await
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
        parent_tool_invocation_id: None,
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
        parent_tool_invocation_id: None,
    };

    let notification = super::notifications::format_completion_notification(&completion);
    assert!(
        notification.contains("cancelled"),
        "notification should indicate the subagent was cancelled"
    );
}

/// #1181: for an EPHEMERAL / unnamed subagent, the completion notification
/// must point the parent at the by-session-id readback
/// (`read_subagent_session(session_id=...)`). Pre-#1181 it said only "the
/// summary is included above", leaving the parent no discoverable path to
/// the persisted full output — the live incident had the parent conclude
/// "there's no named session to read back" while the complete transcript
/// sat readable at the subagent's session.
#[test]
fn format_completion_notification_for_unnamed_subagent_points_at_session_id_readback() {
    let subagent_session_id = SessionId::new();
    let completion = SubagentCompletion {
        task_id: TaskId::new(),
        subagent_name: None, // ephemeral / unnamed
        status: TaskStatus::Completed,
        summary: "Research finished (truncated summary)".to_string(),
        parent_session_id: SessionId::new(),
        parent_agent_id: AgentId::new(),
        subagent_session_id,
        task_description: Some("Research the topic".to_string()),
        tool_count: Some(4),
        duration_ms: Some(9000),
        token_usage: None,
        parent_tool_invocation_id: None,
    };

    let notification = super::notifications::format_completion_notification(&completion);
    assert!(
        notification.contains("read_subagent_session"),
        "unnamed completion must point at the readback tool, got: {notification}"
    );
    assert!(
        notification.contains(&subagent_session_id.0.to_string()),
        "unnamed completion must carry the subagent's session id, got: {notification}"
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
    let _ = state.run_manager.insert_run(run);

    // Simulate what execute_run does on FailedWithToolCalls: mark as failed
    // with the error message while tool calls are persisted separately.
    let error_msg = "LLM API error after 2 tool calls".to_string();
    assert!(
        state
            .run_manager
            .mark_run_as_failed(run_id, error_msg.clone())
    );

    let run = state.run_manager.get_run(run_id).expect("run should exist");
    assert_eq!(run.status(), RunStatus::Failed);
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
    let _ = state.run_manager.insert_run(run);

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
        run.status(),
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
    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx
        .send(RunTrigger {
            agent_id,
            session_id,
            input: "Subagent completed its task".to_string(),
            source: MessageSource::SubagentCompletion,
            context_id: session.context_id.clone(),
        })
        .await
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
            parent_tool_invocation_id: None,
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

/// Drive `forward_runtime_events` with a single foreground
/// `RuntimeEvent::SubagentStarted` and assert it persists a durable
/// `subagent_started` lifecycle marker to the parent's session history
/// (#1125, A1-1).
///
/// Helper: pre-seeds an `invoke_agent` `Content::ToolCall` row into history
/// (mirroring the runtime agent loop, which appends the call row before
/// `tool.execute()` runs), runs `forward_runtime_events` on the supplied
/// event to completion, and returns the resulting parent-session history.
///
/// `subagent_name` is threaded into the `SubagentStarted` event so callers can
/// exercise both the named (`Some(..)`) and ephemeral/unnamed (`None`) paths —
/// the latter pins the conditional key-omission in the marker arm of
/// `forward_runtime_events`.
async fn run_subagent_started_marker_case(
    context_id: &str,
    background: bool,
    subagent_name: Option<&str>,
) -> (Vec<alms_session::Message>, uuid::Uuid, SessionId) {
    use alms_runtime::RuntimeEvent;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let parent_agent_id = AgentId::new();
    let parent_session = state
        .session_manager
        .get_or_create(parent_agent_id, context_id);
    let parent_session_id = parent_session.id;

    let tool_invocation_id = uuid::Uuid::new_v4();
    let subagent_session_id = SessionId::new();

    // Seed the parent's `invoke_agent` tool-call row BEFORE the marker, just
    // like the runtime agent loop (loop_impl.rs ~343) appends the call row
    // before `tool.execute()` runs. The marker must land AFTER this row so
    // the rehydrator sees the chip's tool row first.
    state
        .session_manager
        .append_message(
            parent_session_id,
            alms_session::Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: alms_session::Role::Assistant,
                content: alms_session::Content::ToolCall {
                    name: "invoke_agent".to_string(),
                    params: serde_json::json!({ "agent_name": "researcher" }),
                },
                timestamp: alms_core::Timestamp::now(),
                metadata: None,
            },
        )
        .unwrap();

    // Drive `forward_runtime_events` with one foreground SubagentStarted.
    let (tx, rx) = mpsc::unbounded_channel::<RuntimeEvent>();
    tx.send(RuntimeEvent::SubagentStarted {
        tool_invocation_id,
        subagent_name: subagent_name.map(str::to_string),
        subagent_session_id,
        background,
    })
    .unwrap();
    drop(tx); // close the channel so the forwarder loop terminates

    super::tools::forward_runtime_events(
        rx,
        RunId::new(),
        parent_session_id,
        state.run_manager.clone(),
        state.approval_store.clone(),
        state.session_manager.clone(),
        context_id.to_string(),
        None,
    )
    .await;

    let history = state
        .session_manager
        .get_history(parent_session_id)
        .unwrap();

    shutdown_token.cancel();
    (history, tool_invocation_id, subagent_session_id)
}

/// #1125 (A1-1): a FOREGROUND `subagent_started` event on a user-facing
/// session must persist a durable lifecycle marker carrying
/// `tool_invocation_id` + `subagent_session_id` + `subagent_name`,
/// positioned AFTER the parent's `invoke_agent` tool-call row.
///
/// Falsifiable: removing the `persist_lifecycle_marker` call in the
/// `SubagentStarted` arm of `forward_runtime_events` drops the marker and
/// fails the "exactly one marker" assertion.
#[tokio::test]
async fn foreground_subagent_started_persists_marker_after_invoke_row() {
    let (history, tool_invocation_id, subagent_session_id) =
        run_subagent_started_marker_case("web", false, Some("researcher")).await;

    // Locate the marker and the parent's invoke_agent tool-call row.
    let marker_pos = history.iter().position(|m| {
        m.metadata.as_ref().is_some_and(|meta| {
            meta.get("type").and_then(|v| v.as_str()) == Some("subagent_started")
        })
    });
    let invoke_pos = history.iter().position(|m| {
        matches!(
            &m.content,
            alms_session::Content::ToolCall { name, .. } if name == "invoke_agent"
        )
    });

    let marker_pos = marker_pos.expect("expected a subagent_started marker in parent history");
    let invoke_pos = invoke_pos.expect("expected the seeded invoke_agent tool-call row");

    // Exactly one marker (no duplicate persistence).
    let marker_count = history
        .iter()
        .filter(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("type").and_then(|v| v.as_str()) == Some("subagent_started")
            })
        })
        .count();
    assert_eq!(
        marker_count, 1,
        "expected exactly one subagent_started marker"
    );

    // ORDERING INVARIANT: marker lands AFTER the invoke_agent tool row so the
    // rehydrator resolves the chip's tool row first, then fills its session id.
    assert!(
        marker_pos > invoke_pos,
        "subagent_started marker (idx {marker_pos}) must come AFTER the \
         invoke_agent tool-call row (idx {invoke_pos})"
    );

    // Metadata shape — the keys Iris's history.js + subagents.js consume.
    let marker = &history[marker_pos];
    assert_eq!(marker.role, alms_session::Role::System);
    let meta = marker.metadata.as_ref().unwrap();
    assert_eq!(meta["synthetic"], true);
    assert_eq!(meta["type"], "subagent_started");
    assert_eq!(
        meta["tool_invocation_id"].as_str(),
        Some(tool_invocation_id.to_string().as_str()),
        "marker must carry the parent's invoke_agent tool_invocation_id (#1127 disambiguator)"
    );
    assert_eq!(
        meta["subagent_session_id"].as_str(),
        Some(subagent_session_id.0.to_string().as_str()),
        "marker must carry the subagent's session id for chip rehydration"
    );
    assert_eq!(
        meta["subagent_name"].as_str(),
        Some("researcher"),
        "marker must carry the subagent name when present"
    );

    // The marker is DM-hygiene-filtered like every other lifecycle marker.
    assert!(
        alms_tools::dm_filter::is_synthetic_marker(marker),
        "subagent_started marker must be filtered by is_synthetic_marker"
    );
}

/// #1125 (A1-1): an UNNAMED (ephemeral) foreground `subagent_started` event
/// must persist a marker that OMITS the `subagent_name` key entirely —
/// matching the live SSE event's `skip_serializing_if` so the frontend
/// rehydrator (which reads `md.subagent_name` defensively as possibly
/// undefined) never sees a `null`/empty name.
///
/// This pins the conditional `if let Some(name) = subagent_name` key insert in
/// the `SubagentStarted` arm of `forward_runtime_events` at the PERSISTED
/// MARKER level — the named case (above) exercises the `Some(..)` branch, this
/// exercises the `None` branch. Falsifiable: unconditionally writing
/// `subagent_name` (e.g. `json!(null)`) fails the `is_none()` assertion.
#[tokio::test]
async fn unnamed_foreground_subagent_started_omits_subagent_name() {
    let (history, tool_invocation_id, subagent_session_id) =
        run_subagent_started_marker_case("web", false, None).await;

    // The marker is still persisted (foreground, non-internal) — only the
    // `subagent_name` key differs from the named case.
    let marker = history
        .iter()
        .find(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("type").and_then(|v| v.as_str()) == Some("subagent_started")
            })
        })
        .expect("expected a subagent_started marker for the unnamed foreground case");

    let meta = marker.metadata.as_ref().unwrap();

    // The id keys the frontend joins on are still present and correct — the
    // rehydrator resolves the chip by `tool_invocation_id`, not by name, so the
    // omitted name must not cost it the session id.
    assert_eq!(
        meta["tool_invocation_id"].as_str(),
        Some(tool_invocation_id.to_string().as_str()),
        "unnamed marker must still carry the tool_invocation_id join key"
    );
    assert_eq!(
        meta["subagent_session_id"].as_str(),
        Some(subagent_session_id.0.to_string().as_str()),
        "unnamed marker must still carry the subagent session id"
    );

    // CONTRACT: the `subagent_name` key is ABSENT (not present-as-null) for an
    // ephemeral/unnamed subagent — the wire shape Iris's frontend depends on.
    assert!(
        meta.get("subagent_name").is_none(),
        "subagent_name must be OMITTED from the marker for an unnamed subagent, \
         got {:?}",
        meta.get("subagent_name")
    );

    // The unnamed `display_text` branch renders without a name suffix.
    let alms_session::Content::Text(ref text) = marker.content else {
        panic!("expected text content on the subagent_started marker");
    };
    assert_eq!(
        text, "Subagent started.",
        "unnamed marker display text must omit the ' '<name>'' suffix"
    );
}

/// #1125 (A1-1): the BACKGROUND path must NOT persist a `subagent_started`
/// marker — background subagents are already reload-safe via their persisted
/// `{task_id, session_id}` tool result, so a start marker would be redundant.
#[tokio::test]
async fn background_subagent_started_persists_no_marker() {
    let (history, _inv, _sub) =
        run_subagent_started_marker_case("web", true, Some("researcher")).await;

    let has_marker = history.iter().any(|m| {
        m.metadata.as_ref().is_some_and(|meta| {
            meta.get("type").and_then(|v| v.as_str()) == Some("subagent_started")
        })
    });
    assert!(
        !has_marker,
        "background subagent_started must NOT persist a marker (already reload-safe)"
    );
}

/// #1125 (A1-1): internal / DM contexts must NOT accrue a `subagent_started`
/// marker — gated by `!is_internal_context_id`, exactly like the
/// `run_warning` marker. A `subagent_` context id is internal.
#[tokio::test]
async fn internal_context_subagent_started_persists_no_marker() {
    // `subagent_…` is classified internal by `is_internal_context_id`.
    let (history, _inv, _sub) =
        run_subagent_started_marker_case("subagent_task-internal", false, Some("researcher")).await;

    let has_marker = history.iter().any(|m| {
        m.metadata.as_ref().is_some_and(|meta| {
            meta.get("type").and_then(|v| v.as_str()) == Some("subagent_started")
        })
    });
    assert!(
        !has_marker,
        "internal/DM-context subagent_started must NOT persist a marker"
    );
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
            parent_tool_invocation_id: None,
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
            parent_tool_invocation_id: None,
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
            parent_tool_invocation_id: None,
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
    let notification = super::notifications::format_dm_ended_notification(
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
    let ignored = super::notifications::format_dm_ended_notification(
        "alice",
        ConversationEndReason::Ignored,
        None,
        false,
    );
    let depth = super::notifications::format_dm_ended_notification(
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

    let execution_barrier = super::lifecycle::install_admission_execution_barrier(session_id);

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
    let (status, resp) = match super::lifecycle::create_run(State(state.clone()), Json(req)).await {
        Ok(ok) => ok,
        Err((code, body)) => panic!("create_run failed: status={code:?} body={:?}", body.0),
    };
    assert_eq!(status, axum::http::StatusCode::CREATED);

    // Rendezvous before the executor claims the input, then keep it paused
    // while the admission snapshot is inspected below.
    execution_barrier.wait().await;

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
    let correlated_run_id = user_msgs[0]
        .metadata
        .as_ref()
        .and_then(|md| md.get("run_id"))
        .and_then(|value| value.as_str());
    let expected_run_id = resp.0.run_id.0.to_string();
    assert_eq!(
        correlated_run_id,
        Some(expected_run_id.as_str()),
        "pre-persisted input must carry its authoritative run id",
    );

    shutdown_token.cancel();
    execution_barrier.wait().await;
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
/// against `run.resolved_config()` from the persisted run, NOT against a
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
        .resolved_config()
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
    // (The snapshot value here is exactly the agent record's `debug_mode`
    // — the #546-era notification-flip that could override it for
    // system-triggered runs was removed.)
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

// =====================================================================
// #1148 — the server-default `(model, provider)` pair is live for the
// NEXT run, with no daemon restart.
//
// These are the end-to-end pins. Everything below drives the real
// `create_run` -> queue -> `execute_run` chain and asserts against
// `run.resolved_config()` — the snapshot `mark_run_as_running_with_config`
// takes from the `LlmClient` the agent loop is about to send on. A test
// that only asserted `state.server_llm_default` changed would pass with
// the run path still reading a boot-time clone, i.e. it would pass
// against the bug.
// =====================================================================

/// Everything a #1148 test needs kept alive for its duration.
///
/// The `data_dir` tempdir is load-bearing, not tidiness. `AppState::new`
/// reads `{data_dir}/settings.json` at boot and `persist_settings` writes
/// it on every accepted PATCH, so a harness that leaves `data_dir` at the
/// `GatewayConfig::default()` cwd-relative `./.alms` would have one test's
/// PATCH silently become the next test's boot-time server default —
/// order-dependent failures that only reproduce when the suite is run in
/// a particular sequence. Each harness gets its own directory instead.
struct LlmDefaultHarness {
    state: AppState,
    _data_dir: tempfile::TempDir,
    _shutdown_token: CancellationToken,
    _completion_rx: mpsc::UnboundedReceiver<SubagentCompletion>,
    _trigger_rx: mpsc::Receiver<RunTrigger>,
    _dm_event_rx: mpsc::Receiver<DmEvent>,
}

/// Build an `AppState` with a mock LLM, a SQLite store, an isolated
/// `data_dir`, and a populated `[llm.providers]` map, so `PATCH /settings`
/// can validate provider switches (the map is config-file-only and
/// `GatewayConfig::default()` leaves it empty).
fn llm_default_harness(provider: &str, model: &str) -> LlmDefaultHarness {
    let mut providers = std::collections::BTreeMap::new();
    providers.insert(
        "openrouter".to_string(),
        alms_core::config::ProviderEntry {
            kind: alms_core::config::ProviderKind::OpenAiCompatible,
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key_env: None,
            api_key: Some("sk-or-test".into()),
            model: None,
            auth_scheme: alms_core::config::AuthScheme::Bearer,
            quirks: alms_core::config::ProviderQuirks::default(),
        },
    );
    providers.insert(
        "anthropic".to_string(),
        alms_core::config::ProviderEntry {
            kind: alms_core::config::ProviderKind::Anthropic,
            base_url: "https://api.anthropic.com/v1".into(),
            api_key_env: None,
            api_key: Some("sk-ant-test".into()),
            // No entry-level model on purpose: it is what makes a
            // provider-only switch fall through to the #863 decision.
            model: None,
            auth_scheme: alms_core::config::AuthScheme::Header {
                name: "x-api-key".into(),
            },
            quirks: alms_core::config::ProviderQuirks::default(),
        },
    );
    let llm_config = alms_runtime::LlmConfig {
        mock: true,
        provider: provider.to_string(),
        default_model: model.to_string(),
        providers,
        ..alms_runtime::LlmConfig::default()
    };
    let data_dir = tempfile::tempdir().expect("tempdir for isolated settings.json");
    let gateway_config = GatewayConfig {
        db_path: Some(":memory:".to_string()),
        data_dir: Some(data_dir.path().to_path_buf()),
        llm_config,
        ..GatewayConfig::default()
    };
    let gateway = crate::gateway::Gateway::new(gateway_config).unwrap();
    let scheduler = Arc::new(alms_runtime::Scheduler::new());
    let shutdown_token = CancellationToken::new();
    let (completion_tx, completion_rx) = mpsc::unbounded_channel();
    let (trigger_tx, trigger_rx) = mpsc::channel(64);
    let (dm_event_tx, dm_event_rx) = mpsc::channel(64);
    let state = AppState::new(
        gateway,
        scheduler,
        shutdown_token.clone(),
        completion_tx,
        trigger_tx,
        dm_event_tx,
    )
    .unwrap();
    LlmDefaultHarness {
        state,
        _data_dir: data_dir,
        _shutdown_token: shutdown_token,
        _completion_rx: completion_rx,
        _trigger_rx: trigger_rx,
        _dm_event_rx: dm_event_rx,
    }
}

/// Seed an agent record with the given per-agent overrides.
fn seed_llm_default_test_agent(
    state: &AppState,
    name: &str,
    model: Option<&str>,
    provider: Option<&str>,
) -> AgentId {
    use alms_core::registry::AgentRecord;
    use chrono::Utc;

    let agent_id = AgentId::new();
    let now = Utc::now();
    let agent = AgentRecord {
        id: agent_id,
        name: name.to_string(),
        description: String::new(),
        model: model.map(str::to_string),
        posture: None,
        provider: provider.map(str::to_string),
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
    agent_id
}

/// Apply a `PATCH /settings` body and assert it was accepted.
///
/// Goes through the real handler (not a direct `server_llm_default`
/// write) so every test below exercises the validation + commit + client
/// rebuild chain an operator's UI click actually takes.
async fn patch_server_default(state: &AppState, patch: serde_json::Value) {
    use axum::Json;
    use axum::extract::State;
    use axum::response::IntoResponse;

    let resp = crate::settings::patch_settings(State(state.clone()), Json(patch.clone()))
        .await
        .into_response();
    assert_eq!(
        resp.status(),
        axum::http::StatusCode::OK,
        "PATCH /settings {patch} must be accepted"
    );
}

/// Drive one `POST /runs` to `run_started` and return the
/// `ResolvedRunConfig` the run path actually committed.
async fn resolved_config_for_one_run(
    state: &AppState,
    agent_id: AgentId,
    context_id: &str,
) -> alms_core::ResolvedRunConfig {
    use alms_core::CreateRunRequest;
    use axum::Json;
    use axum::extract::State;

    let session = state.session_manager.get_or_create(agent_id, context_id);
    let session_id = session.id;
    // Subscribe BEFORE `create_run`: the producer persists the resolved
    // snapshot via `mark_run_as_running_with_config` immediately before
    // broadcasting `run_started` (#895 ordering), so observing the event
    // is sufficient to know the snapshot is queryable.
    let mut session_rx = subscribe_session(state, session_id);

    let req = CreateRunRequest {
        session_id,
        ..serde_json::from_value(serde_json::json!({
            "session_id": session_id.0.to_string(),
            "input": { "type": "text", "text": "which model am I?" },
        }))
        .expect("CreateRunRequest must deserialize")
    };

    let (status, resp) = match super::lifecycle::create_run(State(state.clone()), Json(req)).await {
        Ok(ok) => ok,
        Err((code, body)) => panic!("create_run failed: status={code:?} body={:?}", body.0),
    };
    assert_eq!(status, axum::http::StatusCode::CREATED);
    let run_id = resp.0.run_id;

    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), session_rx.recv())
            .await
            .expect("test must observe run_started within 10s")
            .expect("session sender must not close before run_started");
        if event.event_type == "run_started" {
            break;
        }
    }

    state
        .run_manager
        .get_run(run_id)
        .expect("run must exist after create_run enqueued it")
        .resolved_config()
        .expect("resolved_config must be populated once the run reaches Running")
        .clone()
}

/// **The core acceptance test for #1148.** A `PATCH /settings` that moves
/// the server-default model must reach the *next* run — no restart.
///
/// Pre-fix, `state.llm` was a by-value clone taken in `AppState::new`, so
/// this run resolved against the boot model and the operator was told
/// (correctly, at the time) that a restart was required. Post-fix the
/// PATCH rebuilds the shared client the run path reads.
///
/// The assertion is on `run.resolved_config().model` — the snapshot taken
/// from the client the agent loop is about to send on — not on
/// `server_llm_default`, which would still be green with the bug present.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patched_server_default_model_reaches_the_next_run_without_restart() {
    let harness = llm_default_harness("openrouter", "z-ai/glm-5.2");
    let state = &harness.state;
    // No per-agent model or provider — this agent inherits the server
    // default, which is the population the issue is about.
    let agent_id = seed_llm_default_test_agent(state, "inherits-default", None, None);

    // Baseline: the boot pair is what a run resolves to today.
    let before = resolved_config_for_one_run(state, agent_id, "web-before").await;
    assert_eq!(before.model, "z-ai/glm-5.2");
    assert_eq!(before.provider, "openrouter");

    // Operator changes the server-default model in the UI.
    patch_server_default(
        state,
        serde_json::json!({ "model": "moonshotai/kimi-k2.5" }),
    )
    .await;

    // ...and the very next run uses it. No restart in between.
    let after = resolved_config_for_one_run(state, agent_id, "web-after").await;
    assert_eq!(
        after.model, "moonshotai/kimi-k2.5",
        "#1148: the next run must resolve against the patched server default. \
         A boot-time `state.llm` clone would still report z-ai/glm-5.2 here."
    );
    assert_eq!(
        after.provider, "openrouter",
        "a model-only PATCH must not disturb the provider"
    );
}

/// A live server-default switch must not step on agents that carry their
/// own model. Per-agent overrides are the higher-precedence layer and
/// stay that way — `PATCH /settings` only moves the value agents fall
/// back to.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patched_server_default_does_not_override_a_per_agent_model() {
    let harness = llm_default_harness("openrouter", "z-ai/glm-5.2");
    let state = &harness.state;
    let pinned =
        seed_llm_default_test_agent(state, "pinned-agent", Some("openai/gpt-4o-mini"), None);
    let inheriting = seed_llm_default_test_agent(state, "inheriting-agent", None, None);

    patch_server_default(
        state,
        serde_json::json!({ "model": "moonshotai/kimi-k2.5" }),
    )
    .await;

    let pinned_cfg = resolved_config_for_one_run(state, pinned, "web").await;
    assert_eq!(
        pinned_cfg.model, "openai/gpt-4o-mini",
        "a per-agent model must still win over a freshly patched server default"
    );

    let inheriting_cfg = resolved_config_for_one_run(state, inheriting, "web").await;
    assert_eq!(
        inheriting_cfg.model, "moonshotai/kimi-k2.5",
        "an agent without an override must pick the new default up — the same \
         PATCH has to move one agent and not the other"
    );
}

/// A live server-default **provider** switch retargets the wire for the
/// next run: provider name and model both move, and the mock adapter the
/// run resolves is rebuilt from `[llm.providers.anthropic]`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patched_server_default_provider_reaches_the_next_run_without_restart() {
    let harness = llm_default_harness("openrouter", "z-ai/glm-5.2");
    let state = &harness.state;
    let agent_id = seed_llm_default_test_agent(state, "inherits-default", None, None);

    patch_server_default(
        state,
        serde_json::json!({
            "provider": "anthropic",
            "model": "claude-sonnet-4-6",
        }),
    )
    .await;

    let after = resolved_config_for_one_run(state, agent_id, "web").await;
    assert_eq!(after.provider, "anthropic");
    assert_eq!(after.model, "claude-sonnet-4-6");
}

/// **The coherence constraint.** The `#863`
/// `MISSING_MODEL_AFTER_PROVIDER_SWITCH` decision must be computed
/// against the *live* server-default pair, not the boot-time one.
///
/// The agent here pins `provider: anthropic` and carries no model. While
/// the server default is also `anthropic` that is not a switch at all, so
/// the run inherits the server-default model and starts fine. The moment
/// the operator moves the server default to `openrouter`, the same agent
/// record becomes a genuine provider switch with no model available at
/// any layer — and `POST /runs` must reject it with the structured 400
/// before any LLM call.
///
/// If the run path still read a boot-time client, the second `create_run`
/// would happily succeed and the agent would send an OpenRouter slug to
/// Anthropic's wire. Getting this wrong is what turns a config change
/// into a fleet of opaque downstream 4xx errors, so it is pinned
/// end-to-end rather than at the helper.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_model_after_provider_switch_is_judged_against_the_live_default() {
    use alms_core::CreateRunRequest;
    use axum::Json;
    use axum::extract::State;

    let harness = llm_default_harness("anthropic", "claude-sonnet-4-6");
    let state = &harness.state;
    // Per-agent provider matching the server default => not a switch.
    let agent_id = seed_llm_default_test_agent(state, "anthropic-pinned", None, Some("anthropic"));

    let before = resolved_config_for_one_run(state, agent_id, "web-before").await;
    assert_eq!(before.provider, "anthropic");
    assert_eq!(
        before.model, "claude-sonnet-4-6",
        "baseline: no provider switch, so the server-default model applies"
    );

    // Move the server default off anthropic. The agent record is
    // untouched — but it is now a provider switch with no model anywhere.
    patch_server_default(
        state,
        serde_json::json!({
            "provider": "openrouter",
            "model": "z-ai/glm-5.2",
        }),
    )
    .await;

    let session = state.session_manager.get_or_create(agent_id, "web-after");
    let req = CreateRunRequest {
        session_id: session.id,
        ..serde_json::from_value(serde_json::json!({
            "session_id": session.id.0.to_string(),
            "input": { "type": "text", "text": "should be rejected" },
        }))
        .expect("CreateRunRequest must deserialize")
    };
    let err = super::lifecycle::create_run(State(state.clone()), Json(req))
        .await
        .expect_err(
            "the live provider switch must be rejected — a boot-time client \
             would have let this run through with a cross-namespace model",
        );
    assert_eq!(err.0, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(
        err.1.0["error_code"], "MISSING_MODEL_AFTER_PROVIDER_SWITCH",
        "the structured 400 must still fire, now against the live pair: {:?}",
        err.1.0
    );
    assert_eq!(err.1.0["new_provider"], "anthropic");
    assert_eq!(
        err.1.0["prev_provider"], "openrouter",
        "`prev_provider` must name the LIVE server default, not the boot one"
    );
}

/// In-flight runs are unaffected: a PATCH that lands while a run is
/// already executing must not retarget that run's wire. The run resolves
/// its client once at start and holds it by value for the duration.
///
/// **Coverage boundary — this is the weakest of the five #1148 pins, and
/// deliberately so.** `resolved_config()` is written once by
/// `try_mark_run_as_running_with_config` and never rewritten, and nothing
/// here holds the run open past `run_started` — with the mock adapter it
/// has most likely already finished. So the assertion would stay green
/// even if the runtime *did* re-read the shared handle mid-run.
///
/// The property itself is structurally guaranteed rather than test-
/// enforced: `llm` is moved into `AgentRuntime::new` and the loop owns it
/// by value, so there is no handle left to re-read. Holding a run open to
/// give the assertion something to fail against would need a tool-gated
/// mock adapter that does not exist in-repo. What this test does earn is
/// the other half of the claim — that the PATCH landed and moved the live
/// client — which is asserted explicitly below.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patching_the_default_mid_run_does_not_disturb_the_in_flight_run() {
    use alms_core::CreateRunRequest;
    use axum::Json;
    use axum::extract::State;

    let harness = llm_default_harness("openrouter", "z-ai/glm-5.2");
    let state = &harness.state;
    let agent_id = seed_llm_default_test_agent(state, "inherits-default", None, None);

    let session = state.session_manager.get_or_create(agent_id, "web");
    let session_id = session.id;
    let mut session_rx = subscribe_session(state, session_id);

    let req = CreateRunRequest {
        session_id,
        ..serde_json::from_value(serde_json::json!({
            "session_id": session_id.0.to_string(),
            "input": { "type": "text", "text": "in flight" },
        }))
        .expect("CreateRunRequest must deserialize")
    };
    let (_status, resp) = super::lifecycle::create_run(State(state.clone()), Json(req))
        .await
        .expect("create_run should succeed");
    let run_id = resp.0.run_id;

    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), session_rx.recv())
            .await
            .expect("test must observe run_started within 10s")
            .expect("session sender must not close before run_started");
        if event.event_type == "run_started" {
            break;
        }
    }

    // The run has already resolved and committed its config. PATCH now.
    patch_server_default(
        state,
        serde_json::json!({ "model": "moonshotai/kimi-k2.5" }),
    )
    .await;

    let snapshot = state
        .run_manager
        .get_run(run_id)
        .expect("run must exist")
        .resolved_config()
        .expect("resolved_config must be populated")
        .clone();
    assert_eq!(
        snapshot.model, "z-ai/glm-5.2",
        "the already-running run must keep the pair it resolved at start"
    );
    assert_eq!(
        state.llm.read().default_model(),
        "moonshotai/kimi-k2.5",
        "…while the live client HAS moved — otherwise the assertion above \
         would pass simply because the PATCH never landed"
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

    // A non-DM shared session (agent_id = nil). This test originally used
    // a `dm:` session as its shared-session vehicle, but POST /runs on DM
    // sessions is rejected since #1156 (Option C — DM sessions are
    // agent-to-agent only). The behaviour under test — per-agent config
    // resolution via the request's `agent_id` on a shared session — is
    // independent of the context flavour.
    let session_id = SessionId::new();
    let session = state
        .session_manager
        .get_or_create_shared(session_id, "shared:config-resolution-test");

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
    let resolved = crate::configuration::resolve_agent_config(
        runs[0].agent_id,
        &state.session_manager,
        &base_agent_config,
        &state.llm.read().clone(),
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
/// openrouter, default_model: z-ai/glm-5.2, providers: empty).
/// Agent record carries `provider: Some("anthropic")` and `model: None`,
/// and there is no `[llm.providers.anthropic]` entry to supply a model.
/// This is the canonical #863 leak shape — pre-fix the agent loop would
/// send Anthropic the OpenRouter server default; pre-#863 it would
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
    let _ = state.run_manager.insert_run(run.clone());

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
            dm_ended_peer: None,
        },
    )
    .await;

    // 1. Terminal status is Failed — the failure arm fired.
    let final_run = state
        .run_manager
        .get_run(run_id)
        .expect("run must still exist after execute_run returns");
    assert_eq!(
        final_run.status(),
        RunStatus::Failed,
        "run must reach Failed via the resolve_outcome failure arm; got {:?}",
        final_run.status(),
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
    let _ = state.run_manager.insert_run(running_run);
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

#[tokio::test]
async fn run_panic_is_reconciled_to_failed_and_cleans_activity_state() {
    let (state, _shutdown, _completion_rx, _trigger_rx, _dm_rx) = test_app_state();
    let agent_id = AgentId::new();
    let session_id = SessionId::new();
    let run = Run::new(session_id, agent_id, "panic".to_string());
    let run_id = run.run_id;
    let cancel_token = CancellationToken::new();
    let _ = state.run_manager.insert_run(run);
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    let mut activity = subscribe_agent(&state, agent_id);

    super::lifecycle::execute_run_guarded_future(
        state.clone(),
        super::RunParams {
            run_id,
            session_id,
            agent_id,
            input: "panic".to_string(),
            context_id: "web".to_string(),
            cancel_token,
            is_peer_message: false,
            is_system_triggered: false,
            input_pre_persisted: false,
            dm_ended_peer: None,
        },
        async { panic!("synthetic queued work panic") },
    )
    .await;

    let failed = state.run_manager.get_run(run_id).expect("run retained");
    assert_eq!(failed.status(), RunStatus::Failed);
    assert_eq!(
        failed.error.as_deref(),
        Some("Run panicked during execution")
    );
    assert!(
        !state.run_manager.cancel_run(run_id),
        "panic reconciliation must remove the cancellation token"
    );
    assert!(!state.run_manager.has_active_runs(session_id));

    let ended = activity.try_recv().expect("activity-ended event");
    assert_eq!(ended.event_type, "session_activity_ended");
    assert_eq!(
        ended.data["run_id"].as_str(),
        Some(run_id.0.to_string().as_str())
    );
}

#[tokio::test]
async fn late_cleanup_panic_does_not_reclassify_a_completed_run_as_failed() {
    let (state, _shutdown, _completion_rx, _trigger_rx, _dm_rx) = test_app_state();
    let agent_id = AgentId::new();
    let session_id = SessionId::new();
    let run = Run::new(session_id, agent_id, "complete".to_string());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run);
    state.run_manager.mark_run_as_running(run_id);
    assert!(state.run_manager.mark_run_as_completed(
        run_id,
        "done".to_string(),
        Default::default()
    ));

    super::lifecycle::execute_run_guarded_future(
        state.clone(),
        super::RunParams {
            run_id,
            session_id,
            agent_id,
            input: "complete".to_string(),
            context_id: "web".to_string(),
            cancel_token: CancellationToken::new(),
            is_peer_message: false,
            is_system_triggered: false,
            input_pre_persisted: false,
            dm_ended_peer: None,
        },
        async { panic!("synthetic late cleanup panic") },
    )
    .await;

    let completed = state.run_manager.get_run(run_id).expect("run retained");
    assert_eq!(completed.status(), RunStatus::Completed);
    assert_eq!(completed.output.as_deref(), Some("done"));
    assert!(completed.error.is_none());
}

#[tokio::test]
async fn full_agent_queue_rejects_before_run_side_effects() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state.session_manager.get_or_create(agent_id, "web");
    let session_id = session.id;
    let mut events = subscribe_session(&state, session_id);

    let held: Vec<_> = (0..crate::session_queue::MAX_PENDING_PER_KEY)
        .map(|_| {
            state
                .agent_queue
                .try_reserve(agent_id)
                .expect("fill per-agent capacity")
        })
        .collect();
    let messages_before = state
        .session_manager
        .get_history(session_id)
        .expect("session history")
        .len();

    let request = CreateRunRequest {
        session_id,
        agent_id: None,
        input: RunInput::Text {
            text: "must be rejected cleanly".into(),
        },
    };
    let Err((status, body)) =
        super::lifecycle::create_run(State(state.clone()), Json(request)).await
    else {
        panic!("saturated queue must reject the request");
    };

    assert_eq!(status, axum::http::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body.0["error_code"], "AGENT_QUEUE_FULL");
    assert!(state.run_manager.list_by_session(session_id, 10).is_empty());
    assert_eq!(
        state
            .session_manager
            .get_history(session_id)
            .expect("session history")
            .len(),
        messages_before
    );
    assert!(
        drain_events(&mut events)
            .iter()
            .all(|event| event.event_type != "run_created")
    );

    drop(held);
    shutdown_token.cancel();
}

#[test]
fn queue_admission_429_includes_retry_after_header() {
    let response = axum::response::IntoResponse::into_response(
        super::lifecycle::queue_admission_error(crate::session_queue::AdmissionError::PerKeyFull),
    );
    assert_eq!(
        response.headers().get(axum::http::header::RETRY_AFTER),
        Some(&axum::http::HeaderValue::from_static("1"))
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
        .get_or_create_with_id(session_id, bob_id, "dm:alice:bob")
        .unwrap();
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
            interrupted: true,
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
                ConversationEndReason::Errored {
                    message,
                    interrupted,
                } => {
                    assert_eq!(message, "LLM provider error");
                    assert!(
                        interrupted,
                        "the caller's `interrupted` classification must reach \
                         the trigger unchanged — it is what decides whether \
                         the peer's end costs a turn (#1258)"
                    );
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
            interrupted: true,
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
            interrupted: true,
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
    let _ = state.run_manager.insert_run(run);
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
        state.run_manager.get_run(run_id).unwrap().status(),
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
    let _ = state.run_manager.insert_run(run);
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
    let exit = super::dm_lifecycle::handle_dm_run_completion(
        super::dm_lifecycle::DmRunCompletionContext {
            state: &state,
            run_id,
            session_id: dm_session_id,
            agent_id: bob_id,
            agent_name: Some("bob"),
            context_id: dm_context,
            is_peer_message: true,
            tool_calls: &ignore_records,
            response: "",
            reasoning: None,
        },
    )
    .await;
    assert_eq!(
        exit,
        super::dm_lifecycle::DmRunExit::Ended,
        "handle_dm_run_completion must take the Ended exit for a peer-DM \
         run that called ignore_message"
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
    let _ = state.run_manager.insert_run(run);
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
fn subscribe_agent(
    state: &AppState,
    agent_id: AgentId,
) -> crate::server::ManagedSubscription<AgentId> {
    state.run_manager.subscribe_agent(agent_id)
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
    let _ = state.run_manager.insert_run(run);
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
    let _ = state.run_manager.insert_run(run);
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

/// Subscribe to the GLOBAL cross-agent session-activity feed (#1211) —
/// mirrors what `stream_session_activity` (`GET /events/session-activity`)
/// registers, the feed the web UI sidebar subscribes to for the active-run
/// dot across every agent's sessions.
fn subscribe_activity(state: &AppState) -> crate::server::ManagedSubscription<()> {
    state.run_manager.subscribe_activity()
}

fn drain_activity_events(
    subscription: &mut crate::server::ManagedSubscription<()>,
) -> Vec<SseEventData> {
    let mut events = Vec::new();
    while let Ok(event) = subscription.try_recv() {
        events.push(event);
    }
    events
}

/// Drive a REAL run to completion through the full `execute_run` path
/// (mock LLM) so the `session_activity_started` / `_ended` lifecycle events
/// fire exactly as they do in production. Used by the #1211 activity-feed
/// regression tests.
async fn drive_activity_run(
    state: &AppState,
    agent_id: AgentId,
    session_id: SessionId,
    ctx: &str,
    input: &str,
) {
    let run = Run::new(session_id, agent_id, input.to_string());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());
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
            context_id: ctx.to_string(),
            cancel_token,
            is_peer_message: false,
            is_system_triggered: false,
            input_pre_persisted: false,
            dm_ended_peer: None,
        },
    )
    .await;
}

/// The set of `session_id`s a feed saw a `session_activity_started` for.
fn started_session_ids(events: &[SseEventData]) -> std::collections::HashSet<String> {
    events
        .iter()
        .filter(|e| e.event_type == "session_activity_started")
        .filter_map(|e| e.data.get("session_id").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
        .collect()
}

/// Regression pin for #1211 (root cause located by live repro): the sidebar
/// surfaces sessions owned by agents OTHER than the currently-active one
/// (the cross-agent Jobs / Direct-messages / Notifications sections), but
/// the per-agent feed (`GET /agents/{id}/events`) is scoped to a single
/// agent — so a run on another agent's session never reached the active
/// agent's feed and its active-run dot never lit unless the row was
/// selected.
///
/// This drives REAL runs (mock LLM, full `execute_run`) on two agents and
/// asserts:
///
/// - The **global** activity feed (`/events/session-activity`) receives
///   `session_activity_started` for runs on BOTH agents' sessions — the
///   delivery the sidebar needs.
/// - The **per-agent** feed for agent A receives ONLY agent A's activity —
///   the (intentional, unchanged) per-agent scoping that made the global
///   feed necessary in the first place. Agent B's activity is exactly what
///   the pre-fix live repro observed missing from A's feed.
#[tokio::test]
async fn cross_agent_activity_reaches_global_feed_not_per_agent_feed() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();

    let agent_a = AgentId::new();
    let agent_b = AgentId::new();

    let sess_a = state
        .session_manager
        .get_or_create(agent_a, "chat-a-other")
        .id;
    let sess_b = state.session_manager.get_or_create(agent_b, "job_b").id;

    // The active agent A's per-agent feed (what the sidebar subscribed to
    // pre-#1211) and the global cross-agent feed (post-#1211).
    let mut feed_a = subscribe_agent(&state, agent_a);
    let mut feed_global = subscribe_activity(&state);

    // Run on agent A's OWN session, then on agent B's session.
    drive_activity_run(&state, agent_a, sess_a, "chat-a-other", "hi from A other").await;
    drive_activity_run(&state, agent_b, sess_b, "job_b", "hi from B job").await;

    let global_started = started_session_ids(&drain_activity_events(&mut feed_global));
    let per_agent_a_started = started_session_ids(&drain_events(&mut feed_a));

    // The global feed carries BOTH agents' activity — this is what lets the
    // sidebar light the dot on a cross-agent (agent B) session.
    assert!(
        global_started.contains(&sess_a.0.to_string()),
        "global activity feed must carry agent A's session activity; saw {global_started:?}"
    );
    assert!(
        global_started.contains(&sess_b.0.to_string()),
        "global activity feed must carry agent B's (cross-agent) session activity — \
         this is the #1211 delivery the per-agent feed could not provide; saw {global_started:?}"
    );

    // The per-agent feed stays scoped: A sees its own, never B's. (B's
    // absence here is exactly the pre-fix repro: the sidebar, subscribed
    // only to A's feed, never learned about B's active run.)
    assert!(
        per_agent_a_started.contains(&sess_a.0.to_string()),
        "agent A's per-agent feed must carry its own activity; saw {per_agent_a_started:?}"
    );
    assert!(
        !per_agent_a_started.contains(&sess_b.0.to_string()),
        "agent A's per-agent feed must NOT carry agent B's activity (per-agent scoping is \
         intentional and unchanged); saw {per_agent_a_started:?}"
    );

    shutdown_token.cancel();
}

/// Isolation regression for the #1220 review (Codex): the global
/// session-activity feed must live in its own namespace so NO agent id can
/// collide with it — not even the `acacacac-…` value an earlier draft used
/// as a shared `agent_senders` key. `ALMS_AGENT_ID` / the sidecar / the
/// registry all accept an arbitrary UUID, so an operator could name an agent
/// exactly that. This drives real runs with an agent whose id IS that value
/// and asserts the two failure modes a shared namespace would have caused:
///
/// - **No leak:** the colliding-id agent's own `/agents/{id}/events` feed
///   must NOT receive another agent's `session_activity_*` (the per-agent
///   isolation boundary holds even for this id).
/// - **No skipped mirror:** the colliding-id agent's OWN activity must still
///   reach the global feed (the mirror is unconditional — no `agent_id`
///   guard that a colliding id could trip).
#[tokio::test]
async fn global_activity_feed_isolated_from_agent_with_colliding_id() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();

    // The exact value an earlier draft used as the shared activity-feed key.
    // An operator can legitimately set an agent to this id.
    let colliding = AgentId(uuid::Uuid::from_bytes([0xAC; 16]));
    let other = AgentId::new();

    let sess_colliding = state
        .session_manager
        .get_or_create(colliding, "chat-collide")
        .id;
    let sess_other = state.session_manager.get_or_create(other, "job-other").id;

    // The colliding-id agent's own per-agent feed + the global feed.
    let mut feed_colliding = subscribe_agent(&state, colliding);
    let mut feed_global = subscribe_activity(&state);

    // A run on a DIFFERENT agent's session.
    drive_activity_run(&state, other, sess_other, "job-other", "hi from other").await;

    let colliding_pa = started_session_ids(&drain_events(&mut feed_colliding));
    let global_after_other = started_session_ids(&drain_activity_events(&mut feed_global));

    // No leak: the other agent's activity must NOT land on the colliding-id
    // agent's per-agent feed (pre-fix, a shared namespace would have leaked
    // EVERY agent's activity here).
    assert!(
        !colliding_pa.contains(&sess_other.0.to_string()),
        "per-agent isolation must hold even for the colliding id — the other agent's \
         activity must NOT reach it; saw {colliding_pa:?}"
    );
    // The global feed does carry it (sanity — it's cross-agent).
    assert!(
        global_after_other.contains(&sess_other.0.to_string()),
        "global feed must carry the other agent's activity; saw {global_after_other:?}"
    );

    // Now a run on the COLLIDING-id agent's OWN session.
    drive_activity_run(
        &state,
        colliding,
        sess_colliding,
        "chat-collide",
        "hi from collide",
    )
    .await;

    let global_after_collide = started_session_ids(&drain_activity_events(&mut feed_global));
    let colliding_pa_own = started_session_ids(&drain_events(&mut feed_colliding));

    // No skipped mirror: the colliding-id agent's OWN activity must still
    // reach the global feed (pre-fix, an `agent_id == ACTIVITY_FEED_KEY`
    // guard would have skipped it).
    assert!(
        global_after_collide.contains(&sess_colliding.0.to_string()),
        "the colliding-id agent's own activity must still mirror to the global feed \
         (the mirror is unconditional); saw {global_after_collide:?}"
    );
    // And it does reach its own per-agent feed (ordinary behaviour).
    assert!(
        colliding_pa_own.contains(&sess_colliding.0.to_string()),
        "the colliding-id agent's own per-agent feed must carry its own activity; \
         saw {colliding_pa_own:?}"
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
    let _ = state.run_manager.insert_run(run.clone());

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
            dm_ended_peer: None,
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
    let _ = state.run_manager.insert_run(run.clone());

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
                dm_ended_peer: None,
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
        run_snapshot.status(),
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
/// `run.status()`, which differs: pre-fix the run is still `Queued` at
/// broadcast time; post-fix it is already `Running`.
///
/// **Interposer pattern:** spawn `execute_run`, subscribe a session
/// sender, and probe `run.status()` synchronously the moment
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
    let _ = state.run_manager.insert_run(run.clone());

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
                dm_ended_peer: None,
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
            // We probe `run.status()` rather than `has_active_runs` because
            // both `Queued` and `Running` count as active, so the latter
            // does not distinguish pre-fix (`Queued` at broadcast) from
            // post-fix (`Running` at broadcast).
            probed_status = state.run_manager.get_run(run_id).map(|r| r.status());
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
        "run.status() must be Running at the moment run_started is observed \
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
    let _ = state.run_manager.insert_run(run);
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
        run_snapshot.status(),
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
    let _ = state.run_manager.insert_run(run.clone());

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
                dm_ended_peer: None,
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
    let _ = state.run_manager.insert_run(run);
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
        run_snapshot.status(),
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
    let _ = state.run_manager.insert_run(run);
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
        run_snapshot.status(),
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
    let _ = state.run_manager.insert_run(run.clone());

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
            dm_ended_peer: None,
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
        final_run.status(),
        RunStatus::Failed,
        "run must reach Failed status when the LLM is unreachable; got {:?} (error={:?})",
        final_run.status(),
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
            stream_epoch: None,
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
            stream_epoch: None,
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
    let _ = state.run_manager.insert_run(running_run);
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
    let _ = state.run_manager.insert_run(a);
    // Sleep enough for `created_at` to differ deterministically.
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let b = Run::new(session_id, agent_id, "B".into());
    let b_id = b.run_id;
    let _ = state.run_manager.insert_run(b);
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let c = Run::new(session_id, agent_id, "C".into());
    let c_id = c.run_id;
    let _ = state.run_manager.insert_run(c);

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
            dm_ended_peer: None,
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
    let _ = state.run_manager.insert_run(running);
    state.run_manager.mark_run_as_running(running_id);

    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let q1 = Run::new(session_id, agent_id, "q1".into());
    let q1_id = q1.run_id;
    let _ = state.run_manager.insert_run(q1);
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let q2 = Run::new(session_id, agent_id, "q2".into());
    let q2_id = q2.run_id;
    let _ = state.run_manager.insert_run(q2);

    // Queued #1 should be position 1 (next up — one Running ahead).
    let resp_q1 = super::read_api::get_run_status(State(state.clone()), Path(q1_id))
        .await
        .expect("get_run_status should succeed for q1");
    assert_eq!(resp_q1.0.queue_position, Some(1));
    assert_eq!(resp_q1.0.status, RunStatus::Queued);

    // Queued #2 should be position 2.
    let resp_q2 = super::read_api::get_run_status(State(state.clone()), Path(q2_id))
        .await
        .expect("get_run_status should succeed for q2");
    assert_eq!(resp_q2.0.queue_position, Some(2));

    // Running run has no queue_position.
    let resp_running = super::read_api::get_run_status(State(state.clone()), Path(running_id))
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
    let _ = state.run_manager.insert_run(run);

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
            dm_ended_peer: None,
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
    let _ = state.run_manager.insert_run(run.clone());

    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    let mut activity_feed = subscribe_activity(&state);

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
            dm_ended_peer: None,
        },
    )
    .await;
    // PR #1220 review regression: the budget-rejection arm must emit its
    // terminal activity after flipping Queued -> Failed. Otherwise the
    // authoritative snapshot still sees this run as active and leaves the
    // sidebar dot stuck.
    let activity_events = drain_activity_events(&mut activity_feed);
    let run_id_text = run_id.0.to_string();
    let ended = activity_events
        .iter()
        .find(|event| {
            event.event_type == "session_activity_ended"
                && event.data.get("run_id").and_then(|value| value.as_str())
                    == Some(run_id_text.as_str())
        })
        .expect("budget-rejected run must publish terminal session activity");
    assert_eq!(
        ended
            .data
            .get("has_active_run")
            .and_then(|value| value.as_bool()),
        Some(false),
        "budget-rejected run terminal activity must carry the settled false predicate",
    );

    // 1. Terminal status is Failed — the budget arm fired before any LLM
    //    call. NOT Cancelled (would mean the cancel-token early-exit fired
    //    instead) and NOT Completed (would mean the guard didn't trip).
    let final_run = state
        .run_manager
        .get_run(run_id)
        .expect("run must still exist after execute_run returns");
    assert_eq!(
        final_run.status(),
        RunStatus::Failed,
        "run must reach Failed via the budget arm; got {:?}",
        final_run.status(),
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
        final_run.resolved_config().is_none(),
        "Failed-before-running runs must not have a resolved_config snapshot; got {:?}",
        final_run.resolved_config(),
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
    let _ = state.run_manager.insert_run(run.clone());

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
            dm_ended_peer: None,
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
    use chrono::Utc;

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
    let now = Utc::now();
    let agent = AgentRecord {
        id: agent_id,
        name: "notify-debug-gate-agent".into(),
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
        debug_mode,
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

    super::lifecycle::execute_run(
        state.clone(),
        super::RunParams {
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
    let _ = state.run_manager.insert_run(run);
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
        run_after.status(),
        RunStatus::Cancelled,
        "run.status() MUST be Cancelled immediately after `cancel_run` \
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

#[tokio::test]
async fn http_cancel_persists_before_firing_the_cancellation_token() {
    use axum::extract::{Path, State};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-cancel-persistence-order");
    let run = Run::new(session.id, agent_id, "test".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run);
    assert!(state.run_manager.mark_run_as_running(run_id));
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    let mut session_rx = subscribe_session(&state, session.id);
    state.run_manager.inject_next_persistence_failure();

    let Err((status, body)) =
        super::lifecycle::cancel_run(State(state.clone()), Path(run_id)).await
    else {
        panic!("durable cancellation failure must fail the HTTP request");
    };

    assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body.0["error"]["code"], "LIFECYCLE_PERSISTENCE_FAILED");
    assert!(
        cancel_token.is_cancelled(),
        "the quarantined worker must be stopped after the durable attempt fails"
    );
    let quarantined = state.run_manager.get_run(run_id).unwrap();
    assert_eq!(quarantined.status(), RunStatus::Failed);
    assert_eq!(quarantined.terminal_reason(), Some("persistence_failed"));
    let peer_error = super::lifecycle::lifecycle_persistence_error_for_peer(&state, run_id, None)
        .expect("the quarantined terminal reason must retain persistence provenance");
    assert_eq!(peer_error, "Runtime error");
    assert!(!peer_error.contains("injected run persistence failure"));
    assert!(
        drain_events(&mut session_rx)
            .iter()
            .any(|event| event.event_type == "run_error"),
        "the persistence failure must be published immediately"
    );
    assert!(
        state
            .run_manager
            .activity_events_from(0)
            .await
            .iter()
            .any(|event| event.event_type == "session_activity_ended"),
        "the activity boundary must close immediately"
    );
    shutdown_token.cancel();
}

/// #1254 regression — when the HTTP `cancel_run` handler wins the race and
/// `execute_run`'s terminal arm consequently takes its "already terminal"
/// skip branch, the session stream must STILL receive the terminal event.
///
/// The skip line in the reported log reads like a dropped event, which is
/// what made this look like a defect. It is not: `cancel_run` OWNS the
/// broadcast when it wins, and the terminal arm stays silent precisely so
/// the event is not duplicated. This test pins that ownership so the two
/// sides can never both defer to each other.
///
/// Which skip branch, precisely: the terminal-transition barrier sits
/// before `match result`, so the mock-LLM loop has already returned
/// `Ok(_)` by the time the cancel lands and the token can no longer steer
/// it into a `Cancelled` arm. What runs is the **completed arm's**
/// already-terminal gate ("Run {} was already terminal when its loop
/// returned Ok"), which is structurally identical to the four
/// `Cancelled`/`Failed`/`Err` gates below it: same `marked_*` transition
/// bool, same silence, same reliance on the winner having broadcast. That
/// the run finishes with `run_cancelled` and no `run_finished` is exactly
/// this gate suppressing itself. The reported production line came from
/// the `CancelledWithToolCalls` sibling; that arm needs a hanging LLM to
/// reach and is not what this test drives.
///
/// Deterministic by construction — the barrier parks `execute_run`
/// immediately before its terminal transition, so the cancel is guaranteed
/// to land first rather than relying on timing.
///
/// Asserts the BROADCAST, not the persisted status: the status was already
/// correct in production and is exactly what hid the reported symptom.
#[tokio::test]
async fn http_cancel_emits_terminal_sse_even_when_the_terminal_arm_skips() {
    use axum::extract::{Path, State};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-1254-cancel-broadcast");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "complete in mock mode".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    // Subscribe before anything runs so this is a live-delivery assertion.
    let mut session_rx = subscribe_session(&state, session_id);
    let barrier = super::lifecycle::install_terminal_transition_barrier(run_id);

    let exec_state = state.clone();
    let handle = tokio::spawn(async move {
        super::lifecycle::execute_run(
            exec_state,
            super::RunParams {
                run_id,
                session_id,
                agent_id,
                input: run.input,
                context_id: "test-1254-cancel-broadcast".to_string(),
                cancel_token,
                is_peer_message: false,
                is_system_triggered: false,
                input_pre_persisted: false,
                dm_ended_peer: None,
            },
        )
        .await;
    });

    // `execute_run` is now parked immediately before its terminal transition.
    barrier.wait().await;

    // The HTTP cancel wins the race: it flips the run terminal and owns the
    // terminal broadcast.
    let _ = super::lifecycle::cancel_run(State(state.clone()), Path(run_id))
        .await
        .expect("cancel_run should succeed for a Running run");

    // Release `execute_run`. Its completed arm now finds the state already
    // terminal and takes the already-terminal skip branch.
    barrier.wait().await;
    tokio::time::timeout(std::time::Duration::from_secs(10), handle)
        .await
        .expect("execute_run must leave the terminal transition")
        .expect("execute_run task should not panic");

    let events = drain_events(&mut session_rx);
    let terminal: Vec<&str> = events
        .iter()
        .map(|event| event.event_type.as_str())
        .filter(|kind| matches!(*kind, "run_cancelled" | "run_finished" | "run_error"))
        .collect();

    assert_eq!(
        terminal,
        vec!["run_cancelled"],
        "cancelling must put exactly one terminal event on the session \
         stream even though `execute_run`'s terminal arm skipped its own \
         broadcast (#1254); saw terminal events {terminal:?} out of {:?}",
        events
            .iter()
            .map(|event| event.event_type.as_str())
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
    let _ = state.run_manager.insert_run(run.clone());

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
                dm_ended_peer: None,
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
        final_run.status(),
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
    assert!(matches!(run.status(), RunStatus::Queued));
    assert!(
        run.mark_cancelled(),
        "Queued → Cancelled must report transition=true"
    );
    assert!(matches!(run.status(), RunStatus::Cancelled));
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
    assert!(matches!(failed_run.status(), RunStatus::Failed));
    assert!(
        !failed_run.mark_cancelled(),
        "mark_cancelled on a Failed run must report false (no transition)"
    );
    assert!(
        matches!(failed_run.status(), RunStatus::Failed),
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
    let _ = state.run_manager.insert_run(run);
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
        final_run.status(),
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
    let _ = state.run_manager.insert_run(run);

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

    let response = super::read_api::get_run_reasoning(State(state.clone()), Path(run_id))
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
    let response2 = super::read_api::get_run_reasoning(State(state.clone()), Path(run_id))
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
    let _ = state.run_manager.insert_run(run);

    let response = super::read_api::get_run_reasoning(State(state.clone()), Path(run_id))
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
    let _ = state.run_manager.insert_run(run_a);
    let run_b = Run::new(session_id, agent_id, "b".into());
    let run_b_id = run_b.run_id;
    let _ = state.run_manager.insert_run(run_b);

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

    let resp_a = super::read_api::get_run_reasoning(State(state.clone()), Path(run_a_id))
        .await
        .expect("get_run_reasoning should succeed for run A");
    let resp_b = super::read_api::get_run_reasoning(State(state.clone()), Path(run_b_id))
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
    let _ = state.run_manager.insert_run(run);

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

    let response = super::read_api::get_run_reasoning(State(state.clone()), Path(run_id))
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
    let _ = state.run_manager.insert_run(run);

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

    let response = super::read_api::get_run_reasoning(State(state.clone()), Path(run_id))
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
    let _ = state.run_manager.insert_run(run);

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

    let response = super::read_api::get_run_reasoning(State(state.clone()), Path(run_id))
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
    let _ = state.run_manager.insert_run(run_a);
    let run_b = Run::new(session_id, agent_id, "b".into());
    let run_b_id = run_b.run_id;
    let _ = state.run_manager.insert_run(run_b);

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

    let resp_b = super::read_api::get_run_reasoning(State(state.clone()), Path(run_b_id))
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
    let _ = state.run_manager.insert_run(run);

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

    let response = super::read_api::get_run_reasoning(State(state.clone()), Path(run_id))
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
// #1133 / A3-1 — get_run_reasoning terminal seal (`null` cursor + empty text
// + authoritative `terminal` flag)
// ---------------------------------------------------------------------------

/// A *live* run that has emitted post-boundary reasoning returns its
/// accumulated text, a non-null `last_event_id` cursor, AND `terminal: false`.
/// Pins that the terminal seal does NOT fire for a running run, so live
/// multi-turn streaming is unaffected.
#[tokio::test]
async fn get_run_reasoning_live_run_returns_text_cursor_and_terminal_false() {
    use axum::extract::{Path, State};
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-1133-reasoning-live");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "go think".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run);
    // Run is in flight — the production state while the LLM call streams.
    state.run_manager.mark_run_as_running(run_id);

    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "still ", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "thinking", None),
        )
        .await;

    let response = super::read_api::get_run_reasoning(State(state.clone()), Path(run_id))
        .await
        .expect("get_run_reasoning should succeed for a live run");
    let body = response.0;

    assert_eq!(
        body["text"].as_str().unwrap(),
        "still thinking",
        "a live run must still return its accumulated post-boundary reasoning"
    );
    assert!(
        body["last_event_id"].as_u64().is_some(),
        "a live run with reasoning must return a non-null last_event_id cursor"
    );
    assert_eq!(
        body["terminal"].as_bool(),
        Some(false),
        "a non-terminal (Running) run must report terminal: false so the \
         frontend keeps it live (no load-time dedupe, spinner preserved)"
    );
    assert!(
        body["seal_event_id"].is_null(),
        "a live run must report a null seal_event_id — the coverage anchor is \
         meaningful only for a terminal run; a live run is never added to the \
         frontend suppress-set"
    );

    shutdown_token.cancel();
}

/// The core #1133 fix on the natural-completion path: once a run is terminal
/// (`Completed`), `get_run_reasoning` seals it — empty `text`, `null`
/// `last_event_id`, `terminal: true` — *regardless* of the final-turn
/// reasoning still sitting in the non-ephemeral session event log. (Unlike
/// `get_run_text`, whose in-memory buffer is evicted on terminal transition,
/// `reasoning_delta` is durable and has no natural backstop, hence this
/// explicit seal.)
#[tokio::test]
async fn get_run_reasoning_terminal_completed_run_seals_to_null_cursor() {
    use axum::extract::{Path, State};
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-1133-reasoning-completed");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "think then finish".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run);
    state.run_manager.mark_run_as_running(run_id);

    // Final-turn reasoning lands in the durable session event log...
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "final ", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "answer", None),
        )
        .await;

    // ...and the run completes (exactly what execute_run's Ok arm does).
    let transitioned = state.run_manager.mark_run_as_completed(
        run_id,
        "done".to_string(),
        alms_core::TokenUsage::default(),
    );
    assert!(
        transitioned,
        "Running → Completed must transition (test fixture sanity check)"
    );

    let response = super::read_api::get_run_reasoning(State(state.clone()), Path(run_id))
        .await
        .expect("get_run_reasoning should succeed for a terminal run");
    let body = response.0;

    assert_eq!(
        body["text"].as_str().unwrap(),
        "",
        "a terminal run must blank the reasoning text — the sealed assistant \
         message already renders it, so re-seeding would double-render"
    );
    assert!(
        body["last_event_id"].is_null(),
        "a terminal run must return a null last_event_id so the client stays \
         at the messages-GET HWM and the terminal SSE event replays (else the \
         spinner sticks)"
    );
    assert_eq!(
        body["terminal"].as_bool(),
        Some(true),
        "a terminal run must report terminal: true — the authoritative signal \
         the frontend keys its dedupe / spinner-clear off (empty text alone is \
         overloaded with the live-but-no-reasoning case)"
    );
    // This fixture flips the run terminal in the store but never broadcasts
    // `run_finished`, so no terminal event is in the log and the seal anchor
    // is absent — which the frontend treats conservatively (do NOT suppress).
    assert!(
        body["seal_event_id"].is_null(),
        "with no terminal SSE event in the log, seal_event_id must be null"
    );

    shutdown_token.cancel();
}

/// #1133 Codex #3 / sub-race B — pins the ordering invariant that makes the
/// frontend coverage gate sound: `seal_event_id` (the terminal SSE event's id)
/// is strictly ABOVE every reasoning-delta id, so a messages-GET that resolved
/// before the seal (HWM == delta HWM) correctly fails the
/// `historyHWM >= seal_event_id` check (sub-race B → render once), while one
/// that resolved after it passes (sub-race A → suppress the duplicate). The
/// runtime guarantees the ordering by sealing the assistant message into
/// history and THEN flipping terminal + broadcasting the event (`execute_run`'s
/// Ok arm: `append_message` → `mark_run_as_completed` → `send_event`).
/// Deterministic and single-task — asserts the emitted field and id ordering,
/// not a two-task interleave.
#[tokio::test]
async fn get_run_reasoning_terminal_seal_event_id_is_above_delta_hwm() {
    use axum::extract::{Path, State};
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-1133-seal-event-id");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "think then finish".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run);
    state.run_manager.mark_run_as_running(run_id);

    // Final-turn reasoning streams into the durable session event log during
    // the agent loop.
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "final ", None),
        )
        .await;
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "answer", None),
        )
        .await;

    // Capture the session HWM at the moment a messages GET racing in sub-race
    // B would have sampled it: AFTER the deltas, but BEFORE the run goes
    // terminal and broadcasts `run_finished`. This is exactly the HWM the
    // frontend would carry as `historyHWM` if its step-2 messages GET resolved
    // here, before the runtime sealed the assistant message.
    let delta_hwm = state
        .run_manager
        .latest_session_event_id(session_id)
        .await
        .expect("session HWM must exist after the reasoning deltas");

    // The runtime seals the assistant message into history (a session-store
    // write, not modelled here), THEN flips terminal and broadcasts. Mirror
    // that ordering: state flip first, then the `run_finished` broadcast.
    assert!(state.run_manager.mark_run_as_completed(
        run_id,
        "done".to_string(),
        alms_core::TokenUsage::default(),
    ));
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::run_finished(run_id, true, alms_core::TokenUsage::default()),
        )
        .await;

    let response = super::read_api::get_run_reasoning(State(state.clone()), Path(run_id))
        .await
        .expect("get_run_reasoning should succeed for a terminal run");
    let body = response.0;

    // The seal anchor is exposed and equals the `run_finished` event id.
    let seal_event_id = body["seal_event_id"]
        .as_u64()
        .expect("seal_event_id must be present and numeric on a terminal run");

    // The load-time reasoning cursor stays null on terminal (no overshoot) —
    // seal_event_id is a SEPARATE field and must not un-null the cursor.
    assert!(
        body["last_event_id"].is_null(),
        "the reasoning cursor must stay null on terminal; seal_event_id is a \
         separate coverage anchor, not the cursor"
    );
    assert_eq!(body["terminal"].as_bool(), Some(true));

    // The load-bearing ordering invariant: the seal anchor is strictly above
    // the reasoning-delta HWM (see the assertion message for how the frontend
    // gate relies on it).
    assert!(
        seal_event_id > delta_hwm,
        "seal_event_id ({seal_event_id}) must be strictly greater than the \
         reasoning-delta HWM ({delta_hwm}) — this is what lets the frontend's \
         `historyHWM >= seal_event_id` gate distinguish a messages-GET that \
         resolved before the seal (sub-race B, render once) from one that \
         resolved after it (sub-race A, suppress the duplicate)"
    );

    shutdown_token.cancel();
}

/// The cancel-path variant of the seal. A trailing `reasoning_delta` that
/// races `run_cancelled` (logged *after* it here, mirroring the HTTP-cancel
/// drain window where a delta can be assigned an id above `run_cancelled`)
/// must NOT defeat the seal: once the run is `Cancelled`, the response is
/// `{ text: "", last_event_id: null, terminal: true }`.
///
/// NOTE (deterministic by design): this test does NOT assert any ordering
/// between the trailing delta's id and the `run_cancelled` id — that two-task
/// interleave cannot be driven deterministically (see the
/// `http_cancel_wins_against_natural_completion` comment), and atomic
/// `log_event` makes each event indivisible without ordering which task wins
/// the lock. The seal is robust precisely *because* it keys off the run's
/// terminal status, not the cursor.
#[tokio::test]
async fn get_run_reasoning_cancel_path_trailing_delta_still_seals() {
    use axum::extract::{Path, State};
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-1133-reasoning-cancel");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "cancel mid-think".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run);
    state.run_manager.mark_run_as_running(run_id);

    // HTTP cancel_run wins the race: flip to Cancelled + broadcast
    // run_cancelled synchronously (exactly what the cancel handler does).
    let cancelled = state.run_manager.mark_run_as_cancelled(run_id);
    assert!(
        cancelled,
        "Running → Cancelled must transition (test fixture sanity check)"
    );
    state
        .run_manager
        .send_event(run_id, session_id, SseEventData::run_cancelled(run_id))
        .await;

    // A trailing reasoning_delta from the still-draining forwarder lands in
    // the session event log AFTER run_cancelled — the exact id-race the
    // unwinnable cursor could not survive. The terminal seal handles it
    // without any id-ordering assumption.
    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::reasoning_delta(run_id, "trailing", None),
        )
        .await;

    let response = super::read_api::get_run_reasoning(State(state.clone()), Path(run_id))
        .await
        .expect("get_run_reasoning should succeed for a cancelled run");
    let body = response.0;

    assert_eq!(
        body["text"].as_str().unwrap(),
        "",
        "a cancelled run must blank reasoning text even with a trailing \
         post-cancel delta in the durable log"
    );
    assert!(
        body["last_event_id"].is_null(),
        "a cancelled run must return a null cursor so run_cancelled replays — \
         the trailing delta must not be able to drag the cursor above the \
         terminal event"
    );
    assert_eq!(
        body["terminal"].as_bool(),
        Some(true),
        "a cancelled run is terminal: true"
    );
    // The seal anchor is the `run_cancelled` event id, captured even though a
    // trailing reasoning_delta was logged after it — the trailing delta is not
    // a terminal-type event, so it never becomes the anchor.
    assert!(
        body["seal_event_id"].as_u64().is_some(),
        "a cancelled run must expose the run_cancelled event id as seal_event_id"
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
    let _ = state.run_manager.insert_run(run);

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

    let response = super::read_api::get_run_text(State(state.clone()), Path(run_id))
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
    let response2 = super::read_api::get_run_text(State(state.clone()), Path(run_id))
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
    let _ = state.run_manager.insert_run(run);

    let response = super::read_api::get_run_text(State(state.clone()), Path(run_id))
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
    let _ = state.run_manager.insert_run(run_a);
    let run_b = Run::new(session_id, agent_id, "b".into());
    let run_b_id = run_b.run_id;
    let _ = state.run_manager.insert_run(run_b);

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

    let resp_a = super::read_api::get_run_text(State(state.clone()), Path(run_a_id))
        .await
        .expect("get_run_text should succeed for run A");
    let resp_b = super::read_api::get_run_text(State(state.clone()), Path(run_b_id))
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
    let _ = state.run_manager.insert_run(run);

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

    let response = super::read_api::get_run_text(State(state.clone()), Path(run_id))
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
    let _ = state.run_manager.insert_run(run);

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

    let response = super::read_api::get_run_text(State(state.clone()), Path(run_id))
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
    let _ = state.run_manager.insert_run(run);

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

    let response = super::read_api::get_run_text(State(state.clone()), Path(run_id))
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
    let _ = state.run_manager.insert_run(run);

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

    let response = super::read_api::get_run_text(State(state.clone()), Path(run_id))
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
    let result = super::read_api::get_run_text(State(state.clone()), Path(unknown_run)).await;
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
    let _ = state.run_manager.insert_run(run);

    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::token_delta(run_id, "Hello", None),
        )
        .await;

    // Sanity-check: buffer is populated mid-run.
    let response = super::read_api::get_run_text(State(state.clone()), Path(run_id))
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

    let response = super::read_api::get_run_text(State(state.clone()), Path(run_id))
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

/// A notification run must not reach the LLM when its synthetic input cannot
/// be committed. Otherwise the assistant reply can survive in the run record
/// while the hidden user turn that triggered it disappears after restart.
#[tokio::test]
async fn notification_input_persistence_failure_fails_closed_before_llm() {
    let directory = tempfile::tempdir().unwrap();
    let db_path = directory.path().join("notification-persistence-failure.db");
    let db_path = db_path.to_string_lossy().into_owned();
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm_at(&db_path);
    let agent_id = AgentId::new();
    let context_id = "notification-persistence-failure";
    let session = state.session_manager.get_or_create(agent_id, context_id);
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "deliver this notification".into());
    let run_id = run.run_id;
    state
        .run_manager
        .insert_run(run.clone())
        .expect("queued run should be persisted before the failure is injected");

    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    let mut session_events = subscribe_session(&state, session_id);

    // Delete only the SQLite row through the store, deliberately leaving the
    // SessionManager's in-memory projection intact. The next message INSERT
    // now fails deterministically on the session foreign key while normal
    // in-memory reads still succeed — exactly the split-brain shape that
    // append_message used to hide by logging and returning Ok.
    let store = state
        .session_manager
        .store()
        .expect("SQLite-backed test state must expose its store")
        .clone();
    store
        .delete_session(session_id)
        .expect("durable session deletion should inject the FK failure");

    super::lifecycle::execute_run(
        state.clone(),
        super::RunParams {
            run_id,
            session_id,
            agent_id,
            input: run.input,
            context_id: context_id.to_string(),
            cancel_token,
            is_peer_message: false,
            is_system_triggered: true,
            input_pre_persisted: false,
            dm_ended_peer: None,
        },
    )
    .await;

    let events = drain_events(&mut session_events);
    let event_types: Vec<&str> = events
        .iter()
        .map(|event| event.event_type.as_str())
        .collect();
    assert!(
        event_types.contains(&"run_error"),
        "persistence failure must emit run_error; got {event_types:?}"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event.event_type.as_str(),
            "run_finished" | "token_delta" | "reasoning_delta"
        )),
        "failed notification must not emit completion or reply events; got {event_types:?}"
    );

    // The mock client has no invocation counter. These are the existing,
    // non-invasive runtime boundaries: `building_context` is emitted as
    // `run_on_session` begins, and `calling_llm` immediately precedes the
    // client call. `execute_run` awaits the event forwarder before returning,
    // so their absence proves this failure stopped before runtime execution.
    let runtime_phases: Vec<&str> = events
        .iter()
        .filter(|event| event.event_type == "status")
        .filter_map(|event| event.data.get("phase")?.as_str())
        .collect();
    assert!(
        !runtime_phases
            .iter()
            .any(|phase| matches!(*phase, "building_context" | "calling_llm")),
        "notification persistence failure must stop before the runtime/LLM boundary; \
         got status phases {runtime_phases:?}"
    );

    let failed = state
        .run_manager
        .get_run(run_id)
        .expect("run should remain queryable");
    assert_eq!(
        failed.status(),
        RunStatus::Failed,
        "notification persistence failure must stop execution"
    );
    assert!(
        failed.output.is_none(),
        "failed notification must not retain mock LLM output"
    );
    assert!(
        failed
            .error
            .as_deref()
            .is_some_and(|error| error.contains("SQLite save_message")),
        "run should expose the durable input failure, got {:?}",
        failed.error
    );

    let history = state.session_manager.get_history(session_id).unwrap();
    assert!(
        history.iter().all(|message| {
            !message
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("notification_input"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        }),
        "failed notification input must not be published into memory"
    );
    assert!(
        history
            .iter()
            .all(|message| message.role != alms_session::Role::Assistant),
        "failed notification must not publish an assistant reply into memory"
    );
    assert!(
        store.load_messages(session_id).unwrap().is_empty(),
        "failed notification input must not exist in SQLite"
    );

    // Reopen through a distinct connection instead of trusting the original
    // managers' in-memory projections. This is the state a restart can see.
    let reopened_store = alms_session::SqliteStore::open(&db_path)
        .expect("notification regression database should reopen");
    let reopened_run = reopened_store
        .load_run(run_id)
        .expect("reopened run query should succeed")
        .expect("failed run must remain durable after reopen");
    assert_eq!(reopened_run.status(), RunStatus::Failed);
    assert!(reopened_run.output.is_none());
    assert!(
        reopened_run
            .error
            .as_deref()
            .is_some_and(|error| error.contains("SQLite save_message")),
        "reopened run should retain the durable input failure, got {:?}",
        reopened_run.error
    );
    assert!(
        reopened_store
            .load_messages(session_id)
            .expect("reopened message query should succeed")
            .is_empty(),
        "failed notification input and assistant reply must remain absent after reopen"
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

// ---------------------------------------------------------------------------
// POST /sessions/{id}/subagent/cancel — session-keyed subagent cancel
// ---------------------------------------------------------------------------

/// The 404 leg over the PRODUCTION router: a session with no live subagent
/// must return 404 with the `NO_LIVE_SUBAGENT` error code. Exercising this
/// through `protected_router()` (rather than calling the handler directly)
/// pins the route registration — `/sessions/{session_id}/subagent/cancel`
/// wired to `cancel_subagent` — so the frontend's `cancelSubagent(sessionId)`
/// helper can't silently 404-on-every-request due to a routing drift.
#[tokio::test]
async fn cancel_subagent_endpoint_404_when_no_live_subagent() {
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();

    let router = crate::server::routes::protected_router().with_state(state);
    let uri = format!("/sessions/{}/subagent/cancel", SessionId::new().0);
    let response = router
        .oneshot(HttpRequest::post(&uri).body(Body::empty()).unwrap())
        .await
        .expect("router oneshot should not fail");

    assert_eq!(
        response.status(),
        axum::http::StatusCode::NOT_FOUND,
        "a session with no live subagent must 404"
    );
    let body_bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read response body");
    let json: serde_json::Value =
        serde_json::from_slice(&body_bytes).expect("error body should be JSON");
    assert_eq!(
        json["error"]["code"], "NO_LIVE_SUBAGENT",
        "error code must be NO_LIVE_SUBAGENT; got {json}"
    );

    shutdown_token.cancel();
}

/// The 200 leg: cancelling a LIVE subagent through the handler fires its
/// cancellation token and the subagent actually terminates `Cancelled`.
///
/// Determinism note: `#[tokio::test]` uses the current-thread runtime, and
/// the handler body has no await points before the (synchronous)
/// `cancel_subagent_by_session` call — so the `tokio::spawn`ed
/// `run_subagent` task cannot run to completion between `spawn_subagent`
/// returning and the cancel firing. The handle is still live (Pending) at
/// cancel time, every time. The subagent then observes the already-fired
/// token at its first cancellation checkpoint and lands `Cancelled`, which
/// the awaited `TaskResult` asserts end-to-end.
#[tokio::test]
async fn cancel_subagent_endpoint_cancels_live_subagent() {
    use axum::extract::{Path as AxumPath, State as AxumState};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();

    // Spawn a background subagent directly on the coordinator (the same
    // object the endpoint reaches through `state.coordinator`). Unnamed, so
    // no registry entry is needed.
    let request = alms_coordinator::SubagentRequest {
        task: "long-running background research".to_string(),
        parent_session: SessionId::new(),
        parent_agent_id: AgentId::new(),
        parent_run_id: None,
        subagent_name: None,
        parent_tool_invocation_id: None,
    };
    let (task_id, sub_session_id) = state
        .coordinator
        .spawn_subagent(request, None, true, None)
        .await
        .expect("spawn_subagent should succeed");
    let result_rx = state
        .coordinator
        .take_result_rx(task_id)
        .expect("result receiver should be available");

    // Cancel through the HTTP handler (session-keyed).
    let response =
        super::lifecycle::cancel_subagent(AxumState(state.clone()), AxumPath(sub_session_id))
            .await
            .expect("cancel of a live subagent must return 200");
    assert_eq!(
        response.0["status"], "cancelling",
        "the 200 body must report status=cancelling"
    );

    // The subagent must actually terminate Cancelled (not Completed): the
    // token was fired before its task ever ran, so its first checkpoint
    // takes the cancellation arm.
    let task_result = result_rx.await.expect("should receive a task result");
    assert_eq!(
        task_result.status,
        TaskStatus::Cancelled,
        "the cancelled subagent must land TaskStatus::Cancelled; got {:?}",
        task_result.status
    );

    // Idempotence / double-click: the subagent is now terminal, so a second
    // cancel must report 404 (no live subagent), not 200.
    let second =
        super::lifecycle::cancel_subagent(AxumState(state.clone()), AxumPath(sub_session_id)).await;
    assert!(
        second.is_err(),
        "a second cancel after termination must be an error (404)"
    );
    assert_eq!(
        second.unwrap_err().0,
        axum::http::StatusCode::NOT_FOUND,
        "the second cancel must be a 404"
    );

    shutdown_token.cancel();
}

/// #1254 regression — cancelling a parent must put a terminal SSE event on
/// the in-flight subagent's OWN session, not merely a `cancelled` row in the
/// database.
///
/// This is the acceptance case the issue flagged as uncovered: the parent's
/// cancellation token propagates to a FOREGROUND subagent (in the reported
/// session, subagent `d19b6d62` went terminal 1ms after its parent), and the
/// fullscreen subagent-session view has to learn about it from the SSE feed.
///
/// The assertion is deliberately on the BROADCAST, not on `TaskStatus` —
/// the persisted status was already correct in production, and that is
/// precisely what hid the reported symptom for so long.
#[tokio::test]
async fn cancelled_subagent_emits_terminal_sse_on_its_own_session() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();

    // The parent's cancellation token — the same object the HTTP
    // `cancel_run` handler fires via `RunManager::cancel_run`.
    let parent_cancel_token = CancellationToken::new();

    let request = alms_coordinator::SubagentRequest {
        task: "long-running foreground research".to_string(),
        parent_session: SessionId::new(),
        parent_agent_id: AgentId::new(),
        parent_run_id: None,
        subagent_name: None,
        parent_tool_invocation_id: None,
    };
    // `is_background = false` — the foreground `invoke_agent` shape.
    let (task_id, sub_session_id) = state
        .coordinator
        .spawn_subagent(request, None, false, Some(parent_cancel_token.clone()))
        .await
        .expect("spawn_subagent should succeed");
    let result_rx = state
        .coordinator
        .take_result_rx(task_id)
        .expect("result receiver should be available");

    // Subscribe BEFORE the cancel so this is a live-delivery assertion and
    // not just a replay of the persisted session log.
    let mut session_subscription = state.run_manager.subscribe_session(sub_session_id);

    // Cancel the PARENT. Propagation through the child token is what drives
    // the subagent terminal — nothing cancels the subagent directly.
    parent_cancel_token.cancel();

    let task_result = result_rx.await.expect("should receive a task result");
    assert_eq!(
        task_result.status,
        TaskStatus::Cancelled,
        "the subagent must land Cancelled via parent-token propagation; got {:?}",
        task_result.status
    );

    // Await the terminal ON THE SUBSCRIPTION, not on `session_events_from`.
    // The self-sink's drain task orders the terminal event after all of the
    // subagent's buffered content, so it lands asynchronously — `result_rx`
    // resolving does not mean it has already been broadcast.
    //
    // Reading the persisted log instead would be the wrong surface: #1254's
    // symptom was "the client never received it", and `send_event` writes
    // the log and fans out in the same call, so a log-based assertion still
    // passes with `fan_out_to` broken or with the terminal rerouted through
    // the log-skipping `send_transient_session_event`. Draining the channel
    // is what actually pins delivery — and it needs no sleep, because
    // `recv()` wakes on the send.
    let mut delivered: Vec<String> = Vec::new();
    let saw_terminal = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some(event) = session_subscription.recv().await {
            let event_type = event.event_type.clone();
            delivered.push(event_type.clone());
            if event_type == "run_cancelled" {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(
        saw_terminal,
        "the cancelled subagent's own session must receive `run_cancelled` \
         on its live SSE stream (#1254); delivered: {delivered:?}"
    );

    // Whatever is already queued behind the terminal — a second broadcast
    // from a racing path would be sitting right here.
    delivered.extend(
        drain_events(&mut session_subscription)
            .into_iter()
            .map(|event| event.event_type),
    );
    let cancelled_count = delivered
        .iter()
        .filter(|kind| kind.as_str() == "run_cancelled")
        .count();
    assert_eq!(
        cancelled_count, 1,
        "the cancelled subagent's own session must receive EXACTLY one \
         `run_cancelled` SSE event (#1254); delivered: {delivered:?}"
    );

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// DM completion gate tests (#1154 implicit replies)
// ---------------------------------------------------------------------------

/// Implicit reply happy path: a peer-triggered DM run that completes with
/// plain final text — and NO `send_message` call — must have that text
/// delivered to the peer: persisted to the shared DM session with
/// `message_type: "dm"` metadata and a `RunTrigger` emitted for the peer.
#[tokio::test]
async fn dm_completion_gate_delivers_final_text() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    // Alice opens the DM; drain her trigger.
    let _ = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _ = tr.try_recv();

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    let run_id = RunId::new();

    // Bob's run completed with plain text and no tool calls.
    let exit = super::dm_lifecycle::handle_dm_run_completion(
        super::dm_lifecycle::DmRunCompletionContext {
            state: &state,
            run_id,
            session_id: dm_session_id,
            agent_id: bob_id,
            agent_name: Some("bob"),
            context_id: dm_context,
            is_peer_message: true,
            tool_calls: &[],
            response: "Hello Alice, pong!",
            reasoning: None,
        },
    )
    .await;
    assert_eq!(
        exit,
        super::dm_lifecycle::DmRunExit::Delivered,
        "plain final text must take the Delivered exit"
    );

    // The reply is persisted to the shared DM session as a real DM message.
    let history = state.session_manager.get_history(dm_session_id).unwrap();
    let delivered = history.iter().find(|m| {
        m.metadata.as_ref().is_some_and(|meta| {
            meta.get("from_agent").and_then(|v| v.as_str()) == Some("bob")
                && meta.get("message_type").and_then(|v| v.as_str()) == Some("dm")
        })
    });
    let delivered = delivered.expect("bob's implicit reply must be persisted as a DM message");
    assert!(
        matches!(&delivered.content, alms_session::Content::Text(t) if t == "Hello Alice, pong!"),
        "persisted DM message must carry the final assistant text"
    );

    // The peer is triggered with the reply text.
    let trigger = tr
        .try_recv()
        .expect("peer must be triggered by the delivery");
    assert_eq!(trigger.agent_id, alice_id);
    assert_eq!(trigger.input, "Hello Alice, pong!");
    assert!(
        matches!(trigger.source, MessageSource::Agent { ref from_name, .. } if from_name == "bob"),
        "trigger source must attribute the reply to bob"
    );

    shutdown_token.cancel();
}

/// B1 regression: a run whose only action was a FAILED `send_message`
/// (bad recipient → soft-error tool result) and no final text must take
/// the **errored** exit — the peer gets a `dm_ended` notification instead
/// of waiting forever. Pre-#1154, the presence-only termination check
/// completed the run as if the reply had been delivered.
#[tokio::test]
async fn dm_completion_gate_failed_send_no_longer_silently_completes() {
    use alms_core::ToolCallRole;

    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    let _ = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _ = tr.try_recv();

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    let run_id = RunId::new();

    // Bob's run called send_message with a bad recipient (soft error
    // result) and produced no final text.
    let failed_send_records = vec![
        alms_core::ToolCallRecord {
            seq: 0,
            role: ToolCallRole::Assistant,
            tool_name: Some("send_message".to_string()),
            tool_id: Some("call_1".to_string()),
            params: Some(r#"{"to":"nonexistent","message":"hi"}"#.to_string()),
            result: None,
            timestamp: chrono::Utc::now(),
            from_agent: None,
        },
        alms_core::ToolCallRecord {
            seq: 1,
            role: ToolCallRole::Tool,
            tool_name: Some("send_message".to_string()),
            tool_id: Some("call_1".to_string()),
            params: None,
            result: Some(r#"{"error":"Agent not found."}"#.to_string()),
            timestamp: chrono::Utc::now(),
            from_agent: None,
        },
    ];

    let exit = super::dm_lifecycle::handle_dm_run_completion(
        super::dm_lifecycle::DmRunCompletionContext {
            state: &state,
            run_id,
            session_id: dm_session_id,
            agent_id: bob_id,
            agent_name: Some("bob"),
            context_id: dm_context,
            is_peer_message: true,
            tool_calls: &failed_send_records,
            response: "",
            reasoning: None,
        },
    )
    .await;
    assert_eq!(
        exit,
        super::dm_lifecycle::DmRunExit::Errored,
        "failed send_message + no final text must take the Errored exit, \
         not silently complete"
    );

    // The peer is notified via the ConversationEnded trigger...
    let trigger = tr
        .try_recv()
        .expect("peer must be notified of the errored end");
    assert!(
        matches!(
            trigger.source,
            MessageSource::ConversationEnded {
                reason: ConversationEndReason::Errored {
                    interrupted: false,
                    ..
                },
                ..
            }
        ),
        "trigger must carry the Errored reason, classified as NOT interrupted \
         — this run COMPLETED, it just had nothing deliverable on its last \
         turn, so its transcript still has to reach the operator's chat \
         (#1258); got {:?}",
        trigger.source
    );

    // ...and the dm_ended marker lands in the session.
    let history = state.session_manager.get_history(dm_session_id).unwrap();
    assert!(
        history.iter().any(|m| {
            m.metadata
                .as_ref()
                .and_then(|meta| meta.get("message_type"))
                .and_then(|v| v.as_str())
                == Some("dm_ended")
        }),
        "dm_ended marker must be persisted on the errored exit"
    );

    shutdown_token.cancel();
}

/// Design default #3: an empty / whitespace-only final text with no end
/// tool takes the **errored** exit (the runtime's bounded nudge already
/// ran) — no empty message is delivered, and the peer is notified.
#[tokio::test]
async fn dm_completion_gate_empty_reply_ends_with_error() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    let _ = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _ = tr.try_recv();

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    let run_id = RunId::new();

    let exit = super::dm_lifecycle::handle_dm_run_completion(
        super::dm_lifecycle::DmRunCompletionContext {
            state: &state,
            run_id,
            session_id: dm_session_id,
            agent_id: bob_id,
            agent_name: Some("bob"),
            context_id: dm_context,
            is_peer_message: true,
            tool_calls: &[],
            response: "   \n",
            reasoning: None,
        },
    )
    .await;
    assert_eq!(exit, super::dm_lifecycle::DmRunExit::Errored);

    // No DM message from bob may have been delivered.
    let history = state.session_manager.get_history(dm_session_id).unwrap();
    assert!(
        !history.iter().any(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("from_agent").and_then(|v| v.as_str()) == Some("bob")
                    && meta.get("message_type").and_then(|v| v.as_str()) == Some("dm")
            })
        }),
        "no (empty) DM message may be delivered to the peer"
    );

    // The peer is notified via the Errored conversation end.
    let trigger = tr
        .try_recv()
        .expect("peer must be notified of the errored end");
    assert!(matches!(
        trigger.source,
        MessageSource::ConversationEnded {
            reason: ConversationEndReason::Errored { .. },
            ..
        }
    ));

    shutdown_token.cancel();
}

/// The `[Thinking]`-only promotion fallback (`response == reasoning`,
/// #1098) is NOT a deliverable reply: the gate must take the errored exit
/// rather than deliver a reasoning trace to the peer.
#[tokio::test]
async fn dm_completion_gate_promoted_reasoning_not_delivered() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    let _ = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _ = tr.try_recv();

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    let run_id = RunId::new();

    let trace = "Let me think about whether this ping needs a response...";
    let exit = super::dm_lifecycle::handle_dm_run_completion(
        super::dm_lifecycle::DmRunCompletionContext {
            state: &state,
            run_id,
            session_id: dm_session_id,
            agent_id: bob_id,
            agent_name: Some("bob"),
            context_id: dm_context,
            is_peer_message: true,
            tool_calls: &[],
            response: trace,
            reasoning: Some(trace),
        },
    )
    .await;
    assert_eq!(
        exit,
        super::dm_lifecycle::DmRunExit::Errored,
        "a promoted reasoning trace must NOT be delivered as a reply"
    );

    let history = state.session_manager.get_history(dm_session_id).unwrap();
    assert!(
        !history.iter().any(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("from_agent").and_then(|v| v.as_str()) == Some("bob")
                    && meta.get("message_type").and_then(|v| v.as_str()) == Some("dm")
            })
        }),
        "the reasoning trace must not land in the DM session as a message"
    );

    let trigger = tr
        .try_recv()
        .expect("peer must be notified of the errored end");
    assert!(matches!(
        trigger.source,
        MessageSource::ConversationEnded {
            reason: ConversationEndReason::Errored { .. },
            ..
        }
    ));

    shutdown_token.cancel();
}

/// Non-peer-triggered runs on a DM session (and peer runs on non-DM
/// sessions) are outside the gate: NotPeerDm, no side effects.
#[tokio::test]
async fn dm_completion_gate_not_peer_dm_is_noop() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    let _ = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _ = tr.try_recv();

    let dm_session_id = SessionId::deterministic_dm("alice", "bob");

    // Peer flag off → gate does not apply even on a dm: context.
    let exit = super::dm_lifecycle::handle_dm_run_completion(
        super::dm_lifecycle::DmRunCompletionContext {
            state: &state,
            run_id: RunId::new(),
            session_id: dm_session_id,
            agent_id: bob_id,
            agent_name: Some("bob"),
            context_id: "dm:alice:bob",
            is_peer_message: false,
            tool_calls: &[],
            response: "hello",
            reasoning: None,
        },
    )
    .await;
    assert_eq!(exit, super::dm_lifecycle::DmRunExit::NotPeerDm);

    // dm: prefix missing → gate does not apply even for peer messages.
    let exit = super::dm_lifecycle::handle_dm_run_completion(
        super::dm_lifecycle::DmRunCompletionContext {
            state: &state,
            run_id: RunId::new(),
            session_id: dm_session_id,
            agent_id: bob_id,
            agent_name: Some("bob"),
            context_id: "web-chat-1234",
            is_peer_message: true,
            tool_calls: &[],
            response: "hello",
            reasoning: None,
        },
    )
    .await;
    assert_eq!(exit, super::dm_lifecycle::DmRunExit::NotPeerDm);

    assert!(
        tr.try_recv().is_err(),
        "NotPeerDm exits must produce no triggers"
    );

    shutdown_token.cancel();
}

/// Option C (#1156, Tim's review on the #1154 PR): DM sessions are
/// agent-to-agent only. `POST /runs` always enqueues with
/// `is_peer_message: false`, so a run created through it on a `dm:`
/// session would arm the implicit-reply machinery (DM recipient prompt +
/// send_message peer-fold) while the completion gate refuses delivery
/// (`NotPeerDm`) — a guaranteed silent drop. The handler must reject with
/// a structured 400 instead.
#[tokio::test]
async fn create_run_rejects_dm_session_with_structured_400() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    let session = state
        .session_manager
        .get_or_create_shared(dm_session_id, dm_context);

    let req = CreateRunRequest {
        session_id: session.id,
        // Shared DM sessions require an agent_id on the request; supply
        // one so the rejection under test (and not AGENT_ID_REQUIRED) is
        // what fires.
        agent_id: Some(AgentId::new()),
        input: RunInput::Text { text: "hi".into() },
    };

    let Err((status, body)) = super::lifecycle::create_run(State(state.clone()), Json(req)).await
    else {
        panic!("create_run must reject non-peer runs on dm: sessions (#1156)");
    };
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(
        body.0["error_code"], "DM_SESSION_NOT_DIRECTLY_RUNNABLE",
        "rejection must carry the structured error code; got {:?}",
        body.0
    );
    assert_eq!(body.0["context_id"], dm_context);

    // The rejection must fire BEFORE any side effects: no run recorded,
    // no user input pre-persisted to the DM session.
    assert!(
        state.run_manager.list_by_session(session.id, 10).is_empty(),
        "no run may be created on the rejected path"
    );
    assert!(
        state
            .session_manager
            .get_history(session.id)
            .unwrap()
            .is_empty(),
        "no input may be persisted on the rejected path"
    );

    shutdown_token.cancel();
}

/// #1289 item 3: `POST /runs` on a subagent session is rejected on
/// principle, not incidentally.
///
/// Subagent turns come from `invoke_agent` -> `SubagentDispatcher` ->
/// `run_subagent_loop`, which alone records the `parent_run_id` linkage
/// and returns the result to the awaiting parent. A run created here
/// would write into a coordinator-owned transcript and deliver to nobody.
///
/// The request deliberately carries **no** `agent_id`. That is the case
/// the old incidental `AGENT_SESSION_MISMATCH` never covered — with
/// `agent_id` omitted the handler took the session's own id and proceeded,
/// before #1278 as well as after — so a guard that only fired on a
/// supplied-and-differing `agent_id` would leave this test red.
#[tokio::test]
async fn create_run_rejects_subagent_session_with_structured_400() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    // #1278 keying: filed under the INVOKED agent's registry id, context
    // naming the invoking parent.
    let invoked = AgentId::new();
    let parent = AgentId::new();
    let context_id = format!("subagent_{}_reviewer", parent.0);
    let session = state.session_manager.get_or_create(invoked, &context_id);

    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: None,
        input: RunInput::Text { text: "hi".into() },
    };

    let Err((status, body)) = super::lifecycle::create_run(State(state.clone()), Json(req)).await
    else {
        panic!("create_run must reject operator runs on subagent sessions (#1289)");
    };
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(
        body.0["error_code"], "SUBAGENT_SESSION_NOT_DIRECTLY_RUNNABLE",
        "rejection must carry the structured error code; got {:?}",
        body.0
    );
    assert_eq!(body.0["context_id"], context_id);

    // The rejection must fire BEFORE any side effect, exactly like the DM
    // guard it sits next to: no run recorded, no user input pre-persisted
    // into a transcript a live coordinator loop may be writing.
    assert!(
        state.run_manager.list_by_session(session.id, 10).is_empty(),
        "no run may be created on the rejected path"
    );
    assert!(
        state
            .session_manager
            .get_history(session.id)
            .unwrap()
            .is_empty(),
        "no input may be persisted on the rejected path"
    );

    shutdown_token.cancel();
}

/// The guard keys on the `subagent_` prefix, not on a successful parse,
/// so every shape the coordinator has ever minted is covered: the
/// ephemeral `subagent_{parent}_{task_id}`, and the legacy pre-#1185
/// `subagent_{task_id}` that carries no parent segment and which
/// `parse_subagent_context` returns `None` for. A guard written on the
/// parse would let the legacy shape through.
#[tokio::test]
async fn create_run_rejects_every_subagent_context_shape() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let parent = AgentId::new();

    for context_id in [
        format!("subagent_{}_{}", parent.0, uuid::Uuid::new_v4()),
        format!("subagent_{}", uuid::Uuid::new_v4()),
    ] {
        let session = state
            .session_manager
            .get_or_create(AgentId::new(), &context_id);
        let req = CreateRunRequest {
            session_id: session.id,
            agent_id: None,
            input: RunInput::Text { text: "hi".into() },
        };
        let Err((status, body)) =
            super::lifecycle::create_run(State(state.clone()), Json(req)).await
        else {
            panic!("create_run must reject the subagent context {context_id}");
        };
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(
            body.0["error_code"], "SUBAGENT_SESSION_NOT_DIRECTLY_RUNNABLE",
            "{context_id} must be rejected by the subagent guard; got {:?}",
            body.0
        );
    }

    shutdown_token.cancel();
}

/// Positive control on the guard's width. `subagent_` is a prefix test,
/// not a substring one: an ordinary chat whose `context_id` merely
/// contains the word must still be runnable, or the guard has rotted into
/// "reject anything that mentions subagents".
///
/// `classify_session_type` already draws this line — the assertion is
/// here so that relaxing the guard to `contains` is a red test rather
/// than a silent outage of every session with an unlucky name.
#[tokio::test]
async fn create_run_admits_a_chat_session_whose_context_merely_mentions_subagents() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "notes-about-subagent_design");

    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: Some(agent_id),
        input: RunInput::Text { text: "hi".into() },
    };
    let result = super::lifecycle::create_run(State(state.clone()), Json(req)).await;

    match result {
        Ok((status, _)) => assert_eq!(status, axum::http::StatusCode::CREATED),
        Err((status, body)) => panic!(
            "an ordinary chat session must not hit the subagent guard; \
             got {status:?} {:?}",
            body.0
        ),
    }

    shutdown_token.cancel();
}

/// Companion to `create_run_rejects_dm_session_with_structured_400`: the
/// peer-trigger path (`MessageBus` -> `RunTrigger` -> `run_trigger_loop`,
/// which enqueues with `is_peer_message: true`) must be UNAFFECTED by the
/// Option C rejection — it never goes through `create_run`. End-to-end
/// against the mock LLM: the triggered run is created, executes, and the
/// DM completion gate delivers bob's implicit reply to alice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peer_triggered_dm_run_is_not_rejected_and_delivers() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_mock_llm();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    // Alice DMs bob through the real MessageBus — persists the message to
    // the shared DM session and emits the RunTrigger the gateway's
    // trigger loop would normally consume.
    let _ = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let trigger = tr
        .try_recv()
        .expect("MessageBus must emit a RunTrigger for bob");
    assert_eq!(trigger.agent_id, bob_id);

    // Feed the trigger through the actual run_trigger_loop.
    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx.send(trigger).await.unwrap();
    drop(test_tx);
    super::notifications::run_trigger_loop(test_rx, state.clone()).await;

    let dm_session_id = SessionId::deterministic_dm("alice", "bob");

    // The run was created — NOT rejected.
    let runs = state.run_manager.list_by_session(dm_session_id, 10);
    assert!(
        !runs.is_empty(),
        "peer-triggered DM run must be created on the trigger path"
    );

    // ...and executes to completion: the mock LLM's reply is delivered to
    // the DM session by the completion gate. Poll with a timeout — the
    // run executes on the agent queue's spawned task.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let delivered = loop {
        let history = state.session_manager.get_history(dm_session_id).unwrap();
        let found = history
            .iter()
            .find(|m| {
                m.metadata.as_ref().is_some_and(|meta| {
                    meta.get("from_agent").and_then(|v| v.as_str()) == Some("bob")
                        && meta.get("message_type").and_then(|v| v.as_str()) == Some("dm")
                })
            })
            .cloned();
        if let Some(msg) = found {
            break msg;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for bob's implicit reply to be delivered; \
             history: {history:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert!(
        matches!(&delivered.content, alms_session::Content::Text(t) if t.contains("[mock]")),
        "delivered DM message must carry the mock LLM's reply text; got {delivered:?}"
    );

    // The delivery re-triggers alice (normal DM ping-pong), proving the
    // full MessageBus::send shape was used — not a bare session write.
    let alice_trigger = tokio::time::timeout(std::time::Duration::from_secs(5), tr.recv())
        .await
        .expect("alice's trigger must arrive after the delivery")
        .expect("trigger channel must stay open");
    assert_eq!(alice_trigger.agent_id, alice_id);
    assert!(
        matches!(alice_trigger.source, MessageSource::Agent { ref from_name, .. } if from_name == "bob"),
        "alice's trigger must attribute the reply to bob; got {:?}",
        alice_trigger.source
    );

    shutdown_token.cancel();
}

/// B4 regression: a peer-triggered DM run that dies on a pre-loop setup
/// failure (e.g. `resolve_agent_config` error — BEFORE the agent name is
/// known) must still notify the peer. The helper re-resolves the agent
/// name from the registry by ID.
#[tokio::test]
async fn dm_setup_failure_notifies_peer_without_agent_name() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    let _ = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _ = tr.try_recv();

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    let run_id = RunId::new();

    // agent_name: None — the resolve-failure arm fires before the registry
    // record is loaded; the helper must resolve "bob" by agent_id.
    super::dm_lifecycle::notify_dm_peer_of_setup_failure(
        &state,
        &run_id,
        &dm_session_id,
        bob_id,
        None,
        dm_context,
        true,
        "failed to resolve agent config".to_string(),
    )
    .await;

    // Peer notified with the Errored reason.
    let trigger = tr
        .try_recv()
        .expect("peer must be notified of the setup failure");
    assert_eq!(trigger.agent_id, alice_id);
    match trigger.source {
        MessageSource::ConversationEnded {
            ref from_name,
            ref reason,
            ..
        } => {
            assert_eq!(from_name, "bob", "helper must resolve the agent name by ID");
            assert!(
                matches!(
                    reason,
                    ConversationEndReason::Errored {
                        message,
                        interrupted: true,
                    } if message.contains("failed to resolve agent config")
                ),
                "reason must carry the setup-failure message, classified as an \
                 interrupted end — the run never started its loop, so no turn \
                 of this DM completed (#1258); got {reason:?}"
            );
        }
        other => panic!("expected ConversationEnded source, got {other:?}"),
    }

    // dm_ended marker persisted.
    let history = state.session_manager.get_history(dm_session_id).unwrap();
    assert!(
        history.iter().any(|m| {
            m.metadata
                .as_ref()
                .and_then(|meta| meta.get("message_type"))
                .and_then(|v| v.as_str())
                == Some("dm_ended")
        }),
        "dm_ended marker must be persisted on setup failure"
    );

    shutdown_token.cancel();
}

/// Non-DM / non-peer setup failures are outside the helper: no triggers,
/// no markers.
#[tokio::test]
async fn dm_setup_failure_noop_outside_peer_dm() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (_alice_id, bob_id) = seed_alice_bob(&state);

    super::dm_lifecycle::notify_dm_peer_of_setup_failure(
        &state,
        &RunId::new(),
        &SessionId::new(),
        bob_id,
        Some("bob"),
        "web-chat-1234",
        false,
        "boom".to_string(),
    )
    .await;

    assert!(
        tr.try_recv().is_err(),
        "non-peer-DM setup failures must not emit triggers"
    );

    shutdown_token.cancel();
}

/// S1 (#1154): a peer-triggered DM run cancelled while still `Queued`
/// (HTTP cancel / #1109 deny cascade / session-cancel / shutdown) reaches
/// `execute_run`'s pre-loop early-exit. That exit must notify the DM peer
/// with `UserCancelled` — otherwise the peer is stranded on "Chatting
/// with…" until the 1800s depth-expiry sweep, because neither the
/// synchronous `cancel_run` handler nor the early-exit historically
/// signalled the peer.
#[tokio::test]
async fn queued_then_cancelled_dm_run_notifies_peer() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    // Open the DM: alice -> bob. This creates the shared DM session and the
    // depth entry, and emits alice's RunTrigger for bob (which we discard —
    // we drive bob's run manually below).
    let _ = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _ = tr.try_recv(); // discard alice->bob trigger

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    let mut dm_rx = subscribe_session(&state, dm_session_id);

    // Bob's peer-triggered DM run, inserted as Queued, then cancelled while
    // still queued (mirrors the HTTP cancel / deny cascade landing before the
    // queue dispatches the work item).
    let run = Run::new(dm_session_id, bob_id, "ping".to_string());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    cancel_token.cancel();

    // The per-agent queue eventually dispatches the work item; `execute_run`
    // hits the pre-loop early-exit (token already cancelled).
    super::lifecycle::execute_run(
        state.clone(),
        super::RunParams {
            run_id,
            session_id: dm_session_id,
            agent_id: bob_id,
            input: run.input,
            context_id: dm_context.to_string(),
            cancel_token,
            is_peer_message: true,
            is_system_triggered: true,
            input_pre_persisted: false,
            dm_ended_peer: None,
        },
    )
    .await;

    // The run must be Cancelled (never auto-started).
    assert_eq!(
        state.run_manager.get_run(run_id).unwrap().status(),
        RunStatus::Cancelled,
        "queued-then-cancelled run must stay Cancelled"
    );

    // The peer (alice) must be notified with UserCancelled — the S1 fix.
    let trigger = tr
        .try_recv()
        .expect("peer must be notified of the queued-cancel (S1)");
    assert_eq!(trigger.agent_id, alice_id, "notification targets the peer");
    match trigger.source {
        MessageSource::ConversationEnded {
            ref from_name,
            ref reason,
            ..
        } => {
            assert_eq!(from_name, "bob", "ended-by is the cancelled agent");
            assert!(
                matches!(reason, ConversationEndReason::UserCancelled),
                "queued-cancel must signal UserCancelled; got {reason:?}"
            );
        }
        other => panic!("expected ConversationEnded source, got {other:?}"),
    }

    // dm_ended marker with reason=user_cancelled persisted to the DM session.
    let history = state.session_manager.get_history(dm_session_id).unwrap();
    assert!(
        history.iter().any(|m| {
            let meta = m.metadata.as_ref();
            meta.and_then(|x| x.get("message_type"))
                .and_then(|v| v.as_str())
                == Some("dm_ended")
                && meta.and_then(|x| x.get("reason")).and_then(|v| v.as_str())
                    == Some("user_cancelled")
        }),
        "dm_ended marker with reason=user_cancelled must be persisted"
    );

    // dm_conversation_ended SSE emitted on the DM session stream.
    tokio::task::yield_now().await;
    let events = drain_events(&mut dm_rx);
    assert!(
        events
            .iter()
            .any(|e| e.event_type == "dm_conversation_ended"),
        "expected dm_conversation_ended SSE on the DM session stream"
    );

    shutdown_token.cancel();
}

/// S1 (#1154) negative: a NON-peer run cancelled while queued (e.g. a
/// user-initiated `POST /runs` run) must NOT emit any DM peer notification —
/// the helper is gated on `is_peer_message && dm:`.
#[tokio::test]
async fn queued_then_cancelled_non_peer_run_does_not_notify() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (_alice_id, bob_id) = seed_alice_bob(&state);

    let session = state.session_manager.get_or_create(bob_id, "web-chat-1");
    let session_id = session.id;

    let run = Run::new(session_id, bob_id, "hello".to_string());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    cancel_token.cancel();

    super::lifecycle::execute_run(
        state.clone(),
        super::RunParams {
            run_id,
            session_id,
            agent_id: bob_id,
            input: run.input,
            context_id: "web-chat-1".to_string(),
            cancel_token,
            // Not a peer message — the user cancelled their own run.
            is_peer_message: false,
            is_system_triggered: false,
            input_pre_persisted: false,
            dm_ended_peer: None,
        },
    )
    .await;

    assert_eq!(
        state.run_manager.get_run(run_id).unwrap().status(),
        RunStatus::Cancelled
    );
    assert!(
        tr.try_recv().is_err(),
        "non-peer queued-cancel must not emit a DM peer notification"
    );

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// 12. Subagent status snapshot on session-stream reattach (#1189 follow-up)
// ---------------------------------------------------------------------------

/// Build an `AppState` whose LLM streams ONE OpenAI-format content chunk and
/// then holds the connection open without finishing the response.
///
/// This freezes a real subagent run in its "actively writing" state: the
/// runtime received a `token_delta` (so the coordinator relay emitted — and
/// recorded — a `writing` activity signal) but the run cannot complete until
/// the generous `stream_chunk_timeout_secs` fires, long after the test ends.
/// That is exactly the live condition under which the "chip stuck on
/// Starting…" bug was reproduced.
async fn test_app_state_with_streaming_then_stalling_llm() -> (
    AppState,
    CancellationToken,
    mpsc::UnboundedReceiver<SubagentCompletion>,
    mpsc::Receiver<RunTrigger>,
    mpsc::Receiver<DmEvent>,
) {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{addr}");

    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                // One OpenAI-compatible SSE chunk with visible content, then
                // hold the socket (no finish_reason, no EOF): the client's
                // stream stays "in flight" for the rest of the test.
                let chunk = concat!(
                    "data: {\"id\":\"stall\",\"object\":\"chat.completion.chunk\",",
                    "\"created\":0,\"model\":\"m\",\"choices\":[{\"index\":0,",
                    "\"delta\":{\"role\":\"assistant\",\"content\":\"partial answer \"},",
                    "\"finish_reason\":null}]}\n\n"
                );
                let response =
                    format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n{chunk}");
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.flush().await;
                // Park until the client goes away.
                std::future::pending::<()>().await;
            });
        }
    });

    let llm_config = alms_runtime::LlmConfig {
        base_url,
        api_key: "fake-key-for-test".to_string(),
        // Generous timeouts: the run must still be mid-stream ("writing")
        // when the test reattaches — nothing here should fire during the
        // test window.
        timeout_secs: 60,
        stream_chunk_timeout_secs: 60,
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
    let (trigger_tx, trigger_rx) = mpsc::channel(64);
    let (dm_event_tx, dm_event_rx) = mpsc::channel(64);
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

/// REGRESSION (#1189 follow-up): a session-stream subscriber that attaches
/// while a background subagent is mid-phase must immediately learn the
/// subagent's CURRENT activity.
///
/// The live `subagent_activity` signal is ephemeral (never persisted or
/// replayed) and deduplicated at the source to ONE emission per activity
/// transition, so it only reaches the subscribers attached at that instant.
/// Before the fix, any client that (re)attached afterwards — page reload,
/// session switch back from the subagent view, a second tab, an SSE
/// reconnect — rendered the chip as "Starting…" until the subagent's NEXT
/// kind transition, even while it was actively writing. This test drives the
/// REAL end-to-end path: a real background subagent (spawned through the
/// coordinator) makes a real streaming LLM call that delivers one token and
/// stalls, the REAL relay emits + records the `writing` signal through the
/// production bg-event forwarder/drain, and then a NEW subscriber attaches
/// through the same `attach_session_stream` path the
/// `GET /sessions/{id}/events` endpoint uses.
///
/// With the snapshot replay removed (the pre-fix behaviour), the reattached
/// subscriber receives nothing and the final assertions fail.
#[tokio::test]
async fn reattached_session_stream_receives_subagent_activity_snapshot() {
    let (state, shutdown_token, _cr, _tr, _dr) =
        test_app_state_with_streaming_then_stalling_llm().await;

    let parent_agent_id = AgentId::new();
    let parent_session = state
        .session_manager
        .get_or_create(parent_agent_id, "parent-chat");
    let parent_session_id = parent_session.id;

    // Subscriber attached BEFORE the subagent starts — the control group.
    // Uses the same attach path; the snapshot is empty at this point (no
    // subagents yet), so it receives only genuinely live events.
    let mut early_rx = super::streaming::attach_session_stream(&state, parent_session_id);

    // Production-shaped background event leg: the coordinator relay forwards
    // status signals into a `RuntimeEventForwarder`, and a drain task routes
    // them via `route_bg_event` onto the parent's session stream — mirroring
    // the wiring `execute_run` installs for `invoke_agent` (parent-dead leg,
    // which is the steady state for a long-lived background subagent).
    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<alms_runtime::RuntimeEvent>();
    let bg_fwd: Arc<dyn alms_tools::EventForwarder> =
        Arc::new(super::tools::RuntimeEventForwarder::new(bg_tx));
    let bg_run_id = RunId::new();
    let drain_state = state.clone();
    tokio::spawn(async move {
        while let Some(event) = bg_rx.recv().await {
            match super::tools::route_bg_event(event, None, bg_run_id, parent_session_id) {
                Some(super::tools::RoutedBgEvent::Persist(sse)) => {
                    drain_state
                        .run_manager
                        .send_session_event(parent_session_id, bg_run_id, sse)
                        .await;
                }
                Some(super::tools::RoutedBgEvent::Transient(sse)) => {
                    drain_state
                        .run_manager
                        .send_transient_session_event(parent_session_id, sse);
                }
                None => {}
            }
        }
    });

    // REAL background dispatch: real subagent runtime, real (stalling)
    // streaming LLM call, real relay. `parent_inv` is the parent's
    // invoke_agent tool-invocation-id — the chip-resolution correlator the
    // UI matches identity-exactly (#1190 Codex P2).
    let parent_inv = uuid::Uuid::new_v4();
    let (task_uuid, _sub_session_id) = state
        .coordinator
        .dispatch_background(
            "Investigate the flaky test".to_string(),
            parent_session_id,
            parent_agent_id,
            None,
            Some(bg_fwd),
            None,
            None,
            Some(parent_inv),
        )
        .await
        .expect("dispatch_background should succeed");

    // Wait until the subagent is observably WRITING: the stub delivered its
    // one token, the relay emitted the (single, deduplicated) live signal,
    // and the recording landed on the handle.
    let mut attempts = 0;
    while state
        .coordinator
        .subagent_activity_snapshot(parent_session_id)
        .is_empty()
    {
        attempts += 1;
        assert!(
            attempts < 200,
            "subagent never reached the 'writing' state — streaming stub broken?"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // The attached-at-emission subscriber got the live signal (this leg
    // always worked — it is what the pre-fix tests modelled).
    let early_events = drain_events(&mut early_rx);
    let expected_label = format!("subagent-{}", &task_uuid.to_string()[..8]);
    assert!(
        early_events
            .iter()
            .any(|e| e.event_type == "subagent_activity"
                && e.data.get("kind").and_then(|v| v.as_str()) == Some("writing")
                && e.data.get("source_agent").and_then(|v| v.as_str())
                    == Some(expected_label.as_str())),
        "subscriber attached before the transition must receive the live \
         `writing` signal, got: {:?}",
        early_events
            .iter()
            .map(|e| (&e.event_type, &e.data))
            .collect::<Vec<_>>()
    );

    let hwm_before = state
        .run_manager
        .latest_session_event_id(parent_session_id)
        .await;

    // THE REGRESSION: a subscriber attaching AFTER the (once-per-transition,
    // ephemeral) signal fired. The subagent is still actively writing — the
    // reattached client must be told so instead of showing "Starting…".
    let mut reattached_rx = super::streaming::attach_session_stream(&state, parent_session_id);
    let snapshot_event = reattached_rx
        .try_recv()
        .expect("reattached session stream must immediately receive the subagent status snapshot");
    assert_eq!(snapshot_event.event_type, "subagent_activity");
    assert_eq!(
        snapshot_event.data.get("kind").and_then(|v| v.as_str()),
        Some("writing"),
        "the snapshot must carry the subagent's CURRENT activity kind"
    );
    assert_eq!(
        snapshot_event
            .data
            .get("source_agent")
            .and_then(|v| v.as_str()),
        Some(expected_label.as_str()),
        "the snapshot must be tagged with the same label as the live signals \
         so it resolves to the same status-bar chip"
    );
    assert_eq!(
        snapshot_event
            .data
            .get("parent_tool_invocation_id")
            .and_then(|v| v.as_str()),
        Some(parent_inv.to_string().as_str()),
        "the snapshot must carry the parent invoke_agent correlator so the \
         UI resolves the chip identity-exactly — with concurrent unnamed \
         subagents, the label alone first-matches onto the WRONG chip \
         (#1190 Codex P2)"
    );

    // Snapshot events keep the live signal's ephemerality contract: no
    // event id (passes the replay dedup filter) and nothing persisted.
    assert!(
        snapshot_event.event_id.is_none(),
        "snapshot subagent_activity must not carry a persisted event id"
    );
    assert_eq!(
        state
            .run_manager
            .latest_session_event_id(parent_session_id)
            .await,
        hwm_before,
        "replaying the snapshot must not write the session event log"
    );

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// Job episodes (#1198): deferred completion across DMs / subagents
// ---------------------------------------------------------------------------

/// Extract the persisted `job_notification` marker's `(text, metadata)` from
/// a session's history. Mirrors the helper in `notifications::tests`.
fn job_marker_from(state: &AppState, session_id: SessionId) -> (String, serde_json::Value) {
    let history = state.session_manager.get_history(session_id).unwrap();
    let marker = history
        .iter()
        .find(|m| {
            m.metadata
                .as_ref()
                .and_then(|meta| meta.get("type"))
                .and_then(|v| v.as_str())
                == Some("job_notification")
        })
        .expect("job_notification marker must be persisted on the user-facing session");
    let text = match &marker.content {
        alms_session::Content::Text(t) => t.clone(),
        _ => panic!("job marker should be text content"),
    };
    (text, marker.metadata.clone().unwrap())
}

/// Create a recurring job in the store (daily at midnight — never fires
/// during a test on its own).
fn create_recurring_job(state: &AppState, agent_id: AgentId, prompt: &str) -> alms_core::JobId {
    state
        .job_store
        .create(alms_core::job::CreateJobRequest {
            agent_id,
            prompt: prompt.to_string(),
            schedule: alms_core::job::JobSchedule::Recurring {
                cron: "0 0 * * *".to_string(),
            },
        })
        .expect("job creation must succeed")
        .id
}
#[tokio::test]
async fn job_mutation_responses_return_authoritative_persisted_entities() {
    use axum::response::IntoResponse as _;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();
    let (agent_id, _) = seed_alice_bob(&state);

    let response = crate::jobs::create_job(
        axum::extract::State(state.clone()),
        axum::Json(alms_core::job::CreateJobRequest {
            agent_id,
            prompt: "authoritative response".to_string(),
            schedule: alms_core::job::JobSchedule::Recurring {
                cron: "0 0 * * *".to_string(),
            },
        }),
    )
    .await
    .into_response();
    assert_eq!(response.status(), axum::http::StatusCode::CREATED);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("create response body");
    let created: serde_json::Value =
        serde_json::from_slice(&body).expect("create response must be JSON");
    assert!(
        created["next_run_at"].is_string(),
        "create response must include the scheduler's persisted next run"
    );
    assert_eq!(created["lifecycle_revision"], 0);

    let job_id = state
        .job_store
        .list()
        .into_iter()
        .find(|job| job.prompt == "authoritative response")
        .expect("created job")
        .id;
    let response = crate::jobs::cancel_job(
        axum::extract::State(state.clone()),
        axum::extract::Path(job_id),
    )
    .await
    .into_response();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("cancel response body");
    let cancelled: serde_json::Value =
        serde_json::from_slice(&body).expect("cancel response must be JSON");
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(cancelled["lifecycle_revision"], 1);
    assert_eq!(cancelled["terminal_reason"], "operator_cancelled");

    shutdown_token.cancel();
}

fn create_once_job(state: &AppState, agent_id: AgentId, prompt: &str) -> alms_core::JobId {
    state
        .job_store
        .create(alms_core::job::CreateJobRequest {
            agent_id,
            prompt: prompt.to_string(),
            schedule: alms_core::job::JobSchedule::Once {
                run_at: chrono::Utc::now(),
            },
        })
        .expect("job creation must succeed")
        .id
}

/// Drive a job into the normally spent one-shot state. Completion is distinct
/// from operator cancellation so late detached results remain deliverable.
fn spend_one_shot(state: &AppState, job_id: alms_core::JobId) {
    state
        .job_store
        .record_run(
            job_id,
            chrono::Utc::now(),
            alms_core::JobStatus::Completed,
            None,
        )
        .expect("record_run must succeed");
    assert_eq!(
        state.job_store.get(job_id).unwrap().status(),
        alms_core::JobStatus::Completed,
        "sanity: a spent one-shot carries JobStatus::Completed"
    );
    assert!(
        !state.operator_cancelled_jobs.contains(&job_id),
        "sanity: a spent one-shot was never operator-cancelled"
    );
}

/// The #1198 regression floor: a job whose turn opens no async work must
/// complete at turn-1 end exactly like the pre-episode flow — completion
/// card on the user-facing session, `record_run` + re-arm, episode gone.
///
/// Runs the REAL pipeline end-to-end: `fire_job_run` -> episode open ->
/// `execute_run` (mock LLM) -> tail hook -> quiescent close.
#[tokio::test]
async fn episode_job_with_no_async_work_completes_at_turn_end() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();
    let agent_id = AgentId::new();

    // A user-facing session so the completion card has a target.
    let web_session_id = state.session_manager.get_or_create(agent_id, "web").id;

    let job_id = create_recurring_job(&state, agent_id, "daily digest");

    super::notifications::fire_job_run(state.clone(), job_id)
        .await
        .expect("fire_job_run must succeed");

    // Episode closed at turn end (no pending work).
    assert!(
        state.job_episodes.snapshot(job_id).is_none(),
        "a no-async-work episode must close at turn-1 end"
    );

    // The completion card was persisted with episode stats (one turn, no
    // async work, not timed out).
    let (_text, meta) = job_marker_from(&state, web_session_id);
    assert_eq!(meta["job_id"], job_id.0.to_string());
    assert_eq!(meta["episode"]["turns"], 1);
    assert_eq!(meta["episode"]["dm_count"], 0);
    assert_eq!(meta["episode"]["subagent_count"], 0);
    assert_eq!(meta["episode"]["timed_out"], false);

    // The job record was updated and the recurring schedule re-armed.
    let job = state.job_store.get(job_id).expect("job still exists");
    assert_eq!(job.status(), alms_core::JobStatus::Active);
    assert!(job.last_run_at.is_some(), "record_run must have fired");
    assert!(
        job.next_run_at.expect("re-armed") > chrono::Utc::now(),
        "no cron tick elapsed during the turn — normal future re-arm"
    );

    shutdown_token.cancel();
}

#[tokio::test]
async fn scheduler_rearms_job_after_run_registration_persistence_failure() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();
    let agent_id = AgentId::new();
    let job_id = create_recurring_job(&state, agent_id, "retry durable registration");
    let (fire_tx, fire_rx) = mpsc::unbounded_channel();
    let loop_state = state.clone();
    let loop_handle = tokio::spawn(async move {
        super::notifications::scheduler_fire_loop(fire_rx, loop_state).await;
    });

    state.run_manager.inject_next_persistence_failure();
    fire_tx.send(job_id).unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if state.scheduler.pending_count().await == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("registration failure should promptly re-arm the job");

    assert!(state.run_manager.runs.is_empty());
    assert!(state.job_episodes.snapshot(job_id).is_none());
    let retrying = state.job_store.get(job_id).unwrap();
    assert_eq!(retrying.status(), alms_core::JobStatus::Pending);
    assert_eq!(retrying.retry_count(), 1);
    assert!(retrying.next_run_at.is_some());
    assert!(
        retrying
            .last_error()
            .is_some_and(|error| error.contains("persistence")),
        "the durable job record must retain dispatch failure provenance"
    );
    loop_handle.abort();
    shutdown_token.cancel();
}

/// #1198 exit 1/5: a queued-then-cancelled episode run (pre-cancel early
/// exit — it never executes) must still release its in-flight reservation,
/// closing the episode and running the full close block (card +
/// `record_run` + re-arm). A missed release here would stall the job until
/// the 4-hour deadline sweep.
#[tokio::test]
async fn episode_precancelled_turn_releases_reservation_and_closes() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let web_session_id = state.session_manager.get_or_create(agent_id, "web").id;
    let job_id = create_recurring_job(&state, agent_id, "cancelled before start");

    // Mirror fire_job_run's setup, but cancel the token before execution.
    let context_id = format!("job_{}", job_id.0);
    let session_id = state
        .session_manager
        .get_or_create(agent_id, &context_id)
        .id;
    let run = Run::for_job(
        session_id,
        agent_id,
        "cancelled before start".into(),
        job_id,
    );
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());
    state
        .job_episodes
        .open(job_id, session_id, agent_id, run_id);

    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    cancel_token.cancel();

    super::lifecycle::execute_run(
        state.clone(),
        super::RunParams {
            run_id,
            session_id,
            agent_id,
            input: run.input,
            context_id,
            cancel_token,
            is_peer_message: false,
            is_system_triggered: true,
            input_pre_persisted: false,
            dm_ended_peer: None,
        },
    )
    .await;

    // The reservation was released and the episode closed.
    assert!(
        state.job_episodes.snapshot(job_id).is_none(),
        "pre-cancelled turn must release its reservation and close the episode"
    );
    // The close block ran: card persisted, job recorded + re-armed.
    let (_text, meta) = job_marker_from(&state, web_session_id);
    assert_eq!(meta["job_status"], "cancelled");
    let job = state.job_store.get(job_id).unwrap();
    assert!(job.last_run_at.is_some());
    assert_eq!(job.status(), alms_core::JobStatus::Active);

    shutdown_token.cancel();
}

/// #1198 step 5 (DM side): a `ConversationEnded` trigger for the JOB AGENT
/// whose DM is pending on an open episode must be routed onto the JOB
/// session (not the trigger's original target) and its continuation run
/// must carry the episode's `job_id`.
#[tokio::test]
async fn conversation_ended_routes_continuation_onto_job_session() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    // Alice's job session + open episode with a pending DM to bob.
    let job_id = create_recurring_job(&state, alice_id, "ask bob");
    let job_context = format!("job_{}", job_id.0);
    let job_session_id = state
        .session_manager
        .get_or_create(alice_id, &job_context)
        .id;
    let turn1 = RunId::new();
    state
        .job_episodes
        .open(job_id, job_session_id, alice_id, turn1);
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    assert!(matches!(
        state
            .job_episodes
            .on_run_complete(job_id, turn1, vec![dm_session_id], vec![]),
        super::job_episode::RunCompletion::Open
    ));

    // The trigger's original target: alice's invisible notifications session
    // (the pre-#1198 fallback shape).
    let notif_session = state
        .session_manager
        .get_or_create(alice_id, "notifications:alice");

    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx
        .send(RunTrigger {
            agent_id: alice_id,
            session_id: notif_session.id,
            input: "DM ended marker".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: bob_id,
                from_name: "bob".to_string(),
                reason: ConversationEndReason::Ignored,
                self_notification: false,
                source_session_id: None,
            },
            context_id: notif_session.context_id.clone(),
        })
        .await
        .unwrap();
    drop(test_tx);
    super::notifications::run_trigger_loop(test_rx, state.clone()).await;

    // The continuation run landed on the JOB session, stamped with job_id.
    let runs = state.run_manager.list_by_session(job_session_id, 10);
    assert!(
        !runs.is_empty(),
        "continuation run must be created on the job session"
    );
    assert_eq!(
        runs[0].job_id,
        Some(job_id),
        "continuation run must carry the episode's job_id"
    );
    assert!(
        state
            .run_manager
            .list_by_session(notif_session.id, 10)
            .is_empty(),
        "the notifications: fallback target must NOT receive the run"
    );

    shutdown_token.cancel();
}

/// #1258 exception: the "interrupted ends get no run" rule is scoped to the
/// trigger's OWN target — the session the operator sits on. A #1198 job
/// episode awaiting the DM still gets its continuation run even when the DM
/// died on a failure, because that run resumes the JOB rather than narrating
/// the DM; dropping it would hang the episode until the 4h deadline sweep.
#[tokio::test]
async fn interrupted_dm_end_still_fires_its_job_episode_continuation() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    let job_id = create_recurring_job(&state, alice_id, "ask bob");
    let job_context = format!("job_{}", job_id.0);
    let job_session_id = state
        .session_manager
        .get_or_create(alice_id, &job_context)
        .id;
    let turn1 = RunId::new();
    state
        .job_episodes
        .open(job_id, job_session_id, alice_id, turn1);
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    assert!(matches!(
        state
            .job_episodes
            .on_run_complete(job_id, turn1, vec![dm_session_id], vec![]),
        super::job_episode::RunCompletion::Open
    ));

    let notif_session = state
        .session_manager
        .get_or_create(alice_id, "notifications:alice");

    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx
        .send(RunTrigger {
            agent_id: alice_id,
            session_id: notif_session.id,
            input: "DM ended marker".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: bob_id,
                from_name: "bob".to_string(),
                reason: ConversationEndReason::Errored {
                    message: "LLM rate limit exceeded".to_string(),
                    interrupted: true,
                },
                self_notification: false,
                source_session_id: None,
            },
            context_id: notif_session.context_id.clone(),
        })
        .await
        .unwrap();
    drop(test_tx);
    super::notifications::run_trigger_loop(test_rx, state.clone()).await;

    let runs = state.run_manager.list_by_session(job_session_id, 10);
    assert_eq!(
        runs.len(),
        1,
        "the job's continuation must survive an interrupted DM end; got {runs:?}"
    );
    assert_eq!(
        runs[0].job_id,
        Some(job_id),
        "continuation run must carry the episode's job_id"
    );
    assert!(
        state
            .run_manager
            .list_by_session(notif_session.id, 10)
            .is_empty(),
        "the trigger's own target still gets nothing — the episode override \
         reroutes, it does not duplicate"
    );

    shutdown_token.cancel();
}

/// #1205 (Tim's S2 on PR #1202): TWO open episodes of the same agent both
/// pending the same deterministic DM session — one `ConversationEnded`
/// trigger must produce a continuation run for BOTH episodes (each on its
/// own job session, stamped with its own job id). Pre-fix only the
/// HashMap-order winner resolved; the loser's episode hung until the 4h
/// deadline backstop.
#[tokio::test]
async fn conversation_ended_resolves_all_episodes_pending_on_same_dm() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, _bob_id) = seed_alice_bob(&state);

    // Two jobs for alice, each with an open episode pending on the SAME
    // deterministic alice<->bob DM session.
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    let mut jobs = Vec::new();
    for prompt in ["ask bob (job 1)", "ask bob (job 2)"] {
        let job_id = create_recurring_job(&state, alice_id, prompt);
        let job_session_id = state
            .session_manager
            .get_or_create(alice_id, format!("job_{}", job_id.0))
            .id;
        let turn1 = RunId::new();
        state
            .job_episodes
            .open(job_id, job_session_id, alice_id, turn1);
        assert!(matches!(
            state
                .job_episodes
                .on_run_complete(job_id, turn1, vec![dm_session_id], vec![]),
            super::job_episode::RunCompletion::Open
        ));
        jobs.push((job_id, job_session_id));
    }

    // A single ConversationEnded trigger for alice (the fallback target is
    // her invisible notifications session).
    let notif_session = state
        .session_manager
        .get_or_create(alice_id, "notifications:alice");
    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx
        .send(RunTrigger {
            agent_id: alice_id,
            session_id: notif_session.id,
            input: "DM ended marker".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: _bob_id,
                from_name: "bob".to_string(),
                reason: ConversationEndReason::Ignored,
                self_notification: false,
                source_session_id: None,
            },
            context_id: notif_session.context_id.clone(),
        })
        .await
        .unwrap();
    drop(test_tx);
    super::notifications::run_trigger_loop(test_rx, state.clone()).await;

    // BOTH job sessions received a continuation run stamped with their own
    // job id — no episode is left waiting for the deadline.
    for (job_id, job_session_id) in &jobs {
        let runs = state.run_manager.list_by_session(*job_session_id, 10);
        assert!(
            !runs.is_empty(),
            "job {job_id:?} must receive a continuation run on its job session"
        );
        assert_eq!(
            runs[0].job_id,
            Some(*job_id),
            "continuation run must carry its own episode's job_id"
        );
    }
    // The fallback target received nothing.
    assert!(
        state
            .run_manager
            .list_by_session(notif_session.id, 10)
            .is_empty(),
        "the notifications: fallback target must NOT receive a run when \
         episodes resolved"
    );
    // Neither episode still has a pending DM entry.
    for (job_id, _) in &jobs {
        if let Some(snap) = state.job_episodes.snapshot(*job_id) {
            assert_eq!(
                snap["pending_dms"], 0,
                "job {job_id:?} must have no pending DM left after the end signal"
            );
        }
        // A `None` snapshot means the continuation already executed and
        // closed the episode — equally correct (nothing pending).
    }

    shutdown_token.cancel();
}

/// #1207 (Tim's S4 on PR #1202): an episode continuation's run id must be
/// recorded on the episode BEFORE `enqueue_triggered_run` returns — it is
/// noted right after `insert_run`, before the run is handed to the agent
/// queue. Pre-fix the callers noted the id only after the enqueue call
/// returned, so an instantly-terminating continuation could close the
/// episode with its final run id missing from `runs` (wrong `turns` count
/// and completion-card deep-link — cosmetic, but pinned here so the
/// ordering can't regress).
#[tokio::test]
async fn triggered_run_id_noted_on_episode_before_enqueue_returns() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();

    let job_id = create_recurring_job(&state, agent_id, "long task");
    let job_context = format!("job_{}", job_id.0);
    let job_session_id = state
        .session_manager
        .get_or_create(agent_id, &job_context)
        .id;
    // Open the episode and park a pending-subagent entry (turn-1 "opened"
    // a subagent that never completes in this test). The non-empty pending
    // set makes quiescence STRUCTURALLY impossible, so the episode stays
    // open even if the enqueued continuation executes to termination
    // before the assertions below. (Tim's S1 on PR #1210: `in_flight_runs`
    // alone would NOT guarantee that — `enqueue_low` spawns a real handler
    // task, and only the current-thread test runtime plus the absence of
    // an await between the enqueue and the snapshot kept the pre-hardened
    // shape deterministic.)
    let turn1 = RunId::new();
    state
        .job_episodes
        .open(job_id, job_session_id, agent_id, turn1);
    assert!(matches!(
        state.job_episodes.on_run_complete(
            job_id,
            turn1,
            vec![],
            vec![(uuid::Uuid::new_v4(), SessionId::new())]
        ),
        super::job_episode::RunCompletion::Open
    ));

    let run_id = super::notifications::enqueue_triggered_run(
        &state,
        agent_id,
        job_session_id,
        "continuation input".to_string(),
        job_context,
        "notification:dm_ended:test".to_string(),
        false,
        Some(job_id),
        None,
    )
    .await
    .expect("run must be created (job is not cancelled)");

    // The contract under test: the run id is already on the episode when
    // enqueue_triggered_run returns.
    let snap = state
        .job_episodes
        .snapshot(job_id)
        .expect("episode must still be open (pending subagent parked)");
    assert_eq!(
        snap["runs"], 2,
        "the continuation run id must be noted on the episode before \
         enqueue_triggered_run returns (turn1 + continuation)"
    );
    // And the run record carries the job stamp (#1198), so
    // cancel_runs_for_job / the episode exit hook cover it.
    assert_eq!(
        state.run_manager.get_run(run_id).and_then(|r| r.job_id),
        Some(job_id),
        "continuation run must be job-stamped"
    );

    shutdown_token.cancel();
}

/// The agent-match guard: the PEER's `ConversationEnded` trigger (same DM
/// session, different agent) must NOT consume the episode's pending entry —
/// its notification run keeps the pre-#1198 routing.
#[tokio::test]
async fn conversation_ended_for_peer_does_not_touch_episode() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    let job_id = create_recurring_job(&state, alice_id, "ask bob");
    let job_session_id = state
        .session_manager
        .get_or_create(alice_id, format!("job_{}", job_id.0))
        .id;
    let turn1 = RunId::new();
    state
        .job_episodes
        .open(job_id, job_session_id, alice_id, turn1);
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    state
        .job_episodes
        .on_run_complete(job_id, turn1, vec![dm_session_id], vec![]);

    // BOB's trigger for the same conversation end.
    let bob_notif = state
        .session_manager
        .get_or_create(bob_id, "notifications:bob");
    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx
        .send(RunTrigger {
            agent_id: bob_id,
            session_id: bob_notif.id,
            input: "DM ended marker".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: alice_id,
                from_name: "alice".to_string(),
                reason: ConversationEndReason::Ignored,
                self_notification: false,
                source_session_id: None,
            },
            context_id: bob_notif.context_id.clone(),
        })
        .await
        .unwrap();
    drop(test_tx);
    super::notifications::run_trigger_loop(test_rx, state.clone()).await;

    // Bob's run stays on his own target; alice's pending DM is untouched.
    let runs = state.run_manager.list_by_session(bob_notif.id, 10);
    assert!(!runs.is_empty(), "bob's notification run must be created");
    assert_eq!(runs[0].job_id, None, "bob's run must not be job-stamped");
    let snap = state
        .job_episodes
        .snapshot(job_id)
        .expect("alice's episode must still be open");
    assert_eq!(
        snap["pending_dms"], 1,
        "the peer's trigger must not consume the episode's pending DM"
    );

    shutdown_token.cancel();
}

/// #1198 step 5 (subagent side): a `SubagentCompletion` whose task is
/// pending on an open episode resolves it — the notification run (already
/// routed to the parent == job session) is stamped with the job id, and the
/// #1041 marker-before-SSE ordering is preserved.
#[tokio::test]
async fn subagent_completion_resolves_episode_and_stamps_job_id() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();

    let job_id = create_recurring_job(&state, agent_id, "spawn researcher");
    let job_context = format!("job_{}", job_id.0);
    let job_session_id = state
        .session_manager
        .get_or_create(agent_id, &job_context)
        .id;
    let turn1 = RunId::new();
    state
        .job_episodes
        .open(job_id, job_session_id, agent_id, turn1);
    let task_id = TaskId::new();
    let subagent_session_id = state
        .session_manager
        .get_or_create(AgentId::new(), "subagent")
        .id;
    assert!(matches!(
        state.job_episodes.on_run_complete(
            job_id,
            turn1,
            vec![],
            vec![(task_id.0, subagent_session_id)]
        ),
        super::job_episode::RunCompletion::Open
    ));

    let (test_tx, test_rx) = mpsc::unbounded_channel();
    test_tx
        .send(SubagentCompletion {
            task_id,
            subagent_name: Some("researcher".to_string()),
            status: TaskStatus::Completed,
            summary: "Done.".to_string(),
            parent_session_id: job_session_id,
            parent_agent_id: agent_id,
            subagent_session_id,
            task_description: Some("investigate".to_string()),
            tool_count: Some(1),
            duration_ms: Some(10),
            token_usage: None,
            parent_tool_invocation_id: None,
        })
        .unwrap();
    drop(test_tx);
    super::notifications::completion_notification_loop(test_rx, state.clone()).await;

    // The continuation run is on the job session and job-stamped.
    let runs = state.run_manager.list_by_session(job_session_id, 10);
    assert!(!runs.is_empty(), "continuation run must exist");
    assert_eq!(runs[0].job_id, Some(job_id));

    // The pending entry was consumed (episode open with 0 pending, or —
    // if the enqueued continuation already failed fast in the background —
    // closed entirely). Either way no pending subagent may remain.
    if let Some(snap) = state.job_episodes.snapshot(job_id) {
        assert_eq!(snap["pending_subagents"], 0);
    }

    // #1041 ordering invariant untouched: the marker exists in history.
    let history = state.session_manager.get_history(job_session_id).unwrap();
    assert!(
        history.iter().any(|m| {
            m.metadata
                .as_ref()
                .and_then(|meta| meta.get("type"))
                .and_then(|v| v.as_str())
                == Some("subagent_completion")
        }),
        "subagent_completion marker must be persisted"
    );

    shutdown_token.cancel();
}

/// D5 deadline expiry: `close_episode(_, _, timed_out = true)` completes
/// the job with the deadline note and detached-count while leaving the
/// pending work untouched (detach-and-complete — nothing is cancelled).
/// The sweep's drain half is covered by the tracker unit test
/// (`take_expired_drains_past_deadline_episodes_with_pending_work`).
#[tokio::test]
async fn deadline_close_detaches_and_completes_with_note() {
    let (state, shutdown_token, _cr, mut trigger_rx, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let web_session_id = state.session_manager.get_or_create(agent_id, "web").id;
    let job_id = create_recurring_job(&state, agent_id, "slow conversation");
    let job_session_id = state
        .session_manager
        .get_or_create(agent_id, format!("job_{}", job_id.0))
        .id;

    // A completed turn-1 run so the card has a real run to read.
    let mut run = Run::for_job(job_session_id, agent_id, "slow conversation".into(), job_id);
    assert!(run.mark_running());
    assert!(run.mark_completed("asked bob, waiting".into(), Default::default()));
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run);

    // An expired episode with one pending DM (as the sweep would drain it).
    let pending_dm = SessionId::new();
    let episode = super::job_episode::JobEpisode {
        job_id,
        session_id: job_session_id,
        agent_id,
        started_at: chrono::Utc::now() - chrono::Duration::minutes(5),
        deadline: std::time::Instant::now(),
        pending_dms: std::iter::once(pending_dm).collect(),
        pending_subagents: std::collections::HashMap::new(),
        in_flight_runs: 0,
        runs: vec![run_id],
        finished_runs: std::collections::HashSet::new(),
        dm_total: 1,
        subagent_total: 0,
        catch_up_queued: false,
    };

    super::notifications::close_episode(&state, episode, true).await;

    let (text, meta) = job_marker_from(&state, web_session_id);
    assert!(
        text.contains("[Episode deadline reached after 4h — 1 pending task(s) detached]"),
        "deadline close must carry the detach note, got: {text}"
    );
    assert_eq!(meta["episode"]["timed_out"], true);
    assert_eq!(meta["episode"]["detached"], 1);

    // Detach-and-complete: the pending DM was NOT ended — no
    // ConversationEnded trigger was emitted.
    assert!(
        trigger_rx.try_recv().is_err(),
        "deadline close must not end the pending DM"
    );

    // The job completed + re-armed normally.
    let job = state.job_store.get(job_id).unwrap();
    assert!(job.last_run_at.is_some());
    assert_eq!(job.status(), alms_core::JobStatus::Active);

    shutdown_token.cancel();
}

/// D6 coalesced catch-up: an episode that outlived >=1 cron tick fires
/// exactly one immediate catch-up at close (`next_run_at ~= now`), while an
/// episode that did NOT outlive a tick re-arms for the next future tick.
#[tokio::test]
async fn catch_up_fires_when_episode_outlived_cron_tick() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let _web = state.session_manager.get_or_create(agent_id, "web");

    // Every-minute cron so "outlived a tick" needs only minutes of skew.
    let make_job = |prompt: &str| {
        state
            .job_store
            .create(alms_core::job::CreateJobRequest {
                agent_id,
                prompt: prompt.to_string(),
                schedule: alms_core::job::JobSchedule::Recurring {
                    cron: "* * * * *".to_string(),
                },
            })
            .unwrap()
            .id
    };
    let make_episode = |job_id: alms_core::JobId, started_at: chrono::DateTime<chrono::Utc>| {
        let job_session_id = state
            .session_manager
            .get_or_create(agent_id, format!("job_{}", job_id.0))
            .id;
        super::job_episode::JobEpisode {
            job_id,
            session_id: job_session_id,
            agent_id,
            started_at,
            deadline: std::time::Instant::now(),
            pending_dms: std::collections::HashSet::new(),
            pending_subagents: std::collections::HashMap::new(),
            in_flight_runs: 0,
            runs: vec![RunId::new()],
            finished_runs: std::collections::HashSet::new(),
            dm_total: 0,
            subagent_total: 0,
            catch_up_queued: false,
        }
    };

    // Case 1: episode outlived >=1 tick (started 3 minutes ago).
    let missed_job = make_job("missed a tick");
    let episode = make_episode(
        missed_job,
        chrono::Utc::now() - chrono::Duration::minutes(3),
    );
    super::notifications::close_episode(&state, episode, false).await;
    let job = state.job_store.get(missed_job).unwrap();
    let next = job.next_run_at.expect("catch-up must set next_run_at");
    assert!(
        next <= chrono::Utc::now() + chrono::Duration::seconds(1),
        "coalesced catch-up must be due immediately, got {next}"
    );

    // Case 2: no tick elapsed (started just now) — normal future re-arm.
    let ontime_job = make_job("on time");
    let episode = make_episode(ontime_job, chrono::Utc::now());
    super::notifications::close_episode(&state, episode, false).await;
    let job = state.job_store.get(ontime_job).unwrap();
    let next = job.next_run_at.expect("re-arm must set next_run_at");
    assert!(
        next > chrono::Utc::now(),
        "no missed tick — next firing must be in the future, got {next}"
    );

    shutdown_token.cancel();
}

/// D7 cancellation teardown: `DELETE /jobs/{id}` on a job with an open
/// episode ends its pending DM with `UserCancelled` (dm_ended marker +
/// ConversationEnded triggers) and removes the episode.
#[tokio::test]
async fn cancel_job_teardown_ends_pending_dms() {
    let (state, shutdown_token, _cr, mut trigger_rx, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    let job_id = create_recurring_job(&state, alice_id, "ask bob then wait");
    let job_context = format!("job_{}", job_id.0);
    let job_session_id = state
        .session_manager
        .get_or_create(alice_id, &job_context)
        .id;

    // A REAL conversation via the bus (from the job session — a valid
    // source post-#1198) so end_conversation has a live pair to end.
    let receipt = state
        .message_bus
        .send(
            "alice",
            alice_id,
            "bob",
            bob_id,
            "please review X",
            Some(job_session_id),
        )
        .await
        .expect("send must succeed");
    let dm_session_id = receipt.session_id;
    let _ = trigger_rx.try_recv(); // drain bob's DM trigger

    // Open episode with that DM pending.
    let turn1 = RunId::new();
    state
        .job_episodes
        .open(job_id, job_session_id, alice_id, turn1);
    state
        .job_episodes
        .on_run_complete(job_id, turn1, vec![dm_session_id], vec![]);

    // DELETE /jobs/{id}.
    let response = crate::jobs::cancel_job(
        axum::extract::State(state.clone()),
        axum::extract::Path(job_id),
    )
    .await;
    use axum::response::IntoResponse as _;
    assert_eq!(
        response.into_response().status(),
        axum::http::StatusCode::OK
    );

    // Episode gone.
    assert!(state.job_episodes.snapshot(job_id).is_none());

    // The DM was ended with UserCancelled: dm_ended marker on the DM session.
    let history = state.session_manager.get_history(dm_session_id).unwrap();
    let ended = history
        .iter()
        .find(|m| {
            m.metadata
                .as_ref()
                .and_then(|meta| meta.get("message_type"))
                .and_then(|v| v.as_str())
                == Some("dm_ended")
        })
        .expect("teardown must write a dm_ended marker to the DM session");
    assert_eq!(
        ended
            .metadata
            .as_ref()
            .unwrap()
            .get("reason")
            .and_then(|v| v.as_str()),
        Some("user_cancelled"),
        "teardown must end the DM with the UserCancelled reason"
    );

    // And the peer got a ConversationEnded trigger.
    let mut saw_conversation_ended = false;
    while let Ok(trigger) = trigger_rx.try_recv() {
        if matches!(trigger.source, MessageSource::ConversationEnded { .. }) {
            saw_conversation_ended = true;
        }
    }
    assert!(
        saw_conversation_ended,
        "teardown must emit ConversationEnded trigger(s)"
    );

    shutdown_token.cancel();
}

/// #1206 (Tim's S3 on PR #1202): `DELETE /jobs` teardown spawns
/// notification runs asynchronously — the DM-sender self-notification
/// routed back onto the job session (via D3 source-session routing) and the
/// `SubagentCompletion(Cancelled)` notification whose parent session IS the
/// job session. Both are created AFTER `cancel_runs_for_job` already swept,
/// so pre-fix they were unstamped orphans that each burned an LLM turn
/// post-kill. Post-fix they are suppressed at the source: after the
/// teardown-emitted signals are processed, NO live/queued run exists for
/// the job.
///
/// The suppression must stay SCOPED to the cancelled job's context: with the
/// operator-cancel intent registered, a trigger targeting a non-job context is
/// still allowed to create its run. That is asserted at the end of this test
/// on a *concluded* end, which is the only end class that still buys a turn
/// since #1258. (Pre-#1258 the peer's `UserCancelled` ended-notification
/// carried the scoping guard; it is an interrupted end now and spends
/// nothing, so it can no longer distinguish "scoped correctly" from
/// "suppressed by #1258".)
#[tokio::test]
async fn cancel_job_teardown_leaves_no_runs_for_the_job() {
    let (state, shutdown_token, _cr, mut trigger_rx, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    let job_id = create_recurring_job(&state, alice_id, "ask bob and spawn researcher");
    let job_context = format!("job_{}", job_id.0);
    let job_session_id = state
        .session_manager
        .get_or_create(alice_id, &job_context)
        .id;

    // A real conversation via the bus, sent FROM the job session (a valid
    // source post-#1198/D3) — so the teardown's end_conversation emits the
    // sender self-notification trigger targeting the JOB session.
    let receipt = state
        .message_bus
        .send(
            "alice",
            alice_id,
            "bob",
            bob_id,
            "please review X",
            Some(job_session_id),
        )
        .await
        .expect("send must succeed");
    let dm_session_id = receipt.session_id;
    let _ = trigger_rx.try_recv(); // drain bob's DM-delivery trigger

    // A pending background subagent on the same episode.
    let task_id = TaskId::new();
    let sub_session_id = state
        .session_manager
        .get_or_create(AgentId::new(), "subagent")
        .id;

    // Open the episode with the DM AND the subagent pending.
    let turn1 = RunId::new();
    state
        .job_episodes
        .open(job_id, job_session_id, alice_id, turn1);
    assert!(matches!(
        state.job_episodes.on_run_complete(
            job_id,
            turn1,
            vec![dm_session_id],
            vec![(task_id.0, sub_session_id)]
        ),
        super::job_episode::RunCompletion::Open
    ));

    // DELETE /jobs/{id}.
    let response = crate::jobs::cancel_job(
        axum::extract::State(state.clone()),
        axum::extract::Path(job_id),
    )
    .await;
    use axum::response::IntoResponse as _;
    assert_eq!(
        response.into_response().status(),
        axum::http::StatusCode::OK
    );
    assert!(state.job_episodes.snapshot(job_id).is_none());

    // Process the teardown-emitted ConversationEnded triggers exactly as
    // production's run_trigger_loop would — alice's self-notification
    // (targeting the job session) and bob's peer notification.
    let (replay_tx, replay_rx) = mpsc::channel(8);
    let mut teardown_triggers = 0;
    while let Ok(trigger) = trigger_rx.try_recv() {
        if matches!(trigger.source, MessageSource::ConversationEnded { .. }) {
            teardown_triggers += 1;
            replay_tx.send(trigger).await.unwrap();
        }
    }
    assert!(
        teardown_triggers >= 2,
        "teardown must emit ConversationEnded triggers for sender and peer, \
         got {teardown_triggers}"
    );
    drop(replay_tx);
    super::notifications::run_trigger_loop(replay_rx, state.clone()).await;

    // Process the SubagentCompletion(Cancelled) the teardown's subagent
    // cancel produces, exactly as production's completion loop would.
    let (test_tx, test_rx) = mpsc::unbounded_channel();
    test_tx
        .send(SubagentCompletion {
            task_id,
            subagent_name: Some("researcher".to_string()),
            status: TaskStatus::Cancelled,
            summary: "Cancelled.".to_string(),
            parent_session_id: job_session_id,
            parent_agent_id: alice_id,
            subagent_session_id: sub_session_id,
            task_description: Some("investigate".to_string()),
            tool_count: Some(0),
            duration_ms: Some(5),
            token_usage: None,
            parent_tool_invocation_id: None,
        })
        .unwrap();
    drop(test_tx);
    super::notifications::completion_notification_loop(test_rx, state.clone()).await;

    // THE invariant: DELETE /jobs leaves no live/queued run for the job —
    // neither job-stamped nor unstamped-on-the-job-session.
    let job_runs = state.run_manager.list_by_session(job_session_id, 50);
    assert!(
        job_runs.is_empty(),
        "no run may be spawned on the cancelled job's session by teardown \
         signals; got {job_runs:?}"
    );

    // #1258: teardown ends the DM with `UserCancelled` — an INTERRUPTED end,
    // which no longer buys an LLM turn anywhere. The peer therefore gets no
    // notification run either; `DELETE /jobs` spends nothing.
    //
    // (Pre-#1258 this asserted the opposite, as a scoping guard that the #1206
    // job-session suppression had not eaten a legitimate notification. The
    // guard is preserved below in the form that still holds: suppressing the
    // RUN must not suppress the RECORD of the end. The positive
    // "notifications: runs are still created" case now lives on a concluded
    // end — see `notification_stays_on_invisible_session_when_no_source`.)
    let bob_notif_session = SessionId::deterministic("notifications:bob");
    let bob_runs = state.run_manager.list_by_session(bob_notif_session, 10);
    assert!(
        bob_runs.is_empty(),
        "an operator-cancelled DM end must not spend a turn on the peer's \
         notification session; got {bob_runs:?}"
    );

    // ...but the end is still RECORDED: the shared DM session carries the
    // `dm_ended` marker, so neither agent believes the conversation is open
    // and a reader of that session sees why it stopped.
    let dm_messages = state
        .session_manager
        .get_history(dm_session_id)
        .expect("DM session must exist");
    assert!(
        dm_messages.iter().any(|m| {
            m.metadata
                .as_ref()
                .and_then(|meta| meta.get("message_type"))
                .and_then(|v| v.as_str())
                == Some("dm_ended")
        }),
        "teardown must still write the dm_ended marker; got {dm_messages:?}"
    );

    // #1206 scoping guard (Tim's S4 on PR #1267). The assertions above are all
    // about runs NOT being created, so on their own they cannot tell a
    // correctly scoped suppression apart from one that has widened to "any
    // trigger while an operator-cancelled job exists". Drive one more trigger
    // through the SAME state — `operator_cancelled_jobs` still holds this job
    // — targeting a NON-job context, and require that it does create its run.
    //
    // It has to be a CONCLUDED end: since #1258 an interrupted one buys no
    // turn regardless of scoping, which is exactly why the peer's
    // `UserCancelled` notification above stopped being able to carry this
    // guard.
    let (scope_tx, scope_rx) = mpsc::channel(1);
    scope_tx
        .send(RunTrigger {
            agent_id: bob_id,
            session_id: bob_notif_session,
            input: "DM ended".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: alice_id,
                from_name: "alice".to_string(),
                reason: ConversationEndReason::Ignored,
                self_notification: false,
                source_session_id: None,
            },
            context_id: "notifications:bob".to_string(),
        })
        .await
        .unwrap();
    drop(scope_tx);
    super::notifications::run_trigger_loop(scope_rx, state.clone()).await;

    let bob_runs_after = state.run_manager.list_by_session(bob_notif_session, 10);
    assert!(
        !bob_runs_after.is_empty(),
        "the #1206 job-session suppression must stay scoped to the cancelled \
         job's own context — a concluded DM end on notifications:bob must \
         still create its run while that job is registered as \
         operator-cancelled; got {bob_runs_after:?}"
    );

    shutdown_token.cancel();
}
/// A trigger that began just before `DELETE /jobs` may already have inserted
/// an un-stamped (D5-style) run on the job session. The cancellation sweep
/// must still cancel its registered token; otherwise that continuation can
/// be queued after the DELETE and spend a turn despite operator intent.
#[tokio::test]
async fn cancel_job_cancels_unstamped_run_on_its_job_session() {
    use axum::{extract::Path, extract::State, response::IntoResponse};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let job_id = create_recurring_job(&state, agent_id, "race a terminal DM trigger");
    let job_session_id = state
        .session_manager
        .get_or_create(agent_id, format!("job_{}", job_id.0))
        .id;

    // This is intentionally not job-stamped: it models a terminal trigger
    // that no longer resolved to a live episode, so `cancel_runs_for_job`
    // alone cannot find it.
    let run = Run::new(job_session_id, agent_id, "late trigger".to_string());
    let run_id = run.run_id;
    let token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, token.clone());
    let _ = state.run_manager.insert_run(run);

    let response = crate::jobs::cancel_job(State(state.clone()), Path(job_id))
        .await
        .into_response();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert!(
        token.is_cancelled(),
        "operator cancellation must cancel an un-stamped run on its job session"
    );

    shutdown_token.cancel();
}

/// The #1206 suppression must key on explicit operator-cancel intent. Scenario:
/// a one-shot dispatches a background subagent, hits the four-hour deadline,
/// and closes as `Completed`; the subagent keeps running and later completes.
/// Its late completion lands on the job session and must still produce the
/// documented orphan notification run.
#[tokio::test]
async fn spent_one_shot_detached_subagent_completion_not_suppressed() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();

    let job_id = create_once_job(&state, agent_id, "one shot with detached subagent");
    let job_context = format!("job_{}", job_id.0);
    let job_session_id = state
        .session_manager
        .get_or_create(agent_id, &job_context)
        .id;

    // The D5 deadline close already happened: episode gone (never opened
    // here — resolve must miss), job recorded as spent.
    spend_one_shot(&state, job_id);

    // The detached subagent completes for REAL (Completed, not Cancelled).
    let (test_tx, test_rx) = mpsc::unbounded_channel();
    test_tx
        .send(SubagentCompletion {
            task_id: TaskId::new(),
            subagent_name: Some("researcher".to_string()),
            status: TaskStatus::Completed,
            summary: "Late detached result.".to_string(),
            parent_session_id: job_session_id,
            parent_agent_id: agent_id,
            subagent_session_id: state
                .session_manager
                .get_or_create(AgentId::new(), "subagent")
                .id,
            task_description: Some("investigate".to_string()),
            tool_count: Some(3),
            duration_ms: Some(50),
            token_usage: None,
            parent_tool_invocation_id: None,
        })
        .unwrap();
    drop(test_tx);
    super::notifications::completion_notification_loop(test_rx, state.clone()).await;

    // The late result IS delivered: a notification run exists on the job
    // session (unstamped — the documented D5 orphan-run shape).
    let runs = state.run_manager.list_by_session(job_session_id, 10);
    assert!(
        !runs.is_empty(),
        "a spent one-shot's detached subagent result must NOT be suppressed \
         (suppression keys on operator intent, not lifecycle status)"
    );
    assert_eq!(
        runs[0].job_id, None,
        "the late orphan run is unstamped (episode long gone)"
    );

    shutdown_token.cancel();
}

/// The DM half of the Codex P2 scenario: a one-shot's deadline-detached DM
/// ends AFTER the D5 close (job recorded spent = `Cancelled`). The late
/// `ConversationEnded`'s D3 fallback routes the notification run onto the
/// job session (the sender's source session) — it must NOT be suppressed.
#[tokio::test]
async fn spent_one_shot_detached_dm_ended_not_suppressed() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    let job_id = create_once_job(&state, alice_id, "one shot that DMs bob");
    let job_context = format!("job_{}", job_id.0);
    let job_session_id = state
        .session_manager
        .get_or_create(alice_id, &job_context)
        .id;

    spend_one_shot(&state, job_id);

    // The late ConversationEnded for the JOB AGENT, in the D3 fallback
    // shape: the bus routed the sender self-notification to the source
    // session — the job session. The episode is long gone, so resolve_dm
    // misses and the pre-#1198 routing (this trigger's target) applies.
    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx
        .send(RunTrigger {
            agent_id: alice_id,
            session_id: job_session_id,
            input: "DM ended marker".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: bob_id,
                from_name: "bob".to_string(),
                reason: ConversationEndReason::Ignored,
                self_notification: false,
                source_session_id: Some(job_session_id),
            },
            context_id: job_context.clone(),
        })
        .await
        .unwrap();
    drop(test_tx);
    super::notifications::run_trigger_loop(test_rx, state.clone()).await;

    let runs = state.run_manager.list_by_session(job_session_id, 10);
    assert!(
        !runs.is_empty(),
        "a spent one-shot's detached DM-ended notification must NOT be \
         suppressed (suppression keys on operator intent, not lifecycle status)"
    );
    assert_eq!(
        runs[0].job_id, None,
        "the late orphan run is unstamped (episode long gone)"
    );

    shutdown_token.cancel();
}

/// S1 (#1202, Tim's review): the widened cancel-vs-close TOCTOU.
/// `close_episode` reads the job (Cancelled pre-check passes), AWAITS the
/// completion fanout, then records + re-arms — a `DELETE /jobs` landing in
/// that window must NOT be overwritten back to `Active` (a cancelled
/// recurring job would resurrect and re-arm).
///
/// Reproduces the lost race deterministically at the exact layer it
/// happens: take the `job` snapshot while the job is live (as
/// `close_episode` does before the fanout), land the cancel, then run the
/// post-fanout `record_and_rearm` with the STALE snapshot — on the
/// catch-up branch, the resurrection-prone write (`Active`, `next_run_at =
/// now`). The store's absorbing-`Cancelled` guard must refuse the write
/// and the `Recorded` gate must skip the scheduler re-arm.
#[tokio::test]
async fn delete_jobs_interleaved_with_close_cannot_resurrect_job() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();

    // Every-minute recurring job; episode started 3 minutes ago so
    // record_and_rearm takes the D6 catch-up branch (Active + due-now —
    // the exact write that resurrected pre-fix).
    let job_id = state
        .job_store
        .create(alms_core::job::CreateJobRequest {
            agent_id,
            prompt: "racy job".to_string(),
            schedule: alms_core::job::JobSchedule::Recurring {
                cron: "* * * * *".to_string(),
            },
        })
        .unwrap()
        .id;

    // The stale snapshot close_episode holds across its fanout await.
    let job_snapshot = state.job_store.get(job_id).expect("job is live");
    assert_ne!(job_snapshot.status(), alms_core::JobStatus::Cancelled);

    let job_session_id = state
        .session_manager
        .get_or_create(agent_id, format!("job_{}", job_id.0))
        .id;
    let episode = super::job_episode::JobEpisode {
        job_id,
        session_id: job_session_id,
        agent_id,
        started_at: chrono::Utc::now() - chrono::Duration::minutes(3),
        deadline: std::time::Instant::now(),
        pending_dms: std::collections::HashSet::new(),
        pending_subagents: std::collections::HashMap::new(),
        in_flight_runs: 0,
        runs: vec![RunId::new()],
        finished_runs: std::collections::HashSet::new(),
        dm_total: 0,
        subagent_total: 0,
        catch_up_queued: false,
    };

    // The interleaved DELETE /jobs lands INSIDE close_episode's window
    // (after the pre-check read above, before the record write below).
    assert_eq!(state.job_store.cancel(job_id).unwrap(), Some(true));

    // close_episode's post-fanout tail runs with the stale snapshot.
    super::notifications::record_and_rearm(&state, &job_snapshot, &episode, false).await;

    // The job must stay Cancelled — not resurrected, not re-armed.
    let job = state.job_store.get(job_id).expect("job still in store");
    assert_eq!(
        job.status(),
        alms_core::JobStatus::Cancelled,
        "a DELETE interleaved with episode close must leave the job Cancelled (S1)"
    );
    assert_eq!(
        job.next_run_at, None,
        "the refused write must not re-arm next_run_at"
    );
    assert_eq!(
        job.last_run_at, None,
        "the refused write must not touch last_run_at"
    );

    shutdown_token.cancel();
}

/// The full recurring job entity `record_and_rearm` takes by reference.
///
/// [`create_recurring_job`]'s daily cron keeps the next occurrence
/// unambiguously in the future, so `record_and_rearm` deterministically takes
/// the normal re-arm branch rather than the D6 coalesced catch-up.
fn future_recurring_job(state: &AppState, agent_id: AgentId) -> alms_core::job::Job {
    let job_id = create_recurring_job(state, agent_id, "durable re-arm");
    state.job_store.get(job_id).expect("job was just created")
}

/// #1233: a *transient* persistence failure at episode close must not stop a
/// recurring job. `JobStore::transition_job` persists before committing to
/// memory, so a failed `save_job` advances neither — pre-fix the `Err` arm
/// was a bare `error!` and the job was left `Active` with `next_run_at`
/// pinned to the tick that already fired and no `schedule_once` issued.
/// The bounded retry budget must absorb the failure and close normally.
#[tokio::test]
async fn transient_persistence_failure_at_episode_close_still_rearms_the_job() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let job = future_recurring_job(&state, agent_id);
    let episode = completed_episode(&state, agent_id, job.id);

    // Exactly one injected failure — inside the budget.
    state.job_store.inject_next_persistence_failure();
    super::notifications::record_and_rearm(&state, &job, &episode, false).await;

    let recorded = state.job_store.get(job.id).expect("job still in store");
    assert_eq!(recorded.status(), alms_core::JobStatus::Active);
    assert!(
        recorded.last_run_at.is_some(),
        "the retried write must durably record the firing"
    );
    assert!(
        recorded.next_run_at.is_some(),
        "the recurring job must be re-armed durably after the retry succeeds"
    );
    assert_eq!(
        state.job_store.rearm_failures_total(),
        0,
        "a failure absorbed inside the budget is not a degraded close"
    );
    assert_eq!(
        state.scheduler.pending_count().await,
        1,
        "the scheduler must hold the job's next firing"
    );

    shutdown_token.cancel();
}

/// #1233: when the persistence failure outlives the retry budget, the job
/// must still not silently stall. Nothing durable can be written — the store
/// is exactly what is failing — so the guarantee is: re-arm in memory at the
/// next cron occurrence, and count the degradation in
/// `job_rearm_failures_total` so `GET /operations/metrics` shows it.
#[tokio::test]
async fn exhausted_episode_close_retries_rearm_the_recurring_job_in_memory() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let job = future_recurring_job(&state, agent_id);
    let episode = completed_episode(&state, agent_id, job.id);

    state
        .job_store
        .inject_persistence_failures(super::notifications::JOB_REARM_MAX_ATTEMPTS);
    super::notifications::record_and_rearm(&state, &job, &episode, false).await;

    let recorded = state.job_store.get(job.id).expect("job still in store");
    assert_eq!(
        recorded.status(),
        job.status(),
        "the refused write must leave the job exactly as it was"
    );
    assert_eq!(
        recorded.last_run_at, None,
        "no partial commit: neither SQLite nor memory advanced"
    );
    assert_eq!(
        state.job_store.rearm_failures_total(),
        1,
        "the degraded close must be observable in operations metrics"
    );
    assert_eq!(
        state.scheduler.pending_count().await,
        1,
        "the job must still be armed in memory — a silent stall until the \
         next daemon restart is the defect this closes"
    );

    shutdown_token.cancel();
}

fn completed_episode(
    state: &AppState,
    agent_id: AgentId,
    job_id: alms_core::JobId,
) -> super::job_episode::JobEpisode {
    let session_id = state
        .session_manager
        .get_or_create(agent_id, format!("job_{}", job_id.0))
        .id;
    let mut run = Run::for_job(session_id, agent_id, "scheduled work".into(), job_id);
    let run_id = run.run_id;
    run.mark_running();
    assert!(run.mark_completed("done".into(), TokenUsage::default()));
    let _ = state.run_manager.insert_run(run);

    super::job_episode::JobEpisode {
        job_id,
        session_id,
        agent_id,
        started_at: chrono::Utc::now(),
        deadline: std::time::Instant::now(),
        pending_dms: std::collections::HashSet::new(),
        pending_subagents: std::collections::HashMap::new(),
        in_flight_runs: 0,
        runs: vec![run_id],
        finished_runs: std::collections::HashSet::new(),
        dm_total: 0,
        subagent_total: 0,
        catch_up_queued: false,
    }
}

fn has_job_marker(state: &AppState, session_id: SessionId, job_id: alms_core::JobId) -> bool {
    state
        .session_manager
        .get_history(session_id)
        .unwrap()
        .iter()
        .any(|message| {
            message.metadata.as_ref().is_some_and(|metadata| {
                metadata.get("type").and_then(|value| value.as_str()) == Some("job_notification")
                    && metadata.get("job_id").and_then(|value| value.as_str())
                        == Some(job_id.0.to_string().as_str())
            })
        })
}

/// Poll a future exactly once while its job gate is held. This registers the
/// mutex waiter in Tokio's FIFO queue without relying on task scheduling or
/// sleeps, making the winner order in the race tests deterministic.
fn assert_pending_on_job_gate<F: std::future::Future>(mut future: std::pin::Pin<&mut F>) {
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(
        future.as_mut().poll(&mut context).is_pending(),
        "future must wait while the job completion/cancellation gate is held"
    );
}

/// PR #1222 regression: when DELETE is first in the per-job gate queue, it
/// must make the job terminal before close observes the status. No completion
/// card may be persisted after the operator's cancellation wins.
#[tokio::test]
async fn cancel_wins_before_episode_close_without_completion_card() {
    use axum::{extract::Path, extract::State, response::IntoResponse};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let web_session_id = state.session_manager.get_or_create(agent_id, "web").id;
    let job_id = create_recurring_job(&state, agent_id, "cancel wins");
    let episode = completed_episode(&state, agent_id, job_id);

    let gate = state.job_completion_cancellation_gate(job_id);
    let guard = gate.lock().await;
    let mut cancel = Box::pin(crate::jobs::cancel_job(State(state.clone()), Path(job_id)));
    assert_pending_on_job_gate(cancel.as_mut());
    let mut close = Box::pin(super::notifications::close_episode(&state, episode, false));
    assert_pending_on_job_gate(close.as_mut());

    drop(guard);
    let response = cancel.await.into_response();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    close.await;

    assert_eq!(
        state.job_store.get(job_id).unwrap().status(),
        alms_core::JobStatus::Cancelled
    );
    assert!(
        !has_job_marker(&state, web_session_id, job_id),
        "a cancellation that wins the gate must suppress the completion card"
    );

    shutdown_token.cancel();
}

/// PR #1222 regression: when close is first in the per-job gate queue, its
/// card and record update complete atomically with respect to DELETE. The
/// subsequent cancellation is valid, but it cannot erase the visible result
/// of work that finished first.
#[tokio::test]
async fn episode_close_wins_before_cancel_and_keeps_completion_card() {
    use axum::{extract::Path, extract::State, response::IntoResponse};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let web_session_id = state.session_manager.get_or_create(agent_id, "web").id;
    let job_id = create_recurring_job(&state, agent_id, "completion wins");
    let episode = completed_episode(&state, agent_id, job_id);

    let gate = state.job_completion_cancellation_gate(job_id);
    let guard = gate.lock().await;
    let mut close = Box::pin(super::notifications::close_episode(&state, episode, false));
    assert_pending_on_job_gate(close.as_mut());
    let mut cancel = Box::pin(crate::jobs::cancel_job(State(state.clone()), Path(job_id)));
    assert_pending_on_job_gate(cancel.as_mut());

    drop(guard);
    close.await;
    let response = cancel.await.into_response();
    assert_eq!(response.status(), axum::http::StatusCode::OK);

    let (_text, metadata) = job_marker_from(&state, web_session_id);
    assert_eq!(metadata["job_id"], job_id.0.to_string());
    assert_eq!(
        state.job_store.get(job_id).unwrap().status(),
        alms_core::JobStatus::Cancelled,
        "DELETE may cancel the recurring job after its completed episode is visible"
    );

    shutdown_token.cancel();
}

/// A slow completion/cancellation arbitration on job A must not introduce
/// head-of-line blocking for an unrelated job B.
#[tokio::test]
async fn blocked_job_gate_does_not_delay_unrelated_job_cancellation() {
    use axum::{extract::Path, extract::State, response::IntoResponse};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let blocked_job_id = create_recurring_job(&state, agent_id, "blocked job");
    let independent_job_id = create_recurring_job(&state, agent_id, "independent job");

    let blocked_gate = state.job_completion_cancellation_gate(blocked_job_id);
    let blocked_guard = blocked_gate.lock().await;
    let independent_cancel =
        crate::jobs::cancel_job(State(state.clone()), Path(independent_job_id));
    let response = tokio::time::timeout(std::time::Duration::from_secs(1), independent_cancel)
        .await
        .expect("an unrelated job must not wait for another job's gate")
        .into_response();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(
        state.job_store.get(independent_job_id).unwrap().status(),
        alms_core::JobStatus::Cancelled
    );

    let gate_count = state.job_completion_cancellation_gates.len();
    let response = crate::jobs::cancel_job(State(state.clone()), Path(alms_core::JobId::new()))
        .await
        .into_response();
    assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
    assert_eq!(
        state.job_completion_cancellation_gates.len(),
        gate_count,
        "unknown job ids must not grow the per-job gate registry"
    );

    drop(blocked_guard);
    let response = crate::jobs::cancel_job(State(state.clone()), Path(blocked_job_id))
        .await
        .into_response();
    assert_eq!(response.status(), axum::http::StatusCode::OK);

    shutdown_token.cancel();
}

#[tokio::test]
async fn operational_metrics_route_exposes_live_snapshot() {
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let subscription = state.run_manager.subscribe_activity();
    let router = crate::server::routes::protected_router().with_state(state.clone());
    let response = router
        .oneshot(
            HttpRequest::get("/operations/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("metrics route should respond");
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["subscribers"]["activity"], 1);
    assert!(json["queue_saturation_rejections_total"].is_number());
    assert!(json["replay_epoch_mismatches_total"].is_number());
    assert!(json["persistence_snapshot_rejections_total"].is_number());
    assert!(json["job_dispatch_retry_exhaustions_total"].is_number());
    assert!(json["job_rearm_failures_total"].is_number());
    assert!(json["stale_run_recovery_failures_total"].is_number());
    assert!(json["job_boot_catch_ups_total"].is_number());
    assert!(json["job_bootstrap_failures_total"].is_number());
    assert!(json["persistence_rows_skipped_total"].is_number());

    // The per-table breakdown reports every known table, including the
    // zeroes, so a scraper's key set is stable across deployments — and
    // stable even with no SQLite store configured, as here (#1241).
    let by_table = json["persistence_rows_skipped_by_table"]
        .as_object()
        .expect("per-table breakdown should be an object");
    assert_eq!(
        by_table.len(),
        alms_session::sqlite::PersistenceTable::ALL.len()
    );
    for table in alms_session::sqlite::PersistenceTable::ALL {
        assert_eq!(
            by_table.get(table.as_str()),
            Some(&serde_json::json!(0)),
            "missing or non-zero entry for {}",
            table.as_str()
        );
    }

    // #1246: field degradations are a separate counter from row skips, with
    // the same stable-key-set guarantee. The two must stay distinct on the
    // wire — one means "rows the daemon cannot see", the other "rows it is
    // serving with a column it could not read".
    assert!(json["persistence_fields_degraded_total"].is_number());
    let by_field = json["persistence_fields_degraded_by_field"]
        .as_object()
        .expect("per-field breakdown should be an object");
    assert_eq!(
        by_field.len(),
        alms_session::sqlite::DegradedField::ALL.len()
    );
    for field in alms_session::sqlite::DegradedField::ALL {
        assert_eq!(
            by_field.get(field.as_str()),
            Some(&serde_json::json!(0)),
            "missing or non-zero entry for {}",
            field.as_str()
        );
        assert!(
            field.as_str().contains('.'),
            "field keys are <table>.<column> so they name the cell to inspect"
        );
    }

    drop(subscription);
    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// #1299: the post-end turn cannot re-open the conversation that just ended
// ---------------------------------------------------------------------------
//
// `MAX_DM_DEPTH` bounds ONE conversation, and `end_conversation_locked`
// clears `depths` and `expired_pairs` — so the next `send_message` between
// the same two agents restarts at depth 1. The post-end turn is where that
// re-entry is immediate and unattended: the agent has just been handed the
// transcript of the conversation that ended and is one `send_message` away
// from starting it again.
//
// `ConversationEnded` runs are `is_peer_message = false`, so before #1299
// `SendMessageTool` was registered with no fold at all. The fix gives them a
// fold of their own, keyed on the ended peer.
//
// These rows pin `apply_send_message_fold` — the whole registration decision
// as `execute_run` makes it — by *executing* the tool it hands to the runtime
// against the real `MessageBus`. "Not delivered" is therefore observed twice:
// in the tool's own result, and in the absence of the `RunTrigger` that would
// have invoked the peer and re-opened the pair.

/// Build the `send_message` tool exactly as `execute_run` would for a run
/// with the given `(is_peer_message, context_id, dm_ended_peer)`.
fn send_tool_for_run(
    state: &AppState,
    agent_id: AgentId,
    agent_name: &str,
    session_id: SessionId,
    is_peer_message: bool,
    context_id: &str,
    dm_ended_peer: Option<&str>,
) -> alms_tools::SendMessageTool {
    let sender: Arc<dyn MessageSender> = state.message_bus.clone();
    super::lifecycle::apply_send_message_fold(
        alms_tools::SendMessageTool::new(
            sender,
            agent_id,
            agent_name.to_string(),
            state.session_manager.clone(),
            session_id,
        ),
        is_peer_message,
        context_id,
        agent_name,
        dm_ended_peer,
    )
}

/// The acceptance criterion: a `ConversationEnded` run cannot `send_message`
/// the peer whose conversation just ended.
///
/// The mutation this fails on is restoring the unfolded registration — the
/// pre-#1299 `else { None }` arm in `apply_send_message_fold`. Its exact
/// effect is the control row below.
#[tokio::test]
async fn conversation_ended_run_cannot_send_message_to_the_ended_peer() {
    use alms_tools::Tool;
    let (state, shutdown_token, _cr, mut trigger_rx, _dr) = test_app_state_with_sqlite();
    let (_alice_id, bob_id) = seed_alice_bob(&state);
    // Bob's post-end turn lands on his notifications session (the
    // source-less routing) and his conversation with alice has just ended.
    let session = state
        .session_manager
        .get_or_create(bob_id, "notifications:bob");

    let tool = send_tool_for_run(
        &state,
        bob_id,
        "bob",
        session.id,
        false, // ConversationEnded runs are NOT peer messages
        "notifications:bob",
        Some("alice"),
    );

    let result = tool
        .execute(serde_json::json!({ "to": "alice", "message": "one more thing" }))
        .await
        .expect("the fold is a non-error result — the agent must not retry");

    assert_eq!(result["folded"], true);
    assert_eq!(result["delivered"], false);
    assert!(
        trigger_rx.try_recv().is_err(),
        "no RunTrigger may be emitted: a delivered send invokes alice and \
         re-opens the pair at depth 1, which is the unbounded loop #1299 closes"
    );

    shutdown_token.cancel();
}

/// Control row for the one above: with no ended peer the same send DOES
/// deliver and DOES re-open the conversation.
///
/// This is what the post-end turn did before #1299 — asserted here so the
/// fold row above cannot pass for the wrong reason (a `MessageBus` that
/// silently refused to deliver in this harness would make it vacuous).
#[tokio::test]
async fn run_with_no_ended_peer_still_delivers_to_that_agent() {
    use alms_tools::Tool;
    let (state, shutdown_token, _cr, mut trigger_rx, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);
    let session = state
        .session_manager
        .get_or_create(bob_id, "notifications:bob");

    let tool = send_tool_for_run(
        &state,
        bob_id,
        "bob",
        session.id,
        false,
        "notifications:bob",
        None, // no conversation ended — nothing to fold
    );

    let result = tool
        .execute(serde_json::json!({ "to": "alice", "message": "one more thing" }))
        .await
        .expect("an ordinary send must succeed");

    assert_eq!(result["delivered"], true);
    assert!(result.get("folded").is_none());
    let trigger = trigger_rx
        .try_recv()
        .expect("an unfolded send invokes the recipient — this is the re-open");
    assert_eq!(trigger.agent_id, alice_id);

    shutdown_token.cancel();
}

/// The post-end turn keeps every other capability it has today: the fold
/// removes exactly one recipient, not `send_message` itself. Reporting the
/// outcome to a third agent is one of the things the turn exists for.
#[tokio::test]
async fn conversation_ended_run_keeps_send_message_for_third_parties() {
    use alms_tools::Tool;
    let (state, shutdown_token, _cr, mut trigger_rx, _dr) = test_app_state_with_sqlite();
    let (_alice_id, bob_id) = seed_alice_bob(&state);
    let carol_id = seed_agent(&state, "carol");
    let session = state
        .session_manager
        .get_or_create(bob_id, "notifications:bob");

    let tool = send_tool_for_run(
        &state,
        bob_id,
        "bob",
        session.id,
        false,
        "notifications:bob",
        Some("alice"),
    );

    let result = tool
        .execute(serde_json::json!({ "to": "carol", "message": "alice and I are done" }))
        .await
        .expect("a send to a third agent must succeed");

    assert_eq!(
        result["delivered"], true,
        "only the ended peer is folded — the turn can still report elsewhere"
    );
    let trigger = trigger_rx
        .try_recv()
        .expect("the third party must actually be invoked");
    assert_eq!(trigger.agent_id, carol_id);

    shutdown_token.cancel();
}

/// The #1198 / #1205 job-episode arm.
///
/// When the end resolves an open job episode, `run_trigger_loop` reroutes the
/// continuation onto the agent's `job_*` session — and that arm is also the
/// one that survives the #1258 interrupted-end suppression, so it can be the
/// ONLY run an end produces. The fold is keyed on the ended peer carried by
/// the trigger, not on `context_id`, precisely so it still applies there: a
/// `job_*` context names no peer, and this is the arm that re-opens with
/// nobody watching.
#[tokio::test]
async fn job_episode_continuation_after_a_dm_end_still_folds_the_ended_peer() {
    use alms_tools::Tool;
    let (state, shutdown_token, _cr, mut trigger_rx, _dr) = test_app_state_with_sqlite();
    let (_alice_id, bob_id) = seed_alice_bob(&state);
    let job_context = format!("job_{}", uuid::Uuid::new_v4());
    let session = state.session_manager.get_or_create(bob_id, &job_context);

    let tool = send_tool_for_run(
        &state,
        bob_id,
        "bob",
        session.id,
        false,
        &job_context,
        Some("alice"),
    );

    let result = tool
        .execute(serde_json::json!({ "to": "alice", "message": "resuming — one more ask" }))
        .await
        .expect("the fold is a non-error result");

    assert_eq!(
        result["folded"], true,
        "a job-episode continuation of an ended DM must not re-open it either"
    );
    assert!(trigger_rx.try_recv().is_err());

    shutdown_token.cancel();
}

/// The #1154 arm is unchanged: a live peer-triggered DM turn still folds its
/// current peer, read off the `dm:` context id, with `dm_ended_peer` unset.
#[tokio::test]
async fn peer_dm_turn_still_folds_its_current_peer() {
    use alms_tools::Tool;
    let (state, shutdown_token, _cr, mut trigger_rx, _dr) = test_app_state_with_sqlite();
    let (_alice_id, bob_id) = seed_alice_bob(&state);
    let dm_context = alms_core::dm_context_id("alice", "bob");
    let session = state.session_manager.get_or_create(bob_id, &dm_context);

    let tool = send_tool_for_run(
        &state,
        bob_id,
        "bob",
        session.id,
        true, // peer-triggered DM turn
        &dm_context,
        None,
    );

    let result = tool
        .execute(serde_json::json!({ "to": "alice", "message": "replying" }))
        .await
        .expect("the fold is a non-error result");

    assert_eq!(result["folded"], true);
    assert!(trigger_rx.try_recv().is_err());

    shutdown_token.cancel();
}
