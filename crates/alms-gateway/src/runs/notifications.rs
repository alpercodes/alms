//! DM notification routing, scheduler integration, and trigger loops.

use super::job_episode::{self, EPISODE_DEADLINE_SECS, RunCompletion};
use super::{RunParams, find_user_facing_session};
use crate::cron_utils;
use crate::server::AppState;
use crate::sse::SseEventData;
use alms_core::{JobId, JobSchedule, JobStatus, Run, RunId, RunStatus, SessionId};
use alms_session::job_store::{DispatchFailureOutcome, RecordRunOutcome};
use alms_tools::message_sender::ConversationEndReason;
use chrono::Utc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument, warn};

use super::lifecycle::execute_run_guarded;
const JOB_DISPATCH_MAX_ATTEMPTS: u32 = 3;
const JOB_DISPATCH_RETRY_BASE_SECS: u64 = 5;
/// Bounded retry budget for the durable episode-close job write (#1233).
pub(super) const JOB_REARM_MAX_ATTEMPTS: u32 = 3;
/// Base backoff between episode-close write retries. Short on purpose: the
/// write is a local SQLite statement, and `close_episode` holds the job's
/// completion/cancellation gate across the retries.
const JOB_REARM_RETRY_BASE_MS: u64 = 100;

// ---------------------------------------------------------------------------
// Scheduler integration
// ---------------------------------------------------------------------------

/// Receives fired job IDs from the scheduler and dispatches agent runs.
///
/// Admission is awaited on this dedicated producer task. The admitted run
/// executes as queue work, so the loop can receive the next firing once the
/// work is submitted without spawning an unbounded task per firing.
pub(crate) async fn scheduler_fire_loop(mut rx: mpsc::UnboundedReceiver<JobId>, state: AppState) {
    while let Some(job_id) = rx.recv().await {
        let Some(job) = state.job_store.get(job_id) else {
            continue;
        };
        if job.status().is_terminal() {
            continue;
        }
        let state_clone = state.clone();
        let retry_state = state.clone();
        let Ok(reservation) = state.agent_queue.reserve(job.agent_id).await else {
            break;
        };
        if let Err(error) = reservation.submit(Box::pin(async move {
            if let Err(dispatch_error) = fire_job_run(state_clone, job_id).await {
                error!("Job {} run dispatch failed: {}", job_id, dispatch_error);
                let Some(job) = retry_state.job_store.get(job_id) else {
                    return;
                };
                if job.status().is_terminal() {
                    return;
                }
                let multiplier = 1u64 << job.retry_count().min(6);
                let delay_secs = JOB_DISPATCH_RETRY_BASE_SECS.saturating_mul(multiplier);
                let retry_at = Utc::now()
                    + chrono::Duration::seconds(delay_secs.try_into().unwrap_or(i64::MAX));
                match retry_state.job_store.record_dispatch_failure(
                    job_id,
                    dispatch_error.to_string(),
                    retry_at,
                    JOB_DISPATCH_MAX_ATTEMPTS,
                ) {
                    Ok(DispatchFailureOutcome::RetryScheduled { attempt, retry_at }) => {
                        retry_state
                            .scheduler
                            .schedule_once(
                                job_id,
                                tokio::time::Instant::now()
                                    + std::time::Duration::from_secs(delay_secs),
                            )
                            .await;
                        warn!(
                            %job_id,
                            attempt,
                            max_attempts = JOB_DISPATCH_MAX_ATTEMPTS,
                            %retry_at,
                            "Scheduled bounded retry after job dispatch failure"
                        );
                    }
                    Ok(DispatchFailureOutcome::Exhausted { attempts }) => {
                        error!(
                            %job_id,
                            attempts,
                            "Job dispatch retry budget exhausted"
                        );
                    }
                    Ok(
                        DispatchFailureOutcome::RefusedTerminal | DispatchFailureOutcome::NotFound,
                    ) => {}
                    Err(error) => {
                        error!(%job_id, %error, "Failed to persist job dispatch failure");
                    }
                }
            }
        })) {
            warn!(?error, %job_id, "Scheduled job queue closed before dispatch");
            break;
        }
    }
}

