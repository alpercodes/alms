//! DM notification routing, scheduler integration, and trigger loops.

use super::job_episode::{self, EPISODE_DEADLINE_SECS, RunCompletion};
use super::{RunParams, find_user_facing_session};
use crate::cron_utils;
use crate::server::AppState;
use crate::sse::SseEventData;
use alms_core::{JobId, JobSchedule, JobStatus, Run, RunId, RunStatus, SessionId};
use alms_session::job_store::RecordRunOutcome;
use alms_tools::message_sender::ConversationEndReason;
use chrono::Utc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument, warn};

use super::lifecycle::execute_run;

// ---------------------------------------------------------------------------
// Scheduler integration
// ---------------------------------------------------------------------------

/// Receives fired job IDs from the scheduler and dispatches agent runs.
///
/// Each fired job is handled in its own spawned task so a slow run does not
/// block the fire loop from processing subsequent firings.
pub(crate) async fn scheduler_fire_loop(mut rx: mpsc::UnboundedReceiver<JobId>, state: AppState) {
    while let Some(job_id) = rx.recv().await {
        // Resolve session for queue keying so jobs on the same session
        // don't race with each other or with interactive runs.
        let Some(job) = state.job_store.get(job_id) else {
            continue;
        };
        if job.status == JobStatus::Cancelled {
            continue;
        }
        let state_clone = state.clone();
        state.agent_queue.enqueue(
            job.agent_id,
            Box::pin(async move {
                if let Err(e) = fire_job_run(state_clone, job_id).await {
                    error!("Job {} run dispatch failed: {}", job_id, e);
                }
            }),
        );
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
    if job.status == JobStatus::Cancelled {
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

    let run = Run::for_job(session_id, job.agent_id, job.prompt.clone(), job_id);
    let run_id = run.run_id;
    state.run_manager.insert_run(run.clone());
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
    execute_run(
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

    // Guard: if the job was cancelled (or deleted) while the episode was
    // open, do not notify / overwrite the Cancelled status / re-arm.
    // Generalizes the pre-#1198 cancelled-during-run guard.
    let Some(job) = state.job_store.get(job_id) else {
        info!(job_id = %job_id, "Job disappeared during episode — skipping close");
        return;
    };
    if job.status == JobStatus::Cancelled {
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
    record_and_rearm(state, &job, &episode).await;
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
) {
    let job_id = episode.job_id;
    let now = Utc::now();
    match &job.schedule {
        JobSchedule::Once { .. } => {
            // Target status is Cancelled (a spent one-shot). A refusal means
            // a DELETE won the race and the job is already Cancelled — the
            // same terminal state either way; nothing to re-arm.
            match state
                .job_store
                .record_run(job_id, now, JobStatus::Cancelled, None)
            {
                Ok(RecordRunOutcome::Recorded) => {}
                Ok(outcome) => {
                    info!(job_id = %job_id, ?outcome, "record_run not applied at episode close")
                }
                Err(e) => error!(job_id = %job_id, "Failed to record job run: {e}"),
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
                match state
                    .job_store
                    .record_run(job_id, now, JobStatus::Active, Some(now))
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
                    Err(e) => error!(job_id = %job_id, "Failed to record job run: {e}"),
                }
            } else {
                let next = cron_utils::next_after(cron, now);
                if next.is_none() {
                    warn!("Recurring cron '{}' has no future occurrences", cron);
                }
                match state
                    .job_store
                    .record_run(job_id, now, JobStatus::Active, next)
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
                    Err(e) => error!(job_id = %job_id, "Failed to record job run: {e}"),
                }
            }
        }
    }
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
        Some(run) => match run.status {
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
        // identify the firing job and its hidden session. `truncated` is the
        // authoritative "there is more to fetch" flag the card keys on (the
        // reload mirror of the SSE field).
        {
            let mut meta = serde_json::json!({
                "job_status": status,
                "run_id": run_id.0.to_string(),
                "job_id": job_id.0.to_string(),
                "job_session_id": job_session_id,
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

/// Forward a `dm_conversation_ended` event to the agent's user-facing
/// web-chat session so the human watching that session sees a notification.
///
/// Without this, the `dm_conversation_ended` SSE event only lands on the DM
/// session's SSE stream (which the user is typically not watching) and the
/// notification run executes on a `notifications:` session (also invisible
/// to the web-chat).
///
/// This mirrors `notify_job_completion`: find the most recent user-facing
/// session, emit an SSE event, and persist a marker message so it survives
/// page reloads.
pub(super) async fn notify_dm_ended_to_webchat(
    state: &AppState,
    agent_id: alms_core::AgentId,
    peer_name: &str,
    reason: &str,
    context_id: &str,
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

    // Emit SSE event on the web-chat session so connected UI clients see it.
    let dummy_run_id = RunId::new();
    state
        .run_manager
        .send_session_event(
            target_session_id,
            dummy_run_id,
            SseEventData::dm_conversation_ended(
                target_session_id,
                "system",
                peer_name,
                reason,
                context_id,
            ),
        )
        .await;

    // Persist a marker message so it appears on reload.
    let reason_text = match reason {
        "ignored" => "no further replies".to_string(),
        "depth_exceeded" => "message limit reached".to_string(),
        other => other.to_string(),
    };
    super::markers::persist_lifecycle_marker(
        &state.session_manager,
        target_session_id,
        "dm_ended_notification",
        format!("[DM conversation ended] Conversation with {peer_name} ended ({reason_text})."),
        serde_json::json!({
            "peer": peer_name,
            "reason": reason,
            "context_id": context_id,
        }),
    );

    info!(
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

        let run_id = enqueue_triggered_run(
            &state,
            agent_id,
            session_id,
            notification,
            context_id,
            "subagent".to_string(),
            false, // subagent completion — not a peer message
            episode_route.as_ref().map(|r| r.job_id),
        )
        .await;
        if let Some(route) = episode_route {
            state.job_episodes.note_run(route.job_id, run_id);
        }

        debug!(
            run_id = %run_id.0,
            session_id = %session_id.0,
            task_id = %completion.task_id.0,
            "Notification run enqueued"
        );
    }
}

/// Creates a run, registers it, sends the SSE `run_created` event, and
/// enqueues the run at low priority for execution.
///
/// Shared helper for [`completion_notification_loop`] and [`run_trigger_loop`],
/// which both follow the same create-register-enqueue pattern.
#[allow(clippy::too_many_arguments)] // internal helper; params mirror RunParams + the #1198 job stamp
async fn enqueue_triggered_run(
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
) -> RunId {
    let mut run = Run::new(session_id, agent_id, input.clone());
    run.job_id = job_id;
    let run_id = run.run_id;
    state.run_manager.insert_run(run);

    // Mirror the `create_run` reconstruction: `SessionQueue::pending_count`
    // only counts items still *waiting* in the queue -- the currently-
    // executing work item has already been dequeued and its counter
    // decremented. Add 1 if the agent has a `Running` run so the UI gets
    // an accurate `queued_behind` for notification/trigger runs too.
    //
    // Note: there is a narrow sub-millisecond TOCTOU window between
    // `pending.fetch_sub(1)` inside the queue handler and
    // `mark_run_as_running` in `execute_run`. During that window both
    // signals read false and `queued_behind` may undercount by 1. The
    // window is bounded by executor dispatch latency and is considered
    // acceptable; closing it would require a separate in-flight counter
    // inside `SessionQueue`.
    let agent_running = state.run_manager.agent_has_running_run(agent_id);
    let queued_behind = state.agent_queue.pending_count(&agent_id) + usize::from(agent_running);
    state
        .run_manager
        .send_session_event(
            session_id,
            run_id,
            SseEventData::run_created(run_id, session_id, true, Some(source_label), queued_behind),
        )
        .await;

    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    let state_clone = state.clone();
    state.agent_queue.enqueue_low(
        agent_id,
        Box::pin(async move {
            execute_run(
                state_clone,
                RunParams {
                    run_id,
                    session_id,
                    agent_id,
                    input,
                    context_id,
                    cancel_token,
                    is_peer_message,
                    // All runs via enqueue_triggered_run are system-triggered
                    // (no human watching), so Guarded posture is overridden.
                    is_system_triggered: true,
                    // System-triggered runs persist their own input through
                    // the notification or peer-message paths, so no gateway-
                    // side pre-persistence is needed here.
                    input_pre_persisted: false,
                },
            )
            .await;
        }),
    );

    run_id
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
/// `MessageSource::ConversationEnded` trigger.  The notification tells the
/// peer agent that the DM conversation has ended, includes the reason, and
/// — when `conversation_history` is provided — embeds the full DM
/// transcript so the agent can act immediately without calling
/// `read_messages`.
pub(super) fn format_dm_ended_notification(
    from_name: &str,
    reason: ConversationEndReason,
    conversation_history: Option<&str>,
) -> String {
    let reason_text = match &reason {
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
        ConversationEndReason::Errored { message } => {
            format!(
                "The conversation with agent \"{from_name}\" ended because \
                 the run failed: {message}"
            )
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
pub(crate) async fn run_trigger_loop(
    mut rx: mpsc::Receiver<alms_coordinator::message_bus::RunTrigger>,
    state: AppState,
) {
    use alms_coordinator::message_bus::MessageSource;

    while let Some(trigger) = rx.recv().await {
        let session_id = trigger.session_id;
        let agent_id = trigger.agent_id;
        let context_id = trigger.context_id;

        // Build a source label for SSE `run_created` events and determine
        // whether this is a peer DM run (which needs the DM addendum) or
        // a notification run (which must NOT get the DM addendum).
        // `dm_peer_name` is captured for DM runs so we can forward a
        // lightweight activity event to the agent's webchat session (#651).
        let (source_label, is_peer, input, dm_peer_name, episode_route) = match &trigger.source {
            MessageSource::Agent { from_name, .. } => (
                format!("peer:{from_name}"),
                true,
                // Peer DM: input already persisted by MessageBus — pass it
                // through so the Run record has a copy.
                trigger.input,
                Some(from_name.clone()),
                None,
            ),
            MessageSource::SubagentCompletion => {
                ("subagent".to_string(), false, trigger.input, None, None)
            }
            MessageSource::ConversationEnded {
                from_name,
                reason,
                source_session_id,
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

                // -- No rerouting for pure recipients --
                //
                // When `source_session_id` is `None`, the agent was a pure
                // DM recipient who never called `send_message` from a
                // user-facing session.  The notification run stays on the
                // invisible `notifications:{agent}` session so it does NOT
                // pollute the agent's web-chat with the user.
                //
                // The visual "DM ended" indicator is handled separately by
                // `notify_dm_ended_to_webchat` below, which sends a
                // lightweight SSE event + marker message to the web-chat
                // without creating a full LLM notification run there.
                //
                // When `source_session_id` IS present (the agent initiated
                // the DM from a user-facing session), the MessageBus already
                // set the trigger's `session_id` to that source session, so
                // the notification run appears in the correct chat.
                if source_session_id.is_none() {
                    debug!(
                        agent_id = %agent_id.0,
                        "No source session for agent — notification run will \
                         execute on invisible notifications: session (agent was pure recipient)"
                    );
                }

                // -- Forward dm_conversation_ended to the agent's web-chat --
                //
                // Every agent that receives a ConversationEnded trigger
                // needs the visual DM-ended indicator on their web-chat
                // session.  This covers:
                //
                // - **Peer** (the other agent in the DM): always receives
                //   a ConversationEnded trigger, needs the banner (#497).
                //
                // - **Sender** (the agent that called ignore_message):
                //   receives a self-notification trigger (#556) and gets
                //   the banner here.  The ignore_message path in
                //   execute_run (lifecycle.rs) does NOT call
                //   notify_dm_ended_to_webchat — it defers to this path
                //   to avoid duplicates.
                //
                // For depth_exceeded, both the recipient and the
                // sender (when the sender has a source session) get
                // ConversationEnded triggers — `end_conversation`
                // emits both (#556).
                {
                    let reason_str = reason.to_string();
                    let dm_context = peer_name_resolved
                        .as_ref()
                        .map(|peer_name| alms_core::dm_context_id(from_name, peer_name))
                        .unwrap_or_default();
                    notify_dm_ended_to_webchat(
                        &state,
                        agent_id,
                        from_name,
                        &reason_str,
                        &dm_context,
                    )
                    .await;
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
                // continuation out of the job). A hit atomically removes
                // the pending entry and reserves the continuation run; the
                // routing override below then pins the run to the job
                // session so the agent resumes with its full job context.
                let episode_route = peer_name_resolved.as_ref().and_then(|peer_name| {
                    let dm_session_id = SessionId::deterministic_dm(from_name, peer_name);
                    state.job_episodes.resolve_dm(dm_session_id, agent_id)
                });

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
                    ),
                    None,
                    episode_route,
                )
            }
        };

        // #1198 routing override: a resolved episode DM pins the
        // continuation run to the JOB session (superseding the
        // `source_sessions`-derived target — see design doc § D3 for why
        // the tracker, not the bus, is the primary router). On a miss the
        // pre-#1198 routing applies byte-for-byte.
        let (session_id, context_id, episode_job_id) = match episode_route {
            Some(route) => {
                info!(
                    job_id = %route.job_id,
                    dm_target = %session_id.0,
                    job_session = %route.job_session_id.0,
                    "ConversationEnded resolved to open job episode — routing \
                     continuation onto the job session (#1198)"
                );
                (route.job_session_id, route.context_id, Some(route.job_id))
            }
            None => (session_id, context_id, None),
        };

        info!(
            session_id = %session_id.0,
            agent_id = %agent_id.0,
            source = %source_label,
            "RunTrigger -> creating run"
        );

        let run_id = enqueue_triggered_run(
            &state,
            agent_id,
            session_id,
            input,
            context_id.clone(),
            source_label,
            is_peer,
            episode_job_id,
        )
        .await;
        if let Some(job_id) = episode_job_id {
            state.job_episodes.note_run(job_id, run_id);
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
        run.status = RunStatus::Completed;
        run.output = Some(output);
        let run_id = run.run_id;
        state.run_manager.insert_run(run);
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
        run.status = RunStatus::Failed;
        run.error = Some("boom ".repeat(crate::sse::JOB_SUMMARY_MAX_CHARS));
        let run_id = run.run_id;
        state.run_manager.insert_run(run);

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
