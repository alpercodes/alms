// SPDX-License-Identifier: Apache-2.0

//! `GET /runs/{id}/reasoning` and `GET /runs/{id}/text` rehydration (#1043, #1077, #1133, #1107).

use super::{drain_events, subscribe_session, test_app_state, test_app_state_with_mock_llm_at};
use crate::sse::SseEventData;
use alms_core::{AgentId, Run, RunId, RunStatus};
use tokio_util::sync::CancellationToken;

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

    let response = crate::runs::read_api::get_run_reasoning(State(state.clone()), Path(run_id))
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
    let response2 = crate::runs::read_api::get_run_reasoning(State(state.clone()), Path(run_id))
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

    let response = crate::runs::read_api::get_run_reasoning(State(state.clone()), Path(run_id))
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

    let resp_a = crate::runs::read_api::get_run_reasoning(State(state.clone()), Path(run_a_id))
        .await
        .expect("get_run_reasoning should succeed for run A");
    let resp_b = crate::runs::read_api::get_run_reasoning(State(state.clone()), Path(run_b_id))
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

    let response = crate::runs::read_api::get_run_reasoning(State(state.clone()), Path(run_id))
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

    let response = crate::runs::read_api::get_run_reasoning(State(state.clone()), Path(run_id))
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

    let response = crate::runs::read_api::get_run_reasoning(State(state.clone()), Path(run_id))
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

    let resp_b = crate::runs::read_api::get_run_reasoning(State(state.clone()), Path(run_b_id))
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

    let response = crate::runs::read_api::get_run_reasoning(State(state.clone()), Path(run_id))
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

    let response = crate::runs::read_api::get_run_reasoning(State(state.clone()), Path(run_id))
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

    let response = crate::runs::read_api::get_run_reasoning(State(state.clone()), Path(run_id))
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

    let response = crate::runs::read_api::get_run_reasoning(State(state.clone()), Path(run_id))
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

    let response = crate::runs::read_api::get_run_reasoning(State(state.clone()), Path(run_id))
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

    let response = crate::runs::read_api::get_run_text(State(state.clone()), Path(run_id))
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
    let response2 = crate::runs::read_api::get_run_text(State(state.clone()), Path(run_id))
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

    let response = crate::runs::read_api::get_run_text(State(state.clone()), Path(run_id))
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

    let resp_a = crate::runs::read_api::get_run_text(State(state.clone()), Path(run_a_id))
        .await
        .expect("get_run_text should succeed for run A");
    let resp_b = crate::runs::read_api::get_run_text(State(state.clone()), Path(run_b_id))
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

    let response = crate::runs::read_api::get_run_text(State(state.clone()), Path(run_id))
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

    let response = crate::runs::read_api::get_run_text(State(state.clone()), Path(run_id))
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

    let response = crate::runs::read_api::get_run_text(State(state.clone()), Path(run_id))
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

    let response = crate::runs::read_api::get_run_text(State(state.clone()), Path(run_id))
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
    let result = crate::runs::read_api::get_run_text(State(state.clone()), Path(unknown_run)).await;
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
    let response = crate::runs::read_api::get_run_text(State(state.clone()), Path(run_id))
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

    let response = crate::runs::read_api::get_run_text(State(state.clone()), Path(run_id))
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

    crate::runs::lifecycle::execute_run(
        state.clone(),
        crate::runs::RunParams {
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