/// Create and execute an agent run triggered by a scheduled job.
///
/// `pub(super)` so the sibling `integration_tests` module can exercise the
/// full fire -> episode-open -> turn -> close pipeline end-to-end (#1198).
#[instrument(level = "info", skip(state), fields(job_id = %job_id))]
pub(super) async fn fire_job_run(state: AppState, job_id: JobId) -> alms_core::AlmsResult<()> {
    // Look up the job — it may have been cancelled between scheduling and firing.
    let Some(job) = state.job_store.get(job_id) else {
        info!("Skipping fired job — not found in store");
        return Ok(());
    };
    if job.status().is_terminal() {
        info!("Skipping fired job — already cancelled");
        return Ok(());
    }

    // #1198 D6 defensive guard: a firing that lands while the job's episode
    // is still open (unreachable under re-arm-at-close, but a future
    // scheduler change or a bootstrap past-due re-fire could produce it) is
    // absorbed into the episode's coalesced catch-up dirty-bit instead of
    // overlapping on the shared job session.
    if state.job_episodes.absorb_fire_if_open(job_id) {
        return Ok(());
    }

    // Use a stable context_id so each job accumulates session history across firings.
    let context_id = format!("job_{}", job_id.0);
    let session = state
        .session_manager
        .get_or_create(job.agent_id, &context_id);
    let session_id = session.id;

    let admission_guard =
        super::lifecycle::acquire_run_admission_guard(&state.run_admission_gates, session_id).await;
    state.session_manager.get(session_id)?;
    let run = Run::for_job(session_id, job.agent_id, job.prompt.clone(), job_id);
    let run_id = run.run_id;
    state.run_manager.insert_run(run.clone())?;
    drop(admission_guard);
    // Job runs execute inline (not via agent_queue) so queued_behind is 0.
    state
        .run_manager
        .send_session_event(
            session_id,
            run_id,
            SseEventData::run_created(run_id, session_id, true, Some("job".to_string()), 0),
        )
        .await;
    info!("Job fired -> run {}", run_id.0);

    // #1198: open the job episode BEFORE the first turn executes. From here
    // on, completion bookkeeping (card + record_run + recurring re-arm) is
    // owned by `close_episode`, driven by `finish_episode_run` at every
    // `execute_run` exit — the episode stays open across triggered DMs and
    // background subagents until quiescence or the 4-hour deadline.
    state
        .job_episodes
        .open(job_id, session_id, job.agent_id, run_id);

    // Execute the run (awaits completion; errors are handled inside execute_run).
    // Register the token so scheduled job runs are cancellable via POST /runs/{id}/cancel
    // in addition to the job-level DELETE /jobs/{id} path.
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    execute_run_guarded(
        state.clone(),
        RunParams {
            run_id,
            session_id,
            agent_id: job.agent_id,
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

    // #1198: the post-run block (completion card + record_run + re-arm)
    // moved to `close_episode`, invoked by the episode hook inside
    // `execute_run` when the episode reaches quiescence — which, for a
    // turn with no async work, is right here at turn-1 end (behavior
    // identical to the pre-#1198 flow).
    Ok(())
}

// ---------------------------------------------------------------------------
// Job episodes (#1198): deferred completion, quiescence, deadline sweep
// ---------------------------------------------------------------------------

/// Completion-card statistics for a closed episode.
pub(super) struct EpisodeCloseStats {
    pub turns: usize,
    pub dm_count: usize,
    pub subagent_count: usize,
    pub timed_out: bool,
    /// Pending items left open at a deadline-forced close (0 for
    /// quiescent closes). Detach-and-complete: these keep running on
    /// their own lifecycle.
    pub detached: usize,
}

/// Feed one episode-run exit into the tracker and run the close side
/// effects when the episode reaches quiescence.
///
/// MUST be called from **every** `execute_run` exit for job-stamped runs —
/// the five sites are the pre-cancel early exit, the resolve-failure exit,
/// the token-budget rejection exit, the runtime-construction failure exit,
/// and the common terminal tail. A missed exit leaks the episode's
/// `in_flight_runs` reservation and stalls the close until the deadline
/// sweep (see the design doc, step 4).
pub(super) async fn finish_episode_run(
    state: &AppState,
    job_id: Option<JobId>,
    run_id: RunId,
    tool_calls: &[alms_core::ToolCallRecord],
) {
    let Some(job_id) = job_id else { return };
    let opened_dms = job_episode::dms_opened(tool_calls);
    let opened_subagents = job_episode::subagents_spawned(tool_calls);
    match state
        .job_episodes
        .on_run_complete(job_id, run_id, opened_dms, opened_subagents)
    {
        RunCompletion::Closed(episode) => {
            info!(
                job_id = %job_id,
                run_id = %run_id.0,
                turns = episode.runs.len(),
                "Job episode quiescent — closing"
            );
            close_episode(state, *episode, false).await;
        }
        RunCompletion::Open => {
            debug!(
                job_id = %job_id,
                run_id = %run_id.0,
                "Episode run completed — episode stays open (pending work / in-flight runs)"
            );
        }
        RunCompletion::Untracked => {}
    }
}

/// Release a continuation reservation when admission ends before a durable
/// run exists. If it was the episode's final outstanding work item, perform
/// the normal close side effects immediately.
async fn abandon_episode_continuation(state: &AppState, job_id: Option<JobId>) {
    let Some(job_id) = job_id else { return };
    match state.job_episodes.abandon_reserved_continuation(job_id) {
        RunCompletion::Closed(episode) => {
            info!(
                job_id = %job_id,
                "Abandoned continuation admission was the episode's final reservation"
            );
            close_episode(state, *episode, false).await;
        }
        RunCompletion::Open => {
            debug!(
                job_id = %job_id,
                "Released abandoned continuation reservation; episode remains open"
            );
        }
        RunCompletion::Untracked => {}
    }
}

/// Close a job episode: completion card, `record_run`, and the recurring
/// re-arm / coalesced catch-up (D6). `timed_out` marks a deadline-forced
/// close (D5 detach-and-complete) — the card carries a deadline note and
/// the pending items are left running.
///
/// Owns what used to be `fire_job_run`'s post-run block, generalized from
/// "after the one turn" to "after the whole episode".
pub(super) async fn close_episode(
    state: &AppState,
    episode: job_episode::JobEpisode,
    timed_out: bool,
) {
    let job_id = episode.job_id;

    // A completion and DELETE /jobs are mutually exclusive visible outcomes.
    // Hold the gate through the notification fanout so a cancellation cannot
    // win after the pre-check yet before the card is emitted.
    let job_completion_cancellation_gate = state.job_completion_cancellation_gate(job_id);
    let job_completion_cancellation_guard = job_completion_cancellation_gate.lock().await;
    // Guard: if the job was cancelled (or deleted) while the episode was
    // open, do not notify / overwrite the Cancelled status / re-arm.
    // Generalizes the pre-#1198 cancelled-during-run guard.
    let Some(job) = state.job_store.get(job_id) else {
        info!(job_id = %job_id, "Job disappeared during episode — skipping close");
        return;
    };
    if job.status().is_terminal() {
        info!(job_id = %job_id, "Job was cancelled during episode — skipping close");
        return;
    }

    // The card's run handle is the episode's final run (turn 1 when no
    // async work happened — the pre-#1198 shape).
    let last_run = episode
        .runs
        .last()
        .copied()
        .unwrap_or_else(alms_core::RunId::new);
    let stats = EpisodeCloseStats {
        turns: episode.runs.len(),
        dm_count: episode.dm_total,
        subagent_count: episode.subagent_total,
        timed_out,
        detached: episode.pending_count(),
    };

    // -- Job completion notification --
    // Send a notification to the agent's most recent user-facing session
    // so the user can see that the job ran (even if they weren't watching
    // the hidden job_* session).
    notify_job_completion(
        state,
        job.agent_id,
        &job.prompt,
        last_run,
        job_id,
        Some(&stats),
    )
    .await;

    // Update the job record and re-arm. The S1 guard (#1202, Tim's review):
    // `notify_job_completion` above AWAITED between the Cancelled pre-check
    // and this write — a `DELETE /jobs` landing in that window used to flip
    // the job `Cancelled -> Active` and re-arm it (a cancelled recurring
    // job resurrected). The store's absorbing-`Cancelled` guard now refuses
    // the stale write, and every re-arm below is gated on `Recorded`.
    record_and_rearm(state, &job, &episode, timed_out).await;
    drop(job_completion_cancellation_guard);
}

/// Record the episode's run on the job and re-arm recurring schedules —
/// the D6 coalesced catch-up when >=1 cron tick elapsed during the
/// episode, otherwise the normal next-tick arm.
///
/// Split out of [`close_episode`] so the S1 race shape is directly
/// testable: the caller holds a `job` snapshot read BEFORE the completion
/// fanout, and a `DELETE /jobs` may have landed since. Correctness does
/// not depend on the snapshot's freshness — `JobStore::record_run`'s
/// absorbing-`Cancelled` guard refuses the stale write atomically (same
/// entry lock as `cancel`), and the `Recorded` gate here skips the
/// scheduler re-arm on refusal.
pub(super) async fn record_and_rearm(
    state: &AppState,
    job: &alms_core::job::Job,
    episode: &job_episode::JobEpisode,
    timed_out: bool,
) {
    let job_id = episode.job_id;
    let now = Utc::now();
    match &job.schedule {
        JobSchedule::Once { .. } => {
            // A one-shot closes as Completed. Refusal means another terminal
            // transition (normally operator cancellation) won the race.
            match record_run_with_retry(
                state,
                job_id,
                now,
                JobStatus::Completed,
                None,
                Some(if timed_out {
                    alms_core::JobTerminalReason::DeadlineReached
                } else {
                    alms_core::JobTerminalReason::Completed
                }),
            )
            .await
            {
                Ok(RecordRunOutcome::Recorded) => {}
                Ok(outcome) => {
                    info!(job_id = %job_id, ?outcome, "record_run not applied at episode close")
                }
                Err(error) => {
                    // #1233: the one-shot ran but could not be marked
                    // terminal. Nothing durable can be written — the store is
                    // what is failing — so the honest outcome is: count it,
                    // and say plainly that a restart will replay this job.
                    state.job_store.record_rearm_failure();
                    error!(
                        job_id = %job_id,
                        attempts = JOB_REARM_MAX_ATTEMPTS,
                        %error,
                        "One-shot job completion could not be persisted after the retry budget. \
                         The job stays non-terminal and WILL RE-FIRE at the next daemon restart; \
                         run `DELETE /jobs/{job_id}` to suppress the replay (#1233)"
                    );
                }
            }
        }
        JobSchedule::Recurring { cron } => {
            // D6: coalesced catch-up. If at least one cron tick elapsed
            // while the episode was open (or the defensive fire-path guard
            // absorbed a firing), fire exactly ONE catch-up now — missed
            // ticks never queue individually (identical prompts; see the
            // design doc § D6).
            let missed = episode.catch_up_queued
                || cron_utils::next_after(cron, episode.started_at)
                    .map(|t| t <= now)
                    .unwrap_or(false);
            if missed {
                match record_run_with_retry(state, job_id, now, JobStatus::Active, Some(now), None)
                    .await
                {
                    Ok(RecordRunOutcome::Recorded) => {
                        // Route through the scheduler with a zero delay so the
                        // catch-up reuses the normal fire path (cancellation
                        // checks, per-agent queueing, the absorb guard).
                        state
                            .scheduler
                            .schedule_once(job_id, tokio::time::Instant::now())
                            .await;
                        info!(
                            job_id = %job_id,
                            "Episode outlived >=1 cron tick — coalesced catch-up fired (D6)"
                        );
                    }
                    Ok(outcome) => {
                        info!(
                            job_id = %job_id,
                            ?outcome,
                            "Job cancelled/removed during episode close — catch-up skipped (S1)"
                        );
                    }
                    Err(error) => rearm_after_failed_close(state, job_id, cron, now, &error).await,
                }
            } else {
                let next = cron_utils::next_after(cron, now);
                if next.is_none() {
                    warn!("Recurring cron '{}' has no future occurrences", cron);
                }
                match record_run_with_retry(state, job_id, now, JobStatus::Active, next, None).await
                {
                    Ok(RecordRunOutcome::Recorded) => {
                        if let Some(next) = next {
                            let delay = (next - now).to_std().unwrap_or(std::time::Duration::ZERO);
                            let instant = tokio::time::Instant::now() + delay;
                            state.scheduler.schedule_once(job_id, instant).await;
                            info!("Recurring job re-armed for {}", next);
                        }
                    }
                    Ok(outcome) => {
                        info!(
                            job_id = %job_id,
                            ?outcome,
                            "Job cancelled/removed during episode close — re-arm skipped (S1)"
                        );
                    }
                    Err(error) => rearm_after_failed_close(state, job_id, cron, now, &error).await,
                }
            }
        }
    }
}

/// Persist an episode-close job write under a bounded retry budget (#1233).
///
/// `JobStore::transition_job` persists BEFORE committing to memory, so an
/// `Err` here means neither SQLite nor memory advanced: the job is still
/// `Active` with `next_run_at` pinned to the tick that already fired. Before
/// this budget existed, the caller's only response was an `error!` line, so a
/// single transient SQLite failure silently stopped a recurring job until the
/// next daemon restart (Tim's S1 on #1225, carried forward through #1230).
///
/// The only other `Err` source is lifecycle-revision exhaustion at
/// `MAX_LIFECYCLE_REVISION` (`i64::MAX`), which is unreachable in practice;
/// retrying it is harmless because the retries are bounded and sub-second.
async fn record_run_with_retry(
    state: &AppState,
    job_id: JobId,
    ran_at: chrono::DateTime<Utc>,
    new_status: JobStatus,
    next_run_at: Option<chrono::DateTime<Utc>>,
    terminal_reason: Option<alms_core::JobTerminalReason>,
) -> alms_core::AlmsResult<RecordRunOutcome> {
    let mut attempt = 1u32;
    loop {
        let result = state.job_store.record_run_with_reason(
            job_id,
            ran_at,
            new_status,
            next_run_at,
            terminal_reason,
        );
        match result {
            Ok(outcome) => return Ok(outcome),
            Err(error) if attempt < JOB_REARM_MAX_ATTEMPTS => {
                warn!(
                    %job_id,
                    attempt,
                    max_attempts = JOB_REARM_MAX_ATTEMPTS,
                    %error,
                    "Episode-close job write failed — retrying"
                );
                let backoff = JOB_REARM_RETRY_BASE_MS.saturating_mul(1u64 << (attempt - 1));
                tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Keep a recurring job alive after its episode-close write exhausted the
/// retry budget (#1233).
///
/// The guarantee is deliberately narrower than a clean close, because the
/// durable store is exactly what is failing: nothing new can be persisted.
/// What this does guarantee is that the job does not *silently stop*.
///
/// - The job is re-armed **in memory** at its next cron occurrence, so the
///   schedule keeps running. The next occurrence is used rather than an
///   immediate catch-up so a persistently failing store cannot turn every
///   close into another immediate firing.
/// - The failure is counted in `job_rearm_failures_total`, surfaced by
///   `GET /operations/metrics`, so the degradation is observable rather than
///   buried in a log line.
/// - **Both SQLite and memory keep the stale `next_run_at`.**
///   `JobStore::transition_job` persists before `*entry = candidate`, so a
///   failed write advances neither. The in-memory job is what `GET /jobs`
///   serves, so the API reports a `next_run_at` in the *past* while the
///   scheduler is actually armed for the next occurrence, and `jobs-tab.js`
///   renders that past date verbatim — this is the symptom an operator sees
///   first. The next successful run overwrites it; a restart before then
///   replays that tick through the staggered boot catch-up (#1235).
async fn rearm_after_failed_close(
    state: &AppState,
    job_id: JobId,
    cron: &str,
    now: chrono::DateTime<Utc>,
    error: &alms_core::AlmsError,
) {
    state.job_store.record_rearm_failure();
    let Some(next) = cron_utils::next_after(cron, now) else {
        error!(
            %job_id,
            attempts = JOB_REARM_MAX_ATTEMPTS,
            %error,
            "Episode-close job write failed after the retry budget and cron '{cron}' has no \
             future occurrence — the job is stopped (#1233)"
        );
        return;
    };
    let delay = (next - now).to_std().unwrap_or(std::time::Duration::ZERO);
    state
        .scheduler
        .schedule_once(job_id, tokio::time::Instant::now() + delay)
        .await;
    error!(
        %job_id,
        attempts = JOB_REARM_MAX_ATTEMPTS,
        %error,
        next_fire_at = %next,
        "Episode-close job write failed after the retry budget — job re-armed IN MEMORY ONLY. \
         The persisted next_run_at is stale until the next successful run (#1233)"
    );
}

/// The D5 deadline sweep: force-close episodes past their 4-hour deadline
/// with detach-and-complete semantics (the job completes with a deadline
/// note; still-live pending work keeps running, never force-cancelled).
///
/// Races degrade gracefully: a run completing after the sweep removed its
/// episode reports `Untracked` and finishes as a plain run; a later
/// DM/subagent resolution misses and falls back to default routing.
pub(crate) async fn job_episode_sweep_loop(state: AppState) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(
        job_episode::EPISODE_SWEEP_INTERVAL_SECS,
    ));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = state.shutdown_token.cancelled() => break,
            _ = ticker.tick() => {
                for episode in state.job_episodes.take_expired() {
                    warn!(
                        job_id = %episode.job_id,
                        pending = episode.pending_count(),
                        in_flight_runs = episode.in_flight_runs,
                        "Job episode deadline reached — detach-and-complete (#1198 D5)"
                    );
                    close_episode(&state, episode, true).await;
                }
            }
        }
    }
}

/// Character cap for the job-name label shown on the completion card.
const JOB_NAME_MAX_CHARS: usize = 60;

/// Send a job-completion notification to the agent's most recent user-facing
/// session. This makes job runs visible in the chat without creating a full
/// notification run (which would trigger another LLM call).
async fn notify_job_completion(
    state: &AppState,
    agent_id: alms_core::AgentId,
    job_prompt: &str,
    run_id: RunId,
    job_id: JobId,
    episode: Option<&EpisodeCloseStats>,
) {
    // Determine the outcome from the completed run. `fetchable` marks the
    // Completed arm — the only case where the full text lives on
    // `run.output` and is therefore retrievable via GET /runs/{run_id}
    // (#1196). Failed/cancelled text isn't fetchable, so those arms never
    // signal a fetch regardless of capping.
    let (status, raw_summary, fetchable) = match state.run_manager.get_run(run_id) {
        Some(run) => match run.status() {
            RunStatus::Completed => ("success", run.output.unwrap_or_default(), true),
            RunStatus::Failed => (
                "error",
                run.error.unwrap_or_else(|| "unknown error".to_string()),
                false,
            ),
            RunStatus::Cancelled => ("cancelled", "run was cancelled".to_string(), false),
            RunStatus::Queued | RunStatus::Running => {
                // Shouldn't happen — execute_run already returned.
                ("unknown", "run still in progress".to_string(), false)
            }
        },
        None => ("error", "run record not found".to_string(), false),
    };

    // #1198 D5: a deadline-forced close prepends a note so the card (and
    // the persisted marker) say WHY the job completed with work still
    // open. The run-derived status is kept — the final turn itself may
    // have succeeded; only the episode's wait was cut short.
    //
    // N1 (Tim on #1202): the note is applied BEFORE the cap. The SSE
    // constructor re-caps its summary at the same JOB_SUMMARY_MAX_CHARS —
    // capping first and prepending after made an at-cap deadline close
    // diverge between the live SSE payload (note pushed the text over the
    // cap, tail re-clipped) and the persisted marker (kept the full text).
    // Composing first and capping once keeps both surfaces byte-identical.
    let raw_summary = match episode {
        Some(stats) if stats.timed_out => format!(
            "[Episode deadline reached after {}h — {} pending task(s) detached]\n{raw_summary}",
            EPISODE_DEADLINE_SECS / 3600,
            stats.detached,
        ),
        _ => raw_summary,
    };

    // Single cap for both surfaces: JOB_SUMMARY_MAX_CHARS (4000, #1196) so
    // "Show more" reveals a useful chunk inline while a runaway output (or
    // provider error body — S2 #1196) can't land untruncated in session
    // history. `truncated` keeps the #1196 contract: "the full output is
    // fetchable via GET /runs/{run_id}" — only the Completed arm signals it.
    let (summary, capped) =
        crate::sse::truncate_chars(&raw_summary, crate::sse::JOB_SUMMARY_MAX_CHARS);
    let truncated = fetchable && capped;

    // Find the agent's most recent user-facing session (exclude internal sessions).
    let Some(target) = find_user_facing_session(&state.session_manager, agent_id) else {
        debug!(
            "No user-facing session for agent {} — skipping job notification",
            agent_id
        );
        return;
    };
    let target_session_id = target.id;

    // Build the display label for the prompt. Collapse any newlines to spaces
    // FIRST so the label is single-line: the marker text is
    // `[Scheduled job {label}] {job_name}\n{summary}`, and the frontend splits
    // name-vs-summary on the first newline. A prompt with a newline in its
    // leading ~60 chars would otherwise leak its tail into the summary (#1196
    // defect b). Truncation is char-based via `truncate_chars`.
    let single_line_prompt = job_prompt.replace(['\n', '\r'], " ");
    let (job_name, _) = crate::sse::truncate_chars(&single_line_prompt, JOB_NAME_MAX_CHARS);

    // Deep-link handle to the job's hidden session — the same context id
    // `fire_job_run` uses (`job_{job_id}`) so consumers can resolve the run's
    // originating session.
    let job_session_id = format!("job_{}", job_id.0);

    // #1217: the context handle above is NOT a `SessionId` — `GET /session/{id}`
    // (`Path<SessionId>`) can't resolve it, so the card's "Go to job session"
    // button 400s when it navigates by the handle. Resolve the job session's
    // real random `SessionId` here (the same `(agent_id, "job_{id}")` key
    // `fire_job_run` created it under) and emit THAT so the button navigates
    // by a value the session endpoint accepts. `None` if the hidden session
    // isn't resident (e.g. evicted) — the card then just omits the button.
    let job_session_uuid = state
        .session_manager
        .session_id_for_context(agent_id, &job_session_id)
        .map(|sid| sid.0.to_string());

    // Send SSE event to the target session so connected UI clients see it.
    state
        .run_manager
        .send_session_event(
            target_session_id,
            alms_core::RunId::new(), // no associated run on this session
            SseEventData::job_completed(
                target_session_id,
                &job_name,
                status,
                &summary,
                run_id,
                job_id,
                &job_session_id,
                job_session_uuid.as_deref(),
                truncated,
            ),
        )
        .await;

    // Persist a marker message to the session history so it appears on reload.
    let label = match status {
        "success" => "completed",
        "error" => "failed",
        _ => "finished",
    };
    super::markers::persist_lifecycle_marker(
        &state.session_manager,
        target_session_id,
        "job_notification",
        format!("[Scheduled job {label}] {job_name}\n{summary}"),
        // Deep-link handles (#1196): run_id lets the card fetch the full
        // persisted output via GET /runs/{run_id}; job_id / job_session_id
        // identify the firing job and its hidden session. `job_session_uuid`
        // (#1217) is the hidden session's real `SessionId` — the reload mirror
        // of the SSE field, and the value the card's "Go to job session"
        // button navigates by (the `job_session_id` context handle is not a
        // `SessionId`). `truncated` is the authoritative "there is more to
        // fetch" flag the card keys on (the reload mirror of the SSE field).
        {
            let mut meta = serde_json::json!({
                "job_status": status,
                "run_id": run_id.0.to_string(),
                "job_id": job_id.0.to_string(),
                "job_session_id": job_session_id,
                "job_session_uuid": job_session_uuid,
                "truncated": truncated,
            });
            // #1198: episode stats (additive — pre-episode markers simply
            // lack the key, and consumers treat it as optional).
            if let Some(stats) = episode {
                meta["episode"] = serde_json::json!({
                    "turns": stats.turns,
                    "dm_count": stats.dm_count,
                    "subagent_count": stats.subagent_count,
                    "timed_out": stats.timed_out,
                    "detached": stats.detached,
                });
            }
            meta
        },
    );

    info!(
        "Job notification sent to session {} (status={status})",
        target_session_id.0
    );
}

/// Forward a `dm_conversation_ended` signal to the agent's user-facing
/// web-chat session. Two separable concerns:
///
/// - The `dm_conversation_ended` SSE event is emitted **unconditionally**.
///   It is the ONLY path that clears the "Chatting with {peer}" status on
///   the web-chat (`clearAgentPhase`): `dm_lifecycle.rs` emits
///   `dm_conversation_ended` on the DM-session stream (which the operator is
///   not viewing), and the source-session notification run *re-asserts* the
///   DM phase (#688 preservation) rather than clearing it. So the phase-clear
///   is not optional for any agent whose DM has ended (#1215 C1).
///
/// - A persisted `dm_ended_notification` marker is the reloadable visible
///   "DM ended" banner. It is persisted UNLESS the resolved user-facing
///   `target` is itself one of `run_target_session_ids` — i.e. unless the
///   notification run lands on this exact chat, where the run is already the
///   visible notification and a marker would duplicate it ("initiator gets
///   both", #1215). Keying on the FINAL run targets (computed AFTER the #1205
///   episode-routing override) makes the predicate complete over every case:
///   a pure recipient (run on the invisible `notifications:` session), an
///   internal `job_*` source, a DIFFERENT user-facing chat than the target,
///   or an episode-rerouted run on a job session — in all of them `target` is
///   not among the run targets, so the marker persists (#1218 P2). Since #1258
///   an *interrupted* end (cancel, or a run that died mid-turn) produces no run
///   on the trigger's own target. The only run targets it can still have are
///   #1198 job-episode continuations, and `runs/mod.rs` classifies `job_*`
///   sessions as internal, so they can never be the `find_user_facing_session`
///   target — for an interrupted end the marker is therefore always the
///   delivery.
///
/// `detail` is the failure text of an `errored` end. It rides on both surfaces
/// (SSE field + marker metadata) because an interrupted end no longer spends an
/// LLM turn explaining itself — the banner is where the operator learns *why*
/// (#1258).
///
/// Known gap (#1258): an agent with no user-facing session at all — a purely
/// background or channel-driven one — gets `None` from
/// `find_user_facing_session` and this function early-returns. Pre-#1258 an
/// interrupted end at least landed a run on `notifications:{agent}`, so the
/// end was in that agent's history; now it is recorded only as the bus's
/// (reader-invisible) `dm_ended` row on the DM session. A fallback that
/// persists the marker on the trigger's own target when no user-facing
/// session exists would close it.
pub(super) async fn notify_dm_ended_to_webchat(
    state: &AppState,
    agent_id: alms_core::AgentId,
    peer_name: &str,
    reason: &str,
    detail: Option<&str>,
    context_id: &str,
    run_target_session_ids: &[SessionId],
) {
    info!(
        agent_id = %agent_id,
        peer = %peer_name,
        reason = %reason,
        "notify_dm_ended_to_webchat called — looking for user-facing session"
    );

    // Find the agent's most recent user-facing session (exclude internal sessions).
    let Some(target) = find_user_facing_session(&state.session_manager, agent_id) else {
        info!(
            agent_id = %agent_id,
            "No user-facing session for agent — skipping DM ended web-chat notification"
        );
        return;
    };
    let target_session_id = target.id;

    // Decide marker persistence FIRST: suppress it when the notification run
    // lands on THIS exact target session (target is among the run targets),
    // because then the run is already the visible notification here and a
    // marker would duplicate it ("initiator gets both", #1215). Persist in
    // every other case: pure recipient (run on the invisible notifications:
    // session), an internal job_* source, a DIFFERENT user-facing chat than
    // this target, or an episode-rerouted run on a job session (#1205) -- in
    // all of them the run is elsewhere, so the web-chat needs the marker
    // (#1218 P2).
    let persist_marker = !run_target_session_ids.contains(&target_session_id);

    // Emit the phase-clear SSE (unconditional -- the C1 fix: the only path
    // that clears "Chatting with {peer}" on the web-chat). `suppress_banner`
    // mirrors the marker decision: whenever the marker is suppressed, the LIVE
    // `dm_ended` banner is suppressed too, so a live viewer of a source-having
    // chat sees only the notification run, not run + banner (the live half of
    // "initiator gets both", #1215). DM-session emitters never set this, so the
    // DM-session-view banner is unaffected.
    let dummy_run_id = RunId::new();
    state
        .run_manager
        .send_session_event(
            target_session_id,
            dummy_run_id,
            SseEventData::dm_conversation_ended_webchat(
                target_session_id,
                "system",
                peer_name,
                reason,
                context_id,
                !persist_marker,
                detail,
            ),
        )
        .await;

    // Persist the reloadable marker (same condition as the banner suppression).
    if persist_marker {
        // Mirrors the frontend's `DM_END_REASON_LABELS` so the persisted text
        // and the rendered banner agree. `user_cancelled` / `errored` used to
        // fall through raw here because those ends always came with a run that
        // explained them in prose; since #1258 they do not, so the marker is
        // the explanation and must read like one.
        let reason_text = match reason {
            "ignored" => "no further replies",
            "depth_exceeded" => "message limit reached",
            "user_cancelled" => "cancelled by user",
            "errored" => "run failed",
            other => other,
        };
        let content = match detail {
            Some(detail) => format!(
                "[DM conversation ended] Conversation with {peer_name} ended \
                 ({reason_text}: {detail})."
            ),
            None => format!(
                "[DM conversation ended] Conversation with {peer_name} ended ({reason_text})."
            ),
        };
        super::markers::persist_lifecycle_marker(
            &state.session_manager,
            target_session_id,
            "dm_ended_notification",
            content,
            {
                let mut meta = serde_json::json!({
                    "peer": peer_name,
                    "reason": reason,
                    "context_id": context_id,
                });
                // Reload mirror of the SSE `detail` field. Absent (not null)
                // for every non-`errored` end, so pre-#1258 markers keep the
                // exact same shape.
                if let Some(detail) = detail {
                    meta["detail"] = serde_json::json!(detail);
                }
                meta
            },
        );
    }

    info!(
        persist_marker,
        "DM ended notification forwarded to web-chat session {} (peer={peer_name}, reason={reason})",
        target_session_id.0
    );
}

/// Forward a `dm_activity_started` event to the agent's user-facing
/// web-chat session so the status bar can show "Chatting with {peer}".
///
/// This mirrors [`notify_dm_ended_to_webchat`]: find the most recent
/// user-facing session and emit a lightweight SSE event.  No marker
/// message is persisted because DM activity is transient — the status
/// bar resets when the DM run ends.
///
/// See #651.
async fn notify_dm_started_to_webchat(
    state: &AppState,
    agent_id: alms_core::AgentId,
    peer_name: &str,
    context_id: &str,
) {
    let Some(target) = find_user_facing_session(&state.session_manager, agent_id) else {
        debug!(
            agent_id = %agent_id,
            "No user-facing session for agent — skipping DM started notification"
        );
        return;
    };
    let target_session_id = target.id;

    let dummy_run_id = RunId::new();
    state
        .run_manager
        .send_session_event(
            target_session_id,
            dummy_run_id,
            SseEventData::dm_activity_started(target_session_id, peer_name),
        )
        .await;

    debug!(
        "DM activity started forwarded to web-chat session {} (peer={peer_name}, context={context_id})",
        target_session_id.0
    );
}

/// Forward a DM `status` event to the agent's user-facing web-chat
/// session as a `dm_activity_status` event.
///
/// Only key phases (`executing_tools`, `calling_llm`) are forwarded to
/// avoid flooding the webchat stream with noise.
///
/// **Note**: `forward_runtime_events` in `tools.rs` now caches the webchat
/// session lookup and emits `dm_activity_status` events directly, so this
/// function is no longer called. Kept for potential future use.
///
/// See #651.
#[allow(dead_code)]
async fn notify_dm_status_to_webchat(
    session_manager: &alms_session::SessionManager,
    run_manager: &crate::server::RunManager,
    agent_id: alms_core::AgentId,
    peer_name: &str,
    phase: &str,
    detail: Option<String>,
) {
    let Some(target) = find_user_facing_session(session_manager, agent_id) else {
        return;
    };
    let target_session_id = target.id;

    let dummy_run_id = RunId::new();
    run_manager
        .send_session_event(
            target_session_id,
            dummy_run_id,
            SseEventData::dm_activity_status(target_session_id, peer_name, phase, detail),
        )
        .await;
}

// ---------------------------------------------------------------------------
// Subagent completion notifications
// ---------------------------------------------------------------------------

/// Receives background subagent completion events and creates follow-up
/// runs on the parent agent's session so the parent is automatically notified.
///
/// This mirrors `scheduler_fire_loop`: each notification is enqueued via
/// `SessionQueue` to respect per-session FIFO ordering.
pub(crate) async fn completion_notification_loop(
    mut rx: mpsc::UnboundedReceiver<alms_coordinator::SubagentCompletion>,
    state: AppState,
) {
    while let Some(completion) = rx.recv().await {
        let session_id = completion.parent_session_id;
        let agent_id = completion.parent_agent_id;

        // Verify the parent session still exists.
        let context_id = match state.session_manager.get(session_id) {
            Ok(session) => session.context_id,
            Err(_) => {
                warn!(
                    session_id = %session_id.0,
                    task_id = %completion.task_id.0,
                    "Parent session not found for subagent completion notification — skipping"
                );
                continue;
            }
        };

        let status_str = match completion.status {
            alms_coordinator::TaskStatus::Completed => "done",
            alms_coordinator::TaskStatus::Failed => "fail",
            alms_coordinator::TaskStatus::Cancelled => "cancelled",
            _ => "done",
        };

        // ORDERING INVARIANT — DO NOT REORDER (issue #1041, codex P2 on PR #1049):
        // The history marker MUST be persisted BEFORE the SSE
        // `subagent_completed` event is dispatched. `send_session_event`
        // advances the per-session SSE event log (`last_event_id`), and
        // `GET /sessions/{id}/messages` reads `last_event_id` BEFORE it
        // reads message history (see `routes.rs::get_session_messages`
        // comment around the `latest_session_event_id` call). If we fired
        // the SSE event first, a reload landing between the two writes
        // would observe an `last_event_id` past the completion event
        // alongside a history snapshot that lacks the marker. The reconnect
        // would then skip the completion event on replay and Iris's chip
        // rehydration logic would leave the subagent stuck in "running"
        // until another full reload. Persisting the marker first ensures
        // that whenever the SSE event is visible to a reloader, the marker
        // is already in history; live SSE subscribers tolerate the duplicate
        // (they may briefly see the marker before the event, which the
        // frontend dedupes by subagent session id).
        {
            let name = completion.subagent_name.as_deref().unwrap_or("subagent");
            let label = match status_str {
                "fail" => "failed",
                "cancelled" => "cancelled",
                _ => "completed",
            };

            // Build the metadata object with all fields the frontend needs
            // so it can reconstruct the full SubagentCompletionCard
            // (session_id, task, tool_count, duration, summary, token_usage)
            // instead of a plain system message.
            let mut meta = serde_json::json!({
                "subagent_name": name,
                "status": status_str,
                "session_id": completion.subagent_session_id.0.to_string(),
                "summary": &completion.summary,
            });
            // Carry the parent's invoke_agent invocation id (#1125, A1-2) into
            // the persisted marker so a reload that replays history (rather than
            // the live SSE event) can still resolve the completion to the right
            // SubagentBar entry by invocation id. Mirrors the SSE field; omitted
            // when the emitter doesn't carry the id (legacy callers).
            if let Some(inv) = completion.parent_tool_invocation_id {
                meta["tool_invocation_id"] = serde_json::json!(inv.to_string());
            }
            if let Some(ref task) = completion.task_description {
                meta["task_description"] = serde_json::json!(task);
            }
            if let Some(tc) = completion.tool_count {
                meta["tool_count"] = serde_json::json!(tc);
            }
            if let Some(ms) = completion.duration_ms {
                meta["duration_ms"] = serde_json::json!(ms);
            }
            if let Some(ref usage) = completion.token_usage {
                let mut token_usage = serde_json::json!({
                    "prompt_tokens": usage.prompt_tokens,
                    "completion_tokens": usage.completion_tokens,
                });
                // Reasoning tokens only appear on the wire when the provider
                // reports them separately (OpenAI o-series, DeepSeek, xAI).
                // Absent otherwise so non-reasoning subagents stay
                // byte-identical to pre-#768 completion markers.
                if let Some(rt) = usage.reasoning_tokens {
                    token_usage["reasoning_tokens"] = serde_json::json!(rt);
                }
                // Cache tokens (#766) only appear for Anthropic subagents.
                // Same skip-when-None contract as reasoning_tokens — keeps
                // non-Anthropic markers byte-identical to pre-#766.
                if let Some(cc) = usage.cache_creation_input_tokens {
                    token_usage["cache_creation_input_tokens"] = serde_json::json!(cc);
                }
                if let Some(cr) = usage.cache_read_input_tokens {
                    token_usage["cache_read_input_tokens"] = serde_json::json!(cr);
                }
                meta["token_usage"] = token_usage;
            }

            super::markers::persist_lifecycle_marker(
                &state.session_manager,
                session_id,
                "subagent_completion",
                format!("Subagent '{}' {}.", name, label),
                meta,
            );
        }

        // Notify session subscribers that a subagent completed. This
        // updates the SubagentBar and shows a system message BEFORE the
        // notification run starts. Must run AFTER the history marker is
        // persisted — see the ORDERING INVARIANT block above.
        state
            .run_manager
            .send_session_event(
                session_id,
                alms_core::RunId::new(), // no run yet
                SseEventData::subagent_completed(
                    session_id,
                    completion
                        .parent_tool_invocation_id
                        .map(crate::sse::ToolInvocationId),
                    completion.subagent_name.clone(),
                    status_str,
                    &completion.summary,
                    completion.subagent_session_id,
                ),
            )
            .await;

        let notification = format_completion_notification(&completion);

        // #1198 step 5: resolve this completion against open job episodes.
        // A hit removes the pending `Subagent` entry and RESERVES the
        // continuation run in the same atomic step (no quiescence can fire
        // in the gap). Routing needs no override here — the parent session
        // of a job-dispatched subagent IS the job session. A miss (episode
        // already closed by the deadline sweep or cancellation) degrades to
        // the plain pre-#1198 notification run.
        let episode_route = state.job_episodes.resolve_subagent(completion.task_id.0);
        if let Some(ref route) = episode_route {
            info!(
                job_id = %route.job_id,
                task_id = %completion.task_id.0,
                "Subagent completion resolved to open job episode — continuation run"
            );
        }

        info!(
            session_id = %session_id.0,
            task_id = %completion.task_id.0,
            subagent = ?completion.subagent_name,
            "Subagent completion -> creating notification run"
        );

        let Some(run_id) = enqueue_triggered_run(
            &state,
            agent_id,
            session_id,
            notification,
            context_id,
            "subagent".to_string(),
            false, // subagent completion — not a peer message
            episode_route.as_ref().map(|r| r.job_id),
            None, // no conversation ended here — nothing to fold (#1299)
        )
        .await
        else {
            // #1206: cancelled-job teardown — the completion is recorded in
            // history (marker + SSE above) but burns no LLM turn.
            continue;
        };

        debug!(
            run_id = %run_id.0,
            session_id = %session_id.0,
            task_id = %completion.task_id.0,
            "Notification run enqueued"
        );
    }
}

/// Returns the job id when `context_id` addresses a job session
/// (`job_{uuid}`) whose job the operator cancelled via `DELETE /jobs/{id}`
/// — the target shape of the teardown-spawned orphan runs from #1206.
///
/// The explicit intent set is populated only by `DELETE /jobs/{id}` and is
/// checked synchronously by late producers that may hold a pre-cancellation
/// job snapshot. Completed and failed jobs are never inserted, so deadline-
/// detached results remain deliverable after the episode closes.
fn operator_cancelled_job_for_context(state: &AppState, context_id: &str) -> Option<JobId> {
    let raw = context_id.strip_prefix("job_")?;
    let job_id = JobId(uuid::Uuid::parse_str(raw).ok()?);
    state
        .operator_cancelled_jobs
        .contains(&job_id)
        .then_some(job_id)
}

/// Creates a run, registers it (including the #1207 episode `note_run`
/// stamp for job-episode continuations), sends the SSE `run_created` event,
/// and enqueues the run at low priority for execution.
///
/// Shared helper for [`completion_notification_loop`] and [`run_trigger_loop`],
/// which both follow the same create-register-enqueue pattern.
///
/// These producers intentionally wait on one shared queue-consumer loop.
/// Saturation for one agent can therefore delay other agents on that producer,
/// preserving producer FIFO ordering at the cost of cross-agent head-of-line
/// blocking. Callers must remain dedicated producer tasks; never invoke this
/// waiting admission path from inside a queue work item.
///
/// Returns `None` (no run created) when the target is the session of an
/// **operator-cancelled** job — see the #1206 suppression comment inside.
///
/// `pub(super)` for the #1207 regression test in `integration_tests`,
/// which pins the "run id noted on the episode before this returns"
/// contract.
#[allow(clippy::too_many_arguments)] // internal helper; params mirror RunParams + the #1198 job stamp
pub(super) async fn enqueue_triggered_run(
    state: &AppState,
    agent_id: alms_core::AgentId,
    session_id: SessionId,
    input: String,
    context_id: String,
    source_label: String,
    is_peer_message: bool,
    // #1198: when this run continues a job episode, its Run record is
    // stamped with the job id so `cancel_runs_for_job` covers it and the
    // episode hook inside `execute_run` engages at every exit.
    job_id: Option<JobId>,
    // #1299: for a `ConversationEnded` post-end turn, the peer whose
    // conversation just ended. `execute_run` registers `send_message` folded
    // toward that peer so the turn cannot immediately re-open the
    // conversation it was notified about. `None` for every other trigger.
    dm_ended_peer: Option<String>,
) -> Option<RunId> {
    // Wait before taking the cancellation gate or creating any run state.
    // Internal triggers are durable work and must not be silently dropped
    // when the bounded queue is temporarily saturated.
    let reservation = match state.agent_queue.reserve(agent_id).await {
        Ok(reservation) => reservation,
        Err(error) => {
            warn!(
                ?error,
                agent_id = %agent_id.0,
                "Triggered run queue unavailable"
            );
            abandon_episode_continuation(state, job_id).await;
            return None;
        }
    };

    let admission_guard =
        super::lifecycle::acquire_run_admission_guard(&state.run_admission_gates, session_id).await;
    let target_session = match state.session_manager.get(session_id) {
        Ok(session) => session,
        Err(_)
            if context_id.starts_with("notifications:")
                && session_id == SessionId::deterministic(&context_id) =>
        {
            // Notification fallback sessions intentionally begin life at the
            // first trigger. Create the deterministic session inside the same
            // admission boundary as run registration so it cannot produce an
            // orphan. A DELETE that linearizes first may be followed by this
            // new notification, but the resulting run again has an
            // authoritative parent session.
            match state.session_manager.get_or_create_with_id(
                session_id,
                agent_id,
                context_id.clone(),
            ) {
                Ok(session) => session,
                Err(error) => {
                    warn!(
                        session_id = %session_id.0,
                        source = %source_label,
                        error = %error,
                        "Suppressing triggered run because its deterministic session could not be persisted"
                    );
                    drop(admission_guard);
                    drop(reservation);
                    abandon_episode_continuation(state, job_id).await;
                    return None;
                }
            }
        }
        Err(_) => {
            warn!(
                session_id = %session_id.0,
                source = %source_label,
                "Suppressing triggered run because its target session was deleted"
            );
            drop(admission_guard);
            drop(reservation);
            abandon_episode_continuation(state, job_id).await;
            return None;
        }
    };
    if target_session.id != session_id {
        warn!(
            session_id = %session_id.0,
            actual_session_id = %target_session.id.0,
            source = %source_label,
            "Suppressing triggered run because its context resolves to another session"
        );
        drop(admission_guard);
        drop(reservation);
        abandon_episode_continuation(state, job_id).await;
        return None;
    }

    // #1206 (Tim's S3 on PR #1202): suppress post-teardown orphan runs.
    // `DELETE /jobs` tears down an open episode by ending its pending DMs
    // (the DM-sender self-notification trigger routes back onto the job
    // session via D3) and cancelling its pending subagents (the
    // `SubagentCompletion(Cancelled)` notification's parent session IS the
    // job session). Both notification runs are created HERE, asynchronously,
    // AFTER `cancel_runs_for_job` already swept — so merely stamping them
    // with the job id could not stop them from burning a turn post-kill.
    //
    // The suppression keys on OPERATOR-CANCEL INTENT (the
    // `operator_cancelled_jobs` set populated only by `DELETE /jobs`), not
    // on job lifecycle status — see `operator_cancelled_job_for_context`.
    // Consequences of that keying:
    //
    // - Deadline-detached episodes (D5) always deliver their late results:
    //   a detached subagent/DM completing after the 4h sweep produces the
    //   documented orphan run on the job session — for RECURRING jobs
    //   (re-armed `Active`) and for spent ONE-SHOTS (recorded `Completed`
    //   or `Failed`, never operator-cancelled) alike. Keying on status
    //   instead would also have to enumerate every terminal variant; the
    //   intent set is one membership check that cannot drift.
    // - A run targeting the session of an operator-killed job serves no
    //   one (the completion card is never coming), so it is suppressed.
    //   If an episode resolve reserved a continuation for a run suppressed
    //   here (DELETE racing a terminal signal), the reservation is moot —
    //   `cancel_job` removes the episode unconditionally, before its first
    //   await.
    // Serialize this intent check and token registration with DELETE /jobs.
    let cancel_token = CancellationToken::new();
    let job_trigger_cancellation_gate = state.job_trigger_cancellation_gate.lock();
    if let Some(cancelled_job) = operator_cancelled_job_for_context(state, &context_id) {
        info!(
            job_id = %cancelled_job,
            session_id = %session_id.0,
            source = %source_label,
            "Suppressing triggered run targeting an operator-cancelled job's session (#1206)"
        );
        drop(job_trigger_cancellation_gate);
        drop(admission_guard);
        drop(reservation);
        abandon_episode_continuation(state, job_id).await;
        return None;
    }

    let mut run = Run::new(session_id, agent_id, input.clone());
    run.job_id = job_id;
    let run_id = run.run_id;
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    if let Err(error) = state.run_manager.insert_run(run) {
        state.run_manager.remove_cancel_token(run_id);
        drop(job_trigger_cancellation_gate);
        drop(admission_guard);
        drop(reservation);
        error!(
            run_id = %run_id.0,
            error = %error,
            "Triggered run registration failed durably"
        );
        abandon_episode_continuation(state, job_id).await;
        return None;
    }

    // #1207 (Tim's S4 on PR #1202): record an episode continuation's run id
    // IMMEDIATELY after the run exists — before the enqueue below could
    // possibly execute (and instantly terminate) it. Pre-fix `note_run` was
    // called by the callers after this function returned, so an instantly-
    // terminating continuation could close the episode before its id was
    // noted, leaving the closed episode's `runs` set — and with it the
    // completion card's `turns` count / deep-link — missing the final id.
    if let Some(job_id) = job_id {
        state.job_episodes.note_run(job_id, run_id);
    }

    // The low-priority dispatch receipt counts all submitted normal and low
    // work ahead at the channel linearization point. Add the active run,
    // which has already left the pending queue.
    //
    // Note: there is a narrow sub-millisecond TOCTOU window between
    // `pending.fetch_sub(1)` inside the queue handler and
    // `mark_run_as_running` in `execute_run`. During that window both
    // signals read false and `queued_behind` may undercount by 1. The
    // window is bounded by executor dispatch latency and is considered
    // acceptable; closing it would require a separate in-flight counter
    // inside `SessionQueue`.
    drop(job_trigger_cancellation_gate);
    drop(admission_guard);

    let (start_tx, start_rx) = tokio::sync::oneshot::channel();
    let state_clone = state.clone();
    let receipt = match reservation.submit_low(Box::pin(async move {
        let _ = start_rx.await;
        execute_run_guarded(
            state_clone,
            RunParams {
                run_id,
                session_id,
                agent_id,
                input,
                context_id,
                cancel_token,
                is_peer_message,
                is_system_triggered: true,
                input_pre_persisted: false,
                dm_ended_peer,
            },
        )
        .await;
    })) {
        Ok(receipt) => receipt,
        Err(error) => {
            let persistence_error = state
                .run_manager
                .try_mark_run_as_failed(run_id, "Run queue closed before dispatch".to_string())
                .err();
            if let Some(persistence_error) = persistence_error {
                state
                    .run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::run_error(
                            run_id,
                            &format!(
                                "Triggered run dispatch failure could not be persisted: {persistence_error}"
                            ),
                        ),
                    )
                    .await;
            }
            state.run_manager.remove_cancel_token(run_id);
            finish_episode_run(state, job_id, run_id, &[]).await;
            warn!(
                ?error,
                run_id = %run_id.0,
                "Triggered run queue closed before dispatch"
            );
            return None;
        }
    };
    let agent_running = state.run_manager.agent_has_running_run(agent_id);
    let queued_behind = receipt.queued_ahead() + usize::from(agent_running);

    let event_state = state.clone();
    let event_task = tokio::spawn(async move {
        event_state
            .run_manager
            .send_session_event(
                session_id,
                run_id,
                SseEventData::run_created(
                    run_id,
                    session_id,
                    true,
                    Some(source_label),
                    queued_behind,
                ),
            )
            .await;
        let _ = start_tx.send(());
    });
    let _ = event_task.await;

    Some(run_id)
}

/// Template for subagent completion notifications, loaded at compile time from
/// `crates/alms-runtime/prompts/subagent_completed.md`.
const SUBAGENT_COMPLETED_TEMPLATE: &str =
    include_str!("../../../alms-runtime/prompts/subagent_completed.md");

/// Format a human-readable notification message for the parent agent.
pub(super) fn format_completion_notification(c: &alms_coordinator::SubagentCompletion) -> String {
    let status = match c.status {
        alms_coordinator::TaskStatus::Completed => "completed",
        alms_coordinator::TaskStatus::Failed => "failed",
        alms_coordinator::TaskStatus::Cancelled => "cancelled",
        _ => "finished",
    };

    let (label, follow_up) = match &c.subagent_name {
        Some(name) => (
            format!("\"{name}\""),
            format!("Use read_subagent_session(\"{name}\") for the full conversation history."),
        ),
        // #1181: ephemeral / unnamed subagents are readable by session id —
        // point the parent at the exact call that works. Pre-#1181 this said
        // only "the summary is included above", leaving the parent with no
        // discoverable path to the persisted full output when the summary
        // was truncated.
        None => (
            format!("(task {})", c.task_id.0),
            format!(
                "Use read_subagent_session(session_id=\"{}\") for the full conversation history.",
                c.subagent_session_id.0
            ),
        ),
    };

    SUBAGENT_COMPLETED_TEMPLATE
        .replace("{label}", &label)
        .replace("{status}", status)
        .replace("{summary}", &c.summary)
        .replace("{follow_up}", &follow_up)
}

/// Maximum character length for the formatted conversation transcript
/// included in DM-ended notifications. Very long conversations are
/// truncated from the beginning (keeping the most recent messages) so the
/// agent sees the tail of the discussion.
pub(super) const DM_HISTORY_MAX_CHARS: usize = 4000;

/// Template for DM-ended notification with conversation history, loaded at
/// compile time from `crates/alms-runtime/prompts/dm_ended_with_history.md`.
const DM_ENDED_WITH_HISTORY_TEMPLATE: &str =
    include_str!("../../../alms-runtime/prompts/dm_ended_with_history.md");

/// Template for DM-ended notification without history (fallback), loaded at
/// compile time from `crates/alms-runtime/prompts/dm_ended_no_history.md`.
const DM_ENDED_NO_HISTORY_TEMPLATE: &str =
    include_str!("../../../alms-runtime/prompts/dm_ended_no_history.md");

/// Format a human-readable notification message for a DM conversation ending.
///
/// This is used by `run_trigger_loop` when it receives a
/// `MessageSource::ConversationEnded` trigger, and — when `conversation_history`
/// is provided — embeds the full DM transcript so the agent can act immediately
/// without calling `read_messages`.
///
/// `self_notification` selects the phrasing (#1215):
///
/// - `false` (peer notification): `from_name` IS the agent that ended the DM,
///   so "Agent {from_name} ended the conversation" correctly tells the OTHER
///   party who ended it.
/// - `true` (the ender's own self-notification, #556 / #1215): the RECIPIENT is
///   the ender and `from_name` is the PEER, so we must NOT attribute the ending
///   to `from_name`. Uses self-appropriate "Your DM conversation with
///   {from_name} has ended" phrasing instead.
pub(super) fn format_dm_ended_notification(
    from_name: &str,
    reason: ConversationEndReason,
    conversation_history: Option<&str>,
    self_notification: bool,
) -> String {
    let reason_text = if self_notification {
        // The recipient IS the ender — never blame `from_name` (the peer).
        match &reason {
            ConversationEndReason::Ignored => {
                format!(
                    "Your DM conversation with agent \"{from_name}\" has ended (you chose not to reply)."
                )
            }
            ConversationEndReason::DepthExceeded => {
                format!(
                    "Your DM conversation with agent \"{from_name}\" ended \
                     because the maximum message depth was reached."
                )
            }
            ConversationEndReason::UserCancelled => {
                "Your DM conversation was cancelled by the user.".to_string()
            }
            ConversationEndReason::Errored { message, .. } => {
                format!(
                    "Your DM conversation with agent \"{from_name}\" ended \
                     because the run failed: {message}"
                )
            }
        }
    } else {
        match &reason {
            ConversationEndReason::Ignored => {
                format!("Agent \"{from_name}\" ended the conversation (chose not to reply).")
            }
            ConversationEndReason::DepthExceeded => {
                format!(
                    "The conversation with agent \"{from_name}\" was terminated \
                     because the maximum message depth was reached."
                )
            }
            ConversationEndReason::UserCancelled => {
                "The DM conversation was cancelled by the user.".to_string()
            }
            ConversationEndReason::Errored { message, .. } => {
                format!(
                    "The conversation with agent \"{from_name}\" ended because \
                     the run failed: {message}"
                )
            }
        }
    };

    match conversation_history {
        Some(history) if !history.is_empty() => DM_ENDED_WITH_HISTORY_TEMPLATE
            .replace("{reason}", &reason_text)
            .replace("{history}", history),
        _ => {
            // Fallback: no history available (session already cleaned up,
            // or error reading it). Point the agent at read_messages.
            DM_ENDED_NO_HISTORY_TEMPLATE
                .replace("{reason}", &reason_text)
                .replace("{from}", from_name)
        }
    }
}

/// Format a DM session's messages into a human-readable conversation
/// transcript suitable for embedding in a notification.
///
/// Only text messages are included (tool calls, tool results, images, and
/// system markers like `dm_ended` are skipped). Each message is formatted
/// as:
///
/// ```text
/// [HH:MM] agent_name: message text
/// ```
///
/// The output is truncated to [`DM_HISTORY_MAX_CHARS`] characters. When
/// truncation is needed, the oldest messages are dropped and a leading
/// note indicates how many messages were omitted.
pub(super) fn format_dm_conversation_history(messages: &[alms_session::Message]) -> String {
    // Collect renderable lines (only text messages with content).
    let mut lines: Vec<String> = Vec::new();

    for msg in messages {
        // Use the centralised filter from alms-tools to skip non-text,
        // empty, and synthetic markers — eliminates duplicated logic.
        // See #627 (persist_lifecycle_marker consolidation).
        if alms_tools::dm_filter::is_synthetic_marker(msg) {
            continue;
        }

        // After the filter, content is guaranteed to be non-empty text.
        let text = match &msg.content {
            alms_session::Content::Text(t) => t.as_str(),
            _ => continue, // defensive — should not reach here
        };

        // Extract sender name from metadata, or fall back to role.
        let sender = msg
            .metadata
            .as_ref()
            .and_then(|m| m.get("from_agent"))
            .and_then(|v| v.as_str())
            .unwrap_or(match msg.role {
                alms_session::Role::User => "user",
                alms_session::Role::Assistant => "assistant",
                alms_session::Role::System => "system",
                alms_session::Role::Tool => "tool",
            });

        let ts = msg.timestamp.0.format("%H:%M");
        lines.push(format!("[{ts}] {sender}: {text}"));
    }

    if lines.is_empty() {
        return String::new();
    }

    // Build the full transcript and truncate from the front if needed.
    let full = lines.join("\n");
    if full.len() <= DM_HISTORY_MAX_CHARS {
        return full;
    }

    // Walk from the end to find how many lines fit within the budget,
    // leaving room for the "[N earlier messages omitted]" prefix.
    let omitted_prefix_budget = 60; // generous estimate
    let budget = DM_HISTORY_MAX_CHARS.saturating_sub(omitted_prefix_budget);
    let mut included_start = lines.len();
    let mut accumulated = 0usize;
    for (i, line) in lines.iter().enumerate().rev() {
        // +1 for the newline separator
        let cost = line.len() + if i < lines.len() - 1 { 1 } else { 0 };
        if accumulated + cost > budget {
            break;
        }
        accumulated += cost;
        included_start = i;
    }

    let omitted = included_start;
    let truncated_lines = &lines[included_start..];
    format!(
        "[{omitted} earlier message(s) omitted]\n{}",
        truncated_lines.join("\n")
    )
}

// ---------------------------------------------------------------------------
// RunTrigger loop (peer messaging)
// ---------------------------------------------------------------------------

/// The peer whose DM with the trigger's recipient just ended, if any (#1299).
///
/// Used by [`plan_triggered_runs`] to stamp every run a trigger produces with
/// the one recipient it may not `send_message`. `MAX_DM_DEPTH` bounds one
/// conversation and `end_conversation` resets it, so this is the only thing
/// standing between a pair and an unbounded cap → end → re-open cycle.
///
/// **`from_name` is the right name for both `ConversationEnded` triggers the
/// bus emits, and it means opposite things in them.** On the peer
/// notification `from_name` is the ENDER and the recipient is the peer; on
/// the ender's own #556 / #1215 self-notification the recipient is the ender
/// and `from_name` is the PEER. Either way it names the agent that is *not*
/// `trigger.agent_id` — exactly the one a `send_message` from this turn
/// would re-open with.
///
/// Three things hold that invariant up. Both production emitters live in
/// `end_conversation_locked` (`bus.rs`) and say so in comments; the bus tests
/// pin both shapes (`test_initiator_gets_self_notification_when_ending_dm`
/// and its depth-exceeded / ignore siblings); and the #1198 episode lookup in
/// `run_trigger_loop` already depends on it — `SessionId::deterministic_dm(
/// from_name, peer_name_resolved)` resolves the right DM session on both
/// shapes only because `from_name` is always the other party.
///
/// The signature carries a fourth: taking only the source, this function
/// *cannot* see the recipient, so the "read `from_name` as the ender in both
/// shapes" mis-implementation is not expressible here. Keep the parameter
/// list narrow — it is doing as much work as the tests are.
///
/// The two non-DM sources fold nothing: a live peer DM turn folds on its own
/// `is_peer_message` arm, keyed on the `dm:` context id, and a subagent
/// completion has no ended conversation. The match is exhaustive so a new
/// source has to state its answer.
fn dm_ended_peer_for_source(
    source: &alms_coordinator::message_bus::MessageSource,
) -> Option<String> {
    use alms_coordinator::message_bus::MessageSource;
    match source {
        MessageSource::ConversationEnded { from_name, .. } => Some(from_name.clone()),
        MessageSource::Agent { .. } | MessageSource::SubagentCompletion => None,
    }
}

/// One run a [`RunTrigger`](alms_coordinator::message_bus::RunTrigger) should
/// produce, and the fold it carries.
///
/// The fold rides ON the target rather than beside it (#1299): a trigger can
/// fan out to several runs on different sessions, and every one of them is a
/// turn in which the ended peer must not be addressable. Making it a field
/// removes the possibility of a caller applying it to some targets and not
/// others.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TriggeredRunTarget {
    pub(super) session_id: SessionId,
    pub(super) context_id: String,
    /// Set when this run continues a job episode (#1198 / #1205).
    pub(super) job_id: Option<JobId>,
    /// The peer this run may not `send_message` — see
    /// [`dm_ended_peer_for_source`] and `lifecycle::apply_send_message_fold`.
    pub(super) dm_ended_peer: Option<String>,
}

