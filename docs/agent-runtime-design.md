# Agent Runtime Design: Config, Context, and Workspace

Design for the three UX requirements from `docs/agent-ux-requirements.md`. These are interconnected — the config system configures the context manager, the workspace files feed into the context, and the context manager decides what the LLM actually sees.

---

## 1) Configuration System

### Problem
Config is currently scattered across `GatewayConfig`, `LlmConfig`, `SessionConfig`, and `AgentConfig` — each with hardcoded defaults, some reading from env vars, some not. No single config file, no validation, no documentation of what each setting does.

### Design

**Single config file** at a well-known location: `alms.toml` (workspace root or `~/.config/alms/config.toml`).

```toml
# alms.toml — All settings with sane defaults. Only override what you need.

[server]
bind = "127.0.0.1:8080"

[llm]
# provider = "openrouter"           # openrouter | openai | anthropic | local
# base_url = "https://openrouter.ai/api/v1"
# model = "moonshotai/kimi-k2.6"
# api_key loaded from secrets store via `alms auth set` (never in config file)
timeout_secs = 120
max_retries = 2
# Token budget per run (0 = unlimited)
max_tokens_per_run = 0

[session]
idle_timeout_secs = 86400           # 24 hours
max_messages = 10000
max_context_tokens = 256_000        # storage limit (>= context.max_input_tokens)

[context]
# How to manage the context window sent to the LLM
strategy = "truncate"               # truncate (default) | compact | full
# Max tokens to send to LLM (should be < model context window)
max_input_tokens = 128000
# When strategy = "compact": trigger compaction when history exceeds
# this fraction of the EFFECTIVE history budget (max_input_tokens minus
# the system / input / episodic / reserve overhead). Range: 0.50–0.95.
# Default: 0.80.
compact_trigger_pct = 0.80
# When compaction fires, retain this fraction of the EFFECTIVE history
# budget worth of recent verbatim messages. Range: 0.20–0.60.
# Default: 0.40.
compact_retain_pct = 0.40
# Episodic memory: cross-session summary generation after each run
# run_summary_mode = "llm"          # off | heuristic | llm (default)
# run_summary_budget = 2000         # max tokens for episodic injection (hard cap: 15% of max_input_tokens)
# summary_max_tokens = 1000         # max output tokens for LLM summarizer call (0 is auto-corrected to 1000)

[tools]
# Which builtins to enable
enabled = ["echo", "math", "http_get"]
# Default timeout for tool execution
timeout_secs = 30
# Max output size from a tool (bytes)
max_output_bytes = 65536

# Permission-based allow/deny list for the `shell` tool.
# Patterns are regex strings matched against the full command string.
# Evaluation order: deny wins; then, if `allowed_commands` is non-empty,
# only matching commands pass (allowlist mode); otherwise all non-denied
# commands pass (denylist-only mode).
#
# `classifier_overrides` is an operator-only escape hatch for the
# destructive-command classifier (see `tools.shell_classification_mode`):
# patterns here bypass the classifier floor but still honour
# `denied_commands` and the OS-level sandbox. Operator intent only —
# never model-visible and cannot be set via `PATCH /settings`.
#
# Applied in addition to the hardcoded destructive-command denylist in
# `alms-sandbox` (defense-in-depth). Compiled once at startup — changing
# these values at runtime via `PATCH /settings` is not supported; restart
# the gateway to pick up new patterns.
[tools.shell_permissions]
allowed_commands     = ["^(git|cargo|npm)\\b"]
denied_commands      = ["git\\s+push\\s+.*--force", "^rm\\s+-rf\\s+/"]
classifier_overrides = ["^rm\\s+-rf\\s+/tmp/alms-test-.*"]

[channels.telegram]
# token loaded from secrets store via `alms auth set telegram <token>`
poll_interval_secs = 5
```

**Principles:**
- Secrets (API keys, tokens) come from **`.alms/secrets.json`** via `alms auth set`, never config files or env vars
- Every field has a default — zero-config startup should work (with mock LLM)
- Human-readable durations (`"24h"`, `"30d"`, `"5m"`)
- Validation on startup: reject invalid values with clear messages before starting
- `alms config check` CLI command to validate without starting

**Implementation:**
- Use the `config` crate (already in workspace deps) with layered loading:
  1. Compiled defaults
  2. Config file (`alms.toml`)
  3. Environment variables (`ALMS_LLM_MODEL`, `ALMS_SESSION_IDLE_TIMEOUT`, etc.)
  4. CLI flags
