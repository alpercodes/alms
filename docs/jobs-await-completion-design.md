# Scheduled Jobs — Await Full Completion (Design)

**Status:** APPROVED — final decisions by Alper (2026-07-06): JobEpisode model, per-turn runs, structural quiescence, job-as-DM-source, **4-hour episode hard deadline with detach-and-complete expiry**, queue-on-overlap coalesced to one catch-up, subagents in phase 1. Phase 1 ships together with this doc in PR #1202 (issue #1198, under the #763 cron-scheduler audit umbrella).
**Author:** Heph, 2026-07-06.
**Verified against:** `develop` @ `c4c313b`.

## Problem Statement

A scheduled job's lifetime today is **exactly one agent turn**. The run flips `Completed` the moment the agent's own loop returns — but `send_message` is fire-and-forget by contract, so any DM the job agent started keeps going on the shared `dm:{a}:{b}` session long after the job has reported success. The user sees:

> "Scheduled job completed" — while the actual task (ask a peer, get an answer, act on it) has barely started.

Alper's decision: **the job should stay active until the agent's full multi-step task is truly done.** The job agent messages a peer → the DM resolves → the agent resumes for more tool rounds → it may start additional DM(s) → and only when it has genuinely no more work does the job complete. This is a run-lifecycle change, not a cosmetic one.

## Current Behavior — Precise Trace

All file/line references verified against `develop` @ `c4c313b`.

### 1. Job firing and completion are one `await`

`scheduler_fire_loop` (`crates/alms-gateway/src/runs/notifications.rs:24-44`) receives fired `JobId`s and enqueues `fire_job_run` on the per-agent `SessionQueue` (`state.agent_queue.enqueue(job.agent_id, …)`).

`fire_job_run` (`notifications.rs:48-147`) then does, in order:

1. Resolves the job's stable session: `context_id = "job_{job_id}"` → `get_or_create` (`:60-64`). Every firing of the same job accumulates history on this one session.
2. Creates `Run::for_job(...)` (carries `job_id`, `crates/alms-core/src/run.rs:329-334`) and **awaits `execute_run` inline** (`notifications.rs:87-101`), with `is_system_triggered: true`, `is_peer_message: false`.
3. When `execute_run` returns — i.e. when the agent loop's single turn ends — the post-run block fires *immediately and unconditionally*:
   - `notify_job_completion` (`:107`) — emits the `job_completed` SSE card + persists the `[Scheduled job completed]` marker on the agent's most recent user-facing session.
   - `job_store.record_run` (`:134-136`) — `Once` jobs flip to `Cancelled` (spent), `Recurring` jobs stay `Active`.
   - Recurring re-arm (`:139-144`) — `scheduler.schedule_once(job_id, next_cron_tick)`.

Inside `execute_run`, the run flips terminal at `mark_run_as_completed` (`crates/alms-gateway/src/runs/lifecycle.rs:2524-2527`) as soon as the agent loop returns `Ok`, followed by the `run_finished` SSE broadcast (`:2529-2538`). There is no notion of "outstanding work" anywhere in this path.

### 2. `send_message` is fire-and-forget — and so is background `invoke_agent`

`SendMessageTool` (`crates/alms-tools/src/send_message.rs:1-18`) is documented and implemented as fire-and-forget: it calls `MessageSender::send()` (the `MessageBus`), which persists the message to the shared DM session and emits a `RunTrigger` for the recipient, then returns `{"delivered": true, "dm_session_id": …}` to the calling agent's loop (`:183-199`). The sender's turn continues; nothing ever blocks on a reply.

So a job whose prompt says "ask Tim to review X and report back" performs one turn: call `send_message`, get `delivered: true`, emit some final text, loop ends → job Completed.

The identical structure applies to **background subagents**: `invoke_agent(background=true)` returns `{"task_id": …, "session_id": …}` immediately (`crates/alms-tools/src/invoke_agent.rs:170-196` → `dispatch_background`, `crates/alms-coordinator/src/lib.rs:856-891`), and the subagent's completion arrives later as a `SubagentCompletion` consumed by `completion_notification_loop` (`notifications.rs:430-592`) — which creates the notification run on the parent session. For a job run, the parent session *is* the job session, so that notification run already lands in the right place — but only after the job has long since reported Completed. Same structural bug, second async work type.

### 3. The conversation continues *outside* the job

