# Three rounds on one PR, each finding something the last did not

> Reconstructed from the review history of the ALMS project's original issue tracker.
> Cross-references like `#1246` point at that tracker and are kept verbatim for provenance.

**Source:** PR #1271 -- feat(settings): make the server-default LLM model/provider live (no restart)  
**Rounds:** 3  


Making the server-default model changeable without a restart turned out to have more exit paths than it appeared. Each round verifies that the previous round's findings are genuinely closed before looking for new ones -- the third round finds a fourth path that commits half a change.

---


## Round 1 -- 2026-08-25


## Review by Tim (automated)

**Verdict: Needs minor fixes.** The wiring is right, and the acceptance tests point at the right object — `run.resolved_config()`, populated from `execute_run`'s resolution at `lifecycle.rs:1719`, i.e. the client the loop is about to send on rather than the settings surface. Lock discipline is clean. One real behavioural hole survives the new gate, and the apply order inside `refresh_llm_from_server_default` is unpinned by any fixture. Both are small, local fixes in code you already touched.

Answers to the three decisions you flagged are at the bottom — all three hold, with one caveat on #1.

---

## Critical

### 1. A rejected provider switch still commits the model half — and that half now lands on the live wire

`settings.rs:998-1022`. Keying `provider_arm_validated_model` on provider *acceptance* is the right key for the case you found. But the `body.model` branch still runs when the provider arm **rejected a known provider**, and it then validates the model against the *unchanged live* provider — which will often accept it.

Live `(openrouter, z-ai/glm-5.2)`, body `{"provider": "anthropic", "model": "openai/gpt-4o-mini"}`:

1. Provider arm (`:914-969`): candidate model is `openai/gpt-4o-mini`, `new_kind = Anthropic`, `model_belongs_to_kind` false, error pushed, `provider_to_commit = None`.
2. `provider_arm_validated_model = false` (`:974`).
3. `reject_model_incompatible_with_live_provider` (`:1123-1140`) reads the **live** provider — still `openrouter` — gets `OpenAiCompatible`, which is permissive, and returns `None`: accept.
4. The model commits, `llm_default_changed = true`, `refresh_llm_from_server_default()` fires (`:1040-1048`).
5. The response is `422 INCOMPATIBLE_MODEL_FOR_PROVIDER`, and because `errors` is non-empty, `persist_settings` is skipped.

Net: the operator gets a 422 saying the switch failed, every subsequent default-agent run silently moves to `openai/gpt-4o-mini`, and `settings.json` still says `z-ai/glm-5.2` — so a restart silently reverts it. Pre-#1148 this body dirtied `server_llm_default` only, a display wart. This PR promotes it to the run path, which is the exact class of outcome the new gate exists to prevent, and it contradicts the PR description's claim that "a rejected switch leaves the live client — provider, model, base URL, API key — completely untouched" (true for `rejected_provider_switch_leaves_the_live_client_untouched`, which sends a provider-only body; not true for the mixed body).

`unknown_provider_does_not_smuggle_an_incoherent_model_through` is this case's sibling and passes only because its live provider happens to be `anthropic` at that point. Change that fixture's live provider to `openrouter` and the same body smuggles the model through.

Minimal fix, using the same "one mistake, one error" principle you already applied:

```rust
// A provider was requested and did NOT make it through (unknown name,
// empty string, or an incompatible pair). The operator asked for a pair;
// commit neither half. The provider error already explains the rejection.
let provider_requested_but_rejected = body.provider.is_some() && provider_to_commit.is_none();
let provider_arm_validated_model = provider_to_commit.is_some();
```

then in the `body.model` branch, ahead of the coherence gate:

```rust
} else if provider_requested_but_rejected {
    // no second error, no commit
}
```

That also closes `{"provider": "", "model": "..."}`, which today commits the model behind an empty-provider 422.

**The mutation row nobody wrote — M16:** *"commit the model half of a rejected pair."* It survives today; no test kills it. Suggested pin:

```rust
/// live = (openrouter, z-ai/glm-5.2); body = {provider: anthropic,
/// model: openai/gpt-4o-mini}. The pair is rejected, so neither half may
/// reach the client the run path reads. The model is coherent with the
/// provider that stays in force, which is exactly why it slips the gate.
async fn rejected_provider_switch_does_not_commit_the_model_half()
```

---

## Suggestions

### 2. The second row nobody wrote — M17: swap the apply order in `refresh_llm_from_server_default`

`state.rs:245-267` applies provider **then** model, correctly mirroring boot. Nothing pins that order. `apply_provider` (`llm_client/mod.rs:881-883`) overwrites `default_model` from `[llm.providers.<name>].model` when the entry has one, so `with_model(...).with_provider_and_secrets(...)` would clobber the operator's patched model with the entry model. Every provider fixture in this PR — `settings_test_app_state_with_two_providers` and `llm_default_harness`, both `model: None` on purpose — makes that mutation invisible. M12/M13 pin only *that* each apply happens, not their order.

Add a fixture carrying an entry-level model (a third provider, or a dedicated one, since `rejected_provider_switch_leaves_the_live_client_untouched` depends on `anthropic` having none) and assert that `{"provider": "<p>", "model": "<explicit>"}` leaves the live client on `<explicit>` rather than on `[llm.providers.<p>].model`. That is the single observable asymmetry between the two orders, and the boot path shares it.

### 3. Whitespace-mangled string literals (5 new, one of them on the wire)

Some editing step joined wrapped lines into single-line literals containing runs of ~10 spaces. `grep -c '"[^"]*   [^"]*"'` on `settings.rs`: 5 on this branch, 0 on `develop`.

- `settings.rs:1136` — the operator-facing `422 INCOMPATIBLE_MODEL_FOR_PROVIDER` body: `does not belong to the          wire kind`, two more runs after that, plus `pick a model from 'anthropic''s namespace` (doubled apostrophe). This string renders in the Settings modal error list.
- `settings.rs:1046` and `state.rs:264` — both new INFO log lines, same damage.
- `settings.rs:2838` / `:2842` / `:2849` — test assertion messages, harmless.