- Single `AlmsConfig` struct that contains all sub-configs
- Parse and validate once at startup, pass `Arc<AlmsConfig>` to all components

---

## 2) Context Window Management

### Problem
OpenClaw tells the agent to "summarize your session" — this fails because:
- The LLM might summarize poorly or lose critical context
- The summarization itself burns tokens
- It's non-deterministic (different quality each time)
- The user sees degraded responses and doesn't know why

ALMS originally hardcoded `take(50)` messages in the agent loop. No compression, no awareness of token budget. (This has since been replaced by the `ContextBuilder` in `context.rs`.)

### Design

**Separation of concerns:**
- **Full history** (in SessionManager): everything, append-only, for audit and persistence
- **Context window** (in ContextBuilder): what the LLM actually sees, actively managed

**ContextBuilder** — a new component in `alms-runtime`:

```
┌─────────────────────────────────────────────┐
│              Context Window                  │
│                                              │
│  ┌──────────────────────────────────────┐   │
│  │ System Prompt                         │   │
│  │ (personality + goals + user + memories │   │
│  │  + base prompt + DM addendum*)        │   │
│  └──────────────────────────────────────┘   │
│  ┌──────────────────────────────────────┐   │
│  │ Episodic Summaries (cross-session)    │   │
│  │ "[Your conversation history...]"      │   │
│  │ **User chat (last active: ...)**      │   │
│  │  Helped debug CORS issue.             │   │
│  └──────────────────────────────────────┘   │
│  ┌──────────────────────────────────────┐   │
│  │ Rolling Summary (within-session)      │   │
│  │ "Previously: user asked about X,      │   │
│  │  agent built Y, decided Z..."         │   │
│  └──────────────────────────────────────┘   │
│  ┌──────────────────────────────────────┐   │
│  │ Recent Messages (last N turns)        │   │
│  │ [full user/assistant/tool messages]   │   │
│  └──────────────────────────────────────┘   │
│  ┌──────────────────────────────────────┐   │
│  │ Current User Input                    │   │
│  └──────────────────────────────────────┘   │
└─────────────────────────────────────────────┘
```

