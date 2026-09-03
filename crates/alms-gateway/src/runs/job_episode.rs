// SPDX-License-Identifier: Apache-2.0

//! Job-episode tracking for scheduled jobs (#1198).
//!
//! A **job episode** is the unit of work spanning a job firing's first turn
//! plus every piece of async work it triggers — DM conversations and
//! background subagents — plus every follow-up turn those trigger on the
//! job's own session. While an episode is open, the job's completion
//! notification, `record_run`, and recurring re-arm are all deferred; they
//! fire at **episode close** (see `notifications::close_episode`).
//!
//! An episode closes via:
//!
//! - **Quiescence**: a turn (run) belonging to the episode completes with an
//!   empty pending-work set and no other episode run queued or running.
//! - **Deadline** (D5): a hard per-episode deadline
//!   ([`EPISODE_DEADLINE_SECS`], 4 hours). Expiry is *detach-and-complete*:
//!   the job completes with a deadline note; still-live pending work is left
//!   running on its own lifecycle, never force-cancelled.
//!
//! Concurrency: all episode state lives behind ONE `parking_lot::Mutex` so
//! every compound operation (resolve = remove-pending + reserve-run;
//! quiescence check = decrement + inspect + remove) is atomic. Episodes are
//! low-frequency objects (one per firing job), so a global lock is free in
//! practice and eliminates the cross-map race classes a sharded design
//! would invite. No lock is held across `await` points — the tracker is
//! fully synchronous; async side effects happen on the returned values.
//!
//! See `docs/jobs-await-completion-design.md` for the full design.

use alms_core::{AgentId, JobId, RunId, SessionId, ToolCallRecord, ToolCallRole};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tracing::{debug, warn};
use uuid::Uuid;

/// Hard per-episode deadline: 4 hours (Alper's final call on #1198).
///
/// Threaded through [`JobEpisodeTracker::new`] so tests can shorten it; the
/// production value is wired in `AppState::new`.
pub(crate) const EPISODE_DEADLINE_SECS: u64 = 14_400;

/// How often the deadline sweep (`notifications::job_episode_sweep_loop`)
/// checks for expired episodes.
pub(crate) const EPISODE_SWEEP_INTERVAL_SECS: u64 = 60;

// ---------------------------------------------------------------------------
// Episode state
// ---------------------------------------------------------------------------

/// One open job episode. Returned by value when the episode closes
/// (quiescence / deadline / cancellation) so the caller can run the close
/// side effects without holding the tracker lock.
#[derive(Debug)]
pub(crate) struct JobEpisode {
    pub(crate) job_id: JobId,
    /// The `job_{id}` session every episode run executes on.
    pub(crate) session_id: SessionId,
    pub(crate) agent_id: AgentId,
    /// Wall-clock open time — the basis for the D6 catch-up cron math.
    pub(crate) started_at: DateTime<Utc>,
    /// Monotonic deadline (`opened + EPISODE_DEADLINE_SECS`).
    pub(crate) deadline: Instant,
    /// Open DM conversations started by episode runs, keyed by the
    /// deterministic DM session id from the `send_message` result.
    pub(crate) pending_dms: HashSet<SessionId>,
    /// Background subagents dispatched by episode runs:
    /// `task_id -> subagent session id` (the session id is kept for the
    /// cancellation teardown, which cancels by session).
    pub(crate) pending_subagents: HashMap<Uuid, SessionId>,
    /// Episode runs currently queued or running. Incremented when a run is
    /// reserved (open / resolve), decremented by `on_run_complete`.
    pub(crate) in_flight_runs: usize,
    /// Every run that belonged to the episode, in order (turn 1 first).
    pub(crate) runs: Vec<RunId>,
    /// Runs whose in-flight reservation has already been released.
    ///
    /// The domain panic guard may retry completion after `execute_run`
    /// reached its normal terminal tail. Tracking identities makes that
    /// retry idempotent instead of decrementing a concurrent continuation.
    pub(crate) finished_runs: HashSet<RunId>,
    /// Lifetime totals for the completion-card stats.
    pub(crate) dm_total: usize,
    pub(crate) subagent_total: usize,
    /// D6 defensive dirty-bit: a firing arrived while this episode was open
    /// (should be unreachable under re-arm-at-close, but any future
    /// scheduler change or a bootstrap re-fire is absorbed here instead of
    /// overlapping). Coalesced into the same single catch-up as elapsed
    /// cron ticks at close.
    pub(crate) catch_up_queued: bool,
}