`cargo fmt` does not touch string interiors, so CI cannot catch these. A `\` line continuation strips the newline *and* the leading whitespace.

### 4. Validate, commit, refresh is not atomic across concurrent PATCHes

`patch_settings` reads the live provider for the coherence gate (`:921`, `:1131`), commits (`:980-1020`), then rebuilds (`:1042`), holding nothing across the sequence. Two concurrent PATCHes — `{"model": "gpt-4o"}` (validated against a live `openrouter`, accepted) interleaved with `{"provider": "anthropic", "model": "claude-sonnet-4-6"}` (also accepted) — can commit in an order that leaves the live client on `(anthropic, gpt-4o)` and then rebuilds it. That is the incoherent-pair-on-the-live-wire outcome the new gate was added to prevent, reached through a different door.

Rare in practice (one operator, one UI) and pre-existing as a `settings.json` corruption, but the blast radius moved to the run path with this PR. Cheap fix: one `parking_lot::Mutex<()>` on `AppState` held across the model/provider section of the handler — PATCH is not a hot path. A follow-up issue is fine if you would rather not widen this PR.

### 5. `docs/api.md` § 10.2 contradicts a test in this PR

The new section says "Validation, **all of which must pass before anything is committed**". That holds for the budget rule (early return at `:534-537`) and for the pair's own gates, but `partial_failure_still_applies_the_committed_pair_to_the_live_client` asserts the opposite for a cross-section failure: a body that 422s on `context.strategy` still commits the pair *and rebuilds the live client*, skipping only persistence.

The behaviour is right and matches the other sections. What is undocumented is the operator-visible consequence: **after a partial failure the live pair and `settings.json` disagree, and a restart silently reverts the model.** That is unpleasant to diagnose from outside ("I got a 422, the model changed anyway, then a restart changed it back"). One sentence in § 10.2 and one in the CHANGELOG bullet.

### 6. `patching_the_default_mid_run_does_not_disturb_the_in_flight_run` is the weakest of the five pins

It waits for `run_started`, PATCHes, then asserts on `run.resolved_config()` — a write-once record (`try_mark_run_as_running_with_config`, `:1719`) that nothing ever rewrites. Nothing keeps the run in flight past `run_started` either; with the mock adapter it has most likely already finished. The test would stay green even if the runtime *did* re-read the shared handle mid-run.

The property itself is structurally guaranteed — `llm` is moved into `AgentRuntime::new` at `:1895` and the loop owns it by value — so this is a coverage-honesty note, not a defect. Either say so in the doc comment or hold the run open (a tool-gated mock) so the assertion has something to fail against. Your two stated coverage boundaries were both honest; this is a third that was not listed.

### 7. An empty candidate model can now reach the live client

`settings.rs:923-925`: `body_model` is empty-filtered, the `current_default_model` fallback is not. A daemon booted with an empty `default_model`, plus a provider-only PATCH to an `OpenAiCompatible` provider with no entry model, yields `candidate_model = Some("")`; `model_belongs_to_kind("", OpenAiCompatible)` is `true`, so `model = ""` commits and `with_model("")` clears the live client's model — every subsequent run fails with a missing-model error. The `None => false` arm at `:930-932` was meant to catch this shape; add `.filter(|m| !m.is_empty())` to the fallback so it does. Pre-existing in the commit path, live now.

### 8. Two pre-existing no-op tests now assert a tautology

`patch_provider_only_noop_does_not_mutate_default_or_set_restart_required` (`:4511-4523`) and `patch_model_only_noop_does_not_set_restart_required` (`:4575-4586`) still carry "Bonus" assertions that `restart_required` is absent, with comments claiming they would fail without the no-op guard. The field can no longer exist on the wire, so those assertions can never fail — which slightly inflates the M8 row. Repoint them at `state.llm.read().default_model()`, which *would* catch a lost guard and is what `no_op_patch_does_not_rebuild_the_live_client` already does properly.

### 9. Test-hygiene finding — agreed, file it

Your `GatewayConfig::default()` / `crates/alms-gateway/.alms/settings.json` diagnosis is right, and the harness owning its own tempdir is the correct local fix. The pre-existing writers in `settings.rs` are a genuine latent flake source: a PATCH test that runs before its `state.data_dir` override leaves a file that a later `AppState::new` in the same cwd reads as its boot default. Worth its own issue — either make `AppState::new` refuse a cwd-relative `data_dir` under `cfg(test)`, or give `settings_test_app_state()` a tempdir by construction so no individual test has to remember.

---

## Nits

- `state.rs:188` adds a third doc reference to `gateway.rs::Gateway::run_telegram`. No such function exists — the Telegram loop lives inside `Gateway::run_until_shutdown` (`gateway.rs:675`, loop at `:728`). Two of the three references are pre-existing (`state.rs:166`, `settings.rs:377`); worth fixing all three while you are here.
- CHANGELOG: "a model its provider cannot speak" oversells the gate. It is a namespace prefix check (`claude-`, `gemini-`), so `claude-sonnet-4-7` passes happily. "a model from another provider's namespace" is what it actually enforces. `docs/api.md` states the rule accurately.
- `docs/config.md`'s `[llm] provider` / `model` block (`:20-22`) is now the one place an operator can read about these keys without learning that they are PATCH-mutable and that `settings.json` wins over `alms.toml` on the next boot. One cross-link to § 10.2.
- `AppState::new` (`:498-506`) passes `llm.read().clone()` into `with_agent_config` and immediately discards it via `with_shared_llm`. Harmless, but a `Coordinator::with_agent_config_and_shared_llm` — or taking the `Arc` in the constructor — would avoid both the throwaway client and the "which handle am I holding" question at the read site.

---

## Verified, so you know what was actually checked

- **Lock discipline is clean.** Every `state.llm` access is `.read().clone()` inside a single statement; no guard crosses an `.await`. `refresh_llm_from_server_default` drops the `server_llm_default` and `llm` read guards before taking `secrets.read()`, and drops `secrets` before `llm.write()` — so it never inverts the run path's (llm then secrets) order and never takes a write under a read. No reentrant acquisition in `patch_settings`: every `server_llm_default` write guard is block-scoped above the refresh call.
- **No missed consumer.** The three production `AgentRuntime::new` sites are exactly `execute_run` (live), `Coordinator::spawn_subagent` (live via the shared handle), and the Telegram loop (documented boot snapshot), and there is one `Coordinator` construction site. Nothing else reads the server-default client.
- **`state.llm_config.provider` / `.default_model` really are dead for live paths.** The only non-test readers are the boot block in `AppState::new`. `validate_patch_budget` already takes the live pair (`:481`), and the `POST /runs` budget check takes the resolved client. The "left stale on purpose" comment is accurate today, and the field-level warning you added is what will keep it that way.
- **Summarization does not drift.** `build_summary_client` derives from the run's resolved `llm` (`lifecycle.rs:1887`), not `state.llm`, so a mid-run PATCH cannot retarget the summarizer.
- **The create_run / execute_run window is handled.** A PATCH landing between the `create_run` pre-flight and `execute_run`'s re-resolution cannot put an incoherent pair on the wire: `execute_run` re-resolves and fails the run with the same structured message (`:1442-1478`).
- **A bonus fix you may not have noticed.** `GET /settings.base_url` was already stale *at boot*, not only after a PATCH. `AppState::new` applies a persisted provider to the client (`state.rs:413`) but sets only `llm_config.provider` — nothing ever updates `llm_config.base_url`. So pre-PR, a daemon restarted after a persisted provider switch reported the old base URL beside the new provider name. Reading it off the client fixes that too; worth half a sentence in the CHANGELOG.

---

## The three decisions

**1. Tightening validation on a model-only PATCH — correct call, with one caveat.**

The reasoning is sound and the blast radius genuinely changed: what used to be a bad row that bit on the next restart now breaks every run the instant the PATCH returns. Reusing `model_belongs_to_kind` and the existing error code keeps the surface coherent instead of inventing a rule.

`OpenAiCompatible` staying permissive is **not** a hole — it is required. `model_belongs_to_kind` returns `true` for that kind everywhere else, including the runtime's per-agent provider switches (#860 / #863 / #942). Making PATCH stricter than the runtime would reject pairs the runtime runs happily, and OpenRouter legitimately routes `anthropic/claude-*` and `google/gemini-*` on an OpenAI-shaped wire. Anything tighter needs a real model catalogue, which does not exist. `patch_model_only_accepted_on_openai_compatible_provider` pins the right boundary.

The caveat is Critical #1: the gate is correct for the bodies it sees, but it does not see the body that matters most — a rejected pair, where the model half walks straight past it. Close that and the tightening is complete. Do not veto it.

**2. Coordinator inclusion — reasoning verified, keep it in this PR.**

Checked directly. `base_agent_config` is the shared `Arc<RwLock<AgentConfig>>` handed to the coordinator at `state.rs:498-501`, and `spawn_subagent` snapshots it under the lock two lines below your new llm read (`lib.rs:626-634`). So before this PR both the parent run and its subagents resolved from boot-pinned clients and could not disagree; an `AppState`-only fix would have *introduced* the split rather than leaving one. That is a defect prevented, which is the right bar for scope expansion, and the cost is three lines of production code plus a builder. Splitting it out would mean merging a PR that knowingly creates a divergence — keep them together.

The `#[cfg(test)] llm_snapshot()` mirror is an acceptable stand-in given no mock adapter echoes the model, and M10/M11 do separate the two failure modes (ignoring the handle versus snapshotting at construction). `coordinator_without_shared_llm_keeps_its_own_client` is the right complement: it shows the sharing comes from the builder rather than incidental aliasing.

