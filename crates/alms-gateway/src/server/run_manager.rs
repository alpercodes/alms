// SPDX-License-Identifier: Apache-2.0

//! Run tracking, event broadcasting, cancellation, and persistence.
//!
//! [`RunManager`] is the central piece of run lifecycle management in the
//! gateway.  It owns the in-memory run map, per-run and per-session SSE
//! senders, in-flight counters for graceful shutdown, and optional SQLite
//! persistence.

use crate::event_log::{AgentEventLogManager, EventLogManager, LoggedEvent, ReplayWindow};
use crate::sse::SseEventData;
use alms_core::{AgentId, Run, RunId, RunTransition, SessionId, TransitionOutcome};
use dashmap::{DashMap, mapref::entry::Entry};
use std::collections::HashMap;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use tokio::sync::mpsc;
use tokio_stream::Stream;
use tokio_util::sync::CancellationToken;
use tracing::info;

/// Fixed internal key for the DEDICATED [`RunManager::activity_event_log`]
/// instance that backs the global cross-agent session-activity feed
/// (`GET /events/session-activity`, #1211).
///
/// Its value is irrelevant to correctness: `activity_event_log` is a
/// SEPARATE `AgentEventLogManager` from the per-agent `agent_event_log`, and
/// the global feed's subscribers live in a SEPARATE `activity_senders` list
/// — never in the `AgentId`-indexed `agent_senders` map. That separation is
/// deliberate and load-bearing: `ALMS_AGENT_ID`, the sidecar loader, and the
/// registry all accept an ARBITRARY agent UUID, so any key shared with the
/// per-agent namespace could be claimed by a real agent and (a) leak every
/// other agent's activity onto its supposedly agent-scoped
/// `/agents/{id}/events` feed and (b) make its own activity skip the mirror.
/// Keeping the activity feed in its own namespace makes that whole collision
/// class impossible by construction. Private — never routed or accepted as
/// an agent id.
const ACTIVITY_LOG_KEY: AgentId = AgentId(uuid::Uuid::from_bytes([0xAC; 16]));

/// Per-run accumulator of in-flight visible-reply text for the current
/// parent-agent turn (#1107).
///
/// See the `run_text_buffers` field on [`RunManager`] for the rationale.
/// `text` is the concatenation of every `token_delta` chunk fired by the
/// parent agent (subagent deltas are filtered out — they belong to a
/// different surface) since the last parent-agent `tool_start` /
/// `tool_end` event, or since the start of the run if no parent-agent
/// tool event has fired yet. `last_session_event_id` is the session
/// event log HWM at the moment the most recent chunk was appended —
/// used as the SSE replay watermark so the client can advance its
/// `Last-Event-Id` cursor past any logged events that fired alongside
/// the rehydrated deltas, mirroring the contract of the reasoning
/// rehydration endpoint.
#[derive(Debug, Clone, Default)]
pub struct RunTextBuffer {
    pub text: String,
    pub last_session_event_id: Option<u64>,
}

const SSE_SUBSCRIBER_BUFFER: usize = 256;

#[derive(Debug, Clone)]
pub(crate) enum SubscriptionSender {
    /// Replayable feeds can evict a slow consumer and let it recover from its
    /// persisted cursor without losing semantic state.
    Bounded(mpsc::Sender<SseEventData>),
    /// Run/session feeds also carry live-only deltas. They must preserve
    /// delivery until disconnect because replay cannot reconstruct them.
    Lossless(mpsc::UnboundedSender<SseEventData>),
}

#[derive(Debug)]
enum SubscriptionReceiver {
    Bounded(mpsc::Receiver<SseEventData>),
    Lossless(mpsc::UnboundedReceiver<SseEventData>),
}

#[derive(Debug, Clone, Copy)]
enum SubscriptionDelivery {
    Bounded,
    Lossless,
}

type SubscriberMap<K> = Arc<DashMap<K, HashMap<u64, SubscriptionSender>>>;

/// Live SSE subscription with prompt unregister-on-drop cleanup.
///
/// Run/session streams use lossless channels because they contain transient
/// events that cannot be replayed. Agent/global-activity streams are bounded
/// because every semantic event is logged and recoverable after eviction.
/// All variants use the same drop guard, so an idle disconnect cannot leave a
/// sender behind until the next event.
pub struct ManagedSubscription<K>
where
    K: Eq + Hash + Clone,
{
    id: u64,
    key: K,
    receiver: SubscriptionReceiver,
    senders: SubscriberMap<K>,
}

impl<K> Stream for ManagedSubscription<K>
where
    K: Eq + Hash + Clone + Unpin,
{
    type Item = SseEventData;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match &mut self.receiver {
            SubscriptionReceiver::Bounded(receiver) => receiver.poll_recv(cx),
            SubscriptionReceiver::Lossless(receiver) => receiver.poll_recv(cx),
        }
    }
}

impl<K> ManagedSubscription<K>
where
    K: Eq + Hash + Clone,
{
    /// Queue a synthetic attach-time snapshot through this subscription.
    pub(crate) fn try_send(&self, event: SseEventData) -> bool {
        self.senders
            .get(&self.key)
            .and_then(|senders| senders.get(&self.id).cloned())
            .is_some_and(|sender| match sender {
                SubscriptionSender::Bounded(sender) => sender.try_send(event).is_ok(),
                SubscriptionSender::Lossless(sender) => sender.send(event).is_ok(),
            })
    }

    #[cfg(test)]
    pub(crate) fn try_recv(&mut self) -> Result<SseEventData, mpsc::error::TryRecvError> {
        match &mut self.receiver {
            SubscriptionReceiver::Bounded(receiver) => receiver.try_recv(),
            SubscriptionReceiver::Lossless(receiver) => receiver.try_recv(),
        }
    }

    pub async fn recv(&mut self) -> Option<SseEventData> {
        match &mut self.receiver {
            SubscriptionReceiver::Bounded(receiver) => receiver.recv().await,
            SubscriptionReceiver::Lossless(receiver) => receiver.recv().await,
        }
    }
}

impl<K> Drop for ManagedSubscription<K>
where
    K: Eq + Hash + Clone,
{
    fn drop(&mut self) {
        if let Entry::Occupied(mut entry) = self.senders.entry(self.key.clone()) {
            entry.get_mut().remove(&self.id);
            if entry.get().is_empty() {
                entry.remove();
            }
        }
    }
}

fn subscribe_to<K>(
    senders: &SubscriberMap<K>,
    next_subscription_id: &AtomicU64,
    key: K,
    delivery: SubscriptionDelivery,
) -> ManagedSubscription<K>
where
    K: Eq + Hash + Clone,
{
    let id = next_subscription_id.fetch_add(1, Ordering::Relaxed);
    let (sender, receiver) = match delivery {
        SubscriptionDelivery::Bounded => {
            let (sender, receiver) = mpsc::channel(SSE_SUBSCRIBER_BUFFER);
            (
                SubscriptionSender::Bounded(sender),
                SubscriptionReceiver::Bounded(receiver),
            )
        }
        SubscriptionDelivery::Lossless => {
            let (sender, receiver) = mpsc::unbounded_channel();
            (
                SubscriptionSender::Lossless(sender),
                SubscriptionReceiver::Lossless(receiver),
            )
        }
    };
    senders.entry(key.clone()).or_default().insert(id, sender);
    ManagedSubscription {
        id,
        key,
        receiver,
        senders: Arc::clone(senders),
    }
}

