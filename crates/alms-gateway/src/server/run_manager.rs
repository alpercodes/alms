//! Run tracking, event broadcasting, cancellation, and persistence.
//!
//! [`RunManager`] is the central piece of run lifecycle management in the
//! gateway.  It owns the in-memory run map, per-run and per-session SSE
//! senders, in-flight counters for graceful shutdown, and optional SQLite
//! persistence.

use crate::event_log::{AgentEventLogManager, EventLogManager, LoggedEvent};
use crate::sse::SseEventData;
use alms_core::{AgentId, Run, RunId, SessionId};
use dashmap::DashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::info;

/// Run manager for tracking runs and their event streams
#[derive(Debug, Clone)]
pub struct RunManager {
    pub event_senders: Arc<DashMap<RunId, Vec<mpsc::UnboundedSender<SseEventData>>>>,
    pub runs: Arc<DashMap<RunId, Run>>,
    /// In-memory event log for SSE reconnect during current process lifetime.
    /// Events are lost on restart — this does **not** provide cross-restart durability.
    pub event_log: EventLogManager,
    /// Session-level event senders for persistent SSE streams.
    pub session_senders: Arc<DashMap<SessionId, Vec<mpsc::UnboundedSender<SseEventData>>>>,
    /// Session-level event log for reconnect support.
    pub session_event_log: crate::event_log::SessionEventLogManager,
    /// Agent-scoped event senders for the per-agent SSE feed
    /// (`GET /agents/{agent_id}/events`, #856).
    pub agent_senders: Arc<DashMap<AgentId, Vec<mpsc::UnboundedSender<SseEventData>>>>,
    /// Agent-scoped event log for `Last-Event-Id` reconnect on the
    /// agent-scoped feed.
    pub agent_event_log: AgentEventLogManager,
    /// Counter of in-flight (spawned but not yet finished) run tasks.
    in_flight: Arc<AtomicUsize>,
    /// Notified when an in-flight run completes (counter reaches zero).
    drain_notify: Arc<tokio::sync::Notify>,
    /// Per-run cancellation tokens for cooperative cancellation.
    cancel_tokens: Arc<DashMap<RunId, CancellationToken>>,
    /// Optional SQLite store for run persistence.
    sqlite_store: Option<Arc<alms_session::SqliteStore>>,
}

impl RunManager {
    pub fn new() -> Self {
        Self {
            event_senders: Arc::new(DashMap::new()),
            runs: Arc::new(DashMap::new()),
            event_log: EventLogManager::new(),
            session_senders: Arc::new(DashMap::new()),
            session_event_log: crate::event_log::SessionEventLogManager::new(),
            agent_senders: Arc::new(DashMap::new()),
            agent_event_log: AgentEventLogManager::new(),
            in_flight: Arc::new(AtomicUsize::new(0)),
            drain_notify: Arc::new(tokio::sync::Notify::new()),
            cancel_tokens: Arc::new(DashMap::new()),
            sqlite_store: None,
        }
    }

    /// Set the SQLite store for run persistence.
    pub fn with_store(mut self, store: Arc<alms_session::SqliteStore>) -> Self {
        self.sqlite_store = Some(store);
        self
    }

    /// Load persisted runs from SQLite into the in-memory DashMap.
    ///
    /// First marks any stale `queued`/`running` rows as `failed` (leftovers
    /// from a previous process that crashed or was killed), then loads recent
    /// terminal runs (completed/failed/cancelled) from the last 7 days.
    pub fn hydrate_from_store(&self) {
        let Some(store) = &self.sqlite_store else {
            return;
        };

        // Mark stale queued/running runs as failed before loading.
        match store.mark_stale_runs_failed() {
            Ok(count) if count > 0 => {
                info!(
                    "Marked {} stale queued/running runs as failed (gateway restarted)",
                    count
                );
            }
            Err(e) => {
                tracing::warn!("Failed to mark stale runs as failed: {}", e);
            }
            _ => {}
        }

        match store.load_all_runs() {
            Ok(runs) => {
                let loaded = runs.len();
                for run in runs {
                    self.runs.insert(run.run_id, run);
                }
                if loaded > 0 {
                    info!("Loaded {} persisted runs from SQLite", loaded);
                }
            }
            Err(e) => {
                tracing::warn!("Failed to load runs from SQLite: {}", e);
            }
        }
    }

    /// Increment the in-flight counter. Call when spawning a run task.
    pub fn track_in_flight(&self) {
        self.in_flight.fetch_add(1, Ordering::AcqRel);
    }

    /// Decrement the in-flight counter and wake drain waiters.
    pub fn untrack_in_flight(&self) {
        let prev = self.in_flight.fetch_sub(1, Ordering::AcqRel);
        if prev == 1 {
            self.drain_notify.notify_waiters();
        }
    }