**3. Telegram documented, not addressed — agree, and the code backs the argument.**

Verified at `gateway.rs:733-742`: the loop already carries the identical boot-snapshot caveat for `self.config.agent_config`, and it resolves against `&self.llm`, the `Gateway`'s own client, at `:749`. `state.gateway` is an `Arc<tokio::sync::Mutex<Gateway>>` (`state.rs:59`) locked at `server/mod.rs:117` for the daemon's lifetime, so a handler genuinely cannot reach in. Making this one pair the exception would trade a uniform rule for a per-field matrix, which is the wrong deal for an operator's memory.

Naming the pair inside the existing caveat in `docs/api.md` § 10.2 and in the Settings modal is the right minimum. Please open the follow-up ("Telegram live-config pass: share `Arc<RwLock<AgentConfig>>` and `Arc<RwLock<LlmClient>>` into `Gateway`") so the caveat has a tracked exit rather than becoming permanent by default.


## Round 2 -- 2026-08-25


## Review by Tim (automated)

**Verdict: Needs minor fixes.** All nine items from the last pass are genuinely closed — I checked each against the code, not the summary. `2449f0d` is a better patch than the one I proposed, and the M22 call is right: I'll take your version over mine.

Two things left. One is the fourth door on the pair invariant — the mirror of the one you just closed, and it falsifies a sentence this PR added to `docs/api.md`. The other is the row nobody wrote, which your new rule *did* fire on but which came out as a test that cannot kill the mutation it was written for.

Answers to your two open questions (M22 equivalence, and the 250 ms test) are at the bottom.

---

## Critical

### The fourth door: an empty `model` still commits the provider half — and a model the operator never named

`settings.rs:947`, `:996-1003`, `:1025-1046`, `:1048-1053`. `provider_requested_but_rejected` closes provider-rejected-model-survives. The mirror — model-rejected, provider-survives — is still open, because the `model.is_empty()` check at `:1049` lives *after* the pair has already been committed at `:1025-1046` and made live at `:1092`.

Repro on a fixture that already exists in this file, `settings_test_app_state_with_entry_model_provider()` (`:3685`). Live `(openrouter, z-ai/glm-5.2)`; body `{"provider": "anthropic-pinned", "model": ""}`:

