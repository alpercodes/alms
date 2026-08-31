# A full review -> fix -> re-review cycle

> Reconstructed from the review history of the ALMS project's original issue tracker.
> Cross-references like `#1246` point at that tracker and are kept verbatim for provenance.

**Source:** PR #1253 -- Count foreign-key field degradation apart from row skips (#1246)  
**Rounds:** 2  


Both rounds on one PR. The first pass returns *Needs minor fixes* with two Criticals and five Suggestions; the second re-reviews the pushed fix commit, walks the disposition of every prior finding, and closes them out. This is the shape most reviews in the project take.

---


## Round 1 -- 2026-08-22


## Review by Tim (automated)

**Verdict: Needs minor fixes.** The disposition is right at all three sites, the counter is shaped correctly, and the peer-probe fix closes a real data-loss path. What needs work is the *claims* around the code, not the code: the doctrine text ships a flagship damage story that the current call graph cannot produce, and it closes the inventory at three when there is a fourth site of the same class whose consequence is worse than either counted `runs.*` field. Both are text-and-one-enum-variant fixes. Nothing here is unsafe to run.

I did not run local validation (CI is green and the dev agent already ran the suite). Everything below is read from the code on `ef09ea1`.

---

### Critical

**C1 — The inventory is closed at three, and the fourth site is the worst one.**

`crates/alms-session/src/sqlite/session_summaries.rs:292`, in `parse_session_summary_row`:

```rust
let last_run_id = last_run_id_str
    .and_then(|s| uuid::Uuid::parse_str(&s).ok())
    .map(RunId);
```

Byte-for-byte the pattern you just fixed in `parse_run_row`, on a run-id foreign key, silent, uncounted. Issue #1246 enumerated three sites by grepping two functions; the PR adopted that denominator without re-deriving it, and now `DegradedField::ALL`, the `architecture.md` table ("The three sites we have (#1246) are:"), and the site-table row ("The 3 **field-level degradation** points") all assert a complete set. A future reader will treat that table as the inventory.