fn fan_out_to<K>(senders: &SubscriberMap<K>, key: K, event: &SseEventData)
where
    K: Eq + Hash + Clone,
{
    let Entry::Occupied(mut entry) = senders.entry(key) else {
        return;
    };
    let mut slow = 0usize;
    entry.get_mut().retain(|_, sender| match sender {
        SubscriptionSender::Bounded(sender) => match sender.try_send(event.clone()) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                slow += 1;
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        },
        SubscriptionSender::Lossless(sender) => sender.send(event.clone()).is_ok(),
    });
    if entry.get().is_empty() {
        entry.remove();
    }
    if slow > 0 {
        tracing::warn!(
            dropped_subscribers = slow,
            buffer_capacity = SSE_SUBSCRIBER_BUFFER,
            "Dropped slow SSE subscribers after their bounded buffers filled"
        );
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct RunManagerMetrics {
    pub transition_rejections_total: u64,
    pub replay_gaps_total: u64,
    pub replay_epoch_mismatches_total: u64,
    pub run_subscribers: usize,
    pub session_subscribers: usize,
    pub agent_subscribers: usize,
    pub activity_subscribers: usize,
}

/// Run manager for tracking runs and their event streams
#[derive(Debug, Clone)]
pub struct RunManager {
    /// Random identity for this gateway lifetime. SSE clients use it to
    /// distinguish a valid cursor from one minted before a restart.
    stream_epoch: uuid::Uuid,
    pub(crate) event_senders: SubscriberMap<RunId>,
    pub(crate) runs: Arc<DashMap<RunId, Run>>,
    /// In-memory event log for SSE reconnect during current process lifetime.
    /// Events are lost on restart — this does **not** provide cross-restart durability.
    pub event_log: EventLogManager,
    /// Session-level event senders for persistent SSE streams.
    pub(crate) session_senders: SubscriberMap<SessionId>,
    /// Session-level event log for reconnect support.
    pub session_event_log: crate::event_log::SessionEventLogManager,
    /// Agent-scoped event senders for the per-agent SSE feed
    /// (`GET /agents/{agent_id}/events`, #856).
    pub(crate) agent_senders: SubscriberMap<AgentId>,
    /// Agent-scoped event log for `Last-Event-Id` reconnect on the
    /// agent-scoped feed.
    pub agent_event_log: AgentEventLogManager,
    /// Subscriber list for the GLOBAL cross-agent session-activity feed
    /// (`GET /events/session-activity`, #1211). A DEDICATED namespace,
    /// separate from the `AgentId`-indexed `agent_senders` map, so no
    /// operator-supplied agent id can ever collide with it and leak
    /// cross-agent activity onto a per-agent feed. Each entry is removed by its
    /// [`ManagedSubscription`] drop guard as soon as the HTTP stream closes.
    activity_senders: SubscriberMap<()>,
    next_subscription_id: Arc<AtomicU64>,
    transition_rejections: Arc<AtomicU64>,
    replay_gap_detections: Arc<AtomicU64>,
    replay_epoch_mismatches: Arc<AtomicU64>,
    /// Serializes authoritative session-activity snapshots with global event
    /// logging, so concurrent run transitions cannot publish an older
    /// `has_active_run` value after a newer one.
    activity_event_gate: Arc<tokio::sync::Mutex<()>>,
    /// Event log backing `Last-Event-Id` replay on the global activity feed
    /// (#1211). A dedicated `AgentEventLogManager` instance (separate from
    /// `agent_event_log`), keyed internally by [`ACTIVITY_LOG_KEY`]. Reuses
    /// the agent-log machinery for the `AGENT_EVENT_LOG_MAX` bound + replay.
    activity_event_log: AgentEventLogManager,
    /// Per-run accumulator of in-flight visible-reply text (#1107).
    ///
    /// `token_delta` SSE events are deliberately *not* persisted to either
    /// the per-run or per-session event log (they are flagged ephemeral in
    /// `send_event` for cost reasons — visible text is flushed to the
    /// message store at end-of-turn). That leaves the in-flight assistant
    /// reply with no rehydration source: switching into / out of a session
    /// mid-stream causes the partial reply to disappear because the
    /// history GET doesn't have it yet and the SSE replay cursor sits
    /// past every delta that already fired. This buffer is the in-memory
    /// equivalent of the reasoning-rehydration path from #1043 / #1077,
    /// scoped per parent-agent turn (cleared on parent-agent
    /// `tool_start` / `tool_end`).
    pub run_text_buffers: Arc<DashMap<RunId, RunTextBuffer>>,
    /// Counter of in-flight (spawned but not yet finished) run tasks.
    in_flight: Arc<AtomicUsize>,
    /// Notified when an in-flight run completes (counter reaches zero).
    drain_notify: Arc<tokio::sync::Notify>,
    /// Per-run cancellation tokens for cooperative cancellation.
    cancel_tokens: Arc<DashMap<RunId, CancellationToken>>,
    /// Optional SQLite store for run persistence.
    sqlite_store: Option<Arc<alms_session::SqliteStore>>,
    #[cfg(test)]
    fail_next_persistence: Arc<AtomicBool>,
}

impl RunManager {
    pub fn new() -> Self {
        Self {
            stream_epoch: uuid::Uuid::new_v4(),
            event_senders: Arc::new(DashMap::new()),
            runs: Arc::new(DashMap::new()),
            event_log: EventLogManager::new(),
            session_senders: Arc::new(DashMap::new()),
            session_event_log: crate::event_log::SessionEventLogManager::new(),
            agent_senders: Arc::new(DashMap::new()),
            agent_event_log: AgentEventLogManager::new(),
            activity_senders: Arc::new(DashMap::new()),
            next_subscription_id: Arc::new(AtomicU64::new(1)),
            transition_rejections: Arc::new(AtomicU64::new(0)),
            replay_gap_detections: Arc::new(AtomicU64::new(0)),
            replay_epoch_mismatches: Arc::new(AtomicU64::new(0)),
            activity_event_gate: Arc::new(tokio::sync::Mutex::new(())),
            activity_event_log: AgentEventLogManager::new(),
            run_text_buffers: Arc::new(DashMap::new()),
            in_flight: Arc::new(AtomicUsize::new(0)),
            drain_notify: Arc::new(tokio::sync::Notify::new()),
            cancel_tokens: Arc::new(DashMap::new()),
            sqlite_store: None,
            #[cfg(test)]
            fail_next_persistence: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn stream_epoch(&self) -> uuid::Uuid {
        self.stream_epoch
    }

    pub fn operational_metrics(&self) -> RunManagerMetrics {
        RunManagerMetrics {
            transition_rejections_total: self.transition_rejections.load(Ordering::Relaxed),
            replay_gaps_total: self.replay_gap_detections.load(Ordering::Relaxed),
            replay_epoch_mismatches_total: self.replay_epoch_mismatches.load(Ordering::Relaxed),
            run_subscribers: self.event_senders.iter().map(|entry| entry.len()).sum(),
            session_subscribers: self.session_senders.iter().map(|entry| entry.len()).sum(),
            agent_subscribers: self.agent_senders.iter().map(|entry| entry.len()).sum(),
            activity_subscribers: self.activity_senders.iter().map(|entry| entry.len()).sum(),
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
    ///
    /// Since #1236 a row that cannot be reconciled is skipped inside the sweep
    /// (logged with its remediation SQL and counted in
    /// `stale_run_recovery_failures_total`), so the `Err` arm below is reached
    /// only when the sweep transaction itself cannot begin or commit — a
    /// whole-database problem, not one bad row. Such a skipped row is then
    /// also excluded from the in-memory projection: booting past it must not
    /// mean serving it as a live pending run.
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
                tracing::error!(
                    "Failed to quarantine stale runs; refusing to hydrate run state: {}",
                    e
                );
                return;
            }
            _ => {}
        }

        match store.load_all_runs() {
            Ok(runs) => {
                let mut loaded = 0usize;
                let mut unreconciled = 0usize;
                for run in runs {
                    // `load_all_runs` has no status filter, so after a
                    // successful sweep any row still `Queued`/`Running` is by
                    // definition one the sweep failed to reconcile — it is
                    // already counted in `stale_run_recovery_failures_total`
                    // and logged with its remediation SQL. Leaving it durable
                    // is right; projecting it into the live registry is not.
                    // A phantom pending run reads as real to `has_active_runs`
                    // (making the session permanently undeletable via
                    // `DELETE /sessions/{id}`, which is the operator's natural
                    // remediation), pins `has_active_run: true` on the sidebar
                    // forever, and — because it predates this process — sorts
                    // to the head of the `created_at ASC` queue, shifting every
                    // real queue position for that agent.
                    if matches!(
                        run.status(),
                        alms_core::RunStatus::Queued | alms_core::RunStatus::Running
                    ) {
                        unreconciled += 1;
                        continue;
                    }
                    self.runs.insert(run.run_id, run);
                    loaded += 1;
                }
                if loaded > 0 {
                    info!("Loaded {} persisted runs from SQLite", loaded);
                }
                if unreconciled > 0 {
                    tracing::error!(
                        unreconciled,
                        "Skipped {unreconciled} unreconciled queued/running run row(s) during \
                         hydration — they stay durable and are reported by \
                         stale_run_recovery_failures_total, but are not projected as live runs \
                         (#1236)"
                    );
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

    pub fn subscribe_run(&self, run_id: RunId) -> ManagedSubscription<RunId> {
        subscribe_to(
            &self.event_senders,
            &self.next_subscription_id,
            run_id,
            SubscriptionDelivery::Lossless,
        )
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
                        r.status(),
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

    pub fn insert_run(&self, run: Run) -> alms_core::AlmsResult<()> {
        self.update_run(run).map(|_| ())
    }

    /// Persist the two durable facts that define HTTP run admission as one
    /// transaction. Returns the transaction's session activity timestamp when
    /// SQLite is enabled.
    pub(crate) fn persist_run_admission(
        &self,
        run: &Run,
        message: &alms_session::Message,
    ) -> alms_core::AlmsResult<Option<alms_core::Timestamp>> {
        self.fail_if_persistence_injected()?;
        self.sqlite_store
            .as_ref()
            .map(|store| store.save_run_with_initial_message(run, message))
            .transpose()
    }

    /// Publish a run that was already committed by `persist_run_admission`
    /// into the in-memory projection without issuing a second SQLite write.
    pub(crate) fn insert_persisted_run(&self, run: Run) -> alms_core::AlmsResult<()> {
        let run_id = run.run_id;
        match self.runs.entry(run_id) {
            Entry::Vacant(entry) => {
                entry.insert(run);
                Ok(())
            }
            Entry::Occupied(_) => Err(alms_core::AlmsError::Runtime(format!(
                "run {} was already registered during admission",
                run_id.0
            ))),
        }
    }

    pub fn get_run(&self, run_id: RunId) -> Option<Run> {
        self.runs.get(&run_id).map(|r| r.value().clone())
    }

    pub fn update_run(&self, run: Run) -> alms_core::AlmsResult<bool> {
        let run_id = run.run_id;
        let revision = run.lifecycle_revision();
        let accepted = match self.runs.entry(run_id) {
            Entry::Vacant(entry) => {
                self.persist_run_candidate(&run)?;
                entry.insert(run);
                true
            }
            Entry::Occupied(mut entry) => {
                let current = entry.get();
                let accept = run.lifecycle_revision() > current.lifecycle_revision();
                if accept {
                    if let Err(error) = self.persist_run_candidate(&run) {
                        let current_is_active = matches!(
                            current.status(),
                            alms_core::RunStatus::Queued | alms_core::RunStatus::Running
                        );
                        let candidate_is_terminal = matches!(
                            run.status(),
                            alms_core::RunStatus::Completed
                                | alms_core::RunStatus::Failed
                                | alms_core::RunStatus::Cancelled
                        );
                        if current_is_active && candidate_is_terminal {
                            let mut quarantine = current.clone();
                            let _ = quarantine.transition(RunTransition::Fail {
                                error: format!("lifecycle persistence failed: {error}"),
                                terminal_reason: "persistence_failed".to_string(),
                            });
                            entry.insert(quarantine);
                        }
                        return Err(error);
                    }
                    entry.insert(run);
                }
                accept
            }
        };
        if !accepted {
            tracing::debug!(
                run_id = %run_id.0,
                revision,
                "Ignored stale or conflicting run snapshot"
            );
        }
        Ok(accepted)
    }

    /// Atomically transition a run to Running state and persist the snapshot.
    ///
    /// The run data is cloned while still holding the DashMap lock, so the
    /// persisted state cannot reflect a concurrent mutation.
    pub fn mark_run_as_running(&self, run_id: RunId) -> bool {
        self.try_mark_run_as_running(run_id)
            .unwrap_or_else(|error| {
                tracing::error!(run_id = %run_id.0, "Run start persistence failed: {error}");
                false
            })
    }

    pub fn try_mark_run_as_running(&self, run_id: RunId) -> alms_core::AlmsResult<bool> {
        Ok(self
            .transition_run(
                run_id,
                RunTransition::Start {
                    resolved_config: None,
                },
            )?
            .is_some_and(|outcome| outcome.is_applied()))
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
    /// per-run > per-agent > server-default layering has settled — i.e.
    /// the values the LLM adapter actually uses on the wire.
    pub fn mark_run_as_running_with_config(
        &self,
        run_id: RunId,
        resolved_config: alms_core::ResolvedRunConfig,
    ) -> bool {
        self.try_mark_run_as_running_with_config(run_id, resolved_config)
            .unwrap_or_else(|error| {
                tracing::error!(run_id = %run_id.0, "Run start persistence failed: {error}");
                false
            })
    }

    pub fn try_mark_run_as_running_with_config(
        &self,
        run_id: RunId,
        resolved_config: alms_core::ResolvedRunConfig,
    ) -> alms_core::AlmsResult<bool> {
        Ok(self
            .transition_run(
                run_id,
                RunTransition::Start {
                    resolved_config: Some(resolved_config),
                },
            )?
            .is_some_and(|outcome| outcome.is_applied()))
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
        self.try_mark_run_as_completed(run_id, output, usage)
            .unwrap_or_else(|error| {
                tracing::error!(run_id = %run_id.0, "Run completion persistence failed: {error}");
                false
            })
    }

    pub fn try_mark_run_as_completed(
        &self,
        run_id: RunId,
        output: String,
        usage: alms_core::TokenUsage,
    ) -> alms_core::AlmsResult<bool> {
        Ok(self
            .transition_run(run_id, RunTransition::Complete { output, usage })?
            .is_some_and(|outcome| outcome.is_applied()))
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
        self.try_mark_run_as_failed(run_id, error)
            .unwrap_or_else(|persistence_error| {
                tracing::error!(run_id = %run_id.0, "Run failure persistence failed: {persistence_error}");
                false
            })
    }

    pub fn try_mark_run_as_failed(
        &self,
        run_id: RunId,
        error: String,
    ) -> alms_core::AlmsResult<bool> {
        Ok(self
            .transition_run(
                run_id,
                RunTransition::Fail {
                    error,
                    terminal_reason: "failed".to_string(),
                },
            )?
            .is_some_and(|outcome| outcome.is_applied()))
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
        self.try_mark_run_as_cancelled(run_id)
            .unwrap_or_else(|error| {
                tracing::error!(run_id = %run_id.0, "Run cancellation persistence failed: {error}");
                false
            })
    }

    pub fn try_mark_run_as_cancelled(&self, run_id: RunId) -> alms_core::AlmsResult<bool> {
        Ok(self
            .transition_run(
                run_id,
                RunTransition::Cancel {
                    terminal_reason: "cancelled".to_string(),
                },
            )?
            .is_some_and(|outcome| outcome.is_applied()))
    }

    /// Apply one authoritative lifecycle transition and persist only an
    /// accepted revision.
    pub fn transition_run(
        &self,
        run_id: RunId,
        transition: RunTransition,
    ) -> alms_core::AlmsResult<Option<TransitionOutcome<alms_core::RunStatus>>> {
        let Some(mut entry) = self.runs.get_mut(&run_id) else {
            return Ok(None);
        };
        let mut candidate = entry.clone();
        let outcome = candidate.transition(transition);
        if matches!(outcome, TransitionOutcome::Rejected { .. }) {
            self.transition_rejections.fetch_add(1, Ordering::Relaxed);
        }
        if matches!(outcome, TransitionOutcome::Rejected { .. })
            && entry.lifecycle_revision() >= alms_core::MAX_LIFECYCLE_REVISION
        {
            return Err(alms_core::AlmsError::Runtime(format!(
                "run {} lifecycle revision is exhausted",
                run_id.0
            )));
        }
        let reached_terminal = matches!(
            outcome,
            TransitionOutcome::Applied { to, .. } if to.is_terminal()
        );
        if outcome.is_applied() {
            if let Err(error) = self.persist_run_candidate(&candidate) {
                let mut quarantined = entry.clone();
                let _ = quarantined.transition(RunTransition::Fail {
                    error: format!("lifecycle persistence failed: {error}"),
                    terminal_reason: "persistence_failed".to_string(),
                });
                *entry = quarantined;
                drop(entry);
                self.run_text_buffers.remove(&run_id);
                return Err(error);
            }
            *entry = candidate;
        }
        drop(entry);
        if reached_terminal {
            self.run_text_buffers.remove(&run_id);
        }
        Ok(Some(outcome))
    }

    #[cfg(test)]
    pub(crate) fn inject_next_persistence_failure(&self) {
        self.fail_next_persistence.store(true, Ordering::Release);
    }

    fn persist_run_candidate(&self, run: &Run) -> alms_core::AlmsResult<()> {
        self.fail_if_persistence_injected()?;
        if let Some(store) = &self.sqlite_store {
            store.save_run(run)?;
        }
        Ok(())
    }

    fn fail_if_persistence_injected(&self) -> alms_core::AlmsResult<()> {
        #[cfg(test)]
        if self.fail_next_persistence.swap(false, Ordering::AcqRel) {
            return Err(alms_core::AlmsError::Runtime(
                "injected run persistence failure".to_string(),
            ));
        }
        Ok(())
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

    pub fn has_cancel_token(&self, run_id: RunId) -> bool {
        self.cancel_tokens.contains_key(&run_id)
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
                        run.status(),
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
                        run.status(),
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
                    r.status(),
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
                r.agent_id == agent_id && matches!(r.status(), alms_core::RunStatus::Queued)
            })
            .map(|e| e.value().clone())
            .collect();
        runs.sort_by_key(|r| r.created_at);
        runs
    }

    /// List `Queued` runs for a session, sorted FIFO by `created_at` ASC.
    ///
    /// Used by the approval-deny handler (#1109) — see `resolve_approval`
    /// for why queued runs are cancelled before the decision is delivered.
    pub fn list_queued_for_session(&self, session_id: SessionId) -> Vec<Run> {
        let mut runs: Vec<Run> = self
            .runs
            .iter()
            .filter(|e| {
                let r = e.value();
                r.session_id == session_id && matches!(r.status(), alms_core::RunStatus::Queued)
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
            r.agent_id == agent_id && matches!(r.status(), alms_core::RunStatus::Running)
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
            || event.event_type == "context_debug"
            || event.event_type == "subagent_activity";

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

        // In-flight visible-reply text accumulator (#1107).
        //
        // `token_delta` is ephemeral and not persisted to either log, so
        // the only way to rehydrate the partial reply when a user switches
        // into a streaming session is to keep an in-memory per-run buffer
        // here. Subagent deltas (with `source_agent` set) are filtered out
        // because the UI's live `token_delta` handler also early-returns
        // on them — the rehydration surface must mirror the live wire
        // shape to avoid surfacing text that would never have rendered on
        // a fresh subscription.
        //
        // Parent-agent `tool_start` / `tool_end` mark turn boundaries:
        // the visible text up to that point has been sealed into the
        // assistant message that the messages GET will return on the next
        // load, so the buffer must be cleared to avoid double-rendering
        // the prior turn's text on top of the freshly rehydrated sealed
        // bubble (the #1077 symptom, transplanted to the text channel).
        // Subagent tool events do not move the boundary (see the
        // analogous logic in `get_run_reasoning`).
        if event.event_type == "token_delta" {
            let is_subagent = event
                .data
                .get("source_agent")
                .map(|v| !v.is_null())
                .unwrap_or(false);
            if !is_subagent && let Some(delta) = event.data.get("delta").and_then(|v| v.as_str()) {
                // Sample the session HWM under the same read snapshot that
                // produced the latest persisted event — any non-ephemeral
                // event that fires after this sample will have an event_id
                // strictly greater than what we record here, so the client
                // bumping `lastEventId` to this watermark cannot skip
                // events that were not already accounted for elsewhere.
                let hwm = self.session_event_log.latest_event_id(session_id).await;
                let mut entry = self.run_text_buffers.entry(run_id).or_default();
                entry.text.push_str(delta);
                if let Some(id) = hwm {
                    entry.last_session_event_id = Some(
                        entry
                            .last_session_event_id
                            .map(|prev| prev.max(id))
                            .unwrap_or(id),
                    );
                }
            }
        } else if event.event_type == "tool_start" || event.event_type == "tool_end" {
            let is_subagent = event
                .data
                .get("source_agent")
                .map(|v| !v.is_null())
                .unwrap_or(false);
            if !is_subagent {
                // Parent-agent tool boundary — the assistant text accumulated
                // so far has been sealed into the closing message of the
                // prior turn; drop the buffer so the next turn starts clean.
                self.run_text_buffers.remove(&run_id);
            }
        } else if event.event_type == "stream_reset" {
            let is_subagent = event
                .data
                .get("source_agent")
                .map(|v| !v.is_null())
                .unwrap_or(false);
            if !is_subagent {
                // #1162 sym-2: the partial accumulated here belongs to the
                // abandoned stream. Drop it so a user switching into the
                // session mid-fallback does not rehydrate the stale partial —
                // the re-emitted full `token_delta` that follows re-accumulates
                // the correct text from scratch (mirrors the live UI reset).
                self.run_text_buffers.remove(&run_id);
            }
        }

        fan_out_to(&self.event_senders, run_id, &event);

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

        fan_out_to(&self.session_senders, session_id, &session_event);
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

        fan_out_to(&self.session_senders, session_id, &tagged);
    }

    /// Fan a session-only event out to live session subscribers WITHOUT
    /// persisting it to any event log.
    ///
    /// Used for high-frequency / moment-in-time signals (the `subagent_activity`
    /// status events from the background-subagent channel) that are worthless
    /// on replay and must not grow the session log. `event_id` is left `None`
    /// so the replay dedup filter in `stream_with_replay` passes the event
    /// through — the same contract as `send_event`'s ephemeral fast path.
    /// Synchronous: pure in-memory fan-out, no log I/O to await.
    pub fn send_transient_session_event(&self, session_id: SessionId, event: SseEventData) {
        fan_out_to(&self.session_senders, session_id, &event);
    }

    pub fn subscribe_session(&self, session_id: SessionId) -> ManagedSubscription<SessionId> {
        subscribe_to(
            &self.session_senders,
            &self.next_subscription_id,
            session_id,
            SubscriptionDelivery::Lossless,
        )
    }

    /// Register an SSE sender for the agent-scoped feed
    /// (`GET /agents/{agent_id}/events`, #856).
    pub fn subscribe_agent(&self, agent_id: AgentId) -> ManagedSubscription<AgentId> {
        subscribe_to(
            &self.agent_senders,
            &self.next_subscription_id,
            agent_id,
            SubscriptionDelivery::Bounded,
        )
    }

    /// Send an agent-scoped event to the per-agent SSE feed and persist
    /// it to the agent event log for SSE reconnect (#856).
    ///
    /// Filtering is performed at the sender map: only subscribers to
    /// `agent_id`'s feed receive the event. Subscribers to other agents'
    /// feeds (or no feed at all) are unaffected.
    ///
    /// **#1211 global mirror:** `session_activity_*` events are ALSO
    /// mirrored onto the global cross-agent activity feed via
    /// [`send_activity_event`](Self::send_activity_event) so the web UI
    /// sidebar can light the active-run dot on sessions owned by ANY agent
    /// (jobs, DMs, and other agents' sessions surfaced in the sidebar's
    /// cross-agent sections), not just the operator's currently-active
    /// agent. The per-agent fan-out below stays scoped — its isolation is
    /// relied on by other consumers — and the global feed lives in a
    /// SEPARATE namespace (`activity_senders` / `activity_event_log`), so
    /// per-agent replay and isolation are unaffected.
    pub async fn send_agent_event(
        &self,
        agent_id: AgentId,
        run_id: RunId,
        session_id: SessionId,
        mut event: SseEventData,
    ) {
        // Mirror session-activity onto the global cross-agent feed BEFORE the
        // per-agent fan-out consumes `event`. Unconditional for
        // `session_activity_*` — there is deliberately NO `agent_id` guard:
        // the global feed is a separate namespace (no recursion possible),
        // and an operator MAY legitimately set an agent's id (via
        // `ALMS_AGENT_ID`, the sidecar, or the registry) to any UUID, so
        // every agent's activity — including a hypothetical id-collision —
        // must still reach the global feed.
        if event.event_type.starts_with("session_activity") {
            let _activity_guard = self.activity_event_gate.lock().await;
            if let Some(data) = event.data.as_object_mut() {
                data.insert(
                    "has_active_run".to_string(),
                    serde_json::Value::Bool(self.has_active_runs(session_id)),
                );
            }
            self.send_activity_event(run_id, session_id, event.clone())
                .await;
        }

        // Per-agent fan-out (#856), unchanged: log to the agent-scoped event
        // log and deliver to that agent's `/agents/{id}/events` subscribers.
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

        fan_out_to(&self.agent_senders, agent_id, &tagged);
    }

    /// Log + fan out a `session_activity_*` event onto the GLOBAL cross-agent
    /// activity feed (`GET /events/session-activity`, #1211).
    ///
    /// Uses the DEDICATED `activity_senders` list + `activity_event_log`
    /// instance, never the `AgentId`-indexed per-agent maps, so no
    /// operator-supplied agent id can collide with the feed and leak activity
    /// across the per-agent isolation boundary. The event-log reuse gives
    /// `Last-Event-Id` replay and the `AGENT_EVENT_LOG_MAX` bound for free.
    async fn send_activity_event(&self, run_id: RunId, session_id: SessionId, event: SseEventData) {
        let event_id = self
            .activity_event_log
            .log_event(
                ACTIVITY_LOG_KEY,
                run_id,
                session_id,
                &event.event_type,
                event.data.clone(),
            )
            .await;
        let mut tagged = event;
        tagged.event_id = Some(event_id);

        fan_out_to(&self.activity_senders, (), &tagged);
    }

    /// Subscribe to the global cross-agent session-activity feed.
    ///
    /// The returned stream unregisters itself on drop, including when the
    /// browser disconnects before any further activity is broadcast.
    pub fn subscribe_activity(&self) -> ManagedSubscription<()> {
        subscribe_to(
            &self.activity_senders,
            &self.next_subscription_id,
            (),
            SubscriptionDelivery::Bounded,
        )
    }

    /// Get agent-scoped events from a specific ID for SSE reconnect (#856).
    pub async fn agent_events_from(&self, agent_id: AgentId, from_id: u64) -> Vec<LoggedEvent> {
        self.agent_event_log.events_from(agent_id, from_id).await
    }

    /// Get global cross-agent session-activity events from a specific ID for
    /// SSE reconnect (#1211). Backs `Last-Event-Id` replay on the global feed.
    pub async fn activity_events_from(&self, from_id: u64) -> Vec<LoggedEvent> {
        self.activity_event_log
            .events_from(ACTIVITY_LOG_KEY, from_id)
            .await
    }

    fn observe_replay_window(&self, window: ReplayWindow) -> ReplayWindow {
        if window.replay_gap {
            self.replay_gap_detections.fetch_add(1, Ordering::Relaxed);
        }
        window
    }

    pub(crate) fn observe_replay_epoch_mismatch(&self, epoch_mismatch: bool) {
        if epoch_mismatch {
            self.replay_epoch_mismatches.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub async fn run_replay_window(
        &self,
        run_id: RunId,
        last_event_id: Option<u64>,
    ) -> ReplayWindow {
        let window = self.event_log.replay_window(run_id, last_event_id).await;
        self.observe_replay_window(window)
    }

    pub async fn session_replay_window(
        &self,
        session_id: SessionId,
        last_event_id: Option<u64>,
    ) -> ReplayWindow {
        let window = self
            .session_event_log
            .replay_window(session_id, last_event_id)
            .await;
        self.observe_replay_window(window)
    }

    pub async fn agent_replay_window(
        &self,
        agent_id: AgentId,
        last_event_id: Option<u64>,
    ) -> ReplayWindow {
        let window = self
            .agent_event_log
            .replay_window(agent_id, last_event_id)
            .await;
        self.observe_replay_window(window)
    }

    pub async fn activity_replay_window(&self, last_event_id: Option<u64>) -> ReplayWindow {
        let window = self
            .activity_event_log
            .replay_window(ACTIVITY_LOG_KEY, last_event_id)
            .await;
        self.observe_replay_window(window)
    }

    /// Close all active SSE sender channels (per-run, per-session, per-agent).
    ///
    /// Dropping the senders causes the corresponding `UnboundedReceiverStream`
    /// in each SSE response to terminate, which allows Axum's graceful
    /// shutdown to complete instead of waiting indefinitely for long-lived
    /// SSE connections.
    pub fn close_all_senders(&self) {
        let run_count: usize = self.event_senders.iter().map(|entry| entry.len()).sum();
        let session_count: usize = self.session_senders.iter().map(|entry| entry.len()).sum();
        let agent_count: usize = self.agent_senders.iter().map(|entry| entry.len()).sum();
        self.event_senders.clear();
        self.session_senders.clear();
        self.agent_senders.clear();
        // Global cross-agent activity feed (#1211) — dedicated sender list.
        let activity_count: usize = self.activity_senders.iter().map(|entry| entry.len()).sum();
        self.activity_senders.clear();
        if run_count + session_count + agent_count + activity_count > 0 {
            info!(
                "Closed {} per-run, {} per-session, {} per-agent, and {} activity SSE sender(s) for shutdown",
                run_count, session_count, agent_count, activity_count
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

    /// Snapshot the in-flight visible-reply text buffer for a run (#1107).
    ///
    /// Returns the accumulated parent-agent `token_delta` text for the
    /// current turn, plus the session event log HWM at the moment the
    /// most recent chunk was appended. Returns `None` when the run has
    /// not yet emitted any post-boundary text — the caller (the
    /// `/runs/{id}/text` endpoint) treats `None` as an empty rehydration
    /// payload rather than a 404, mirroring the reasoning endpoint's
    /// "well-formed empty" contract.
    pub fn run_text_buffer_snapshot(&self, run_id: RunId) -> Option<RunTextBuffer> {
        self.run_text_buffers.get(&run_id).map(|r| r.clone())
    }

    /// Evict a run's in-flight visible-reply text buffer (#1180).
    ///
    /// The `mark_run_as_*` terminal helpers evict this for top-level runs, but a
    /// subagent's OWN run reaches terminal state via the coordinator's
    /// `registrar.update_run` (which does not touch this buffer), so the
    /// self-session terminal path evicts it explicitly here to avoid a leak.
    pub fn evict_run_text_buffer(&self, run_id: RunId) {
        self.run_text_buffers.remove(&run_id);
    }
}

impl Default for RunManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl alms_core::RunRegistrar for RunManager {
    async fn register_run(&self, run: Run) -> alms_core::AlmsResult<()> {
        self.insert_run(run)
    }

    fn update_run(&self, run: Run) -> alms_core::AlmsResult<()> {
        RunManager::update_run(self, run).map(|_| ())
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

    #[tokio::test]
    async fn operational_metrics_track_rejections_gaps_and_live_subscribers() {
        let rm = RunManager::new();
        let run = Run::new(SessionId::new(), AgentId::new(), "metrics".into());
        let run_id = run.run_id;
        rm.insert_run(run).unwrap();
        rm.transition_run(
            run_id,
            RunTransition::Cancel {
                terminal_reason: "test".into(),
            },
        )
        .unwrap();
        let rejected = rm
            .transition_run(
                run_id,
                RunTransition::Start {
                    resolved_config: None,
                },
            )
            .unwrap()
            .unwrap();
        assert!(matches!(rejected, TransitionOutcome::Rejected { .. }));
        assert!(rm.run_replay_window(RunId::new(), Some(9)).await.replay_gap);
        rm.observe_replay_epoch_mismatch(true);
        let subscription = rm.subscribe_activity();
        assert_eq!(rm.operational_metrics().activity_subscribers, 1);
        drop(subscription);
        let metrics = rm.operational_metrics();
        assert_eq!(metrics.transition_rejections_total, 1);
        assert_eq!(metrics.replay_gaps_total, 1);
        assert_eq!(metrics.replay_epoch_mismatches_total, 1);
        assert_eq!(metrics.activity_subscribers, 0);
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

        let mut rx1 = rm.subscribe_run(run_id);
        let mut rx2 = rm.subscribe_run(run_id);

        rm.send_event(run_id, session_id, SseEventData::connected(run_id))
            .await;

        let e1 = rx1.recv().await.expect("subscriber 1 should receive");
        let e2 = rx2.recv().await.expect("subscriber 2 should receive");
        assert_eq!(e1.event_type, "connected");
        assert_eq!(e2.event_type, "connected");
    }

    #[tokio::test]
    async fn test_dropped_subscriber_unregisters_immediately() {
        let rm = RunManager::new();
        let run_id = RunId::new();
        let session_id = SessionId::new();

        let mut rx_alive = rm.subscribe_run(run_id);
        let rx_dead = rm.subscribe_run(run_id);
        assert_eq!(rm.event_senders.get(&run_id).unwrap().len(), 2);
        drop(rx_dead);
        assert_eq!(rm.event_senders.get(&run_id).unwrap().len(), 1);

        rm.send_event(run_id, session_id, SseEventData::connected(run_id))
            .await;

        // Alive subscriber still gets the event
        let e = rx_alive
            .recv()
            .await
            .expect("alive subscriber should receive");
        assert_eq!(e.event_type, "connected");

        assert_eq!(rm.event_senders.get(&run_id).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_remove_senders_cleans_all() {
        let rm = RunManager::new();
        let run_id = RunId::new();

        let _rx1 = rm.subscribe_run(run_id);
        let _rx2 = rm.subscribe_run(run_id);

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
        let _ = rm.insert_run(queued);
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
        let _ = rm.insert_run(run_a);
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
        let _ = rm.insert_run(run);
        rm.mark_run_as_running(run_id);
        assert!(rm.mark_run_as_cancelled(run_id));

        let r = rm.get_run(run_id).unwrap();
        assert_eq!(r.status(), alms_core::RunStatus::Cancelled);
        assert!(r.ended_at.is_some());
    }

    #[test]
    fn failed_completion_persistence_does_not_commit_and_restart_recovers_running_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runs.db");
        let store = Arc::new(alms_session::SqliteStore::open(&path).unwrap());
        let rm = RunManager::new().with_store(store.clone());
        let run = Run::new(SessionId::new(), AgentId::new(), "test".to_string());
        let run_id = run.run_id;
        let _ = rm.insert_run(run);
        assert!(rm.mark_run_as_running(run_id));

        rm.inject_next_persistence_failure();
        assert!(
            rm.try_mark_run_as_completed(run_id, "must not commit".to_string(), Default::default())
                .is_err()
        );
        let current = rm.get_run(run_id).unwrap();
        assert_eq!(current.status(), alms_core::RunStatus::Failed);
        assert_eq!(current.terminal_reason(), Some("persistence_failed"));
        assert_eq!(current.output, None);
        drop(rm);
        drop(store);

        let restarted_store = Arc::new(alms_session::SqliteStore::open(&path).unwrap());
        let restarted = RunManager::new().with_store(restarted_store);
        restarted.hydrate_from_store();
        let recovered = restarted.get_run(run_id).unwrap();
        assert_eq!(recovered.status(), alms_core::RunStatus::Failed);
        assert_eq!(recovered.terminal_reason(), Some("gateway_restarted"));
        assert_eq!(recovered.output, None);
    }

    /// #1236: a run row that cannot be reconciled — here one whose lifecycle
    /// revision is exhausted, so the recovery transition is rejected — is
    /// skipped rather than aborting startup. Until #1236 this arm errored the
    /// whole sweep, `hydrate_from_store` bailed, and (via `Gateway::new`) the
    /// daemon would not boot at all. One bad row must not blank out run state
    /// or keep the gateway down; it is counted and logged with its remediation
    /// SQL instead.
    ///
    /// It must ALSO not be projected into the live registry. Booting past the
    /// row is the point; serving it as a real pending run is not — that would
    /// make its session permanently undeletable (`DELETE /sessions/{id}`
    /// returns 409 `ACTIVE_RUNS`), pin the sidebar's active-run indicator on
    /// forever, and shift every real queue position for the agent.
    #[test]
    fn unreconcilable_stale_run_is_skipped_without_blocking_hydration() {
        let store = Arc::new(alms_session::SqliteStore::open_in_memory().unwrap());
        let exhausted = Run::from_persisted(
            RunId::new(),
            SessionId::new(),
            AgentId::new(),
            alms_core::RunStatus::Queued,
            "exhausted".to_string(),
            None,
            None,
            None,
            chrono::Utc::now(),
            None,
            None,
            None,
            None,
            None,
            alms_core::MAX_LIFECYCLE_REVISION,
            None,
        );
        let exhausted_id = exhausted.run_id;
        store.save_run(&exhausted).unwrap();

        let mut healthy = Run::new(SessionId::new(), AgentId::new(), "healthy".to_string());
        assert!(healthy.mark_running());
        let healthy_id = healthy.run_id;
        store.save_run(&healthy).unwrap();

        let manager = RunManager::new().with_store(Arc::clone(&store));
        manager.hydrate_from_store();

        assert_eq!(
            store.stale_run_recovery_failures_total(),
            1,
            "the unreconcilable row must be counted, not silently dropped"
        );
        assert_eq!(
            manager.get_run(healthy_id).map(|run| run.status()),
            Some(alms_core::RunStatus::Failed),
            "the other stale run must still be quarantined and hydrated"
        );
        assert!(
            manager.get_run(exhausted_id).is_none(),
            "failed recovery must not hydrate an active exhausted run: a phantom \
             pending run makes its session permanently undeletable, pins the \
             active-run indicator, and shifts real queue positions"
        );
        assert!(
            store.load_run(exhausted_id).unwrap().is_some(),
            "the row stays durable and operator-findable — only the in-memory \
             projection is suppressed"
        );
    }

    #[test]
    fn coordinator_update_persists_before_committing_memory() {
        let store = Arc::new(alms_session::SqliteStore::open_in_memory().unwrap());
        let manager = RunManager::new().with_store(store.clone());
        let mut running = Run::new(SessionId::new(), AgentId::new(), "subagent".to_string());
        assert!(running.mark_running());
        let run_id = running.run_id;
        let _ = manager.insert_run(running.clone());
        let mut completed = running;
        assert!(completed.mark_completed("done".to_string(), Default::default()));

        manager.inject_next_persistence_failure();
        assert!(manager.update_run(completed).is_err());
        let quarantined = manager.get_run(run_id).unwrap();
        assert_eq!(quarantined.status(), alms_core::RunStatus::Failed);
        assert_eq!(quarantined.terminal_reason(), Some("persistence_failed"));
        assert_eq!(
            store.load_run(run_id).unwrap().unwrap().status(),
            alms_core::RunStatus::Running
        );
    }

    #[test]
    fn stale_coordinator_snapshot_cannot_replace_terminal_run() {
        let rm = RunManager::new();
        let mut running = Run::new(SessionId::new(), AgentId::new(), "test".into());
        assert!(running.mark_running());
        let stale = running.clone();
        let run_id = running.run_id;
        let _ = rm.insert_run(running);

        assert!(rm.mark_run_as_completed(run_id, "done".into(), Default::default()));
        rm.update_run(stale).unwrap();

        let current = rm.get_run(run_id).unwrap();
        assert_eq!(current.status(), alms_core::RunStatus::Completed);
        assert_eq!(current.lifecycle_revision(), 2);
    }

    #[test]
    fn equal_revision_snapshot_cannot_replace_authoritative_payload() {
        let rm = RunManager::new();
        let mut run = Run::new(SessionId::new(), AgentId::new(), "test".into());
        assert!(run.mark_running());
        assert!(run.mark_completed("authoritative".into(), Default::default()));
        let run_id = run.run_id;
        let _ = rm.insert_run(run.clone());

        let mut conflicting = run;
        conflicting.output = Some("delayed conflicting payload".to_string());
        rm.update_run(conflicting).unwrap();

        let current = rm.get_run(run_id).unwrap();
        assert_eq!(current.output.as_deref(), Some("authoritative"));
        assert_eq!(current.terminal_reason(), None);
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
        let _ = rm.insert_run(run);
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
            r.status(),
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
            r.status(),
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

        let _ = rm.insert_run(active_run);
        let _ = rm.insert_run(done_run);
        rm.mark_run_as_running(active_id);
        rm.mark_run_as_running(done_id);
        assert!(rm.mark_run_as_completed(done_id, "output".into(), Default::default()));

        // Register senders for both.
        let _rx1 = rm.subscribe_run(active_id);
        let _rx2 = rm.subscribe_run(done_id);

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
        let _rx = rm.subscribe_run(orphan_id);
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
        let _ = rm.insert_run(run);

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
        let _ = rm.insert_run(run);
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
        let _ = rm.insert_run(run);

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
        let _ = rm.insert_run(Run::new(SessionId::new(), agent_a, "a1".into()));
        let _ = rm.insert_run(Run::new(SessionId::new(), agent_a, "a2".into()));
        let _ = rm.insert_run(Run::new(SessionId::new(), agent_a, "a3".into()));
        let _ = rm.insert_run(Run::new(SessionId::new(), agent_b, "b1".into()));

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
            let _ = rm.insert_run(Run::new(SessionId::new(), agent_id, format!("run {i}")));
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
        let _ = rm.insert_run(r1);
        let _ = rm.insert_run(r2);

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

        let _ = rm.insert_run(Run::new(session_a, agent_id, "sa".into()));
        let _ = rm.insert_run(Run::new(session_b, agent_id, "sb".into()));

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
        match status {
            alms_core::RunStatus::Queued => {}
            alms_core::RunStatus::Running => assert!(run.mark_running()),
            alms_core::RunStatus::Completed => {
                assert!(run.mark_running());
                assert!(run.mark_completed("done".into(), Default::default()));
            }
            alms_core::RunStatus::Failed => assert!(run.mark_failed("failed".into())),
            alms_core::RunStatus::Cancelled => assert!(run.mark_cancelled()),
        }
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

        let _ = rm.insert_run(older_running);
        let _ = rm.insert_run(newer_queued);

        // Both runs are returned (limit is generous) and ordered newest-first.
        let runs = rm.list_by_session(session_id, 200);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].run_id, newer_id, "newest first");
        assert_eq!(runs[1].run_id, older_id);
        assert_eq!(runs[1].status(), alms_core::RunStatus::Running);

        // The frontend prefers running over queued, so the running run
        // is what activeRunId.value should resolve to. We assert that
        // selection logic here as a data-layer cross-check.
        let active = runs
            .iter()
            .find(|r| r.status() == alms_core::RunStatus::Running)
            .or_else(|| {
                runs.iter()
                    .find(|r| r.status() == alms_core::RunStatus::Queued)
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
        let _ = rm.insert_run(old_running);

        // Insert 25 newer terminal runs in front of it.
        for i in 0..25 {
            let run = make_run_at(
                session_id,
                agent_id,
                base - chrono::Duration::seconds(50 - i),
                alms_core::RunStatus::Completed,
            );
            let _ = rm.insert_run(run);
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
                .any(|r| r.run_id == old_running_id && r.status() == alms_core::RunStatus::Running),
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
        let _ = rm.insert_run(dm_running);

        // 15 newer non-DM runs across other sessions for the same agent.
        for i in 0..15 {
            let run = make_run_at(
                SessionId::new(),
                agent_id,
                base - chrono::Duration::seconds(100 - i),
                alms_core::RunStatus::Completed,
            );
            let _ = rm.insert_run(run);
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
                .any(|r| r.run_id == dm_running_id && r.status() == alms_core::RunStatus::Running),
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
        let _ = rm.insert_run(run);
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
        let _ = rm.insert_run(run_b);
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

        let mut rx_a = rm.subscribe_agent(agent_a);
        let mut rx_b = rm.subscribe_agent(agent_b);

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

    /// The global feed publishes the authoritative session-level predicate,
    /// not a last-event-wins interpretation of one run's terminal event.
    #[tokio::test]
    async fn test_activity_events_are_cardinality_safe_for_overlapping_runs() {
        let rm = RunManager::new();
        let agent_a = AgentId::new();
        let agent_b = AgentId::new();
        let session_id = SessionId::new();
        let mut subscription = rm.subscribe_activity();

        let run_a = Run::new(session_id, agent_a, "a".into());
        let run_a_id = run_a.run_id;
        let _ = rm.insert_run(run_a);
        rm.mark_run_as_running(run_a_id);
        rm.send_agent_event(
            agent_a,
            run_a_id,
            session_id,
            SseEventData::session_activity_started(session_id, run_a_id, agent_a),
        )
        .await;
        let started_a = subscription.try_recv().expect("first start event");
        assert_eq!(started_a.data["has_active_run"], true);

        let run_b = Run::new(session_id, agent_b, "b".into());
        let run_b_id = run_b.run_id;
        let _ = rm.insert_run(run_b);
        rm.mark_run_as_running(run_b_id);
        rm.send_agent_event(
            agent_b,
            run_b_id,
            session_id,
            SseEventData::session_activity_started(session_id, run_b_id, agent_b),
        )
        .await;
        let started_b = subscription.try_recv().expect("second start event");
        assert_eq!(started_b.data["has_active_run"], true);

        assert!(rm.mark_run_as_completed(run_a_id, "done a".into(), Default::default()));
        rm.send_agent_event(
            agent_a,
            run_a_id,
            session_id,
            SseEventData::session_activity_ended(session_id, run_a_id, agent_a),
        )
        .await;
        let ended_a = subscription.try_recv().expect("first end event");
        assert_eq!(
            ended_a.data["has_active_run"], true,
            "the second run keeps the session active"
        );

        assert!(rm.mark_run_as_completed(run_b_id, "done b".into(), Default::default()));
        rm.send_agent_event(
            agent_b,
            run_b_id,
            session_id,
            SseEventData::session_activity_ended(session_id, run_b_id, agent_b),
        )
        .await;
        let ended_b = subscription.try_recv().expect("final end event");
        assert_eq!(
            ended_b.data["has_active_run"], false,
            "the final run clears the session activity"
        );
    }

    #[test]
    fn test_activity_subscriptions_unregister_on_drop_without_broadcast() {
        let rm = RunManager::new();

        for _ in 0..100 {
            let subscription = rm.subscribe_activity();
            assert_eq!(rm.activity_senders.get(&()).unwrap().len(), 1);
            drop(subscription);
            assert!(
                rm.activity_senders.is_empty(),
                "dropping an idle subscription must unregister it immediately"
            );
        }
    }

    #[test]
    fn all_subscription_kinds_unregister_on_idle_drop() {
        let rm = RunManager::new();
        for _ in 0..100 {
            let run = rm.subscribe_run(RunId::new());
            let session = rm.subscribe_session(SessionId::new());
            let agent = rm.subscribe_agent(AgentId::new());
            drop((run, session, agent));
        }
        assert!(rm.event_senders.is_empty());
        assert!(rm.session_senders.is_empty());
        assert!(rm.agent_senders.is_empty());
    }

    #[test]
    fn replayable_slow_subscriber_is_dropped_at_bounded_buffer_limit() {
        let rm = RunManager::new();
        let agent_id = AgentId::new();
        let _subscription = rm.subscribe_agent(agent_id);
        for _ in 0..=SSE_SUBSCRIBER_BUFFER {
            fan_out_to(
                &rm.agent_senders,
                agent_id,
                &SseEventData::connected(RunId::new()),
            );
        }
        assert!(
            !rm.agent_senders.contains_key(&agent_id),
            "a replayable subscriber that cannot drain its bounded buffer must be evicted"
        );
    }

    #[test]
    fn live_only_session_burst_is_not_evicted_or_truncated() {
        let rm = RunManager::new();
        let session_id = SessionId::new();
        let mut subscription = rm.subscribe_session(session_id);
        let count = SSE_SUBSCRIBER_BUFFER + 17;

        for _ in 0..count {
            rm.send_transient_session_event(session_id, SseEventData::connected(RunId::new()));
        }

        assert!(
            rm.session_senders.contains_key(&session_id),
            "session feeds carry non-replayable events and must remain lossless"
        );
        for _ in 0..count {
            assert!(subscription.try_recv().is_ok());
        }
        assert!(subscription.try_recv().is_err());
    }

    #[test]
    fn attach_time_snapshot_burst_is_not_limited_by_replayable_buffer_size() {
        let rm = RunManager::new();
        let session_id = SessionId::new();
        let mut subscription = rm.subscribe_session(session_id);
        let count = SSE_SUBSCRIBER_BUFFER + 17;

        for _ in 0..count {
            assert!(
                subscription.try_send(SseEventData::connected(RunId::new())),
                "synthetic attach-time snapshots must all be queued"
            );
        }
        for _ in 0..count {
            assert!(subscription.try_recv().is_ok());
        }
        assert!(subscription.try_recv().is_err());
    }
}
