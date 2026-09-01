# ALMS Architecture — Multi-Agent Hierarchy

## What is ALMS?

**ALMS** = **Agent Loop Management System**

A Rust-based agent platform with two communication layers: (1) vertical delegation via `invoke_agent`, where a top-level agent can spawn subagents and receive their results; and (2) peer-to-peer direct messaging via `send_message`, where registered agents exchange messages through a shared MessageBus. Subagents can be ephemeral (one-shot) or persistent (named, with session history preserved across invocations). Recursive subagent spawning is not currently wired.

---

## Core Design Principles

1. **Delegation + Peer Messaging** — Top-level agents delegate through `invoke_agent`; registered agents communicate directly through `send_message` and the shared MessageBus. Recursive delegation is future work.
2. **Workers** — Subagents do work and return a result. They can be ephemeral (one-shot, fresh session) or persistent (named, with conversation history preserved across invocations via deterministic session IDs)
3. **Two Communication Layers** — Layer 1: parent-child delegation via `invoke_agent` (blocking/background). Layer 2: peer-to-peer direct messaging via `send_message`, persisted in shared DM sessions with perspective mapping and processed through bounded trigger runs.
4. **Explicit over Implicit** — Clear task boundaries, observable handoffs via SSE
5. **Security First** — Policy-controlled tools, posture approvals, filesystem/shell guardrails, and auditability. A unified capability-grant model remains future work.

---

## Multi-Agent Topology

### Layer 1 — Parent/subagent delegation

A top-level agent can call the `invoke_agent` tool to spawn a subagent. The subagent executes a full multi-turn agent loop and returns its result as a tool response, or completes in the background and triggers an automatic notification run. Subagent runtimes do not currently register `invoke_agent`, so the live hierarchy is one level deep.

```
[User] ──► [Agent A]
                │
                ├── invoke_agent → [Subagent B]
                │                       │
                │                       └── returns result ──► [Agent A continues]
                │
                └── invoke_agent → [Subagent C]
                                        │
                                        └── returns result ──► [Agent A continues]
```

**Key properties:**
- Top-level agents orchestrate; recursive subagent orchestration is not wired
- Subagents can be **ephemeral** (fresh session per invocation) or **persistent** (named — same `name` reuses session, preserving conversation history via UUID v5 deterministic identity)
- Results propagate up the tree via tool responses
- Cancelling a parent run cancels its directly spawned subagents
- Each subagent has its own tool registry, context window, and system prompt
- Subagent runs a full `agent_loop` (multi-iteration tool use; bounded by token budget, provider `max_tokens`, posture approvals, and run cancellation)

### Layer 2 — Peer-to-Peer Direct Messaging (Phase 1 + DM lifecycle implemented)

Agents can send messages to any other agent by name via the `send_message` tool. Messages are delivered through a shared `MessageBus` in the Coordinator and stored in DM sessions (deterministic UUID v5 identity based on the sorted agent-name pair). The recipient's `ContextBuilder` uses perspective mapping (`build_with_perspective`) to correctly attribute messages as "self" vs "other" based on the `from_agent` metadata. This enables bidirectional collaboration without requiring a parent-child relationship.

