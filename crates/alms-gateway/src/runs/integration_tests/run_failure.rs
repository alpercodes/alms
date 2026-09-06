// SPDX-License-Identifier: Apache-2.0

//! Failure paths: partial tool calls, the state-flip-before-broadcast invariant (#927), and the removed duplicate error marker (#912).

use super::{
    seed_alice_bob, subscribe_session, test_app_state, test_app_state_with_failing_llm,
    test_app_state_with_hanging_llm, test_app_state_with_sqlite,
};
use crate::sse::SseEventData;
use alms_coordinator::{SubagentCompletion, TaskId, TaskStatus};
use alms_core::{AgentId, Run, RunStatus, SessionId, TokenUsage};
use tokio_util::sync::CancellationToken;

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

    let notification = crate::runs::notifications::format_completion_notification(&completion);
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

    let notification = crate::runs::notifications::format_completion_notification(&completion);
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

    let notification = crate::runs::notifications::format_completion_notification(&completion);
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
        crate::runs::lifecycle::execute_run(
            exec_state,
            crate::runs::RunParams {
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

    crate::runs::lifecycle::execute_run(
        state.clone(),
        crate::runs::RunParams {
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
    let result = crate::runs::stream_agent_events(
        State(state.clone()),
        Path(unknown_id),
        HeaderMap::new(),
        Query(crate::runs::SessionEventsQuery {
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
    let ok = crate::runs::stream_agent_events(
        State(state.clone()),
        Path(alice_id),
        HeaderMap::new(),
        Query(crate::runs::SessionEventsQuery {
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
