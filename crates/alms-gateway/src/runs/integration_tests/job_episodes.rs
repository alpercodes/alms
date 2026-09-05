// SPDX-License-Identifier: Apache-2.0

//! #1198 — job episodes: deferred completion across DMs and subagents.

use super::{
    create_recurring_job, seed_alice_bob, test_app_state, test_app_state_with_mock_llm,
    test_app_state_with_sqlite,
};
use crate::server::AppState;
use alms_coordinator::message_bus::{MessageSource, RunTrigger};
use alms_coordinator::{SubagentCompletion, TaskId, TaskStatus};
use alms_core::{AgentId, Run, RunId, SessionId, TokenUsage};
use alms_tools::MessageSender;
use alms_tools::message_sender::ConversationEndReason;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

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

    crate::runs::notifications::fire_job_run(state.clone(), job_id)
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
        crate::runs::notifications::scheduler_fire_loop(fire_rx, loop_state).await;
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

    crate::runs::lifecycle::execute_run(
        state.clone(),
        crate::runs::RunParams {
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
        crate::runs::job_episode::RunCompletion::Open
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
    crate::runs::notifications::run_trigger_loop(test_rx, state.clone()).await;

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
        crate::runs::job_episode::RunCompletion::Open
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
    crate::runs::notifications::run_trigger_loop(test_rx, state.clone()).await;

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
            crate::runs::job_episode::RunCompletion::Open
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
    crate::runs::notifications::run_trigger_loop(test_rx, state.clone()).await;

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
        crate::runs::job_episode::RunCompletion::Open
    ));

    let run_id = crate::runs::notifications::enqueue_triggered_run(
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
    crate::runs::notifications::run_trigger_loop(test_rx, state.clone()).await;

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
        crate::runs::job_episode::RunCompletion::Open
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
    crate::runs::notifications::completion_notification_loop(test_rx, state.clone()).await;

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
    let episode = crate::runs::job_episode::JobEpisode {
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

    crate::runs::notifications::close_episode(&state, episode, true).await;

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
        crate::runs::job_episode::JobEpisode {
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
    crate::runs::notifications::close_episode(&state, episode, false).await;
    let job = state.job_store.get(missed_job).unwrap();
    let next = job.next_run_at.expect("catch-up must set next_run_at");
    assert!(
        next <= chrono::Utc::now() + chrono::Duration::seconds(1),
        "coalesced catch-up must be due immediately, got {next}"
    );

    // Case 2: no tick elapsed (started just now) — normal future re-arm.
    let ontime_job = make_job("on time");
    let episode = make_episode(ontime_job, chrono::Utc::now());
    crate::runs::notifications::close_episode(&state, episode, false).await;
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
        crate::runs::job_episode::RunCompletion::Open
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
    crate::runs::notifications::run_trigger_loop(replay_rx, state.clone()).await;

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
    crate::runs::notifications::completion_notification_loop(test_rx, state.clone()).await;

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
    crate::runs::notifications::run_trigger_loop(scope_rx, state.clone()).await;

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
    crate::runs::notifications::completion_notification_loop(test_rx, state.clone()).await;

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
    crate::runs::notifications::run_trigger_loop(test_rx, state.clone()).await;

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
    let episode = crate::runs::job_episode::JobEpisode {
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
    crate::runs::notifications::record_and_rearm(&state, &job_snapshot, &episode, false).await;

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
    crate::runs::notifications::record_and_rearm(&state, &job, &episode, false).await;

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
        .inject_persistence_failures(crate::runs::notifications::JOB_REARM_MAX_ATTEMPTS);
    crate::runs::notifications::record_and_rearm(&state, &job, &episode, false).await;

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
) -> crate::runs::job_episode::JobEpisode {
    let session_id = state
        .session_manager
        .get_or_create(agent_id, format!("job_{}", job_id.0))
        .id;
    let mut run = Run::for_job(session_id, agent_id, "scheduled work".into(), job_id);
    let run_id = run.run_id;
    run.mark_running();
    assert!(run.mark_completed("done".into(), TokenUsage::default()));
    let _ = state.run_manager.insert_run(run);

    crate::runs::job_episode::JobEpisode {
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
    let mut close = Box::pin(crate::runs::notifications::close_episode(
        &state, episode, false,
    ));
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
    let mut close = Box::pin(crate::runs::notifications::close_episode(
        &state, episode, false,
    ));
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
