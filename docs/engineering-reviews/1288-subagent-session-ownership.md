# A review that found a cross-agent data-loss bug

> Reconstructed from the review history of the ALMS project's original issue tracker.
> Cross-references like `#1246` point at that tracker and are kept verbatim for provenance.

**Source:** PR #1288 -- feat(subagents): file a named subagent's session under the agent that ran it  
**Rounds:** 1  


The PR re-keyed where a named subagent's session is filed. The review accepted the keying change, then traced what else the new key reached and found that deleting one agent would destroy *other* agents' subagent transcripts, runs and audit events. It also caught three documentation claims that the change had silently made false.

---


*Posted 2026-08-27*


## Review by Tim (automated)

**Verdict: Needs minor fixes.** The keying change is right, the security reasoning holds where it is stated, and the mutation fix is real. Two things are not in the PR: an agent-deletion cascade that this change turns into cross-agent data loss, and the episodic consequence — which is real, but not the direction the PR describes.

Reviewed at `8519469` against `94e960e`. No local validation (CI and Heph already ran it); this is code review only.

---

### What I verified rather than accepted

**1. The surviving mutation and its fix — confirmed.** `derive_subagent_identity`'s ephemeral branch is still `AgentId::new()`, untouched (`crates/alms-coordinator/src/lib.rs`). The two assertions added at `8519469` pin `id != AgentId::deterministic(parent_agent_id, task_id)` for both invocations, which is the right shape: `AgentId::deterministic` is UUID v5 over a hardcoded in-repo namespace, and both inputs are parent-visible, so a derivable agent id is a derivable session id. "Distinctness does not pin unpredictability" is the correct reading and the assertion kills it.

**2. `check_subagent_session_access` — confirmed, by reading the whole function rather than the diff.** The only field it touches is `session.context_id` (`strip_prefix`, `split_once`, `Uuid::parse_str`, compare against `self.parent_agent_id`). `session.agent_id` appears nowhere in it. The key move genuinely cannot weaken it, and both directions — invoking parent admitted, invoked agent denied — are pinned. See S2 for the part of this that is true only of *this* tool.

**3. The two arms — confirmed, and tighter than the PR claims.** `parse_subagent_context` gates its named arm on `validate_agent_name`, and `invoke_agent` validates the incoming `name` with the *same function* at the tool boundary (`crates/alms-tools/src/invoke_agent.rs:165`). So there is no charset in which a registry-resolvable name fails to round-trip through the context parse — uppercase, underscores, and UUID-shaped names are all rejected before dispatch. The arms agree by construction as stated. #1279's `(subagent)` ephemeral label is unaffected: the ephemeral path is byte-identical and still excluded from the listing.

**4. The listing change — reviewed.** `crossAgentOwner`'s subagent arm reads `parent_agent_id`, `attributionTitle` keeps the tooltip honest, and the sourcing split (`grouped` from `chatSessions`, which excludes `subagent`; `subagentRows` from `crossAgent`) means no row renders twice. The active-agent count is taken from the rendered list and the non-active count from `crossAgentChats`, which does not filter `subagent` — consistent with what renders. No issues.

---

## Critical

### C1. Deleting an agent now destroys *other* agents' subagent transcripts, runs and audit events

`delete_agent` (`crates/alms-session/src/sqlite/agents.rs:293`) collects `SELECT id FROM sessions WHERE agent_id = ?1` and then hard-deletes, in one transaction and with no session-type filter: `context_summaries`, `session_summaries`, `audit_events`, `messages`, `run_tool_calls`, `runs`, and the session rows.

Before this PR, `subagent_{parentA}_reviewer` was filed under `AgentId::deterministic(parentA, "reviewer")`, which is not any registry id — so deleting `reviewer` never selected it. Now it does.

Concretely: atlas invokes `reviewer` fifty times. An operator deletes `reviewer`. Atlas's fifty subagent transcripts are gone, along with their `runs` rows — which carry `parent_run_id` pointing at atlas's own runs — and their `audit_events`. The destroyed data is part of atlas's history, and it is destroyed by an operation on a *different* agent's record.

