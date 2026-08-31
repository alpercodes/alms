# Interrogating a design boundary rather than the diff

> Reconstructed from the review history of the ALMS project's original issue tracker.
> Cross-references like `#1246` point at that tracker and are kept verbatim for provenance.

**Source:** PR #1267 -- fix(dm): an interrupted DM-ended notification is delivered as a marker, not a run (#1258)  
**Rounds:** 2  


Instead of checking the change line by line, the review asks whether the concluded/interrupted boundary is drawn in the right place at all, then tests that claim against every other code path that routes through the same reasons.

---


## Round 1 -- 2026-08-25


## Review by Tim (automated)

Re-review of `96f5f33` (`aad3964` + `96f5f33` on top of the tree I reviewed).

**Verdict: Ready to merge.** Everything I flagged is either fixed or recorded as an explicit decision. More importantly, the one item I pushed hardest on — the `Errored` narrowing — came back with a counter-argument that is **correct**, and my proposed remedy was **wrong** in a way that would have regressed the bug this PR exists to fix. I re-derived it from the code rather than taking it; details below, because the reasoning matters more than the verdict.

Two nits worth a follow-up commit if you touch this again, neither blocking: a stale `sse.rs` doc claim that is the exact sentence S3 corrected elsewhere, and a comment cross-reference in `lifecycle.rs` that imports a justification which does not hold at its site.

---

## The crux: my predicate was wrong, this axis is right

I proposed narrowing with `conversation_history.is_none()`. I checked the rebuttal against the code and it holds, in both halves.

**1. The predicate is a constant.** `MessageBus::end_conversation` ("S2: Validate that the DM session exists") returns early when `session_manager.get(session_id).is_err()`, and the session only exists because `MessageBus::send` persisted a `Role::User` text message into it. `format_dm_conversation_history` filters only `dm_filter::is_synthetic_marker` (empty text / non-text / markers), so the initiating message alone makes the transcript non-empty. In `run_trigger_loop` the only ways `conversation_history` comes back `None` are a registry miss on `peer_name_resolved` or a `get_history` error — degenerate paths, not the ones I was aiming at. So on every real end my predicate collapses to "suppress `UserCancelled` only".

**2. That is not an edge case, it is the reported incident.** `self_notification: true` is set exclusively on the *sender* trigger in `end_conversation`, where "sender" is the agent that called it — and `handle_dm_run_failure` is called by the agent whose run died. So "**Your** DM conversation with agent scout ended" means bimbam ended it, bimbam's run 429'd, and bimbam only had a run in `dm:bimbam:scout` because scout had already sent into that session. Transcript non-empty. My predicate would have let the reported run straight through — I would have "fixed" #1258 by not fixing it. Good that this was checked before implementing rather than after.

The generalisable statement is the one now in `run_trigger_loop`'s rustdoc: **a DM cannot end at all without at least one message in its session**, so "is the transcript empty" is not a predicate, it is a constant. I should have derived that instead of reasoning from the incident narrative.

**Is `interrupted` the right axis?** Yes, and I could not find a third one that beats it. The property I actually cared about — "is there content the operator's web-chat never saw" — is genuinely unavailable as a runtime signal, because the DM session always has content and nothing tracks which of it already reached the initiator's chat. "Did a run reach the end of its turn" is the closest available proxy that is (a) decidable at every construction site, (b) local to the site that knows the answer, and (c) not a timing heuristic. I walked all nine construction sites and agree with every classification.

**What the axis costs, stated plainly.** It is not free, and the PR should be merged with this understood: a DM that exchanged three real turns and then died on the fourth is `interrupted: true`, so those turns never reach the agent's context — the same shape as the loss I objected to, confined now to DMs whose *last* turn died rather than to all `errored` ends. The rustdoc's "Consequence: an interrupted end is invisible to the agent" section and hazard 8 both cover it, and `user_cancelled`'s "the transcript is not destroyed — the DM view still renders it" applies equally. That is the right disposition for a cancel (the operator said stop) and a defensible one for a died run (the retry would likely hit the same 429). Flagging it only so nobody later reads "interrupted implies nothing was lost".

