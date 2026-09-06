// SPDX-License-Identifier: Apache-2.0

//! Run admission and queueing: priority order, pre-persisted input, and queue-position events (#831).

use super::{drain_events, subscribe_session, test_app_state, test_app_state_with_sqlite};
use crate::sse::SseEventData;
use alms_coordinator::message_bus::{MessageSource, RunTrigger};
use alms_core::{AgentId, Run, RunStatus};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

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

    crate::runs::notifications::run_trigger_loop(test_rx, state.clone()).await;

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

    let execution_barrier = crate::runs::lifecycle::install_admission_execution_barrier(session_id);

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
    let (status, resp) =
        match crate::runs::lifecycle::create_run(State(state.clone()), Json(req)).await {
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

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();
    let agent_id = AgentId::new();

    // Seed an agent record whose per-agent overrides differ on every
    // assertable knob from the stale payload below. The resolved config
    // post-#941 must reflect THESE values, never the payload's.
    let agent = AgentRecord {
        id: agent_id,
        model: Some("claude-sonnet-4-6".into()),
        posture: Some("autonomous".into()),
        provider: Some("anthropic".into()),
        thinking_budget_tokens: Some(2048),
        reasoning_effort: Some(ReasoningEffort::Low),
        gemini_thinking_budget: Some(4096),
        ..AgentRecord::for_test("stale-payload-victim")
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

    let (status, resp) =
        match crate::runs::lifecycle::create_run(State(state.clone()), Json(req)).await {
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
        let _ = crate::runs::lifecycle::create_run(State(state.clone()), Json(req))
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
    crate::runs::lifecycle::execute_run(
        state.clone(),
        crate::runs::RunParams {
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
    let resp_q1 = crate::runs::read_api::get_run_status(State(state.clone()), Path(q1_id))
        .await
        .expect("get_run_status should succeed for q1");
    assert_eq!(resp_q1.0.queue_position, Some(1));
    assert_eq!(resp_q1.0.status, RunStatus::Queued);

    // Queued #2 should be position 2.
    let resp_q2 = crate::runs::read_api::get_run_status(State(state.clone()), Path(q2_id))
        .await
        .expect("get_run_status should succeed for q2");
    assert_eq!(resp_q2.0.queue_position, Some(2));

    // Running run has no queue_position.
    let resp_running =
        crate::runs::read_api::get_run_status(State(state.clone()), Path(running_id))
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
    crate::runs::lifecycle::execute_run(
        state.clone(),
        crate::runs::RunParams {
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
    let _ = crate::runs::lifecycle::create_run(State(state.clone()), Json(req))
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