impl JobEpisode {
    fn is_quiescent(&self) -> bool {
        self.pending_dms.is_empty() && self.pending_subagents.is_empty() && self.in_flight_runs == 0
    }

    /// Number of pending items still open (used for the deadline-close note).
    pub(crate) fn pending_count(&self) -> usize {
        self.pending_dms.len() + self.pending_subagents.len()
    }
}

/// Outcome of [`JobEpisodeTracker::on_run_complete`].
#[derive(Debug)]
pub(crate) enum RunCompletion {
    /// The episode reached quiescence and was removed — the caller must run
    /// the close side effects (`notifications::close_episode`).
    Closed(Box<JobEpisode>),
    /// The episode stays open (pending work and/or other in-flight runs).
    Open,
    /// No episode is tracked for this job (already closed by the deadline
    /// sweep or cancellation) — the run completes as a plain run.
    Untracked,
}

/// Routing target returned by the resolve calls: the continuation run for a
/// resolved pending item must execute on the episode's job session, stamped
/// with the episode's job id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContinuationRoute {
    pub(crate) job_id: JobId,
    pub(crate) job_session_id: SessionId,
    /// The job session's context id (`job_{id}`).
    pub(crate) context_id: String,
}

// ---------------------------------------------------------------------------
// Tracker
// ---------------------------------------------------------------------------

/// In-memory registry of open job episodes (one per firing job).
///
/// In-memory by design for phase 1: a daemon restart drops open episodes;
/// recurring jobs self-heal at their next tick. See the design doc's D5
/// persistence note for the phase-2 plan.
#[derive(Debug)]
pub(crate) struct JobEpisodeTracker {
    deadline: Duration,
    episodes: Mutex<HashMap<JobId, JobEpisode>>,
}

impl JobEpisodeTracker {
    pub(crate) fn new(deadline: Duration) -> Self {
        Self {
            deadline,
            episodes: Mutex::new(HashMap::new()),
        }
    }

    /// Fire-path guard (D6): if the job already has an open episode, absorb
    /// the firing into the coalesced catch-up dirty-bit and return `true`
    /// (the caller must NOT start a new turn). Returns `false` when no
    /// episode is open and the firing should proceed.
    pub(crate) fn absorb_fire_if_open(&self, job_id: JobId) -> bool {
        let mut eps = self.episodes.lock();
        if let Some(ep) = eps.get_mut(&job_id) {
            warn!(
                job_id = %job_id,
                "Job fired while its episode is still open — absorbed into \
                 the coalesced catch-up (D6 defensive guard)"
            );
            ep.catch_up_queued = true;
            true
        } else {
            false
        }
    }

    /// Open an episode for a firing job, with turn 1 reserved
    /// (`in_flight_runs = 1`).
    pub(crate) fn open(
        &self,
        job_id: JobId,
        session_id: SessionId,
        agent_id: AgentId,
        turn1_run: RunId,
    ) {
        let episode = JobEpisode {
            job_id,
            session_id,
            agent_id,
            started_at: Utc::now(),
            deadline: Instant::now() + self.deadline,
            pending_dms: HashSet::new(),
            pending_subagents: HashMap::new(),
            in_flight_runs: 1,
            runs: vec![turn1_run],
            finished_runs: HashSet::new(),
            dm_total: 0,
            subagent_total: 0,
            catch_up_queued: false,
        };
        let previous = self.episodes.lock().insert(job_id, episode);
        debug_assert!(
            previous.is_none(),
            "open() must not clobber a live episode — absorb_fire_if_open guards the fire path"
        );
    }

