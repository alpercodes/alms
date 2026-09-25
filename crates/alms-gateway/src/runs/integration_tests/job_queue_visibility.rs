// SPDX-License-Identifier: Apache-2.0

//! #181: a job firing that waits behind its agent's in-flight run is visible
//! while it waits.
//!
//! One run at a time per agent is by design, so a firing that lands mid-run
//! queues. Before #181 it queued invisibly: `fire_job_run` created the job's
//! `Run` only once the queue dequeued it, and hardcoded `queued_behind: 0`.
//! For the whole wait `GET /runs` had nothing and the log said only
//! "Scheduled job fired". These tests drive the production
//! `scheduler_fire_loop` against an agent that is busy the way a real run
//! makes it busy (a `Running` run holding the agent's queue slot), and pin
//! what an operator can now see during the wait and what happens to the
//! queued run afterwards: it runs, or it is cancelled, or it is recovered at
//! boot.

use super::{
    create_recurring_job, drain_events, subscribe_session, test_app_state_with_mock_llm,
    test_app_state_with_mock_llm_at,
};
use crate::server::AppState;
use crate::sse::SseEventData;
use alms_core::{AgentId, JobId, JobStatus, Run, RunId, RunStatus, SessionId};
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse as _;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// The agent's in-flight run, as the queue and the queue-position math see
/// one: a `Running` run in the registry, and a work item holding the agent's
/// queue slot. The busy agent in `queue.rs` has the same shape; this one
/// waits for a start signal instead of yielding, so the slot is known to be
/// held before the job fires.
struct BusyAgent {
    run_id: RunId,
    release: oneshot::Sender<()>,
}

impl BusyAgent {
    async fn start(state: &AppState, agent_id: AgentId, session_id: SessionId) -> Self {
        let run = Run::new(session_id, agent_id, "a long DM turn".into());
        let run_id = run.run_id;
        state.run_manager.insert_run(run).unwrap();
        assert!(state.run_manager.mark_run_as_running(run_id));
        let (release, released) = oneshot::channel::<()>();
        let (started_tx, started) = oneshot::channel::<()>();
        state.agent_queue.enqueue(
            agent_id,
            Box::pin(async move {
                let _ = started_tx.send(());
                let _ = released.await;
            }),
        );
        started.await.expect("the busy run's queue item must start");
        Self { run_id, release }
    }

    /// End the run and free the queue slot, as `execute_run`'s tail does.
    fn finish(self, state: &AppState) {
        assert!(state.run_manager.mark_run_as_completed(
            self.run_id,
            "done".into(),
            alms_core::TokenUsage::default()
        ));
        let _ = self.release.send(());
    }
}