**The test that pins the negative.** `an_interrupted_dm_end_still_has_a_transcript` asserts exactly one thing — `send` then `end_conversation` leaves a non-empty formatted transcript containing the initiating message — and that thing is true. It does not overclaim, and its doc comment states the mechanism (session-must-exist implies at least one `send`) rather than the incident anecdote, which is the durable half. It is a correct guard, not a misleading one. Keep it.

---

## Residual worth understanding (not a change request)

The fix covers the reported cause but not the reported *shape* in full. `dm_lifecycle` Exit 3 — a DM run that produces no deliverable reply — is `interrupted: false` and still creates a run on the trigger's own target. So "operator cancels a web-chat run, and half a second later an unrequested run appears on that same session" stays reachable, via a DM run that completes emptily rather than one that dies. That is the trade my own review argued for, so I own it: relaying a real transcript is worth the turn, and Exit 3 is the case where a transcript most reliably exists. Recording it because the issue's headline complaint is about the shape, and the shape is narrowed, not eliminated.

---

## Verified individually

**#1206 scoping restore (S4).** Correct, and it does the work claimed. The extra trigger runs through the same `state` with `operator_cancelled_jobs` still populated, targets `notifications:bob` (non-job), and uses `Ignored` — and the choice of a *concluded* reason is load-bearing exactly as noted, since an interrupted one would be suppressed by #1258 regardless of scoping and the guard would be vacuous a second time. The doc comment now explains both what flipped and why the old carrier can no longer carry it. M8 killing that one assertion and nothing else matches what I found by inspection: the guard was fully unpinned.

**S3, unsanitised `Errored` tails.** Fixed properly. `peer_error_with_prefix` wraps the foreign tail in `AlmsError::Runtime` and routes it through `sanitize_error_for_session`, whose `Runtime` arm returns a **fixed literal in every branch** — so nothing from the tail can survive, regardless of which keyword it happens to match. That is a stronger guarantee than "truncated", and it is the right call not to route the prefix through it. Both sites covered; the panic path correctly keeps the raw text for its own `run_error` SSE (the operator owns that run) and sends only the sanitised copy to the peer.

**Backward compatibility.** `ConversationEndReason` derives `Serialize`/`Deserialize`, and so does `RunTrigger` — so adding a required `interrupted` field to a variant *would* be breaking if either were ever read back from storage. I grepped for deserialisation sites and found none: both are in-process only, carried over an `mpsc` channel. Safe today. Given #1230's direction, worth remembering: **if a durable trigger queue ever lands, `interrupted` needs `#[serde(default)]`** or old rows fail to parse. Not a change request now.

**Agent-awareness: documenting was the right call.** The alternative (`persist_error_marker` with `kind: "error"`) is not a free win — it survives the strip pass by being rewritten into an `[Error] ...` *user message*, so it lands in the operator's visible chat context on the next turn. Adding a visible artefact to the operator's chat is precisely the surface #1258 complains about, so trading one for the other needs a UX decision, not a reviewer's preference. Recorded in three places with the escape hatch named is the correct disposition.

**Nits 1 and 2, and the blocking doc comment.** All fixed and accurate. The `notify_dm_ended_to_webchat` rustdoc now gives the correct reason (job sessions are internal, hence never the marker target) and additionally documents the no-user-facing-session gap I raised in the same paragraph, which is more than I asked for.

**Docs.** `docs/api.md` section 6's `detail` paragraph matches the wire (`Option<String>` + `skip_serializing_if`, set only by `dm_conversation_ended_webchat`, only from `reason.detail()` which is `Some` only for `Errored`). Hazard 8 in `dm-run-lifecycle.md` is the right home for the authoritative version and states the "predicate is not the transcript" result correctly. `layer2-peer-messaging-design.md:309` no longer asserts the now-false "the peer receives a one-shot notification run", and `:838` documents the field. The CHANGELOG entry is operator-facing and names the consequence.

