# Changelog

Release notes for ALMS, with an emphasis on **operator-facing changes** — default flips,
configuration and wire-shape changes, and upgrade impact. Implementation detail lives in
the git history.

## v0.2.4 — unreleased (`develop`)

### ⚠️ Default changes — read before upgrading

- **Agent-loop hard caps now default ON, and the run-duration guard is inactivity-based.**
  `[llm].max_iterations` defaults to `500` and bounds a single run of any type — web chat,
  scheduled job, or subagent. A run is terminated when it stops making *progress* rather
  than on flat wall-clock, so a long but productive run is no longer clipped. New `[llm]`
  knobs: `between_iterations_secs` (default `180`) and `tool_phase_ceiling_secs` (default
  `900`). The absolute backstop `max_run_duration_secs` rose from 4h to 24h. Set any knob
  to `0` to disable it. All are config-file-only and require a restart.

- **Default model and provider changed.** `[llm].model` now defaults to `z-ai/glm-5.2` on
  the `openrouter` provider. Conversation summarisation defaults to a separate, cheaper
  model rather than reusing the main one. Deployments with an explicit model set are
  unaffected.

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