**DM conversation lifecycle (#384):** DM conversations have an explicit start/exchange/end lifecycle:
- **Start**: First `send_message` creates the shared DM session and begins depth tracking.
- **Exchange**: Each reply increments a depth counter per DM pair (max: `MAX_DM_DEPTH` = 20). The inactivity timeout is 30 minutes (`DEPTH_EXPIRY_SECS` = 1800).
- **End**: Triggered by `ignore_message` (agent declines to reply) or depth limit exceeded. `MessageBus::end_conversation()` writes a `dm_ended` marker to the DM session, resets the depth counter, and emits a `ConversationEnded` `RunTrigger` to the peer.
- **Peer notification**: The peer receives a one-shot notification run. When the peer initiated the DM from a user-facing session, the `MessageBus` routes the notification to that source session so the user sees the response inline. When `source_session_id` is `None` (the agent was a pure DM recipient), the notification run stays on the invisible `notifications:{agent_name}` session — it is NOT rerouted to a user-facing session, to avoid polluting the web-chat. A lightweight `dm_conversation_ended` SSE event + marker message is sent to the web-chat separately by `notify_dm_ended_to_webchat`. This run does NOT include the DM addendum. The agent can then report results, update goals/memories, or take other action.
- **SSE event**: A `dm_conversation_ended` event is emitted on the DM session stream for web UI rendering.

---

## Components

### Coordinator (`alms-coordinator`)

Manages the lifecycle of subagent tasks spawned by a parent agent.

**Responsibilities:**
- Accept `invoke_agent` requests (task description, optional registered name, and parent identity)
- Spawn a subagent `AgentRuntime` for each request
- Return the subagent's final response to the caller as a `TaskResult`
- Cancel subagents when the parent run is cancelled
**Current state:** One-level delegation is implemented. The `invoke_agent` tool is wired to real `AgentRuntime` loops in foreground (blocking) and background (non-blocking with auto-notification) modes. Named subagents use persistent UUID v5 identities, and subagent runs are registered as proper runs visible in `GET /runs`. Recursive delegation is not wired.

```
[Parent AgentRuntime]
        │  invoke_agent tool call
        ▼
[Coordinator::spawn_subagent()]
        │
        ▼
[Subagent AgentRuntime] ──► runs its own agent loop ──► returns TaskResult
        │
        ▼ (forwarded back as tool result)
[Parent AgentRuntime continues]
```

### Agent Runtime (`alms-runtime`)

Executes agent loops for all agents — both top-level (user-facing) and subagents. There is no separate "Main Agent" implementation; the same `AgentRuntime` is used at every level of the hierarchy.

**Agent loop:**
```
Assemble context → LLM call → Parse response →
  If tool calls: execute registered tools (including invoke_agent on top-level runtimes) → loop
  If final reply: emit run_finished → stop
```

**Subagent loop (same, different inputs):**
```
Receive task description as initial user message →
Assemble minimal context (task-specific prompt + configured tools/policy) →
LLM call → ... → emit result → stop
```

**Context assembly order** (for all agents):
```
[System prompt] → [Episodic summaries*] → [Rolling summary*] → [Recent messages] → [Current input]
```
\* Episodic summaries are injected when `run_summary_mode != off` and past session summaries exist. A rolling summary is injected when the canonical `compact` strategy has compressed enough history; `sliding-summary` is accepted only as a deprecated configuration alias.

**Episodic memory** (cross-session awareness):

After each successful run, the gateway may generate a per-session summary and store it in the `session_summaries` SQLite table. On subsequent runs, these summaries are loaded (excluding the current session), formatted with source labels and timestamps, and injected as a system message between the main system prompt and the session history. This gives agents awareness of what they were doing in other conversations without re-reading full transcripts.

Summary generation modes (controlled by `run_summary_mode`):
- `off` — no summaries, no episodic injection
- `heuristic` — deterministic one-liner from truncated input + output (zero LLM cost)
- `llm` (default) — rich 1-3 sentence summary via a lightweight LLM call using `session_summarizer.md` prompt

The episodic token budget (`run_summary_budget`, default: 2000) is hard-capped at 15% of `max_input_tokens` and subtracted from the total context budget so episodic content never starves the current conversation.

```
[Run completes successfully]
        │
        ▼
[generate_and_persist_summary()] (fire-and-forget tokio::spawn)
        │
        ├── derive_source_label(context_id) → skip subagent/episodic sessions
        ├── load existing summary from session_summaries table
        ├── generate new/updated summary (heuristic or LLM)
        └── upsert to session_summaries (agent_id, session_id, summary, source_label)
        
[Next run starts]
        │
        ▼
[load_episodic_summaries()] → load summaries (exclude current session)
        │
        ▼
[format_episodic_for_injection()] → token-budgeted formatted text
        │
        ▼
[build_with_perspective()] → injected as system message after main system prompt
```

### Tool Sandbox (`alms-sandbox`)

Isolated tool execution used by every agent regardless of hierarchy level.

**Built-in tools:** `echo`, `math`, `http_get`, `shell` (primary, `bash -c` command strings with persistent cwd, background execution, and 30KB output truncation; aliased as `shell_exec` for backward compatibility), `fs_read`, `fs_write`, `fs_list`, `fs_edit`, `fs_grep`, `fs_glob` (in alms-sandbox), `workspace_write`, `workspace_read` (in alms-runtime), `invoke_agent`, `read_subagent_session`, `send_message`, `list_agents`, `read_messages`, `ignore_message`, `list_my_sessions`, `read_session` (in alms-tools)

**Read-before-write guard:** `fs_write` and `fs_edit` enforce a read-before-write policy via `FileStateCache` (per-run, shared across all fs tools). Existing files must be read via `fs_read` before they can be written or edited. The guard also detects external modifications (mtime + content-hash fallback) and rejects stale writes. New file creation bypasses the guard. See `crates/alms-sandbox/src/file_state_cache.rs`.

**`fs_read` size limits:** Whole-file reads are capped at 256 KiB; passing `offset` or `limit` opts into a partial read that falls back to a 64 KiB output-byte budget (lowered from 512 KiB in #917 to match prevailing agent-tool caps and reduce pre-truncation bloat now that the in-loop tool-output truncate caps at 32 KB anyway — see #813 / #901). Each individual line is allocation-capped at 256 KiB before being returned — over-cap lines are truncated with an inline marker and the surplus bytes are drained, so a pathological single-line file (e.g. minified bundles) cannot exhaust daemon memory (#902); after #917 a fully-capped single line exceeds the 64 KiB response budget on its own, so such lines are now rejected from the response with `byte_budget_exceeded: true` and the agent must paginate via `offset`/`limit`.

**Sibling workspace reads (#242):** Every agent's identity files live under `<project_root>/.alms/agents/<name>/`, which is naturally inside the project-root sandbox set by `with_project_root` (#945). So a parent agent can already `fs_read('.alms/agents/<sibling>/personality.md')` through normal `fs_read`/`fs_list`/`fs_grep`/`fs_glob` calls — no separate sibling-workspaces extras list, no asymmetric ephemeral-subagent rules. Under per-agent [worktree mode](security-model.md#opt-in-worktree-mode-946) (#946), the gateway pushes `<project_root>/.alms/agents/` onto the read-family fs tools' extras list so cross-agent reads keep working from inside the worktree. `fs_write`/`fs_edit`/`workspace_write` land in the same single sandbox root; `workspace_write` itself is hard-pinned to the agent's own metadata directory by the tool. See [`docs/security-model.md` § 4.4](security-model.md#44-filesystem-sandboxing-implemented) for the full trust model.

**Tool output truncation + spill files (#756 + #851):** Two layers cap how much tool output reaches the LLM context, the audit log, and the SSE stream:

- **`shell` internal spill (#756)** — caps `shell_exec` stdout/stderr at 30 KB head + tail per invocation. The full pre-truncation bytes are written to `{data_dir}/shell_output/{run_id}/shell_<call_id>.txt` so the agent can recover them via `fs_read` / `fs_grep`. Lives *inside* the shell tool, before the JSON result is returned to the agent loop. Configured under `[tools.shell_spill]` in `alms.toml`.
- **Shared in-loop truncate (#851)** — every tool's result (including `fs_read`, `http_get`, `read_session`, etc., not just shell) is routed through `tool_output_truncate::truncate` before it lands in the agent's live message vec, the session DB, the audit log, and the `ToolEnd` SSE event. Caps oversized outputs at **32 KB / 2000 lines** (whichever fires first). The full pre-truncation bytes are spilled to `{data_dir}/tool-output/{run_id}/tool_<call_id>.txt`. Configured under `[tools.tool_output_truncate]` in `alms.toml`.

Both spill features:
- Use a **7-day retention sweep** that runs once at gateway startup (filesystem `mtime` check). Operators tune via `retention_days` in the respective config block.
- Widen the read-family fs_* tools' allowed roots to include the per-run spill directory, so the LLM-visible recovery hint (``Use `fs_grep` to search the full content or `fs_read` with `offset`/`limit` to view specific sections.``) resolves to a path the agent can actually read.
- Lay out subagent spills under `{data_dir}/{shell_output|tool-output}/sub-{task_id}/` so the parent's startup retention sweep collects them the same way it collects parent spills.
- Are config-file-only (not mutable via `PATCH /settings`) — operators tune the TOML and restart the daemon.

When the truncation service rewrites a tool result, the persisted session row carries `truncated_in_loop: true` metadata so `session_msg_to_llm` skips its legacy 2000-byte re-truncation pass on context rebuild — the bytes on disk are exactly the bytes the live agent saw. When the retention sweep later expires the spill file, `session_msg_to_llm` detects the missing path and rewrites the trailing recovery hint to a "retention period has expired" notice so the agent doesn't try to `fs_read` an ENOENT path on a follow-up turn.

**Runtime-policy inheritance:** Subagents inherit the configured sandbox, shell,
filesystem, and tool policy from the parent/server runtime configuration, with
named-agent overrides applied where supported. `SubagentRequest` has no
per-invocation capability-grant field today; adding one requires a separate
security design rather than an implied bearer capability.

**Typed errors across the sandbox boundary (#920):** Tools that wrap a typed `AlmsError` (notably the `invoke_agent` subagent path) propagate it through `SandboxError::Subagent(Box<AlmsError>)` instead of stringifying into `SandboxError::Io`. `ToolRegistry::execute`'s catch-all unwraps the `Subagent` arm back to the inner `AlmsError` so the structured variant (e.g. `AlmsError::SubagentLlmError { provider, status, body }`) survives every boundary verbatim and reaches the parent agent's `tool_result` as a single tractable line. The coordinator carries typed values through a parallel `error_tx`/`error_rx` oneshot alongside the JSON `TaskResult` for the `invoke_agent` path. New tools with structured errors worth preserving should rely on the `From<AlmsError> for SandboxError` impl rather than reaching for `Io`/`Internal`.

### LLM Client (`alms-runtime`)

Multi-provider LLM support with streaming. Provider selected via `llm.provider` config or `ALMS_LLM_PROVIDER` env var. Providers are declared in `[llm.providers.<name>]` tables and looked up by name; the sugar names `anthropic`, `gemini`, `openai`, and `openrouter` are auto-populated so existing configs keep working.

**Native adapters:**
- **Anthropic** (`kind = "anthropic"`) — Anthropic Messages API with full streaming, tool use, and response format mapping.
- **Gemini** (`kind = "gemini"`) — Google Gemini `generateContent` / `streamGenerateContent` API with `systemInstruction` extraction, `functionCall` / `functionResponse` tool parts, and OpenAI-style SSE streaming. Authenticates via the `x-goog-api-key` header.
- **OpenAI-compatible** (`kind = "openai_compatible"`) — reaches any endpoint that speaks the OpenAI chat-completions protocol, with per-provider `base_url` / `auth_scheme` / `quirks`. Out of the box this covers OpenAI, OpenRouter, xAI, DeepSeek, Groq, Mistral, Ollama, LM Studio, self-hosted vLLM, Together, Fireworks, etc. — adding a new provider is a docs entry, not code.

See `docs/config.md` for copy-paste provider examples. API keys are resolved in order: (1) `SecretsStore` (`alms auth set <provider> <key>`), then (2) the provider entry's `api_key_env` / `api_key`. No keys are read from arbitrary env vars.

### Session Manager (`alms-session`)

Owns conversation history and workspace state. Backed by **SQLite** (`./.alms/alms.db`) for durable persistence of sessions, audit events, scheduled jobs, and the agent registry.

Schema changes use ordered, transactional migrations with a durable
`schema_migrations` history and fail closed on unknown future versions. See
[Database migrations, compatibility, and rollback](database-migrations.md).

Rows that cannot be parsed or reconciled are **quarantined**, not fatal: they
stay durable, are kept out of live in-memory state, and are counted on
`GET /operations/metrics`. A handful of *columns* are degraded rather than
dropped, where dropping the row would be worse; those are counted separately
and more loudly, because a degraded column does reach live state. See
[Reconciliation policy: absence must be a safe belief](#reconciliation-policy-absence-must-be-a-safe-belief).

**On-disk layout (post-#945 / #946 — workspace v2):**

```
<project_root>/
├── .alms/
│   ├── alms.db              # SQLite — sessions, audit, jobs, agent registry
│   ├── secrets.json         # API keys (optional AES-256-GCM via ALMS_MASTER_KEY)
│   ├── logs/                # Daily-rotated daemon logs
│   ├── shell_output/        # Per-run shell stdout/stderr spills (#756, 7-day retention)
│   ├── tool-output/         # Per-run shared tool-output spills (#851, 7-day retention)
│   ├── agents/              # Per-agent metadata (renamed from workspaces/ in #945)
│   │   └── <name>/
│   │       ├── personality.md
│   │       ├── goals.md
│   │       ├── memories.md
│   │       └── user.md
│   └── worktrees/           # Per-agent git worktrees (#946 — only for agents with mode = "git")
│       └── <name>/          # `git worktree add <path> -b alms/<name>`
└── ...                       # Rest of the project tree — the sandbox root for default-mode agents
```

The `agents/` flat layout replaces the pre-#945 `workspaces/<agent>/` nesting. The `worktrees/` sibling exists only for agents created with `--worktree-mode git`; default-mode agents use the project root as their sandbox boundary directly. See [`docs/security-model.md` § 4.4](security-model.md#44-filesystem-sandboxing-implemented) for how the layout maps onto the filesystem-sandbox model and [§ "Opt-in worktree mode"](security-model.md#opt-in-worktree-mode-946) for the worktree provisioning + removal flow.

**Current parent/child association:**
```
parent run
  └── coordinator task_id → child session + child run
```

- The coordinator tracks live task handles; the parent session record does not own a child-task list
- Cancelling a parent run reaches its directly spawned children
- Usage is persisted on each child run and is not rolled into the parent run totals or the `invoke_agent` tool response

### Gateway (`alms-gateway`)

HTTP/SSE control plane. Handles top-level user interactions and exposes coordinator state.

**Run endpoints:** `POST /runs`, `GET /runs/{id}`, `GET /runs/{id}/events`, `POST /runs/{id}/cancel`
**Agent endpoints:** `GET/POST /agents`, `GET/PUT/DELETE /agents/{id_or_name}`, `POST /agents/{id_or_name}/default`
**Workspace endpoints:** `GET /agents/{id_or_name}/workspace`, `PUT /agents/{id_or_name}/workspace/{file}`

**Other:** `GET /settings`, `GET /audit`, `POST /jobs`, `GET /jobs/{id}`, `GET /sessions`, `GET /health`

**SSE event propagation:** The parent stream receives `subagent_started`, `subagent_completed` (**background subagents only** — the completion channel is fired behind `handle.is_background`, so a foreground subagent's only terminal signal on the parent stream is the `tool_end` of its `invoke_agent` call; see [`api.md`](api.md) and the pins in `crates/alms-gateway/src/runs/subagent_chip_timing_tests.rs`), and transient coarse `subagent_activity` status. Raw child token, reasoning, tool-parameter, and tool-result events stay on the child session's own SSE stream, where the fullscreen subagent view can stream and replay them.

### Channel Adapters (`alms-channel`)

Telegram reaches registered top-level agents through the channel adapter. The embedded web UI consumes the gateway API: it can open a child session's own stream and cancel a live child by session ID. The channel adapter does not expose a route for users to create arbitrary coordinator tasks directly.

---

## Reconciliation policy: absence must be a safe belief

A system-wide invariant about what the daemon is permitted to **believe**. It
governs every startup sweep, recovery pass, and loader that reads durable
state — it is not a database-operations procedure.

> **The rule.** When a startup pass, recovery sweep, or loader finds a row it
> cannot repair or parse, ALMS **quarantines** it rather than refusing to run.
> The row stays durable and untouched; it is **not projected into live
> in-memory state**; it is counted on `GET /operations/metrics`; and it is
> logged with whatever identifies it and, where one exists, its remediation.
> The daemon boots.
>
> Quarantine is legal only where the daemon behaves correctly while believing
> the row is **absent**. Where absence would cause already-completed work to
> run again, the failure is fatal instead. Where absence is merely *untidy* —
> it strands durable rows but re-executes nothing — quarantine still applies,
> but the site must say what it strands.

**The test to apply at a new site — one question:**

> *If I drop this row entirely, does the daemon do anything it would not have
> done had the row been correct?*
>
> - **No** → quarantine. Absence is safe.
> - **Yes, it re-executes something already done in the world** → fatal.
> - **Yes, but only by *stranding* durable rows — orphans or unmigrated rows
>   left behind, with nothing re-executing and nothing removed that should
>   have survived** → quarantine, *and* the call site must name what it
>   strands in a comment and prefix its log `detail` with the site, so the
>   drop is distinguishable from a loader drop on the same table.

**The stranding in the third branch must be additive.** A drop that causes
rows to be *deleted* which should have been kept is not this branch and not
quarantinable: durable garbage is tolerable only because it is still
repairable by hand, and data that is gone is not.

**"Fatal" is scoped to where the site runs.** At a startup or recovery site it
means the daemon refuses to open the database — that is the sense used in
"Exactly one fatal site" below. At a request-scoped write path it means the
operation fails and its transaction rolls back; the daemon keeps serving
either way. The branch you pick is the same wherever you are; the remedy it
names is not.

That is the whole classification, and it is narrow on purpose. It is *not*
"classify by risk" — "could this cause incorrect execution?" invites a
judgement call at every site, whereas "can this row be absent, and if not,
what exactly survives?" has an answer you can defend in a review.

**The third branch is for write paths, and it was added because the first two
gave no answer at one.** A collection loop that gathers ids *in order to delete
or rewrite them* is inside the rule's scope but off both of its horns.
`delete_agent` is the case: a session id it cannot read is a session whose
messages, runs, and tool-call rows survive the delete with no parent agent and
no retry path, because the agent row is gone. Absence is **not** safe there —
the rows leak, permanently. But nothing re-executes, so fatal is wrong, and
actively the worse answer: failing the delete makes the agent permanently
undeletable, which is the #1236 pattern of a false belief disabling its own
remedy. Quarantine, counted, with the leak named at the site is the least bad
outcome — and the leak stays recoverable by hand precisely because the rows are
all still there. Durable garbage is a real third outcome; a rule that pretends
otherwise gets quoted at a site it cannot classify.

**Check which way the collected list points before reaching for this branch.**
The additive qualifier is what keeps it safe, and the same idiom supports both
polarities. `delete_agent`'s DM-candidate loop gathers sessions *to purge*, so
a dropped row leaks a session that should have gone. Written the other way
round — gathering the sessions to *keep*, which is how a retention sweep or a
session GC would naturally be written — a dropped row would **delete a live
session instead of leaking a dead one**. Nothing re-executes in either case and
both leave durable state inconsistent, which is why the branch is worded around
stranding rather than around inconsistency: only the first is additive, and
only the first is quarantinable.

### Why the axis is not "fail closed vs. run degraded"

Refusing to boot is not a safety property. It is availability spent to buy one,
and it only pays off if the operator acts before harm occurs. ALMS is a
single-process daemon: refusing to boot removes the API, the UI,
`DELETE /jobs`, `DELETE /sessions`, and every diagnostic the product ships,
leaving the operator with `sqlite3` against a stopped daemon. Startup is the
moment we have the *least* leverage to help them, so it is the worst place to
choose unavailability.

The hazard was never "the daemon is running". It is "the daemon believes
something false". So the rule bites on **projection into live state**, not on
continuation.

#1236 is the proof. It made the right *policy* call — skip the unreconcilable
run row, keep booting — and still shipped a defect, because
`hydrate_from_store` then loaded that same row into the live run registry as
`Queued`. The phantom made the session permanently undeletable
(`DELETE /sessions/{id}` → 409 `ACTIVE_RUNS`), pinned the sidebar's active-run
indicator on forever, and — being older than every real run — sorted to the
head of the FIFO queue and shifted every real queue position for that agent.
Same policy, two implementations, one safe and one not.

### The four obligations

A site may quarantine only if it does **all four**:

1. **A counter on `GET /operations/metrics`** — visible without log access.
2. **One `warn!`/`error!` line carrying the strongest identifier available** —
   the row id where the row is identifiable, otherwise the table and the
   failing column. Bounded (once per row) at a one-shot sweep; unbounded (once
   per read) at a loader.
3. **A bounded blast radius: the quarantined fact must not reach live state.**
4. **A remediation that does not require stopping the daemon.** Through the
   product where the entity is addressable; `sqlite3` against the *live*
   database where it is not.

All four are requirements, with no exempt class. Obligations 2 and 4 are
stated in terms of what is *available* at the site rather than in terms of a
row id and a product endpoint, because at a loader neither necessarily exists
— see below. That is a weaker guarantee, not a waived one, and the next
section says exactly how much weaker.

**These bind a site that *quarantines*. A field-level degradation is not a
quarantine and cannot satisfy obligation 3** — see the scope note below.
Keeping the row *is* projecting it into live state, so there is no version of
that site which bounds the blast radius; the only way to discharge obligation 3
would be to drop the row, which at those four sites is the worse outcome. Do
not read this as a fourth escape hatch. It is a narrower permission: a site may
degrade a field instead of dropping the row **only** with an argument that
dropping is actively worse, and it still owes obligations 1, 2, and 4 in full,
against a *louder* counter precisely because obligation 3 is unavailable. If
you cannot make that argument, the row is unusable without the column and the
answer is a row skip.

**Obligation 3 is the one to check in review.** The skip and the projection
almost always live in different functions, and a reviewer checks the function
that changed.

Obligations 3 and 4 are more entangled than they look. In #1236 the phantom
`Queued` run was *what made* `DELETE /sessions/{id}` return 409 — so fixing
obligation 3 restored obligation 4 for free. **A false belief often disables
its own remedy.** Expect those two failures together, and suspect a missing
obligation 3 whenever the in-product repair path is blocked.

#### How one-shot sweeps and per-read loaders discharge obligations 2 and 4

A startup sweep runs once, so it can afford an `error!` per row carrying the
row id and the exact repair SQL, and its counter is effectively a count of
distinct bad rows. A loader runs on *every read*: the same corrupt row is
re-encountered by every caller, so its log stays at `warn!` and its counter
necessarily counts **skips, not rows** (see `persistence_rows_skipped_total`).

**Obligation 2** is discharged at a loader by the strongest identifier
available rather than by the row id, because frequently there is no row id to
give: the column that failed to parse *is* the identifier — `sessions.id`,
`runs.agent_id`, `agents.created_at` are the cases we have tests for. What the
operator gets is the table and the failing column, and the line cannot be
bounded. That is genuinely less than a sweep offers, and **the counter is what
carries the weight instead**. It is the reason #1241 was worth doing on its
own: it converts "did we silently lose rows?" from an archaeology exercise
across log files into a question with an answer.

**Obligation 4** is discharged at a loader by `sqlite3` against the *live*
database rather than through the product — and it has to be, for the same
reason. The in-product repair for a corrupt `sessions` row would be
`DELETE /sessions/{id}`, which cannot be addressed when the unparseable column
is the id. What obligation 4 protects is the operator's ability to act **while
the daemon serves**, and that is intact: edit or delete the row and the next
read picks up the repair. Where the entity *is* addressable — the stale-run
and job-bootstrap sweeps — the product path is required, not optional.

### Every reconciliation site we have

| Site | Is absence safe? | Policy | Accounting |
|---|---|---|---|
| `mark_stale_runs_failed` (`alms-session/src/sqlite/runs.rs`) — a `queued`/`running` row left by a dead process | **Yes.** The run is dead either way; nothing re-executes. | Quarantine | `stale_run_recovery_failures_total`; `error!` with the `run_id` and its remediation SQL. The row is also excluded from `RunManager::hydrate_from_store`, so it is never served as a live pending run (obligation 3). |
| `bootstrap_scheduler` (`alms-gateway/src/server/mod.rs`) — a job whose startup fire time cannot be persisted | **Yes.** "This job is not scheduled" is truthful and safe. | Quarantine | `job_bootstrap_failures_total`; `error!` with the job id. |
| The 25 row-drop points across 14 **loaders** in `alms-session` — any row that fails to parse | **Yes**, by the same argument. Nothing re-executes and nothing is left behind; the row is simply not served. | Quarantine | `persistence_rows_skipped_total`, per table (#1241); `warn!` with the table and the parse error. |
| The 4 row-drop points on **write paths** in `alms-session` — `delete_agent` (session ids, DM candidates, and the peer probe in the row below) and `migrate_telegram_context_ids` | **No — third branch.** Nothing re-executes, but the delete leaves that session's dependent rows orphaned, or the session keeps its legacy `telegram_{chat_id}` context id. | Quarantine, with the leak named at the site | `persistence_rows_skipped_total` under `sessions` (#1241); `warn!` whose `detail` is prefixed with the site, so it is distinguishable from a loader drop on the same table. |
| The 4 **field-level degradation** points — `parse_run_row` (`job_id`, `parent_run_id`), `parse_session_summary_row` (`last_run_id`), and `delete_agent`'s agent-name lookup | **Not the question here.** The row is *kept*; dropping it is what would be unsafe (see the scope note below). | Degrade the column, keep the row — obligation 3 is unattainable and is traded for a louder counter | `persistence_fields_degraded_total`, per `<table>.<column>` (#1246); `warn!` naming the row and the consequence. |
| DM-cascade peer probe in `delete_agent` — `SELECT 1 FROM agents WHERE name = ?` | **No, and the failure direction is the point.** A false "peer absent" *deletes* a DM session whose peer is alive, which is not additive and not quarantinable. But refusing to answer is: the session is simply not purged. | Quarantine the *answer*, not the row — only a peer **proven** absent may purge; anything unprovable leaves the DM session stranded and lets the delete commit | `persistence_fields_degraded_total` under `agents.name` when the peer's own name is unreadable, `persistence_rows_skipped_total` under `sessions` when the probe itself fails (#1246). |
| Schema-version guard (`alms-session/src/sqlite/migrations.rs`) — a database newer than the binary | **No.** You cannot interpret rows you cannot read, and job completion state is among them: a completion record you cannot read is indistinguishable from "not yet run", so already-executed jobs fire again. | **Fatal** — `refusing to open` | n/a |

The jobs row deserves its justification spelled out, because it is the one most
likely to be cited wrongly later. Jobs are non-fatal **not** because
availability beats correctness, but because *"this job is not scheduled"* is a
true and safe statement, whereas *"no jobs at all are scheduled and the daemon
is down"* is neither. Availability is the by-product, not the justification. If
this were written down as "availability wins", someone would cite it at the
migration guard next year.

**Scope note: field-level fallbacks are a second class, not a weaker one.**
The rule above is about *rows*. Some parsers instead apply **field-level**
fallbacks, keeping the row and degrading one column. Those are deliberately not
counted in `persistence_rows_skipped_total`, which would otherwise stop meaning
"rows the daemon cannot see" — the rows are still served, they are just wrong.
They have their own counter, `persistence_fields_degraded_total` (#1246), with
a `<table>.<column>` breakdown.

**Do not read "the row survives" as "this is the milder outcome".** It is the
less *contained* one, because a row skip discharges obligation 3 and a field
degradation cannot. A skipped row is kept out of live state, so the blast
radius is bounded by definition: the daemon's view is incomplete but nothing it
serves is false. A degraded field is projected into live state by construction
— there is no way to withhold one column without dropping the whole row — and
it is invisible from the outside, because a degraded value does not read as
"corrupt", it reads as an ordinary value. `runs.job_id → None` reads as "this
run has no job".

The worked example is `session_summaries.last_run_id`, because there the false
diagnosis *is* the damage. That column is the compare-and-swap sentinel for
episodic summary upserts (#1123). Degraded to `None`,
`upsert_session_summary_optimistic` takes its `WHERE last_run_id IS NULL`
branch, matches nothing against the non-NULL garbage cell, falls through to the
`INSERT`, trips the unique constraint, and reports a **conflict**. The caller
reloads the same degraded `None` and retries three times — each attempt a fresh
LLM summarization call — then logs a concurrency error naming a cause that is
not the cause. Episodic memory for that session can never be updated again, and
every signal the operator has points somewhere else. That is a false belief
projected into live state, which is the precise hazard this whole rule exists
to name.

**Which class a site belongs to is decided by the same one-question test,**
applied to the *field* rather than the row: if dropping the row would leave the
daemon behaving no worse than the degraded row does, drop it — that is a row
skip, and `persistence_rows_skipped_total` is the counter. Degrading is
justified only where **dropping is actively worse**, and that argument has to
be made per field. `DegradedField::ALL` is the enforced inventory — a test
pins the exact label set, so a fifth site cannot be added without failing it.
The four we have (#1246) are:

| Field | Read path | Why it degrades rather than drops |
|---|---|---|
| `runs.job_id` | Boot only | The run is no longer attributable to its job: `GET /runs` reports `job_id: null` and `derive_trigger` labels it `"user"` instead of `"scheduled"`. Dropping costs the boot: `mark_stale_runs_failed` collects its sweep with `collect::<Result<_, _>>()` and `Gateway::new` propagates that with `?` (`alms-gateway/src/gateway.rs`), so a row-level parse error there means the daemon does not start (#1236) — while the durable row keeps claiming `queued`/`running` forever, because the sweep can no longer see it. Note that `hydrate_from_store`'s *own* call to the same sweep swallows the error and returns; it is specifically the `Gateway::new` call that makes this fatal. |
| `runs.parent_run_id` | Boot only | Same shape, smaller still: the run reads as top-level rather than as a subagent run of its parent, plus a null `parent_session_id` breadcrumb. Dropping a whole run to fix one attribution field is plainly worse. |
| `session_summaries.last_run_id` | **Live** | Not attribution at all — it is the episodic-summary CAS sentinel, and degrading it deadlocks every future summary write for that session behind a false conflict, at three LLM calls per attempt (see above). Dropping the row loses the summary for the same remediation, and hides the cause. This is the only degradation site on a live read path, so it is the one whose counter can climb fast. |
| `agents.name` | Write path | Does not mis-attribute a row — it makes `delete_agent` skip DM cleanup, stranding shared DM sessions whose participants are all gone. Additive stranding, so it is the third branch above; counted and named at the site. This site distinguishes `QueryReturnedNoRows` (no such agent — skipping cleanup is *correct*, not a fault, and is not counted) from a genuine read failure, and also covers the DM-cascade peer-probe row in the site table above. |

**Do not discriminate by column kind.** The distinction is not "enum fallbacks
are mild, foreign-key fallbacks are not" — it is whether **the fallback value
is one the operator could legitimately have configured.** `reasoning_effort →
None` and `worktree_mode → Off` pass that test: they land on a config default a
human could have chosen, nothing downstream can tell the difference, and there
is no difference to tell. `job_id → None` fails it — `None` is not "the default
job", it is "no job", and that is a claim about the world rather than a setting.

`str_to_run_status` is the case that proves the discriminator matters, because
it is an *enum* fallback that fails the test: an unrecognised status becomes
`Queued`, `load_all_runs` has no status filter, and hydration then classifies
the row as an unreconciled queued row, drops it, and logs an error pointing at
`stale_run_recovery_failures_total` — a sweep that never touched it, since
`mark_stale_runs_failed` selects only `status IN ('queued','running')`. The run
disappears from history and the operator is sent to the wrong counter. It is
left uncounted (#1246 scoped itself to foreign keys) and documented at the site;
do not cite it as an example of a benign enum fallback.
`runs.lifecycle_revision` (`row.get(15).unwrap_or_default()` → `0`) is a third
fallback in the same parser that fits neither bucket cleanly; it is at least
partly covered downstream by `persistence_snapshot_rejections`.

**Check the polarity of a fallback before picking a counter for it.** Every
other site in this section fails towards *keeping* rows, which is why
quarantine works: the damage is additive and repairable by hand. The DM-cascade
peer probe in `delete_agent` points the other way. It reads `SELECT 1 FROM
agents WHERE name = ?` to decide whether a shared DM session is unreachable,
and a `false` sends the session to the purge list — so a fallback there
*deletes* a DM session whose peer is alive, which the "must be additive"
qualifier above puts outside the quarantinable class entirely.

The rule that site enforces is **only a peer proven absent may purge**, which
is stricter than "only `QueryReturnedNoRows` means absent" and deliberately so.
`QueryReturnedNoRows` is not by itself proof of absence for a probe keyed on
`name`: if the peer's *own* `agents.name` cell is a BLOB or NULL, `name = ?`
with a text parameter matches nothing — SQLite never compares a BLOB equal to a
TEXT value, and TEXT column affinity does not convert an already-stored BLOB —
so a live peer is indistinguishable from an absent one through exactly the
branch the site trusts. The probe therefore only accepts a miss as absence when
every `agents.name` cell in the table is readable text, and that check fails
closed.

Note the *disposition* for the two unprovable cases: they strand, they do not
fail. Refusing the delete would be safe too, but between two safe options the
same test that decided `agents.name` decides this one — a corrupt `agents`
table must not make every agent that has ever had a DM permanently
undeletable, which is the #1236 pattern of a false belief disabling its own
remedy. Stranding is additive and counted; the delete commits.

### Exactly one fatal site

Applied across the system, the rule yields **exactly one sanctioned fatal
reconciliation site** — the schema-version guard in
`alms-session/src/sqlite/migrations.rs` — and it was already fatal before the
rule was written down.

That is a feature, not an accident. "One fatal site" is easy to state, easy to
test, and easy to notice when someone adds a second. **A second fatal site
should be conspicuous**, and adding one needs an explicit argument that absence
is unsafe — that is, that dropping the row would cause work already done in the
world to be done again. Absent that argument, the answer is quarantine.

The qualifier *reconciliation* is load-bearing. Startup also fails closed when
the schema itself cannot be established — a gap in migration history, a
migration body that will not apply, a file-backed database that refuses WAL.
Those are not judgements about what to believe about a row; they mean **no**
row can be trusted to be interpretable, which is upstream of the rule rather
than an exception to it. See
[Database migrations, compatibility, and rollback](database-migrations.md).

### No escape-hatch flag

Do **not** add a `--skip-recovery` flag or its env-var equivalent. It gets set
once during an incident at 2am and then lives in the systemd unit forever;
nobody removes it. It silently converts a one-time degraded boot into a
permanent policy change, which is worse than either policy chosen deliberately.

It is also unnecessary under this rule: if quarantine is the default the daemon
always boots, so the operator always has the API. **The escape hatch *is* the
running daemon.** Remediation belongs on the API surface, not in a startup
flag.

The one place a flag might seem justified — the migration refusal — is where it
is most dangerous. An `--ignore-schema-version` switch is the rollback-
corruption hazard with a CLI flag attached (see
[Database migrations, compatibility, and rollback](database-migrations.md)). If
that capability is ever wanted, it belongs in a separate offline `alms db`
subcommand that refuses to start the server, not in a serving flag.

### Boundary condition: the rule assumes a single writer

**This classification is only valid while exactly one process writes the
database.** A `running` row can be treated as absent only because no other
process is executing it. If ALMS ever supports two daemons against one
database — or any second writer to `runs` — then `running` rows stop being
safely absent and move to the **fatal** class, and every entry in the table
above must be re-derived from the test rather than inherited.

Any change that introduces a second writer must revisit this section in the
same change.

**Nothing currently enforces this.** The assumption is stated here and checked
by nobody: two daemons against one `.alms/alms.db` would each quarantine the
other's live `running` rows, and the first symptom would be lost runs rather
than an error. #1247 tracks making it fail at boot — an advisory lock on the
database file, or an `owner_pid`/`boot_id` row written under `BEGIN IMMEDIATE`
at open.

---

## Message Flow Example

**User:** "Build me a Rust web server with JWT auth"

```
[User] ──► [Top-level Agent]
                │
                │  Decides to delegate:
                ├── invoke_agent("Design the API schema") ──► [Subagent: Design]
                │                                                   │ returns OpenAPI spec
                │                                                   ▼
                ├── invoke_agent("Implement auth middleware") ──► [Subagent: Auth]
                │   (receives spec as context)                       │ returns auth code
                │                                                   ▼
                └── Synthesizes results: "Here's your server with JWT auth..."
                    │
[User] ◄────────────┘
```

The top-level agent decides *when* and *what* to delegate, and may sequence or parallelize direct children. Coordinator-created subagent runtimes do not register `send_message` or `invoke_agent`; peer DM is available to registered top-level agents, not to sibling subagents.

---

## Token Efficiency

Token cost is a first-class constraint:

- **Scoped subagent context** — Ephemeral subagents use a task-specific prompt; named subagents load their own registered identity, configuration, workspace, and persistent session history
- **Context compression** — `ContextBuilder` supports `truncate` (default) and canonical `compact`; `sliding-summary` remains a deprecated alias
- **Usage tracking** — `prompt_tokens` + `completion_tokens` are accumulated per run; child usage remains on the child run instead of being rolled into the parent total
- **Cost observability** — `run_finished` SSE event and `GET /runs/{id}` expose per-run token counts
- **Episodic budget cap** — Cross-session summaries are hard-capped at 15% of `max_input_tokens` so episodic context never starves the current conversation
- **Explicit model selection** — Named-agent provider/model overrides are implemented; automatic complexity-based routing is not

---

## Implementation Status

### Completed ✅
- Core types, session manager, agent runtime, native tool registry
- HTTP gateway with SSE streaming, approval workflow, audit log
- Built-in tools: echo, math, http_get, shell (primary name; bash -c, persistent cwd, background execution, 30KB truncation; shell_exec alias preserved), fs_read, fs_write, fs_list, fs_edit, fs_grep, fs_glob, workspace_write, workspace_read, invoke_agent, read_subagent_session, send_message, list_agents, read_messages, ignore_message, list_my_sessions, read_session
- Cron/scheduler, SQLite persistence, web UI with agent selector
- Coordinator with real AgentRuntime loops, foreground + background subagents
- `invoke_agent` tool with `name` param for persistent subagent sessions (UUID v5 deterministic identity)
- Named subagent workspaces with registry-based config (model, posture)
- `alms agent create` initializes workspace directory with empty identity files
- Default system prompt includes CLI awareness (`alms --help` via shell_exec)
- Subagent lifecycle and coarse activity events surface in the parent stream; full child content stays on the child session stream
- Child runs and sessions are queryable through the run/session APIs; there is no public `/tasks` route
- Agent registry with named persistent agents, per-agent config overrides
- Token-by-token SSE streaming and canonical `compact` context compression (`sliding-summary` remains a deprecated alias)
- Peer-to-peer direct messaging via `send_message` tool + MessageBus + DM sessions with perspective mapping (Layer 2 Phase 1)
- DM conversation lifecycle: `ignore_message` and depth-exceeded trigger `end_conversation` with `dm_ended` session markers, depth counter reset, `ConversationEnded` peer notification via `notifications:{agent}` sessions, and `dm_conversation_ended` SSE events (#384 Phases 1-7)
- Cross-session episodic memory via run summaries (`session_summaries` table, heuristic + LLM modes, context injection with 15% budget cap)
- Provider-neutral reasoning stream (#767–#769) — Anthropic extended-thinking and OpenAI-compat reasoning models share the same `RuntimeEvent::ReasoningDelta` / `reasoning_delta` SSE event / `ReasoningPanel` UI / `reasoning_blocks` persistence. Each provider family has a dedicated config knob:
  - **Anthropic**: `[llm.anthropic].thinking_budget_tokens` — streams `thinking_delta` content blocks.
  - **OpenAI-compat** (#768): `[llm.openai].reasoning_effort` — `"low"/"medium"/"high"/"minimal"`; serialized on the wire only for reasoning-capable models (OpenAI o-series / GPT-5 / xAI Grok reasoning variants). Stripped for non-reasoning OpenAI models (gpt-4o etc.) and for DeepSeek R1 (reasoning is implicit on `deepseek-reasoner`). Response-side, `reasoning_content` / `reasoning_summary` / `reasoning` wire fields all deserialize into the same `reasoning_content` channel with a priority ordering (canonical > summary > raw). OpenAI o-series `usage.completion_tokens_details.reasoning_tokens` and DeepSeek/xAI flat `usage.reasoning_tokens` both flow through `TokenUsage.reasoning_tokens`.
  - **Gemini** (#769): `[llm.gemini].thinking_budget` enables `includeThoughts`; streamed `thought: true` parts use the shared reasoning channel, and `usageMetadata.thoughtsTokenCount` populates `TokenUsage.reasoning_tokens`.

### Pending 🎯
- Recursive subagent spawning, richer first-class progress reporting, and
  bounded `invoke_agent` result truncation — see the current-status banner in
  `docs/autonomous-subagents-design.md`

---

## Code Structure

```
crates/
  alms-core/          # Shared types, errors, unified config
  alms-coordinator/   # Subagent lifecycle management (hierarchy root)
  alms-runtime/       # Agent loop (shared by top-level agents and subagents)
                      #   agent/ — AgentRuntime, loop, context building, DM helpers
                      #   agent/environment.rs — constructor and order-safe environment/tool builders
                      #   context.rs — ContextBuilder (token-budgeted context window)
                      #   workspace.rs — AgentWorkspace (personality/goals/memories/user files)
                      #   workspace_tool.rs — WorkspaceWriteTool (stays here, depends on AgentWorkspace)
  alms-tools/         # Tool implementations extracted from alms-runtime
                      #   8 agent tools (send_message, invoke_agent, read_session, etc.)
                      #   SubagentDispatcher, MessageSender traits
                      #   EventForwarder trait for type-erased runtime event forwarding
  alms-session/       # Session state, SQLite persistence
  alms-sandbox/       # Tool execution, native builtin tools, registry
  alms-channel/       # User-facing adapters (Telegram)
  alms-gateway/       # HTTP/SSE control plane and embedded web UI
                      #   configuration/resolution.rs — agent/provider/model resolution
                      #   runs/read_api.rs — read-only run query endpoints
                      #   runs/lifecycle.rs — create/cancel/execution lifecycle
                      #   runs/notifications.rs — background completion and notification routing
  alms-cli/           # CLI entrypoint
```

### Dependency graph (no cycles, 9 crates)

```
alms-cli → alms-gateway → alms-runtime      → alms-core
                        → alms-tools        → alms-core
                                            → alms-session
                                            → alms-sandbox
                        → alms-coordinator  → alms-core
                                            → alms-session
                                            → alms-runtime
                                            → alms-tools
                        → alms-channel      → alms-core
                        → alms-session      → alms-core
           alms-runtime → alms-sandbox      → alms-core
                        → alms-session
         → alms-session
```

The `EventForwarder` trait in `alms-tools` enables type-erased event forwarding from subagent runs back to the gateway's SSE stream, without introducing a dependency from alms-tools to alms-runtime.

---

*Architecture updated: 2026-08-01*
*Topology: one-level delegation (`invoke_agent`) + Peer DM (`send_message` via MessageBus)*