---

## Mutation table

The re-derivation is what I asked for and the numbers are now credible. M2 at 13 and M6b at 13 are both in the range inspection suggests, and the three I missed are fair — `conversation_ended_for_peer_does_not_touch_episode` and `test_conversation_ended_no_reroute_when_source_session_none` both assert on a created run without saying so in their names, which is exactly what an inspection pass skips. Point taken that my own estimate was low.

**M9 reported as survived rather than quietly dropped is the right instinct**, and extracting `peer_error_with_prefix` so the *mechanism* is killable (M9c) is a reasonable partial. But the gap is **solvable and cheaper than stated**, and the stated cause conflates two different things:

- "The path is not drivable without a fault-injection seam" — largely true. `SendError::Internal` at `dm_lifecycle.rs:305` is reachable via a full trigger channel, but `end_conversation` reserves its own trigger permits up front, so the same saturation kills the assertion carrier. (There *is* a non-injection seam: make the registry resolve the peer name to the run's own `agent_id` and `MessageBus::send`'s first check returns `SelfMessage` with the channel untouched. But `end_conversation` then needs a live `depths` entry for that pair, which the setup cannot produce cleanly — so it is not obviously better than what you have.)
- "Therefore the site's use of the helper cannot be pinned" — this does not follow. M9 mutates a *composition* (`peer_error_with_prefix(prefix, &e.to_string())` becoming `format!("{prefix}{e}")`), and a composition is killable wherever it has a name. Lift it one level — `fn dm_delivery_failure_reason(e: &SendError) -> ConversationEndReason` next to the call site — and a plain unit test asserting that its `.detail()` is `Some("reply delivery failed: Runtime error")` for a `SendError::Internal` carrying a database path kills M9 without driving the path at all. Same for the panic site's `peer_failure_message`. That is the identical move that produced `peer_error_with_prefix`; it just stopped one level short of the sites.

Not blocking. But "unpinnable without fault injection" is a claim that gets inherited, and I do not think it is true here.

---

## Nits

1. **`sse.rs:1283-1288` still carries the claim S3 removed.** The `DmConversationEndedData::detail` doc says *"already bounded at 300 chars by `PEER_ERROR_MESSAGE_MAX_LEN`"*. That is the exact sentence `ConversationEndReason::detail()` was corrected away from — `peer_error_with_prefix` puts a self-authored prefix *outside* the truncator, so the field is sanitised and roughly bounded but can exceed 300. Fixed in one place, left standing in the other, and this one sits on the type that faces the wire. Suggest matching the corrected wording: *"sanitised via `sanitize_error_for_session` and bounded at `PEER_ERROR_MESSAGE_MAX_LEN`, apart from a short self-authored prefix"*.

2. **`lifecycle.rs:2842` imports a rationale that is false at its site.** The comment reads *"`interrupted: true` either way — see the `Cancelled` arm below"*, and the `Cancelled` arm's stated reason is *"the run was cancelled, so no turn of this DM completed"*. In this arm the loop returned **`Ok`** — a turn *did* complete; what happened is that a cancel or shutdown won the state-flip race, so `handle_dm_run_completion` was skipped and the produced reply was never delivered. `interrupted: true` is still the right answer (the operator asked for stop, and the reply never reached the transcript), but the reason given is not the reason that applies. This is the kind of cross-reference that gets copied into the next arm someone adds. Suggest the local justification instead: *"`interrupted: true` — a cancel/shutdown won the race, so the completed turn's reply was never delivered and the operator asked for work here to stop."*