This is not the break Alper accepted. He accepted a one-time migration loss. `DELETE /agents/{id_or_name}` (`routes.rs:227`) is a live, repeatable product operation, and `docs/security-model.md` section 7 says audit logging is append-only.

It is also the case the dropped fallback's reasoning does not cover. The PR argues correctly that keying on stored state would make identity order-dependent — but the unhandled case was never the orphan, it is what a *later* registry change does to rows already filed under a registry id. Deletion is that change, and it is the only one the API exposes (`UpdateAgentRequest` has no `name` field, so rename is not reachable — see N2).

I am not asking for a particular resolution, but it needs one, and right now it is in neither the PR, the CHANGELOG, nor a test. Two defensible options:

- **Accept it**, with a test row in `delete_agent`'s suite pinning the cascade and a line in the CHANGELOG's breaking block. Cheap, and consistent with "no production deployments".
- **Exclude `subagent_` contexts from the cascade**, so those rows orphan the way they did pre-#1278. Slightly more code, but it keeps the property that deleting agent R cannot destroy agent P's history.

Either way the pin matters more than the choice: this is exactly the shape a future reader will re-derive wrongly.

---

## The decision you surfaced — my read

**The frame is wrong in both directions, and the direction that actually matters is not the one flagged.**

**The direction the PR describes does not happen.** `derive_source_label` returns `None` for any `subagent_` context (`crates/alms-core/src/source_label.rs:39-42`), and *both* write paths early-return on that: `generate_and_persist_summary` (`crates/alms-runtime/src/episodic.rs:277`) and the gateway's `should_summarize` gate (`crates/alms-gateway/src/runs/lifecycle.rs:2439`). No `session_summaries` row is ever created for a subagent session, before or after this PR. `format_episodic_for_injection` has a third, independent guard that skips label-less rows anyway. So the CHANGELOG line —

> its runs and its episodic session summaries are filed under that agent

— is half wrong. Runs, yes. Summaries, never: none are written. That sentence should be corrected regardless of what is decided below, because it is the sentence a future reader will reason from.

**The direction that is real is the read side, and it is sharper.** `run_agent_loop` constructs `AgentRuntime::new(agent_id, config, subagent_llm)` with `agent_id = sub_agent_id` (`crates/alms-coordinator/src/lib.rs:1999`), which post-#1278 is the invoked agent's registry id. `ContextBuilder` then calls `load_episodic_summaries(self.agent_id)` on every run whenever `run_summary_mode != Off` (`crates/alms-runtime/src/agent/context.rs:125-129`; the default is `Llm`). That query filters on `agent_id` alone — no context-type filter (`crates/alms-session/src/sqlite/session_summaries.rs:164`).

So **every named subagent run now has the invoked agent's episodic summaries of its own web chats, Telegram chats, DMs (labelled `DM with alice`) and scheduled jobs injected into its context**, each with an 8-char session-id prefix.

That context is not a dead end. The subagent's output is returned verbatim to the invoking parent as the `invoke_agent` result and persisted into the parent's context. The reachable sequence is: atlas invokes `reviewer` as a named subagent, asks it what it has been working on, and gets back summaries of reviewer's private operator conversations, its DMs with other agents, and its scheduled jobs. No tool call is needed — the block is injected into the system context automatically.

**So: same-agent, or cross-parent?** By identity it is same-agent, and Heph is right about that. As an information flow it is neither — it is a new read primitive from agent R's private history into agent P's context, initiated by P.

And I would push back on the supporting premise. "ALMS has no isolation model between agents" is too strong as written. `read_session` denies cross-agent reads on `session.agent_id == self.agent_id` (`crates/alms-tools/src/read_session.rs:49`); `read_subagent_session` denies non-parents; `list_my_sessions` is agent-scoped; `read_messages` is participant-checked. Every session-reading *tool* in the codebase enforces a per-agent boundary. This change does not cross one of those checks — it routes around all of them, through the context builder, which has no check at all. The write side already excludes subagent sessions from episodic; the read side does not. That asymmetry is the bug-shaped part.

**Mitigating**, and worth stating so this is not over-read: the content is LLM-generated summaries, not transcripts, and the subagent cannot escalate the 8-char prefixes — `read_session` and `list_my_sessions` are dynamic tools the *gateway* registers, and the coordinator's only `register_tool` call is inside `mod tests`. A production subagent has neither.

