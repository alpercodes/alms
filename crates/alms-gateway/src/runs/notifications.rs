//! DM notification routing, scheduler integration, and trigger loops.

use super::{RunOverrides, RunParams, find_user_facing_session};
use crate::cron_utils;
use crate::server::AppState;
use crate::sse::SseEventData;
use alms_core::{JobId, JobSchedule, JobStatus, Run, RunId, RunStatus, SessionId};
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
#[instrument(level = "info", skip(state), fields(job_id = %job_id))]
async fn fire_job_run(state: AppState, job_id: JobId) -> alms_core::AlmsResult<()> {
    // Look up the job — it may have been cancelled between scheduling and firing.
    let Some(job) = state.job_store.get(job_id) else {
        info!("Skipping fired job — not found in store");
        return Ok(());
    };
    if job.status == JobStatus::Cancelled {
        info!("Skipping fired job — already cancelled");
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
            overrides: RunOverrides::default(),
            context_id,
            cancel_token,
            is_peer_message: false,
            is_system_triggered: true,
        },
    )
    .await;

    // -- Job completion notification --
    // Send a notification to the agent's most recent user-facing session
    // so the user can see that the job ran (even if they weren't watching
    // the hidden job_* session).
    notify_job_completion(&state, job.agent_id, &job.prompt, run_id).await;

    // Guard: if the job was cancelled while the run was in progress, do not
    // overwrite the Cancelled status or re-arm the scheduler.
    if state
        .job_store
        .get(job_id)
        .map(|j| j.status == JobStatus::Cancelled)
        .unwrap_or(true)
    {
        info!("Job was cancelled during run, skipping post-run update");
        return Ok(());
    }

    // Update job record after run completes.
    let now = Utc::now();
    let (new_status, next_run_at) = match &job.schedule {
        JobSchedule::Once { .. } => (JobStatus::Cancelled, None),
        JobSchedule::Recurring { cron } => {
            let next = cron_utils::next_after(cron, now);
            if next.is_none() {
                warn!("Recurring cron '{}' has no future occurrences", cron);
            }
            (JobStatus::Active, next)
        }
    };

    state
        .job_store
        .record_run(job_id, now, new_status, next_run_at)?;

    // Re-arm recurring jobs with the next computed fire time.
    if let Some(next) = next_run_at {
        let delay = (next - now).to_std().unwrap_or(std::time::Duration::ZERO);
        let instant = tokio::time::Instant::now() + delay;
        state.scheduler.schedule_once(job_id, instant).await;
        info!("Recurring job re-armed for {}", next);
    }

    Ok(())
}

