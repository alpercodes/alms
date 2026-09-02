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

- **`workspace_write`'s `mode` now defaults per file: `memories` appends, the other three replace** — #1305. Previously every file defaulted to `"write"` (a flat `.unwrap_or("write")`), so an LLM that omitted `mode` replaced the whole file. For `memories.md` that is the wrong branch to guess, and it reopened #1280's failure mode from the other side: the agent's "read" of its memories is the **prompt build**, so the snapshot a `workspace_write` replaces is at best one tool batch old and at worst as old as the run. (This entry originally said the prompt was assembled once per run and "carried through every tool iteration". It is not — `agent_loop` re-reads the workspace after every tool batch — corrected in #1310, above.) Anything appended inside that window is silently erased, and that needs no second agent: a run that appends a few memories and then tidies up with an explicit `mode: "write"` before finishing erases its own. (Concurrent writers make it worse — the coordinator deliberately permits several parents onto one named agent's workspace — but they are not required.) #1292 and #1294 cannot reach any of it; there is no critical section for a lock to bracket, only a stale snapshot.
  - ⚠️ **This changes what an existing tool call does for every agent already calling it.** A `workspace_write` on `memories` with no `mode` now *adds* to the file instead of replacing it. Nothing else changed: `personality`, `goals` and `user` still default to `"write"`, an explicit `mode` is still honoured everywhere, and `mode: "write"` on `memories` still replaces the file wholesale (with the staleness risk, now taken knowingly).
  - **The accepted cost:** a model that omits `mode` while genuinely meaning a wholesale rewrite of `memories.md` now appends its rewrite after the old content, duplicating entries. What it replaces was invisible and unrecoverable, so preserving data wins the tie regardless, and the duplication is visible in the next context build and repairable with an explicit `mode: "write"` — past 4000 bytes that repair now takes a `workspace_read` first, because #1310 refuses a replacement composed from the injected window (see above). Both halves of that lapsed past the 4000-char injection cap, which was head-anchored — fixed in #1308, below.
  - The tool's `mode` description states both halves of the trade, and the tool result echoes the *effective* mode, so a model that guessed wrong can see it in the same turn. The web UI no longer contradicts that: the request row rendered a fabricated `write` pill for an omitted `mode`, which now disagreed with the result row — it renders no pill at all when the caller sent none, leaving the effective mode as the single answer.
  - Rejecting the stale replacement instead — a compare-and-swap — was considered and deferred here as the more expensive fix. **#1310 has since done it** (above), and corrected this reasoning twice: an agent could always re-read its workspace with `fs_read` (the gap was that no tool did it *by name* and nothing gave the model the path), and #1310 ships a rejection that names its recovery rather than one carrying the file contents, because a payload sized to be safe is not large enough to be sufficient. The default now lives in `WorkspaceFile::default_write_mode`, with all of this recorded there.

### Notable changes

- **Dead-code sweep: seven provably-unreachable items removed.** `cargo clippy -D warnings` already catches private dead code, so this pass targeted what the lint cannot see — `pub` items in library crates. Each removal was established by demoting the crate’s whole `pub` surface to `pub(crate)` and letting rustc name what breaks (cross-crate uses become errors, in-crate-unused items become `dead_code` warnings), then re-confirmed two ways: `#[cfg(any())]`-ing the item out and rebuilding `--all-targets`, **and** a whole-tree text search for the identifier. The second is not redundant — it is the only one that covers doctests, which `--all-targets` does not build. (The workspace has exactly two compiled doctests, both crate-level `rust,no_run` examples, and no `#[doc = include_str!(…)]`, `paste!` or `concat_idents!` anywhere to defeat the search.) Nothing here changes behaviour.
  - `alms_sandbox::ShellExecTool` — a `pub type ... = ShellTool` alias whose own doc said "for backward compatibility. Use `ShellTool` instead". No code referenced it; the four surviving mentions (`shell/mod.rs`, `shell/security.rs`) are prose about the pre-redesign implementation and now say "since removed" so a future sweep does not go looking for a type. **`SHELL_TOOL_ALIAS` / the `"shell_exec"` wire name is a different thing and is very much live** — it is reachable from `tools.enabled` and from agents still emitting the old name, so it is not decided by counting Rust call sites. That distinction is now recorded on the const itself.
  - `alms_sandbox::shell::types::BackgroundTask` — never constructed anywhere; `ShellState.background_tasks` stores `BackgroundTaskResult`.
  - `alms_sandbox::shell::output::truncate_output` and its private `byte_truncate` helper (plus the five unit tests that were their only callers) — fully superseded by the `&[u8]` twin `truncate_output_bytes`, which is what the shell exec path calls. One of those tests was also pinning the *surviving* twin's length boundary, so it was ported rather than dropped: `test_truncate_output_bytes_at_limit_unchanged` now covers `bytes.len() == MAX_OUTPUT_BYTES`. It deliberately uses a multi-line input — a single-line one passes even with the guard mutated to `<`, because the byte-level fallback appends its note only when it actually drops bytes.
  - `alms_runtime::tool_output_truncate::DEFAULT_RETENTION_DAYS` — a stale duplicate of `ToolOutputTruncateConfig::default().retention_days`; the sweep at gateway startup reads the config value, never this const.
  - `alms_gateway::sse::RunEventStream::{stream, stream_with_replay}` — **a footgun, not just a duplicate.** Live SSE responses are built from `RunManager::subscribe_*`, which return a `ManagedSubscription<K>` whose `Drop` unregisters the sender from the subscriber map, so a browser disconnect cannot leave a dead sender behind. These two took a raw `mpsc::UnboundedReceiver` instead and were the only entry points that could build an SSE response with none of that bookkeeping. All six call sites in `runs/streaming.rs` already used `stream_with_replay_source` or `stream_replay_only`. `RunEventStream`’s doc now records why a receiver-shaped constructor should not come back, and stale prose naming `stream_with_replay` in `streaming.rs` and `run_manager.rs` names the surviving function.
  - `crates/alms-gateway/static/ui/hooks/use-local-storage.js` — an orphaned frontend module with no importer anywhere in the tree. What proves it was never shipped is that `server/routes.rs:47` is the workspace’s sole rust-embed `#[folder]` and points at `static/ui-dist/` only, with no Vite `publicDir`, no `import.meta.glob`, and one script tag in `index.html` — so `ui-dist/` is exactly the entry-reachable graph. `ui-dist/` being byte-identical after `npm run ui:build` (CI-enforced in `.github/workflows/ci.yml`) corroborates it, but on its own would not: tree-shaking gives the same result either way.
  - `[workspace.dependencies]`: `tonic`, `prost` (a gRPC transport that was never implemented) and `crossbeam`. The proof is that no member manifest references them — a `tonic.workspace = true` anywhere would be a hard manifest error, not a silent diff — so these are declaration-only removals and no member crate’s dependency graph changes. `Cargo.lock` being untouched corroborates: `tonic` and `prost` never appear in it at all, and the `crossbeam-*` entries that do are transitive, not this umbrella crate.