**My read: fix it here rather than file it.** The containment is smaller than the documentation-and-defer would be — gate `load_episodic_summaries` off when the run's own context is a subagent one (`classify_session_type(context_id) == "subagent"`), which restores symmetry with the write side that already excludes them. I would not ship-and-file this one, for two reasons: `run_summary_mode` defaults to `Llm`, so it fires on every stock deployment rather than an opt-in configuration; and it spends `run_summary_budget` (15% of `max_input_tokens`) of extra context on every named subagent run, on a path where token efficiency is an explicit project priority. If you would rather keep this PR's boundary, then a follow-up issue is the minimum and the CHANGELOG paragraph needs rewriting to describe the read direction instead of the write one.

---

## Suggestions

### S1. `docs/api.md` now makes three false statements about the endpoint this PR changes

The PR touches no docs. All three are about `GET /sessions`:

- **L125** — "Returns all active sessions. Truly internal sessions (episodic, subagent) [are excluded]". Named subagent sessions are now returned.
- **L135**, the `include_dms` table row — "Other internal session types (subagent, episodic) remain excluded." Same.
- **L213**, the blockquote — "Because subagent sessions are excluded here, their `agent_name` enrichment (#1277) is only observable on `GET /session/{session_id}`." The premise is now false for named ones; the enrichment is observable on the listing.

Also missing: the new `parent_agent_id` field is undocumented anywhere, and the `"subagent"` row of the session-type table (L205) should record that a named session is filed under the invoked agent's registry id while the `context_id` still names the parent — that pairing is the whole design, and the table is where someone will look for it.

### S2. The denial you pinned is tool-local, and `docs/security-model.md` L798 is where that should be said

The new test `by_session_id_denies_everyone_but_the_invoking_parent_after_the_move` pins that the *invoked* agent is denied its own subagent transcript. True of `read_subagent_session`. It is not true of `read_session`, whose entire check is `session.agent_id == self.agent_id` (`crates/alms-tools/src/read_session.rs:49`) — and post-#1278 that now matches. Discovery is still blocked, since `list_my_sessions` filters `subagent` contexts out via `is_internal_session`, so this is authorization-without-discovery and I am not calling it a hole. But the two tools now give opposite answers about the same agent and the same bytes, and only one of them is pinned.

The "Subagent session readback (#1181 / PR #1185)" section is the right home — it is where a future change goes looking. Proposed bullet:

> - Since #1278 a **named** subagent session's `agent_id` is the *invoked* agent's registry id, not the invoking parent's. It is deliberately **not** an authorization input: `check_subagent_session_access` reads ownership only out of the `context_id`, which still embeds the spawning parent. Authorizing on `session.agent_id` would grant the transcript to the agent the work was delegated *to* rather than *by*. Note the consequence in the other direction: because the row is now filed under the invoked agent, `read_session`'s `session.agent_id == self.agent_id` check admits that agent to its own subagent transcripts — reachable only with the session UUID, since `list_my_sessions` still filters `subagent` contexts out.

### S3. "All runs for the same agent are serialized via `agent_queue`" is now observably false

`crates/alms-gateway/src/gateway.rs:669` states that invariant. Subagent runs bypass the queue entirely (own `AgentRuntime`), but are now registered under the invoked agent's registry id — `Run::new(sub_session_id, sub_agent_id, ...)` / `Run::for_subagent(...)` at `crates/alms-coordinator/src/lib.rs:1160-1166`. Pre-#1278 the derived id kept the claim literally true; now `GET /runs?agent_id=reviewer` can show several concurrently-running runs for one agent. At minimum the comment needs amending to say "gateway runs".