3. **`SendError` deserves a typed label, not `AlmsError::Runtime`.** `peer_error_with_prefix` at `dm_lifecycle.rs:322` wraps `e.to_string()` in `Runtime`, and every `SendError` variant misses all of the sanitiser's keywords, so the banner detail at that site is always literally `"reply delivery failed: Runtime error"`. Safe, but it discards information that was never sensitive: the *variant* of `SendError` is a safe discriminant (`RecipientNotFound`, `SelfMessage`, `Internal`); only `Internal`'s payload is not. Matching on the variant would give the operator something actionable at identical safety. Low value while that site keeps its run (the run narrates it anyway) — file it rather than do it.

4. **`integration_tests.rs:13634`** — the closing brace of `cancel_job_teardown_leaves_no_runs_for_the_job` and the next item's doc comment are adjacent with no blank line; `cargo fmt` does not insert one. Cosmetic.

---

**What changed my mind:** the transcript predicate was the wrong lever and I was wrong to press for it. The counter-argument is correct on both the code and the incident, the alternative axis is the best available, and the test recording the negative result is honestly scoped. What is left is two doc corrections and a suggestion about M9 — nothing that should hold the merge.


## Round 2 -- 2026-08-25


## Review by Tim (automated)

**Verdict: Needs minor fixes.** The design call is sound and the implementation is careful — the reason-class cut is the right *shape* of predicate, the episode carve-out is correctly ordered, and the wire-compat claim holds up under inspection. What needs fixing before merge is small: a new field on a documented SSE event that `docs/api.md` doesn't mention, a test doc-comment that now contradicts its own assertions, and two rustdoc claims that are wrong in ways a future reader would act on. Everything else below is a follow-up or a design note I want recorded, not a blocker.

---

## Critical

Nothing. No leaked state, no unbalanced error path, no widened boundary. `enqueue_triggered_run` is the only thing skipped for an interrupted end, and with `job_id == None` it has no side effects beyond the queue reservation and run creation — so suppressing it releases nothing and strands nothing. The phase-clear SSE stays unconditional, so no web-chat is left stuck on "Chatting with {peer}".

---

## The central design claim: is the concluded/interrupted boundary in the right place?

**Right shape, slightly too wide on one edge.** The property that actually makes the notification run load-bearing is *"is there transcript content that never reached the web-chat"*. `reason` is a proxy for that property, and it is a clean proxy for three of the four variants:

- `Ignored` / `DepthExceeded` → the DM ran its course, there is always content. Keep the run. Agreed.
- `UserCancelled` → the operator said *stop doing things here*. Even if content exists, relaying it against an explicit stop is the wrong default. Agreed, and widening past `errored` was the right call for the reason given — `lifecycle.rs:2817/2930/3017` all map an in-flight cancel to `UserCancelled`, so an `errored`-only fix would have missed the headline complaint. Confirmed by inspection.
- `Errored` → **not a clean proxy.** It covers two materially different situations.

The one that concerns me is `handle_dm_run_completion`'s Exit 3 (`dm_lifecycle.rs:136`), which ends the conversation with `Errored { message: "agent run completed without producing a reply" }`.

That is a run that *completed*. It just had nothing deliverable on its **last** turn — after possibly several delivered turns. `format_dm_ended_notification` (`notifications.rs:1602`) applies the history template for **every** reason, `Errored` included, so pre-#1258 that transcript did reach the operator's chat. Post-#1258 it does not, and the peer's replies live only in the DM session. That is precisely the "spurious spinner traded for a silently dropped answer" outcome that blanket option 3 was rejected to avoid — it just now happens inside the `Errored` class instead of the `Ignored` one. `dm_lifecycle.rs:305` (`reply delivery failed: {e}`) has the same shape: a real exchange, then a failure on the last hop.

The hook to narrow it is already in scope, one line away — `conversation_history` is computed in the same match arm. Something of the shape `reason.is_interrupted() && (matches!(reason, UserCancelled) || conversation_history.is_none())` keeps the reported incident suppressed (429 on the first DM turn → no history) and keeps `UserCancelled` unconditional (the operator said stop), while restoring the relay when the DM actually produced something.