1. `:947` — `body_model = None`. The empty-filter you added for #7 routes `Some("")` to `None`, which is right for the compat check but means the commit path below stops seeing that the operator sent a `model` at all.
2. `:948` — `entry_model = Some("claude-haiku-4-5")`; `:957` `candidate_model` = that; `new_kind = Anthropic`; `model_ok`.
3. `:996` — `body_model.is_none()` and `"anthropic-pinned" != "openrouter"`, so `model_to_commit_with_provider = Some("claude-haiku-4-5")`.
4. `:1024` — `provider_requested_but_rejected = false`. Correctly: the provider half was fine.
5. `:1025-1046` — both halves commit, `llm_default_changed = true`.
6. `:1049` — *now* the empty model is rejected. One error.
7. `:1092` — rebuild fires. Live client is `(anthropic-pinned, claude-haiku-4-5)`.
8. `:1121` — errors non-empty, `persist_settings` skipped.

The operator gets `422 model: empty string not accepted`, the daemon silently moves to a provider **and a model they never named** (`claude-haiku-4-5` came from `alms.toml`, not the body), `settings.json` still holds the old pair, and a restart reverts it. Same outcome as the case you just fixed, reached from the other side.

It does not need an entry model, either — that shape is just the worst one. Any accepted provider whose kind tolerates the live model (a second `OpenAiCompatible` entry, say) commits the provider half alone and rebuilds.

**This one falsifies a sentence this PR wrote.** `docs/api.md` § 10.2 now says the pair is committed "all or nothing: if any rule below fails, neither `model` nor `provider` lands — not on the live client, not in `settings.json`", and row 2 of its own table is *"`model` / `provider` must be non-empty when present"*. The body above breaks exactly that row and lands the provider anyway. So this is not a hypothetical edge — the docs already promise the behaviour, and either the code or the doc has to move.

Reachability: **not** from the UI — `static/ui/components/settings-modal.js:495` gates on `defaultModel.value &&`, so the modal never sends an empty `model`. It is an API / scripted-client shape, which is the same reachability class as `{"provider": "", "model": "..."}` that you did close, and it is the class `docs/api.md` is written for.

Fix is the mirror of your own flag, hoisted above the provider arm (~`:886`):

```rust
// Mirror of `provider_requested_but_rejected`: a model half that cannot
// be honoured must not let the provider half commit behind the 422 —
// nor the target provider's entry model, which the operator never named.
let model_requested_but_rejected = body.model.as_deref().is_some_and(str::is_empty);
```

then gate the two commits at `:1025` / `:1036` on `&& !model_requested_but_rejected`. `{"provider": "typo", "model": ""}` still yields two errors, which is correct — two mistakes, two errors.

Suggested pin and row:

```rust
/// Fourth door, and the mirror of `rejected_provider_switch_does_not_commit_the_model_half`.
/// live = (openrouter, z-ai/glm-5.2); body = {provider: "anthropic-pinned", model: ""}.
/// The provider half is fine, so the pair-rejection flag does not fire — but the
/// model half is rejected at `:1049`, several statements after the pair has already
/// been committed and rebuilt. The committed model is the provider *entry's*
/// (`claude-haiku-4-5`), which the body never named.
async fn empty_model_does_not_commit_the_provider_half()
```

Assert 422, `state.llm.read().provider() == "openrouter"`, `default_model() == "z-ai/glm-5.2"`, `server_llm_default.model` unchanged, and `!settings_path(&state.data_dir).exists()`.

**M27:** *"commit the provider half (and the target entry's model) when the model half is an empty string."* Survives today.

---

## Suggestions

### The row nobody wrote — and it is a call-site row, not a callee row

`state.rs:281`. Mutation: `with_provider_and_secrets(&target.provider, &secrets_guard)` becomes `with_provider(&target.provider)`. One token, and the call is right there in the same expression.

`with_provider` (`llm_client/mod.rs:944-949`) hands `apply_provider` a `|_| None` resolver, which on a provider *change* takes the "Provider changed but no API key found — clearing stale key" branch at `:920-929` and calls `self.config.api_key.clear()`. So under that mutation every live provider switch leaves the shared client with an **empty API key**, and every subsequent default-agent run fails with an opaque 401 that nothing ties back to the PATCH.

**Nothing in the repo kills it.** Two independent reasons:

- `LlmClient::api_key()` is `#[cfg(test)]` (`llm_client/mod.rs:1058-1062`), so no `alms-gateway` test can observe the field at all.
- `test_provider_round_trip_restores_entry_api_key` — the test you added for this — calls `with_provider_and_secrets` **directly**. It passes unchanged under the mutation, because the mutation is not in the callee. It pins that `with_provider_and_secrets` resolves keys; it says nothing about whether `refresh_llm_from_server_default` calls it.

You flagged the boundary honestly twice (`settings.rs:3118-3125` and `:4279-4288`), so this is not a hidden assumption. But both notes point at the callee test as covering the gap, and it doesn't.

What makes this worth a row rather than a shrug: **this exact bug already happened in this codebase, in the sibling code path.** `settings.rs:4196-4209` documents it — *"Pre-fix: `AppState::new` applied a persisted `provider` via `llm.with_provider(provider)`, which never resolves keys ... leaving an empty `api_key` on the LlmClient."* #1081 fixed the boot call site. This PR adds a second call site with the same two choices available and no regression net on the new one.

On your rule — *"write a row for every decision whose rationale I stated"* — it **did** fire here. `refresh_llm_from_server_default`'s doc comment states the rationale explicitly ("Key resolution goes through `with_provider_and_secrets`, matching both the boot path and `resolve_agent_config`'s per-agent provider switch"). The rule produced a test; it just produced the wrong *kind*. Refinement worth carrying: **when the stated rationale is about *which function I called*, the row lives at the call site, and a test of the callee cannot kill it.** That is a different question from "does the callee behave correctly", and it is the one that was left unanswered.

Cheap close, and it is the same move `base_url()` just made in this PR:

```rust
/// Whether a non-empty API key is currently resolved.
///
/// Carries no secret material — the key itself stays test-only. Exists so
/// the gateway can pin that a live provider switch re-resolves credentials
/// (#1148); `apply_provider` clears the outgoing provider's key, so a call
/// site that skips the secrets resolver leaves the shared client unable to
/// authenticate and every subsequent run 401s.
pub fn has_api_key(&self) -> bool {
    !self.config.api_key.is_empty()
}
```

Then one gateway test: empty `SecretsStore`, PATCH `{provider: "anthropic-pinned", model: "claude-sonnet-4-6"}` on the fixture at `:3685` (its entry already carries `api_key: Some("sk-ant-pinned")`, `:3693`), assert `state.llm.read().has_api_key()`. That kills the mutation, and it also closes the identical acknowledged blind spot at `:4279` for the boot path — one accessor, two coverage holes.

---

## Verified — the two questions you left open

### M22: your equivalence claim holds, and deleting the flag beats my patch

I checked it against `385e41a` rather than taking it. `provider_arm_validated_model = provider_to_commit.is_some()` (old `:974`).

- **True direction.** True implies the arm accepted `provider = p`, and `p` is committed to `server_llm_default` at old `:975-984`, *above* the `body.model` branch. So `reject_model_incompatible_with_live_provider` reads `p` and re-derives `provider_kind_for_name(p, state.llm_config.providers)` — the identical call the arm made for `new_kind`. Any `model` reaching the gate is non-empty (`:1049` returns first), so `body_model == Some(model)` and `candidate_model == body_model` at `:957` — the arm validated *this exact string against this exact kind* and passed. The gate therefore returns `None` in every reachable true case.
- **False direction.** Short-circuit not taken; gate runs. Identical to having no parameter.

Dead in both directions. It was an equivalent mutant, not a coverage gap, and you were right to delete rather than keep it. My patch would have shipped a flag whose only remaining job was to be dead — yours is strictly better and I'd take it.

**The ordering dependency you created in its place is genuinely pinned, not merely described.** `an_accepted_switch_is_not_re_judged_against_the_outgoing_provider` (`:3406`): move the gate above the commit and it reads `anthropic`, `model_belongs_to_kind("z-ai/glm-5.2", Anthropic)` is false, and the `assert_eq!(status, OK)` at `:3437` fails. I also checked the test cannot self-neutralise: its setup PATCH (`{anthropic, claude-sonnet-4-6}` against a live `openrouter`) still returns 200 under the mutation, because the outgoing wire there is permissive — so the mutant reaches the assertion that catches it. Real kill.

And your side observation is correct and was worth finding: every pre-existing test in this file switches *to* the strict kind, where the outgoing `OpenAiCompatible` wire accepts everything and the two readings agree. `patch_provider_and_model_retargets_the_live_client_wire`, `patched_model_wins_over_the_target_provider_entry_model` and `rejected_provider_switch_does_not_commit_the_model_half` all move toward Anthropic. Nothing tested the way out until you wrote it.

### The 250 ms test earns its place — keep it, but it can cost less

The flake concern doesn't apply here, because the assertion polarity is inverted from the shape that flakes. `timeout(250ms, &mut bg).await.is_err()` fails only if the handler **completes** while the lock is held; load, a slow runner, or a busy CI box make it *more* likely to pass, not less. That is the opposite of the "must finish within N ms" shape that costs re-runs. A panic in the task doesn't pass it vacuously either — the `JoinHandle` would be ready and `is_err()` false.

It is also honest about what it proves: `:3656-3660` asserts the blocked PATCH did not reach the live client, and `:3668-3673` asserts it then completes with 200 *and moves the model*, so it cannot pass on a request that never entered the locked region.

Two things worth knowing rather than changing:

- The 250 ms is a **deterministic** cost every suite run, not a risk. If you want it back, ~50 ms is equally sound — the failure it catches (no lock at all) completes in microseconds, so the margin buys nothing.
- `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]` plus a blocking `parking_lot` guard held across `.await` (hence the scoped `#[allow(clippy::await_holding_lock)]` at `:3628`) means one of the two workers is parked on the mutex for the duration. It works because the test future itself runs on `block_on`'s thread. Fine as written, but it is the kind of arrangement that breaks quietly if someone later trims `worker_threads` or adds a second spawned task — worth one line in the doc comment saying the worker count is load-bearing.

**The lock itself is right, and I verified its coverage rather than assuming it.** Outside `#[cfg(test)]`, `server_llm_default.write()` occurs only at `settings.rs:1030`, `:1041`, `:1068`, and `llm.write()` only at `state.rs:284` in `refresh_llm_from_server_default`, which has exactly one caller — the handler. So the mutex covers every production writer. And `patch_settings` has zero `.await` between `:456` and `:1138`, so the `await_holding_lock` tripwire argument is real and not aspirational. Fixing this here rather than deferring was the right call.

---

## Everything else from the last pass — closed

Checked against code, not the summary:

- **#1, the three doors.** All three tests exist and assert the object that matters. `rejected_provider_switch_does_not_commit_the_model_half` (`:3338`) asserts the live client, the displayed default, `!settings_path().exists()` **and** `errors.len() == 1` — the live/disk divergence is pinned rather than implied, which is the part that actually hurts an operator.
- **#2, M17.** `settings_test_app_state_with_entry_model_provider` (`:3685`) correctly rebuilds the live client so its own `providers` snapshot carries the third entry — `apply_provider` reads the client's map, not `state.llm_config`, and getting that wrong would have made the fixture silently inert. `patched_model_wins_over_the_target_provider_entry_model` (`:3729`) is a real kill for the swapped order.
- **#3, whitespace.** `settings.rs`, `state.rs` and `alms-coordinator/src/lib.rs` are all at 0 matches, matching `develop`. Repaired properly, including the doubled apostrophe.
- **#5, § 10.2.** The split is right and the partial-failure block says the operator-visible thing rather than the mechanism. (The "all or nothing" half is what the Critical above falsifies.)
- **#6, in-flight.** Boundary stated accurately, and the added `state.llm.read().default_model()` assertion (`:4348-4353`) closes the "passes because the PATCH never landed" hole.
- **#7, empty filters.** Both candidates filtered (`:947`, `:948-953`), and the commit now reads `candidate_model` directly at `:1001` instead of re-deriving — so the committed value cannot drift from the approved one. That last bit wasn't asked for and is the better fix.
- **#8.** Both repointed at `state.llm.read().default_model()` with a desynchronised sentinel; they can now fail.
- **Nits.** `run_telegram` is at 0 references repo-wide. CHANGELOG, `docs/config.md` and the `base_url` bullet all landed. `with_agent_config_and_shared_llm` is the better shape — `with_agent_config` delegating means there is one way to get a shared handle and one way to get a private one, and `coordinator_built_from_an_owned_client_keeps_its_own_handle` pins the distinction.
- **CI.** All three checks green on `2449f0d` (CI, Frontend, Security audit) — confirmed after they finished.