/// Poll `probe` until it yields, or fail naming `what`.
async fn eventually<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(found) = probe() {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

fn start_fire_loop(state: &AppState) -> mpsc::UnboundedSender<JobId> {
    let (fire_tx, fire_rx) = mpsc::unbounded_channel();
    tokio::spawn(crate::runs::notifications::scheduler_fire_loop(
        fire_rx,
        state.clone(),
    ));
    fire_tx
}

fn job_session(state: &AppState, agent_id: AgentId, job_id: JobId) -> SessionId {
    state
        .session_manager
        .get_or_create(agent_id, format!("job_{}", job_id.0))
        .id
}

/// Wait for the job run's `run_created` on the job session and return it.
async fn await_run_created(
    subscription: &mut crate::server::ManagedSubscription<SessionId>,
) -> SseEventData {
    let mut seen = Vec::new();
    eventually("the job run's run_created", || {
        seen.extend(drain_events(subscription));
        seen.iter().find(|e| e.event_type == "run_created").cloned()
    })
    .await
}

fn run_id_of(event: &SseEventData) -> RunId {
    serde_json::from_value(event.data["run_id"].clone()).expect("run_created carries run_id")
}

/// The incident, and the fix end to end. While the agent is busy, the
/// firing's run exists as `queued`, `run_created` says one run is ahead of
/// it, and both read APIs report it. Once the agent is free, it runs as a
/// normal job turn and the episode closes.
#[tokio::test]
async fn job_firing_behind_a_busy_agent_is_queued_and_visible_then_runs() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();
    let agent_id = AgentId::new();
    let web_session_id = state.session_manager.get_or_create(agent_id, "web").id;
    let job_id = create_recurring_job(&state, agent_id, "nightly digest");
    let job_session_id = job_session(&state, agent_id, job_id);
    let mut job_events = subscribe_session(&state, job_session_id);
    let busy = BusyAgent::start(&state, agent_id, web_session_id).await;

    start_fire_loop(&state).send(job_id).unwrap();

    let created = await_run_created(&mut job_events).await;
    let job_run_id = run_id_of(&created);
    assert_eq!(
        created.data["queued_behind"], 1,
        "one run -- the busy one -- is ahead of the firing; got {}",
        created.data
    );
    assert_eq!(created.data["source"], "job");

    // Everything below is observed before the busy run ends.
    assert_eq!(
        state.run_manager.get_run(busy.run_id).unwrap().status(),
        RunStatus::Running
    );
    assert_eq!(
        state.run_manager.get_run(job_run_id).unwrap().status(),
        RunStatus::Queued
    );
    // GET /runs?agent_id=… -- the Runs tab's view, empty for the job before #181.
    let listed = crate::runs::list_runs(
        State(state.clone()),
        Query(crate::runs::ListRunsQuery {
            session_id: None,
            agent_id: Some(agent_id),
            limit: None,
        }),
    )
    .await
    .unwrap()
    .0;
    let entry = listed["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|run| run["run_id"] == job_run_id.0.to_string())
        .unwrap_or_else(|| panic!("GET /runs must list the queued job run; got {listed}"));
    assert_eq!(entry["status"], "queued");
    assert_eq!(entry["trigger"], "scheduled");
    assert_eq!(entry["job_id"], job_id.0.to_string());
    // GET /runs/{id} -- the reload / polling view of the same position.
    let status = crate::runs::get_run_status(State(state.clone()), Path(job_run_id))
        .await
        .unwrap()
        .0;
    assert_eq!(status.queue_position, Some(1));
    // The episode opened with the firing, turn 1 reserved.
    let episode = state
        .job_episodes
        .snapshot(job_id)
        .expect("the episode opens when the firing is admitted");
    assert_eq!(episode["in_flight_runs"], 1);
    assert_eq!(episode["runs"], 1);

    busy.finish(&state);

    let finished = eventually("the job run to finish", || {
        state
            .run_manager
            .get_run(job_run_id)
            .filter(|run| run.status().is_terminal())
    })
    .await;
    assert_eq!(finished.status(), RunStatus::Completed);
    eventually("the episode to close", || {
        state.job_episodes.snapshot(job_id).is_none().then_some(())
    })
    .await;
    let job = state.job_store.get(job_id).unwrap();
    assert!(
        job.last_run_at.is_some(),
        "the episode close recorded the run"
    );
    assert_eq!(job.status(), JobStatus::Active);
    // run_created is published once, at admission -- not again at dequeue.
    let later = drain_events(&mut job_events);
    assert!(
        !later
            .iter()
            .any(|e| e.event_type == "run_created" && run_id_of(e) == job_run_id),
        "run_created must not be re-emitted when the queued run starts"
    );

    shutdown_token.cancel();
}

/// `DELETE /jobs` while the firing's run is queued: the sweep finds the
/// queued run (its token is registered at admission), and the episode goes
/// with the job. When the agent frees up the run ends `Cancelled` without
/// its turn ever starting -- no LLM call, no completion card, no re-arm.
#[tokio::test]
async fn deleting_a_job_cancels_its_run_while_queued() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();
    let agent_id = AgentId::new();
    let web_session_id = state.session_manager.get_or_create(agent_id, "web").id;
    let job_id = create_recurring_job(&state, agent_id, "nightly digest");
    let job_session_id = job_session(&state, agent_id, job_id);
    let mut job_events = subscribe_session(&state, job_session_id);
    let busy = BusyAgent::start(&state, agent_id, web_session_id).await;

    start_fire_loop(&state).send(job_id).unwrap();
    let job_run_id = run_id_of(&await_run_created(&mut job_events).await);

    let response = crate::jobs::cancel_job(State(state.clone()), Path(job_id))
        .await
        .into_response();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert!(
        state.job_episodes.snapshot(job_id).is_none(),
        "DELETE /jobs removes the episode the queued firing opened"
    );

    busy.finish(&state);

    // Had the sweep missed the queued run, the mock LLM would run its turn
    // here and the run would end Completed.
    let ended = eventually("the queued job run to end", || {
        state
            .run_manager
            .get_run(job_run_id)
            .filter(|run| run.status().is_terminal())
    })
    .await;
    assert_eq!(ended.status(), RunStatus::Cancelled);
    assert!(ended.started_at.is_none(), "its turn never started");
    let history = state.session_manager.get_history(job_session_id).unwrap();
    assert!(
        !history
            .iter()
            .any(|m| m.role == alms_session::Role::Assistant),
        "no turn may reach the LLM for a cancelled job"
    );
    let web_history = state.session_manager.get_history(web_session_id).unwrap();
    assert!(
        !web_history.iter().any(|m| {
            m.metadata
                .as_ref()
                .and_then(|meta| meta.get("type"))
                .and_then(|v| v.as_str())
                == Some("job_notification")
        }),
        "a cancelled job gets no completion card"
    );
    assert_eq!(
        state.job_store.get(job_id).unwrap().status(),
        JobStatus::Cancelled
    );
    assert_eq!(state.scheduler.pending_count().await, 0, "not re-armed");

    shutdown_token.cancel();
}