/// Decide which runs a trigger produces, and what each of them may not
/// message.
///
/// Pure given its four decision inputs (`source`, the trigger's own target,
/// and `episode_routes`); `agent_id` and `source_label` are log context only.
/// `episode_routes` is computed by the caller because resolving it *reserves*
/// the continuations — the side effect stays outside, the decision inside.
///
/// # Routing precedence
///
/// 1. **A resolved episode wins** (#1198 / #1205). Each route pins its
///    continuation to the JOB session, superseding the `source_sessions`-
///    derived target — see design doc § D3 for why the tracker, not the bus,
///    is the primary router. One run per resolved episode: when two episodes
///    of the same agent were pending on the ended DM session, BOTH get their
///    continuation, each on its own job session with its own job id.
/// 2. **Otherwise an INTERRUPTED end produces nothing** (#1258). An operator
///    cancel, or a run that died mid-turn, has no outcome to relay, so it
///    must not put an unrequested run on the operator's session; the
///    persisted marker is the delivery. An end whose run *completed* —
///    including `errored` with an unusable result — still gets its run, so
///    its transcript reaches the operator's chat.
/// 3. **Otherwise the trigger's own target**, byte-for-byte as pre-#1198.
///
/// The episode override deliberately beats the suppression: a job
/// continuation resumes the JOB, not the DM, and dropping it would stall the
/// job until its deadline. So the suppression applies only to the trigger's
/// own target, exactly where the operator sits.
///
/// # Why the fold is decided here
///
/// That precedence is also what makes the #1299 fold subtle. The arm that
/// survives the interrupted-end suppression is the job arm — so a job
/// continuation can be the ONLY run an end produces, it lands on a `job_*`
/// session that names no peer, and it re-opens with nobody watching. Deriving
/// the fold from `context_id` at the registration site would miss exactly
/// that arm. Deriving it here, from the source, and stamping it on every
/// target makes "every run of an ended DM is folded" one statement instead of
/// a coincidence between two files.
///
/// # What is and is not covered by tests
///
/// This function is pinned (routing precedence, and the peer on every target
/// including the job continuations), and so is the fold itself
/// (`lifecycle::apply_send_message_fold`). What remains uncovered is the
/// stretch between them, where the peer is a plain value being passed along:
/// `enqueue_triggered_run`'s parameter, `RunParams::dm_ended_peer`, and
/// `execute_run`'s call to `apply_send_message_fold`. Substituting `None` at
/// any of the three disarms the fold with every test still green.
///
/// The reasons differ, and only the third is structural: a test cannot reach
/// `execute_run`'s call because tool registration needs an LLM-backed runtime
/// the gateway tests do not have. The first two are merely *unpinned* — they
/// are reachable, and a future harness could cover them. Do not read the
/// runtime argument as covering all three.
///
/// What the plan shape buys is not uniformity — the old code had that too, by
/// having exactly one call site — but uniformity at the TYPE level: the peer
/// is a field of `TriggeredRunTarget`, so a new routing arm has to state it
/// rather than inherit it by luck, and the plan is a value tests can inspect,
/// which is what makes a per-arm mistake killable at all. (The arms that
/// exist are pinned; a new arm stating `None` is a one-token disarm no
/// current row catches.) Keep the peer on the target.
pub(super) fn plan_triggered_runs(
    source: &alms_coordinator::message_bus::MessageSource,
    trigger_session_id: SessionId,
    trigger_context_id: &str,
    episode_routes: Vec<super::job_episode::ContinuationRoute>,
    agent_id: alms_core::AgentId,
    source_label: &str,
) -> Vec<TriggeredRunTarget> {
    use alms_coordinator::message_bus::MessageSource;

    let dm_ended_peer = dm_ended_peer_for_source(source);

    // Computed from the source directly rather than assigned inside a match
    // arm, so a future arm cannot silently inherit the `false` default.
    let end_was_interrupted = matches!(
        source,
        MessageSource::ConversationEnded { reason, .. } if reason.is_interrupted()
    );

    if !episode_routes.is_empty() {
        return episode_routes
            .into_iter()
            .map(|route| {
                info!(
                    job_id = %route.job_id,
                    dm_target = %trigger_session_id.0,
                    job_session = %route.job_session_id.0,
                    "ConversationEnded resolved to open job episode — routing \
                     continuation onto the job session (#1198)"
                );
                TriggeredRunTarget {
                    session_id: route.job_session_id,
                    context_id: route.context_id,
                    job_id: Some(route.job_id),
                    dm_ended_peer: dm_ended_peer.clone(),
                }
            })
            .collect();
    }

    if end_was_interrupted {
        info!(
            session_id = %trigger_session_id.0,
            agent_id = %agent_id.0,
            source = %source_label,
            "DM end was interrupted (cancelled, or the run died mid-turn) \
             — delivering the notification as a marker, starting no run \
             (#1258)"
        );
        return Vec::new();
    }

    vec![TriggeredRunTarget {
        session_id: trigger_session_id,
        context_id: trigger_context_id.to_string(),
        job_id: None,
        dm_ended_peer,
    }]
}

