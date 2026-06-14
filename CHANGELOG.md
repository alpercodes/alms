# Changelog

Per-release notes for ALMS, with an emphasis on **operator-facing changes** — default flips, config/wire-shape changes, and upgrade impact. Implementation detail for individual changes lives in the linked PRs and git history; this file is the curated, upgrade-relevant summary. New entries go here (or in the PR description), **not** in `CLAUDE.md`.

## v0.2.4 — in progress (`develop`)

### ⚠️ Operator-facing default changes (read before upgrading)

- **Agent-loop hard caps now default ON** — PR #1160 / #987.
  - `[llm].max_iterations` defaults to `500`; `[llm].max_run_duration_secs` defaults to `14400` (4 hours).
  - They bound a single run's LLM-call count and wall-clock duration, and apply to **all run types** (web chat, cron jobs, subagents — inherited verbatim by subagents), not just DMs.
  - After upgrading, a previously-unbounded deployment will end any run exceeding 500 LLM calls or 4 hours as `failed`. For a peer-triggered DM run, the gateway's completion gate converts the cap trip into an `Errored` conversation end so the peer is notified.
  - Set either knob to `0` to disable (the escape hatch for genuinely long workloads). Both are **config-file-only**: not mutable via `PATCH /settings`, no `ALMS_*` env override, restart required. See `docs/config.md` § "Agent-loop hard caps".

- **Default model + Anthropic thinking budget flipped** — PR #1081.
  - `[llm].model` default went from `moonshotai/kimi-k2.5` to `moonshotai/kimi-k2.6` on the `openrouter` provider — affects deployments with no explicit `[llm].model` or env override.
  - `[llm.anthropic].thinking_budget_tokens` default went from `0` (disabled) to `2048` (enabled) — Anthropic deployments that did not explicitly set it to `0` will start paying ~2048 thinking tokens per turn.
  - **To disable thinking after the flip:** set the per-agent `thinking_budget_tokens` to `0` (an explicit `Some(0)` disables it), or pin `[llm.anthropic].thinking_budget_tokens = 0` fleet-wide (`alms.toml` or `PATCH /settings`). ⚠️ `clear_thinking_budget_tokens: true` does **not** disable — it resets the per-agent override to `None`, which now *inherits* the 2048 default (i.e. re-enables thinking). See the precedence chain in #767 / #809 / #941.

### Notable changes

- **Implicit DM replies + completion gate** (#1154 / #1156): a peer-triggered DM run's final assistant text **is** the reply (no `send_message`-per-turn). Every peer DM run exits as exactly one of delivered / ended / errored. DM sessions are agent-only — `POST /runs` on a `dm:` session returns `400 DM_SESSION_NOT_DIRECTLY_RUNNABLE`.
- **DM frontend rendering fixes** (#1155) and **backend hardening** (#1160 — iteration/duration caps, depth-leak heal, bounded channels, recipient-aware conflict, queued-cancel peer notification).
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