    /// Wait until all in-flight runs complete, or timeout expires.
    /// Returns `true` if drained, `false` on timeout.
    ///
    /// Uses an absolute deadline so the timeout is not reset when
    /// intermediate notifications arrive (e.g. individual runs completing
    /// while others are still in progress).
    pub async fn wait_drain(&self, timeout: std::time::Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Register the notification future BEFORE checking the counter
            // to avoid lost wakeups.
            let notified = self.drain_notify.notified();
            if self.in_flight.load(Ordering::Acquire) == 0 {
                return true;
            }
            tokio::select! {
                _ = notified => {}
                _ = tokio::time::sleep_until(deadline) => return false,
            }
        }
    }

    pub fn register_sender(&self, run_id: RunId, sender: mpsc::UnboundedSender<SseEventData>) {
        self.event_senders.entry(run_id).or_default().push(sender);
    }

    pub fn remove_senders(&self, run_id: RunId) {
        self.event_senders.remove(&run_id);
    }

    /// Remove sender entries for runs that have already reached a terminal
    /// state. This is a defense-in-depth measure against the TOCTOU race in
    /// SSE subscription (see #149): if a sender is registered between the
    /// status check and `remove_senders` in `execute_run`, the entry becomes
    /// orphaned. Calling this periodically (or on demand) cleans up any
    /// leaked entries.
    pub fn purge_terminal_senders(&self) {
        self.event_senders.retain(|run_id, _| {
            let is_terminal = self
                .runs
                .get(run_id)
                .map(|r| {
                    matches!(
                        r.status,
                        alms_core::RunStatus::Completed
                            | alms_core::RunStatus::Failed
                            | alms_core::RunStatus::Cancelled
                    )
                })
                .unwrap_or(true); // run not found => definitely stale
            if is_terminal {
                tracing::debug!(run_id = %run_id.0, "Purged orphaned sender entry for terminal run");
            }
            !is_terminal
        });
    }

    pub fn insert_run(&self, run: Run) {
        if let Some(store) = &self.sqlite_store
            && let Err(e) = store.save_run(&run)
        {
            tracing::warn!(run_id = %run.run_id.0, "Failed to persist new run to SQLite: {e}");
        }
        self.runs.insert(run.run_id, run);
    }

    pub fn get_run(&self, run_id: RunId) -> Option<Run> {
        self.runs.get(&run_id).map(|r| r.value().clone())
    }

    pub fn update_run(&self, run: Run) {
        if let Some(store) = &self.sqlite_store
            && let Err(e) = store.save_run(&run)
        {
            tracing::warn!(run_id = %run.run_id.0, "Failed to persist run update to SQLite: {e}");
        }
        self.runs.insert(run.run_id, run);
    }

    /// Atomically transition a run to Running state and persist the snapshot.
    ///
    /// The run data is cloned while still holding the DashMap lock, so the
    /// persisted state cannot reflect a concurrent mutation.
    pub fn mark_run_as_running(&self, run_id: RunId) {
        let snapshot = self.modify_and_snapshot(run_id, |r| r.mark_running());
        self.persist_snapshot(run_id, snapshot);
    }

    /// Atomically transition a run to `Running` AND attach the layered
    /// run-config snapshot (#837), persisting both in a single SQLite
    /// upsert.
    ///
    /// Mirrors [`Self::mark_run_as_running`] for the state-flip side and
    /// adds [`alms_core::Run::set_resolved_config`] inside the same
    /// DashMap-locked closure so the persisted snapshot always reflects
    /// the new `Running` status with the snapshot present, never a torn
    /// intermediate state. Used by `lifecycle::execute_run` after the
    /// per-run > per-agent > server-default layering has settled and the
    /// notification-run debug-flip has applied — i.e. the values the LLM
    /// adapter actually uses on the wire.
    pub fn mark_run_as_running_with_config(
        &self,
        run_id: RunId,
        resolved_config: alms_core::ResolvedRunConfig,
    ) {
        let snapshot = self.modify_and_snapshot(run_id, |r| {
            r.mark_running();
            r.set_resolved_config(resolved_config);
        });
        self.persist_snapshot(run_id, snapshot);
    }

    /// Atomically transition a run to Completed state and persist the snapshot.
    ///
    /// Returns `true` if the run was actually transitioned from `Running`
    /// to `Completed`, `false` if the run was already in a terminal state
    /// or had not yet been marked `Running` (or did not exist). Callers
    /// that need to fan out a `run_finished` SSE event (or any other
    /// post-completion bookkeeping — episodic summary, DM lifecycle
    /// handler) MUST gate on this return value so the side effects fire
    /// exactly once per run, even when an HTTP `cancel_run` races against
    /// natural completion. See issues #1046 and #1052 for the race
    /// motivation. Skips the SQLite write when no transition occurred so
    /// a stale `Completed` snapshot can't clobber an existing
    /// `Cancelled`/`Failed` row.
    #[must_use]
    pub fn mark_run_as_completed(
        &self,
        run_id: RunId,
        output: String,
        usage: alms_core::TokenUsage,
    ) -> bool {
        self.modify_and_persist_if(run_id, |r| r.mark_completed(output, usage))
    }

    /// Atomically transition a run to Failed state and persist the snapshot.
    ///
    /// Returns `true` if the run was actually transitioned to `Failed`
    /// from `Queued` or `Running`, `false` if the run was already in a
    /// terminal state (or did not exist). Callers that need to fan out a
    /// `run_error` SSE event should gate the broadcast on this return
    /// value — same first-writer-wins shape as
    /// [`Self::mark_run_as_cancelled`] and [`Self::mark_run_as_completed`].
    /// See issues #1046 / #1052.
    #[must_use]
    pub fn mark_run_as_failed(&self, run_id: RunId, error: String) -> bool {
        self.modify_and_persist_if(run_id, |r| r.mark_failed(error))
    }

    /// Atomically transition a run to Cancelled state and persist the snapshot.
    ///
    /// Returns `true` if the run was actually transitioned from a non-terminal
    /// state (`Queued`/`Running`) to `Cancelled`, `false` if the run was
    /// already in a terminal state (or did not exist). Callers that need to
    /// fan out a `run_cancelled` SSE event should gate the broadcast on this
    /// return value so the event fires exactly once per run, even when the
    /// synchronous HTTP `cancel_run` handler (#1050) races against
    /// `execute_run`'s terminal arm for the same `(run_id)`. See issues
    /// #1046 / #1052 for the race motivation.
    #[must_use]
    pub fn mark_run_as_cancelled(&self, run_id: RunId) -> bool {
        self.modify_and_persist_if(run_id, |r| r.mark_cancelled())
    }

    /// Modify a run under DashMap lock; persist the snapshot only when the
    /// closure reports a real transition.
    ///
    /// Used by the three [`alms_core::Run::mark_completed`] /
    /// [`alms_core::Run::mark_failed`] / [`alms_core::Run::mark_cancelled`]
    /// terminal transitions, all of which return `bool` (#1052). If the
    /// run was already terminal the closure returns `false`, we skip the
    /// SQLite write to avoid clobbering the existing terminal row, and we
    /// propagate `false` to the caller so it can skip its post-flip side
    /// effects (DM lifecycle, SSE broadcast, episodic summary).
    fn modify_and_persist_if(&self, run_id: RunId, f: impl FnOnce(&mut Run) -> bool) -> bool {
        let Some(mut entry) = self.runs.get_mut(&run_id) else {
            return false;
        };
        let transitioned = f(entry.value_mut());
        let snapshot = if transitioned {
            Some(entry.clone())
        } else {
            None
        };
        // Drop the DashMap lock before the SQLite write to keep the
        // critical section short.
        drop(entry);
        if transitioned {
            self.persist_snapshot(run_id, snapshot);
        }
        transitioned
    }

    /// Modify a run in the DashMap and return a clone while still under lock.
    ///
    /// Returns `None` if the run does not exist (callers always `insert_run`
    /// first, so this should not happen in practice).
    fn modify_and_snapshot(&self, run_id: RunId, f: impl FnOnce(&mut Run)) -> Option<Run> {
        let mut entry = self.runs.get_mut(&run_id)?;
        f(entry.value_mut());
        Some(entry.clone())
    }

    /// Persist a previously-snapshotted run to SQLite (if store is configured).
    fn persist_snapshot(&self, run_id: RunId, snapshot: Option<Run>) {
        if let Some(store) = &self.sqlite_store
            && let Some(run) = snapshot
            && let Err(e) = store.save_run(&run)
        {
            tracing::warn!(run_id = %run_id.0, "Failed to persist run to SQLite: {e}");
        }
    }

    /// Store a per-run cancellation token.
    pub fn register_cancel_token(&self, run_id: RunId, token: CancellationToken) {
        self.cancel_tokens.insert(run_id, token);
    }

    /// Trigger cancellation for a run. Returns true if the token was found.
    pub fn cancel_run(&self, run_id: RunId) -> bool {
        if let Some(entry) = self.cancel_tokens.get(&run_id) {
            entry.value().cancel();
            true
        } else {
            false
        }
    }

    /// Remove a per-run cancellation token (cleanup after run ends).
    pub fn remove_cancel_token(&self, run_id: RunId) {
        self.cancel_tokens.remove(&run_id);
    }

    /// Cancel every registered in-flight run.
    ///
    /// Called during graceful shutdown so that agent loops exit at their next
    /// cancellation check-point instead of running to completion.
    pub fn cancel_all_in_flight(&self) -> usize {
        let mut count = 0;
        for entry in self.cancel_tokens.iter() {
            entry.value().cancel();
            count += 1;
        }
        count
    }

    /// Return the current value of the in-flight counter.
    pub fn in_flight_count(&self) -> usize {
        self.in_flight.load(Ordering::Acquire)
    }

    /// Cancel any in-progress runs spawned by a given job.
    ///
    /// Iterates in-memory runs, finds those with `job_id == Some(target)` that
    /// are still `Queued` or `Running`, and triggers their cancellation tokens.
    /// Returns the number of runs cancelled.
    pub fn cancel_runs_for_job(&self, target: alms_core::JobId) -> usize {
        let active_run_ids: Vec<RunId> = self
            .runs
            .iter()
            .filter(|entry| {
                let run = entry.value();
                run.job_id == Some(target)
                    && matches!(
                        run.status,
                        alms_core::RunStatus::Queued | alms_core::RunStatus::Running
                    )
            })
            .map(|entry| entry.key().to_owned())
            .collect();

        let mut cancelled = 0;
        for run_id in active_run_ids {
            if self.cancel_run(run_id) {
                tracing::info!(
                    run_id = %run_id.0,
                    job_id = %target,
                    "Cancelled in-progress run for cancelled job"
                );
                cancelled += 1;
            }
        }
        cancelled
    }

    /// Cancel any active (queued/running) runs on a given session.
    ///
    /// Returns the number of runs cancelled. Used by the DM cancel endpoint
    /// to stop in-progress DM runs before signalling conversation end.
    pub fn cancel_runs_for_session(&self, target: SessionId) -> usize {
        let active_run_ids: Vec<RunId> = self
            .runs
            .iter()
            .filter(|entry| {
                let run = entry.value();
                run.session_id == target
                    && matches!(
                        run.status,
                        alms_core::RunStatus::Queued | alms_core::RunStatus::Running
                    )
            })
            .map(|entry| entry.key().to_owned())
            .collect();

        let mut cancelled = 0;
        for run_id in active_run_ids {
            if self.cancel_run(run_id) {
                tracing::info!(
                    run_id = %run_id.0,
                    session_id = %target.0,
                    "Cancelled in-progress run for DM cancellation"
                );
                cancelled += 1;
            }
        }
        cancelled
    }

    /// Returns `true` if any queued or running runs exist for the given session.
    pub fn has_active_runs(&self, session_id: SessionId) -> bool {
        self.runs.iter().any(|e| {
            let r = e.value();
            r.session_id == session_id
                && matches!(
                    r.status,
                    alms_core::RunStatus::Queued | alms_core::RunStatus::Running
                )
        })
    }

    /// List `Queued` runs for an agent, sorted FIFO by `created_at` ASC.
    ///
    /// Used by the queue-position broadcast (#831) to compute each queued
    /// run's 1-indexed position after the head of the per-agent queue
    /// advances. The FIFO ordering matches the actual dispatch order
    /// enforced by `SessionQueue`.
    pub fn list_queued_for_agent(&self, agent_id: alms_core::AgentId) -> Vec<Run> {
        let mut runs: Vec<Run> = self
            .runs
            .iter()
            .filter(|e| {
                let r = e.value();
                r.agent_id == agent_id && matches!(r.status, alms_core::RunStatus::Queued)
            })
            .map(|e| e.value().clone())
            .collect();
        runs.sort_by_key(|r| r.created_at);
        runs
    }

    /// Returns `true` if any `Running` run exists for the given agent.
    ///
    /// Used by `create_run` to compute `queued_behind` accurately: the per-agent
    /// `SessionQueue::pending_count` only counts items still waiting in the
    /// queue (a currently-executing item has already been dequeued and its
    /// pending counter decremented). Without this check, a user who sends a
    /// message while the agent is running another task would see
    /// `queued_behind == 0` and the UI would show "Thinking..." even though
    /// the run is in fact queued behind the running one.
    pub fn agent_has_running_run(&self, agent_id: alms_core::AgentId) -> bool {
        self.runs.iter().any(|e| {
            let r = e.value();
            r.agent_id == agent_id && matches!(r.status, alms_core::RunStatus::Running)
        })
    }

    /// List runs for a session, newest first, up to `limit`.
    pub fn list_by_session(&self, session_id: SessionId, limit: usize) -> Vec<Run> {
        let mut runs: Vec<Run> = self
            .runs
            .iter()
            .filter(|e| e.value().session_id == session_id)
            .map(|e| e.value().clone())
            .collect();
        runs.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        runs.truncate(limit);
        runs
    }

    /// List runs for an agent across all sessions, newest first, up to `limit`.
    pub fn list_by_agent(&self, agent_id: alms_core::AgentId, limit: usize) -> Vec<Run> {
        let mut runs: Vec<Run> = self
            .runs
            .iter()
            .filter(|e| e.value().agent_id == agent_id)
            .map(|e| e.value().clone())
            .collect();
        runs.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        runs.truncate(limit);
        runs
    }

    /// Send event to all active subscribers AND persist to event log.
    /// Dead subscribers (closed channels) are pruned automatically.
    /// Fans out to both per-run and per-session subscribers.
    pub async fn send_event(&self, run_id: RunId, session_id: SessionId, mut event: SseEventData) {
        let is_ephemeral = event.event_type == "token_delta"
            || event.event_type == "status"
            || event.event_type == "context_debug";

        // Per-run event log — skip ephemeral events (token_delta, status,
        // context_debug) to avoid persisting high-frequency or transient data.
        // Status events are superseded within milliseconds by the next phase
        // or by run_finished, so replaying stale ones on SSE reconnect would
        // be confusing. context_debug snapshots can be 100-400KB for large
        // context windows and are only meaningful at the moment they are
        // emitted — replaying them on reconnect would waste memory and
        // bandwidth. Live subscribers still receive these events via fan-out
        // below.
        if !is_ephemeral {
            let event_id = self
                .event_log
                .log_event(run_id, session_id, &event.event_type, event.data.clone())
                .await;
            event.event_id = Some(event_id);
        }

        if let Some(mut senders) = self.event_senders.get_mut(&run_id) {
            let before = senders.len();
            senders.retain(|sender| sender.send(event.clone()).is_ok());
            let pruned = before - senders.len();
            if pruned > 0 {
                tracing::debug!(run_id = %run_id.0, pruned, "Pruned dead SSE subscriber(s)");
            }
        }

        // Per-session fan-out: forward to session subscribers.
        // Skip session event LOG for ephemeral events (token_delta, status,
        // context_debug) — deltas are high-frequency, status events are
        // transient, and context_debug snapshots are large and only meaningful
        // at the moment of emission. Session reconnect doesn't need individual
        // deltas — the chat history is loaded via getSessionMessages instead.
        let session_event = if is_ephemeral {
            // Fast path: no logging, just fan out. Leave event_id as None
            // so the dedup filter in stream_with_replay passes it through
            // (dedup only drops Some(id) where id <= max_replay_id).
            event
        } else {
            let session_event_id = self
                .session_event_log
                .log_event(session_id, run_id, &event.event_type, event.data.clone())
                .await;
            let mut e = event;
            e.event_id = Some(session_event_id);
            e
        };

        if let Some(mut senders) = self.session_senders.get_mut(&session_id) {
            senders.retain(|sender| sender.send(session_event.clone()).is_ok());
            if senders.is_empty() {
                drop(senders);
                self.session_senders.remove(&session_id);
            }
        }
    }

    /// Send a session-only event (not associated with a specific run).
    pub async fn send_session_event(
        &self,
        session_id: SessionId,
        run_id: RunId,
        event: SseEventData,
    ) {
        let session_event_id = self
            .session_event_log
            .log_event(session_id, run_id, &event.event_type, event.data.clone())
            .await;
        let mut tagged = event;
        tagged.event_id = Some(session_event_id);

        if let Some(mut senders) = self.session_senders.get_mut(&session_id) {
            senders.retain(|sender| sender.send(tagged.clone()).is_ok());
            if senders.is_empty() {
                drop(senders);
                self.session_senders.remove(&session_id);
            }
        }
    }

    pub fn register_session_sender(
        &self,
        session_id: SessionId,
        sender: mpsc::UnboundedSender<SseEventData>,
    ) {
        self.session_senders
            .entry(session_id)
            .or_default()
            .push(sender);
    }

    /// Register an SSE sender for the agent-scoped feed
    /// (`GET /agents/{agent_id}/events`, #856).
    pub fn register_agent_sender(
        &self,
        agent_id: AgentId,
        sender: mpsc::UnboundedSender<SseEventData>,
    ) {
        self.agent_senders.entry(agent_id).or_default().push(sender);
    }

    /// Send an agent-scoped event to the per-agent SSE feed and persist
    /// it to the agent event log for SSE reconnect (#856).
    ///
    /// Filtering is performed at the sender map: only subscribers to
    /// `agent_id`'s feed receive the event. Subscribers to other agents'
    /// feeds (or no feed at all) are unaffected.
    pub async fn send_agent_event(
        &self,
        agent_id: AgentId,
        run_id: RunId,
        session_id: SessionId,
        event: SseEventData,
    ) {
        let event_id = self
            .agent_event_log
            .log_event(
                agent_id,
                run_id,
                session_id,
                &event.event_type,
                event.data.clone(),
            )
            .await;
        let mut tagged = event;
        tagged.event_id = Some(event_id);

        if let Some(mut senders) = self.agent_senders.get_mut(&agent_id) {
            senders.retain(|sender| sender.send(tagged.clone()).is_ok());
            if senders.is_empty() {
                drop(senders);
                self.agent_senders.remove(&agent_id);
            }
        }
    }

    /// Get agent-scoped events from a specific ID for SSE reconnect (#856).
    pub async fn agent_events_from(&self, agent_id: AgentId, from_id: u64) -> Vec<LoggedEvent> {
        self.agent_event_log.events_from(agent_id, from_id).await
    }

    /// Close all active SSE sender channels (per-run, per-session, per-agent).
    ///
    /// Dropping the senders causes the corresponding `UnboundedReceiverStream`
    /// in each SSE response to terminate, which allows Axum's graceful
    /// shutdown to complete instead of waiting indefinitely for long-lived
    /// SSE connections.
    pub fn close_all_senders(&self) {
        let run_count = self.event_senders.len();
        let session_count = self.session_senders.len();
        let agent_count = self.agent_senders.len();
        self.event_senders.clear();
        self.session_senders.clear();
        self.agent_senders.clear();
        if run_count + session_count + agent_count > 0 {
            info!(
                "Closed {} per-run, {} per-session, and {} per-agent SSE sender(s) for shutdown",
                run_count, session_count, agent_count
            );
        }
    }

    /// Get per-run events from a specific ID for reconnect
    pub async fn events_from(&self, run_id: RunId, from_id: u64) -> Vec<LoggedEvent> {
        self.event_log.events_from(run_id, from_id).await
    }

    /// Get per-session events from a specific ID for reconnect
    pub async fn session_events_from(
        &self,
        session_id: SessionId,
        from_id: u64,
    ) -> Vec<LoggedEvent> {
        self.session_event_log
            .events_from(session_id, from_id)
            .await
    }

    /// Return the highest session-level event ID, or `None` if no events exist.
    ///
    /// Exposed to the REST messages endpoint so the client can pass
    /// `?last_event_id=<n>` when opening the SSE stream.
    pub async fn latest_session_event_id(&self, session_id: SessionId) -> Option<u64> {
        self.session_event_log.latest_event_id(session_id).await
    }
}