    /// Feed one episode-run exit: decrement the in-flight reservation, add
    /// any async work the run opened (scanned from its tool records by the
    /// caller), and decide quiescence. MUST be called from **every**
    /// `execute_run` exit for job-stamped runs — a missed exit stalls the
    /// episode until the deadline sweep.
    pub(crate) fn on_run_complete(
        &self,
        job_id: JobId,
        run_id: RunId,
        opened_dms: Vec<SessionId>,
        opened_subagents: Vec<(Uuid, SessionId)>,
    ) -> RunCompletion {
        let mut eps = self.episodes.lock();
        let Some(ep) = eps.get_mut(&job_id) else {
            debug!(
                job_id = %job_id,
                run_id = %run_id.0,
                "Run completed for an untracked episode (already closed by \
                 deadline sweep or cancellation) — completing as a plain run"
            );
            return RunCompletion::Untracked;
        };

        if !ep.finished_runs.insert(run_id) {
            debug!(
                job_id = %job_id,
                run_id = %run_id.0,
                "Ignoring duplicate episode run completion"
            );
            return RunCompletion::Open;
        }

        if ep.in_flight_runs == 0 {
            // Bookkeeping violation — every decrement must pair with a
            // reservation (open / resolve). Trace loudly; saturate at zero
            // so the episode can still close instead of underflowing.
            warn!(
                job_id = %job_id,
                run_id = %run_id.0,
                "Episode in_flight_runs underflow — run exit without a \
                 matching reservation (bookkeeping bug; see #1198 step 4)"
            );
        }
        ep.in_flight_runs = ep.in_flight_runs.saturating_sub(1);

        for dm in opened_dms {
            if ep.pending_dms.insert(dm) {
                ep.dm_total += 1;
            }
        }
        for (task_id, sub_session) in opened_subagents {
            if ep.pending_subagents.insert(task_id, sub_session).is_none() {
                ep.subagent_total += 1;
            }
        }

        if ep.is_quiescent() {
            let ep = eps.remove(&job_id).expect("entry exists — checked above");
            RunCompletion::Closed(Box::new(ep))
        } else {
            RunCompletion::Open
        }
    }

    /// Resolve a DM terminal signal (`ConversationEnded`) against open
    /// episodes. Hits only for episodes whose agent is `agent_id` and whose
    /// pending set contains `dm_session_id` — the same DM session appears
    /// in the triggers of BOTH participants, and only the job agent's
    /// trigger may consume the pending entries (the peer's continuation
    /// belongs to the peer, not to the job).
    ///
    /// Resolves **every** matching episode, not just one (#1205): DM
    /// sessions are deterministic per agent pair, so two open episodes of
    /// the same agent can both be pending on the same DM session (two jobs
    /// each messaged the same peer). A single `ConversationEnded` means the
    /// conversation on that session is over for ALL of them — resolving
    /// only the HashMap-order winner would strand the loser's pending entry
    /// until the 4h deadline backstop. The caller enqueues one continuation
    /// run per returned route.
    ///
    /// On each hit the pending entry is removed and a continuation run is
    /// **reserved** (`in_flight_runs += 1`) in the same atomic step, so a
    /// concurrent `on_run_complete` can never observe "pending empty, no
    /// in-flight" in the gap before the continuation run is enqueued.
    /// `note_run` fires inside `enqueue_triggered_run` as soon as each run
    /// id exists (#1207).
    pub(crate) fn resolve_dm(
        &self,
        dm_session_id: SessionId,
        agent_id: AgentId,
    ) -> Vec<ContinuationRoute> {
        let mut eps = self.episodes.lock();
        eps.values_mut()
            .filter(|ep| ep.agent_id == agent_id && ep.pending_dms.contains(&dm_session_id))
            .map(|ep| {
                ep.pending_dms.remove(&dm_session_id);
                ep.in_flight_runs += 1;
                ContinuationRoute {
                    job_id: ep.job_id,
                    job_session_id: ep.session_id,
                    context_id: format!("job_{}", ep.job_id.0),
                }
            })
            .collect()
    }

    /// Resolve a background-subagent terminal signal (`SubagentCompletion`)
    /// by task id. Task ids are globally unique, so no agent check is
    /// needed. Same atomic remove-and-reserve contract as [`Self::resolve_dm`].
    pub(crate) fn resolve_subagent(&self, task_id: Uuid) -> Option<ContinuationRoute> {
        let mut eps = self.episodes.lock();
        let ep = eps
            .values_mut()
            .find(|ep| ep.pending_subagents.contains_key(&task_id))?;
        ep.pending_subagents.remove(&task_id);
        ep.in_flight_runs += 1;
        Some(ContinuationRoute {
            job_id: ep.job_id,
            job_session_id: ep.session_id,
            context_id: format!("job_{}", ep.job_id.0),
        })
    }

    /// Release the in-flight reservation created by `resolve_dm` or
    /// `resolve_subagent` when admission fails before a continuation run is
    /// durably registered. No run id is recorded because no run existed.
    pub(crate) fn abandon_reserved_continuation(&self, job_id: JobId) -> RunCompletion {
        let mut eps = self.episodes.lock();
        let Some(ep) = eps.get_mut(&job_id) else {
            return RunCompletion::Untracked;
        };

        if ep.in_flight_runs == 0 {
            warn!(
                job_id = %job_id,
                "Episode continuation reservation underflow while abandoning admission"
            );
        }
        ep.in_flight_runs = ep.in_flight_runs.saturating_sub(1);

        if ep.is_quiescent() {
            let ep = eps
                .remove(&job_id)
                .expect("entry exists after mutable lookup");
            RunCompletion::Closed(Box::new(ep))
        } else {
            RunCompletion::Open
        }
    }