I am **not** asking for that in this PR — it is a UX decision and belongs to whoever owns that call. But I would like the decision recorded, because "an interrupted DM has no outcome to relay" is currently asserted as a fact in three places (PR body, `is_interrupted` rustdoc, `run_trigger_loop` rustdoc) and it is not one. If the answer is "acceptable, the operator can open the DM view", say that in the rustdoc instead of the stronger claim.

### Does anything else route through those reasons?

I enumerated every construction site: `bus.rs:299` (DepthExceeded), `jobs.rs:315` (teardown → UserCancelled), `dm_lifecycle.rs:136/162/305/440`, `lifecycle.rs:2817/2930/3017/3138/3237/3398`. Nothing surprising, and no consumer keys on the reason in a way this change breaks — `format_dm_ended_notification` and the frontend `DM_END_REASON_LABELS` both already handled all four.

### A consequence worth recording: nobody tells the agent any more

This is the part I most want written down, because it is invisible from the diff:

- The `dm_ended_notification` marker is `Role::System` + `synthetic: true`, so `strip_mid_history_system_markers` removes it before the provider. `markers.rs`'s own module docs say so, and then say *"The agent still receives the relevant payload via the `notification_input` user message that `lifecycle.rs` pre-persists alongside the marker"* — which for an interrupted end no longer exists.
- The bus's `dm_ended` record is `Content::Text(String::new())` (`bus.rs:609`), and `dm_filter::is_synthetic_marker` filters empty-text messages, so `read_messages` / `read_session` never surface it either.

Net: after an interrupted end there is **no agent-visible signal anywhere**. The operator is told; the agent is not. Ask it "so what did scout say?" and it has no idea the conversation ended. That may well be the right trade — the operator is right there and better informed than before — but it means the PR body's *"the bus still writes it to the shared DM session, so neither agent believes the conversation is open"* is true about **bus state** (depth reset, tombstone) and not about agent knowledge. Please reword; that sentence will get quoted back later as if the agents were informed.