- **Two spellings of one subagent name are now one subagent** (#6, #12). Agent names resolve case-insensitively (#2), but the name a subagent is *identified* by was still being taken literally in two places, with two different symptoms.
  - **#12, a bug with no issue until it was looked for:** `read_subagent_session` reported that a registered subagent had never been invoked, moments after the same agent invoked it, using the same spelling both times. The key `named_subagent_key` returns was **half-folded** — the `agent_id` half came from a registry lookup that folds case, the `context_id` half interpolated the literal name. `invoke_agent` canonicalized before calling in and the readback did not, so `{"name":"Reviewer"}` wrote to `subagent_{p}_reviewer` and read from `subagent_{p}_Reviewer`. The model was self-consistent; the system was not. This is a tool failure inside a run, not operator-facing duplication — the agent gets a confidently wrong "it may not have been invoked yet".
  - **#6:** an *unregistered* name had no canonical spelling at all, so `Scout` and `scout` forked into two subagents with two sessions, two workspace directories and two chips — while the same two spellings of a *registered* name were one agent. Registering a name should not decide whether two invocations are the same subagent.
  - **One rule, in one place.** `SessionManager::canonical_subagent_name` is now the only place the choice is made: the registry's spelling when a row exists, ASCII-lowercase when it does not. `named_subagent_key` applies it *internally*, on the way in — so the read path cannot forget a step it no longer has to take. Sharing a helper was never enough on its own; #12 was two callers passing different arguments *into* the shared helper, which then faithfully returned two different keys.
  - ⚠️ **A chip the model asked for as `Scout` now renders as `scout`.** Accepted deliberately. Unlike the agent-create form — where the operator authors a durable name once and silently rewriting it would be wrong — an unregistered subagent name exists only as a string the model emitted this turn and may emit differently next turn, which is the defect itself. First-spelling-wins was rejected because it must consult what is already stored, making identity depend on invocation order — the property `named_subagent_key` explicitly documents itself as excluding.
  - **The `active_named` guard folds with the identity, by construction — given a stable registry read.** Collapsing two spellings onto one session *without* collapsing the guard key would admit two concurrent runs against one session history — the failure that guard exists to prevent. The canonicalizer mutates `request.subagent_name`, and the guard insert, its paired removal, the identity derivation, the workspace directory and the chip label all read that one field afterwards. A test holds the guard slot under the folded spelling and requires a case-varied dispatch to be refused; backing out only the request-level fold leaves the session-sharing test passing and fails that one, which is exactly the half-fix worth catching.
    - The qualifier names a real window rather than hedging. The request fold and the key derivation share the *rule* but not the registry *read*, so a transient store error on one and success on the other is the single way they disagree — guard key, workspace dir and label folding one way while the session context folds the other. Left as-is deliberately: it predates this change (the #2 version had the same window), it needs a SQLite failure inside a two-call window, and it is already the declared accepted-fork disposition that `named_subagent_key` documents and now logs.
  - **One inverted behaviour, and it is the point rather than a side effect:** with **no store attached**, a subagent name now folds instead of passing through. Previously a store-less manager returned the name verbatim, which made a subagent's identity depend on whether persistence happened to be configured — a worse thing to condition identity on than case. The fold is a property of the name, not of the deployment.
  - **Existing named-subagent sessions whose name has uppercase are re-homed** and their prior transcript stops being reachable by name — the same accepted break #1278 made, for the same reason (no production deployments, and order-dependent identity is worse to carry forward than one lost transcript).
  - **One fear checked and dismissed, so it is not re-derived:** `{workspace_dir}/{name}/` uses the name, so two spellings share one directory on case-insensitive filesystems (Windows/macOS) and split on Linux — the same asymmetry that motivated #2. It is *not* a new hazard: `AgentWorkspace::acquire_lock` takes an OS advisory lock on a sidecar path, which case-folds along with the filesystem, and concurrent writers on one workspace are already a designed-for condition. The real residue was the shape of the fork differing by platform — two independent subagents on Linux, two transcripts sharing one memory on Windows/macOS — which the fold removes.

- **History-reconstructed tool rows are correlatable with the live event stream again** (#5). `ToolCallRecord` stored only the LLM provider's `tool_id`. The id every consumer actually keys on — the `tool_start` / `tool_end` / `subagent_started` / `subagent_completed` SSE events, the `tool_invocation_id` written into `session_messages` metadata, every frontend identity lookup — was not stored at all, so any chat row the UI rebuilt from `run_tool_calls` was uncorrelatable *by construction*. That is the shared root of a recurring class of chip/row identity bugs, several of which have been point-fixed in `use-session-stream.js` one at a time. Records now carry `tool_invocation_id` end to end (new SQLite migration 5; both inserters, both loaders, both producer sites in the agent loop), and the merge path in `mapHistoryMessages` names its rows with it, falling back to `tool_id` when absent.
  - **What this fixes, concretely:** a `tool_end` can now match a reconstructed row by id instead of guessing; a subagent chip rehydrated from such a row stores the correlator, so PR #3's repair sweep can reach it and its "View session" button resolves through `startedSessionByInvocation` instead of coming up empty.
  - **What it does not fix, and why.** Rows written before the column existed have no correlator to recover — nothing was written down — so they stay provider-keyed and uncorrelatable. That limit is pinned by a test rather than left implicit. The `tool_end` "close the last running tool row" fallback is therefore **not** removed: its population shrinks to pre-#5 records, but the *other* reason the primary match misses is that the row is absent rather than mis-keyed (`persist_assistant_tool_calls` is fire-and-forget, so a dropped write leaves no row for a call genuinely in flight), and no correlator helps when there is no row. The two are indistinguishable at the call site, so narrowing it wants evidence — PR #3's divergence warning is the instrument, and it should get quieter on its own.
  - **PR #3's repair sweep stays load-bearing**; what changed is its reach. It exists for a dropped or gapped `tool_end`, a cause nothing here touches. But it matches `terminalRows` on the row's id, so it could previously only repair chips whose terminal row came from `session_messages`; a merge-path row arrived named with the provider id and was invisible to it. It now covers those too.
  - ⚠️ **One behaviour change ships with this, deliberately.** A tool row with no result used to render `running` whenever *any* run was active on the session — a fact about the session, not about the run that issued the call. Cancel a run while a foreground `invoke_agent` is in flight, then reload while a later run is active, and the dead invocation rendered as running and grew a subagent chip that nothing could ever terminate: the run that would have emitted `tool_end` is gone, and the repair sweep needs a persisted result the row will never have. Such a row now renders `done`. The rule is one-directional and evidence-gated — it only moves `running` → `done`, only when the row's own run is present in the runs snapshot **and** terminal; a run missing from the snapshot is unknown, not assumed finished, and keeps the previous behaviour. `done` rather than `fail` because that is already what the row renders the moment no run is active: the change makes the rendering independent of an unrelated run, it does not add a state.

- **Agent names may now contain capital letters** (#2). `validate_agent_name` restricted names to `[a-z0-9-]`; auditing why turned up a slug convention rather than a decision — nothing depended on the absence of uppercase. The class is now `[A-Za-z0-9-]` and the operator's casing is stored and displayed verbatim: type `Atlas`, get `Atlas`. The two properties the old class was actually carrying are preserved. It is still the gate on an LLM-supplied `invoke_agent` name before that name is rendered as a UI identity label (uppercase adds no `<`, `&`, quote, whitespace, or path separator), and it still rejects UUID-shaped names, which is what keeps the named/ephemeral subagent discriminator disjoint (`Uuid::parse_str` is itself case-insensitive over hex, so the widening moved nothing).
  - ⚠️ **Uniqueness and lookup are now case-insensitive.** An agent's workspace is a directory at `{workspace_dir}/{name}/`, and Windows/macOS filesystems are case-insensitive while Linux is not — so `Atlas` and `atlas` as two registry rows would share one directory on two platforms and split across two on the third, the same Linux/Windows asymmetry documented in `docs/security-model.md` § 4.4. Creating `atlas` when `Atlas` exists is now a `409 DUPLICATE_NAME` (new SQLite migration 4 adds a `UNIQUE INDEX ... (name COLLATE NOCASE)`), and `GET /agents/atlas` resolves `Atlas`. Storage is case-preserving; only comparison folds case. The same rule reaches the CLI, `invoke_agent` and `send_message`, all of which funnel through `load_agent_by_name`.
  - **Callers must use the resolved record's name, not the string they looked up with**, wherever a name becomes durable identity. Two places did not and were fixed in the same change: `send_message` handed `MessageBus::send` the raw `to` from the model (which would have filed `dm:Atlas:bob` and `dm:atlas:bob` as two DM sessions for one pair of agents), and `invoke_agent` names now canonicalize against the registry at `spawn_subagent` before anything keys off them — the subagent `context_id`, the `active_named` concurrency guard, and the workspace directory would otherwise fork on the model's spelling. This closed the fork for *registered* subagent names only; unregistered names were deliberately left byte-for-byte, and **#6 (below) has since closed that too**, folding them together with the `active_named` guard key in one change.
  - **Reserved names (`default` / `dm` / `workspace`) and the `[security].allow_full_os_access` list are matched case-insensitively too.** Reserved names fold for coherence with the uniqueness rule, not because an uppercase spelling could reach the space being reserved — it could not, since every consumer of the `dm:` and internal prefixes matches them case-sensitively and none derives a prefix from an agent name. Folding the security list is likewise not a widening, though **not** for the reason uniqueness suggests: that list is also consulted with an *unregistered*, model-supplied `invoke_agent` name, where uniqueness does not apply. It adds no capability because `invoke_agent` names are validated before reaching it and were lowercase-only before this release, so a model that wanted a listed entry could always type its exact spelling — folding adds spellings that reach an entry, not entries the model could not already reach. What it does change is that an entry an operator wrote with capitals was previously dead and is now live, which is the near-miss being closed: `alms.toml` saying `atlas` while the agent is `Atlas` silently granting nothing.
  - **Client side:** the agent-create form's normalizer no longer lowercases (it would have silently rewritten the operator's `Atlas` with no visible trace — the preview line only renders when normalization changed something), and the first-run onboarding form's own regex, which would have rejected `Atlas` outright, was widened to match. Both mirrors of the reserved-name and UUID-shape checks are now case-insensitive. The four browser-side agent-name comparisons that decide *identity* — two deriving a DM peer, two deciding cross-agent row ownership — now fold case through one shared helper, mirroring the way `dm_peer` is the single participant-matching rule on the Rust side. ⚠️ **One deliberate user-visible change comes with that.** Deriving the DM peer used to answer "the active agent is not a participant" with the *first* participant. Opening a DM between two other agents is a supported click — the sidebar shows non-owned DM rows and opening one deliberately does not switch agents — so that guess put "Chatting with alice…" in the status bar of an agent who is not in the conversation, which is the #1166 failure (a header naming someone on a guess) in miniature. Such a peek now shows the neutral running phase instead. Nothing goes from correct to neutral: every label the old fallback produced on this path named a conversation the active agent was not in. Existing all-lowercase names are unaffected; no data migration is needed.

- **A DM reasoning collapsible is no longer labelled with whichever agent the sidebar happens to be on** (#1166). A DM tool row carries no attribution of its own — the name the operator reads is the header of the `dm_reasoning` block the tool is grouped into, and `run_started` was deriving that header from render state it had already deleted. It recovered the run's `peer:` source by scanning `chatMessages` for **any** queued `thinking` row, then fell back to `activeAgent`. Three ordinary inputs left it with nothing: `loadSession` reconstructs a queued run's indicator with no `source` (it never had one — opening a DM mid-conversation means that run's `run_created` sits at or before the history cursor and never replays); two runs queued at once made the run-id-blind scan read the *first* row's source; and `sealLastAgent`, which every `dm_message` and every run end calls, sweeps queued rows run-id-blind, so a run queued behind a busy agent arrived with no row at all. `activeAgent` is never a safe guess on a DM: opening one deliberately does not switch the active agent, so it is the peer about half the time and, when the operator is parked on a third agent, names someone who is not in the conversation. Reload was always right (it reads `from_agent` off the persisted rows), which is why this only ever showed live and healed on refresh. The run's source is now held off-render for the life of the stream, `run_started` routes both its lookup and its sweep by `run_id`, and the reconstructed indicator carries the run's owning agent taken straight from the run record's `agent_id`. **No wire or config change.**
  - ⚠️ **One case goes from named to unnamed:** a DM collapsible whose source cannot be recovered now renders the neutral "Agent reasoning" where it previously rendered a name. Nothing goes from correct to neutral — `POST /runs` is rejected outright on a `dm:` session, so every name the old fallback produced was a guess — but a **subagent-completion notification run landing on a DM session** is a real shape that used to be labelled and now is not.
  - **Partially fixed, declared in-code:** this closes the *attribution* consequence of those blind sweeps, not the queue chip itself. `sealLastAgent` and `flushDeltaBuffer` still drop every `thinking` row unconditionally, so a still-queued run keeps losing its "Queued — position N" chip and `run_queue_position` (#831) still finds nothing to decrement. That fix rescopes two shared non-DM helpers and is tracked on #1321.
  - **One latent route closed while in there:** in the `tool_start` chain the `invoke_agent` arm sat *above* the `source_agent` drop, so a subagent-tagged `invoke_agent` — alone among tagged tools — rendered as a parent tool row, and in a DM that is a tool escaping the collapsible with the DM gate **true**. Unreachable until recursive subagent spawning ships; the arm is now gated so arm order is no longer load-bearing.

- **Tool re-registration no longer logs on the happy path, so WARN is worth reading again** (#1260). The tool registry warned on every replacement, and a run rebuilds its registry from scratch: `AgentRuntime::new` registers the builtins, then `attach_fs_cache_to_registry`, `with_shell_default_env`, `with_shell_spill`, `with_tool_output_truncate` and `with_project_root` each rebuild `shell` and the `fs_*` family with a narrower sandbox root, a file-state cache, or the run's spill directories. That is **29 WARN lines per run** for the intended lifecycle — 606 in one afternoon's testing, against which a genuine `shell` bug (#1255) sat unread for fifteen occurrences. A warning that fires on the normal path has no signal value and costs the warnings that do. The registry now warns **only when the implementation behind a name changes**: a new `ToolIdentity` supertrait (blanket-implemented, so no `Tool` impl changes) gives `dyn Tool` its concrete `TypeId`, and a replacement is silent when the incoming tool is the same concrete type reporting the same canonical `name()`. Reconfiguring a tool is silent; a different type claiming an established name, or an alias re-pointed at a tool that calls itself something else, still warns — now with the displaced and incoming type names on the event so it is actionable.
  - **The `info!` a level down went with it.** Every registration also logged `"Successfully registered tool: {name}"` at INFO — the same non-event as the `debug!` immediately above it, ~35 lines per run at the default level. Removed; the `debug!` and the one-line `"Registered built-in tools"` summary remain. A run's whole registration churn is now 1 WARN and 5 INFO lines (0 WARN on Linux, where the shell-sandbox platform notice is an INFO).
  - **Scope:** the `shell_exec` alias was the one insert that bypassed both checked entry points — it wrote straight into the map behind an open-coded copy of the `tools.enabled` check — and now goes through `register_as` with the same filter semantics. Not addressed: the per-run rebuild itself (the issue's third option). It is load-bearing, not incidental — the tool instances are bound to per-run state (`run_id`-scoped spill directories, the run's cancel token inside `invoke_agent`, a per-run `FileStateCache`) — so a shared registry needs per-run scoping, not just hoisting. No config knob, no wire change.
- **`read_session` and `read_subagent_session` stop silently returning 20 messages, and now say what they left out** (#1032). #1028 fixed this for `read_messages`; its two siblings kept the same defect for another three months. Both parsed `last_n` as `params.get("last_n").and_then(|v| v.as_u64()).unwrap_or(20)`, so an omitted `last_n` returned the last 20 messages of a 500-message session **and said nothing** — the response's `message_count` reported the real total, but nothing distinguished "that is the whole session" from "there are 480 more above this". An agent asking for a subagent's transcript got a silently-cropped one and had no way to detect it. The same expression also swallowed *malformed* input: `{"last_n": -1}`, `3.5`, `"20"` and `true` all fell through to 20, so a caller paging deterministically could not tell its request had been discarded.
  - Both tools now return the `total_count` / `returned_count` / `truncated` / `truncation_reason` contract alongside the existing `message_count` / `showing` keys, which are kept for back-compat and still agree. An omitted `last_n` returns **everything**, bounded by a 60,000-serialized-byte cap and a 200-message backstop; an explicit `last_n` is still honoured verbatim but is flagged `truncated` when older messages exist, so "you asked for 5" and "there are only 5" stay distinguishable.
  - ⚠️ **Two behaviour changes for existing callers.** A call that omitted `last_n` now returns the whole transcript instead of the last 20 — bounded, but larger. And a **malformed** `last_n` is now an `InvalidParameters` error instead of a silent 20; the JSON schema pins the type too, but schema enforcement at the LLM layer is best-effort, so the runtime validates.
  - The caps are measured against the **serialized JSON** entry, not raw UTF-8 — the #1028 P1 lesson: content full of `"`, `\` or newlines costs the model its post-escape size, and a raw measurement admits roughly twice as much as intended.
  - `read_subagent_session`'s `summary_only` fallback (recent messages when no summary exists) carries the same fields. It keeps its own much smaller cap — it is a consolation prize for a missing summary, not a transcript read — but now *reports* it, and reports it honestly: an explicit `last_n` **above** that cap is not credited as `explicit_last_n`, because the caller's number is not what limited the result. `read_session`'s `summary_only` returns no message array at all, so it deliberately carries no counts.
  - The contract now lives in one place, `alms_tools::session_read`, and all **three** tools go through it — including `read_messages`, which migrated with all 25 of its existing tests unchanged. The issue proposed migrating one sibling at a time and deciding on a shared module afterwards, on the premise that the tools' output shapes differ too much. They do differ, but every difference (DM sender attribution, marker filtering, the fallback shape) sits *outside* the selection — in the projection closure, in which slice is passed in, or in the cap value — so the boundary was knowable without a trial migration. No wire removals, no config knob.
- **A replacing `workspace_write` is now refused when it would delete text the agent has not been shown, and a new `workspace_read` tool is how it gets shown** (#1310). #1305 changed what an *omitted* `mode` means; this is the branch the agent asks for explicitly. `mode: "write"` sends a whole file, and the only copy the model has is the one in its context — which differs from the file on disk in three ways it cannot detect. It may be a **window**: past the 4000-byte injection cap the prompt carries the tail of `memories.md`, so a replacement built from it deletes everything above the cut. That one needs no concurrency, no second agent and no unusual sequence — it is the steady state of every memories file that has grown past the cap. It may have been **never shown at all**: `user.md` is omitted from `dm:` / `subagent_` / `job_` / `notifications:` prompts *and* defaults to `"write"`, so in those runs the ordinary no-`mode` call was a blind whole-file erasure. Or it may have **changed since**: another live instance of the same named agent, an operator editing from the UI, or the run's own earlier `workspace_write` calls in the same tool batch.
  - `AgentWorkspace` now records what it hands the agent — the prompt injection, a `workspace_read` result, and the agent's own successful replacement — and compares that record against the file, **under the file's sidecar lock**, before staging anything. A refusal is an in-band `{"ok": false, "refused": "never_shown" | "shown_partially" | "changed_since_shown", "error": "…"}` result: the loop continues, the model gets a structured reason, and `tool_result_ok` already counts a top-level `error` as a failed call, so nothing records it as a success.
  - ⚠️ **This changes what an existing tool call does.** A `mode: "write"` that used to land can now come back refused. It is refused *only* when it would actually destroy something: a missing, empty or blank target is written unconditionally (a fresh agent bootstrapping its `personality.md` never meets the guard), `mode: "append"` is never checked because an append cannot delete, and the operator's `PUT /agents/{id}/workspace/{file}` is exempt — the operator is the authority on their own workspace.
  - **New tool: `workspace_read`** — `{file: "personality" | "goals" | "memories" | "user"}` returns that file as it is on disk, capped at 12000 bytes (three times the injection cap, and sized so the *serialised* result — the truncator measures `value.to_string()`, and JSON escaping costs up to 2 bytes per byte — still fits inside `tool_output_truncate`'s 32 KB default, so a second truncation does not land on top of it). It is registered next to `workspace_write` by `with_workspace` and, like every other tool, is subject to a `tools.enabled` allowlist. **The refusal message therefore names `mode: "append"` first** — that is the same tool the agent is already calling, so it is reachable whenever the message is — and `workspace_read` second, for the case that genuinely needs the whole file. A capped read reports `complete: false` and does **not** unblock the replacement: a read that laundered a 12 KB window into permission to replace a 40 KB file would reintroduce the defect through the fix.
  - **A whole view survives the prompt rebuilds that follow it.** The prompt is rebuilt after every tool batch, and for an over-cap `memories.md` that rebuild computes "the agent has seen a window" every time — so an unconditional re-record would overwrite the `whole` view a `workspace_read` had just established, on the one rebuild that is guaranteed to land between the read and the write (they cannot be the same batch, because the model needs the read result to compose the replacement). Left alone, the advertised recovery would refuse forever for the entire 4001..=12000 band, which is `shown_partially`'s whole recoverable population. The injection recorder therefore keeps an existing `whole` view **when the bytes still match**; if the file moved, the contents differ, the fresh record lands, and the refusal fires exactly as it would have.
  - **Two corrections to what #1305 and #1308 recorded.** (1) There *was* always a way to re-read a workspace file — the workspace lives at `<project_root>/.alms/agents/<name>/`, inside the sandbox root, so `fs_read` reaches it by path. What was missing is that no tool read a workspace file *by name*, nothing handed the model the path, and `fs_read` is not guaranteed enabled — which is why the fix ships its own read rather than pointing at that one. (2) The system prompt is **not** fixed for the whole run: `agent_loop` calls `rebuild_system_prompt_for_tool_loop` after every tool batch, which re-reads the workspace and replaces the system message outright. So an agent that appends in one batch and replaces in the next has been re-shown the file in between, and is not refused. What a rebuild does not cover — and what the guard is for — is a single batch containing both, a concurrent writer landing after the last rebuild, and the two view problems (windowed, never shown) that no rebuild can fix.
- **An agent past the memories cap now sees its *recent* memories, not its oldest** (#1308). `build_system_prompt_prefix` capped the injected `memories.md` at 4000 chars with `truncate_to_char_boundary`, which keeps the **first** N — a head-anchored window. That was survivable while `workspace_write` replaced the file; #1305 made an omitted `mode` **append**, which is right for the data loss it fixed, but the file then grows at the tail while the read window stayed pinned to the head. Past the cap that is not a size limit but a write-only memory: nothing the agent wrote after the cap ever reached its context again, and the oldest entries became permanent regardless of whether they were still true. The window is now **tail**-anchored — the last 4000 bytes, via a new `alms_core::tail_to_char_boundary` and a named `MEMORIES_INJECTION_CAP`. **The marker moved to the front and now states a consequence, not just a size:** `[Older memories truncated: showing the most recent N of M bytes. This is the end of memories.md, not the whole file -- writing this text back with mode "write" would delete the older entries above the cut.]`. That sentence is the fix's other half. #1305's documented repair for an over-appended file is to resend it with an explicit `mode: "write"`, composed from whatever is in the system prompt. (This bullet originally said no tool read a workspace file back by name and that the injection was therefore the agent's only view of its memories. The first half was true and is no longer — see #1310 above, which adds `workspace_read` and refuses a replacement built from a window. The second half was always too strong: `fs_read` could reach the file by path.) An unannounced window makes the repair a deletion; tail-anchoring alone would just change which half got deleted. A partial *leading* line is also dropped, but **only when the window actually opens mid-entry** — a head window could only end mid-entry (visibly cut off), while a tail window begins mid-entry, and half an entry read from its middle can assert the opposite of the entry it came from; an already-aligned window keeps the entry it opens on. **No config knob, no wire change**, and files under 4000 bytes are injected byte-for-byte as before. One stale claim removed while in there: the code comment on the cap said memories "will be properly budgeted by ContextBuilder" — `ContextBuilder` never trims a system prompt, it measures it and shrinks *history* to pay for it, so the cap is the only bound this text has ever had.
- **An interrupted DM end is no longer invisible to the agent** (#1300). Since #1258 an end that was *cut short* — the operator cancelled the run, or it died mid-turn — starts no notification run on the trigger's own target, which was the right fix for a spurious spinner appearing 470ms after a cancel. What went with the run was the news. Everything else the end leaves behind is hidden from the agent by a *different* mechanism: the `dm_ended_notification` marker is `Role::System` + synthetic, so the pre-provider strip removes it, and the bus's `dm_ended` row is empty-text, so `is_synthetic_marker` hides it from both `read_messages` and `read_session`. There was therefore **no observation an agent could make** that distinguished "the reply is still coming" from "the end was interrupted" — the operator was told, the agent was not, and asking it "what did the peer say?" got an agent that did not know the conversation had closed. The routing plan now returns a record in place of the run it suppressed, persisted with `persist_error_marker` (#874) onto **the session the run would have used** — not the operator's chat, which already has its own display-only copy, and not a separately-resolved one, so the record still lands when the agent has no user-facing session at all. `kind: "error"` is the one marker shape the runtime rewrites into a surviving `[Error] …` user message, so it reaches the model on that session's next turn. Its text names the peer, says the run was cancelled or why it failed, says no reply is coming, and points at `read_messages`; the transcript is deliberately **not** inlined the way a notification run's input inlines it, because this text is re-injected on every later turn rather than consumed once. **#1258 is untouched:** no run is created, nothing is woken, and there is no post-end turn to guard. The record fires only on the arm that produces no run — an interrupted end that resolves an open job episode (#1198 / #1205) still gets its continuation, and that run's input already states the interruption in prose, so a record there would be a second copy of the same sentence. The operator's *delivery* is unchanged — same banner, same persisted marker, same `detail` line — but on the routings where the record lands in a chat the operator is watching, that chat now shows a second entry: the record renders through the UI's existing `kind === "error"` branch, as a red error block under the banner. They are different records for different readers with different text (the banner is display-only and never reaches the model; the record is the agent's copy and does), which is the split `markers.rs` documents for every other lifecycle event — the #1258 suppression is what removed the second one. Suppressing the banner instead would regress #1258's own delivery contract. **One agent-facing wording change:** #1297 deliberately kept `send_message`'s description and delivered-note absolute about being notified when a peer ends the conversation, *because* no remedy existed and a hedge would have left "go check `read_messages`" as the only lever — the polling instinct #1111 exists to close, and one that provably could not detect this case. Both strings now name the second delivery ("by a run, or, when that end was itself cut short, by a note carried into your next turn in this session"), which adds a fact and no lever. No wire change, no config knob.
- **The turn an agent gets after a DM ends can no longer message the peer whose conversation just ended** (#1299). `MAX_DM_DEPTH` bounds a *conversation*; nothing bounded conversations between a *pair*, because `end_conversation` clears the depth counter and the sweep tombstone together — so the next `send_message` between the same two agents restarted at depth 1. The post-end turn was where that re-entry was immediate and unattended: `ConversationEnded` runs are not peer messages, so `send_message` was registered with no fold at all, and the turn that hands the agent the full transcript left it one call away from re-opening the same conversation, indefinitely. Those runs already had half this treatment — the DM addendum is withheld from them on exactly the "not a peer message" reasoning — and this is the other half. The fold is keyed on the peer carried by the `ConversationEnded` trigger rather than on the session's context id, and it is stamped onto **every** run the trigger produces: the trigger's own target and each #1198 / #1205 job-episode continuation. That matters because the job arm lands on a `job_*` session that names no peer, survives the #1258 interrupted-end suppression (so it can be the only run an end produces), and re-opens with nobody watching. **The fold removes exactly one recipient for exactly one turn** — everything else the post-end turn exists for is unchanged (#556, #1215): reporting to any third agent, updating goals and memories, reading the transcript. Nothing prevents the pair re-opening later by ordinary means, and **no pair-level re-open counter was added** — the decision, and why such a counter's false-positive population is the legitimate one (a recurring job whose purpose is periodic two-agent coordination), is recorded in `docs/layer2-peer-messaging-design.md` § 14.3. No wire change, no config knob. The one agent-visible difference: a `send_message` at the ended peer now returns a non-error "not sent" result whose note says the conversation has ended, instead of delivering.
- **`read_session` no longer serves a subagent transcript to the agent the work was delegated *to*** (#1298). `read_subagent_session` authorizes on the parent embedded in the `context_id` (#1181/#1185); `read_session` authorized on `session.agent_id`, which #1288 moved onto the invoked agent's registry id — so for exactly the rows one tool refused, the other granted. Latent rather than open (`list_my_sessions` filters `subagent` contexts out, so there was no supported way to learn the id), but authorization-without-discovery is a weaker guarantee than an access check, and `session.agent_id` cannot answer the question at all now: every parent invoking the same **registered** named subagent files under that one registry id, so it no longer separates one parent's delegation from another's, and on the other two arms (an unregistered name, an ephemeral subagent) it names no agent at all — so the over-grant existed on the registered-name arm alone, and the grant there was to the delegate, not to other parents. The rule is now stated **once**, in `alms_core::subagent_session_access` — *a subagent session belongs to the parent named in its `context_id`, never to the agent whose id it happens to be filed under* — and both tools call it, so they grant, refuse, and word their refusals identically for the same bytes. `delete_agent`'s cascade already read the same ownership (#1295) and is unchanged. Also unchanged: the #1185 hardening the fix had to preserve — the session UUID is still not a bearer capability, and legacy pre-#1185 `subagent_{task_id}` contexts are still denied to everyone, the parent included. No wire change, and the parent's `read_subagent_session` path is the documented door and behaves exactly as before. **One operator-facing consequence:** the two tools are now a single access surface under two names, so a `tools.enabled` allowlist naming `read_session` but not `read_subagent_session` — which previously left the parent with no door onto a subagent transcript, because `read_session` denied it too — now provides one. The access check is identical either way; only reachability changed. The default (`tools.enabled` empty = all enabled) is unaffected.
- **The agent-to-agent tool descriptions now say which relationship each tool creates, and stop teaching agents to poll** (#1296 / #1111 / #1112). No behaviour or wire change — the strings the LLM reads. `invoke_agent` advertised `'reviewer'` as its worked example for `name`, which is the exact case `docs/layer2-peer-messaging-design.md` § 9.1 gives for preferring `send_message`; and `send_message` described itself as "fire-and-forget… use `read_messages` to check the conversation later", which made the DM path read as send-then-poll-and-hope. It never was: a DM *invokes* the recipient, their reply invokes the sender back on the DM session, and an end that completes invokes the sender for that outcome too (#1154 / #384) — none of it reachable by polling. (An *interrupted* end — cancelled, or the peer's run died mid-turn — deliberately starts no run on the trigger's own target, #1258, though a resolved job episode's continuation still fires on the job session, #1198; the module doc in `send_message.rs` records why the agent-facing strings stay absolute about it anyway.) `invoke_agent` now states the subordinate relationship (the work belongs to the caller's run; only the caller can read the transcript) and points at `send_message` for peer requests; `send_message` states the triggered-run mechanic in both its description and its delivered-result note; `read_messages` describes itself as a transcript reader rather than a way to wait. § 9.1 was re-audited against shipped behaviour in the same pass: its rule survived, three rows of its comparison table did not (background `invoke_agent` is notified, not polled; a **named, registered** subagent's session row moved under the invoked agent in #1288 while ownership stayed with the parent, the ephemeral/unregistered path keeping its derived key; the #1154 completion gate scopes what "never silence" covers, and a DM recipient no longer "may or may not respond"). The one genuine ergonomic difference — `invoke_agent` returns inline, whereas a DM sender's continuation happens on the *DM* session — is stated rather than papered over.
- **An agent's memories survive a concurrent write to its workspace** (#1280). `AgentWorkspace::append_file` was an unlocked read-modify-write (`read_to_string` → `format!` → `fs::write`), and a named subagent resolves to the *same* workspace directory as the agent it was invoked from — a path the coordinator deliberately allows several parents onto at once. Two appends that overlapped therefore left only one of them on disk: no error, no warning, and a still well-formed `memories.md`, so the loss was durable and invisible. The append now runs under an exclusive advisory lock (a sidecar `.{file}.lock`, so nothing ever locks the data file itself and readers are never blocked) and writes through an append-mode handle, so it lands at the file's current end and never rewrites bytes that arrived after the call started. Two runs writing memories at the same time still do so without seeing each other's entries — serialising the *runs* is a separate question, tracked on #1278.
- **The two *replacing* writers to a workspace file are serialised and atomic too** (#1294). #1280 locked `append_file`; the writers that replace a file were left bypassing it — `AgentWorkspace::write_file`, which is what the `workspace_write` tool took **by default** for every file at the time (its `mode` parameter read `.unwrap_or("write")`, so an LLM that omitted it landed there — #1305 has since moved `memories` to append by default, leaving the other three here), and `PUT /agents/{id}/workspace/{file}`, which reached past `AgentWorkspace` for the path. Both now take the same sidecar lock, so a replacement can no longer land inside an append's observe-then-write cycle and silently erase the memory it was writing; a lock that cannot be taken *fails* a replacement rather than proceeding without it (an append is non-destructive unserialised, a replacement is not). Both also stage the new content beside the target and rename it into place instead of truncating the target first: `std::fs::write` opens with `O_TRUNC`, so the file was *empty* for the length of every edit, and `read_file` maps an empty or unreadable file to "no memories" — a run building its context in that window got a system prompt with the agent's memories partly or wholly missing, with no error on either side. Editing a workspace file from the UI could do this to a live run.
  - **Operator-visible on Windows only:** a replacement is now a rename, and `MoveFileEx` needs delete access to the target, so a `PUT` (or a `workspace_write`) can fail with `IO_ERROR` while an outside process holds the file open without `FILE_SHARE_DELETE` — where the old truncating write would have succeeded. Deliberate: a visible failure the caller can retry beats an invisible torn read. Retry the write, or close whatever is holding the file.
- ⚠️ **A named subagent's session is now filed under the invoked agent, and
  appears in that agent's own sidebar timeline** (#1278). Previously the
  session was keyed on `AgentId::deterministic(parent_agent_id, name)` — an id
  matching no registered agent — and `GET /sessions` excluded subagent rows
  outright, so an agent invoked as somebody's subagent had its work show up
  nowhere. It is now keyed on the invoked agent's **registry id** and listed,
  labelled with the invoking parent (new `parent_agent_id` field on subagent
  session envelopes). The `context_id` is unchanged, so the `read_subagent_session`
  parent-ownership check is unaffected: reading a subagent transcript by
  session id still requires being the agent that *invoked* it, not the one
  that ran it.
  - **Breaking, no migration: existing named subagent transcripts are
    abandoned.** The next `invoke_agent` for a given name resolves the new key
    and starts a fresh session; the old rows stay in the database, unreachable
    by name and grouped under no agent. Accepted deliberately — ALMS has no
    production deployments — rather than shipping migration code for an
    obligation nobody has.
  - Unregistered names and ephemeral (unnamed) subagents are unchanged: neither
    has a registry id to be filed under, so both keep their previous keys.
    Ephemeral sessions also stay out of the listing — they have no agent whose
    timeline they could join, and one sidebar row per one-shot call would be
    noise. They remain reachable by session id via `GET /session/{id}`.
  - Because a named subagent run now *is* the registered agent, its **runs**
    are filed under that agent too, and so appear in `GET /runs?agent_id=…`.
    (Its *episodic summaries* are not: no `session_summaries` row has ever
    been written for a `subagent_` context, before or after this change.)
  - **Episodic memory is no longer loaded on any subagent run.** The read
    side had no session-type gate, so filing the run under the invoked
    agent's id would have injected that agent's summaries of its own
    operator chats, Telegram threads, DMs and scheduled jobs into a context
    whose output goes straight back to the invoking parent as the
    `invoke_agent` result — a new read path from one agent's private history
    into another's, with no tool call needed. The gate restores symmetry
    with the write side, which has always excluded subagent sessions. Also
    saves `run_summary_budget` (15% of `max_input_tokens`) on every named
    subagent run.
  - **Deleting an agent no longer destroys other agents' subagent
    transcripts.** `delete_agent`'s cascade selected sessions by `agent_id`
    alone, so once a named subagent session was filed under the invoked
    agent, deleting that agent would have hard-deleted the *invoking*
    parent's transcripts, runs and audit events. Ownership for the cascade
    now reads the parent out of the `context_id` — the same record
    authorization already reads — so deleting the invoked agent spares them
    and deleting the parent takes them, wherever they are filed. Unlike the
    keying break above this was not a one-time upgrade cost:
    `DELETE /agents/{id_or_name}` is a repeatable operation.
  - Side effect of the same fix: subagent sessions with no registry agent —
    ephemeral ones, and named ones whose name was never registered — are now
    cleaned up when their parent is deleted. Previously no delete path
    enumerated them at all and they accumulated indefinitely.
  - Named subagent rows in the sidebar do **not** offer a delete control.
    They are read-only surfaces whose lifecycle the coordinator owns, and
    `DELETE /session/{id}` does not check for an active run.
  - **`alms session list` gained a `TYPE` column** (#1289). The `--agent`
    path shows the agent's named subagent transcripts since #1278, and
    nothing in the table distinguished one from a chat. The listing is
    otherwise **deliberately uncurated**, and does not adopt
    `GET /sessions`' exclusions: on the `--agent` path the two surfaces
    already return the *same* subagent rows (named in, ephemeral out) by
    different mechanisms, and that path never curated anything to begin
    with — `load_sessions_by_agent` has no type filter, so episodic,
    notification and job rows have always listed there. `--json` gained
    the matching `session_type` field, plus `parent_agent_id` on subagent
    rows (same names as the HTTP envelope), and `alms session show` now
    prints `Invoked By` for a subagent session — and, routing through the
    same enrichment, carries the two new fields on `session show --json`
    as well.
  - ⚠️ **New rejection: `POST /runs` against a subagent session is now
    refused with `400 SUBAGENT_SESSION_NOT_DIRECTLY_RUNNABLE`** (#1289) —
    the same treatment DM sessions got in #1156, for the same reason.
    Subagent turns are produced by `invoke_agent` → the coordinator, which
    alone records the parent linkage and returns the result to the awaiting
    parent; a run created through `POST /runs` writes into a
    coordinator-owned transcript and is delivered to nobody. Deliberately
    new rather than restored: before #1278 the request was already accepted
    whenever `agent_id` was omitted. The web UI never offered this path
    (subagent sessions render read-only), but `alms run create --session`
    did, on a session id `alms session list --agent` now surfaces.
  - **CLI error messages from `POST /runs` now render instead of
    dumping raw JSON** (#1289). `parse_api_error` read only the nested
    `{"error": {"message"}}` envelope, so the handlers that build a
    **flat** `{"error_code", "message", ...}` body directly — the DM
    guard (#1156), the new subagent guard, and the queue-full /
    shutdown admission errors — printed the whole envelope at the
    operator instead of the sentence written for them. Pre-existing;
    fixed here because the subagent guard's only reachable client is
    the CLI.
  - **Subagent work in `GET /agents/{id}/timeline` is confirmed intended,
    no behaviour change** (#1289). A named subagent's runs and messages
    appear in the *invoked* agent's timeline — that is the feature #1278
    delivered — and not in the invoking parent's, which keeps the timeline
    from becoming a read path around `read_subagent_session`'s
    parent-ownership check. Ephemeral subagents appear in no timeline,
    matching their exclusion from `GET /sessions`. Recorded in
    `docs/api.md` § 12.1 and on `load_timeline_events`.

- **The server-default LLM model / provider no longer needs a restart** (#1148).
  Changing `model` / `provider` in the Settings modal (or via `PATCH /settings`)
  now takes effect on the **next run**, matching the `context` / `session` /
  `tools` / `llm` sections. Previously the pair was persistence-only: it was
  written to `settings.json`, the response carried `restart_required: true`,
  and the UI showed a yellow "restart required" banner. Both wire fields
  (`restart_required`, `restart_reason`) and the banner are **gone** — clients
  reading them should stop; the response is now always a plain
  `{"status": "ok"}`. Agents carrying a per-agent `model` / `provider` on their
  registry record are unaffected as before; only agents falling back to the
  server default move. Runs already in flight keep the model they started on,
  and the pair is still persisted for restart survival.
  - `GET /settings.base_url` now reports the live client's URL. It was stale
    after a persisted provider switch even across a restart, so a daemon could
    report the old endpoint beside the new provider name.
  - ⚠️ **New rejection:** a `PATCH /settings` that would leave the server
    default with a model from another provider's namespace is now refused with
    `422 INCOMPATIBLE_MODEL_FOR_PROVIDER` instead of being accepted and biting
    on the next restart. (It is a namespace check — `claude-*` on Anthropic
    wires, `gemini-*` on Gemini — not a model catalogue.) This already applied to provider switches; it now also
    covers a **model-only** PATCH (e.g. `{"model": "gpt-4o"}` while the
    server-default provider is `anthropic`). `OpenAiCompatible` providers —
    OpenAI, OpenRouter, DeepSeek and friends — accept every namespace and are
    unaffected. Send `model` and `provider` together for a cross-namespace
    switch.
  - ⚠️ **Mixed-section rejections leave the pair applied but unpersisted.** If
    one body PATCHes the pair *and* some other section, and only the other
    section fails, the `422` still leaves the new pair on the live client while
    `settings.json` keeps the old one — so a restart silently reverts it.
    Re-send `model` / `provider` alone and check for a `200`. A body rejected
    for the pair itself commits neither half.
  - **Telegram-triggered runs still use a boot-time snapshot** and pick the new
    pair up only after a restart. This is the same documented HTTP-vs-Telegram
    propagation split that already applies to every other live-mutable section
    (`docs/api.md` § 10.2), not a new limitation.
- **A cancelled or failed DM no longer starts a run on your session** (#1258). When a DM ended because its run was cancelled or died (an upstream 429, a tool panic, a posture trip), the gateway started a fresh LLM turn on the operator's web-chat to tell the agent about it. Cancelling a run and then watching a new spinner appear ~half a second later on the same session was indistinguishable from the cancel having been ignored. Such an end now arrives as the existing "DM conversation with {peer} ended" banner — persisted, so it survives reload — and spends no turn. The banner gained a `detail` line carrying the failure text (`dm_conversation_ended` SSE + `dm_ended_notification` marker metadata, both optional and absent for non-failure ends), since no turn narrates the failure any more. DM ends whose run *completed* still get their notification run, because they carry a transcript the agent has to relay: `ignored` and `depth_exceeded`, and also a failed end where the run finished but produced nothing deliverable or could not deliver its last reply — those may follow several real exchanges that live only in the DM session. Job-episode continuations (#1198) also still fire, so a job awaiting a DM is never stalled by the change. One consequence, **since fixed by #1300 in this same release** (see above): after an interrupted end the *operator* was told and the *agent* was not — the DM transcript stayed in the DM view, but the agent had no signal the conversation had closed. It now gets a `persist_error_marker` record on the session the suppressed run would have used. Still no run: the spurious-turn fix described here is unchanged.
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