/// Processes `RunTrigger` events from the `MessageBus`.
///
/// Each trigger creates a run on the target agent's session, reusing the
/// same `execute_run` path as user-initiated and notification runs.
///
/// For `Agent` triggers (peer DMs), the message has already been persisted
/// to the shared DM session by the `MessageBus`; we pass `is_peer = true`
/// so `execute_run` uses `run_on_session` (no double-write).
///
/// For `ConversationEnded` triggers, the notification text has NOT been
/// persisted — the MessageBus only wrote a `dm_ended` marker to the DM
/// session, not to the notification session.  We format a richer
/// notification here and pass `is_peer = false` so `execute_run` uses
/// `runtime.run()`, which persists the input to the notification session.
///
/// # Interrupted ends get no run (#1258)
///
/// A `ConversationEnded` whose reason
/// [`is_interrupted`](alms_tools::message_sender::ConversationEndReason::is_interrupted)
/// — the DM run was cancelled, or it died mid-turn — creates **no**
/// notification run. It is delivered as the persisted
/// `dm_ended_notification` marker plus its live banner instead, both of which
/// `notify_dm_ended_to_webchat` already produces for every routing where the
/// run lands elsewhere.
///
/// The bug this fixes: a DM run dying on an upstream 429 put a fresh,
/// unrequested run on the operator's web-chat 470ms after they cancelled a run
/// on that same session — indistinguishable from the cancel having been
/// ignored. The turn bought nothing: the run that was going to produce this
/// DM's outcome never finished, so all the notification could say was "the DM
/// stopped", which the marker states directly and for free.
///
/// # Why the cut is "was the turn cut short", not "is the transcript empty"
///
/// What makes a notification run load-bearing is transcript content that never
/// reached the operator's web-chat: in the #556 flow the peer's replies live
/// only in the `dm:` session, and the notification run is what relays them
/// (the #429 history embedding). So the tempting predicate is "does the DM
/// have a transcript" — and it does not work:
///
/// - `MessageBus::end_conversation` refuses to run at all unless the DM
///   session exists, and the session only exists because a `send_message`
///   persisted a message into it. The initiating message alone makes the
///   history non-empty, so "transcript is empty" is essentially never true.
/// - In the #1258 incident itself the DM held the peer's opening message —
///   the ended run was the *recipient's*. A transcript predicate would not
///   have suppressed the reported run.
///
/// The predicate that actually separates the cases is whether a **run reached
/// the end of its turn**, which is what `is_interrupted` encodes:
///
/// - `ignored` / `depth_exceeded` — a run completed and the DM ran its
///   course. Keeps its run.
/// - `errored { interrupted: false }` — a run completed but its result was
///   unusable (`dm_lifecycle` Exit 3's "no deliverable reply", or a failed
///   final delivery hop). Earlier turns may have been delivered and exist
///   only in the DM session, so this keeps its run too. Without this edge the
///   fix would silently drop the very transcripts it was designed to protect.
/// - `errored { interrupted: true }` — the run died (LLM failure, panic,
///   setup failure, teardown persistence failure). No run.
/// - `user_cancelled` — unconditional, regardless of transcript: the operator
///   asked for work on this session to stop, and starting a turn is the one
///   thing they said not to do. The transcript is not destroyed — it stays in
///   the DM session and the DM conversation view still renders it.
///
/// The one exception is a #1198 job-episode continuation: those runs exist to
/// resume the *job*, not to narrate the DM, so an interrupted end that resolves
/// an open episode still fires them (otherwise the job stalls until its
/// deadline).
///
/// # Consequence: an interrupted end is invisible to the agent
///
/// Recorded because it is invisible from the diff. After an interrupted end
/// there is no agent-visible signal anywhere:
///
/// - the `dm_ended_notification` marker is `Role::System` + `synthetic`, so
///   `strip_mid_history_system_markers` removes it before the provider (and
///   the `notification_input` user message that `markers.rs` documents as the
///   agent's copy is exactly the thing this suppression skips);
/// - the bus's `dm_ended` record is empty-text, so `dm_filter`'s
///   `is_synthetic_marker` hides it from `read_messages` / `read_session`.
///
/// So the bus state is consistent — depth reset, tombstone written, neither
/// side can keep sending — but ask the agent "what did the peer say?" and it
/// does not know the conversation ended. The operator is told; the agent is
/// not. That is the accepted trade for #1258: the operator is the one who
/// cancelled, is watching, and can open the DM view. If the agent ever needs
/// telling without spending a turn, the machinery is `persist_error_marker`
/// (#874), which survives the strip pass and is rewritten into an `[Error] …`
/// user message on the next turn.
pub(crate) async fn run_trigger_loop(
    mut rx: mpsc::Receiver<alms_coordinator::message_bus::RunTrigger>,
    state: AppState,
) {
    use alms_coordinator::message_bus::MessageSource;

    while let Some(trigger) = rx.recv().await {
        let session_id = trigger.session_id;
        let agent_id = trigger.agent_id;
        let context_id = trigger.context_id;

        // #1218 P2 (#3): the ConversationEnded arm stashes what the web-chat
        // DM-ended forward needs — `(from_name, reason_str, detail,
        // dm_context)` — and the forward itself is deferred until AFTER the
        // final run routing (`run_targets`) is known, so the marker persists
        // unless the run itself lands on the marker target (see the forward
        // below the match).
        let mut dm_ended_webchat: Option<(String, String, Option<String>, String)> = None;

        // #1258 (was this end interrupted?) and #1299 (which peer the runs
        // may not message) are both read off the source, and both belong to
        // the routing decision rather than to any one target — so they live
        // inside `plan_triggered_runs` below, which owns both.

        // Build a source label for SSE `run_created` events and determine
        // whether this is a peer DM run (which needs the DM addendum) or
        // a notification run (which must NOT get the DM addendum).
        // `dm_peer_name` is captured for DM runs so we can forward a
        // lightweight activity event to the agent's webchat session (#651).
        let (source_label, is_peer, input, dm_peer_name, episode_routes) = match &trigger.source {
            MessageSource::Agent { from_name, .. } => (
                format!("peer:{from_name}"),
                true,
                // Peer DM: input already persisted by MessageBus — pass it
                // through so the Run record has a copy.
                trigger.input,
                Some(from_name.clone()),
                Vec::new(),
            ),
            MessageSource::SubagentCompletion => (
                "subagent".to_string(),
                false,
                trigger.input,
                None,
                Vec::new(),
            ),
            MessageSource::ConversationEnded {
                from_name,
                reason,
                self_notification,
                ..
            } => {
                // Resolve the peer (notification recipient) name for SSE
                // events and DM context reconstruction.
                let peer_name_resolved = state
                    .session_manager
                    .store()
                    .and_then(|store| store.load_agent_by_id(agent_id).ok())
                    .flatten()
                    .map(|r| r.name);

                // -- Emit dm_conversation_ended SSE for depth-exceeded (#419) --
                //
                // The ignore_message path emits this event in execute_run
                // (line ~967). The depth-exceeded path calls
                // end_conversation deep inside MessageBus::send(), which
                // has no access to SSE infrastructure. We emit the event
                // here instead, since the ConversationEnded trigger
                // carries all the information we need.
                if matches!(reason, ConversationEndReason::DepthExceeded) {
                    if let Some(ref peer_name) = peer_name_resolved {
                        let dm_context = alms_core::dm_context_id(from_name, peer_name);
                        let dm_session_id = SessionId::deterministic_dm(from_name, peer_name);

                        info!(
                            from = %from_name,
                            peer = %peer_name,
                            dm_session = %dm_session_id.0,
                            "Emitting dm_conversation_ended SSE for depth-exceeded"
                        );

                        // Use a dummy RunId because the notification run has
                        // not been created yet at this point.
                        let dummy_run_id = RunId::new();
                        state
                            .run_manager
                            .send_session_event(
                                dm_session_id,
                                dummy_run_id,
                                SseEventData::dm_conversation_ended(
                                    dm_session_id,
                                    from_name,
                                    peer_name,
                                    &reason.to_string(),
                                    &dm_context,
                                ),
                            )
                            .await;
                    } else {
                        warn!(
                            agent_id = %agent_id.0,
                            from = %from_name,
                            "Skipping dm_conversation_ended SSE for depth-exceeded: \
                             agent not found in registry, cannot resolve peer name"
                        );
                    }
                }

                // -- Stash the web-chat DM-ended forward (made after routing) --
                //
                // The forward is deferred until the FINAL run routing is known
                // (#1218 P2 #3). The reloadable `dm_ended_notification` marker
                // must be suppressed only when the notification RUN itself lands
                // on the marker target, and the #1205 episode-routing override
                // below can reroute the run to a job session AFTER this arm. So
                // we stash the fields here and forward once `run_targets` is
                // built. The phase-clear SSE (the C1 fix — the only path that
                // clears "Chatting with {peer}" on the web-chat) is emitted
                // there too, still unconditionally.
                {
                    let reason_str = reason.to_string();
                    let dm_context = peer_name_resolved
                        .as_ref()
                        .map(|peer_name| alms_core::dm_context_id(from_name, peer_name))
                        .unwrap_or_default();
                    dm_ended_webchat = Some((
                        from_name.clone(),
                        reason_str,
                        reason.detail().map(str::to_string),
                        dm_context,
                    ));
                }

                // -- Fetch DM conversation history (#429) --
                //
                // Resolve the DM session and format its message history so
                // the notification includes the full transcript. This saves
                // the agent an LLM round-trip that would otherwise be spent
                // calling read_messages.
                let conversation_history = peer_name_resolved.as_ref().and_then(|peer_name| {
                    let dm_session_id = SessionId::deterministic_dm(from_name, peer_name);
                    match state.session_manager.get_history(dm_session_id) {
                        Ok(messages) => {
                            let formatted = format_dm_conversation_history(&messages);
                            if formatted.is_empty() {
                                None
                            } else {
                                Some(formatted)
                            }
                        }
                        Err(e) => {
                            warn!(
                                dm_session = %dm_session_id.0,
                                error = %e,
                                "Failed to fetch DM history for notification — \
                                 falling back to read_messages hint"
                            );
                            None
                        }
                    }
                });

                // #1198 step 5: resolve this DM terminal signal against open
                // job episodes. Hits only for the JOB AGENT's trigger (the
                // same DM session appears in both participants' triggers —
                // the agent check inside `resolve_dm` keeps the peer's
                // continuation out of the job). Each hit atomically removes
                // the pending entry and reserves a continuation run; the
                // routing override below then pins each run to its job
                // session so the agent resumes with its full job context.
                //
                // #1205: `resolve_dm` returns EVERY episode pending on the
                // DM session (the deterministic session id means two jobs
                // of the same agent can await the same conversation) — one
                // continuation run is enqueued per resolved episode.
                let episode_routes = peer_name_resolved
                    .as_ref()
                    .map(|peer_name| {
                        let dm_session_id = SessionId::deterministic_dm(from_name, peer_name);
                        state.job_episodes.resolve_dm(dm_session_id, agent_id)
                    })
                    .unwrap_or_default();

                (
                    format!("notification:dm_ended:{from_name}"),
                    // NOT a peer message — the notification run should not get
                    // the DM addendum injected (it tells the agent to use
                    // send_message/ignore_message, which is wrong here).
                    false,
                    // Format a richer notification that includes the reason,
                    // the DM conversation transcript (when available), and a
                    // follow-up hint.
                    format_dm_ended_notification(
                        from_name,
                        reason.clone(),
                        conversation_history.as_deref(),
                        *self_notification,
                    ),
                    None,
                    episode_routes,
                )
            }
        };

        // #1299 / #1258 / #1198: which runs this trigger produces, and what
        // each of them may not message. One plan, so the routing precedence
        // and the fold cannot drift apart — see `plan_triggered_runs`.
        let run_targets = plan_triggered_runs(
            &trigger.source,
            session_id,
            &context_id,
            episode_routes,
            agent_id,
            &source_label,
        );

        // #1218 P2 (#3): forward the DM-ended signal to the agent's web-chat
        // now that the FINAL run routing is known. The `dm_conversation_ended`
        // phase-clear SSE is emitted unconditionally (the C1 fix — the only
        // path that clears "Chatting with {peer}" on the web-chat). The
        // reloadable `dm_ended_notification` marker persists UNLESS the marker
        // target (`find_user_facing_session`) is itself one of the run targets
        // — i.e. unless the notification run is already the visible
        // notification in that same chat. When #1205 episode routing reroutes
        // the run to a job session, the web-chat is not a run target, so the
        // marker persists.
        if let Some((from_name, reason_str, detail, dm_context)) = dm_ended_webchat {
            let run_target_ids: Vec<SessionId> = run_targets.iter().map(|t| t.session_id).collect();
            notify_dm_ended_to_webchat(
                &state,
                agent_id,
                &from_name,
                &reason_str,
                detail.as_deref(),
                &dm_context,
                &run_target_ids,
            )
            .await;
        }

        for target in run_targets {
            info!(
                session_id = %target.session_id.0,
                agent_id = %agent_id.0,
                source = %source_label,
                "RunTrigger -> creating run"
            );

            // The #1207 episode `note_run` stamp happens inside
            // `enqueue_triggered_run`, right after the run is inserted.
            //
            // #1299: the fold travels ON the target, not beside it. Every run
            // the plan produces carries the peer it may not message, so the
            // trigger's own target and each job-episode continuation cannot
            // diverge here — there is no separate variable to forget.
            enqueue_triggered_run(
                &state,
                agent_id,
                target.session_id,
                input.clone(),
                target.context_id,
                source_label.clone(),
                is_peer,
                target.job_id,
                target.dm_ended_peer,
            )
            .await;
        }

        // Forward a lightweight "DM activity started" event to the agent's
        // webchat session so the status bar can show "Chatting with {peer}".
        // This mirrors the `notify_dm_ended_to_webchat` pattern (#651).
        if let Some(peer_name) = dm_peer_name {
            notify_dm_started_to_webchat(&state, agent_id, &peer_name, &context_id).await;
        }
    }
}