    /// Record a continuation run's id (the reservation was already taken by
    /// the resolve call). Called by `enqueue_triggered_run` immediately
    /// after the run is inserted — BEFORE the run is enqueued for execution
    /// (#1207) — so an instantly-terminating continuation can never close
    /// the episode with its own id missing from `runs`.
    pub(crate) fn note_run(&self, job_id: JobId, run_id: RunId) {
        if let Some(ep) = self.episodes.lock().get_mut(&job_id) {
            ep.runs.push(run_id);
        }
    }

    /// Remove the episode (cancellation teardown). The caller owns the
    /// returned pending work.
    pub(crate) fn remove(&self, job_id: JobId) -> Option<JobEpisode> {
        self.episodes.lock().remove(&job_id)
    }

    /// Drain every episode whose deadline has passed (the D5 sweep).
    pub(crate) fn take_expired(&self) -> Vec<JobEpisode> {
        let now = Instant::now();
        let mut eps = self.episodes.lock();
        let expired: Vec<JobId> = eps
            .iter()
            .filter(|(_, ep)| ep.deadline <= now)
            .map(|(id, _)| *id)
            .collect();
        expired
            .into_iter()
            .filter_map(|id| eps.remove(&id))
            .collect()
    }

    /// Observability snapshot for `GET /jobs` — `None` when no episode is
    /// open for the job.
    pub(crate) fn snapshot(&self, job_id: JobId) -> Option<serde_json::Value> {
        let eps = self.episodes.lock();
        let ep = eps.get(&job_id)?;
        Some(serde_json::json!({
            "started_at": ep.started_at.to_rfc3339(),
            "pending_dms": ep.pending_dms.len(),
            "pending_subagents": ep.pending_subagents.len(),
            "in_flight_runs": ep.in_flight_runs,
            "runs": ep.runs.len(),
            "catch_up_queued": ep.catch_up_queued,
            "deadline_remaining_secs":
                ep.deadline.saturating_duration_since(Instant::now()).as_secs(),
        }))
    }
}

// ---------------------------------------------------------------------------
// Pending-work detection (step 2)
// ---------------------------------------------------------------------------

/// Parse a Tool-role record's `result` JSON, returning `None` for non-JSON
/// content (e.g. `"Error: ..."` failure strings, truncated blobs).
fn result_json(record: &ToolCallRecord) -> Option<serde_json::Value> {
    let raw = record.result.as_deref()?;
    serde_json::from_str(raw).ok()
}

/// DM sessions opened by this run: Tool-role `send_message` records whose
/// result reports a successful delivery (`delivered: true` +
/// `dm_session_id`). Folded sends (`folded: true`, the #1154 implicit-reply
/// fold) report `delivered: false` and error results have no
/// `dm_session_id`, so both are excluded naturally.
pub(crate) fn dms_opened(records: &[ToolCallRecord]) -> Vec<SessionId> {
    records
        .iter()
        .filter(|r| r.role == ToolCallRole::Tool && r.tool_name.as_deref() == Some("send_message"))
        .filter_map(|r| {
            let json = result_json(r)?;
            if json.get("delivered").and_then(|v| v.as_bool()) != Some(true) {
                return None;
            }
            let sid = json.get("dm_session_id")?.as_str()?;
            Uuid::parse_str(sid).ok().map(SessionId)
        })
        .collect()
}