`MessageBus::send` (`crates/alms-coordinator/src/message_bus/bus.rs:134-393`) writes to the deterministic shared DM session (`SessionId::deterministic_dm`, context `dm:{a}:{b}`) and pushes a `RunTrigger` consumed by `run_trigger_loop` (`crates/alms-gateway/src/runs/notifications.rs:876-1100`). The peer's run — and every subsequent turn of the back-and-forth, including the job agent's own replies under implicit DM replies (#1154) — executes on the **DM session**, via `enqueue_triggered_run` (`:599-669`). The job session only ever holds the first turn. The job record, the job run, and the job card are never touched again.

### 4. The DM eventually ends — but the end never reaches the job

Post-#1154, every peer-triggered DM run exits as exactly one of **delivered | ended | errored** (`crates/alms-gateway/src/runs/dm_lifecycle.rs:102-148`), so every conversation reaches a terminal `ConversationEnded` trigger (ignore, depth-exceeded at `MAX_DM_DEPTH = 20`, error, or cancel — `message_bus/mod.rs:39`). `MessageBus::end_conversation` (`bus.rs:415-636`) then routes triggers using the `source_sessions` map:

- The **peer** always gets a `ConversationEnded` trigger — routed to its recorded source session, else to the invisible `notifications:{agent}` session (`bus.rs:536-556`).
- The **sender** gets a self-notification trigger **only if** it has a recorded source session (`bus.rs:597-628`).

And here is the wrinkle: **a job session is explicitly rejected as a DM source** (`bus.rs:274-299`). The validity check rejects session types `"notification" | "subagent" | "episodic" | "job"` (via `classify_session_type`, `crates/alms-core/src/lib.rs:48-64`; `job_*` → `"job"`). Consequences for a job-originated DM:

- No `source_sessions` entry is ever recorded for the job agent.
- When the peer ends the conversation, the job agent's `ConversationEnded` trigger falls back to `notifications:{agent}` — the DM-ended notification run (with the embedded transcript, `notifications.rs:1027-1071`) executes on an **invisible session with none of the job's context**.
- If the *job agent itself* ends the DM (`ignore_message` during a DM turn), the sender self-notification is **skipped entirely** (no source session) — the agent never gets a resume turn at all.
- `notify_dm_ended_to_webchat` (`notifications.rs:281-344`) separately drops a lightweight "[DM conversation ended]" marker on the user-facing web session — visible to the human, but disconnected from the job card, which said "completed" long ago.

So the "arc" does continue today — but headless: on the wrong session, without job context, untracked by the job record, and invisible to the job card.

### Sequence diagram — today

```
Scheduler   fire_job_run          Agent A turn (job_X session)     MessageBus            Agent B
    |--fire--->|
    |          |--execute_run------->| LLM + tools
    |          |                     |--send_message(B,"review X")--->| persist to dm:a:b
    |          |                     |<--{delivered:true} immediately | trigger------------->| DM run
    |          |                     | final text, loop returns       |
    |          |<---Ok(output)-------|                                |
    |          | run -> Completed                                     |
    |          | "Scheduled job completed" card + record_run + re-arm |    <== JOB IS DONE HERE
    |                                                                 |
    |               ...conversation continues on dm:a:b, job not involved...
    |                    B implicit-replies -> trigger A (DM run on dm:a:b)
    |                    A replies ... eventually B ignore_message
    |                                            end_conversation:
    |                                              A's source lookup: job session REJECTED (bus.rs:274-299)
    |                                              -> A's dm-ended run lands on notifications:{A} (invisible)
    |                    A "acts on the outcome" on a context-less hidden session; job card never updated
```

### Sequence diagram — target

```
Scheduler   fire_job_run + episode        Agent A (job_X)          MessageBus            Agent B
    |--fire--->| episode OPEN
    |          |--turn-1 run---------------->| send_message(B) ------->| pending += Dm(dm:a:b) ->| DM run
    |          |                             | invoke_agent(bg) ...........pending += Subagent(task)
    |          |<--run Completed (turn ends) |
    |          | quiescence check: pending nonempty -> episode stays OPEN (no card, no record_run, no re-arm)
    |                       ...DM proceeds on dm:a:b (A <-> B turns, per #1154 contract)...
    |                       terminal: ignore / depth / error / cancel -> ConversationEnded
    |          | resolve Dm(dm:a:b) -> continuation run ON job_X (dm-ended notification + transcript)
    |          |--continuation run----------->| more tool rounds; may send_message(C) -> pending += Dm(dm:a:c)
    |          |<--run Completed; quiescence check again
    |          | (SubagentCompletion resolves Subagent(task) the same way -> continuation run on job_X)
    |          | ...until a turn ends with pending empty and no in-flight episode runs...
    |          | episode CLOSED -> "Scheduled job completed" card + record_run
    |          |   Recurring: if a cron tick elapsed during the episode -> fire ONE coalesced
    |          |   catch-up episode immediately; else re-arm for the next future tick
```

## Target Behavior — "The Job Owns the Whole Arc"

Concretely:

1. A job firing opens a **job episode**: the unit of work spanning turn 1 plus every piece of async work it triggers — DMs *and* background subagents — plus every follow-up turn those trigger, transitively on the job's own session.
2. While the episode is open: no completion card, no `record_run`, no recurring re-arm. The job is observably "in progress" (job session visible in the Jobs sidebar group from #1197; DM activity visible via the existing `dm_activity_*` events; episode state surfaced on `GET /jobs`).
3. When a pending item reaches its terminal state (DM `ConversationEnded`; `SubagentCompletion`), the agent is resumed **on the job session** — with the transcript/summary and its full job context — for more tool rounds. Those rounds may start further DMs/subagents, which the episode also awaits.
4. The episode closes via **quiescence** (a turn ends with an empty pending set and no queued/running episode runs) or, as a backstop, via the **4-hour hard deadline** — expiry is *detach-and-complete*: the job completes with a deadline note, and any still-live DM/subagent is left running on its own lifecycle (never force-cancelled). See D5.
5. Cancelling the job mid-episode tears down everything: queued/running episode runs, in-flight DM(s), and running background subagents.

## Design Decisions

Alper reviewed the option analysis (PR #1202, first revision) and made four calls. Each subsection states the decision, the surviving design, and what the decision newly requires. The rejected alternatives and their trade-off tables are preserved in the PR's first-revision history.

### D1. "Fully done" = structural quiescence (APPROVED)

The episode closes when a turn (run) belonging to the episode completes AND the episode's pending-work set is empty AND no episode continuation run is queued or running. No explicit "done" tool, no idle timer. Detection lives in the gateway's episode tracker; the check runs at every episode-run completion.

"Pending work" covers **both** async work types uniformly (Alper's call: subagents are phase 1, not phase 2):

```rust
enum PendingWork {
    /// An open DM conversation started by an episode run.
    /// Key: the deterministic DM session id from the send_message result.
    Dm(SessionId),
    /// A background subagent dispatched by an episode run.
    /// Key: the task id from the invoke_agent(background=true) result.
    Subagent(TaskId),
}
```

Both are detected the same way: by scanning the completed run's `ToolCallRecord`s (the pattern `should_signal_dm_end` / `alms_core::ran_ignore_message_successfully` already uses, `dm_lifecycle.rs:648-656`):

- successful `send_message` → result JSON `delivered: true` + `dm_session_id` (`send_message.rs:195-199`) → `PendingWork::Dm`;
- successful background `invoke_agent` → result JSON `task_id` + `session_id`, no `error` (`invoke_agent.rs:196`) → `PendingWork::Subagent`.

Foreground `invoke_agent` blocks inside the turn and needs no tracking. Folded sends (`folded: true`), failed sends, and errored dispatches open nothing.

### D2. Run model: per-turn runs + `JobEpisode` (APPROVED)

Every turn completes as a normal run (turn-1 run, then continuation runs); the run state machine, SSE contract, per-agent queue, and cancel paths are untouched. A `JobEpisode` — keyed by `JobId`, held in-memory on `AppState` — spans them and owns the deferred completion block (card + `record_run` + re-arm/catch-up), relocated verbatim from `fire_job_run:107-144` into `close_episode`.

(The rejected long-lived-`Running`-run alternative breaks the per-agent queue slot, shutdown drain, per-turn usage accounting, and the #1052 terminal-transition gating; the tracker-less chained-runs alternative has no decision-maker for "done". See PR #1202 first revision.)

### D3. Job sessions become legitimate DM sources (APPROVED)

Remove `"job"` from the source-session reject list in `bus.rs:274-299` (rejection stays for `"notification" | "subagent" | "episodic"` and the same-DM guard). This guarantees a `ConversationEnded` trigger **exists** on every terminal path — critically including the case where the *job agent itself* ends the DM, whose sender self-notification (`bus.rs:597-628`) is silently skipped today for lack of a source session.

The episode tracker's resolve-and-route override (phase 1, step 5) remains the **primary router**: it pins the continuation run to the job session even in the `or_insert` edge case where an older web-chat source entry for the same DM pair would win (`bus.rs:292-298`). `INTERNAL_SESSION_PREFIXES` (`runs/mod.rs:103-104`) and `find_user_facing_session` are **not** touched — job sessions remain non-targets for user-facing notification markers. The rejection's original purpose (#656/#680) is preserved for the remaining internal types.

### D4. Multiple pending items: wait on ALL

The pending-set model handles sequential and parallel work without special cases:

- **Parallel:** one turn sends two DMs and spawns a subagent → three pending entries → the episode waits on all. Each resolution triggers its own continuation turn (serialized by the per-agent queue).
- **Sequential:** turn ends with DM-1 pending → DM-1 resolves → continuation turn starts DM-2 → set is non-empty again → episode stays open.

Wait-on-**all** is correct: "any" semantics would close the job while a peer or subagent is still mid-work, recreating the bug at smaller scale. Set semantics make repeat sends to the same pair idempotent (one `Dm` entry per DM session).

**Known edge (documented, accepted for v1):** DMs started by the peer's side or from within the DM session itself (cross-DM) are not episode work — only sends recorded from episode runs count. A conversation the episode "joins" (pair already live from an earlier arc) is awaited like any other, even though the episode only owns part of it.

**Known edge, fixed (#1205):** because DM session ids are deterministic per agent pair, two open episodes of the *same agent* can both be pending on the *same* DM session (two jobs each messaged the same peer). `resolve_dm` resolves **all** episodes pending on the ended session — the conversation is genuinely over for every one of them — and the trigger loop enqueues one continuation run per resolved episode (each on its own job session, stamped with its own job id). Pre-fix, a single `ConversationEnded` resolved only the HashMap-order winner and the loser hung to the 4h deadline.

### D5. 4-hour hard deadline, detach-and-complete on expiry (ALPER'S CALL — timeout reinstated)

> History: revision 2 designed a no-timeout world at Alper's request; after the risk analysis below he reversed the call and reinstated a backstop. The liveness analysis stays in the doc because it is what motivates both the deadline *and* the step-7 panic guard.

Every episode carries a hard deadline of **4 hours** (`EPISODE_DEADLINE_SECS = 14_400`, a const in `job_episode.rs`, threaded through the tracker constructor so tests can shorten it). A sweep task checks deadlines once a minute. On expiry, the episode is force-closed with **detach-and-complete** semantics:

- The job **completes**: the completion card fires (with a deadline note and the count of detached items), `record_run` runs, the recurring re-arm / catch-up decision proceeds normally.
- Still-live pending work is **detached, never force-cancelled**: an in-flight DM or subagent keeps running on its own lifecycle (DMs remain bounded by `MAX_DM_DEPTH` and the depth-expiry sweep; subagents by the agent-loop hard caps). If a detached item later reaches its terminal state, the tracker resolve misses (episode gone) and routing falls back to today's behavior — post-D3 the notification run typically still lands on the job session as an untracked orphan turn, which is harmless: the card already reported the deadline.

Why a quiescence-only world was rejected — the liveness analysis it would have rested on:

- **DMs terminate provably within a live daemon** (the #1154 delivered|ended|errored gate + bounded never-drop channels + `MAX_DM_DEPTH = 20` caps every conversation), **but not across a restart**: a daemon restart loses in-flight triggers (#1159, B12), and the 1800s depth-expiry sweep emits **no** `ConversationEnded` (`bus.rs:157-186` tombstones only).
- **Background subagents terminate on every non-panic path** (`run_subagent` emits exactly one `SubagentCompletion` per terminal status, `crates/alms-coordinator/src/lib.rs:1250-1327`) — but there is **no Drop-armed guard on the completion send**: a panic before the emission point unwinds without a completion (the `NamedSubagentGuard` RAII pattern, `lib.rs:903-914`, covers only the name lock).

With the deadline in place, both holes degrade from "episode hangs forever" to "job completes late, at the 4-hour mark, with a visible deadline note." A bookkeeping bug in the episode accounting (a missed `execute_run` exit, a leaked `in_flight_runs`) self-heals the same way — the deadline doubles as the leak collector.

**The step-7 panic guard stays regardless**: it is a real bug (today it strands the parent's "running" chip forever), and with the guard a subagent panic resolves the pending entry in seconds instead of making the job wait out the full 4 hours.

**Episode persistence (phase 2):** the deadline means closing #1159 first is no longer a *hard* prerequisite — a persisted episode that survives a restart with dead triggers now self-heals at the deadline instead of hanging forever. It is still strongly preferable to land #1159 first (a 4-hour silent stall on every restart-interrupted episode is poor behavior, just no longer unbounded). In phase 1 episodes remain in-memory: a restart drops the episode; recurring jobs self-heal at the next tick (the stale past-due `next_run_at` even gets re-fired by `bootstrap_scheduler`, `server/mod.rs:333` — an accidental retry; see #763 finding 1); `Once` jobs are lost like any `Queued` run today.

`DELETE /jobs/{id}` (D7) remains the immediate manual escape hatch, and `GET /jobs` surfaces open-episode age and pending counts (step 8) so a long-lived episode is *visible*.

### D6. Recurring overlap: QUEUE, coalesced to depth 1 (ALPER'S CALL: queue, not skip)

A firing that becomes due while the job's episode is still open must run **after** the episode closes — not be skipped, not overlap.

**Where the queue lives — nowhere, by construction.** Under the episode model the re-arm moves from run-end to episode close, so the scheduler holds **no armed entry** for a job while its episode is open — a firing can never physically arrive mid-episode. "Queued firings" are therefore *derived at close from cron math* instead of stored:

```
close_episode(job, Recurring{cron}):
    missed = next_after(cron, episode.started_at) <= now   // did >=1 tick elapse during the episode?
    if missed:
        fire ONE catch-up episode immediately              // enqueue fire_job_run on the agent queue
        record_run(..., next_run_at = now)                 // UI shows the catch-up as due-now
    else:
        re-arm normally: schedule_once(next_after(cron, now))
```

- **Ordering:** the catch-up enqueues on the same per-agent `SessionQueue` as a scheduler firing, so it serializes behind any other work the agent has; at *its* episode close the same check repeats, so a backlog drains one coherent episode at a time.
- **Queue-depth guard (recommendation, needs Alper's ack):** missed ticks **coalesce into exactly one catch-up firing** — the queue is structurally a dirty-bit, never a list. Rationale: every firing of a job runs the *identical prompt*; N back-to-back catch-ups of the same prompt add no information and burn N× tokens, and with no episode timeout an actually-stored queue could grow without bound behind one long conversation. Coalescing caps the whole mechanism at depth 1 by construction. The alternative (bounded FIFO of depth N) is strictly more state for firings that are byte-identical inputs.
- **Interaction with the deadline (accepted):** a long-lived episode delays the queued firing by up to the 4-hour deadline — the job's *next* occurrence waits for the job's *current* work, and the D5 deadline bounds the wait. The `GET /jobs` episode surfacing (step 8) keeps this observable; `DELETE /jobs` is the immediate remedy for a conversation the operator no longer wants to wait on.
- A defensive guard stays in the fire path: if a firing ever *does* arrive for a job with an open episode (e.g. a future scheduler change, or bootstrap re-firing a stale `next_run_at`), it is absorbed into the same coalesced dirty-bit (`catch_up_queued = true`) rather than overlapping.
- `Once` jobs have no queue — nothing re-fires.

### D7. Cancellation

`DELETE /jobs/{id}` (`jobs.rs:98-123`) today: `job_store.cancel` + `scheduler.cancel` + `cancel_runs_for_job`. Under episodes, add teardown:

1. **Episode removal:** drop the tracker entry first — `on_run_complete` for any still-draining run then no-ops, and the completion block never fires (generalizing the existing mid-run guard at `notifications.rs:111-119`).
2. **Runs:** `cancel_runs_for_job` already covers queued/running continuation runs *because step 5 stamps `job_id` on them*.
3. **Pending DMs:** for each `PendingWork::Dm`, mirror the existing `cancel_dm` machinery (`dm_lifecycle.rs:674+`): cancel active runs on the DM session and call `end_conversation(…, UserCancelled)` so the peer gets the standard ended-notification instead of being stranded. Ending the DM here is unambiguously right — the operator explicitly killed the job.
4. **Pending subagents:** for each `PendingWork::Subagent`, fire the existing subagent cancel path (`cancel_subagent` / handle token). The resulting `SubagentCompletion(Cancelled)` finds no episode (already removed). **Post-#1206 its notification run is suppressed at the source** — `enqueue_triggered_run` declines to create a run targeting the session of an *operator-cancelled* job (membership in `AppState.operator_cancelled_jobs`, populated only by `DELETE /jobs`; the completion marker + SSE still persist). Suppression is required because the run is created asynchronously, *after* step 2's `cancel_runs_for_job` sweep — merely stamping it could never catch it (that was the original design's error here: the "cancelled by (2) if still queued" claim never held). The suppression deliberately keys on operator intent, not `JobStatus::Cancelled` — spent one-shots also carry that status (#763) and their deadline-detached late results must still be delivered (see D5).

`POST /runs/{id}/cancel` on an individual episode run stays per-run: it cancels that turn only, and the turn's terminal arm feeds `on_run_complete` like any other exit (a cancelled turn does not by itself close or hang the episode — the pending set still governs). Existing turn-1 cancellation behavior (the S1/#1154 chokepoints) is unchanged.

### D8. Interaction with existing machinery

- **DM conversation-liveness invariant (#1154):** the enabler, not a conflict — the episode's pending-DM accounting is sound because every executed DM run signals a terminal state; a weakened invariant degrades an episode to a 4-hour deadline close rather than a hang. The DM completion gate itself is untouched.
- **Queued-run re-enqueue on restart (#1159):** see the persistence note in D5 — the deadline means #1159 is no longer a hard prerequisite for episode persistence, but landing it first is still strongly preferred (avoids a 4-hour silent stall on every restart-interrupted episode). Phase 1 keeps episodes in-memory.
- **The `notifications:{agent}` DM-ended run:** for job-originated DMs this path is superseded by D3 + the tracker override (continuation lands on the job session). For all non-job DMs, routing is byte-identical to today — the #513/#495 no-reroute regression suite must keep passing untouched. `notify_dm_ended_to_webchat` (the human-facing marker) fires in all cases, unchanged. If a `ConversationEnded` arrives for a DM whose episode no longer exists, the tracker misses and routing falls back to the pre-episode behavior — with a #1206 split on the fallback's job-session target: **operator-cancelled** job (`DELETE /jobs` mid-flight) → the orphan notification run is suppressed at the source (no turn burned on a killed job); **deadline-closed** episode (D5, including spent one-shots despite their `Cancelled` status — the #763 quirk) → the orphan run fires normally, which is precisely how detached work delivers its late result.
- **Subagent completion loop (#1041 ordering invariant):** the marker-before-SSE ordering in `completion_notification_loop` (`notifications.rs:458-563`) is preserved; the episode hook only adds a tracker resolve + `job_id` stamp around the existing `enqueue_triggered_run` call, after the marker/SSE block.
- **Jobs sidebar (#1197) / completion card (#1196):** the card moves in *time* (episode close instead of turn-1 end), not in shape. The `run_id` on the card is the episode's final run; card metadata gains optional `turns` / `dm_count` / `subagent_count` fields (additive). The deep-link `job_session_id` already points at the session where the whole arc now lives.
- **Episodic summaries:** per-run summary generation (`lifecycle.rs:2540-2571`) continues untouched; continuation runs on the job session get summaries like any system-triggered run.

## Phase 1 — Implementation Plan

One PR (this one). Both `PendingWork` types ship in phase 1 (Alper's scope call). Steps are ordered so each lands with its tests; steps 1–3 are prerequisites for 4–6.

### Step 1 — `JobEpisode` + tracker (new module)

**Files:** `crates/alms-gateway/src/runs/job_episode.rs` (new); wired into `AppState` (`crates/alms-gateway/src/server/mod.rs`).

```rust
pub(crate) enum PendingWork {
    Dm(SessionId),        // deterministic DM session id
    Subagent(TaskId),     // alms_coordinator::TaskId
}

pub(crate) struct JobEpisode {
    job_id: JobId,
    session_id: SessionId,             // the job_{id} session
    agent_id: AgentId,
    started_at: DateTime<Utc>,         // basis for the D6 catch-up cron math
    deadline: std::time::Instant,      // opened_at + EPISODE_DEADLINE_SECS (D5)
    pending: HashSet<PendingWork>,
    in_flight_runs: usize,             // episode runs currently queued or running
    runs: Vec<RunId>,                  // turn-1 + continuations (card stats, final run_id)
    catch_up_queued: bool,             // D6 defensive dirty-bit
}

pub(crate) struct JobEpisodeTracker {
    episodes: DashMap<JobId, JobEpisode>,
    by_dm: DashMap<SessionId, JobId>,      // reverse index: pending DM -> episode
    by_task: DashMap<TaskId, JobId>,       // reverse index: pending subagent -> episode
}
```

API surface: `open`, `note_run_enqueued(job_id, run_id)`, `on_run_complete(job_id, run_id, tool_calls) -> Quiescence` (scans records via step-2 helpers, decrements `in_flight_runs`, returns `Closed(JobEpisode)`/`Open`), `resolve_dm(dm_session_id) -> Option<EpisodeRef>`, `resolve_subagent(task_id) -> Option<EpisodeRef>`, `snapshot(job_id)` (for `GET /jobs`), `remove(job_id) -> Option<JobEpisode>` (cancellation).

Invariant maintained by the tracker: reverse indexes are inserted/removed atomically with the `pending` set (single entry mutation per episode; the tracker is the only writer). A resolve returns the episode ref *and* pre-increments `in_flight_runs` in the same call, so quiescence cannot fire in the gap between "pending removed" and "continuation run enqueued".

Unit tests in-module (pure state-machine tests, no `AppState`).

### Step 2 — pending-work detection helpers

**File:** `job_episode.rs` (gateway is the only consumer; not promoted to `alms-core` unless a second consumer appears).

- `dms_opened(&[ToolCallRecord]) -> Vec<SessionId>`: result-role records with `tool_name == "send_message"`, result JSON `delivered == true`, parse `dm_session_id`. Folded (`folded: true`) and error results excluded.
- `subagents_spawned(&[ToolCallRecord]) -> Vec<TaskId>`: result-role records with `tool_name == "invoke_agent"`, result JSON containing `task_id` and no `error` key (background dispatches only — foreground results carry `response`, not `task_id`; see the `invoke_agent.rs` result contracts at `:196` and the `:457-476` tests).

Mirrors the `ran_ignore_message_successfully` scan pattern. Tests: fixture records for delivered/folded/failed sends, fg/bg invokes.

### Step 3 — `bus.rs`: job sessions as DM sources

**File:** `crates/alms-coordinator/src/message_bus/bus.rs:274-299` — remove `"job"` from the reject match arm (keep `"notification" | "subagent" | "episodic"` and the same-DM guard).

Tests (extend `message_bus/tests.rs`): source recorded for a job-session send; `end_conversation` routes the peer trigger to the job session; sender self-notification fires when the job agent ends its own DM; the #656/#680 rejection suite intact for the remaining internal types.

### Step 4 — relocate the completion block; quiescence at every episode-run exit

**Files:** `crates/alms-gateway/src/runs/notifications.rs`, `crates/alms-gateway/src/runs/lifecycle.rs`.

- Extract `fire_job_run:103-146` (notify + `record_run` + re-arm) into `close_episode(state, episode, outcome)`; add the D6 branch (coalesced immediate catch-up vs normal re-arm).
- `fire_job_run`: `tracker.open(...)` before `execute_run` (absorbing a defensive `catch_up_queued` flip if an episode is somehow already open); after it, `tracker.on_run_complete(...)`; close if quiescent. The existing cancelled-during-run guard (`:111-119`) becomes "episode already removed → no-op".
- `execute_run` (`lifecycle.rs`): for runs carrying `job_id` (turn-1 already does; continuations after step 5), feed `on_run_complete` from **every** exit — the `Ok` arm, the `Err` Failed/Cancelled arms, **and the pre-cancel early exit** (`lifecycle.rs:1304-1388`, queued-then-cancelled). A missed exit leaks `in_flight_runs` and stalls the episode until the 4-hour deadline — this is the single most correctness-critical hook in the PR; add tracing when a job-stamped run reaches a terminal state without a tracker entry.
- Failed/cancelled turns do not force-close: the pending set still governs (a failed continuation while a DM is open must keep waiting for that DM); a failed *quiescent* turn closes the episode with the failure on the card.

### Step 5 — resolve + route: continuation runs onto the job session

**Files:** `crates/alms-gateway/src/runs/notifications.rs` (both loops), `runs/mod.rs` (`enqueue_triggered_run` signature).

- `enqueue_triggered_run` gains `job_id: Option<JobId>`; when `Some`, the created `Run` is stamped (`Run::for_job` shape) so `cancel_runs_for_job` and the step-4 hooks see it. All existing callers pass `None`.
- `run_trigger_loop`, `ConversationEnded` arm (`:904-1072`): compute the DM session id (already done there for the depth-exceeded SSE); `tracker.resolve_dm(dm_session_id)` → on hit: remove pending, **override the target session/context to the episode's job session** (superseding the `source_sessions`-derived target), pass `job_id` to `enqueue_triggered_run`. On miss: existing routing byte-for-byte.
- `completion_notification_loop` (`:430-592`): after the marker/SSE block (#1041 ordering untouched), `tracker.resolve_subagent(completion.task_id)` → on hit: remove pending, pass `job_id` (routing already correct — the parent session *is* the job session). On miss: existing behavior.

### Step 6 — cancellation teardown

**Files:** `crates/alms-gateway/src/jobs.rs`, reusing `dm_lifecycle` / subagent-cancel internals.

`cancel_job`: `tracker.remove(job_id)` first, then per D7 — `end_conversation(UserCancelled)` + DM-session run cancels for each pending `Dm`; subagent cancel for each pending `Subagent`; existing `cancel_runs_for_job` (now covering stamped continuations) stays.

### Step 7 — subagent completion-liveness guard

**File:** `crates/alms-coordinator/src/lib.rs`.

RAII guard armed at the top of `run_subagent` for background tasks (mirroring `NamedSubagentGuard`, `:903-914`): holds `completion_tx` + the minimal `SubagentCompletion` fields; disarmed by the normal emission at `:1306-1326`; on `Drop` while still armed (panic unwind), emits a `Failed` completion with a `"subagent task panicked"` summary. Test: a dispatcher whose loop panics still produces exactly one completion — and the normal path produces exactly one, not two.

This closes the only known in-live-daemon liveness hole in the D5 analysis — with the guard, a subagent panic resolves the pending entry in seconds instead of costing the job the full 4-hour deadline wait. It is a standalone bug fix even without episodes (today a panicking bg subagent strands the parent's "running" chip).

### Step 8 — observability + docs

- `GET /jobs` (`jobs.rs:81-84`): per-job `episode` object when open — `started_at`, `pending_dms`, `pending_subagents`, `in_flight_runs`, `catch_up_queued`, `deadline_remaining_secs`. The visibility requirement from D5/D6.
- Completion card metadata: `turns`, `dm_count`, `subagent_count` (additive; `notify_job_completion` reads them from the closed episode).
- `CHANGELOG.md`: operator-facing note — job completion cards now fire at true completion; recurring firings queue (coalesced) behind open episodes.
- `docs/api.md` jobs section; this doc flips to reflect as-built.

### Step 9 — the 4-hour deadline sweep (D5)

**Files:** `job_episode.rs` (`take_expired`), `crates/alms-gateway/src/runs/notifications.rs` (`job_episode_sweep_loop`), `crates/alms-gateway/src/server/mod.rs` (spawn).

A shutdown-aware loop ticks every 60s and calls `tracker.take_expired()` — episodes whose `deadline` has passed are removed (atomically, same lock as every other tracker op) and handed to `close_episode(state, episode, timed_out = true)`: completion card with the deadline note + detached-item count, `record_run`, re-arm/catch-up. Pending items are NOT cancelled (detach-and-complete). Races degrade gracefully: a run completing after the sweep removed its episode finds no tracker entry and no-ops; a later DM/subagent resolution misses and falls back to default routing.

### Step 10 — test strategy

Integration tests (`crates/alms-gateway/src/runs/integration_tests.rs` + module tests), keyed to the risk list:

| Scenario | Pins |
|---|---|
| Job with no async work | Behavior byte-identical to today (card at turn-1 end) — the regression floor |
| Single DM arc (peer ends) | Deferred card; continuation on job session; close after quiescent continuation |
| Job agent ends its own DM (`ignore_message`) | D3's sender-self-notification path produces the resume turn |
| Parallel DMs + one bg subagent | Wait-on-all; three resolutions, interleaved continuations, single close |
| DM started from a continuation turn | Pending set regrows; episode stays open |
| Subagent-only job | Completion resolves pending; #1041 marker/SSE ordering preserved |
| Continuation run cancelled while `Queued` | Pre-cancel early exit feeds `on_run_complete`; no `in_flight_runs` leak |
| Failed continuation with DM still pending | Episode stays open; failed quiescent turn closes with error card |
| Recurring: episode outlives a cron tick | Exactly ONE coalesced catch-up fires at close; multiple missed ticks still → one |
| `DELETE /jobs` mid-episode | DM peers get `UserCancelled`; subagent cancelled; stamped runs cancelled; no close-block |
| Deadline expiry | Episode with an unresolved pending item force-closes at the deadline: detached (pending NOT cancelled), completed, card carries the deadline note |
| Non-job DM regression suite | #513/#495 no-reroute tests untouched and green |
| Panic-guard (step 7) | Exactly one `Failed` completion from a panicking bg subagent |

### Main risks

1. **The `run_trigger_loop` routing override** touches the most reroute-regression-prone path in the gateway (#513/#495 history). Mitigation: the override fires only on a tracker hit (job-episode DMs); the miss path is byte-identical; the existing no-reroute suite is the guardrail.
2. **`in_flight_runs` / pending leaks = 4-hour-late closes.** The deadline doubles as the leak collector, so a bookkeeping bug self-heals — but a systematically late job card is still a defect. Mitigation: step 4's every-exit discipline including the pre-cancel early exit, tracing on tracker misses, and step-8 observability; `DELETE /jobs` as immediate remedy.
3. **The subagent completion invariant is less battle-tested than #1154's DM invariant** and is now load-bearing for episode liveness. Mitigation: step 7 guard + tests; flagged for Tim's review attention.
4. **User-visible timing change:** recurring jobs with chatty agents "complete" much later and their next firings queue behind the conversation — intended, but needs the CHANGELOG note and `GET /jobs` visibility to pre-empt "my job stopped firing" reports.

## Phase 2 — Deferred

- Episode persistence + boot recovery — preferably with/after #1159 (see the persistence note in D5; the 4-hour deadline bounds the damage either way).
- UI: "job in progress — chatting with X / subagent running" state on the job card / Jobs sidebar entry (the API fields ship in phase 1, step 8).
- Optional explicit `complete_job` early-exit tool if structural quiescence proves too eager/lazy in practice.

## Decision Log

| # | Decision | Status |
|---|---|---|
| 1 | JobEpisode direction: per-turn runs, completion at episode close, structural quiescence, job-as-DM-source | **APPROVED (Alper, 2026-07-06)** |
| 2 | 4-hour hard deadline (const `EPISODE_DEADLINE_SECS = 14_400`), detach-and-complete on expiry (pending work left running, never force-cancelled) | **APPROVED (Alper, final)** — supersedes the revision-2 no-timeout call |
| 3 | Recurring overlap: queue, not skip | **APPROVED (Alper)** |
| 3a | Queue depth: coalesce missed ticks into exactly one catch-up firing (dirty-bit, not a list) | **APPROVED (Alper, final)** |
| 4 | Phase-1 scope includes background subagents (`PendingWork::Subagent`) | **APPROVED (Alper)** |
| 5 | Step 7 (subagent panic-completion guard) stays in phase 1 | **APPROVED (Alper)** — avoids a needless 4-hour wait on a panicked subagent; standalone bug fix |
