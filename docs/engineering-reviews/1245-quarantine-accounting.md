# Where the reviewer is argued down, and says so

> Reconstructed from the review history of the ALMS project's original issue tracker.
> Cross-references like `#1246` point at that tracker and are kept verbatim for provenance.

**Source:** PR #1245 -- Quarantine accounting for skipped persistence rows (#1241) + the rule it implements (#1237)  
**Rounds:** 2  


Round two opens by conceding a point: *"you took the right branch. I agree, and the argument you added is stronger than mine."* It then narrows the remaining disagreement to four specific clauses rather than restating the original objection. Worth reading for how the disagreement resolves.

---


## Round 1 -- 2026-08-05


## Review by Tim (automated)

**Verdict: Needs minor fixes.** One operator-facing doc claim is wrong and shouldn't ship (C1). Everything else is a suggestion. The **code** half (#1241) I would merge as-is — it is the cleanest instrumentation PR I have reviewed in this repo.

Reviewed `c0a3da5..feature/quarantine-doctrine-row-skip-metrics` (+837/-48, 18 files).

### Both amendments to my #1237 recommendation are correct. I accept both.

**Amendment 1 — "logged once with its id and a remediation" does not survive a per-read loader. You are right and I was wrong to state it unqualified.**

I wrote that obligation for a one-shot startup sweep, where "one `error!` per bad row carrying its id and the repair SQL" is bounded and the id is actually available. Neither holds at a loader. The row is re-encountered on every read, so the line cannot be bounded; and when the unparseable column *is* the identifier — `sessions.id`, `runs.agent_id`, `agents.created_at` in your own tests — there is nothing left to name it by. Documenting that as a genuine weakening at those 28 sites, rather than bending 28 call sites to a rule written for a different shape, is the right call.

**Amendment 2 — "exactly one fatal site" needed scoping to *reconciliation*. Right, and I verified it against the file.**

`migrations.rs` fails closed in at least four more places: WAL refusal (`:70`), non-contiguous migration history (`:101`), a migration body that will not apply or lands out of order (`:156`, `migration_error`), and an invalid migration list (`:188`/`:195`). Unqualified, my claim is simply false against that file. The distinction you drew — those mean *no* row is interpretable, which is upstream of a rule about what to believe about a particular row, not an exception to it — is the correct one, and it is stated well in both `architecture.md` and `database-migrations.md`.

The rest of the doctrine is, I think, better written than what I recommended. "Refusing to boot is not a safety property. It is availability spent to buy one, and it only pays off if the operator acts before harm occurs" is the sentence I was reaching for and didn't find. Keeping the jobs justification ("*this job is not scheduled* is a true and safe statement") explicitly separate from availability, with the stated reason that otherwise someone cites it at the migration guard next year, is exactly the failure mode doctrine text has.

---

## Critical

### C1 — `docs/api.md` §8.1 tells operators the wrong thing about 8 of the 12 counters

The new paragraph reads:

> Most of these are **quarantine counters**: they count durable state the daemon declined to believe. Any non-zero value means the daemon is serving an incomplete view of the database ... `job_boot_catch_ups_total` is the exception.

Against the JSON block immediately above it, that is not right. Of the twelve scalar counters, **four** are quarantine counters: `job_rearm_failures_total`, `stale_run_recovery_failures_total`, `job_bootstrap_failures_total`, `persistence_rows_skipped_total`. The other eight are not — `queue_saturation_rejections_total`, `lifecycle_transition_rejections_total`, `replay_gaps_total`, `replay_epoch_mismatches_total`, `persistence_snapshot_rejections_total`, `job_dispatch_retry_attempts_total`, `job_dispatch_retry_exhaustions_total` are rejections, and `job_boot_catch_ups_total` is workload.

So "most" is wrong (4 of 12), and "the exception" is wrong (there are eight).

This matters more than an ordinary doc slip, for two reasons:

1. It is **operator-facing instruction on how to read an alert**. As written it tells someone that a non-zero `queue_saturation_rejections_total` — expected under load, and interesting only as a rate — means their database is corrupting.
2. It **contradicts the taxonomy this same PR introduces**, 40 lines away in `operations.rs`, where those seven are grouped under "**Rejections** — a request or transition the daemon refused. Expected to be non-zero under load; interesting as a rate."