impl Default for RunManager {
    fn default() -> Self {
        Self::new()
    }
}

impl alms_core::RunRegistrar for RunManager {
    fn register_run(&self, run: Run) {
        self.insert_run(run);
    }

    fn update_run(&self, run: Run) {
        RunManager::update_run(self, run);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alms_core::AgentId;

    #[tokio::test]
    async fn test_drain_immediate_when_no_in_flight() {
        let rm = RunManager::new();
        assert!(rm.wait_drain(std::time::Duration::from_millis(100)).await);
    }

    #[tokio::test]
    async fn test_drain_waits_for_in_flight() {
        let rm = RunManager::new();
        rm.track_in_flight();

        let rm2 = rm.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            rm2.untrack_in_flight();
        });

        assert!(rm.wait_drain(std::time::Duration::from_secs(1)).await);
    }

    #[tokio::test]
    async fn test_drain_times_out() {
        let rm = RunManager::new();
        rm.track_in_flight();
        // Never untrack — should time out.
        assert!(!rm.wait_drain(std::time::Duration::from_millis(50)).await);
    }

    /// Verify that intermediate notifications (in_flight going 1->0->1) do
    /// not reset the absolute deadline in `wait_drain`.
    #[tokio::test(start_paused = true)]
    async fn test_drain_timeout_is_absolute_not_per_notification() {
        let rm = RunManager::new();
        rm.track_in_flight(); // run A

        let rm2 = rm.clone();
        tokio::spawn(async move {
            // After 20ms: run A finishes (1->0, notifies), run B starts (0->1)
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            rm2.untrack_in_flight();
            rm2.track_in_flight();

            // After another 20ms: same pattern
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            rm2.untrack_in_flight();
            rm2.track_in_flight();

            // After another 20ms: same — now 60ms total, past the 50ms deadline
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            rm2.untrack_in_flight();
            rm2.track_in_flight();
            // Never untrack the last one
        });

        // 50ms total timeout — should fire even though notifications arrived
        // at 20ms and 40ms (which would have reset the timer before the fix).
        assert!(!rm.wait_drain(std::time::Duration::from_millis(50)).await);
    }

    #[tokio::test]
    async fn test_multi_subscriber_broadcast() {
        let rm = RunManager::new();
        let run_id = RunId::new();
        let session_id = SessionId::new();

        let (tx1, mut rx1) = mpsc::unbounded_channel();
        let (tx2, mut rx2) = mpsc::unbounded_channel();
        rm.register_sender(run_id, tx1);
        rm.register_sender(run_id, tx2);

        rm.send_event(run_id, session_id, SseEventData::connected(run_id))
            .await;

        let e1 = rx1.recv().await.expect("subscriber 1 should receive");
        let e2 = rx2.recv().await.expect("subscriber 2 should receive");
        assert_eq!(e1.event_type, "connected");
        assert_eq!(e2.event_type, "connected");
    }

    #[tokio::test]
    async fn test_dead_subscriber_pruned() {
        let rm = RunManager::new();
        let run_id = RunId::new();
        let session_id = SessionId::new();

        let (tx_alive, mut rx_alive) = mpsc::unbounded_channel();
        let (tx_dead, rx_dead) = mpsc::unbounded_channel();
        rm.register_sender(run_id, tx_alive);
        rm.register_sender(run_id, tx_dead);

        // Drop the dead receiver so its sender becomes closed
        drop(rx_dead);

        rm.send_event(run_id, session_id, SseEventData::connected(run_id))
            .await;

        // Alive subscriber still gets the event
        let e = rx_alive
            .recv()
            .await
            .expect("alive subscriber should receive");
        assert_eq!(e.event_type, "connected");

        // Dead sender should have been pruned — only 1 sender left
        let senders = rm.event_senders.get(&run_id).unwrap();
        assert_eq!(senders.len(), 1);
    }

    #[tokio::test]
    async fn test_remove_senders_cleans_all() {
        let rm = RunManager::new();
        let run_id = RunId::new();

        let (tx1, _rx1) = mpsc::unbounded_channel();
        let (tx2, _rx2) = mpsc::unbounded_channel();
        rm.register_sender(run_id, tx1);
        rm.register_sender(run_id, tx2);

        assert!(rm.event_senders.contains_key(&run_id));
        rm.remove_senders(run_id);
        assert!(!rm.event_senders.contains_key(&run_id));
    }

    #[tokio::test]
    async fn test_cancel_run_triggers_token() {
        let rm = RunManager::new();
        let run_id = RunId::new();
        let token = CancellationToken::new();
        rm.register_cancel_token(run_id, token.clone());

        assert!(!token.is_cancelled());
        assert!(rm.cancel_run(run_id));
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn test_cancel_unknown_run_returns_false() {
        let rm = RunManager::new();
        assert!(!rm.cancel_run(RunId::new()));
    }

    #[tokio::test]
    async fn test_remove_cancel_token_cleanup() {
        let rm = RunManager::new();
        let run_id = RunId::new();
        let token = CancellationToken::new();
        rm.register_cancel_token(run_id, token);

        rm.remove_cancel_token(run_id);
        // After removal, cancel_run should return false
        assert!(!rm.cancel_run(run_id));
    }

    #[tokio::test]
    async fn test_agent_has_running_run_returns_true_for_running() {
        let rm = RunManager::new();
        let agent_id = AgentId::new();

        // No runs at all — false.
        assert!(!rm.agent_has_running_run(agent_id));

        // Insert a Queued run — still false (only Running counts).
        let queued = Run::new(SessionId::new(), agent_id, "queued".into());
        let queued_id = queued.run_id;
        rm.insert_run(queued);
        assert!(
            !rm.agent_has_running_run(agent_id),
            "Queued run should not count"
        );

        // Mark it Running — now true.
        rm.mark_run_as_running(queued_id);
        assert!(rm.agent_has_running_run(agent_id));

        // Mark it Completed — back to false.
        assert!(rm.mark_run_as_completed(queued_id, "output".into(), Default::default()));
        assert!(!rm.agent_has_running_run(agent_id));
    }

    #[tokio::test]
    async fn test_agent_has_running_run_isolates_by_agent() {
        let rm = RunManager::new();
        let agent_a = AgentId::new();
        let agent_b = AgentId::new();

        let run_a = Run::new(SessionId::new(), agent_a, "a".into());
        let run_a_id = run_a.run_id;
        rm.insert_run(run_a);
        rm.mark_run_as_running(run_a_id);

        // Agent A is running, agent B is not.
        assert!(rm.agent_has_running_run(agent_a));
        assert!(!rm.agent_has_running_run(agent_b));
    }

    #[tokio::test]
    async fn test_mark_run_as_cancelled() {
        let rm = RunManager::new();
        let run = Run::new(SessionId::new(), AgentId::new(), "test".to_string());
        let run_id = run.run_id;
        rm.insert_run(run);
        rm.mark_run_as_running(run_id);
        assert!(rm.mark_run_as_cancelled(run_id));

        let r = rm.get_run(run_id).unwrap();
        assert_eq!(r.status, alms_core::RunStatus::Cancelled);
        assert!(r.ended_at.is_some());
    }

    /// #1052 — terminal transitions are idempotent and return `false` on
    /// the second call. The lifecycle layer reads this bool to decide
    /// whether to fire `dm_conversation_ended` / `run_finished` /
    /// `run_error` SSE events; an already-terminal run must yield a
    /// `false` so the racing arm doesn't double-broadcast or strand the
    /// DM peer's conversation-state model.
    #[tokio::test]
    async fn test_mark_run_terminal_is_idempotent() {
        let rm = RunManager::new();
        let run = Run::new(SessionId::new(), AgentId::new(), "test".to_string());
        let run_id = run.run_id;
        rm.insert_run(run);
        rm.mark_run_as_running(run_id);

        // First cancel wins.
        assert!(
            rm.mark_run_as_cancelled(run_id),
            "first mark_run_as_cancelled must return true"
        );

        // Second cancel is a no-op — the bool gate is what
        // `execute_run`'s terminal arms rely on (#1052).
        assert!(
            !rm.mark_run_as_cancelled(run_id),
            "second mark_run_as_cancelled on an already-Cancelled run must return false"
        );

        // A racing `mark_run_as_completed` must NOT overwrite the
        // Cancelled status — this is the actual #1052 wart at the data
        // layer.
        assert!(
            !rm.mark_run_as_completed(run_id, "output".into(), Default::default()),
            "mark_run_as_completed on an already-Cancelled run must return false"
        );
        let r = rm.get_run(run_id).unwrap();
        assert_eq!(
            r.status,
            alms_core::RunStatus::Cancelled,
            "Cancelled must not be clobbered by a racing Completed flip"
        );

        // Mirror for the failed path.
        assert!(
            !rm.mark_run_as_failed(run_id, "ignored".into()),
            "mark_run_as_failed on an already-Cancelled run must return false"
        );
        let r = rm.get_run(run_id).unwrap();
        assert_eq!(
            r.status,
            alms_core::RunStatus::Cancelled,
            "Cancelled must not be clobbered by a racing Failed flip"
        );
    }

    #[tokio::test]
    async fn test_purge_terminal_senders_removes_completed() {
        let rm = RunManager::new();
        let active_id = RunId::new();
        let done_id = RunId::new();

        // Insert two runs: one running, one completed.
        let mut active_run = Run::new(SessionId::new(), AgentId::new(), "active".to_string());
        let mut done_run = Run::new(SessionId::new(), AgentId::new(), "done".to_string());
        // Override IDs for deterministic test.
        active_run.run_id = active_id;
        done_run.run_id = done_id;

        rm.insert_run(active_run);
        rm.insert_run(done_run);
        rm.mark_run_as_running(active_id);
        rm.mark_run_as_running(done_id);
        assert!(rm.mark_run_as_completed(done_id, "output".into(), Default::default()));

        // Register senders for both.
        let (tx1, _rx1) = mpsc::unbounded_channel();
        let (tx2, _rx2) = mpsc::unbounded_channel();
        rm.register_sender(active_id, tx1);
        rm.register_sender(done_id, tx2);

        assert!(rm.event_senders.contains_key(&active_id));
        assert!(rm.event_senders.contains_key(&done_id));

        rm.purge_terminal_senders();

        // Active run's sender should be kept, completed run's sender removed.
        assert!(rm.event_senders.contains_key(&active_id));
        assert!(!rm.event_senders.contains_key(&done_id));
    }

    #[tokio::test]
    async fn test_purge_terminal_senders_removes_missing_runs() {
        let rm = RunManager::new();
        let orphan_id = RunId::new();

        // Register a sender for a run that doesn't exist in the runs map.
        let (tx, _rx) = mpsc::unbounded_channel();
        rm.register_sender(orphan_id, tx);
        assert!(rm.event_senders.contains_key(&orphan_id));

        rm.purge_terminal_senders();

        // Sender for nonexistent run should be purged.
        assert!(!rm.event_senders.contains_key(&orphan_id));
    }

    #[tokio::test]
    async fn test_cancel_runs_for_job_cancels_active_run() {
        let rm = RunManager::new();
        let job_id = alms_core::JobId::new();

        // Create a run associated with the job.
        let mut run = Run::for_job(
            SessionId::new(),
            AgentId::new(),
            "job prompt".to_string(),
            job_id,
        );
        let run_id = run.run_id;
        run.mark_running();
        rm.insert_run(run);

        let token = CancellationToken::new();
        rm.register_cancel_token(run_id, token.clone());

        assert!(!token.is_cancelled());
        let cancelled = rm.cancel_runs_for_job(job_id);
        assert_eq!(cancelled, 1);
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn test_cancel_runs_for_job_skips_completed_run() {
        let rm = RunManager::new();
        let job_id = alms_core::JobId::new();

        // Create a completed run for the same job — should not be cancelled.
        let run = Run::for_job(
            SessionId::new(),
            AgentId::new(),
            "done prompt".to_string(),
            job_id,
        );
        let run_id = run.run_id;
        rm.insert_run(run);
        rm.mark_run_as_running(run_id);
        assert!(rm.mark_run_as_completed(run_id, "output".into(), Default::default()));

        let token = CancellationToken::new();
        rm.register_cancel_token(run_id, token.clone());

        let cancelled = rm.cancel_runs_for_job(job_id);
        assert_eq!(cancelled, 0);
        assert!(!token.is_cancelled());
    }

    #[tokio::test]
    async fn test_cancel_runs_for_job_ignores_other_jobs() {
        let rm = RunManager::new();
        let target_job = alms_core::JobId::new();
        let other_job = alms_core::JobId::new();

        // Create a running run for a *different* job.
        let mut run = Run::for_job(
            SessionId::new(),
            AgentId::new(),
            "other prompt".to_string(),
            other_job,
        );
        let run_id = run.run_id;
        run.mark_running();
        rm.insert_run(run);

        let token = CancellationToken::new();
        rm.register_cancel_token(run_id, token.clone());

        let cancelled = rm.cancel_runs_for_job(target_job);
        assert_eq!(cancelled, 0);
        assert!(!token.is_cancelled());
    }

    #[tokio::test]
    async fn test_cancel_all_in_flight_cancels_all_tokens() {
        let rm = RunManager::new();

        let t1 = CancellationToken::new();
        let t2 = CancellationToken::new();
        let t3 = CancellationToken::new();
        rm.register_cancel_token(RunId::new(), t1.clone());
        rm.register_cancel_token(RunId::new(), t2.clone());
        rm.register_cancel_token(RunId::new(), t3.clone());

        let count = rm.cancel_all_in_flight();
        assert_eq!(count, 3);
        assert!(t1.is_cancelled());
        assert!(t2.is_cancelled());
        assert!(t3.is_cancelled());
    }

    #[tokio::test]
    async fn test_cancel_all_in_flight_empty() {
        let rm = RunManager::new();
        assert_eq!(rm.cancel_all_in_flight(), 0);
    }

    #[tokio::test]
    async fn test_in_flight_count() {
        let rm = RunManager::new();
        assert_eq!(rm.in_flight_count(), 0);
        rm.track_in_flight();
        assert_eq!(rm.in_flight_count(), 1);
        rm.track_in_flight();
        assert_eq!(rm.in_flight_count(), 2);
        rm.untrack_in_flight();
        assert_eq!(rm.in_flight_count(), 1);
    }

    /// After cancelling all tokens, runs that check their cancel_token
    /// should exit promptly, allowing wait_drain to complete quickly.
    #[tokio::test]
    async fn test_cancel_all_unblocks_drain() {
        let rm = RunManager::new();
        rm.track_in_flight();
        let token = CancellationToken::new();
        rm.register_cancel_token(RunId::new(), token.clone());

        let rm2 = rm.clone();
        tokio::spawn(async move {
            // Simulate a run that checks its cancel token and exits.
            token.cancelled().await;
            rm2.untrack_in_flight();
        });

        // Cancel all tokens — the spawned "run" should exit and untrack.
        rm.cancel_all_in_flight();

        // Should drain almost immediately (well within 1s).
        assert!(rm.wait_drain(std::time::Duration::from_secs(1)).await);
    }

    #[tokio::test]
    async fn test_list_by_agent_filters_by_agent_id() {
        let rm = RunManager::new();
        let agent_a = AgentId::new();
        let agent_b = AgentId::new();

        // Insert 3 runs for agent_a and 1 for agent_b.
        rm.insert_run(Run::new(SessionId::new(), agent_a, "a1".into()));
        rm.insert_run(Run::new(SessionId::new(), agent_a, "a2".into()));
        rm.insert_run(Run::new(SessionId::new(), agent_a, "a3".into()));
        rm.insert_run(Run::new(SessionId::new(), agent_b, "b1".into()));

        let runs_a = rm.list_by_agent(agent_a, 50);
        assert_eq!(runs_a.len(), 3);
        assert!(runs_a.iter().all(|r| r.agent_id == agent_a));

        let runs_b = rm.list_by_agent(agent_b, 50);
        assert_eq!(runs_b.len(), 1);
        assert_eq!(runs_b[0].agent_id, agent_b);
    }

    #[tokio::test]
    async fn test_list_by_agent_respects_limit() {
        let rm = RunManager::new();
        let agent_id = AgentId::new();

        for i in 0..5 {
            rm.insert_run(Run::new(SessionId::new(), agent_id, format!("run {i}")));
        }

        let runs = rm.list_by_agent(agent_id, 3);
        assert_eq!(runs.len(), 3);
    }

    #[tokio::test]
    async fn test_list_by_agent_sorted_newest_first() {
        let rm = RunManager::new();
        let agent_id = AgentId::new();

        let r1 = Run::new(SessionId::new(), agent_id, "first".into());
        let r2 = Run::new(SessionId::new(), agent_id, "second".into());
        rm.insert_run(r1);
        rm.insert_run(r2);

        let runs = rm.list_by_agent(agent_id, 50);
        assert_eq!(runs.len(), 2);
        // Newest first: second run should come before first.
        assert!(runs[0].created_at >= runs[1].created_at);
    }

    #[tokio::test]
    async fn test_list_by_agent_spans_multiple_sessions() {
        let rm = RunManager::new();
        let agent_id = AgentId::new();
        let session_a = SessionId::new();
        let session_b = SessionId::new();

        rm.insert_run(Run::new(session_a, agent_id, "sa".into()));
        rm.insert_run(Run::new(session_b, agent_id, "sb".into()));

        let runs = rm.list_by_agent(agent_id, 50);
        assert_eq!(runs.len(), 2);

        let session_ids: std::collections::HashSet<_> = runs.iter().map(|r| r.session_id).collect();
        assert!(session_ids.contains(&session_a));
        assert!(session_ids.contains(&session_b));
    }

    #[tokio::test]
    async fn test_list_by_agent_empty() {
        let rm = RunManager::new();
        let runs = rm.list_by_agent(AgentId::new(), 50);
        assert!(runs.is_empty());
    }

    // -------------------------------------------------------------------
    // #735 regression coverage
    //
    // The frontend's active-run restoration path uses these list functions
    // (via GET /runs?session_id=... and ?agent_id=...) to find a still-
    // running run after reload or session switch. Because the lists are
    // newest-first and truncated, an older still-running run can be hidden
    // behind a backlog of newer queued / terminal runs — leaving the UI
    // unable to rehydrate the thinking indicator / approvals.
    //
    // The frontend now requests SESSION_RUNS_RESTORE_LIMIT (200) /
    // AGENT_RUNS_RESTORE_LIMIT (100) for the restore step. These tests
    // pin the data-layer contract that a sufficiently large limit DOES
    // surface the older running run, and that the previous default
    // (20 / 10) does not. Together with the ordering preference in
    // load-session.js they form the regression coverage requested in
    // the issue.
    // -------------------------------------------------------------------

    fn make_run_at(
        session_id: SessionId,
        agent_id: AgentId,
        created_at: chrono::DateTime<chrono::Utc>,
        status: alms_core::RunStatus,
    ) -> Run {
        let mut run = Run::new(session_id, agent_id, "test".into());
        run.created_at = created_at;
        run.status = status;
        run
    }

    /// Regression #735 / scenario 1: a session with an older `running` run
    /// and a newer `queued` run must surface the running run when the
    /// frontend looks for an in-progress run to restore.
    ///
    /// `list_by_session` itself is order-only — the running-vs-queued
    /// preference lives in the JS `loadSession()` (`load-session.js`).
    /// This test asserts the data-layer invariant: both runs are present
    /// in the returned slice so the JS preference can pick the correct
    /// one. (Without this, the JS fix would be defeated by truncation.)
    #[tokio::test]
    async fn test_list_by_session_includes_older_running_with_newer_queued() {
        let rm = RunManager::new();
        let session_id = SessionId::new();
        let agent_id = AgentId::new();
        let base = chrono::Utc::now();

        let older_running = make_run_at(
            session_id,
            agent_id,
            base - chrono::Duration::seconds(10),
            alms_core::RunStatus::Running,
        );
        let newer_queued = make_run_at(session_id, agent_id, base, alms_core::RunStatus::Queued);
        let older_id = older_running.run_id;
        let newer_id = newer_queued.run_id;

        rm.insert_run(older_running);
        rm.insert_run(newer_queued);

        // Both runs are returned (limit is generous) and ordered newest-first.
        let runs = rm.list_by_session(session_id, 200);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].run_id, newer_id, "newest first");
        assert_eq!(runs[1].run_id, older_id);
        assert_eq!(runs[1].status, alms_core::RunStatus::Running);

        // The frontend prefers running over queued, so the running run
        // is what activeRunId.value should resolve to. We assert that
        // selection logic here as a data-layer cross-check.
        let active = runs
            .iter()
            .find(|r| r.status == alms_core::RunStatus::Running)
            .or_else(|| {
                runs.iter()
                    .find(|r| r.status == alms_core::RunStatus::Queued)
            });
        assert_eq!(active.map(|r| r.run_id), Some(older_id));
    }

    /// Regression #735 / scenario 2: a session where the running run is
    /// older than 20 newer runs (the previous frontend default limit)
    /// would have lost the running run; with the new larger limit it
    /// must remain visible.
    #[tokio::test]
    async fn test_list_by_session_running_run_visible_beyond_old_default() {
        let rm = RunManager::new();
        let session_id = SessionId::new();
        let agent_id = AgentId::new();
        let base = chrono::Utc::now();

        // Insert one OLD running run.
        let old_running = make_run_at(
            session_id,
            agent_id,
            base - chrono::Duration::seconds(60),
            alms_core::RunStatus::Running,
        );
        let old_running_id = old_running.run_id;
        rm.insert_run(old_running);

        // Insert 25 newer terminal runs in front of it.
        for i in 0..25 {
            let run = make_run_at(
                session_id,
                agent_id,
                base - chrono::Duration::seconds(50 - i),
                alms_core::RunStatus::Completed,
            );
            rm.insert_run(run);
        }

        // Old (broken) behaviour: limit=20 truncates the running run away.
        let small = rm.list_by_session(session_id, 20);
        assert_eq!(small.len(), 20);
        assert!(
            !small.iter().any(|r| r.run_id == old_running_id),
            "old default limit (20) hides the still-running run"
        );

        // New behaviour: SESSION_RUNS_RESTORE_LIMIT (200) keeps it visible.
        let restored = rm.list_by_session(session_id, 200);
        assert_eq!(restored.len(), 26);
        assert!(
            restored
                .iter()
                .any(|r| r.run_id == old_running_id && r.status == alms_core::RunStatus::Running),
            "new restore limit (200) surfaces the still-running run"
        );
    }

    /// Regression #735 / scenario 3: `restoreGlobalAgentPhase()` looks
    /// for a running DM run for an agent across sessions. With the
    /// previous hardcoded limit of 10, a still-running DM run that is
    /// older than 10 newer runs would be lost, breaking the cross-session
    /// "Chatting with {peer}..." status. The new limit (100) must keep
    /// the active DM run visible.
    #[tokio::test]
    async fn test_list_by_agent_active_dm_visible_beyond_old_default() {
        let rm = RunManager::new();
        let agent_id = AgentId::new();
        let dm_session_id = SessionId::new();
        let base = chrono::Utc::now();

        // The active DM run, which is older than the noise.
        let dm_running = make_run_at(
            dm_session_id,
            agent_id,
            base - chrono::Duration::seconds(120),
            alms_core::RunStatus::Running,
        );
        let dm_running_id = dm_running.run_id;
        rm.insert_run(dm_running);

        // 15 newer non-DM runs across other sessions for the same agent.
        for i in 0..15 {
            let run = make_run_at(
                SessionId::new(),
                agent_id,
                base - chrono::Duration::seconds(100 - i),
                alms_core::RunStatus::Completed,
            );
            rm.insert_run(run);
        }

        // Old (broken) behaviour: limit=10 truncates the DM run away.
        let small = rm.list_by_agent(agent_id, 10);
        assert_eq!(small.len(), 10);
        assert!(
            !small.iter().any(|r| r.run_id == dm_running_id),
            "old default limit (10) hides the active DM run"
        );

        // New behaviour: AGENT_RUNS_RESTORE_LIMIT (100) keeps it visible.
        let restored = rm.list_by_agent(agent_id, 100);
        assert_eq!(restored.len(), 16);
        assert!(
            restored
                .iter()
                .any(|r| r.run_id == dm_running_id && r.status == alms_core::RunStatus::Running),
            "new restore limit (100) surfaces the active DM run"
        );
    }

    /// `has_active_runs` returns `true` when at least one run on the
    /// session is in `Queued` or `Running` state, and `false` once all
    /// runs reach a terminal state. Backs the `has_active_run` field on
    /// `GET /sessions` (#856).
    #[tokio::test]
    async fn test_has_active_runs_reflects_run_lifecycle() {
        let rm = RunManager::new();
        let session_id = SessionId::new();
        let agent_id = AgentId::new();

        // No runs yet.
        assert!(!rm.has_active_runs(session_id));

        // Insert a queued run -> active.
        let run = Run::new(session_id, agent_id, "test".into());
        let run_id = run.run_id;
        rm.insert_run(run);
        assert!(rm.has_active_runs(session_id));

        // Mark running -> still active.
        rm.mark_run_as_running(run_id);
        assert!(rm.has_active_runs(session_id));

        // Mark completed -> no longer active.
        assert!(rm.mark_run_as_completed(run_id, "ok".into(), Default::default()));
        assert!(!rm.has_active_runs(session_id));
    }

    /// `has_active_runs` is correctly scoped by `session_id`. A running
    /// run on session B does not make session A appear active.
    #[tokio::test]
    async fn test_has_active_runs_scoped_by_session() {
        let rm = RunManager::new();
        let session_a = SessionId::new();
        let session_b = SessionId::new();
        let agent_id = AgentId::new();

        let run_b = Run::new(session_b, agent_id, "on B".into());
        let run_b_id = run_b.run_id;
        rm.insert_run(run_b);
        rm.mark_run_as_running(run_b_id);

        assert!(!rm.has_active_runs(session_a), "session A has no runs");
        assert!(rm.has_active_runs(session_b), "session B has a running run");
    }

    /// `send_agent_event` fans out only to subscribers of the matching
    /// `agent_id` — confirming the per-agent SSE feed is properly scoped
    /// (#856).
    #[tokio::test]
    async fn test_agent_event_fanout_filtered_by_agent_id() {
        let rm = RunManager::new();
        let agent_a = AgentId::new();
        let agent_b = AgentId::new();
        let session_id = SessionId::new();
        let run_id = RunId::new();

        let (tx_a, mut rx_a) = mpsc::unbounded_channel();
        let (tx_b, mut rx_b) = mpsc::unbounded_channel();
        rm.register_agent_sender(agent_a, tx_a);
        rm.register_agent_sender(agent_b, tx_b);

        // Send an event for agent A only.
        rm.send_agent_event(
            agent_a,
            run_id,
            session_id,
            SseEventData::session_activity_started(session_id, run_id, agent_a),
        )
        .await;

        // Agent A's subscriber receives the event...
        let received = rx_a.recv().await.expect("agent A should receive");
        assert_eq!(received.event_type, "session_activity_started");
        assert_eq!(received.data["agent_id"], agent_a.0.to_string());

        // ...and agent B's subscriber receives nothing.
        assert!(
            rx_b.try_recv().is_err(),
            "agent B must not receive events for agent A"
        );
    }

    #[tokio::test]
    async fn test_agent_event_replay_via_events_from() {
        let rm = RunManager::new();
        let agent_id = AgentId::new();
        let session_id = SessionId::new();
        let run_id = RunId::new();

        rm.send_agent_event(
            agent_id,
            run_id,
            session_id,
            SseEventData::session_activity_started(session_id, run_id, agent_id),
        )
        .await;
        rm.send_agent_event(
            agent_id,
            run_id,
            session_id,
            SseEventData::session_activity_ended(session_id, run_id, agent_id),
        )
        .await;

        let events = rm.agent_events_from(agent_id, 0).await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "session_activity_started");
        assert_eq!(events[1].event_type, "session_activity_ended");
    }
}
