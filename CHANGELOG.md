# Changelog

Per-release notes for ALMS, with an emphasis on **operator-facing changes** — default flips, config/wire-shape changes, and upgrade impact. Implementation detail for individual changes lives in the linked PRs and git history; this file is the curated, upgrade-relevant summary. New entries go here (or in the PR description), **not** in `CLAUDE.md`.

## v0.2.4 — in progress (`develop`)

### ⚠️ Operator-facing default changes (read before upgrading)

- **Agent-loop hard caps now default ON; run-duration cap is now inactivity-based** — PRs #1160 / #987 / #1150.
  - `[llm].max_iterations` defaults to `500`. They bound a single run and apply to **all run types** (web chat, cron jobs, subagents — inherited verbatim by subagents), not just DMs.
  - **The run-duration guard is now phase-aware inactivity, not flat wall-clock (#1150).** A run is terminated when it stops making *progress* (no streamed token / reasoning delta, LLM response, or tool start) for the budget of its current phase — so a long-but-**productive** run is never clipped. New `[llm]` knobs: `between_iterations_secs` (default `180`, the P1 between-iterations idle budget) and `tool_phase_ceiling_secs` (default `900`, the P3 tool-batch ceiling — deliberately above the `shell` tool's 600s `MAX_TIMEOUT_SECS` so a command run to its own cap doesn't false-stall at the next checkpoint). The P0 "awaiting first activity" budget is *derived* (`stream_chunk_timeout_secs + 30s`); P2 (mid-stream) has no separate timer — each delta resets the clock and a stalled stream is faulted by the per-chunk guard (#1169).
  - **`[llm].max_run_duration_secs` is now only an absolute backstop, default raised `14400` → `86400` (4h → 24h).** Inactivity is the primary guard now, so this only catches a run that pings activity forever (a bug); the higher default means a legitimate long-running scheduled job that makes steady progress is no longer clipped at 4h.
  - **Coordinator's 5-minute (300s) subagent run-kill removed (#1150).** That timer killed legitimately long subagents mid-work; a subagent is now bounded by the same inherited in-loop phase timer + `max_iterations` (the post-completion result-retention window is unchanged). Closes #1150.
  - **A blocking (foreground) `invoke_agent` is exempt from the P3 `tool_phase_ceiling_secs` (#1150).** The parent blocks on the subagent for its whole runtime, and the subagent's progress never reaches the parent's activity clock — so applying P3 would terminate the *parent* the instant a productive long subagent returned (discarding its work). The subagent is bounded by its own inherited phase timer; the parent's absolute `max_run_duration_secs` backstop still applies. A background `invoke_agent` returns immediately and is unaffected.
  - **A Guarded-posture batch blocked on human approval is exempt from the P3 `tool_phase_ceiling_secs` (#1150).** Guarded is the default posture for user-triggered interactive runs; a non-auto-approved tool blocks the run until the human approves or denies. A human who takes longer than the ceiling to approve must not be read as a stall (the same class as the foreground-`invoke_agent` exemption above) — so an approval-gated batch runs under an unbounded phase. The absolute `max_run_duration_secs` backstop (24h) still bounds a truly-abandoned approval. Batches of only auto-approved tools, and all FullControl / Autonomous runs, are unaffected.
  - A stalled run surfaces a distinct session label, **"Agent stopped after stalling (no activity)"**. After upgrading, a previously-unbounded deployment will end any run exceeding 500 LLM calls, stalling past a phase budget, or running 24h as `failed`. For a peer-triggered DM run, the gateway's completion gate converts the trip into an `Errored` conversation end so the peer is notified.
  - Set any knob to `0` to disable that cap/budget (the escape hatch for genuinely long workloads). All are **config-file-only**: not mutable via `PATCH /settings`, no `ALMS_*` env override, restart required. See `docs/config.md` § "Agent-loop hard caps".

- **Default model flipped again + summaries now default to a dedicated cheap model** — #1191 (adopts the `alms-test-workspace2` settings as code defaults).
  - `[llm].model` default went from `moonshotai/kimi-k2.6` to **`z-ai/glm-5.2`** on the `openrouter` provider — affects deployments with no explicit `[llm].model`, persisted `settings.json` model, or env override.
  - `[context].summary_model` / `[context].summary_provider` defaults went from both-`None` (inherit the agent's resolved provider/model, the #872 baseline) to the explicit symmetric pair **`google/gemma-4-31b-it` @ `openrouter`** — episodic run summaries on a fresh boot now hit a small non-reasoning model instead of the agent's chat model. The #877 pair-only invariant is unchanged: a hand-edited `alms.toml` setting only one of the two fields is still rejected at boot (the new defaults do **not** backfill a half-set pair).
  - ⚠️ **If your agents run on a non-OpenRouter provider** (e.g. Anthropic direct), the summary task now needs a resolvable `openrouter` API key — store one via `alms auth set openrouter <key>` or reconfigure the pair. **To restore the old inherit-the-agent-model behaviour**, clear both fields together: `summary_model = ""` + `summary_provider = ""` in `alms.toml`, or empty strings via `PATCH /settings` (whitespace-only counts as empty on both paths). The empty string is the explicit-clear sentinel on all three surfaces — TOML, PATCH, and the persisted `settings.json` — so a PATCH clear is **durable across restarts**: it is persisted as `""` and re-applied on boot instead of letting the compiled default pair resurrect (PR #1194).

- **Default model + Anthropic thinking budget flipped** — PR #1081.
  - `[llm].model` default went from `moonshotai/kimi-k2.5` to `moonshotai/kimi-k2.6` on the `openrouter` provider — affects deployments with no explicit `[llm].model` or env override.
  - `[llm.anthropic].thinking_budget_tokens` default went from `0` (disabled) to `2048` (enabled) — Anthropic deployments that did not explicitly set it to `0` will start paying ~2048 thinking tokens per turn.
  - **To disable thinking after the flip:** set the per-agent `thinking_budget_tokens` to `0` (an explicit `Some(0)` disables it), or pin `[llm.anthropic].thinking_budget_tokens = 0` fleet-wide (`alms.toml` or `PATCH /settings`). ⚠️ `clear_thinking_budget_tokens: true` does **not** disable — it resets the per-agent override to `None`, which now *inherits* the 2048 default (i.e. re-enables thinking). See the precedence chain in #767 / #809 / #941.

- **Per-LLM-call timeout defaults raised for heavy reasoning models** — PR #1177.
  - `[llm].timeout_secs` default `120 → 600` (10 min). This is the per-*call* HTTP deadline (one request→response), **not** a run cap — heavy reasoning models (`minimax/minimax-m3` on openrouter) legitimately reason past 120s, which surfaced as a subagent that "timed out without doing anything". A genuine *stall* still fails fast via `stream_chunk_timeout_secs`.
  - `[llm].stream_chunk_timeout_secs` default `60 → 180` (3 min, the per-chunk body-silence guard). This also lifts the *derived* P0 awaiting-first-activity inactivity budget (`stream_chunk_timeout_secs + 30`) from ~90s to ~210s (#1150) — intended, giving heavy reasoning models room before the first delta.
  - Both apply to all run types and are inherited verbatim by subagents. Pin lower values in `[llm]` (`alms.toml`) if you prefer tighter deadlines. The other agent-loop budgets (`between_iterations_secs` 180, `tool_phase_ceiling_secs` 900, `max_run_duration_secs` 86400) are unchanged.
  - A fixed **30s TCP+TLS connect timeout** was added to the LLM HTTP client so the raised `timeout_secs` doesn't let a dead/unreachable provider (wrong `base_url`, host down) hang for the full 10 min — only connection establishment is bounded, not the post-connect first-byte wait (which legitimately stays under `timeout_secs`). It's a fixed const, not a config knob (connect time is environment-fixed, not model/operator-tunable).

### Notable changes

- **A reverted shell `cd` is now visible to the agent** (#1262) — when a command's final working directory fails containment, the shell tool keeps the previous working directory (unchanged) and now says so in the result: `stdout` gains a `[cwd unchanged: '<attempted>' <verdict>; subsequent commands still run in '<kept>']` line, on both the foreground and the background (`check_task`) path. Previously the revert was a daemon-side `warn!` only, so the agent saw `exit_code: 0`, assumed it had moved, and misread every relative path afterwards. `<verdict>` reports only what the check established: `is outside the sandbox root` when both the root and the candidate resolved and the candidate genuinely was not under the root, and `could not be confirmed inside the sandbox root` when either path could not be canonicalised (the check fails closed either way, but an unresolvable path proves nothing about where it is — under Windows Git Bash's `/tmp` mount that is every command, see #1266). The sandboxed tool description now states the confinement too; unrestricted / `[security].allow_full_os_access` instances skip containment and get neither the sentence nor the notice.
- **Frontend dependency baseline audited and recorded** (#1232) — the `marked` 15→18 and `@preact/signals` 1→2 bumps that #1227 shipped unreviewed were verified against our usage (no rendering regressions; per-row sidebar reactivity intact), pinned by new markdown and active-run-dot tests, and documented in `docs/frontend.md`.
- **Durable job recovery and run admission** (Phase 7, PR #1230; follow-ups in
  #1233 / #1235 / #1236 / #1238). The largest operator-facing change in the
  stabilization series, and the one with a one-way database migration.

  **⚠️ Schema 3 is a one-way migration with a rollback re-execution hazard.**
  Rolling the binary back to `v0.2.4-pre-stabilization` **without restoring
  the pre-migration database backup will re-execute every completed one-shot
  and every retry-exhausted job**, about one second after the rolled-back
  daemon starts — real agent prompts, real LLM spend, real side effects. This
  is fail-*open* and nothing in the checkpoint binary prevents it: that build
  predates the newer-schema refusal guard, maps the unknown `completed` /
  `failed` statuses to `Pending`, and its startup filter excludes only
  `cancelled`. **Back up before upgrading**; see
  [`docs/database-migrations.md`](docs/database-migrations.md) §
  "Roll back to the checkpoint binary" for the full chain and the procedure.

  - **New job statuses on the wire: `completed` and `failed`.** Jobs are no
    longer forced through `cancelled` to express a finished one-shot.
    `terminal_reason` gains `retry_exhausted` alongside `completed`,
    `deadline_reached`, and `operator_cancelled`. Migration v3 rewrites legacy
    one-shot rows that encoded completion as `cancelled` + reason
    `completed`/`deadline_reached` to the distinct `completed` status; rows
    with a NULL reason are conservatively left `cancelled`, and
    `operator_cancelled` is untouched. **Clients that branch on job status
    must handle the two new values.**
  - **Bounded dispatch retries are durable.** The job entity gains
    `retry_count` and `last_error` (schema 3 columns), so a job that fails to
    dispatch retries with exponential backoff across restarts and, on
    exhaustion, lands in `failed` / `retry_exhausted` instead of disappearing.
    Both fields survive cancellation (#1238) so cancelling a job you watched
    fail does not erase the diagnostic.
  - **`DELETE /jobs/{id}` on an already-terminal job returns `409
    JOB_TERMINAL`** (distinct from the existing `409 ALREADY_CANCELLED`).
  - **New authenticated endpoint `GET /operations/metrics`** — process-lifetime
    counters and live subscriber gauges (`docs/api.md` § 8.1). Counters only,
    no payload data.
  - **Boot-time catch-up: restarting now replays missed job ticks.** A
    persisted `next_run_at` is authoritative for every schedule type, so a
    tick missed while the daemon was down is *caught up* at boot rather than
    skipped to the next occurrence. This makes restart the recovery mechanism
    for a failed re-arm — and it means **restarting the gateway costs LLM
    spend proportional to how long it was down.** #1235 staggers that catch-up
    cohort (most-overdue first, 15s apart) so a restart after long downtime
    spreads the firings instead of running them all within one second;
    `job_boot_catch_ups_total` reports the cohort size. Jobs that are not past
    due keep their exact schedule.
  - **A transient persistence failure no longer silently stops a recurring
    job** (#1233). The episode-close write now has a bounded retry budget, and
    on exhaustion the job is re-armed in memory at its next occurrence rather
    than left with no scheduler entry. Surfaced as
    `job_rearm_failures_total`; non-zero means a job's persisted schedule is
    stale until its next successful run.
  - **One bad row no longer prevents the daemon from starting** (#1236).
    Startup stale-run recovery reconciles each row in its own savepoint; a row
    that fails is logged with its `run_id` and the SQL to clear it, counted in
    `stale_run_recovery_failures_total`, and skipped so the rest still
    recover — and it is *not* projected into the live run registry, so it
    cannot masquerade as a pending run (which would make its session
    undeletable and pin the sidebar's active-run indicator). Job bootstrap got
    the same treatment: a job whose startup fire time cannot be persisted is
    skipped and counted in `job_bootstrap_failures_total` instead of aborting
    startup for every other job. Both deliberately relax the fail-closed
    behaviour introduced by "Fail closed on partial session reconciliation":
    an unbootable daemon is a worse outcome than one stale row. **Non-zero
    values on either counter need operator attention** — the affected runs and
    jobs stay broken until repaired.
  - **Run admission is one durable fact.** A run, its initial user message,
    and the session activity timestamp now commit in a single transaction, and
    same-session admissions serialize on a per-session gate so database order,
    in-memory history, queue order, and the session SSE feed cannot diverge.
    A new `500 ADMISSION_PROJECTION_FAILED` covers the case where the durable
    write succeeded but the in-memory projection did not.

- **Silent row loss in the persistence layer is now countable** (#1241), and
  the policy behind it is written down (#1237).
  - **New counters on `GET /operations/metrics`: `persistence_rows_skipped_total`
    and `persistence_rows_skipped_by_table`.** Every list-shaped loader in
    `alms-session` drops rows it cannot parse — a corrupt `agents` row simply
    vanished from the registry, a corrupt `sessions` row from the sidebar,
    with only a `warn!` line as evidence. All 28 drop points (25 across 14
    loaders, plus 3 on write paths) now increment a shared counter attributed
    to the table the row came from. **Non-zero means the daemon is serving an
    incomplete view of the database.** It counts *skips, not distinct rows*:
    the loaders run on every read, so one bad row on a hot path increments it
    repeatedly — read the rate, not the total. Remediation never needs a
    restart, but it differs per producer: at a loader, fix or delete the row
    and the next read picks it up; at the two write-path sites the operation
    has already committed and nothing re-runs it, so the `warn!` `detail`
    prefix tells you which hand-repair applies (`docs/api.md` § 8.1).
  - **Breaking for log-based alerting: those ~25 sites changed their log line.**
    Each used to emit its own message — `"Skipping unparseable agent row: …"`,
    `"Skipping unparseable session row: …"`, `"Skipping tool call record: bad
    role"` and the rest. They now emit a single `"Skipping unparseable
    persistence row"` with structured `table` and `detail` fields. **Any log
    filter or alert keyed on the old strings goes quiet without failing** —
    re-key it on the new message, on `table=`, or preferably on the counter.
  - **Also documented in `docs/api.md` § 8.1: the scalar counters on
    `/operations/metrics` are grouped** into Rejections (expected non-zero
    under load — alert on a slope), Quarantine and degradation (alert on
    `> 0`), and Workload.
    The field order of the JSON response follows the grouping; key order was
    never semantic, but scripts pretty-printing the payload will see it move.
  - **`docs/architecture.md` gains the reconciliation policy: *absence must be
    a safe belief*.** When a startup pass or loader finds a row it cannot
    repair, ALMS quarantines it — left durable and untouched, kept out of live
    in-memory state, logged, and counted — rather than refusing to run.
    Quarantine is legal
    only where the daemon behaves correctly believing the row is absent; where
    absence would re-execute completed work the failure stays fatal. That
    makes the schema-version guard in `sqlite/migrations.rs` the **only**
    sanctioned fatal reconciliation site, and it documents the deliberate
    decision **not** to add a `--skip-recovery` escape hatch. No behaviour
    change on its own — it names what #1233 / #1235 / #1236 / #1241 already do.

- **Foreign-key fallbacks in the persistence layer are no longer silent**
  (#1246), and they are counted apart from row skips.
  - **New counters on `GET /operations/metrics`:
    `persistence_fields_degraded_total` and
    `persistence_fields_degraded_by_field`** (keyed `<table>.<column>`, every
    known field reported including the zeroes). Four parsers replaced an
    unreadable column with a fallback and kept the row, with no log and no
    counter: `runs.job_id` and `runs.parent_run_id` in `parse_run_row`,
    `session_summaries.last_run_id` in `parse_session_summary_row`, and the
    agent-name lookup in `delete_agent`. All four now `warn!` and increment.
  - **`session_summaries.last_run_id` was a real bug, not just missing
    telemetry, and it is the one to alert on.** That column is the
    compare-and-swap sentinel for episodic-summary upserts. When it failed to
    parse, every subsequent summary write for that session came back as a
    *conflict* — so the agent burned three LLM summarization calls, gave up,
    and logged `Failed to persist session summary due to concurrent updates`
    with no concurrent update anywhere in sight. Episodic memory for the
    session was stuck permanently and every signal pointed at the wrong cause.
    It is also the only one of the four on a live read path.
  - **Deliberately a separate counter from `persistence_rows_skipped_total`,
    which is unchanged.** That number means *rows the daemon cannot see*;
    these rows are perfectly visible, just wrong. Folding them together would
    have destroyed both. **Alert on the new counter at least as loudly as the
    old one** — a skipped row is trust withheld and bounded, a degraded field
    is trust misplaced and projected into live state.
  - **The two `runs.*` fields are attribution defects, and repairing them
    needs a restart.** A degraded `job_id` makes `GET /runs` report a null job
    and label the run `user` instead of `scheduled`; `parent_run_id` turns a
    subagent run into a top-level one. Nothing is left running, and no cancel
    is missed — the parser is reached only by the boot-time stale-run sweep
    and by hydration, both of which see terminal rows. Because the live `Run`
    is never refreshed from disk, fixing the cell takes effect at next start;
    `docs/api.md` § 8.1 says so explicitly rather than implying it self-heals.
  - **`DELETE /agents/{id_or_name}` distinguishes cases it used to conflate.**
    "No such agent" is the normal path and skips DM cleanup correctly — not
    counted. A genuinely unreadable `agents.name` skips the same branch but
    strands shared DM sessions whose participants are all gone; that is now
    counted and logged with a `delete_agent <id>:` prefix. The delete still
    succeeds in every case, so an agent never becomes undeletable.
  - **Fixed: `DELETE /agents/{id_or_name}` could delete a live peer's DM
    session, messages and all.** The DM-cascade peer probe treated any
    unsuccessful `SELECT 1 FROM agents WHERE name = ?` as "peer absent" and
    sent the shared DM session to the purge list. Two ways to reach that: a
    transient SQLite error, or — with no error at all — a peer whose own
    `name` cell is not readable text, since SQLite never compares a BLOB or
    NULL equal to a text parameter, so the probe simply matches nothing. The
    rule is now **only a peer proven absent may purge**: a miss counts as
    absence only when every `agents.name` cell in the table is readable text,
    and anything unprovable leaves the DM session alone. **No behaviour change
    for callers** — the delete still commits; the DM session is stranded
    instead of destroyed, and counted so the leak is visible.
  - **`docs/architecture.md`'s reconciliation-policy scope note is corrected.**
    It previously implied field-level fallbacks were a uniformly weaker class
    than row drops. They are a *second* class, and the less contained one: a
    row skip discharges the policy's obligation 3 (the bad fact never reaches
    live state), and a field degradation structurally cannot. The note now
    says when degrading is allowed at all — only where dropping the row is
    actively worse — carries a per-field table of read paths and consequences,
    and adds the polarity check that keeps a fallback from *deleting* data. It
    also fixes the discriminator for which fallbacks are benign: not "enum
    versus foreign key", but whether the fallback value is one the operator
    could legitimately have configured. `str_to_run_status`'s `_ => Queued` is
    an enum fallback that fails that test and is documented at the site.

- **Normalized frontend entity state and authoritative reconnect recovery**
  (PR #1228): agents, sessions, runs, and activity now share one typed reducer
  with revision/cursor guards. SSE replay gaps and epoch resets reconcile from
  authoritative snapshots, overlapping runs remain cardinality-safe, and the
  wire protocol is unchanged.

- **Normalized messages, jobs, and optimistic UI actions** (Phase 6B): chat
  history is now stored once per session by message ID, scheduled jobs share
  the typed entity store, and send/create/cancel operations have explicit
  confirm/rollback transitions. Browser tests now pin reconnect convergence,
  cross-agent message isolation, and delayed-request routing. Job creation and
  cancellation now return the final persisted entity; notably,
  `DELETE /jobs/{id}` returns `200` with the cancelled job instead of an
  empty `204`, so the UI receives the authoritative revision and terminal
  reason without a racy follow-up snapshot.

- **Revision-aware run and job lifecycles**: all production lifecycle changes
  now pass through explicit state machines with legal-transition checks,
  idempotent terminal outcomes, and a monotonically increasing revision.
  Run and job SQLite upserts compare revisions so delayed coordinator,
  scheduler, or recovery snapshots cannot resurrect cancelled work or replace
  a newer terminal result. One-shot jobs retained the legacy cancelled status
  for compatibility while the additive terminal reason distinguished normal
  completion, deadline completion, and operator cancellation — **superseded by
  Phase 7 below**, which introduced real `completed` / `failed` job statuses
  and migrated those legacy rows. Restart recovery also advances the run
  revision and records the gateway-restarted reason.
- **Transactional, versioned SQLite migrations** (PR #1224): startup now
  records ordered schema changes in `schema_migrations`, applies each step
  together with its version row in one `BEGIN IMMEDIATE` transaction, and
  rejects migration gaps or databases newer than the binary supports. Schema
  2 adds backward-compatible lifecycle revision and terminal-reason columns
  for runs and jobs. File-backed databases must now successfully enable WAL;
  filesystems that previously caused SQLite to fall back silently to
  rollback-journal mode will fail startup and must be moved to a WAL-capable
  volume. See `docs/database-migrations.md` for backup, compatibility, and
  rollback guidance.
- **Atomic, bounded per-agent run admission** (PR #1223): the gateway now
  admits at most 64 pending runs per agent and 1,024 across the daemon while
  retaining normal-before-low priority and draining accepted work at shutdown.
  `POST /runs` rejects saturation before any run, message, cancellation
  token, or SSE side effect with `429 AGENT_QUEUE_FULL` /
  `GATEWAY_QUEUE_FULL`, a `retry_after_ms` body field, and
  `Retry-After: 1`; shutdown/unavailable dispatch returns
  `503 QUEUE_UNAVAILABLE`. Internal DM trigger capacity is reserved before
  persistence: saturation now returns an explicit error instead of waiting
  for channel capacity. Depth-overflow and `end_conversation` preserve
  retryable state when trigger capacity is unavailable, and a closed trigger
  channel no longer leaves a marker without its notification run.

- **Sidebar active-run dot now lights on cross-agent sessions** (#1211 / PR #1220): the web UI's blinking active-run indicator previously only lit on the *currently-viewed* session — a run on a session owned by another agent (a scheduled **Job**, a **DM**, or another agent's chat surfaced in the sidebar's cross-agent sections) never lit its dot until you clicked into it. Root cause: the sidebar subscribed only to the *active agent's* per-agent SSE feed (`GET /agents/{id}/events`), which by design never carries other agents' activity. **New endpoint:** `GET /events/session-activity` (see `docs/api.md` § 5.10) — a global, cross-agent SSE feed carrying `session_activity_started` / `session_activity_ended` across every agent's sessions, served from a dedicated broadcast namespace (separate from the per-agent sender map + event log, so no operator-supplied agent id can collide with it and leak activity across the per-agent isolation boundary). The sidebar now subscribes to it and seeds its indicators from every surfaced session. No config or wire-break — the per-agent feed (§ 5.9) is unchanged and remains for agent-scoped consumers.
  - Both the per-agent and global `session_activity_*` payloads now include the additive `has_active_run` boolean: the backend's authoritative post-transition answer for the whole session. Consumers must use it instead of treating every individual `session_activity_ended` as inactivity, because overlapping runs can share one session.

- **Scheduled jobs now stay active until the agent's full task is done (#1198, "job episodes")**: a job that messages a peer (`send_message`) or dispatches a background subagent no longer reports *Completed* at first-turn end. The job's completion card, `record_run`, and the recurring re-arm are deferred until the whole arc settles — the DM resolves / the subagent completes, the agent resumes **on the job session** (with the transcript and its full job context) for more tool rounds, possibly starting further DMs/subagents — and only when a turn ends with nothing pending does the job complete. Operator-visible effects: **(a)** completion cards for "chatty" jobs arrive later (at true completion); the persisted marker's **metadata** gains an optional `episode` object (`turns` / `dm_count` / `subagent_count` / `timed_out` / `detached`) — metadata-only for now, the card UI does not render it yet (phase 2); **(b)** a hard **4-hour episode deadline** backstops the wait — on expiry the job completes with a deadline note and still-live work is *detached* (left running), never force-cancelled; **(c)** recurring firings that come due while an episode is open **queue, coalesced to exactly one immediate catch-up** at close (no overlap, no per-tick pile-up); **(d)** `GET /jobs` exposes an `episode` object for in-flight jobs (pending counts, deadline remaining); **(e)** `DELETE /jobs/{id}` now also ends the episode's pending DMs (`user_cancelled`) and cancels its pending subagents — and (#1206) the teardown no longer spawns follow-up LLM turns on the killed job's session (the DM-ended self-notification and cancelled-subagent notification runs are suppressed at the source; their history markers still persist). The suppression keys on the operator's `DELETE`, **not** on job status: a *one-shot* job that hits the 4h deadline with detached work still running still gets its late results delivered as notification turns on the job session. *(Corrected: this entry originally said such a job is "also recorded `Cancelled` — there is no `Completed` status". Both halves are false as of Phase 7 below — `completed` and `failed` are now real job statuses, and a deadline-closed one-shot records `completed` with terminal reason `deadline_reached`. Keying suppression on operator intent rather than status remains correct, and is now also more robust: it does not have to enumerate terminal variants.)* Job sessions are now legitimate DM sources on the message bus, so a job agent whose conversation ends gets its resume turn on the job session instead of the invisible `notifications:` session. Episodes are in-memory: a daemon restart drops them (recurring jobs self-heal at the next tick). Also fixes a latent bug: a background subagent that *panicked* before emitting its completion stranded the parent's "running" chip forever — a Drop-armed guard now emits a `Failed` completion. Design: `docs/jobs-await-completion-design.md`.


- **Context-debug row no longer auto-appears on notification runs** (PR #1195): the "Context sent to LLM" debug row now honors the per-agent Debug mode toggle on every run type. A #546-era convenience force-enabled `debug_mode` for system-triggered notification runs landing on a user-facing session — **subagent-completion** and **DM-ended** notification runs (job completions were never affected; they don't create a run) — which, after the per-agent toggle landed (#1003), silently overrode a toggle set to off. Most visibly: the row appeared on the parent's turn after a background `invoke_agent` subagent returned, with Debug mode disabled. Operators who want the context snapshot for notification runs enable Debug mode on the agent in Settings — the toggle now gates those runs exactly like normal turns.

- **Subagent cancel controls (session-keyed)**: a running subagent can now be cancelled from the UI — a ✕ control on RUNNING Subagent-status-bar chips and a "Cancel subagent" button in the drilled-down subagent session view — both behind an inline Yes/No confirm step (no native `window.confirm`). **New endpoint:** `POST /sessions/{session_id}/subagent/cancel` (see `docs/api.md` § 5.7.1) fires the subagent's own cancellation token via the coordinator; 404 `NO_LIVE_SUBAGENT` when the session has no live subagent. Session-keyed because a subagent's run id is not cancellable via `POST /runs/{run_id}/cancel` (no cancel token is registered for subagent runs). A cancelled **background** subagent renders the existing terminal *Cancelled* chip; cancelling a **foreground** subagent surfaces on the parent as the blocked `invoke_agent` call failing with "Subagent was cancelled" (parent run continues; chip renders *Failed*). Cancelling the parent run still cascades to subagents as before.

- **Subagent status bar: status-only redesign** (#1180 follow-up, subsumes #1186): the live subagent widget above the message input now shows a concise status per subagent (“Reasoning…”, “Using {tool}”, “Writing…”) instead of streaming the subagent's reasoning text into an expandable panel; clicking a chip navigates straight to the subagent's session (where the full transcript streams, #1184). **Wire change:** the subagent's reasoning/token text and tool params/results are no longer forwarded to (or persisted on) the parent's session stream/event log — the parent receives a new ephemeral, deduplicated `subagent_activity` SSE signal instead (see `docs/api.md`). Existing parent session logs are unaffected; replayed pre-change tagged events are simply ignored by the new UI.
  - **Fast-follow fixes** (#1189 follow-up): the coordinator now records each subagent's latest activity signal and the gateway replays that snapshot to every newly-attached session SSE stream — without it, a client that attached mid-phase (page reload, session switch back from the subagent view, second tab, SSE reconnect) showed the chip stuck on “Starting…” while the subagent was actively writing, because the deduplicated live signal fires at most once per transition and is never replayed. Also: a cancelled background subagent's chip now reads “Cancelled” (terminal, auto-removed) instead of its stale last activity, and the subagent-session “Back to parent session” button sits flush under the session header (it previously rendered inside the message scroller, offset by its padding).

- **Futile buffered-fallback short-circuit on LLM total timeout** (#1162 / #1163 / #1177): the agent loop attempts streaming first and falls back to a buffered `complete()` on a streaming failure. When the streaming attempt blew the **total `timeout_secs`** deadline (a reqwest `operation timed out`), that re-issue waits out the same deadline and fails identically — so the loop now skips the futile fallback and surfaces the diagnostic immediately (a background subagent fails fast with an attributable error instead of dead-airing ~2× the deadline). Every other failure keeps the fallback: a per-chunk *stall* can still recover (the buffered non-streaming first-byte wait absorbs a slow-generating model's mid-stream silence), as can *decode* faults (connection reset, malformed JSON, gzip).

- **Implicit DM replies + completion gate** (#1154 / #1156): a peer-triggered DM run's final assistant text **is** the reply (no `send_message`-per-turn). Every peer DM run exits as exactly one of delivered / ended / errored. DM sessions are agent-only — `POST /runs` on a `dm:` session returns `400 DM_SESSION_NOT_DIRECTLY_RUNNABLE`.
- **DM frontend rendering fixes** (#1155) and **backend hardening** (#1160 — iteration/duration caps, depth-leak heal, bounded channels, recipient-aware conflict, queued-cancel peer notification).
- **DM reload-render fixes**: `loadSession` resolved the DM flag from the per-agent sidebar list, which stopped containing DM sessions when PR #1010 split them into the cross-agent list — so on every DM session load since, tool calls rendered outside the reasoning collapsible, a mid-run load seeded the in-flight implicit reply as a mis-attributed duplicate bubble that survived the run, and a running DM reloaded into a generic "Thinking…" header instead of "Chatting with {peer}…". The flag now resolves from the authoritative `GET /session/{id}` envelope with both sidebar lists as fallback; the #1133 terminal-race reconciliation (clearing a stuck active-run marker / "Thinking…" row when a run finishes inside the load window) now runs for DM sessions too — only the in-flight text/reasoning seed stays DM-skipped.
- **Self-contained `builtin` shell engine** (#1143 / #1144): opt-in `[tools].shell_engine = "builtin"` re-execs `alms shell-host` to evaluate bash via the embedded brush interpreter; `system-bash` remains the default. See `docs/security-model.md` § 4.2a.
- **Dependency hygiene** (#1161): cleared RustSec advisories (rustls-webpki / quinn-proto / rand) and pruned unused dependencies.

## v0.2.3 — released (tag `v0.2.3`)

Stable on `main`; GitHub release with Linux x86_64 + Windows x86_64 binaries. Headline changes:

- Workspace v2 single-sandbox-root model (#945 / #946 / #947 / #948) — single project-root sandbox, flat `.alms/agents/<name>/` metadata, per-agent git worktree mode, operator full-OS-access escape hatch.
- Anthropic no-args tool fix (#967 / #968).
- Structured `MISSING_MODEL_AFTER_PROVIDER_SWITCH` 400 (#863 / #960).
- Per-tool structured output renderers (#873 / #952 / #970).
- Per-run config override path removed (#941) — agents are the single per-tenant config surface.

## Earlier

For releases before v0.2.3, see the git history and the GitHub Releases page.