// ---------------------------------------------------------------------------
// DM event loop (live message SSE forwarding, #632)
// ---------------------------------------------------------------------------

/// Receives [`DmEvent`] notifications from the `MessageBus` and emits
/// `dm_message` SSE events to any web UI client watching the DM session.
///
/// Without this loop, DM messages are invisible during live viewing and only
/// appear on page reload. See #632 bugs 1 and 4.
pub(crate) async fn dm_event_loop(
    mut rx: tokio::sync::mpsc::Receiver<alms_coordinator::message_bus::DmEvent>,
    state: AppState,
) {
    while let Some(event) = rx.recv().await {
        debug!(
            session_id = %event.session_id.0,
            from = %event.from_agent,
            "DmEvent -> emitting dm_message SSE"
        );

        // Use a dummy RunId since dm_message is a session-level event not
        // tied to a specific run.
        let dummy_run_id = alms_core::RunId::new();
        state
            .run_manager
            .send_session_event(
                event.session_id,
                dummy_run_id,
                SseEventData::dm_message(
                    event.session_id,
                    &event.from_agent,
                    &event.from_agent_id.0.to_string(),
                    &event.message,
                    event.ts,
                ),
            )
            .await;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
// Reroute-prevention tests have been consolidated into integration_tests.rs.
// See `notification_stays_on_invisible_session_when_no_source` and siblings.

#[cfg(test)]
mod tests {
    use super::*;
    use alms_coordinator::message_bus::{MessageSource, RunTrigger};
    use alms_core::AgentId;
    use alms_tools::message_sender::ConversationEndReason;

    // -- #1299: which peer the post-end turn may not message -------------
    //
    // `from_name` names the ENDER on the peer notification and the PEER on
    // the ender's own self-notification. Reading it as "the ender" in both
    // would fold the wrong agent on the #556 / #1215 path: the recipient's
    // own name (a no-op fold, leaving the loop open) instead of the peer's.
    // These two rows pin the pair of readings against each other.

    /// A `ConversationEnded` source with the given interruption.
    fn ended(from_name: &str, interrupted: bool) -> MessageSource {
        MessageSource::ConversationEnded {
            from_agent: AgentId::new(),
            from_name: from_name.to_string(),
            reason: if interrupted {
                ConversationEndReason::UserCancelled
            } else {
                ConversationEndReason::Ignored
            },
            self_notification: false,
            source_session_id: None,
        }
    }

    fn job_route(job_id: JobId) -> super::super::job_episode::ContinuationRoute {
        super::super::job_episode::ContinuationRoute {
            job_id,
            job_session_id: SessionId::new(),
            context_id: format!("job_{}", job_id.0),
        }
    }

    #[test]
    fn dm_ended_peer_is_the_ender_on_the_peer_notification() {
        let peer_notification = MessageSource::ConversationEnded {
            from_agent: AgentId::new(),
            from_name: "alice".to_string(),
            reason: ConversationEndReason::Ignored,
            // alice ended it; the recipient of this trigger is bob.
            self_notification: false,
            source_session_id: None,
        };
        assert_eq!(
            dm_ended_peer_for_source(&peer_notification).as_deref(),
            Some("alice"),
            "bob's post-end turn must not be able to message alice back"
        );
    }

    #[test]
    fn dm_ended_peer_is_the_peer_on_the_ender_self_notification() {
        let self_notification = MessageSource::ConversationEnded {
            from_agent: AgentId::new(),
            from_name: "bob".to_string(),
            // alice ended it and is the RECIPIENT here; from_name is the peer.
            self_notification: true,
            reason: ConversationEndReason::Ignored,
            source_session_id: None,
        };
        assert_eq!(
            dm_ended_peer_for_source(&self_notification).as_deref(),
            Some("bob"),
            "the ender's own turn must not be able to re-open with the peer \
             it just stopped talking to (#556 / #1215)"
        );
    }

    #[test]
    fn non_dm_end_triggers_fold_nothing() {
        assert_eq!(
            dm_ended_peer_for_source(&MessageSource::Agent {
                from_agent: AgentId::new(),
                from_name: "alice".to_string(),
            }),
            None,
            "a live DM turn folds on its own `is_peer_message` arm"
        );
        assert_eq!(
            dm_ended_peer_for_source(&MessageSource::SubagentCompletion),
            None
        );
    }

    // -- #1299 / #1258 / #1198: the run plan ------------------------------
    //
    // These rows pin what `run_trigger_loop` actually hands to
    // `enqueue_triggered_run`: the routing precedence AND the fold that
    // rides on each target. Before the plan function existed, the peer was
    // a separate local applied at the call site, and substituting `None`
    // there disarmed the fold with every test still green.

    #[test]
    fn plan_stamps_the_ended_peer_on_the_triggers_own_target() {
        let session_id = SessionId::new();
        let plan = plan_triggered_runs(
            &ended("alice", false),
            session_id,
            "notifications:bob",
            Vec::new(),
            AgentId::new(),
            "notification:dm_ended:alice",
        );
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].session_id, session_id);
        assert_eq!(plan[0].context_id, "notifications:bob");
        assert_eq!(plan[0].job_id, None);
        assert_eq!(
            plan[0].dm_ended_peer.as_deref(),
            Some("alice"),
            "the post-end turn must be handed the peer it may not message"
        );
    }

    /// The arm that made #1299 worth fixing.
    ///
    /// A resolved job episode reroutes the continuation onto the `job_*`
    /// session — a context that names no peer — and this row proves the plan
    /// *delivers* the peer there, not merely that the fold would honour one
    /// if given it. Without this the job arm's coverage stopped at
    /// necessity: the gateway fold test hands `Some("alice")` in directly.
    #[test]
    fn plan_carries_the_ended_peer_onto_every_job_episode_continuation() {
        let routes = vec![job_route(JobId::new()), job_route(JobId::new())];
        let job_sessions: Vec<SessionId> = routes.iter().map(|r| r.job_session_id).collect();

        let plan = plan_triggered_runs(
            &ended("alice", false),
            SessionId::new(),
            "notifications:bob",
            routes,
            AgentId::new(),
            "notification:dm_ended:alice",
        );

        assert_eq!(
            plan.len(),
            2,
            "#1205: one continuation per resolved episode"
        );
        for (target, job_session) in plan.iter().zip(job_sessions) {
            assert_eq!(
                target.session_id, job_session,
                "#1198: the continuation is pinned to its own job session"
            );
            assert!(target.job_id.is_some());
            assert_eq!(
                target.dm_ended_peer.as_deref(),
                Some("alice"),
                "a job continuation of an ended DM re-opens with nobody \
                 watching — it must be folded like any other post-end turn"
            );
        }
    }

    /// The precedence, and the reason the job arm needs the fold most: an
    /// interrupted end suppresses the trigger's own target but NOT a job
    /// continuation, so the folded job run can be the only run an end
    /// produces.
    #[test]
    fn plan_suppresses_only_the_triggers_own_target_when_the_end_was_interrupted() {
        let interrupted = ended("alice", true);

        assert!(
            plan_triggered_runs(
                &interrupted,
                SessionId::new(),
                "web-chat-bob",
                Vec::new(),
                AgentId::new(),
                "notification:dm_ended:alice",
            )
            .is_empty(),
            "#1258: an interrupted end puts no unrequested run on the \
             operator's session"
        );

        let surviving = plan_triggered_runs(
            &interrupted,
            SessionId::new(),
            "web-chat-bob",
            vec![job_route(JobId::new())],
            AgentId::new(),
            "notification:dm_ended:alice",
        );
        assert_eq!(
            surviving.len(),
            1,
            "the episode override still wins — dropping it would stall the \
             job until its deadline"
        );
        assert_eq!(
            surviving[0].dm_ended_peer.as_deref(),
            Some("alice"),
            "the one run an interrupted end still produces must be folded"
        );
    }

    #[test]
    fn plan_folds_nothing_for_non_dm_end_sources() {
        let peer_dm = plan_triggered_runs(
            &MessageSource::Agent {
                from_agent: AgentId::new(),
                from_name: "alice".to_string(),
            },
            SessionId::new(),
            "dm:alice:bob",
            Vec::new(),
            AgentId::new(),
            "peer:alice",
        );
        assert_eq!(peer_dm.len(), 1);
        assert_eq!(
            peer_dm[0].dm_ended_peer, None,
            "a live DM turn folds on its own `is_peer_message` arm"
        );

        let subagent = plan_triggered_runs(
            &MessageSource::SubagentCompletion,
            SessionId::new(),
            "web-chat-bob",
            Vec::new(),
            AgentId::new(),
            "subagent",
        );
        assert_eq!(subagent.len(), 1);
        assert_eq!(subagent[0].dm_ended_peer, None);
    }

    /// Regression test for #513: when a `ConversationEnded` trigger has
    /// `source_session_id: None` (the agent was a pure DM recipient),
    /// `run_trigger_loop` must NOT reroute the notification run to a
    /// user-facing session. The run must stay on the original
    /// `notifications:{agent}` session.
    ///
    /// Before this fix, the gateway rerouted the notification to the
    /// agent's most recent user-facing session (#495), which polluted
    /// the web-chat with notification runs that should have been invisible.
    #[tokio::test]
    async fn test_conversation_ended_no_reroute_when_source_session_none() {
        // -- Build a minimal AppState --
        let gateway_config = crate::gateway::GatewayConfig::default();
        let gateway = crate::gateway::Gateway::new(gateway_config).unwrap();
        let scheduler = std::sync::Arc::new(alms_runtime::Scheduler::new());
        let shutdown_token = CancellationToken::new();
        let (completion_tx, _completion_rx) = mpsc::unbounded_channel();
        // The trigger_tx is consumed by AppState's MessageBus; the test
        // feeds run_trigger_loop via a separate channel below. Bounded
        // (#842 / B11) to match the production channel shape.
        let (trigger_tx, _bus_rx) = mpsc::channel(8);
        let (dm_event_tx, _dm_event_rx) = mpsc::channel(8);
        let state = AppState::new(
            gateway,
            scheduler,
            shutdown_token.clone(),
            completion_tx,
            trigger_tx,
            dm_event_tx,
        )
        .unwrap();

        let agent_id = AgentId::new();
        let sender_agent_id = AgentId::new();

        // Create a `notifications:bob` session (the trigger target).
        let notif_session = state
            .session_manager
            .get_or_create(agent_id, "notifications:bob");
        let notif_session_id = notif_session.id;
        let notif_context_id = notif_session.context_id.clone();

        // Create a user-facing `web` session for the same agent. If the old
        // rerouting logic were still present, the notification run would be
        // incorrectly routed here instead.
        let _web_session = state.session_manager.get_or_create(agent_id, "web");

        // -- Send a ConversationEnded trigger with source_session_id: None --
        let (test_tx, test_rx) = mpsc::channel(8);
        test_tx
            .send(RunTrigger {
                agent_id,
                session_id: notif_session_id,
                input: "DM ended marker".to_string(),
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
        // Drop the sender so the loop exits after processing the one trigger.
        drop(test_tx);

        // -- Run the trigger loop to completion --
        run_trigger_loop(test_rx, state.clone()).await;

        // -- Verify the run was created on the notifications session --
        let runs = state.run_manager.list_by_session(notif_session_id, 10);
        assert!(
            !runs.is_empty(),
            "expected at least one run on the notifications session"
        );
        assert_eq!(
            runs[0].session_id, notif_session_id,
            "notification run must stay on the notifications: session, not be rerouted \
             to the user-facing web session"
        );
        assert_eq!(
            runs[0].agent_id, agent_id,
            "run should belong to the target agent"
        );

        // Clean up: cancel the shutdown token so background tasks (if any) stop.
        shutdown_token.cancel();
    }

    /// Regression test for the codex P2 finding on PR #1049 / issue #1041.
    ///
    /// `completion_notification_loop` MUST persist the
    /// `subagent_completion` history marker BEFORE it dispatches the SSE
    /// `subagent_completed` event. Otherwise a page reload landing between
    /// the two writes would observe `last_event_id` advanced past the SSE
    /// event paired with a history snapshot that still lacks the marker —
    /// Iris's chip rehydration would then see an unpaired `invoke_agent`
    /// row and the chip would be stuck "running" until another full reload.
    ///
    /// This test verifies the end state after the loop drains: both the
    /// history marker AND the SSE event must be present. Because the SSE
    /// log write is what closes the race window, observing it as `Some(_)`
    /// proves the loop reached past the marker block, so seeing the marker
    /// in history at that point locks in the ordering invariant. The
    /// ordering itself is guarded by the inline comment in
    /// `completion_notification_loop`; this test catches regressions where
    /// the marker is dropped, moved to a fire-and-forget task, or otherwise
    /// stops being written before the SSE event log advances.
    #[tokio::test]
    async fn test_subagent_completion_marker_persisted_before_sse_event() {
        // -- Build a minimal AppState --
        let gateway_config = crate::gateway::GatewayConfig::default();
        let gateway = crate::gateway::Gateway::new(gateway_config).unwrap();
        let scheduler = std::sync::Arc::new(alms_runtime::Scheduler::new());
        let shutdown_token = CancellationToken::new();
        let (completion_tx, _completion_rx) = mpsc::unbounded_channel();
        // Bounded (#842 / B11) to match the production channel shape.
        let (trigger_tx, _bus_rx) = mpsc::channel(8);
        let (dm_event_tx, _dm_event_rx) = mpsc::channel(8);
        let state = AppState::new(
            gateway,
            scheduler,
            shutdown_token.clone(),
            completion_tx,
            trigger_tx,
            dm_event_tx,
        )
        .unwrap();

        let parent_agent_id = AgentId::new();
        let parent_session = state.session_manager.get_or_create(parent_agent_id, "web");
        let parent_session_id = parent_session.id;
        let subagent_session_id = state
            .session_manager
            .get_or_create(AgentId::new(), "subagent")
            .id;

        // Baseline: no SSE events on the parent session yet.
        assert!(
            state
                .run_manager
                .latest_session_event_id(parent_session_id)
                .await
                .is_none(),
            "no SSE events should exist before the completion is fed"
        );

        // -- Feed one completion and drain the loop synchronously --
        // Mirrors the pattern in `runs::integration_tests` — a side channel
        // owned by the test feeds the loop, and dropping the sender causes
        // the receiver to return None so the loop exits on its own.
        // A known invocation id so we can assert it threads through to the
        // persisted marker (and, by the shared `completion` field, the SSE
        // event) — the A1-2 fix for #1125.
        let parent_inv_id = uuid::Uuid::new_v4();
        let (test_tx, test_rx) = mpsc::unbounded_channel();
        test_tx
            .send(alms_coordinator::SubagentCompletion {
                task_id: alms_coordinator::TaskId::new(),
                subagent_name: Some("researcher".to_string()),
                status: alms_coordinator::TaskStatus::Completed,
                summary: "All done.".to_string(),
                parent_session_id,
                parent_agent_id,
                subagent_session_id,
                task_description: Some("investigate the thing".to_string()),
                tool_count: Some(3),
                duration_ms: Some(1200),
                token_usage: None,
                parent_tool_invocation_id: Some(parent_inv_id),
            })
            .unwrap();
        drop(test_tx);
        completion_notification_loop(test_rx, state.clone()).await;

        // -- Assert: the SSE event log advanced (the second write fired)
        // AND the history marker is present (the first write fired before
        // the second). The two together prove the ordering invariant for
        // this run. --
        let last_event_id = state
            .run_manager
            .latest_session_event_id(parent_session_id)
            .await;
        assert!(
            last_event_id.is_some(),
            "SSE event log should have at least one subagent_completed entry"
        );

        let history = state
            .session_manager
            .get_history(parent_session_id)
            .unwrap();
        let marker = history
            .iter()
            .find(|m| {
                m.metadata
                    .as_ref()
                    .and_then(|meta| meta.get("type"))
                    .and_then(|v| v.as_str())
                    == Some("subagent_completion")
            })
            .expect(
                "subagent_completion marker missing from history — the ordering invariant in \
                 completion_notification_loop has regressed (codex P2 on PR #1049 / issue #1041)",
            );
        let meta = marker.metadata.as_ref().unwrap();
        assert_eq!(meta["subagent_name"], "researcher");
        assert_eq!(meta["status"], "done");
        assert_eq!(meta["session_id"], subagent_session_id.0.to_string());
        // #1125 A1-2: the parent's invoke_agent invocation id threads through
        // to the marker so a reload can resolve the completion by invocation id.
        assert_eq!(meta["tool_invocation_id"], parent_inv_id.to_string());

        // Clean up: cancel the shutdown token so background tasks (if any) stop.
        shutdown_token.cancel();
    }

    // -----------------------------------------------------------------------
    // #1196 — job-completion summary cap + deep-link handles.
    // -----------------------------------------------------------------------

    /// Build a minimal AppState for the notification-path tests (no SQLite
    /// needed — the in-memory session manager and run store suffice).
    fn build_notification_state() -> (AppState, CancellationToken) {
        let gateway_config = crate::gateway::GatewayConfig::default();
        let gateway = crate::gateway::Gateway::new(gateway_config).unwrap();
        let scheduler = std::sync::Arc::new(alms_runtime::Scheduler::new());
        let shutdown_token = CancellationToken::new();
        let (completion_tx, _cr) = mpsc::unbounded_channel();
        let (trigger_tx, _bus_rx) = mpsc::channel(8);
        let (dm_event_tx, _dm_event_rx) = mpsc::channel(8);
        let state = AppState::new(
            gateway,
            scheduler,
            shutdown_token.clone(),
            completion_tx,
            trigger_tx,
            dm_event_tx,
        )
        .unwrap();
        (state, shutdown_token)
    }

    /// Insert a Completed job run with the given output and return its id.
    fn insert_completed_job_run(
        state: &AppState,
        session_id: SessionId,
        agent_id: AgentId,
        job_id: JobId,
        prompt: &str,
        output: String,
    ) -> RunId {
        let mut run = Run::for_job(session_id, agent_id, prompt.to_string(), job_id);
        assert!(run.mark_running());
        assert!(run.mark_completed(output, Default::default()));
        let run_id = run.run_id;
        let _ = state.run_manager.insert_run(run);
        run_id
    }

    /// Extract the persisted `job_notification` marker's `(text, metadata)`.
    fn job_marker(state: &AppState, session_id: SessionId) -> (String, serde_json::Value) {
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

    /// The persisted marker (and, by the same summary/id values, the SSE
    /// payload) must carry the full ≤4000-char summary and the run/job
    /// deep-link handles — not the old 200-char fragment with a bare
    /// `{job_status}` metadata object.
    #[tokio::test]
    async fn job_completion_marker_carries_full_summary_and_ids() {
        let (state, shutdown_token) = build_notification_state();
        let agent_id = AgentId::new();

        // A user-facing session so `find_user_facing_session` resolves a target.
        let web_session_id = state.session_manager.get_or_create(agent_id, "web").id;

        // Output comfortably past the cap so truncation is exercised.
        let job_id = JobId::new();
        let long_output = "a".repeat(crate::sse::JOB_SUMMARY_MAX_CHARS + 1000);
        let run_id = insert_completed_job_run(
            &state,
            web_session_id,
            agent_id,
            job_id,
            "nightly digest",
            long_output,
        );

        notify_job_completion(&state, agent_id, "nightly digest", run_id, job_id, None).await;

        let (text, meta) = job_marker(&state, web_session_id);

        // Deep-link handles the frontend needs to fetch the full output.
        assert_eq!(meta["job_status"], "success");
        assert_eq!(meta["run_id"], run_id.0.to_string());
        assert_eq!(meta["job_id"], job_id.0.to_string());
        assert_eq!(meta["job_session_id"], format!("job_{}", job_id.0));
        // The authoritative truncation flag — true here because the output is
        // over the cap; the card keys its fetch-on-expand on this.
        assert_eq!(meta["truncated"], true);

        // The summary portion (after the name/summary newline) is capped at
        // JOB_SUMMARY_MAX_CHARS + the "..." ellipsis — far past the old 200.
        let nl = text
            .find('\n')
            .expect("marker has a name/summary delimiter");
        let summary_part = &text[nl + 1..];
        assert!(
            summary_part.ends_with("..."),
            "an over-cap output must be truncated with an ellipsis"
        );
        assert_eq!(
            summary_part.chars().count(),
            crate::sse::JOB_SUMMARY_MAX_CHARS + 3,
            "summary must be capped at 4000 chars (+ ellipsis), not 200"
        );

        shutdown_token.cancel();
    }

    /// #1217 (Tim C1): the card's "Go to job session" button must navigate by
    /// the job session's REAL `SessionId` — a value `GET /session/{id}`
    /// (`Path<SessionId>`) can resolve — not the `job_{id}` context handle
    /// (which 400s). This exercises the RESOLUTION, not just string threading:
    /// it stands up the hidden job session exactly as `fire_job_run` does
    /// (`get_or_create(agent_id, "job_{id}")`), fires the notification, and
    /// pins that the emitted `job_session_uuid` equals that session's real id,
    /// parses back to it, and is distinct from the context handle.
    #[tokio::test]
    async fn job_completion_emits_resolvable_job_session_uuid() {
        let (state, shutdown_token) = build_notification_state();
        let agent_id = AgentId::new();

        // The user-facing session the card renders on.
        let web_session_id = state.session_manager.get_or_create(agent_id, "web").id;

        // The hidden job session — created under the SAME (agent_id, context)
        // key `fire_job_run` uses, so it carries its own random SessionId.
        let job_id = JobId::new();
        let job_ctx = format!("job_{}", job_id.0);
        let real_job_session_id = state.session_manager.get_or_create(agent_id, &job_ctx).id;

        let run_id = insert_completed_job_run(
            &state,
            web_session_id,
            agent_id,
            job_id,
            "nightly digest",
            "done".to_string(),
        );

        notify_job_completion(&state, agent_id, "nightly digest", run_id, job_id, None).await;

        let (_text, meta) = job_marker(&state, web_session_id);

        // The context handle is preserved (identity/debug), but it is NOT a
        // SessionId — navigating by it is the bug being fixed.
        assert_eq!(meta["job_session_id"], job_ctx);

        // The button's navigation target: the hidden session's REAL id.
        let emitted_uuid = meta["job_session_uuid"]
            .as_str()
            .expect("job_session_uuid must be present when the hidden session is resident");
        assert_eq!(
            emitted_uuid,
            real_job_session_id.0.to_string(),
            "the emitted uuid must be the hidden job session's real SessionId"
        );
        assert_ne!(
            emitted_uuid, job_ctx,
            "the navigation target must not be the `job_{{id}}` context handle"
        );

        // RESOLUTION: the emitted value parses as a UUID and, keyed through the
        // session manager, resolves to the same session — exactly the path
        // `GET /session/{id}` (`Path<SessionId>`) takes. The `job_{id}` context
        // handle would fail the UUID parse (a 400), so this round-trip is what
        // was broken before the fix.
        let parsed = SessionId(
            uuid::Uuid::parse_str(emitted_uuid).expect("job_session_uuid must be a valid UUID"),
        );
        assert_eq!(parsed, real_job_session_id);
        assert_eq!(
            state.session_manager.get(parsed).unwrap().context_id,
            job_ctx,
            "the emitted uuid must resolve to the hidden job session"
        );
        assert!(
            uuid::Uuid::parse_str(&job_ctx).is_err(),
            "sanity: the context handle is NOT a UUID — navigating by it 400s"
        );

        shutdown_token.cancel();
    }

    /// #1196 defect (b): a job prompt with a newline in its leading chars must
    /// not leak its tail into the summary. The name is collapsed to a single
    /// line so the marker's first newline is unambiguously the name/summary
    /// delimiter.
    #[tokio::test]
    async fn job_completion_collapses_newline_in_prompt_name() {
        let (state, shutdown_token) = build_notification_state();
        let agent_id = AgentId::new();
        let web_session_id = state.session_manager.get_or_create(agent_id, "web").id;

        let job_id = JobId::new();
        let run_id = insert_completed_job_run(
            &state,
            web_session_id,
            agent_id,
            job_id,
            "ignored",
            "short summary".to_string(),
        );

        // Newline inside the first ~60 chars — the pre-fix bug split here.
        let prompt = "Line one of the prompt\nLine two continues on past the split point";
        notify_job_completion(&state, agent_id, prompt, run_id, job_id, None).await;

        let (text, _meta) = job_marker(&state, web_session_id);
        let nl = text
            .find('\n')
            .expect("marker has a name/summary delimiter");
        let header = &text[..nl];
        let summary_part = &text[nl + 1..];

        // Both prompt lines land on the single header line (newline -> space).
        assert!(header.starts_with("[Scheduled job completed] "));
        assert!(
            header.contains("Line one of the prompt Line two"),
            "prompt newline must collapse to a space on the header line, got: {header}"
        );
        // The summary is exactly the run output — no prompt tail leaked in.
        assert_eq!(
            summary_part, "short summary",
            "summary must be exactly the run output, with no prompt-line-2 leak"
        );

        shutdown_token.cancel();
    }

    /// A multi-byte output just under the char cap must NOT be flagged as
    /// truncated: the old `output.len()` (byte length) test would append a
    /// spurious ellipsis for non-ASCII text even when every char fit (#1196
    /// defect a). Char-based `truncate_chars` keeps it intact.
    #[tokio::test]
    async fn job_completion_summary_is_char_based_not_byte_based() {
        let (state, shutdown_token) = build_notification_state();
        let agent_id = AgentId::new();
        let web_session_id = state.session_manager.get_or_create(agent_id, "web").id;

        // 100 three-byte chars = 300 bytes but only 100 chars — well under the
        // 4000-char cap, so it must be stored verbatim with no ellipsis.
        let multibyte = "€".repeat(100);
        let job_id = JobId::new();
        let run_id = insert_completed_job_run(
            &state,
            web_session_id,
            agent_id,
            job_id,
            "unicode job",
            multibyte.clone(),
        );

        notify_job_completion(&state, agent_id, "unicode job", run_id, job_id, None).await;

        let (text, meta) = job_marker(&state, web_session_id);
        let nl = text
            .find('\n')
            .expect("marker has a name/summary delimiter");
        let summary_part = &text[nl + 1..];
        assert_eq!(
            summary_part, multibyte,
            "a multi-byte summary under the char cap must be stored verbatim (no byte-vs-char ellipsis)"
        );
        assert_eq!(
            meta["truncated"], false,
            "an under-cap summary must report truncated=false so the card doesn't fetch"
        );

        shutdown_token.cancel();
    }

    /// N1 (#1202, Tim's review): the deadline note is composed BEFORE the
    /// 4000-char cap. With an at-cap output, the persisted marker summary
    /// must be exactly `JOB_SUMMARY_MAX_CHARS + "..."` INCLUDING the note —
    /// pre-fix the note was prepended after the cap, so the marker exceeded
    /// the cap while the SSE constructor re-clipped its copy: the two
    /// surfaces diverged by the tail.
    #[tokio::test]
    async fn deadline_note_is_applied_before_the_cap() {
        let (state, shutdown_token) = build_notification_state();
        let agent_id = AgentId::new();
        let web_session_id = state.session_manager.get_or_create(agent_id, "web").id;

        // Output exactly at the cap so ANY prepended note must displace tail
        // chars rather than exceed the cap.
        let job_id = JobId::new();
        let at_cap_output = "a".repeat(crate::sse::JOB_SUMMARY_MAX_CHARS);
        let run_id = insert_completed_job_run(
            &state,
            web_session_id,
            agent_id,
            job_id,
            "slow job",
            at_cap_output,
        );

        let stats = EpisodeCloseStats {
            turns: 2,
            dm_count: 1,
            subagent_count: 0,
            timed_out: true,
            detached: 1,
        };
        notify_job_completion(&state, agent_id, "slow job", run_id, job_id, Some(&stats)).await;

        let (text, meta) = job_marker(&state, web_session_id);
        let nl = text.find('\n').expect("name/summary delimiter");
        let summary_part = &text[nl + 1..];

        // The note leads the summary...
        assert!(
            summary_part
                .starts_with("[Episode deadline reached after 4h — 1 pending task(s) detached]"),
            "deadline note must lead the summary, got: {}",
            &summary_part[..summary_part.len().min(120)]
        );
        // ...and the composed summary is capped at exactly the shared cap —
        // the same string the SSE constructor's re-cap would produce, so
        // live and reload surfaces are byte-identical.
        assert_eq!(
            summary_part.chars().count(),
            crate::sse::JOB_SUMMARY_MAX_CHARS + 3,
            "note + output must be capped ONCE at JOB_SUMMARY_MAX_CHARS (+ ellipsis)"
        );
        assert!(summary_part.ends_with("..."));
        // Output is on run.output -> fetchable -> truncated signals the fetch.
        assert_eq!(meta["truncated"], true);
        assert_eq!(meta["episode"]["timed_out"], true);

        shutdown_token.cancel();
    }

    /// S2 (#1196): a failed run's error is capped in the marker too (not just
    /// the SSE), so a runaway provider error body can't land untruncated in
    /// session history — and `truncated` stays false because the error text
    /// isn't fetchable via `GET /runs/{run_id}` (that returns `run.output`).
    #[tokio::test]
    async fn job_completion_failed_arm_caps_error_and_does_not_signal_fetch() {
        let (state, shutdown_token) = build_notification_state();
        let agent_id = AgentId::new();
        let web_session_id = state.session_manager.get_or_create(agent_id, "web").id;

        // A Failed run with an over-cap error body.
        let job_id = JobId::new();
        let mut run = Run::for_job(web_session_id, agent_id, "flaky job".to_string(), job_id);
        assert!(run.mark_running());
        assert!(run.mark_failed("boom ".repeat(crate::sse::JOB_SUMMARY_MAX_CHARS)));
        let run_id = run.run_id;
        let _ = state.run_manager.insert_run(run);

        notify_job_completion(&state, agent_id, "flaky job", run_id, job_id, None).await;

        let (text, meta) = job_marker(&state, web_session_id);
        assert_eq!(meta["job_status"], "error");
        // Error text is NOT fetchable — must not signal a fetch.
        assert_eq!(meta["truncated"], false);

        let nl = text
            .find('\n')
            .expect("marker has a name/summary delimiter");
        let summary_part = &text[nl + 1..];
        assert!(
            summary_part.ends_with("..."),
            "an over-cap error must be truncated with an ellipsis in the marker"
        );
        assert_eq!(
            summary_part.chars().count(),
            crate::sse::JOB_SUMMARY_MAX_CHARS + 3,
            "the failed-arm error must be capped in the marker, not persisted whole"
        );

        shutdown_token.cancel();
    }
}
