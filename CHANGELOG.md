# Changelog

Release notes for ALMS, with an emphasis on **operator-facing changes** — default flips,
configuration and wire-shape changes, and upgrade impact. Implementation detail lives in
the git history.

## v0.2.4 — unreleased (`develop`)

### ⚠️ Default changes — read before upgrading

Each item below changes behaviour for a deployment that has not set the knob explicitly.

- **Agent-loop hard caps now default ON, and the run-duration guard is inactivity-based.**
  `[llm].max_iterations` defaults to `500` and bounds a single run of any type — web chat,
  scheduled job, or subagent. A run is terminated when it stops making *progress* rather
  than on flat wall-clock, so a long but productive run is no longer clipped. New `[llm]`
  knobs: `between_iterations_secs` (default `180`) and `tool_phase_ceiling_secs` (default
  `900`); the absolute backstop `max_run_duration_secs` rose from 4h to 24h.

  **After upgrading**, a previously-unbounded deployment will end any run exceeding 500 LLM
  calls, stalling past a phase budget, or running 24h as `failed`. A stalled run carries its
  own session label, **"Agent stopped after stalling (no activity)"**, so it is
  distinguishable from a crash. Set any knob to `0` to disable that cap. All are
  config-file-only — not mutable via `PATCH /settings`, no env override, restart required.
  See `docs/config.md` § "Agent-loop hard caps".

- **`[llm.anthropic].thinking_budget_tokens` default `0` → `2048`.** Anthropic deployments
  that never set it explicitly start paying ~2048 thinking tokens per turn.

  **To disable it after upgrading**, set the per-agent `thinking_budget_tokens` to `0` (an
  explicit zero disables), or pin `[llm.anthropic].thinking_budget_tokens = 0` fleet-wide.
  ⚠️ `clear_thinking_budget_tokens: true` does **not** disable thinking — it clears the
  per-agent override back to *inherit*, and inheriting now means `2048`, so it re-enables it.

- **`[llm].timeout_secs` `120` → `600`; `[llm].stream_chunk_timeout_secs` `60` → `180`.**
  The first is the per-call HTTP deadline, raised because heavy reasoning models
  legitimately reason past 120s; the second is the per-chunk body-silence guard. Both apply
  to every run type and are inherited verbatim by subagents.

  **After upgrading**, a provider that accepts the connection and then goes quiet hangs for
  up to 10 minutes instead of 2. A host that is simply unreachable is still bounded by a
  fixed 30s connect timeout. Pin lower values under `[llm]` if you prefer tighter deadlines.

- **Default model and provider changed.** `[llm].model` now defaults to `z-ai/glm-5.2` on
  the `openrouter` provider. Conversation summarisation no longer reuses the agent's model:
  `[context].summary_model` / `summary_provider` default to the pair
  `google/gemma-4-31b-it` @ `openrouter`. Deployments with an explicit model set are
  unaffected.

  ⚠️ **If your agents run on a non-OpenRouter provider** — Anthropic direct, for example —
  summarisation now needs a resolvable OpenRouter key (`alms auth set openrouter <key>`) or
  it fails. **To restore the old inherit-the-agent's-model behaviour**, clear *both* fields
  together: `summary_model = ""` and `summary_provider = ""`. Setting only one is rejected
  at boot. The empty string is the explicit-clear sentinel on all three surfaces — TOML,
  `PATCH /settings`, and the persisted `settings.json` — so a clear survives a restart.

- **`workspace_write`'s `mode` now defaults per file: `memories` appends, the other three
  replace.** Previously every file defaulted to `write`, so a call that omitted `mode`
  replaced the whole file — the wrong branch to guess for `memories.md`.

  ⚠️ **This changes what an existing tool call does for every agent already calling it.**
  A `workspace_write` on `memories` with no `mode` now *adds* to the file instead of
  replacing it. `personality`, `goals` and `user` still default to `write`, and an explicit
  `mode` is still honoured everywhere.

- **`fs_read` output caps lowered.** Whole-file reads cap at 256 KiB; passing `offset` or
  `limit` falls back to a 64 KiB output budget. Large reads must paginate.

- **Server-default model and provider are now changeable without a restart.**

### Multi-agent and DM

- Two spellings of one subagent name now resolve to a single subagent, and agent names may
  contain capital letters (resolved case-insensitively).
- An interrupted DM end is recorded rather than left invisible to the agent, and the turn
  after a DM ends can no longer message the peer whose conversation just closed.
- A cancelled or failed DM no longer starts a run on the recipient's session.
- Agent-to-agent tool descriptions now state which relationship each tool creates and no
  longer teach agents to poll.
- Subagent sessions are filed under the agent that ran them, with session-keyed cancel
  controls and a status-only subagent status bar.

### Persistence and durability

- Transactional, versioned SQLite migrations.
- Silent row loss in the persistence layer is now counted and surfaced, and foreign-key
  fallbacks are no longer silent.
- Durable job recovery and atomic, bounded per-agent run admission.
- Scheduled jobs stay active until the agent's full task completes ("job episodes").

### Tools and workspace

- A replacing `workspace_write` is refused when it would delete text the agent has not
  been shown; a new `workspace_read` tool is how it gets shown.
- An agent's memories survive a concurrent write to its workspace.
- `read_session` and `read_subagent_session` no longer silently return 20 messages, and
  now report what they omitted.
- A reverted shell `cd` is visible to the agent.
- Tool re-registration no longer logs on the happy path, so `WARN` is worth reading again.

### Frontend

- Normalized entity state for messages, jobs, and runs, with authoritative reconnect
  recovery and optimistic UI actions.
- Revision-aware run and job lifecycles.
- History-reconstructed tool rows correlate with the live event stream again.
- A DM reasoning collapsible is no longer labelled with whichever agent the sidebar
  happens to be showing.
- The sidebar active-run indicator lights on cross-agent sessions.

## v0.2.3 — released (tag `v0.2.3`)

Stable on `main`, with Linux x86_64 and Windows x86_64 binaries. Headline changes:

- Workspace v2 single-sandbox-root model — one project-root sandbox, flat
  `.alms/agents/<name>/` metadata, per-agent git worktree mode, and an operator
  full-OS-access escape hatch.
- Anthropic no-args tool fix.
- Structured `MISSING_MODEL_AFTER_PROVIDER_SWITCH` 400 response.
- Per-tool structured output renderers.
- Per-run config override path removed — agents are the single per-tenant config surface.

## Earlier

For releases before v0.2.3, see the git history and the GitHub Releases page.