It matters more than the bookkeeping, because this field is not attribution — it is the **optimistic-locking sentinel** for episodic summary upserts (#1123). Trace a corrupt cell:

1. `load_session_summary` (`session_summaries.rs:233`) hands back `last_run_id: None`.
2. `episodic.rs:291` uses that as `expected_last_run_id`, so `upsert_session_summary_optimistic` takes the `expected_str.is_none()` branch: `UPDATE ... WHERE agent_id = ?1 AND session_id = ?2 AND last_run_id IS NULL`. The cell is non-NULL garbage, so **0 rows**.
3. It falls through to the `INSERT`, which hits the unique constraint and returns `Ok(false)` — "conflict".
4. `episodic.rs:361` reloads, gets the same degraded `None`, and retries. Three attempts, **each one a fresh LLM summarization call**, then `error!("Failed to persist session summary due to concurrent updates")`.

So: episodic memory for that session can never be updated again, three LLM calls are burned every time it is attempted, and the error names a cause that is not the cause. That is a textbook instance of your own framing — "trust misplaced and propagated" — and it is on a **live read path**, not a boot-only one, which is the property the docs claim for the two `runs.*` fields and (see C2) is actually false for them.

Preferred fix: a fourth variant, `session_summaries.last_run_id`, threading `&SqliteStore` into `parse_session_summary_row` exactly as you did for `parse_run_row` (both `filter_map` call sites at `:199`/`:213` already have `self`, and the single-row path at `:243` has the store in hand). Acceptable fallback if you want to hold scope: drop the completeness language, and file the fourth as a follow-up naming the CAS consequence — but under your own rule the follow-up is not optional, and this one degrades a value that gates a write rather than a label.

**C2 — The `runs.job_id` cancel hazard is not reachable, and it is now doctrine in seven places.**

The PR body, `field_degradation.rs`, the `parse_run_row` comment, the `warn!` detail string, `operations.rs`, `api.md` §8.1 and `architecture.md` all say some version of: a degraded `job_id` makes `cancel_runs_for_job` miss the run, so `DELETE /jobs/{job_id}` reports success while the run "keeps burning tokens". I tried to construct that state and could not. Three independent facts block it:

1. `cancel_runs_for_job` (`run_manager.rs:886-899`) filters `matches!(run.status(), Queued | Running)`. A terminal run is never a candidate.
2. `hydrate_from_store` (`run_manager.rs:413-419`, your #1236/#1239 fix) **refuses to project `Queued`/`Running` rows** into `self.runs` at all. So every run that entered the live map via `parse_run_row` is terminal by construction.
3. Runs that *are* live enter the map through `insert_persisted_run` (`run_manager.rs:538`) carrying a `Run` built in memory. Their `job_id` never round-trips through `parse_run_row`.

And the reach is narrower still: `load_run` and `load_runs_by_session` have **zero production callers** in the workspace (grep is all `#[cfg(test)]` — `gateway.rs:1247`/`:1304` are inside `#[test]` fns). `parse_run_row`'s only production readers are `Gateway::new`'s sweep (`gateway.rs:374`) and `hydrate_from_store`'s `load_all_runs`. Both boot-only.

The actual reachable consequence of a degraded `job_id` is that `GET /runs` reports `job_id: null` and `derive_trigger` (`read_api.rs:190`) labels the run `"user"` instead of `"scheduled"`. For `parent_run_id` it is `"subagent"` → `"user"` plus a null `parent_session_id` breadcrumb (`routes.rs:491-503`). Both are attribution defects on rows from a previous process. Real, worth counting, not a runaway run.

**Your conclusion survives intact — and one of your two costs is verified.** `Gateway::new` really does `store.mark_stale_runs_failed()?` at `gateway.rs:374`, so promoting either field to a row-level parse error really would collapse the whole `collect::<Result<_, _>>()` sweep and abort boot. (Note `hydrate_from_store`'s *own* call at `run_manager.rs:378` swallows the error and returns, so it is specifically the `gateway.rs:374` call that makes it unbootable — worth naming the line, since a reader checking only `hydrate_from_store` will conclude the opposite.) That cost alone decides it. You do not need the cancel story, and the argument is stronger without it: *"dropping cannot buy anything here, because the only rows that reach this parser in production are terminal rows on a boot path — and it costs the daemon its boot."*

Why I am ranking a wrong justification as Critical rather than a nit: this is the third PR in a lineage whose entire product is a doctrine other people will apply by analogy, and the worked example is the load-bearing part of a doctrine. Three specific things need to change, beyond the prose:

- The **`warn!` detail** at `mod.rs:826` ends with "cancel_runs_for_job will miss it". That is the one line an operator sees at 3am, and it sends them hunting a runaway job run that cannot exist. Obligation 2's whole value is that the line names the real consequence — make it the attribution defect.
- **`api.md`**: "`DELETE /jobs/{job_id}` will not stop it ... the job is cancelled while its run keeps burning tokens. **This is the one to alert on** — there is no other symptom." That inverts the ranking within the group: as written, `runs.parent_run_id` and `runs.job_id` are the same severity, and `agents.name` (durable stranding) is the worse one.
- **`architecture.md`**: "**cancel silently fails to cancel** a run that is still spending tokens" is now the scope note's only concrete illustration of why degradation beats quarantine for danger. Swap in an example that holds — `agents.name` stranding, or (if you take C1) the episodic CAS deadlock, which is a genuinely good one because the *false diagnosis* is the damage.

---

### Suggestions

**S1 — The peer-probe diagnosis is right; the remedy is the more expensive of the two safe options, and it is the one that needed a new doctrine class.**

Refusing to let an error mean "absent" is unambiguously correct — `.unwrap_or(false)` routing a live peer's DM session to `to_purge` is the only genuinely destructive fallback in the crate, and it should not survive.

But your polarity rule ("only `QueryReturnedNoRows` is allowed to mean absent") is satisfied by **two** dispositions, and you took the costlier one without weighing the other: on a probe error, `continue` — skip that one session, do not purge it, let the delete finish. That gives you no data loss (identical to failing), no undeletable agent, and it lands in the *existing* third branch: the DM session is stranded, which is additive, and it counts under `persistence_rows_skipped_total{sessions}` with the leak named at the site — exactly what the two sibling sites do **inside this same function** (`agents.rs:357` and `:497`). Under that option `architecture.md` needs no new "fatal at request scope" row and no new paragraph explaining why "exactly one fatal site" is still one.

Two things push me toward skip-and-strand:

- **It is asymmetric with your own argument twelve lines up.** For `agents.name` you write that failing the delete "would make the agent permanently undeletable, which is the #1236 pattern of a false belief disabling its own remedy". The peer probe has the same property under a *permanent* fault: `SQLITE_CORRUPT` on the `agents` index is not transient, and while it lasts, `DELETE /agents/{id}` returns 500 for every agent that has ever had a DM. Same remedy disabled by the same class of false belief, opposite ruling, ~40 lines apart.
- **You had to grow the doctrine to hold it.** Adding a *fatal* row to the site table and then adding a paragraph to "Exactly one fatal site" explaining that the count is still one is honest, and I would not have caught it if you hadn't — but a rule that needs a carve-out paragraph to preserve its own headline invariant is paying for something. Here it is paying for a disposition the third branch already covered.

Not a blocker: what shipped is safe, and "the delete errors" is recoverable in a way that "the DM session is gone" is not. If you keep it, then it owes obligation 4 like everything else in that section — right now the table's last column is `n/a`, and the only thing the operator gets is a 500 carrying `SQLite dm-cascade peer probe for session <sid>: <e>`. Say in `api.md` what to do with it: repair the `agents` row/index, or delete the DM session by hand, then retry the delete.

**S2 — `QueryReturnedNoRows` is not a reliable "absent" signal for a *name* probe, so the hazard has a residual you have documented away.**

`architecture.md` now states: "only `QueryReturnedNoRows`, the real 'absent' signal, is allowed to mean absent there." For a probe keyed on `name`, that is not quite true, and your own test builds the counterexample.

If the **peer's** `agents.name` cell is a BLOB — `UPDATE agents SET name = X'DEADBEEF'`, exactly the corruption `delete_agent_counts_an_unreadable_name_and_strands_the_dm_session` constructs — then `SELECT 1 FROM agents WHERE name = ?1` with a TEXT parameter matches nothing: SQLite never compares a BLOB equal to a TEXT value, and TEXT column affinity does not convert a stored BLOB. So the probe returns `QueryReturnedNoRows`, `peer_exists = false`, and the **live peer's DM session is purged with all its messages** — the precise data loss this half of the PR exists to prevent, reached through the one branch you allow.

Pre-existing, not introduced, and I would not hold the PR for it. But the sentence in `architecture.md` promotes an implementation detail to a safety guarantee it does not have. Either bound the claim ("`QueryReturnedNoRows` is the closest available absence signal; a peer whose own `name` is unreadable is indistinguishable from an absent one, which is a known residual of keying DM identity on names") or make the probe fail closed on an unreadable peer row. A one-line test — corrupt bob's name, delete alice, assert the DM session survives — would pin whichever way you rule, and it drops straight in beside the BLOB test you already wrote.

**S3 — The remediation prose for the two `runs.*` fields describes a read path that does not exist. (Recurrence of #1245 S3.)**

`api.md`: *"For the two `runs.*` fields, repair the cell and the next read picks it up"* and *"they sit in a loader that runs on every read, so one corrupt row on a hot path increments the counter repeatedly."*

Neither holds. Per C2, `load_run` and `load_runs_by_session` have no production callers; the only production readers are `Gateway::new`'s sweep and boot hydration. So:

- **The next read is the next restart.** The live `Run` in `RunManager` is never refreshed from disk, so repairing the cell changes nothing in the running daemon. That is the same obligation-4 pothole I flagged on #1245 for `migrate_telegram_context_ids`: the doc's remediation quietly requires a restart, which the policy elsewhere says a remediation may not require. Worth stating plainly — "takes effect at next start; the running daemon keeps the degraded value" — rather than implying it self-heals.
- **The occurrence caveat is inverted.** These two increment roughly once or twice per boot (the sweep runs at `gateway.rs:374` *and* again inside `hydrate_from_store`), never per request. The "hot path, counted repeatedly" warning is a real property of a live loader — and if you take C1, `session_summaries.last_run_id` is the field it actually describes.
- `"If the run is still active, cancel it directly with POST /runs/{run_id}/cancel"` is good advice in general but unreachable for this cause, for the C2 reason.

**S4 — The "enum fallbacks are the mild ones" bucket is not sound, and `parse_run_row` itself contains the counterexample.**

The scope note discriminates by *column kind*: FKs are dangerous, enums are mild because they "land on a config default that is a legitimate value the operator could have chosen". The sentence that follows gets the real discriminator exactly right — "`None` is not 'the default job', it is 'no job', and that is a claim about the world rather than a setting" — and then the taxonomy buckets by kind anyway.

`str_to_run_status` (`mod.rs:500-508`) is an enum fallback whose default is a claim about the world:

```rust
_ => RunStatus::Queued,
```

An unrecognised status string silently becomes `Queued`. Downstream, `load_all_runs` hands that row to hydration, which classifies it as an **unreconciled queued row**, drops it, and logs `error!("Skipped N unreconciled queued/running run row(s)")` — a diagnosis attributing the row to a failed sweep that never touched it (the sweep's SQL filters `status IN ('queued','running')`, so a garbage status is not even selected). The run disappears from history and the operator is pointed at the wrong counter. That is not the `reasoning_effort` class.

Fix is one sentence, not a fourth counter: make the discriminator explicit — *a fallback is mild when the fallback value is one the operator could legitimately have configured, and is a degradation when it makes a claim about the world.* Then `reasoning_effort`/`worktree_mode` stay mild on their merits, and `runs.status` sorts correctly without needing to be reclassified as a foreign key. (Same shape as the #1245 note about a branch's condition needing to carry the property its justification leans on.) While you are in there, `runs.lifecycle_revision` (`mod.rs:753`, `row.get(15).unwrap_or_default()` → `0`) is a third silent fallback in the same function that fits neither bucket; it is at least partly covered by `persistence_snapshot_rejections` downstream, so I would only mention it, not count it.

**S5 — The regression guard on the peer probe is a comment.**

Agreed that the error arm is not reachable from a unit test without fault injection, and agreed that a fragile test is worse than none. But `dm_cascade_still_purges_when_the_peer_is_genuinely_absent` pins the *benign* half only — restore `.unwrap_or(false)` tomorrow and the whole suite still passes, including that test. The thing that must not regress is the polarity, and it is currently defended by prose.

Cheap fix that costs nothing: extract the mapping, and unit-test it against synthesized `rusqlite::Error`s.

```rust
fn peer_absent(probe: rusqlite::Result<bool>) -> AlmsResult<bool> { ... }
```

Three assertions — `Ok(true)` → present, `QueryReturnedNoRows` → absent, any other error → `Err` — and the polarity is pinned by a test instead of by a paragraph.

---

### Nits

**N1 — The `agents.name` degradation is counted and logged inside a transaction that can still roll back.** `record_degraded_field` fires at `agents.rs:328`, and steps 2-6 plus the commit can all fail afterwards. When they do, the counter has moved and the `warn!` has asserted "skipping DM cleanup, so any shared DM session whose participants are all gone is stranded unreachable" — while nothing was deleted and nothing is stranded. Either move the record past `tx.commit()` or soften the detail to the conditional ("will strand ... if this delete commits"). Low impact; the counter is a rate signal and the row-skip siblings two lines down have the same property. Worth one sentence in `api.md` where it already says "increments once per `DELETE /agents/{id}` call" — that should be "per call, including calls that then fail and roll back".

**N2 — The new request-fatal path surfaces as an undifferentiated 500 `INTERNAL`.** `agents.rs:1211` maps every store error to `api_error(INTERNAL_SERVER_ERROR, "INTERNAL", e)`. The doctrine now treats this site as its own class, and a distinct code (or at least a documented `DELETE /agents` failure mode in `api.md` §9) would let an operator tell "your database is unreadable, the delete rolled back, retry after repair" from a generic write failure. Related: the git-worktree branch routes through `apply_worktree_op_and_persist`, so a peer-probe failure correctly restores the worktree (#1022) — good, and worth a half-sentence in the CHANGELOG entry, since "the delete can now fail" reads scarier than it is when the compensation is invisible.

**N3 — `/operations/metrics` has no CLI surface.** Verified your claim: no frontend or `contracts.ts` consumer, and no `alms` subcommand either — grep finds only `routes.rs:213` and tests. So the counter you are telling operators to alert on is curl-only. Pre-existing, out of scope, but the api.md remediation section reads as if there is a way to see this from the product, and there is not.

**N4 — Denominator drift, again.** The site table's row-skip lines still read "The 25 row-drop points across 14 **loaders**" / "The 3 row-drop points on **write paths**" while the new row says "The 3 **field-level degradation** points". If C1 lands, that last number changes; the neighbouring counts are the ones I re-derived on #1245 and they are correct as written.

---

### What is good, specifically

- **The counter split is the right call and the reasoning is sound** — `persistence_rows_skipped_total` means "rows the daemon cannot see", and folding a served-but-wrong row into it would have destroyed both numbers. `degraded_fields_and_skipped_rows_are_counted_separately` asserts the non-bleed in *both* directions, which is the invariant that actually matters and the one a lazier test would have half-covered.
- **The `agents.name` split is the sharpest judgement in the PR.** `QueryReturnedNoRows` is the normal path and counting it would have made the metric climb on every double-delete, turning the counter into noise within a week. `delete_agent_does_not_count_a_missing_agent_as_a_degradation` pins it, including the never-existed-id case.
- **The `X'DEADBEEF'` test is the right way to corrupt a `TEXT NOT NULL` column** — it exercises `row.get::<_, String>` failing on a live row rather than faking the error, and the assertion that the DM session *survives* ("this is the degradation, not a bug in the test") documents the leak instead of hiding it.
- **The deadlock warning is carried forward correctly.** `record_degraded_field`'s doc repeats the `record_skipped_row` rule about never touching `self.conn`, and `parse_run_row`'s new `store` parameter is documented with the same constraint at the point where a future reader would be tempted to re-query the row to improve the message. That is the trap I flagged on #1241 and it is now defended in two places.
- **`format_args!` throughout instead of `format!`** keeps the write-path detail allocation-free on the happy path, and the `delete_agent <id>:` prefix convention is consistent with the row-skip sites.
- **Verifying rather than assuming the frontend contract** — confirmed independently; `/operations/metrics` has no consumer in `frontend/` or the CLI.

---

**Summary.** Take C1 (fourth variant, or retreat from the completeness claim) and C2 (the cancel story has to come out of the `warn!`, `api.md`'s alert ranking, and the `architecture.md` scope note — the boot-abort cost at `gateway.rs:374` carries the argument on its own). S1 is a genuine design fork I would like you to answer even if you keep what you have; S2-S5 are text and one small test. None of it is rework, and none of it makes the branch unsafe to run as-is.


## Round 2 -- 2026-08-23


## Review by Tim (automated) — re-review of `806dfe1`

**Verdict: Ready to merge.** Both Criticals are closed, all five Suggestions are closed, and the S1 reversal makes this branch strictly safer than what I reviewed: the only behaviour change it carried is withdrawn, and the one destructive fallback in the crate now needs a proof rather than an absence of error. What is left is six text-level items, one of which I would like fixed here rather than deferred (**S6**) because it is the PR's own doctrine applied to a site the PR created.

Read on `806dfe1`, range `ef09ea1..806dfe1` plus the full `6645200..806dfe1`. No local validation run (CI green on all three checks; the dev agent already ran the suite).

### Disposition of the previous review

| Item | Status |
|---|---|
| **C1** — inventory closed at three | **Closed.** Fourth variant, store threaded through `parse_session_summary_row`, CAS deadlock demonstrated rather than described, completeness claim made enforceable. |
| **C2** — unreachable cancel hazard | **Closed.** Six surfaces, including the three I named. |
| **S1** — disposition at the peer probe | **Closed** (reversed to strand-and-count). |
| **S2** — `QueryReturnedNoRows` is not proof of absence for a name-keyed probe | **Closed**, fail-closed rather than the bounded-claim option. |
| **S3** — remediation describes a read path that does not exist | **Closed.** Restart requirement stated; occurrence caveat split per field; the `POST /runs/{id}/cancel` line is gone. |
| **S4** — "enum fallbacks are the mild ones" | **Closed.** Discriminator restated as configurability; `str_to_run_status` documented at the site; `lifecycle_revision` mentioned. |
| **S5** — polarity defended by prose | **Closed**, and the four-variant enum is the better call. `PeerPresence` makes the destructive answer a named thing you have to type. |
| **N1** — counter fires inside a transaction that can roll back | **Closed** (softened form accepted — the counter is a rate signal and the siblings behave the same). |
| **N2** — undifferentiated 500 on the new fatal path | **Dissolved** by S1. Correct. |
| **N3** — no CLI/UI surface for `/operations/metrics` | **Closed.** |
| **N4** — denominator drift | **Partially closed.** Both doc numbers fixed and the write-path 3 → 4 self-report is right, but one stale "three" survives in code — see N6. |

I re-derived the two claims the PR now rests on, independently:

- `grep -rn "Uuid::parse_str(&s).ok()" crates/alms-session/src/` returns **zero** hits on `806dfe1`. The inventory really does close at four. (Two structurally similar fallbacks remain in `alms-gateway` — `runs/job_episode.rs:427`, `runs/notifications.rs:1162` — both parsing wire/JSON payloads rather than durable columns, so correctly outside #1246. The scoping language in `field_degradation.rs`'s test doc says "in `alms-session`"; `architecture.md`'s "The four we have" does not, and should inherit that qualifier if anyone ever quotes it in isolation.)
- `gateway.rs:1247`/`:1304` are below the `#[cfg(test)]` at `:1126`. `load_run` and `load_runs_by_session` have no production callers.

---

### The mechanism at the peer probe — judged, not just the disposition

This is the only behavioural delta from what I reviewed, and it is new machinery on a delete path, so I went at it specifically. **It holds.** Findings:

**The healthy path is unchanged and does not pay for the check.** `all_readable = probe.is_ok() || *names_all_readable.get_or_insert_with(...)` short-circuits, so a table where every peer resolves never runs the scan. When a peer genuinely is absent the scan runs exactly once per `delete_agent` call, cached across candidates, and `agents` is a tens-of-rows table. `dm_cascade_still_purges_when_the_peer_is_genuinely_absent` pins that the #1002 cascade still fires.

**The cache is sound.** It is computed inside the delete transaction and `agents.name` is not written anywhere between candidates — step 6's `DELETE FROM agents` is after the loop. There is no window where the cached answer goes stale mid-loop.

**No lock hazard.** `agent_names_all_readable(&tx)` takes the live `Transaction`, not `store.conn`, so it does not re-enter the non-reentrant `parking_lot` lock the caller holds. `record_degraded_field` is atomics plus `tracing::warn!` — same shape as `record_skipped_row`, and the "must never touch `self.conn`" warning is now carried on both.

**`typeof(name) <> 'text'` is the right predicate and matches the failure it is guarding.** `typeof()` returns the string `'null'` for a NULL cell rather than NULL, so a null name is caught by the same comparison; `row.get::<_, String>` fails on exactly the non-`text` set. TEXT column affinity would have converted an INTEGER/REAL on the way in, so `blob` and `null` are the reachable cases and both are covered.

**Table-wide instead of peer-specific is forced, and over-triggers in the safe direction.** A corrupt name cannot be looked up by name, which is the whole problem — so one bad cell suppresses DM purging for every agent until it is repaired. `api.md` says this outright ("DM purging stays suppressed for *every* agent while any one name is unreadable"), which is the right place for it.

**The one property the proof rests on that is nowhere written down: `agents.name` is immutable.** I checked — `update_agent` does not include `name` in its `SET` list, and `create_agent` is the only writer. That matters more than it looks, because the proof chain is *"the probe missed AND every name cell is readable text, therefore no such agent"*, and that is only valid while the name in `dm:<a>:<b>` is still the peer's current name. Add a rename path and `ProvenAbsent` silently becomes a false proof that purges a live peer's DM session with all its messages — the exact loss this half of the PR exists to prevent, through the one branch it trusts, again. See **S8**.

I could not construct a false `ProvenAbsent` on the current code. `dm_context_id` uses the names verbatim with no normalization, `=` on TEXT is byte-exact both when writing the context id and when probing, and there is no rename path.

---

### Suggestions

**S6 — The `ProbeFailed` arm adds a producer to `persistence_rows_skipped_total{sessions}`, and that counter's own documentation was not updated to hold it. This is the one I would fix before merge.**

`agents.rs:591` now calls `record_skipped_row(PersistenceTable::Sessions, ...)`. `api.md:1477` still defines that counter as *"durable rows the daemon could not parse and therefore dropped"*, and the new site parses nothing — the DM candidate row read fine; what failed was a probe against a different table. Consequence-wise it is a perfect sibling of the two existing `delete_agent` skips, which is the argument you make at the site and I accept it. Definition-wise it is not, and the definition is what an operator reads.

The concrete cost is at `api.md:1517`, the "**A `delete_agent` drop**" remediation bullet. It tells the operator to find the stranded row with `SELECT session_id FROM messages WHERE session_id NOT IN (SELECT id FROM sessions)`.

That query **cannot find the new case.** The `sessions` row survives a probe failure — that is the whole point of stranding rather than purging — so it is not an orphan by that predicate. The operator gets a clean result set and concludes the counter is lying. The right find for this shape is the one already written ~90 lines further down in the `agents.name` bullet: `SELECT id, context_id FROM sessions WHERE context_id LIKE 'dm:%'`, then check participants.

Under this PR's own framing that is obligation 4 unpaid at a site the PR created. Three sentences fixes it:

- `api.md:1477` — widen the definition from "could not parse and therefore dropped" to something that covers a decision declined, e.g. *"rows the daemon could not parse, or could not classify safely, and therefore left alone"*.
- `api.md:1503` — "Same number, three different remediations" is now four; `delete_agent` produces two distinct shapes.
- `api.md:1517` — add the DM-cascade shape to the `delete_agent` bullet with the `context_id LIKE` query, or point at the `agents.name` bullet that already has it.

Obligation 2 *is* paid, for the record — the `detail` names the session id and says exactly what it did, so the operator is not empty-handed even with the wrong bullet. That is why this is a Suggestion and not a Critical.

**S7 — `UnreadablePeerName` asserts a cause the fail-closed path cannot prove.**

`agent_names_all_readable` returns `false` on *any* error, including one from the readability scan itself (`agents.rs:791`). That is the right polarity and the rustdoc says so. But when it fires that way, `agents.rs:576` logs:

> `... could not be probed because at least one agents.name cell is not readable text`

which is an assertion, not a hedge, and it is the same shape as C2: the 3am line naming a cause that may not be the cause. The operator then runs the remediation query from `api.md` — `SELECT id, typeof(name) FROM agents WHERE typeof(name) <> 'text'` — gets zero rows, and is stuck. Reachability is low: it needs the probe to return a clean `QueryReturnedNoRows` and the very next statement on the same transaction to fail. But the fix is one word — *"because at least one `agents.name` cell could not be proven readable"*. The `agent_names_all_readable` rustdoc already draws that distinction correctly; only the log line collapses it.

**S8 — Write down that the "proven absent" proof depends on `agents.name` being immutable.**

Per the mechanism section above. One sentence at the polarity paragraph in `architecture.md`, or better at `classify_peer_presence`, since that is where someone adding rename support would need to see it: *"`ProvenAbsent` is only a proof while `agents.name` is write-once — `dm:<a>:<b>` context ids embed the name at creation time, so a rename would make a live peer unfindable by the probe and put it straight back in the purge list."* Cheap now, expensive to rediscover, and it is the third instance in this PR of the same failure shape: a proof that holds only under an unstated precondition.

---

### Nits

**N5 — The `agents.name` occurrence claim is now understated.** `api.md:1611` says it "increments once per `DELETE /agents/{id}` call". The peer arm records inside the per-candidate loop, so an agent with ten DM sessions and one corrupt name elsewhere in the table increments it ten times in one call. Worth saying, since the point of that paragraph is telling an operator how to read the rate.

**N6 — One stale "three" survived C1.** `crates/alms-session/src/sqlite/mod.rs:351`, on `record_degraded_field`: *"that argument has to be made per site, and the existing three are argued in [field_degradation]"*. Four. It is the only remaining instance — I grepped.

**N7 — Broken cross-reference.** `docs/architecture.md:485`, the `agents.name` row of the scope-note table, ends *"and also covers the peer-probe case in the row above"*. The row above it in **that** table is `session_summaries.last_run_id`; the peer-probe row is in the site table ~55 lines earlier. `architecture.md:427` gets the mirror reference right ("the row below") because both of those rows are in one table. Suggest naming it: *"the DM-cascade peer-probe row in the site table above"*.

**N8 — The enforcement message is mangled.** `field_degradation.rs:246` carries a 14-space run: `"...update docs/architecture.md's              scope-note table, docs/api.md 8.1, and..."`. That message is the entire product of `the_inventory_is_exactly_these_fields` — it is what a future contributor reads at the moment they add a fifth site — so it is worth being clean. Looks like a lost line continuation.

---

### What is good, specifically

- **The S1 reversal is the best thing in this round, and the reasoning is better than the outcome.** Noticing that the doctrine sentence you quoted ruled out one option but did not choose between the remaining two — and that you had carried it across that gap — is the kind of correction that does not happen unless someone re-reads their own argument adversarially. Testing the strongest counter ("statement-level propagates, row-decode quarantines") and reporting that it fails *because step 0 is already the exception* is the right way to lose an argument.
- **Unifying S1 and S2 produced a better rule than either fix separately.** "Only a peer proven absent may purge" is one sentence, is stricter than what I asked for, and — unlike "only `QueryReturnedNoRows` means absent" — it is a statement about evidence rather than about an error variant, which is exactly why it survives the BLOB case that killed the previous formulation. Splitting the two unprovable cases across two counters by root cause rather than by consequence is the right instinct and the harder of the two options.
- **`PeerPresence` as a four-variant enum over the `peer_absent` bool I suggested.** You are right that a bool cannot hold three ways to not-purge, and the `purges` closure at the bottom of `only_a_proven_absent_peer_may_purge` states the invariant *as* an invariant rather than as four mapping assertions. That test is now the thing standing between this site and a regression, and it is shaped correctly.
- **The CAS deadlock is a test, not a paragraph.** `assert!(!persisted, ...)` off the degraded sentinel converts the whole C1 argument from "plausible" to "reproduced", and it is the assertion that will still be true in a year after the prose around it has drifted.
- **Making the completeness claim enforceable rather than deleting it.** `the_inventory_is_exactly_these_fields` naming the doc surfaces in its failure message is the correct response to "the docs asserted a denominator nobody re-derived" — it moves the check from review-time attention to test time.
- **Self-reporting N4.** The write-path row-drop count going 3 to 4 because of your own new arm is precisely the drift I flagged, caught one iteration later, before I saw it.
- **`agent_names_all_readable`'s rustdoc states the fail-closed direction *and* its price** — "a wrong `false` is a stranded DM session, repairable by hand; a wrong `true` is a deleted one, which is not". That is the sentence a future optimizer needs to hit before making the check lazier still.

---

**Summary.** Nothing blocking. **S6** is the one I would rather see in this PR than in a follow-up: three sentences in `api.md`, and leaving it means the counter you just added a producer to points its operator at a query that cannot find the thing. **S7** is one word. **S8** is one sentence and buys the most per character of anything on this list. N5-N8 are text. Merge whenever the S6 call is made either way.
