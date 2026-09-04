// SPDX-License-Identifier: Apache-2.0

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

use crate::server::AppState;
use crate::sse::SseEventData;
use crate::test_support::{AppStateWithChannels, TestAppState};
use alms_core::{AgentId, SessionId};

mod activity_feed;
mod cancellation;
mod config_resolution;
mod dm_end;
mod job_episodes;
mod notifications;
mod queue;
mod read_api;
mod run_failure;
mod subagents;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Build a minimal `AppState` suitable for integration tests.
///
/// Uses in-memory session storage (no SQLite), a dummy LLM config, and
/// fresh channels for completion/trigger/dm-event loops.
fn test_app_state() -> AppStateWithChannels {
    TestAppState::new().build_with_channels()
}

/// Build an `AppState` backed by an in-memory SQLite store.
///
/// Used by tests that need a real agent registry (e.g. peer-name -> AgentId
/// resolution in `handle_dm_run_failure`). Mirrors the channel plumbing of
/// [`test_app_state`] but threads `db_path = Some(":memory:")` into the
/// `GatewayConfig` so `session_manager.store()` returns `Some(...)`.
fn test_app_state_with_sqlite() -> AppStateWithChannels {
    TestAppState::new().in_memory_sqlite().build_with_channels()
}

/// Build an `AppState` whose LLM client runs in mock mode and is backed
/// by an in-memory SQLite store. Used by the #1045 HTTP-layer regression
/// test which needs (a) a real `Coordinator` capable of dispatching a
/// subagent to completion (mock LLM avoids the network) and (b) the full
/// gateway router so `GET /sessions/{id}/messages` exercises the actual
/// JSON serialization path the UI sees.
fn test_app_state_with_mock_llm() -> AppStateWithChannels {
    test_app_state_with_mock_llm_at(":memory:")
}

/// File-backed variant of [`test_app_state_with_mock_llm`] for tests that
/// need to reopen the SQLite database and verify restart-visible state.
fn test_app_state_with_mock_llm_at(db_path: &str) -> AppStateWithChannels {
    TestAppState::new()
        .db_path(db_path)
        .mock_llm()
        .build_with_channels()
}

/// Build an `AppState` whose LLM client points at an unreachable local
/// address with a 1-second timeout, so any `execute_run` that reaches the
/// runtime LLM call fails quickly and deterministically through the
/// generic `Err(_)` arm.  Used by the #912 follow-up regression test
/// (PR #930) that asserts the gateway lifecycle no longer writes a
/// duplicate `(run failed) ...` `kind: "error"` marker.
fn test_app_state_with_failing_llm() -> AppStateWithChannels {
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

    TestAppState::new()
        .llm_config(llm_config)
        .build_with_channels()
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
async fn test_app_state_with_hanging_llm() -> AppStateWithChannels {
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

    TestAppState::new()
        .llm_config(llm_config)
        .build_with_channels()
}

/// Seed two agents (`alice` and `bob`) into the SQLite-backed agent registry
/// so that peer-name resolution works in `handle_dm_run_failure` and
/// related lifecycle helpers.
///
/// Returns `(alice_id, bob_id)`.
fn seed_alice_bob(state: &AppState) -> (AgentId, AgentId) {
    use alms_core::registry::AgentRecord;
    let store = state
        .session_manager
        .store()
        .expect("test_app_state_with_sqlite must provide a SQLite store");
    let alice = AgentRecord::for_test("alice");
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
    let store = state
        .session_manager
        .store()
        .expect("test_app_state_with_sqlite must provide a SQLite store");
    let record = AgentRecord::for_test(name);
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
// Agent-scoped session-activity SSE feed (#856)
// ---------------------------------------------------------------------------

/// Subscribe to the agent-scoped session-activity feed and return the receiver.
fn subscribe_agent(
    state: &AppState,
    agent_id: AgentId,
) -> crate::server::ManagedSubscription<AgentId> {
    state.run_manager.subscribe_agent(agent_id)
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