The fix is to reuse your own taxonomy, which is already written and is good: replace the two-way split with the three groups (Rejections / Quarantine / Workload), name which counters are in each, and attach the "incomplete view of the database" sentence to the Quarantine group only.

**Related, and worth saying explicitly: this is the one place declining the rename has a cost.** Your rename rationale is correct and I would have declined too — `docs/api.md`, shipped CHANGELOG entries, and unknowable field `curl` scripts are real consumers, and "no frontend consumer" is not "no consumer". (I re-verified the frontend half independently: nothing in `frontend/`, `contracts.ts`, `static/ui/`, or `ui-dist/` reads `/operations/metrics`, so the #1227 contract rule genuinely does not bite here.) But the *reason* a rename was tempting is precisely the reason C1 happened: the wire names do not carry the group, so prose has to, and prose drifts. Comment-grouping the struct is the right call for now — the api.md paragraph just has to carry the same three groups rather than a two-way split.

---

## Suggestions

### S1 — the doctrine states four mandatory obligations, then documents a class that meets two

"A site may quarantine only if it does **all four**" is immediately followed by "This is a real weakening of obligation 2 at those sites." As written, a reviewer quoting the four obligations at a future PR gets "the 28 loaders don't do that either" as a valid rebuttal, and the rule loses its force at exactly the moment it is being used.

And it is obligation **4** as well as 2. Obligation 4 says "a remediation reachable **through the product**". The loader paragraph then says obligation 4 "is met without ceremony at those sites — edit or delete the row with `sqlite3` while the daemon runs." `sqlite3` is by definition not through the product, and it cannot be here: the in-product remediation for a corrupt `sessions` row would be `DELETE /sessions/{id}`, which you cannot address because the unparseable column *is* the id. That is not a flaw in this PR — it is a true fact about these sites. The doctrine should state it rather than claim compliance.

Suggested restatement so all four are satisfiable as written:

> 2. **One `warn!`/`error!` line carrying the strongest identifier available** — the row id where the row is identifiable, otherwise the table and the failing column. Bounded (once per row) at one-shot sweeps; unbounded (once per read) at loaders.
> 4. **A remediation that does not require stopping the daemon.** Through the product where the entity is addressable; `sqlite3` against the live database where it is not.

That keeps the rule quotable while preserving the honesty of the amendment, which is the part I want kept.

### S2 — the delete-path skips are a third outcome the test doesn't classify, and the table calls them loaders

The doctrine's test is binary: absence-is-safe to quarantine, re-executes-completed-work to fatal. The two new `delete_agent` sites are neither. Applying the test literally: if that session id is dropped, the daemon **does** do something it would not have done had the row been correct — it removes the agent while leaving that session's messages, runs, and tool-call rows undeleted, permanently, with no retry path, because the agent row is gone. Nothing re-executes, so it is not fatal; but "absence is safe" is also false. Durable garbage is a third outcome.

Your inline comment at the site says exactly the right thing ("orphaned, not lost. Counted so the leak is visible"). The doctrine just has no slot for it, and the table row reads "The 28 row-drop points across 16 loaders — any row that fails to parse — **Yes**, by the same argument," which sweeps three non-loader write-path sites into the loader argument. The arithmetic gives it away: 28 points across 16 loaders **plus** `delete_agent` and `migrate_telegram_context_ids`, which are not loaders. (I counted independently and get the same 28 and the same 16 — agents 4, audit 4, jobs 1, messages 3, runs 2, session_summaries 2, sessions 4, timeline 1, tool_calls 7.)

Either carve those three out of that table row with their own one-line justification, or add a third branch to the test: *"Yes, it leaves durable state inconsistent"* to quarantine, but say so at the site and count it.

### S3 — `sessions` now means two different things in one counter

`persistence_rows_skipped_by_table["sessions"]` is incremented by three loaders, **and** by `delete_agent` (twice), **and** by the Telegram context-id migration. `docs/api.md` documents the meaning as "a session missing from the sidebar". An operator seeing `sessions: 3` will go looking for missing sidebar rows when the event may in fact have been a partial agent delete or a partial migration — different symptom, different remediation, same number.

Cheapest fix is one sentence in api.md acknowledging the dual meaning. The more durable one is a distinct label for the write-path sites.

### S4 — the scope note's claim that field-level fallbacks "are logged" is not true of all of them

The two examples it names do log — `reasoning_effort` at `sqlite/mod.rs:532`, `worktree_mode` just below. But `parse_run_row` silently degrades two **foreign keys** with no log and no counter:

```rust
let job_id = job_id_str.and_then(|s| uuid::Uuid::parse_str(&s).ok()).map(alms_core::job::JobId);
let parent_run_id = parent_run_id_str.and_then(|s| uuid::Uuid::parse_str(&s).ok()).map(RunId);
```
(`sqlite/mod.rs:710-715`.) And `delete_agent`'s agent-name lookup does a bare `.ok()` at `agents.rs:305`, where a `None` silently skips the entire DM-cleanup branch.

These are not the weaker class the scope note describes — they are arguably worse than a dropped row. `reasoning_effort -> None` yields a config default. `job_id -> None` is a **false belief projected into live state**: the run stops being attributable to its job, which is precisely the hazard the doctrine's own framing names ("the hazard is that the daemon believes something false"), and it makes `cancel_runs_for_job` miss that run.

I am **not** asking you to instrument them here. That is #1241 scope creep, and your argument that the counter must keep meaning "rows the daemon cannot see" is right. I am asking the scope note not to assert they are all logged, and ideally to note that a field-level fallback on a *foreign key* does not obviously belong to the weaker class.

### S5 — the log line shape changed at ~25 sites and the CHANGELOG doesn't mention it

Every site that emitted its own message (`"Skipping unparseable agent row: {e}"`, `"Skipping unparseable session row: {e}"`, `"Skipping tool call record: bad role"`, and the rest) now emits the single `"Skipping unparseable persistence row"` with `table` and `detail` fields. That is a better shape and I would keep it — but any operator alert or log filter keyed on the old strings goes quiet without failing, which is the worst way for a monitor to break. In a CHANGELOG whose stated emphasis is operator-facing changes, that deserves a half-sentence beside the counter bullet.

### S6 — `record_skipped_row` must never touch `self.conn`, and nothing says so

All 28 call sites execute inside a `query_map`/`filter_map` iteration while the caller holds `self.conn.lock()`, and `conn` is a `parking_lot::Mutex` — non-reentrant, so re-locking on the same thread is a hard deadlock, not a panic. The current body only touches an atomic and `tracing`, so it is correct today.

But the function's own doc invites the exact change that breaks it: *"`detail` carries whatever identifies the row ... the column that failed to parse is often the id itself."* The natural follow-up is "let me re-query the rowid so the log can name it" — which hangs the daemon on the first corrupt row in production, with no error and no panic to point at. One line on the doc comment closes it: **"Callers hold the connection lock; this must never touch `self.conn`."**

---

## What I checked and found clean

- **All 28 sites accounted for, and no site missed.** Counted independently (above); matches your recount. No `Skipping unparseable ... row` strings remain anywhere in `alms-session` outside the new unified one and the deliberately-scoped-out `reasoning_effort` field log. `summaries.rs::load_summary` is a single-row `query_row` with an optional-field fallback — correctly out of scope, not an oversight. Every remaining `.ok()` in the crate is field-level (see S4).
- **The three previously-silent sites** (`delete_agent` x2, Telegram context-id migration) were genuinely not in my #1237 inventory. Good catch, and each got an inline comment explaining what the drop actually costs rather than a generic one.
- **The counters share correctly.** `RowSkipCounters` sits behind an `Arc` cloned in `SqliteStore::Clone`, and `corrupt_agent_row_is_dropped_and_counted` asserts `store.clone().rows_skipped_total()` — the right assertion, since every handle wraps one connection and the totals are meant to describe the database rather than the handle.
- **Test quality is above the bar for this repo**, and specifically covers the failure modes rather than the happy path: the inner-`query_map` drops (bad role, bad timestamp) as distinct from the `Err` arm; the second-phase content-JSON drop in `load_messages`; per-table attribution including an asserted **zero** for a sibling table; and the one most people would skip — the "counts skips, not rows" property asserted directly (`list_agents` twice gives 2). `corrupt_with_sql` planting states no public write path can produce is the right seam for this.
- **Closed enum over a string-keyed map is the right call**, and `every_table_has_a_distinct_slot` pins the hand-written `index()` against `ALL` — the one thing that could silently alias two tables onto one counter.
- **`format_args!` at the call sites** avoids allocating on a path that only runs when something is already wrong.
- **The full key set is reported even with no SQLite store configured**, and the gateway integration test asserts that against `PersistenceTable::ALL` rather than a hardcoded list. That is the right shape for a scraper contract.
- **The startup-sweep boundary held.** `mark_stale_runs_failed` deliberately does not route through this counter — its skips stay in `stale_run_recovery_failures_total` — the runs.rs test says so in a comment, and the doctrine table lists them as separate rows. That distinction was easy to blur and wasn't.
- **`Ordering::Relaxed`** is correct for monotonic observability counters; `total()` summing slots non-atomically can only produce a slightly-stale total, which is fine for what it is.

## Nit

**N1** — `job_boot_catch_ups_total` moved to the tail of `OperationalMetricsSnapshot` to make room for the group comments, so serialization order changed; the `docs/api.md` example JSON still shows it in its old position between `stale_run_recovery_failures_total` and `job_bootstrap_failures_total`. Key order is not semantic, but the example no longer demonstrates the grouping the PR is arguing for — and if C1 is fixed by importing the three groups into api.md, reordering the example to match is free.

## On the open item you raised

You are right that the single-writer boundary condition is prose with nothing enforcing it, and right not to have acted on it here. Worth a follow-up issue rather than scope creep. The cheapest real enforcement is a startup advisory lock on the database file — or an `owner_pid`/`boot_id` row written under `BEGIN IMMEDIATE` at open — so a second daemon against the same file fails loudly at boot instead of silently invalidating every row of that table. That is the change that would let the assumption fail loudly, which is what the section says it wants.


## Round 2 -- 2026-08-19


## Review by Tim (automated) — round 2

**Verdict: Needs minor fixes — doc-only. The code half is unconditionally ready; nothing in `alms-session` or `alms-gateway` needs to change.**

Re-reviewed `7f88b3b..5afdf95` (18 files, +1005/-58). `7f88b3b` is the true merge base — `44ecebf` merged `develop` in and `develop` has since moved to `4ba91a4`, so it is no longer an ancestor of this branch and diffing against it would pull #1244's coordinator work in as phantom net-new. New since my last pass: `5afdf95` alone.

| Item | Status |
|---|---|
| **C1** — `api.md` §8.1 taxonomy contradiction | **Closed** |
| **S1** — four obligations vs. a class meeting two | **Closed** |
| **S2** — delete-path skips unclassified by the test | **Closed**, with three refinements below |
| **S3** — `sessions` means three things in one counter | **Partially closed** — decline accepted, substitute one sentence short |
| **S4** — scope note claims all field fallbacks are logged | **Closed** |
| **S5** — log-line shape change unrecorded | **Closed** |
| **S6** — `record_skipped_row` must not touch `self.conn` | **Closed** |
| **N1** — example JSON order | **Closed**, and improved beyond what I asked |

The two things keeping this off "ready to merge" are both single-sentence doc edits, listed as R1 and R2 at the end. Neither touches code and neither needs a re-run of anything but the docs job.

---

## C1 — closed

`operations.rs` now carries 7 / 4 / 1 across three contiguous comment blocks plus `subscribers` documented as a gauge, and §8.1 names the same members in the same order. I checked the membership field-by-field against the struct rather than against the prose: Rejections is `queue_saturation`, `lifecycle_transition`, `replay_gaps`, `replay_epoch_mismatches`, `persistence_snapshot_rejections`, `job_dispatch_retry_attempts`, `job_dispatch_retry_exhaustions`; Quarantine is `job_rearm_failures`, `stale_run_recovery_failures`, `job_bootstrap_failures`, `persistence_rows_skipped` (+`_by_table`); Workload is `job_boot_catch_ups`. That matches.

Two things you added that I did not ask for and that are the better half of the fix:

- **The alerting posture per group** (slope for Rejections, `> 0` for Quarantine). That is the actual decision an operator makes, and it is the thing the old two-way split got backwards. A group label without a posture would have been a taxonomy; with it, it is an instruction.
- **"Change the two together", stated in both places.** You are right that the specific sentence was the symptom and the missing group-in-the-wire-name is the cause. This is the cheapest available brake on the next drift, and it is in the right two files.

You were also right that it was worse than I said — 7/4/1, so "most" and "the exception" were each wrong in both directions, not one.

---

## S2 — you took the right branch. I agree, and the argument you added is stronger than mine.

**The reasoning holds.** A carve-out on the table row would have fixed the table and left the test binary, and the test is the sentence that gets quoted at a PR two years from now. Adding the branch is the correct level to fix it at.

**The "fatal is actively worse" argument is right, and I verified it rather than taking it.** `delete_agent` returns `AlmsResult<bool>`; a `query_map` error on the session-id collection would propagate out and abort the transaction. Every subsequent attempt reads the same unreadable row, so the agent is not merely undeleted, it is *undeletable* — and the row that blocks the delete is a child of the very agent the operator is trying to remove. That is #1236's shape exactly: the false belief disabling its own remedy, in a tighter loop than #1236 had. Naming it two sections after the #1236 paragraph is the right placement.

Three refinements, all in the branch's condition text — the part that gets quoted, which is why I am raising them at all.

### S2a — the branch is one clause too wide: it licenses destruction, not just stranding

As written:

> **Yes, but only by leaving durable state inconsistent — orphaned or unmigrated rows, with nothing re-executing** → quarantine

The property that actually makes `delete_agent` acceptable is not "nothing re-executes". It is that the damage is **additive** — rows survive that should have gone — so a human with `sqlite3` can still repair it. Your own prose leans on this: *"the leak stays recoverable by hand precisely because the rows are all still there."* That clause is doing the work, and it is in the justification rather than in the test.

Consider the same shape with the polarity flipped. Step 4b of `delete_agent` collects DM candidates **to purge**. Had that loop been written to collect the sessions **to keep** — which is how a retention sweep or a session GC would naturally be written — an unreadable row would cause a live DM session to be deleted rather than leaked. Nothing re-executes; durable state is left inconsistent; it passes the third branch as written; and it is data loss. Same function, same idiom, opposite polarity.

One clause closes it:

> - **Yes, but only by *stranding* durable rows — orphans or unmigrated rows left behind, with nothing re-executing and nothing removed that should have survived** → quarantine, *and* the call site must name what it strands.
>
> The stranding must be additive. A drop that causes rows to be *deleted* which should have been kept is not this branch: the damage is not recoverable by hand, which is the only reason durable garbage is tolerable here.

### S2b — "fatal" is boot-scoped everywhere else in the document, but the third branch is request-scoped

The rule paragraph says ALMS quarantines "rather than **refusing to run**". The fatal exemplar is "**refusing to open**". The whole "Exactly one fatal site" section counts sites that abort startup. So within this document, *fatal* means the daemon does not come up.

The third branch lives on a request-scoped write path. There, the counterfactual to quarantine is not refusing to boot — it is failing the request and rolling back the transaction, which is what your own justification describes ("failing the delete makes the agent permanently undeletable"). So the same word now denotes two different remedies inside one three-branch test, and a reviewer who lands on **branch 2** at a future write path — a collection loop that gathers ids in order to re-fire something already done — gets an answer in the wrong vocabulary. "Make the daemon refuse to boot" is not an available action inside `DELETE /agents/{id}`.

One sentence under the test:

> At a startup or recovery site, *fatal* means the daemon refuses to open the database. At a request-scoped write path it means the operation fails and its transaction rolls back — the daemon keeps serving either way. The branch you pick is the same; the remedy it names is scoped to where the site runs.

### S2c — the branch requires the leak to be named "in the log detail", and it is not

> the call site must name what leaks **in a comment and in the log detail**

The comments do this well — all three sites got one, and the two new ones (DM candidate, Telegram migration) match the quality of the session-id one. The `detail` fields name the **site**, not the leak: `delete_agent <id>: unreadable session id: <err>`, `delete_agent <id>: unreadable dm candidate: <err>`, `telegram context-id migration: <err>`.

That is the right content for `detail` — and §8.1 describes it accurately ("the write-path sites prefix it"). It is `architecture.md` that over-claims, about its own three sites, in the same document that just fixed an over-claim about obligations 2 and 4. Align the doc to the code:

> the call site must name what it strands in a comment, and prefix its log `detail` with the site so the drop is distinguishable from a loader drop on the same table.

### S2d — the arithmetic is still off by two, in the direction the split created

`25 across 16 loaders + 3 write-path` (architecture.md:387, CHANGELOG.md:117). The 25 and the 3 are right — I recounted all 28 call sites and get the same split (the 3 being `agents.rs:324`, `agents.rs:464`, `sessions.rs:249`). The **16** is not.

The 25 loader drop points live in **14** distinct functions: `list_agents`, `agents_with_telegram`, `load_audit`, `query_jobs`, `load_messages`, `load_runs_by_session`, `load_recent_runs`, `load_all_sessions`, `load_sessions_by_agent`, `list_sessions`, `load_session_summaries`, `load_timeline_events`, `load_tool_calls`, `load_tool_calls_for_session`. (15 if you count `query_jobs`'s two public callers instead of the private helper — either way, not 16.)

16 was correct *before* the split: it is the count of all functions containing a drop point, i.e. these 14 plus `delete_agent` and `migrate_telegram_context_ids`. Splitting the points out without splitting the function count leaves the loader row double-counting the exact two functions the row beneath it describes. 14 + 2 = 16 is the tell.

---

## S3 — I accept the decline. The substitute is one sentence short, and it is the actionable sentence.

**Partially closed.**

**On the decline itself: you are right and I withdraw the "more durable" framing.** The map's key is a promise about where to look, and `PersistenceTable::as_str` says so in its own doc comment ("the string form matches the SQL table name so an operator reading the metrics payload can go straight to `sqlite3`"). `agent_deletes` would be an answer to "which code path" when the operator's question is "which table". `timeline` already costs a parenthetical everywhere it appears, and a second non-table key would retire "keyed by SQL table name" as a rule while leaving it as a nine-tenths habit — which is worse than either alternative. The `detail` prefix is the correct disambiguator and it was already there; the gap was that nothing said so. Documenting it is the right fix at the right cost.

**But §8.1 now says "Same number, three different remediations" and then gives one, and that one is wrong for two of the three.** The remediation paragraph, unchanged, reads:

> Remediation is per-table and needs no restart: find the row with `sqlite3 .alms/alms.db` (the `warn!` detail names the failing column), then fix or delete it. The daemon picks up the repair on the next read.

That is exactly right for the 25 loader sites. For the other two producers of `sessions` it is not:

- **`delete_agent`.** The transaction has already committed — the `filter_map` drops the id and the delete proceeds. Repairing the unreadable `sessions` row afterwards changes nothing: the parent agent is gone, nothing re-runs the delete, and the session's `messages`, `runs`, `run_tool_calls`, `audit_events` and summaries stay behind forever. The real remediation is the opposite of "fix the row" — it is to finish the delete by hand.
- **`migrate_telegram_context_ids`.** Called once at channel startup (`gateway.rs:494`, "Phase 2b"), not on any read path. "The daemon picks up the repair on the next read" is simply false here; nothing reads it again until the next boot. The operator has to apply the rename themselves (`UPDATE sessions SET context_id = 'telegram_<agent>_<chat_id>' WHERE id = ...`).

The second one is worth pausing on, because it lands on obligation 4. If the documented remediation is "the next read picks it up", and no next read exists, then the only remaining path the doc offers is a restart — and obligation 4 says a remediation must not require stopping the daemon. The compliant remediation *does* exist (hand-apply the rename, live), so the site is fine; the doc just does not name it, and as written it points the operator at the one action that does not work.

This is a smaller instance of exactly the C1 failure mode: operator-facing prose that names the wrong action. It bites less often, because it needs a corrupt row on a write path to fire at all. But it is the substitute you chose *in place of* the separate key, so it should be finished. Two sentences appended to that paragraph:

> That applies to the loader drops. A `sessions` drop from `delete_agent` cannot be repaired by fixing the row — the delete has already committed and nothing re-runs it, so the remediation is to remove the stranded rows for that `session_id` (`messages`, `runs`, `run_tool_calls`, `audit_events`, `context_summaries`, `session_summaries`) and then the session row itself. A drop from the Telegram context-id migration is not re-read either: it runs once at channel startup, so apply the rename by hand (`UPDATE sessions SET context_id = 'telegram_<agent_name>_<chat_id>' WHERE id = ...`).

The same over-broad sentence is in `CHANGELOG.md:122-123` ("Remediation needs no restart: fix or delete the row and the next read picks it up"), sitting two lines below the mention of the 3 write-path sites. Worth qualifying there too, or dropping the clause.

Your closing line on this — *"if this turns out to be wrong in practice it's because someone alerts on the number without reading the log, in which case the fix is a real dimension on the metric rather than a second key with a different meaning"* — is the right escape route and the right trigger for taking it. I would leave it exactly there.

---

## S1, S4, S5, S6, N1 — all closed

- **S1.** The restatement is in, near-verbatim, and moving the loader discussion into "How one-shot sweeps and per-read loaders discharge obligations 2 and 4" is better than what I suggested: the list stays a list of requirements, and "All four are requirements, with no exempt class" now has nothing beneath it that contradicts it. The obligation-4 reframing — that what it protects is the ability to act *while the daemon serves*, with the product path required wherever the entity is addressable — is a cleaner statement of the principle than "through the product" ever was, and it closes the loophole rather than widening it.
- **S4.** The scope note no longer asserts they are all logged, names both silent sites, and states plainly that a foreign-key fallback is not a defaulted column. Filing #1246 rather than instrumenting here is the right scope call.
- **S5.** In the CHANGELOG, and phrased as breaking-for-alerting, which is the consequence that matters. Also correctly flags the JSON key reordering for anyone pretty-printing the payload — I had not asked for that.
- **S6.** The invitation is gone and the consequence is spelled out. "Do not re-query the row to give the log a better identifier ... the daemon would hang on the first corrupt row it met in production, with no error and no panic to point at", plus the escape route (select it in the original query, pass via `detail`). That is the version that survives someone in a hurry.
- **N1.** Example JSON reordered, and moving `subscribers` to the struct tail was the better catch — it was splitting the Rejections group in half, so the struct was not demonstrating its own taxonomy either. Confirmed no test asserts field order.

**One leftover from S2's third branch, in the helper's doc comment.** `record_skipped_row` still opens "Quarantine one durable row **a loader** could not parse" and asserts "the loaders here all behave correctly if it is simply absent" — which is precisely what is *not* true at the three write-path sites, and is the reason the third branch exists. `row_skips.rs`'s module doc gets this right (lines 12-17, "Three of the counted sites are **not** loaders"). The helper doc is the one a new call-site author reads before adding the 29th site, so it is the one worth correcting: a clause noting that three callers are write paths where absence strands rows, and that such a site owes a comment naming what it strands.

---

## On #1247 — the premise holds, the label does not, and the label is the part that will get quoted

You wrote, for whoever picks it up:

> that change would be a **second fatal site**, which the doctrine says must be conspicuous and needs an explicit argument. The argument is available — with a second writer, `running` rows stop being safely absent and move to the fatal class *by the doctrine's own test* — so it's the rule applying itself, not an exception.

**The premise is correct.** With a second writer, a `running` row belonging to daemon B and swept by daemon A is not safely absent: A marks it failed, the client sees a dead run and retries, and the work executes twice while B's copy is still in flight. That reaches the "re-executes something already done in the world" horn squarely. The boundary-condition section was right to say the classification is only valid under one writer, and #1247 is the right follow-up.

**But the enforcement is not a second fatal *reconciliation* site, by your own qualifier.** You wrote, two sections earlier:

> The qualifier *reconciliation* is load-bearing. Startup also fails closed when the schema itself cannot be established — a gap in migration history, a migration body that will not apply, a file-backed database that refuses WAL. Those are not judgements about what to believe about a row; they mean **no** row can be trusted to be interpretable, which is upstream of the rule rather than an exception to it.

An advisory lock or `owner_pid` check that refuses to open when another daemon holds the database is that same class. It is not a judgement about what to believe about a row; it is "no row can be trusted, because someone else is mutating them underneath you". Same shape as WAL refusal, one word away from the sentence already in the doc — the carve-out currently says *interpretable*, and this case is about *stable*. Widening it to "interpretable, or stable while we read it" brings the lock inside the existing carve-out cleanly.

There is a sharper version of the point. If the lock exists, the second writer never gets in, so `running` rows **never actually reclassify**. The lock does not respond to a violation of the single-writer premise — it *preserves* the premise, which is what keeps every row of the table above valid as written. That is a better argument for #1247 than "it is a second fatal site", and it does not spend the "exactly one fatal site" invariant to make it.

Filing it as a second fatal site costs two things. It makes "exactly one" read as broken when it is not — and you argued yourself that the invariant's value is that a second one is conspicuous, which only works if the count is honest. And it hands a future reviewer a precedent: *#1247 added a fatal reconciliation site, so mine can too*, when the actual precedent is the opposite — a boot-time precondition check, of a class the doctrine already excludes. Worth editing in the issue before someone builds on it. No change needed in this PR.

---

## What I re-verified clean

Round 1 covered the code and none of it moved in `5afdf95` beyond the two struct-field reorderings and the doc comments. Spot-checked this round, per standing policy without running anything locally:

- **§8.1's group membership against the struct, field by field** — 7 / 4 / 1 plus the gauge, same members, same order, `persistence_rows_skipped_by_table` correctly inside the Quarantine block rather than beside it.
- **All 28 call sites still present and still 28**, splitting 25 / 3 exactly as the table claims (the 3 being `agents.rs:324`, `agents.rs:464`, `sessions.rs:249`). Only the function count is wrong (S2d).
- **The two new write-path comments** match the quality of the session-id one — each names what specifically survives the operation rather than saying "counted for visibility".
- **`row_skips.rs` module doc** was updated for the third branch and is accurate; only the `record_skipped_row` helper doc lags.
- **`PersistenceTable::as_str`'s doc comment** independently states the table-name contract you defended in S3, which is why that defence lands: the contract was written down before the argument was needed.

---

## The two fixes keeping this off "ready to merge"

Both doc-only, both a sentence or two, neither touching code:

- **R1 — `25 across 16 loaders` → `14`.** `docs/architecture.md:387` and `CHANGELOG.md:117`. This is in the site table, which is the deliverable of #1237 and the thing that gets quoted; a count that double-counts the two functions described in the row directly beneath it will be noticed by the first person who recounts, and it undercuts a table whose whole authority is that it is exhaustive and exact.
- **R2 — the remediation sentence in §8.1 (and `CHANGELOG.md:122`) is only correct for the loader class.** It is the actionable half of the fix you took *instead of* the separate metric key, so leaving it unqualified leaves S3 addressed in name only. Two sentences, drafted above.

Optional, in descending order of how likely they are to be quoted wrongly later: **S2a** (bound the third branch to stranding, not destruction), **S2b** (say that *fatal* means "fail the request" at a write path), **S2c** (align the "in the log detail" clause to what the sites actually emit), and the `record_skipped_row` doc comment.

---

## Verdict

**Needs minor fixes — doc-only.** The code half of #1241 I would merge on sight and would have merged last round; nothing in it changed for the worse and the two struct reorderings improved it. The blocker is fully closed, and closed better than I asked. Five of the six suggestions are closed; S3 is partially closed and its remainder is R2.

If you would rather merge now and land R1 + R2 as a docs follow-up, that is defensible — there is no code risk and no operator exposure until someone actually corrupts a row on a write path. My preference is to land them here, because both errors are *in the doctrine and its operator-facing companion*, which is the artifact this PR exists to produce. A wrong function gets fixed by the next person who runs it. A wrong rule gets quoted.

Two rounds, two pushbacks from you, both correct, and both improved on what I recommended rather than merely rebutting it. The S3 decline in particular is the right call for a reason I had not weighed — that the key is a promise about where to look, not a label for what happened.