/// Send a job-completion notification to the agent's most recent user-facing
/// session. This makes job runs visible in the chat without creating a full
/// notification run (which would trigger another LLM call).
async fn notify_job_completion(
    state: &AppState,
    agent_id: alms_core::AgentId,
    job_prompt: &str,
    run_id: RunId,
) {
    // Determine outcome from the completed run.
    let (status, summary) = match state.run_manager.get_run(run_id) {
        Some(run) => match run.status {
            RunStatus::Completed => {
                let output = run.output.unwrap_or_default();
                let summary: String = if output.len() > 200 {
                    format!("{}...", output.chars().take(200).collect::<String>())
                } else {
                    output
                };
                ("success", summary)
            }
            RunStatus::Failed => {
                let err = run.error.unwrap_or_else(|| "unknown error".to_string());
                ("error", err)
            }
            RunStatus::Cancelled => ("cancelled", "run was cancelled".to_string()),
            RunStatus::Queued | RunStatus::Running => {
                // Shouldn't happen — execute_run already returned.
                ("unknown", "run still in progress".to_string())
            }
        },
        None => ("error", "run record not found".to_string()),
    };

    // Find the agent's most recent user-facing session (exclude internal sessions).
    let Some(target) = find_user_facing_session(&state.session_manager, agent_id) else {
        debug!(
            "No user-facing session for agent {} — skipping job notification",
            agent_id
        );
        return;
    };
    let target_session_id = target.id;

    // Truncate the prompt for display.
    let job_name: String = if job_prompt.len() > 60 {
        format!("{}...", job_prompt.chars().take(60).collect::<String>())
    } else {
        job_prompt.to_string()
    };

    // Send SSE event to the target session so connected UI clients see it.
    state
        .run_manager
        .send_session_event(
            target_session_id,
            alms_core::RunId::new(), // no associated run on this session
            SseEventData::job_completed(target_session_id, &job_name, status, &summary),
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
        serde_json::json!({"job_status": status}),
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

        // Notify session subscribers that a subagent completed.
        // This updates the SubagentBar and shows a system message BEFORE
        // the notification run starts.
        let status_str = match completion.status {
            alms_coordinator::TaskStatus::Completed => "done",
            alms_coordinator::TaskStatus::Failed => "fail",
            alms_coordinator::TaskStatus::Cancelled => "cancelled",
            _ => "done",
        };
        state
            .run_manager
            .send_session_event(
                session_id,
                alms_core::RunId::new(), // no run yet
                SseEventData::subagent_completed(
                    session_id,
                    completion.subagent_name.clone(),
                    status_str,
                    &completion.summary,
                    completion.subagent_session_id,
                ),
            )
            .await;

        // Persist the subagent completion marker to session history so it
        // survives page refreshes and appears in the chat on reload.
        // Include rich metadata so the frontend can reconstruct the full
        // SubagentCompletionCard (session_id, task, tool_count, duration,
        // summary, token_usage) instead of a plain system message.
        {
            let name = completion.subagent_name.as_deref().unwrap_or("subagent");
            let label = match status_str {
                "fail" => "failed",
                "cancelled" => "cancelled",
                _ => "completed",
            };

            // Build the metadata object with all fields the frontend needs.
            let mut meta = serde_json::json!({
                "subagent_name": name,
                "status": status_str,
                "session_id": completion.subagent_session_id.0.to_string(),
                "summary": &completion.summary,
            });
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
                meta["token_usage"] = serde_json::json!({
                    "prompt_tokens": usage.prompt_tokens,
                    "completion_tokens": usage.completion_tokens,
                });
            }

            super::markers::persist_lifecycle_marker(
                &state.session_manager,
                session_id,
                "subagent_completion",
                format!("Subagent '{}' {}.", name, label),
                meta,
            );
        }

        let notification = format_completion_notification(&completion);

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
        )
        .await;

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
async fn enqueue_triggered_run(
    state: &AppState,
    agent_id: alms_core::AgentId,
    session_id: SessionId,
    input: String,
    context_id: String,
    source_label: String,
    is_peer_message: bool,
) -> RunId {
    let run = Run::new(session_id, agent_id, input.clone());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);

    let queued_behind = state.agent_queue.pending_count(&agent_id);
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
                    overrides: RunOverrides::default(),
                    context_id,
                    cancel_token,
                    is_peer_message,
                    // All runs via enqueue_triggered_run are system-triggered
                    // (no human watching), so Guarded posture is overridden.
                    is_system_triggered: true,
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
fn format_completion_notification(c: &alms_coordinator::SubagentCompletion) -> String {
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
        None => (
            format!("(task {})", c.task_id.0),
            "The subagent result summary is included above.".to_string(),
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
    let reason_text = match reason {
        ConversationEndReason::Ignored => {
            format!("Agent \"{from_name}\" ended the conversation (chose not to reply).")
        }
        ConversationEndReason::DepthExceeded => {
            format!(
                "The conversation with agent \"{from_name}\" was terminated \
                 because the maximum message depth was reached."
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
    mut rx: mpsc::UnboundedReceiver<alms_coordinator::message_bus::RunTrigger>,
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
        let (source_label, is_peer, input, dm_peer_name) = match &trigger.source {
            MessageSource::Agent { from_name, .. } => (
                format!("peer:{from_name}"),
                true,
                // Peer DM: input already persisted by MessageBus — pass it
                // through so the Run record has a copy.
                trigger.input,
                Some(from_name.clone()),
            ),
            MessageSource::SubagentCompletion => {
                ("subagent".to_string(), false, trigger.input, None)
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
                if *reason == ConversationEndReason::DepthExceeded {
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
                        *reason,
                        conversation_history.as_deref(),
                    ),
                    None,
                )
            }
        };

        info!(
            session_id = %session_id.0,
            agent_id = %agent_id.0,
            source = %source_label,
            "RunTrigger -> creating run"
        );

        enqueue_triggered_run(
            &state,
            agent_id,
            session_id,
            input,
            context_id.clone(),
            source_label,
            is_peer,
        )
        .await;

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
    mut rx: tokio::sync::mpsc::UnboundedReceiver<alms_coordinator::message_bus::DmEvent>,
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
        // feeds run_trigger_loop via a separate channel below.
        let (trigger_tx, _bus_rx) = mpsc::unbounded_channel();
        let (dm_event_tx, _dm_event_rx) = mpsc::unbounded_channel();
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
        let (test_tx, test_rx) = mpsc::unbounded_channel();
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
}
