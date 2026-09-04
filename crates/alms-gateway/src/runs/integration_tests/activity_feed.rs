// SPDX-License-Identifier: Apache-2.0

//! #856 — the agent-scoped session-activity SSE feed.

use super::{
    drain_activity_events, drain_events, subscribe_activity, subscribe_agent, subscribe_session,
    test_app_state, test_app_state_with_mock_llm,
};
use crate::server::AppState;
use crate::sse::SseEventData;
use alms_core::{AgentId, Run, RunStatus, SessionId};
use tokio_util::sync::CancellationToken;

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
    crate::runs::lifecycle::execute_run(
        state.clone(),
        crate::runs::RunParams {
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

    crate::runs::lifecycle::execute_run(
        state.clone(),
        crate::runs::RunParams {
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
        crate::runs::lifecycle::execute_run(
            exec_state,
            crate::runs::RunParams {
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
        crate::runs::lifecycle::execute_run(
            exec_state,
            crate::runs::RunParams {
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