/// Background subagents dispatched by this run: Tool-role `invoke_agent`
/// records whose result carries `task_id` + `session_id` (the
/// background-dispatch result shape — foreground results carry `response`
/// instead, and error results carry `error`).
pub(crate) fn subagents_spawned(records: &[ToolCallRecord]) -> Vec<(Uuid, SessionId)> {
    records
        .iter()
        .filter(|r| r.role == ToolCallRole::Tool && r.tool_name.as_deref() == Some("invoke_agent"))
        .filter_map(|r| {
            let json = result_json(r)?;
            if json.get("error").is_some() {
                return None;
            }
            let task = Uuid::parse_str(json.get("task_id")?.as_str()?).ok()?;
            let session = Uuid::parse_str(json.get("session_id")?.as_str()?).ok()?;
            Some((task, SessionId(session)))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> JobEpisodeTracker {
        JobEpisodeTracker::new(Duration::from_secs(EPISODE_DEADLINE_SECS))
    }

    fn open_episode(t: &JobEpisodeTracker) -> (JobId, SessionId, AgentId, RunId) {
        let job_id = JobId::new();
        let session_id = SessionId::new();
        let agent_id = AgentId::new();
        let run_id = RunId::new();
        t.open(job_id, session_id, agent_id, run_id);
        (job_id, session_id, agent_id, run_id)
    }

    fn tool_result(name: &str, result: serde_json::Value) -> ToolCallRecord {
        ToolCallRecord {
            seq: 0,
            role: ToolCallRole::Tool,
            tool_name: Some(name.to_string()),
            tool_id: Some("call_1".to_string()),
            tool_invocation_id: None,
            params: None,
            result: Some(result.to_string()),
            timestamp: Utc::now(),
            from_agent: None,
        }
    }

    // -- quiescence ---------------------------------------------------------

    /// The regression floor: a job whose turn opens no async work closes at
    /// turn end — behavior identical to today.
    #[test]
    fn no_async_work_closes_at_turn_end() {
        let t = tracker();
        let (job_id, _, _, run_id) = open_episode(&t);
        match t.on_run_complete(job_id, run_id, vec![], vec![]) {
            RunCompletion::Closed(ep) => {
                assert_eq!(ep.runs, vec![run_id]);
                assert_eq!(ep.dm_total, 0);
                assert_eq!(ep.subagent_total, 0);
            }
            other => panic!("expected Closed, got {other:?}"),
        }
        assert!(t.snapshot(job_id).is_none(), "episode must be removed");
    }

    /// A turn that opened a DM keeps the episode open; the DM's resolution
    /// reserves a continuation; the quiescent continuation closes.
    #[test]
    fn single_dm_arc_closes_after_quiescent_continuation() {
        let t = tracker();
        let (job_id, job_session, agent_id, run1) = open_episode(&t);
        let dm = SessionId::new();

        match t.on_run_complete(job_id, run1, vec![dm], vec![]) {
            RunCompletion::Open => {}
            other => panic!("pending DM must keep the episode open, got {other:?}"),
        }

        // The DM reaches its terminal state — resolve + reserve.
        let routes = t.resolve_dm(dm, agent_id);
        assert_eq!(
            routes.len(),
            1,
            "pending DM must resolve for the episode agent"
        );
        assert_eq!(routes[0].job_id, job_id);
        assert_eq!(routes[0].job_session_id, job_session);
        assert_eq!(routes[0].context_id, format!("job_{}", job_id.0));

        // Between resolve and the continuation run there is a reservation —
        // no close can fire (nothing completes in the gap by construction;
        // this asserts the reservation exists).
        let run2 = RunId::new();
        t.note_run(job_id, run2);

        match t.on_run_complete(job_id, run2, vec![], vec![]) {
            RunCompletion::Closed(ep) => {
                assert_eq!(ep.runs, vec![run1, run2]);
                assert_eq!(ep.dm_total, 1);
            }
            other => panic!("expected Closed, got {other:?}"),
        }
    }

    /// Wait-on-ALL: two DMs + one subagent must all resolve (each followed
    /// by a quiescent-or-not continuation) before the episode closes.
    #[test]
    fn parallel_pending_items_wait_on_all() {
        let t = tracker();
        let (job_id, _, agent_id, run1) = open_episode(&t);
        let dm_a = SessionId::new();
        let dm_b = SessionId::new();
        let task = Uuid::new_v4();

        assert!(matches!(
            t.on_run_complete(
                job_id,
                run1,
                vec![dm_a, dm_b],
                vec![(task, SessionId::new())]
            ),
            RunCompletion::Open
        ));

        // dm_a resolves; its continuation completes → still open (dm_b + task).
        assert_eq!(t.resolve_dm(dm_a, agent_id).len(), 1, "dm_a resolves");
        let c1 = RunId::new();
        t.note_run(job_id, c1);
        assert!(matches!(
            t.on_run_complete(job_id, c1, vec![], vec![]),
            RunCompletion::Open
        ));

        // task resolves; its continuation completes → still open (dm_b).
        t.resolve_subagent(task).expect("task resolves");
        let c2 = RunId::new();
        t.note_run(job_id, c2);
        assert!(matches!(
            t.on_run_complete(job_id, c2, vec![], vec![]),
            RunCompletion::Open
        ));

        // dm_b resolves; quiescent continuation → closed.
        assert_eq!(t.resolve_dm(dm_b, agent_id).len(), 1, "dm_b resolves");
        let c3 = RunId::new();
        t.note_run(job_id, c3);
        match t.on_run_complete(job_id, c3, vec![], vec![]) {
            RunCompletion::Closed(ep) => {
                assert_eq!(ep.dm_total, 2);
                assert_eq!(ep.subagent_total, 1);
                assert_eq!(ep.runs.len(), 4);
            }
            other => panic!("expected Closed, got {other:?}"),
        }
    }

    /// A continuation turn can open NEW work — the pending set regrows and
    /// the episode stays open.
    #[test]
    fn continuation_turn_can_regrow_pending_set() {
        let t = tracker();
        let (job_id, _, agent_id, run1) = open_episode(&t);
        let dm_a = SessionId::new();
        assert!(matches!(
            t.on_run_complete(job_id, run1, vec![dm_a], vec![]),
            RunCompletion::Open
        ));

        assert_eq!(t.resolve_dm(dm_a, agent_id).len(), 1);
        let c1 = RunId::new();
        t.note_run(job_id, c1);
        // The continuation starts a NEW DM.
        let dm_b = SessionId::new();
        assert!(matches!(
            t.on_run_complete(job_id, c1, vec![dm_b], vec![]),
            RunCompletion::Open
        ));

        assert_eq!(t.resolve_dm(dm_b, agent_id).len(), 1);
        let c2 = RunId::new();
        t.note_run(job_id, c2);
        assert!(matches!(
            t.on_run_complete(job_id, c2, vec![], vec![]),
            RunCompletion::Closed(_)
        ));
    }

    /// A panic caught after the normal terminal tail may call completion a
    /// second time for the same run. It must not release the reservation
    /// held by a concurrently queued continuation.
    #[test]
    fn duplicate_completion_does_not_consume_continuation_reservation() {
        let t = tracker();
        let (job_id, _, agent_id, run1) = open_episode(&t);
        let dm = SessionId::new();

        assert!(matches!(
            t.on_run_complete(job_id, run1, vec![dm], vec![]),
            RunCompletion::Open
        ));
        assert_eq!(t.resolve_dm(dm, agent_id).len(), 1);

        let continuation = RunId::new();
        t.note_run(job_id, continuation);
        assert!(matches!(
            t.on_run_complete(job_id, run1, vec![], vec![]),
            RunCompletion::Open
        ));
        assert_eq!(
            t.snapshot(job_id).unwrap()["in_flight_runs"],
            1,
            "duplicate completion must leave the continuation reservation intact"
        );

        assert!(matches!(
            t.on_run_complete(job_id, continuation, vec![], vec![]),
            RunCompletion::Closed(_)
        ));
    }

    // -- resolve correctness --------------------------------------------------

    /// The same DM session appears in BOTH participants' ConversationEnded
    /// triggers; only the episode agent's trigger may consume the pending
    /// entry.
    #[test]
    fn resolve_dm_requires_matching_agent() {
        let t = tracker();
        let (job_id, _, agent_id, run1) = open_episode(&t);
        let dm = SessionId::new();
        assert!(matches!(
            t.on_run_complete(job_id, run1, vec![dm], vec![]),
            RunCompletion::Open
        ));

        // The PEER's trigger (different agent id) must miss.
        assert!(
            t.resolve_dm(dm, AgentId::new()).is_empty(),
            "peer's trigger must not consume the episode's pending DM"
        );
        // The job agent's trigger hits.
        assert_eq!(t.resolve_dm(dm, agent_id).len(), 1);
        // Second resolution (duplicate end) misses — entry consumed.
        assert!(t.resolve_dm(dm, agent_id).is_empty());
    }

    /// Repeat sends to the same DM pair are idempotent (set semantics): one
    /// pending entry, one dm_total increment.
    #[test]
    fn repeat_sends_to_same_pair_collapse_to_one_pending() {
        let t = tracker();
        let (job_id, _, agent_id, run1) = open_episode(&t);
        let dm = SessionId::new();
        assert!(matches!(
            t.on_run_complete(job_id, run1, vec![dm, dm], vec![]),
            RunCompletion::Open
        ));
        assert_eq!(t.resolve_dm(dm, agent_id).len(), 1, "one entry to resolve");
        let c1 = RunId::new();
        t.note_run(job_id, c1);
        match t.on_run_complete(job_id, c1, vec![], vec![]) {
            RunCompletion::Closed(ep) => assert_eq!(ep.dm_total, 1),
            other => panic!("expected Closed, got {other:?}"),
        }
    }

    /// #1205 (Tim's S2 on PR #1202): two open episodes of the SAME agent
    /// both pending the SAME deterministic DM session — a single
    /// `ConversationEnded` must resolve BOTH (the conversation on that
    /// session is over for both jobs). Pre-fix only the HashMap-order
    /// winner resolved and the loser hung until the 4h deadline.
    #[test]
    fn resolve_dm_resolves_all_episodes_pending_on_same_session() {
        let t = tracker();
        let agent_id = AgentId::new();
        let dm = SessionId::new();

        // Two jobs for the same agent, each with an open episode pending
        // on the same DM session (both messaged the same peer).
        let mut jobs = Vec::new();
        for _ in 0..2 {
            let job_id = JobId::new();
            let session_id = SessionId::new();
            let run1 = RunId::new();
            t.open(job_id, session_id, agent_id, run1);
            assert!(matches!(
                t.on_run_complete(job_id, run1, vec![dm], vec![]),
                RunCompletion::Open
            ));
            jobs.push((job_id, session_id));
        }

        // One ConversationEnded resolves BOTH episodes, each with its own
        // route (own job session, own job id).
        let mut routes = t.resolve_dm(dm, agent_id);
        assert_eq!(
            routes.len(),
            2,
            "both episodes pending on the DM session must resolve"
        );
        routes.sort_by_key(|r| r.job_id.0);
        let mut expected: Vec<ContinuationRoute> = jobs
            .iter()
            .map(|(job_id, session_id)| ContinuationRoute {
                job_id: *job_id,
                job_session_id: *session_id,
                context_id: format!("job_{}", job_id.0),
            })
            .collect();
        expected.sort_by_key(|r| r.job_id.0);
        assert_eq!(routes, expected);

        // Duplicate end: both entries consumed.
        assert!(t.resolve_dm(dm, agent_id).is_empty());

        // Each episode's reserved continuation closes its own episode —
        // neither hangs to the deadline.
        for (job_id, _) in jobs {
            let cont = RunId::new();
            t.note_run(job_id, cont);
            match t.on_run_complete(job_id, cont, vec![], vec![]) {
                RunCompletion::Closed(ep) => {
                    assert_eq!(ep.job_id, job_id);
                    assert!(
                        ep.runs.contains(&cont),
                        "continuation run must be recorded on its episode"
                    );
                }
                other => panic!("episode for {job_id:?} must close, got {other:?}"),
            }
        }
    }

    /// Resolutions against a closed/unknown episode miss gracefully (the
    /// detach-and-complete orphan path).
    #[test]
    fn resolve_after_close_misses() {
        let t = tracker();
        let (job_id, _, agent_id, run1) = open_episode(&t);
        let dm = SessionId::new();
        t.on_run_complete(job_id, run1, vec![dm], vec![]);
        t.remove(job_id).expect("episode removed (cancel path)");
        assert!(t.resolve_dm(dm, agent_id).is_empty());
        assert!(t.resolve_subagent(Uuid::new_v4()).is_none());
    }

    #[test]
    fn abandoned_continuation_reservation_allows_episode_to_close() {
        let t = tracker();
        let (job_id, _, _, run1) = open_episode(&t);
        let task_id = Uuid::new_v4();
        assert!(matches!(
            t.on_run_complete(job_id, run1, vec![], vec![(task_id, SessionId::new())]),
            RunCompletion::Open
        ));
        assert!(t.resolve_subagent(task_id).is_some());

        match t.abandon_reserved_continuation(job_id) {
            RunCompletion::Closed(episode) => {
                assert_eq!(episode.job_id, job_id);
                assert_eq!(episode.runs, vec![run1]);
            }
            other => panic!("abandoned final reservation must close episode, got {other:?}"),
        }
        assert!(t.snapshot(job_id).is_none());
    }

    /// A run exit for an untracked job (episode already closed by the sweep)
    /// reports Untracked and does not panic/underflow.
    #[test]
    fn run_complete_on_untracked_job_is_untracked() {
        let t = tracker();
        assert!(matches!(
            t.on_run_complete(JobId::new(), RunId::new(), vec![], vec![]),
            RunCompletion::Untracked
        ));
    }

    // -- deadline (D5) --------------------------------------------------------

    /// An episode past its deadline is drained by the sweep — with its
    /// pending work intact (detach-and-complete: the caller completes the
    /// job and leaves the pending items alone).
    #[test]
    fn take_expired_drains_past_deadline_episodes_with_pending_work() {
        let t = JobEpisodeTracker::new(Duration::ZERO); // everything expires immediately
        let (job_id, _, _, run1) = open_episode(&t);
        let dm = SessionId::new();
        assert!(matches!(
            t.on_run_complete(job_id, run1, vec![dm], vec![]),
            RunCompletion::Open
        ));

        let expired = t.take_expired();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].job_id, job_id);
        assert_eq!(
            expired[0].pending_count(),
            1,
            "pending work is detached, not cancelled"
        );
        assert!(t.snapshot(job_id).is_none(), "episode removed by sweep");
    }

    /// An episode within its deadline is NOT swept.
    #[test]
    fn take_expired_leaves_live_episodes() {
        let t = tracker(); // 4h deadline
        let (job_id, _, _, run1) = open_episode(&t);
        let dm = SessionId::new();
        t.on_run_complete(job_id, run1, vec![dm], vec![]);
        assert!(t.take_expired().is_empty());
        assert!(t.snapshot(job_id).is_some());
    }

    // -- fire-path guard (D6) --------------------------------------------------

    #[test]
    fn absorb_fire_if_open_sets_catch_up_bit() {
        let t = tracker();
        let (job_id, _, _, run1) = open_episode(&t);
        let dm = SessionId::new();
        t.on_run_complete(job_id, run1, vec![dm], vec![]);

        assert!(t.absorb_fire_if_open(job_id), "open episode absorbs");
        let snap = t.snapshot(job_id).unwrap();
        assert_eq!(snap["catch_up_queued"], true);

        assert!(
            !t.absorb_fire_if_open(JobId::new()),
            "no episode → fire proceeds"
        );
    }

    // -- detection helpers (step 2) ---------------------------------------------

    #[test]
    fn dms_opened_accepts_only_delivered_sends() {
        let dm_id = Uuid::new_v4();
        let records = vec![
            // Delivered send → counted.
            tool_result(
                "send_message",
                serde_json::json!({"delivered": true, "dm_session_id": dm_id.to_string(), "note": "ok"}),
            ),
            // Folded send (#1154 implicit-reply fold) → excluded.
            tool_result(
                "send_message",
                serde_json::json!({"delivered": false, "folded": true, "note": "already in DM"}),
            ),
            // Error result → excluded.
            tool_result(
                "send_message",
                serde_json::json!({"error": "Agent 'zed' not found."}),
            ),
            // Non-JSON result (sandbox failure string) → excluded, no panic.
            ToolCallRecord {
                result: Some("Error: delivery failed".to_string()),
                ..tool_result("send_message", serde_json::json!({}))
            },
            // Different tool → excluded.
            tool_result("echo", serde_json::json!({"delivered": true})),
        ];
        assert_eq!(dms_opened(&records), vec![SessionId(dm_id)]);
    }

    #[test]
    fn subagents_spawned_accepts_only_background_dispatches() {
        let task = Uuid::new_v4();
        let session = Uuid::new_v4();
        let records = vec![
            // Background dispatch → counted.
            tool_result(
                "invoke_agent",
                serde_json::json!({"task_id": task.to_string(), "session_id": session.to_string()}),
            ),
            // Foreground result (response, no task_id) → excluded.
            tool_result(
                "invoke_agent",
                serde_json::json!({"response": "here is the answer", "session_id": session.to_string()}),
            ),
            // Error result → excluded.
            tool_result(
                "invoke_agent",
                serde_json::json!({"error": "no such agent", "task_id": task.to_string(), "session_id": session.to_string()}),
            ),
        ];
        assert_eq!(
            subagents_spawned(&records),
            vec![(task, SessionId(session))]
        );
    }

    /// Assistant-role records (the call side) must not be counted — only
    /// Tool-role results prove the call executed.
    #[test]
    fn call_side_records_are_ignored() {
        let record = ToolCallRecord {
            role: ToolCallRole::Assistant,
            result: Some(
                serde_json::json!({"delivered": true, "dm_session_id": Uuid::new_v4().to_string()})
                    .to_string(),
            ),
            ..tool_result("send_message", serde_json::json!({}))
        };
        assert!(dms_opened(&[record]).is_empty());
    }
}