/// A second firing while the first is still queued is absorbed into the D6
/// catch-up instead of queueing a second run. This follows from the episode
/// opening at admission; before #181 there was no episode during the wait,
/// so nothing absorbed it.
#[tokio::test]
async fn a_second_firing_during_the_wait_is_absorbed_not_queued() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();
    let agent_id = AgentId::new();
    let web_session_id = state.session_manager.get_or_create(agent_id, "web").id;
    let job_id = create_recurring_job(&state, agent_id, "nightly digest");
    let job_session_id = job_session(&state, agent_id, job_id);
    let mut job_events = subscribe_session(&state, job_session_id);
    let busy = BusyAgent::start(&state, agent_id, web_session_id).await;

    let fire_tx = start_fire_loop(&state);
    fire_tx.send(job_id).unwrap();
    await_run_created(&mut job_events).await;
    fire_tx.send(job_id).unwrap();

    eventually("the second firing to be absorbed", || {
        state
            .job_episodes
            .snapshot(job_id)
            .filter(|episode| episode["catch_up_queued"] == true)
    })
    .await;
    assert_eq!(
        state.run_manager.list_by_session(job_session_id, 10).len(),
        1,
        "exactly one queued run for the job"
    );

    busy.finish(&state);
    shutdown_token.cancel();
}

/// #185 review S1: the queue wait is not episode time. A recurring job fires
/// behind a busy agent, and the wait spans one of the job's own cron ticks.
/// Turn 1 then runs after that tick, so it has already covered it: the
/// close must re-arm for the next tick, not fire a D6 catch-up that would
/// run the same prompt again straight away.
///
/// A real tick cannot be waited out here, so the wait is simulated by
/// backdating the queued episode's `started_at` by two days (the helper job
/// is daily at midnight). That is exactly what a long wait did before the
/// fix, when the clock started at admission.
#[tokio::test]
async fn a_firing_that_waits_past_a_tick_is_not_caught_up_at_close() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();
    let agent_id = AgentId::new();
    let web_session_id = state.session_manager.get_or_create(agent_id, "web").id;
    let job_id = create_recurring_job(&state, agent_id, "nightly digest");
    let job_session_id = job_session(&state, agent_id, job_id);
    let mut job_events = subscribe_session(&state, job_session_id);
    let busy = BusyAgent::start(&state, agent_id, web_session_id).await;

    start_fire_loop(&state).send(job_id).unwrap();
    let job_run_id = run_id_of(&await_run_created(&mut job_events).await);
    state
        .job_episodes
        .backdate_started_at(job_id, chrono::Duration::days(2));

    busy.finish(&state);

    let finished = eventually("the job run to finish", || {
        state
            .run_manager
            .get_run(job_run_id)
            .filter(|run| run.status().is_terminal())
    })
    .await;
    assert_eq!(finished.status(), RunStatus::Completed);
    eventually("the episode to close", || {
        state.job_episodes.snapshot(job_id).is_none().then_some(())
    })
    .await;
    let job = state.job_store.get(job_id).unwrap();
    let recorded = job.last_run_at.expect("the episode close recorded the run");
    // A D6 catch-up records `next_run_at = now`, the close time itself; a
    // normal re-arm records the next tick after it.
    assert!(
        job.next_run_at.is_some_and(|next| next > recorded),
        "turn 1 ran after the tick, so the close must re-arm for the next one, \
         not catch up: recorded at {recorded}, next_run_at {:?}",
        job.next_run_at
    );

    shutdown_token.cancel();
}