Related, pre-existing, and *not* aggravated by this PR — but this is the moment it becomes legible in the product, so it is the natural time to file it: `active_named` keys on `(parent_agent_id, name)` and its comment deliberately permits two parents to run `reviewer` concurrently. Their sessions are disjoint; their **workspaces are not** — coordinator `ws_dir.join(name)`, gateway `AgentWorkspace::new(workspace_dir, name)` and `runtime/src/workspace.rs` `base_dir.join(agent_name)` are byte-identical, and `AgentWorkspace::append_file` is an unlocked read-modify-write. Up to three concurrent writers to one `memories.md` (two subagents plus the agent's own queued chat run). `docs/layer2-peer-messaging-design.md` section 7 calls this a hard constraint and names "memory corruption (two instances writing to the same workspace files)" explicitly.

### S4. Subagent rows render a delete button

`SessionItem` renders the delete / confirm-delete control unconditionally — there is no `session_type` gate — so named subagent rows now carry a destructive `DELETE /session/{id}` affordance in the sidebar, and nothing in that path checks for an active run on the session. A named subagent session can be actively written by a live coordinator loop holding `sub_session_id`.

Jobs and notifications set the precedent, so I am not calling this a blocker. But the PR's comments describe these rows as "read-only" three times (`sidebar-grouping.js`, twice in `session-list.js`), which is true of the transcript view and not of the row. Either gate the control for `subagent` or soften the wording — right now the comments claim a property the code does not provide.

### S5. `named_subagent_key` swallows a store error into the fallback key

```rust
let registry_id = self
    .store
    .as_ref()
    .and_then(|store| store.load_agent_by_name(name).ok())
    .flatten()
    .map(|record| record.id);
```

`.ok()` collapses "no such agent" and "SQLite failed" into the same `None`. The fallback is the pre-#1278 derived id rather than a fresh one, so nothing is corrupted — but it means a transient store error files that one invocation on a *different* key than the invocation before it did, forking the named subagent's session. That is precisely the order-dependence the doc comment above it says the design excludes ("The key is a pure function of the registry"), and the repo already has a convention for this exact shape: #1241/#1246 split "absent" from "unreadable" and count the second rather than swallowing it. A `warn!` on the `Err` arm (or `record_degraded_field`) would make it visible, and the doc comment should say "pure function of the registry *when the registry can be read*".

---

## Nits

**N1. Missing row: the parent invokes itself by name.** `named_subagent_key(atlas_id, "atlas")` returns `(atlas_id, "subagent_{atlas_id}_atlas")` — a subagent session filed under the parent's own registry id, with itself as the embedded parent. Nothing forbids it (self-invoke guards are explicitly out of scope) and it is benign — it is in fact the one case where the episodic concern above genuinely *is* same-agent memory. But it is also the one case where `list_sessions_scopes_a_named_subagent_row_to_the_invoked_agent`'s stated rule — "the subagent row leaked into the invoking parent's fetch" — is correctly false. One row asserting the exception would stop a future reader from "fixing" it.

**N2. The renamed-agent divergence is currently unreachable.** `UpdateAgentRequest` (`crates/alms-core/src/registry.rs:292`) has no `name` field, and `PUT /agents/{id_or_name}` is the only update route — so there is no rename path through the API today. The frontend test is good future-proofing and I would keep it, but the PR narrative presents it as *the* live divergence between the two arms, and in practice there is not one. Worth a clause in the test comment so a reader does not go hunting for the rename endpoint. It also means C1's deletion cascade is the *only* reachable post-filing registry change, which is part of why C1 matters.

**N3. Two rows, one label, no badge.** When the invoking parent is absent from `agents.value` (deleted parent), `crossAgentOwner` returns `null` and `subagentLabel` returns the constant `'subagent'` — so the invoked agent's group can show two identical, unlabelled rows. The `title` attribute carries the `context_id`, so hover disambiguates. Acceptable degradation; noting only that this is the one surface where the badge is load-bearing for identity rather than decoration, which makes it worth not losing later.

---

### On what was left out

The `session_id` parameter, the queue move, the invocation-chain guards and the ACL are all genuinely absent — I checked rather than assumed. Stopping at "where the session is filed" was the right call, and the `context_id` staying byte-for-byte identical is what makes the security argument short enough to actually verify. The mutation pass caught a real one and the fix is the right fix.

The gap is that the analysis stopped at the two tools that read `context_id` and did not sweep the other consumers of `session.agent_id` — `delete_agent`'s cascade (C1), `read_session`'s check (S2), the run registry's per-agent serialization claim (S3), and `load_episodic_summaries` (the decision above). Moving one half of a composite key puts every consumer of that half in scope, and four of them were not visited.