If you do want the agent informed without spending a turn, the machinery already exists: `persist_error_marker` (#874) is the documented exception that survives the strip pass and gets rewritten into an `[Error] ...` user message on the next turn. Tagging the `errored` marker `kind: "error"` would cost zero runs. The trade-off is context noise in the operator's chat, so it is a judgement call, not an obvious win.

### One more hole in the no-run path

`notify_dm_ended_to_webchat` early-returns when `find_user_facing_session` is `None`, and `runs/mod.rs:118` treats `dm:` / `notifications:` / `job_` as internal. Pre-#1258 an interrupted end still landed a run on `notifications:{agent}`, so it was at least *in that agent's history*. Now: no run, no marker, and the DM-session record is invisible to readers. That only bites agents with no user-facing chat — which is exactly the background / channel-driven ones. Worth considering a fallback that persists the marker on the trigger's own target when there is no user-facing session, so the end is recorded where the run used to land.

---

## Flagged item 1: does the #1206 guard's intent survive the rewrite?

**Mostly yes, with one thing genuinely unpinned.**

What survives: the primary invariant (`DELETE /jobs` leaves no live/queued run for the job) is still exercised. The `SubagentCompletion(Cancelled)` half still goes through `enqueue_triggered_run` and is still stopped there by `operator_cancelled_job_for_context`, so `job_runs.is_empty()` continues to test #1206 rather than #1258. Delete the #1206 check and that assertion still fails. Good — the rewrite did not hollow out the test.

What no longer holds: the **scoping** half — *"with operator-cancel intent registered, a run targeting a NON-job context is still created."* That is what `!bob_runs.is_empty()` pinned, and the replacement (`dm_ended` marker still written) pins a different property. The two tests named as covering it do not:

- `notification_stays_on_invisible_session_when_no_source` never populates `operator_cancelled_jobs` — there is no job in that test at all. It pins routing (invisible session vs web session), not suppression scope.
- `spent_one_shot_detached_dm_ended_not_suppressed` pins intent-vs-status keying, but again with an **empty** intent set — it proves a *spent* job does not suppress, not that a *cancelled* one does not over-reach.

So today, a regression widening the suppression from "this job's context" to something broader (keyed on agent, or a global flag) would pass the whole suite. The restore is cheap and belongs in the same test, where the intent set is already populated: after the teardown replay, drive one extra `ConversationEnded { reason: Ignored }` targeting `notifications:bob` and assert a run **is** created. ~15 lines, and it puts the guard back in the only state that made it meaningful. Non-blocking, but I would take it now rather than rediscover it.

**Blocking nit in the same test:** the doc comment above `cancel_job_teardown_leaves_no_runs_for_the_job` (`integration_tests.rs:13284`) still ends with "The peer's own ended-notification (not on a job session) must still be created." — directly contradicted by the body's new `bob_runs.is_empty()`. The inline comment was updated; the doc comment was not. That is the sentence a future reader trusts over the assertion. Please fix.

---

## Flagged item 2: is #1168 the same bug?

**No. Agreed it stays open — and I checked rather than took it.**

#1168 is a `run cancelled by user` artefact appearing *inside the DM session* after cancelling an unrelated **queued web-chat** run, with ~80% of subsequent DM messages reported affected. #1258 is an unrequested run appearing *on the web-chat*. Opposite direction, different surface, different code path — and nothing in this diff touches the #1168 path.

The claim about the named suspect is accurate: `notify_dm_peer_of_setup_outcome` gates on `is_peer_message && context_id.starts_with("dm:")` (`dm_lifecycle.rs:461`), and `queued_then_cancelled_non_peer_run_does_not_notify` (`integration_tests.rs:11973`) pins it. So the backend half of #1168's own diagnostic question is already answered in the negative, which leaves the frontend event-routing half — the "does it self-heal on reload?" branch the issue itself calls out — untested. #1168 should stay open **and** be re-scoped toward that half when someone picks it up.

For the record: this PR does not make #1168 harder to reproduce either. A `UserCancelled` DM end now spends no run, but #1168's artefact is written on the DM session by a different path entirely.

---

## Suggestions

**S1 — `docs/api.md` is now behind the wire.** `detail` is a new optional field on the documented `dm_conversation_ended` event (section 6, around line 720) and the doc does not mention it. This is the canonical SSE contract doc, and the `suppress_banner` paragraph right below sets the precedent for exactly this kind of optional-field note. Proposed insert after that paragraph:

> `detail` (optional string, omitted from the wire when absent): the failure text behind an `"errored"` end. Present **only** on the cross-session copy forwarded to an agent's user-facing web-chat, and only for `"errored"`; every DM-session-stream emission and every non-`errored` forward omits it. Since #1258 an interrupted end (`"user_cancelled"` / `"errored"`) starts no notification run, so the banner is the only live surface that explains *why* — clients rendering a "conversation ended" banner should render `detail` as an additional line when present.

**S2 — `docs/dm-run-lifecycle.md` needs the carve-out.** Lines 174 and 208 describe the DM-end notification-run behaviour in detail and are the first place a future reader looks. Neither mentions that an interrupted end now produces no trigger-target run. `docs/layer2-peer-messaging-design.md:309/325/838` has the same gap ("The peer receives a one-shot notification run" — now false for two of four reasons). One sentence in each is enough; I would put the authoritative version in `dm-run-lifecycle.md` and cross-reference it.

**S3 — the `detail()` "already bounded" claim is wrong for two sites.** The rustdoc on `ConversationEndReason::detail()` says the message is *"Already bounded at the construction site (`PEER_ERROR_MESSAGE_MAX_LEN`, 300 chars, in `runs/lifecycle.rs`), so it is safe to embed in an SSE frame and in a persisted marker without re-truncating."* Four sites do go through `truncate_error_for_peer` (`lifecycle.rs:3138`, `3237`, and both `lifecycle_persistence_error_for_peer` paths). Two do not:

- `dm_lifecycle.rs:305` — `Errored { message: format!("reply delivery failed: {e}") }`
- `lifecycle.rs:3398` (panic path) — `Errored { message: failure_message }`, whose persistence branch is `format!("Run panic could not be persisted: {error}")`

Neither is truncated **or** sanitised. `truncate_error_for_peer` exists specifically to route through `sanitize_error_for_session` so raw provider/storage detail (URLs, keys, response bodies, paths) cannot reach the peer's notification context — that is #911 / #930 / #931. This is not a *new* leak: pre-#1258 those same strings went into the notification run's persisted input, a wider exposure than a banner. But #1258 puts them on two surfaces that did not carry them before (the SSE frame to the browser, and the marker's own text) while asserting a guarantee that does not cover them. Either route both sites through `truncate_error_for_peer` — they are one-liners — or soften the rustdoc to say which sites are bounded.

**S4 — restore the #1206 scoping assertion.** See the flagged-item-1 section above.

---

## Mutation table and the wire-compat claim

**Wire-compat: confirmed by inspection.** `DmConversationEndedData.detail` is `Option<String>` with `skip_serializing_if = "Option::is_none"`; the DM-session constructor `dm_conversation_ended` hardcodes `None`; only the web-chat forward can set it, and only from `Errored`. The marker metadata inserts `detail` only when `Some`, so pre-#1258 markers keep their exact shape. `test_dm_conversation_ended_detail_field` pins all three arms including the DM-session one, which is the arm that actually matters. Good test.

(One non-wire format change to be aware of: the marker **text** for `user_cancelled` / `errored` goes from `(user_cancelled)` to `(cancelled by user)`. The UI renders from metadata so it does not care, but anything reading marker text — CLI session dumps, exports — sees the new format from here on. Called out in the PR body, so no action.)

**M2 undercounts the blast radius, again.** M2 (`is_interrupted()` always `true`) is reported as killing `concluded_dm_end_still_starts_its_notification_run`, `concluded_reasons_are_not_interrupted`, and *"2 pre-existing `dm_conversation_ended_*` tests"*. By inspection at least these also assert a concluded end creates a run and would fail:

- `dm_conversation_ended_trigger_creates_notification_and_marker` — `!runs.is_empty()` on the notifications session
- `notification_stays_on_invisible_session_when_no_source` — `!notif_runs.is_empty()`
- `notification_uses_source_session_when_present` — `!runs.is_empty()` on the source session
- `spent_one_shot_detached_dm_ended_not_suppressed` — `!runs.is_empty()`

plus the two `dm_conversation_ended_*` marker tests whose `persist_marker` flips when the run disappears. The mutation is still killed and the conclusion holds — but this is the second table in two PRs where the named set is smaller than the real one (#1265 was 3-named / 5-actual). If the number comes from a name-filtered test run, widen the filter; an undercount reads like the pin is narrower than it is, which is the opposite of what a mutation table is for.

Everything else lines up under inspection: M1's three are exactly the two new interrupted tests plus the teardown test's new `bob_runs.is_empty()`; M3 is well chosen, because `interrupted_dm_end_still_fires_its_job_episode_continuation` is the *only* test that can distinguish the two orderings of the episode override vs the suppression; M4 / M4b / M5 / M6b each land on the assertion named.

The control test (`concluded_dm_end_still_starts_its_notification_run` — same harness, only the reason differs) is the right answer to the vacuous-no-run risk, and `drive_dm_end_on_operator_session` returning all three arms (runs, markers, events), so that a "no run" assertion is forced to also state what the operator *does* get, is a pattern worth reusing.

---

## The sibling hazard: agreed, it is real — file it

Confirmed. `completion_notification_loop` (`notifications.rs:1160`) calls `enqueue_triggered_run` for **every** `SubagentCompletion` including `TaskStatus::Cancelled`, and a cancelled background subagent does produce one (`alms-coordinator/src/lib.rs:1203` — the biased cancel arm yields `TaskStatus::Cancelled`, and the `Err` handler classifies loop-level cancels the same way). The only thing stopping it today is #1206's `operator_cancelled_job_for_context`, which fires **only** when the parent session is an operator-cancelled `job_*` session. A background subagent dispatched from the operator's web-chat and killed by the operator's own cancel cascade still produces a fresh notification run on that web-chat — the exact #1258 shape, from the exact same button.

Right that it did not cause this incident (foreground `invoke_agent` returns as a tool result and emits no completion) and right to keep it out of this PR. When it is filed, note that the fix is **not** symmetric with this one: unlike a DM-ended notification, a subagent completion carries a real outcome (summary, tool count, duration) even when cancelled, and the `subagent_completion` marker + `subagent_completed` SSE are already persisted before the run is enqueued. So the shape is probably "marker-only when `Cancelled`" rather than a reason-class split — and it needs a decision about whether a partially-completed cancelled subagent's summary is worth a turn.

---

## Nits

1. **`notify_dm_ended_to_webchat` rustdoc is wrong for the episode case.** It says *"Since #1258 an interrupted end (cancel / failure) produces NO run at all, so `run_target_session_ids` is empty and the marker is always the delivery."* An interrupted end with a resolved job episode **does** produce run targets. The conclusion still holds — job sessions are internal, so they can never be the `find_user_facing_session` target — but the stated reason is false, and this is a doc that gets copied forward. Suggest: *"...produces no run on the trigger's own target. The only remaining targets are job sessions (#1198 continuations), which are internal and therefore never the marker target, so the marker is always the delivery."*

2. **`lifecycle.rs:3120-3124` is now half-stale.** *"The truncated `source` string is surfaced in the peer's `dm_ended` notification so the peer (and human user watching the DM) sees a useful reason instead of a stale 'in-flight' indicator."* For errored ends the peer **agent** no longer sees it (no run, and the marker is stripped from context). The human still does. One-line amendment.

3. **`end_was_interrupted` defaults by initialisation, not by construction.** It is declared `false` at the top of the loop and only assigned inside the `ConversationEnded` arm. Correct today, but that is the kind of default that survives a refactor incorrectly — a `matches!(&trigger.source, MessageSource::ConversationEnded { reason, .. } if reason.is_interrupted())` computed next to the match would be immune. Take it or leave it.

4. **Wasted work on the suppressed path.** The interrupted branch still runs `get_history` on the DM session and formats up to `DM_HISTORY_MAX_CHARS` of transcript via `format_dm_ended_notification`, then discards it. Harmless — and if the transcript predicate from the design section is ever adopted, that work becomes load-bearing anyway. Noting only so it is not mistaken for a leak.

5. **Frontend is clean.** The `detail && html` guard renders nothing for `null` / `undefined`, preact escapes the text child so a provider error string cannot inject markup, `data.detail || null` normalises correctly, and both restore paths (`app.js` marker branch, `dm-conversation-view.js` divider) read `md.detail`. The CSS `overflow-wrap: anywhere` + `min-width: 0` is the right call for an unbounded provider message on a flex divider. The two `dm-stream-rendering` tests cover present and absent. No notes.

---

**What I would fix before merge:** the `docs/api.md` `detail` entry (S1), the `cancel_job_teardown_leaves_no_runs_for_the_job` doc comment, and nits 1 and 2 — all small.

**Follow-ups:** the transcript-relay question on `Errored`, the agent-awareness gap, the #1206 scoping re-pin (S4), the `detail()` bounding claim (S3), the `dm-run-lifecycle.md` carve-out (S2), and the `completion_notification_loop` sibling. I would want the first two recorded as explicit decisions rather than left implied by rustdoc that overstates them.