/// A hard stop while the firing's run is queued: the work item never runs.
/// The run was persisted `queued` at admission, so the next process's boot
/// sweep (`mark_stale_runs_failed`, which `Gateway::new` runs before
/// anything else) fails it with `gateway_restarted` rather than leaving it
/// dangling. The job itself was never recorded, so it is still due and the
/// boot catch-up (`bootstrap_fire_at`) fires it again.
#[tokio::test]
async fn a_job_run_queued_at_a_hard_stop_is_failed_at_boot_and_the_job_stays_due() {
    let directory = tempfile::tempdir().unwrap();
    let db_path = directory
        .path()
        .join("queued-job-hard-stop.db")
        .to_string_lossy()
        .into_owned();
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm_at(&db_path);
    let agent_id = AgentId::new();
    let web_session_id = state.session_manager.get_or_create(agent_id, "web").id;
    let job_id = state
        .job_store
        .create(alms_core::job::CreateJobRequest {
            agent_id,
            prompt: "one-shot report".to_string(),
            schedule: alms_core::job::JobSchedule::Once {
                run_at: chrono::Utc::now(),
            },
        })
        .unwrap()
        .id;
    let job_session_id = job_session(&state, agent_id, job_id);
    let mut job_events = subscribe_session(&state, job_session_id);
    let _busy = BusyAgent::start(&state, agent_id, web_session_id).await;

    start_fire_loop(&state).send(job_id).unwrap();
    let job_run_id = run_id_of(&await_run_created(&mut job_events).await);

    // The process dies here. A fresh one opens the same database.
    let restarted = alms_session::SqliteStore::open(&db_path).unwrap();
    restarted.mark_stale_runs_failed().unwrap();

    let row = restarted
        .load_run(job_run_id)
        .unwrap()
        .expect("the queued job run was persisted at admission");
    assert_eq!(row.status(), RunStatus::Failed);
    assert_eq!(row.terminal_reason(), Some("gateway_restarted"));
    let job = restarted
        .load_all_jobs()
        .unwrap()
        .into_iter()
        .find(|job| job.id == job_id)
        .expect("the job row survives");
    assert!(
        !job.status().is_terminal(),
        "a one-shot whose run never executed is not spent"
    );
    assert!(
        job.next_run_at.is_some_and(|due| due <= chrono::Utc::now()),
        "still due, so the boot catch-up fires it again; got {:?}",
        job.next_run_at
    );
    // And the boot scheduler does fire it: `bootstrap_scheduler` puts the job
    // in the catch-up cohort, rather than skipping it as spent.
    assert!(
        matches!(
            crate::server::bootstrap_fire_at(&job, chrono::Utc::now()),
            Some(crate::server::BootstrapFire::CatchUp { due_at }) if Some(due_at) == job.next_run_at
        ),
        "the next boot must fire the job once as a catch-up; got {:?}",
        crate::server::bootstrap_fire_at(&job, chrono::Utc::now())
    );

    shutdown_token.cancel();
}