---

## Nits

- `state.rs:178` still names `Coordinator::with_shared_llm`, which this commit deleted. A dangling doc reference of exactly the `run_telegram` kind, created in the commit that fixed `run_telegram`. Should be `Coordinator::with_agent_config_and_shared_llm`.
- `settings.rs:4924` cross-references `patch_provider_only_noop_does_not_mutate_default_or_set_restart_required`, renamed in this commit to `..._or_rebuild_the_client`.
- `refresh_llm_from_server_default` (`state.rs:270-284`) emits the "client rebuilt" INFO *before* `*self.llm.write() = rebuilt`. Cosmetic, but the log claims a past-tense fact that is one statement away.
- `GET /settings` doesn't take `settings_patch_lock`, and it now reads two sources — `server_llm_default` and `state.llm.read().base_url()`. Mid-PATCH it can observe provider committed but model not (`:1030` and `:1041` are separate write guards), or the pair committed but the client not yet rebuilt. Display-only and sub-millisecond; not worth locking the read path. Worth knowing that "serialised" in § 10.2 means writer-vs-writer.
- Not yours: `llm_client/mod.rs:2980` carries the same whitespace damage, but it is pre-existing (`develop` `:2891`), so it is not a miss on this PR. Flagging only so it doesn't read as one — worth a drive-by next time someone is in that file.
- `base_url()` widening from `#[cfg(test)]` to `pub` is fine — no secret material, and the doc says why. Noting it only because it is a surface change a reviewer should justify rather than skim.

---

## On the unfiled test-hygiene finding — yes, file it

`llm_default_harness` (`runs/integration_tests.rs:3956`) now owns its tempdir, which fixes the new tests and is the right local fix. The pre-existing pattern in `settings.rs` is unchanged though: roughly twenty tests each hand-roll `let tmp = tempfile::tempdir(); state.data_dir = tmp.path()`, and any test that forgets writes `crates/alms-gateway/.alms/settings.json` into the source tree, where a later `AppState::new` in the same cwd reads it as its boot default. That is a cross-test dependency with an ordering-dependent failure — the flake shape, just not yet firing.

This PR made it *less* likely, not more, so it is not this PR's to fix. It does warrant its own issue: *"settings tests can leak `settings.json` into the source tree — give `settings_test_app_state()` a tempdir by construction so no individual test has to remember."*

#1272 is the right framing for the Telegram gap — widening past the LLM pair to "which sections should a channel-triggered run see live" is the question that actually needs an answer, rather than adding one field to an exception list.


## Round 3 -- 2026-08-25


## Review by Tim (automated)

**Verdict: Ready to merge.** CI, Frontend and Security audit are all `SUCCESS` on `b1e5f36` — confirmed after they finished, not from the in-progress state.

The restructure is the right call and it holds up. I checked the three properties you claimed rather than the summary: Phase 1 writes only locals, Phase 2 has no rejection path, and the ordering constraint I verified last round is genuinely retired rather than relocated. The class is closed, not the fourth instance.

One new Suggestion below — the last asymmetry you flagged yourself has a second direction you did not trace, and it is the operator-visible one. Not a leak. My read on your two deferred findings is at the bottom.

---

## The restructure — verified, not taken

**Phase 1 writes only locals.** `:903-1088` touches `pair_errors`, `named_provider`, `named_model`, `provider_to_commit`, `model_to_commit` and reads `state.server_llm_default` (`:904`, cloned) and `state.llm_config.providers` (`:929`, `:1000`, `:1009`). No `write()`, no shared mutation of any kind. `llm_default_changed` is declared at `:903` and only assigned inside Phase 2.

**Phase 2 has no rejection path.** `:1099-1123` is two `if let` commits behind one `pair_ok`. No `push`, no branch that can produce an error. The only two production writers of `server_llm_default` outside `#[cfg(test)]` are `:1107` and `:1117`, both inside it, plus the boot seed at `state.rs:609` — so there is nowhere else for a half-pair to land.

**Nothing commits before its own validation.** Every staged value is the one the gate approved: `:1040` commits `candidate_model` from inside the accepting match arm, and `:1085` stages `named_model`, which is validated either by `:1014` (provider arm) or `:1063-1068` (model-only arm). There is no third staging site.

**The retired ordering constraint is genuinely gone.** `reject_model_incompatible_with_provider(&state, &live_pair.provider, model)` is called only from the model-only arm, and that arm is reachable only when `body.provider` was *absent* — a present-but-rejected provider leaves `pair_errors` non-empty at `:973`, which skips all of Phase 1b. `live_pair` is cloned at `:904` before any write, `settings_patch_lock` is held from `:456`, and the only writers are the two inside Phase 2. So the provider passed in is provably the post-patch provider, by argument rather than by sequence. Nothing needs the old constraint. `an_accepted_switch_is_not_re_judged_against_the_outgoing_provider` (`:3468`) still has a live target — `provider_kind_for_name(provider, ..)` becoming `live_pair.provider` at `:1009` makes the switch back out to OpenRouter get judged against the outgoing Anthropic wire, and the `assert_eq!(OK)` at `:3499` fails.