**Strategy: `compact`** (renamed from `sliding-summary` in #869; available, but the compiled default is `truncate` for simplicity; `compact` can be enabled via `alms.toml` or `ALMS_CONTEXT_STRATEGY` env var):

1. **Token counting**: Approximate token count for each message (`chars / 3` as rough estimate for mixed content, or `tiktoken`-style BPE if we add the dep). Track cumulative tokens.

2. **Window management**: Given a budget of `max_input_tokens`:
   - Reserve space for system prompt (~500-2000 tokens depending on workspace)
   - Reserve space for current input
   - Fill remaining budget: recent messages first (newest to oldest), then rolling summary

3. **Threshold-triggered compaction (#869, refined PR #1012)**: When
   the assembled history's token estimate crosses `compact_trigger_pct`
   of the **effective history budget** (`max_input_tokens` minus the
   system prompt, current input, episodic block, and the 1000-token
   reserve `ContextBuilder` uses — i.e. the actual room available for
   history; default `0.80 * effective_budget`):
   - Walk the uncovered tail backwards collecting messages whose cumulative
     tokens fit `compact_retain_pct * effective_budget` (default `0.40 *
     effective_budget`); everything older is the compress range.
   - Use a **cheap/fast model** (not the main agent model) to summarize the
     compress range and extend the rolling summary entry on the session.
   - This is automatic, not visible to the user, and doesn't affect the
     full history. `messages_covered` on the persisted `ContextSummary`
     advances to the new verbatim-tail boundary.
   - The two knobs share a 0.10 minimum gap — `normalize_episodic` and
     PATCH /settings reject combinations that would leave compaction
     unable to measurably reduce context size.
   - PR #1012 / Codex review: pre-fix, the trigger threshold was
     calculated against raw `max_input_tokens` while the dispatch
     `retain_budget` was clamped to the effective budget. With large
     workspace prompts or episodic blocks, `build_compact` could start
     dropping messages by token budget before `maybe_summarize` ever
     fired. Both knobs now share the same effective-budget denominator.

4. **Smart truncation of tool outputs**: Long tool results (e.g., HTTP responses) are truncated in the context window but preserved in full history. The context version says `[truncated: 45KB response, first 2KB shown]`.

5. **Token observability**: Currently the context builder logs total approximate tokens via `tracing::debug` at build time (message count and estimated tokens). Per-run token usage (prompt + completion) is tracked in `RunOutput.usage` and logged at `info` level. Detailed context breakdown (`context_breakdown`, `compression_ratio`) is not yet implemented — this is a future enhancement.

**Where it plugs in:**
- Context assembly lives in `agent/context.rs` (uses `ContextBuilder::build()`)
- ContextBuilder is configured from `[context]` in `alms.toml`
- Summary updates happen after each run completes (background task, not blocking)

**Available strategies** (selectable via `[context] strategy` in `alms.toml`):
- `truncate` (default): pure token-budget walk — keep the most recent messages that fit `max_input_tokens` (no summarization, just drops old ones)
- `compact`: the smart option described above — once history crosses the trigger threshold, fold older messages into a rolling LLM summary and keep a token-bounded verbatim tail
- `full`: send everything (for small conversations or cheap models)

> **Migration callout (v0.2.4 / #869).** `recent_window` and
> `summary_interval` are deprecated and ignored as of v0.2.4 — the
> `compact` strategy is now driven by token thresholds. Configs that
> still set them produce a one-time `WARN` at startup and the values
> are dropped. The legacy `strategy = "sliding-summary"` is accepted as
> an alias for `"compact"` and rewritten on load; rename it at your
> convenience. The PATCH /settings surface is stricter — requests that
> still send `context.recent_window` / `context.summary_interval`
> receive `400 CONTEXT_LEGACY_FIELD_DEPRECATED` so cached UI bundles
> fail fast and update.

**Tool call persistence and context reconstruction:**
Tool calls and their results are persisted to the session database as structured `Content::ToolCall` / `Content::ToolResult` messages. During context building, these are reconstructed into proper OpenAI-format LLM messages (assistant messages with `tool_calls` array, tool-role messages with `tool_call_id`). This means the LLM has full visibility of previous tool executions across runs — it knows what tools were called, with what parameters, and what results were returned.

**Canonical message-shape invariant (issue #586):**

The invariant lives in two distinct layers, and both matter independently:

- **Builder layer** (`Vec<LlmMessage>` out of `ContextBuilder::build_with_perspective`). This is provider-agnostic and uses the three-role alphabet `system` / `user` / `assistant` / `tool`. It is the shape the agent loop and all provider adapters see.
- **Wire layer** (the actual request body each provider adapter emits). This uses the two-role alphabet the provider's API speaks — for Anthropic, `user` / `assistant` — and is reached after each adapter applies its own relabeling and content-block mapping.

**Builder-layer guarantees** (enforced by `ContextBuilder::normalize_for_llm`):

1. **Non-empty.** At least one message.
2. **System prefix only at the front.** Mid-history `Role::System` markers are stripped — lifecycle markers (DM-ended, subagent completion, scheduled-job notifications) live in session history for UI/SSE rendering but never appear in the LLM context. The agent receives the relevant payload via the `notification_input` user message pre-persisted by `lifecycle.rs`.
3. **No adjacent same-role `user`/`assistant` pure-text turns.** Consecutive same-role pure-text turns are merged (content concatenated with a blank line). Tool-call / tool-result pairings are preserved as-is (already handled by `group_tool_calls` and `strip_orphaned_tool_results`). Note that `tool` and `user` are distinct roles at this layer — a canonical tail of `[tool_result, user_text]` satisfies the builder invariant.
4. **Last non-system message is `user`.** Pending assistant tool_calls with no matching tool_result (e.g. crash, cancel, truncation cutting mid-pair) are closed with synthetic `[Tool execution was interrupted]` tool_result messages, mirroring the conventional interrupted-result pattern. This ensures the tail is either a real user turn, a notification_input message, or a synthesised `"Please continue."` placeholder — never an assistant-with-tool_calls that Anthropic would reject.
5. **No empty-content messages.** Empty-body assistant or user turns are dropped before dispatch (conventional provider hygiene).

**Wire-layer guarantees** (per adapter, applied after its own transforms):

- The **Anthropic adapter** (`to_anthropic_request` in `anthropic.rs`) relabels `role="tool"` → `role="user"` so that tool_result blocks land inside a user message, which is what Anthropic requires. That relabel can create adjacencies that the builder invariant does not forbid — concretely, a canonical tail of `[tool_result, user_text]` becomes two adjacent `user` wire messages, which Anthropic rejects with a 400. The adapter therefore runs `merge_consecutive_roles` after its conversion loop, concatenating adjacent same-role messages' content blocks in order. A post-pass `debug_assert!` pins the wire-level no-adjacent-same-role property as a tripwire.
- The **OpenAI-compatible adapter** (OpenAI / OpenRouter / generic OpenAI-compatible providers) serialises the canonical builder shape directly. No role relabeling is needed because OpenAI natively has the `tool` role, so the builder invariant already holds at the wire.

The invariant is pinned by unit tests in `context.rs` (builder layer) and by `debug_assert!`s plus adapter-level tests inside `anthropic.rs` (wire layer — non-empty messages array, trailing user role, no adjacent same-role messages after merge). Provider adapters do ONLY their own genuinely-provider-specific work.

---

## 3) Agent Workspace Files

### Problem
Agents have no persistent identity. Every session starts from the same hardcoded system prompt. No memory across sessions. No way for the user to shape the agent's personality without editing code.

### Design

**Workspace directory structure (post-#945 — workspace v2):**

```
<project_root>/.alms/agents/<name>/
├── personality.md        # Who the agent is (tone, style, constraints)
├── goals.md              # Current objectives and priorities
├── memories.md           # Learned facts, user preferences, past decisions
└── user.md               # Who the user is: name, preferences, background
```

These are plain text files that the user can edit directly, or that the agent can update through the workspace tool. Per-agent config overrides (model, posture, etc.) are stored in the agent registry (SQLite), not in workspace files.

**Workspace attachment (post-#945):** `AgentRuntime::with_workspace(workspace)` is now scoped down. It binds the `WorkspaceWriteTool` (so `workspace_write` knows which agent's metadata directory to write to) and ensures the metadata directory exists. It **no longer touches the filesystem sandbox** — the sandbox root is set independently by `with_project_root(project_root)`, which is the single source of truth for the `fs_*` and `shell` boundary. See [`docs/security-model.md` § 4.4](security-model.md#filesystem-sandboxing) for the full sandbox model.

This is a deliberate departure from the pre-#945 `with_workspace(ws_root)` shape, where the workspace builder narrowed the `fs_*` sandbox to the agent's metadata directory. That coupling was the root cause of the "agent can't read project files" pain — it conflated *where the agent's identity lives* with *what the agent can touch*. Workspace v2 splits those concerns: identity files live at `<project_root>/.alms/agents/<name>/`, and the sandbox boundary is the project root.

The other sandbox-root-related builders on `AgentRuntime` are:

- **`with_project_root(project_root: PathBuf)`** — pin the sandbox at the project root (the default); re-registers `fs_*` + `shell` against it.
- **`with_extra_fs_read_root(root: PathBuf)`** (added in #946) — push an additional read-only root onto the read-family `fs_*` tools. Used by per-agent [worktree mode](security-model.md#opt-in-worktree-mode) to widen worktree-mode agents with `<project>/.alms/agents/` so cross-agent metadata reads keep working from inside the worktree, and by per-run spill directories (`with_shell_spill`, `with_tool_output_truncate`) so the agent can `fs_read` its own spilled tool output.
- **`with_unrestricted_filesystem()`** (added in #947) — drop the sandbox entirely; used by the [`[security].allow_full_os_access`](security-model.md#operator-escape-hatch-allow_full_os_access) operator escape hatch. Subject only to OS-level permissions of the daemon process; `shell_permissions` and the destructive-command classifier still apply.

The gateway's run-lifecycle wires these in a deterministic order: spill / tool-output-truncate builders accumulate extras first, then the sandbox-root builder (`with_unrestricted_filesystem` if the agent is on the security list, otherwise `with_extra_fs_read_root(...).with_project_root(worktree)` if `worktree_mode = "git"`, otherwise `with_project_root(project_root)`), and finally `with_workspace` to bind `workspace_write`. After this, the run starts.

**How they feed into the system prompt:**

The workspace prefix is assembled by `build_system_prompt_prefix()` in `workspace.rs` and **appended** after the base system prompt. The order is:

```
System prompt = [
  base_prompt (initial.md or bootstrap.md),
  "\n\n",
  personality.md contents (raw, if exists),
  "## Current Goals\n" + goals.md contents (if exists),
  "## About the User\n" + user.md contents (if exists, user-facing sessions only),
  "## Memories\n" + memories.md contents (if exists, truncated at 4000 chars)
]
```

The foundational role/identity prompt comes first; agent-specific personalization is appended after it, matching common LLM prompting practice (role/identity first, personalization later). See `docs/system-prompts.md` § "Prompt Assembly Order" for the canonical reference.

For non-user-facing sessions (DM, subagent, job), `user.md` is omitted from the prefix to save tokens and avoid confusion.

Tool descriptions are injected separately by the LLM API layer (via `with_tools()` on the completion request), not in the system prompt text.

The workspace files are read at the **start of each run** (not cached across runs), so edits take effect immediately.

**Size management:**
- `memories.md` is truncated at 4000 characters before injection (hard limit in `build_system_prompt_prefix`)
- Other workspace files have no enforced size limit — they contribute to the system prompt token count which reduces the history budget
- Token budget for the entire system prompt (including workspace prefix) comes from the `max_input_tokens` budget

**Memory updates:**
- The agent can update workspace files via the `workspace_write` tool
- `workspace_write` takes `{file: "memories.md", content: "..."}` or `{file: "memories.md", append: "..."}`
- All four workspace files are agent-writable (personality, goals, memories, user) — this is needed for the bootstrap interview to populate `personality.md`
- Memory writes are audited

**Bootstrap mechanism:**

On first interaction with an agent that has **empty/missing workspace files**, the system:

1. Detects missing `personality.md` (or all workspace files missing)
2. Injects a special bootstrap system prompt:
   ```
   You are a new ALMS agent being set up for the first time.
   Ask the user a few questions to understand your purpose:
   - What should your primary role be?
   - Any preferences for communication style?
   - What name would you like to use for the user?
   After the conversation, use workspace_write to save personality.md and goals.md.
   ```
3. The agent interviews the user (2-4 questions max, not annoying)
4. The agent writes the workspace files using the `workspace_write` tool
5. Subsequent sessions use the workspace files normally

**Bootstrap is one-time** — once `personality.md` exists, it doesn't trigger again. The user can delete workspace files to re-trigger it.

---

## 4) Episodic Memory (Cross-Session Awareness)

### Problem
Agents have no awareness of what happened in previous sessions. Each new session starts from scratch with only workspace files for long-term identity. If the user discussed topic X in a Telegram chat yesterday, the agent in today's web session has no idea.

### Design

**Two distinct summary systems serve different purposes:**

| System | Table | Scope | Purpose |
|--------|-------|-------|---------|
| **Context summaries** | `context_summaries` | Within a single session | Compress old messages so the current session fits in the context window. Used by the `compact` strategy (renamed from `sliding-summary` in #869). |
| **Session summaries** | `session_summaries` | Cross-session | One summary per session, updated after each run. Injected into *other* sessions to provide cross-session awareness. |

**How session summaries are generated:**

After each successful run, the gateway spawns a fire-and-forget `tokio::spawn` task that:
1. Checks if the session type is eligible (subagent and episodic sessions are excluded via `derive_source_label`)
2. Derives a human-readable source label from the `context_id` (e.g. "User chat", "Telegram chat", "DM with bob", "Scheduled job: ...")
3. Loads the existing summary from `session_summaries` (if any)
4. Generates a new or updated summary via the configured mode
5. Upserts the result to `session_summaries` with the source label

**Summary modes** (controlled by `context.run_summary_mode` in `alms.toml` or `ALMS_RUN_SUMMARY_MODE` env var):

- **`off`** — No summaries generated. No episodic injection.
- **`heuristic`** — Deterministic, no LLM call. Produces a one-liner from the first ~120 bytes of run input and ~80 bytes of the agent's response (when available). Successive runs in the same session append entries; oldest lines are trimmed when total exceeds ~500 chars.
- **`llm`** (default) — Lightweight LLM call using `session_summarizer.md` prompt. Receives run input (~2000 chars), agent output (~2000 chars), and existing summary. Produces a concise 1-3 sentence evolving summary. Max 300 output tokens. When the configured model is a reasoning model (e.g. minimax-m2.5, deepseek-r1), the summarizer falls back to `reasoning_content` if `content` is null or empty.

**How episodic context is injected:**

At the start of each run (in `build_context`), when `run_summary_mode != off`:
1. Load all session summaries for this agent (excluding the current session) from SQLite
2. Format them into a token-budgeted block with header and source-labelled entries (most recent first)
3. Pass the formatted text to `build_with_perspective()` which injects it as a system message

**Context assembly order:**
```
[System prompt] -> [Episodic summaries*] -> [Rolling summary*] -> [Recent messages] -> [Current input]
```

**Budget control:**
- `context.run_summary_budget` (default: 2000 tokens) controls how many tokens episodic summaries can consume
- Hard-capped at 15% of `max_input_tokens` — values exceeding the cap are clamped with a warning at config load time (`ContextConfig::normalize_episodic()`)
- The episodic token cost is subtracted from the total context budget, reducing the space available for session history. This ensures episodic content never starves the current conversation.
- Entries are added most-recent-first until the budget is exhausted; remaining entries are dropped.

**Agent self-recall tools:**

Two built-in tools let agents actively query their own session history (rather than passively receiving injected summaries):

- **`list_my_sessions`** — Lists the agent's sessions across all channels (web, Telegram, DM, job). Returns session ID, context type, context ID, source label, message count, last activity, and episodic summary. Excludes internal sessions (subagent, episodic). Current session excluded by default.
- **`read_session`** — Reads conversation history from a specific session by UUID. Returns last N messages and the episodic/context summary. Security: verifies session ownership (`agent_id` match or DM participant check with exact segment matching to prevent substring bypass).

**Storage schema:**

```sql
CREATE TABLE session_summaries (
    agent_id     TEXT NOT NULL,
    session_id   TEXT NOT NULL REFERENCES sessions(id),
    summary      TEXT NOT NULL DEFAULT '',
    last_run_id  TEXT,
    updated_at   TEXT NOT NULL,
    source_label TEXT,
    PRIMARY KEY (agent_id, session_id)
);
CREATE INDEX idx_session_summaries_agent
    ON session_summaries(agent_id, updated_at DESC);
```

### Known limitations

- **Race condition on concurrent runs:** Two concurrent runs for the same session can both load the same base summary, generate independently, and the last writer wins. Acceptable for MVP since concurrent runs on the same session are rare.
- **No summary eviction:** Summaries accumulate indefinitely. A future cleanup job should prune summaries for sessions that have been idle beyond a threshold.
- **Heuristic mode uses truncated snippets:** It captures truncated input (~120 bytes) and output (~80 bytes) rather than full context. LLM mode produces richer summaries at the cost of tokens.

---

## 5) How these connect

```
alms.toml (config)
    │
    ├─→ LLM settings (model, tokens, retries)
    ├─→ Session settings (idle timeout, max messages)
    ├─→ Context settings (strategy, budget, window size)
    └─→ Tool settings (enabled tools, timeouts)

Agent Workspace (per agent)
    │
    ├─→ personality.md ──→ system prompt prefix (raw)
    ├─→ goals.md ────────→ system prompt prefix (## Current Goals)
    ├─→ user.md ─────────→ system prompt prefix (## About the User, user-facing only)
    └─→ memories.md ─────→ system prompt prefix (## Memories, truncated at 4000 chars)

ContextBuilder (per run)
    │
    ├── reads workspace files
    ├── reads session history (full)
    ├── reads/updates rolling summary
    ├── applies token budget from config
    └── produces: Vec<LlmMessage> (what the LLM sees)
```

---

## 6) Implementation order

1. **Config system** — `AlmsConfig` struct, `alms.toml` loading, validation. Small, foundational, unblocks everything.
2. **ContextBuilder** — token counting, sliding window, replaces hardcoded `take(50)`. This is the core improvement.
3. **Agent workspace** — directory structure, file reading, system prompt assembly. Depends on ContextBuilder for token budgeting.
4. **Bootstrap** — workspace_write tool, bootstrap detection, interview flow. Depends on workspace.
5. **Rolling summary** — LLM-based compression of old messages. Can be added after the other pieces work.

---

*Design by Atlas (2026-02-14). Episodic memory section added 2026-03-28. Updated 2026-03-30: tool implementations extracted to `alms-tools` crate, agent.rs split into `agent/` module directory. Updated 2026-04-04: synced workspace, strategy defaults, and token observability sections with actual implementation (audit by Heph). Implements requirements from `docs/agent-ux-requirements.md`.*