**One thing worth saying out loud:** this shape is not novel here. `:611-658` (the compact trigger/retain pair, #1012) and `:660-688` (the summary pair) already compute candidates, validate the candidates, and commit only on success — for exactly the reason you hit. The pair block was the last section in the handler still interleaving. So the restructure is not a new abstraction to maintain, it is the file's own convention finally applied to the one section that skipped it. That is a better argument for it than "four point-fixes" and it belongs in the block comment.

**My own fifth-door pass.** The three checks you listed are correct as far as they go; I also checked the two places that could theoretically commit the pair from outside the block. The budget pre-validation (`:503-560`) only ever returns — `Ok(Some(rejection))` and `Err(..)` both exit before any section runs, so it cannot leak a half-pair. And the other four sections (`context`, `session`, `tools`, `llm`) never touch `server_llm_default`. Combined with the writer enumeration above, the pair's commit surface is exactly Phase 2.

---

## The sweep is not vacuous — and it kills the gate for two independent reasons

I checked this rather than taking it, since it is the same failure you had just caught.

Rejecting cells are plentiful (any `provider: ""`, any `provider: "nope"`, any `model: ""`), so the assertions run. But "runs" is not the interesting question — the interesting one is whether any *rejecting* cell reaches the staging site, because that is the only kind that can kill `pair_ok = true`. Two do, from different arms:

- `baseline=openrouter`, `{provider: "anthropic-pinned", model: "gpt-4o"}` — Phase 1a clean, Phase 1b rejects at `:1050`, then `:1085` stages `model_to_commit = "gpt-4o"`. Under the mutant the model commits with no provider, `llm_default_changed` flips, the client rebuilds, and `displayed_after.model` moves. The sweep's displayed-model assertion fires.
- `baseline=anthropic-pinned`, `{model: "gpt-4o"}` — the same kill through the model-only arm at `:1063`.

So the gate is killable, and by two paths rather than one. Your `fc2c25c` call was right: the re-check was the flag stack reassembling itself, and a gate no test can distinguish from a no-op is worth strictly less than the reasoning that produced it.

Complementarity checks out too — dropping the empty-model rule at `:947` makes every `model: ""` cell return 200, which the sweep skips at `:4041`, leaving it green while `empty_model_does_not_commit_the_provider_half` dies. Genuine complements.

---

## The missing row — closed properly

`has_api_key()` (`llm_client/mod.rs:1082`) is the right surface: yes/no, no key material, and the doc says why it exists in a way the next reader can act on. All three assertion sites are real:

- `patch_provider_switch_re_resolves_the_api_key_on_the_live_client` (`:4097`) is a true kill. It asserts the baseline (`:4113` — the live client *starts* holding the openrouter key, which is what `apply_provider` clears) and that the SecretsStore holds nothing for the target (`:4105`), so the entry is the only key source and a `|_| None` resolver cannot accidentally succeed. Without the baseline assertion this would have been a weaker test than it looks.
- `boot_resolves_provider_entry_api_key_after_persisted_provider_switch` (`:4667`) now has the assertion the #1081 regression test was missing.
- `repeated_provider_switches_are_idempotent_on_the_live_client` (`:3184`) replaces a comment explaining why the property could not be asserted with the assertion.

The generalisation is the durable part: *a callee test cannot kill a call-site mutation.* `state.rs:257-270` now states it at the site where the next person will make the same choice, which is where it does the most good.

---

## Suggestions

### The entry-model asymmetry has a second direction, and it is the one an operator sees

You traced this and concluded it was benign. The no-leak conclusion is right — I re-derived it independently — but the trace only covered one direction. You wrote: *"the only reachable divergence is a no-op PATCH returning 200 where a stricter reading would 422."* The other direction is reachable too, and it is the worse UX.

`:1000-1005` resolves `entry_model` for the named provider **unconditionally**, but the commit guard at `:1034` only adopts it when `provider != live_pair.provider`. So on an idempotent provider PATCH with no body model, Phase 1b judges a value that will never be committed and never looks at the value that stays in force. If `[llm.providers.X].model` is outside X's own namespace — an operator typo, e.g. `[llm.providers.anthropic]` with `kind = "anthropic"` and `model = "gpt-4o"` — then with live `(anthropic, claude-sonnet-4-6)` the body `{"provider": "anthropic"}` returns:

```
422 INCOMPATIBLE_MODEL_FOR_PROVIDER: switching server-default provider to 'anthropic'
    but the post-patch model 'gpt-4o' does not belong to that provider's wire kind
```

for a PATCH that changes nothing, naming a model the operator never sent, and blaming a "switch" that is not one.

What makes this more than a curiosity: **this PR's own test file already asserts the opposite convention one section over.** `:5230-5296` plants `[llm.providers.anthropic].model = "claude-haiku-4-5"` specifically to pin that the budget overlay must *not* consult the entry model on a same-provider PATCH — *"the commit path keeps the live default model, so the overlay must too. Mismatched overlay yields a false-positive rejection."* That is verbatim the argument that applies to Phase 1b, and `:546` already implements it with a provider-inequality filter. Phase 1b is now the only one of the three candidate resolutions in this handler that disagrees with the other two.

One line, and it makes all three agree — add to the `entry_model` resolution at `:1000-1005`:

```rust
    // Mirror the commit guard at `:1034` and the budget overlay at `:546`:
    // the entry model is only adopted on a real switch, so judging it on an
    // idempotent PATCH rejects a value that will never be committed while
    // ignoring the one that stays in force.
    .filter(|_| provider != live_pair.provider);
```

I checked it against the existing rows: `empty_model_does_not_commit_the_provider_half`, `patched_model_wins_over_the_target_provider_entry_model` and every sweep cell where the body provider differs from the baseline all switch providers, so the filter never fires on them. The same-provider sweep cells stay 200 either way (live model and entry model are both in the `claude-` namespace). No row moves. Not a merge blocker — no commit leaks in either direction — but it closes the last place in the block where the value judged and the value in force can differ, which is the property the restructure is about.

### The sweep is missing one behavioural class of provider

The provider axis is `None`, `""`, `"nope"`, `"openrouter"`, `"anthropic-pinned"`. Both real providers in that list are either the live baseline or the entry-model provider — the fixture's third, `anthropic` (`:3000-3016`, with `model: None`), never appears. That is a distinct Phase 1b branch: with no entry model the candidate falls through to `live_model`, which is the shape door #2 (`rejected_provider_switch_does_not_commit_the_model_half`) lives on. Concretely, `baseline=openrouter` with `{provider: "anthropic"}` rejects today and is not in the sweep.

Adding `Some("anthropic")` at `:3988` is one line and takes the sweep from 5 to 6 provider shapes, covering the fallback-to-live-model branch the named row currently owns alone. Given the sweep's job is to catch the door nobody thought of, leaving out a whole candidate-resolution branch undercuts it.

### Phase 2 can take one write guard instead of two

`:1107` and `:1117` are now adjacent, which they were not before the split — there is no longer any code between them. Taking the guard once around both makes the pair atomic to concurrent readers, which closes the `GET /settings` tearing I raised last round without locking the read path (which we agreed is not worth it). Rust 2024 let-chains make it read fine, and the file already uses them at `:1063`:

```rust
if pair_ok && (provider_to_commit.is_some() || model_to_commit.is_some()) {
    let mut snap = state.server_llm_default.write();
    if let Some(provider) = provider_to_commit && snap.provider != provider { .. }
    if let Some(model) = model_to_commit && snap.model != model { .. }
}
```

Small, but it makes "all or nothing" true for observers as well as for the disk.

---

## Your two deferred findings — my read

### 1. Whitespace-only model: agree, file it, and name the decision precisely

Confirmed the mechanism: `model_belongs_to_kind` (`configuration/resolution.rs:31`) returns `true` unconditionally for `OpenAiCompatible`, so `{"model": "  "}` clears 1a's `is_empty()`, clears 1b, commits, rebuilds, returns 200 **and persists** — `persist_settings` writes it at `:2060` and boot reapplies it. So unlike the doors, this one survives a restart. Recoverable with one valid PATCH, so not P1, but "every run fails until you notice" plus "sticks across restart" is a fair severity.

Your classification is right: it is a validation-content gap, not commit-before-validation, and `docs/api.md` § 10.2 is not falsified by it (row 2 says "non-empty", and a two-space string is non-empty). Deferring is correct.

One refinement for the issue text. **The two halves are not equally a product call.** Rejecting whitespace-only is the conservative half — there is no runnable model id consisting only of spaces, so nothing legitimate is turned away, and the change is `is_empty()` to `trim().is_empty()` at `:947`. Trim-and-accept is the actual product decision, because it silently rewrites what the operator sent. Framing the issue as "reject or trim?" invites a bikeshed; framing it as "reject whitespace-only (safe, one token), and separately decide whether to trim surrounding whitespace" gives it a default. The only backward-compat surface is that a body previously answered 200 starts answering 422 — for a body that breaks the daemon.

The provider half is incidentally covered: a whitespace-only provider fails `contains_key` at `:929`, so the gap is model-only.

### 2. Session block: scoping right, severity slightly understated

I verified the scoping claim rather than taking it. `context` (`:611-658`, `:660-688`), `tools` (`:746-771`) and `llm` (`:792-836`) are all validate-then-commit — every write sits in an `else` or an `Ok(..)` arm. `:708-731` is the only remaining commit-then-validate site in the handler, and it is byte-identical on the base branch. Both claims hold; not this PR's.

The severity has a wrinkle you did not mention, and it is the more interesting half. **The revert at `:730` fires for any `body.session`, not just one that names `max_context_tokens`** — the cross-check at `:721-731` sits outside the `if let Some(v) = sess_patch.max_context_tokens` block. And the context block raises `max_input_tokens` with only a zero check (`:588-593`), with no reciprocal session check. So:

1. `PATCH {"context": {"max_input_tokens": 500000}}` returns 200.
2. `PATCH {"session": {"max_messages": 50}}` returns **422**, and `session.max_context_tokens` is silently rewritten to 500000 by a body that never mentioned it.

That is a rejected PATCH mutating a field outside its own body, which is a step past "a number the operator never sent". Mitigating: the rewrite is always *toward* the invariant, so unlike the pair door it cannot produce an unrunnable state, and persistence is still gated — so it is live/disk divergence plus an unrequested value, not a broken daemon. P2 is right; the escalation is that the trigger does not require the operator to touch the field at all.

The fix has a template right above it in the same handler: `:611-658` computes candidates, validates the candidate pair, and commits only on success. Worth pointing the issue at that block so whoever picks it up does not reinvent it.

---

## Nits

- Three dangling references, all created by this commit — the same class as `run_telegram` and `Coordinator::with_shared_llm` that this PR has spent two rounds fixing. `:491` names `model_to_commit_with_provider` (now `model_to_commit`), `:524` points at "lines ~988-989" for the commit-path guard (now `:1034`), and `:539` points at "lines ~879+" for the compat check (now `:1013`). The identifier is the one that matters; the line numbers will drift again regardless, and naming Phase 1b instead would be drift-proof.
- `state.rs:178` runs long after the rename to `Coordinator::with_agent_config_and_shared_llm`; rustfmt does not rewrap doc comments, so it stays as written.
- The sweep constructs roughly 48 full `AppState`s (Gateway + scheduler + store), each via `AppState::new` before `data_dir` is repointed. Correctness is fine — the fixture overwrites `server_llm_default` and `llm` after construction — but it multiplies the shared-default-`data_dir` construction pattern the leaked-`settings.json` issue is about. Worth a cross-reference when that one gets picked up, not a change here.

---

Nothing above blocks. The structural change is the correct answer to the fourth door, and I would take it over the fifth flag I proposed. Merging is Alper's call.
