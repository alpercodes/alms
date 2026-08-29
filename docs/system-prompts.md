# System Prompts

All developer-controlled system prompts live in `crates/alms-runtime/prompts/` as
Markdown files, embedded at compile time via `include_str!()`. They are not
user-editable at runtime; user-facing customization happens through the workspace
files (`personality.md`, `goals.md`, `memories.md`, `user.md`).

## Prompt Files

| File | Purpose | Used By |
|------|---------|---------|
| `initial.md` | Base system prompt for all top-level agents. Sets the agent's default behavior and mentions the `alms --help` CLI discovery hint. | `AgentConfig::default()` in `crates/alms-runtime/src/agent/types.rs` |
| `tool_loop.md` | Continuation prompt appended to the system message after tool results. Tells the LLM to analyze results and decide whether to use more tools or respond. | `SystemPrompts::default()` in `crates/alms-runtime/src/agent/types.rs` |
| `bootstrap.md` | First-time agent onboarding prompt. Replaces the initial prompt when `personality.md` does not exist. Guides the agent through an interview to populate workspace files. | `AgentWorkspace::bootstrap_prompt()` in `crates/alms-runtime/src/workspace.rs` |
| `dm_recipient.md` | Template appended to the system prompt when the agent receives a direct message. Explains the implicit-reply contract (#1154): the final message text is delivered to the peer automatically. Contains a `{peer}` placeholder replaced at runtime with the sender's name. | `build_context()` in `crates/alms-runtime/src/agent/context.rs` |
| `subagent.md` | Default system prompt for ephemeral (unnamed) subagents spawned via `invoke_agent`. | `DEFAULT_SUBAGENT_PROMPT` constant in `crates/alms-coordinator/src/lib.rs` |
| `summarizer.md` | System prompt for the sliding-summary LLM call that compresses old conversation history into a rolling summary. | `maybe_summarize()` in `crates/alms-runtime/src/agent/context.rs` |
| `session_summarizer.md` | System prompt for the episodic memory LLM call that generates cross-session summaries after each run. Focus on *what was accomplished*, not how. 1-3 sentences, past tense. | `generate_llm()` in `crates/alms-runtime/src/episodic.rs` |
| `dm_summarizer.md` | DM-specific instruction prepended to the summarizer transcript. Tells the LLM to preserve per-agent attribution. Contains a `{transcript}` placeholder. | `maybe_summarize()` in `crates/alms-runtime/src/agent/context.rs` |
| `dm_empty_reply_retry.md` | Nudge injected when a peer-triggered DM run is about to end with no deliverable reply text (#1154 implicit replies). Tells the agent its final message text IS the reply, or to use `ignore_message` to end. | `DM_EMPTY_REPLY_RETRY_MSG` in `crates/alms-runtime/src/agent/dm.rs` |
| `dm_ended_with_history.md` | DM conversation ended notification template with embedded transcript. Contains `{reason}` and `{history}` placeholders. | `format_dm_ended_notification()` in `crates/alms-gateway/src/runs.rs` |
| `dm_ended_no_history.md` | Fallback DM conversation ended notification when history is unavailable. Contains `{reason}` and `{from}` placeholders. Points agent to `read_messages`. | `format_dm_ended_notification()` in `crates/alms-gateway/src/runs.rs` |
| `subagent_completed.md` | Background subagent completion notification template. Contains `{label}`, `{status}`, `{summary}`, and `{follow_up}` placeholders. | `format_completion_notification()` in `crates/alms-gateway/src/runs.rs` |

## When Each Prompt Is Used

### `initial.md` -- Default Agent Runs

Every `AgentConfig::default()` loads `initial.md` as `system_prompt`. This is the
prompt used for normal HTTP runs and Telegram-triggered runs when the agent already
has workspace files.

**Code path**: `AgentConfig::default()` -> `include_str!("../prompts/initial.md")`

### `bootstrap.md` -- First-Time Agent Setup

When a named agent has no `personality.md` file (detected by
`AgentWorkspace::needs_bootstrap()`), the gateway replaces the initial system prompt
with the bootstrap prompt. This happens in two places:

1. HTTP runs: `crates/alms-gateway/src/runs.rs` -- `start_run()` checks
   `workspace.needs_bootstrap()` before creating the runtime.
2. Telegram runs: `crates/alms-gateway/src/gateway.rs` -- the Telegram polling
   loop checks the same condition.

**Code path**: `AgentWorkspace::bootstrap_prompt()` -> `include_str!("../prompts/bootstrap.md")`

### `tool_loop.md` -- After Tool Execution

When the LLM returns tool calls and the agent loop processes them, the system
message is updated before the next LLM call. The tool loop prompt is appended to the
initial prompt (not replacing it), so the agent retains its identity while getting
continuation guidance.

**Code path**: `agent_loop()` in `agent/loop_impl.rs` -- after processing tool results, the
system message at `messages[0]` is rebuilt as:
```
assemble_system_prompt(initial_prompt) + "\n\n" + tool_loop_prompt
```
i.e. the base prompt and workspace prefix are assembled first, then the
`tool_loop` continuation guidance is appended on top. For DM sessions the
DM addendum is appended last (see § "Prompt Assembly Order").

### `dm_recipient.md` -- Direct Message Sessions

When the context ID starts with `"dm:"`, the `build_context()` method extracts the
peer agent's name and appends the DM addendum to the system prompt. The `{peer}`
placeholder in the template is replaced with the actual peer name.

**Code path**: `build_context()` in `agent/context.rs` -- after assembling the base system
prompt with workspace prefix, if the context is a DM session. Additionally,
`agent_loop()` in `agent/loop_impl.rs` re-injects the addendum via the `dm_addendum()` helper on every
tool-loop system prompt rebuild, so the agent retains DM awareness across
tool-call iterations (see #346).

### `subagent.md` -- Ephemeral Subagents

When `invoke_agent` spawns an ephemeral (unnamed) subagent, the coordinator uses the
default subagent prompt as `system_prompt`. Named subagents also use this prompt by
default (their per-agent config does not override the system prompt).

**Code path**: `agent_config_for_subagent()` in `crates/alms-coordinator/src/lib.rs`

### `summarizer.md` -- Context Compression

When the context strategy is `"sliding-summary"` and enough new messages have
accumulated past the summary interval, the `maybe_summarize()` method calls the LLM
with the summarizer prompt to compress old messages into a rolling summary.

**Code path**: `maybe_summarize()` in `agent/context.rs` -- builds a separate LLM request
with the summarizer system prompt and a user message containing the transcript.

### `session_summarizer.md` -- Episodic Memory Summaries

When `run_summary_mode` is set to `"llm"`, the gateway spawns a fire-and-forget task
after each successful run to generate a cross-session episodic summary. This task
builds a lightweight LLM request with the session summarizer prompt, the run input
(truncated to ~2000 chars), the agent's output (truncated to ~2000 chars), and the
existing summary (if any). The LLM produces a concise 1-3 sentence summary focused
on what was accomplished, not internal steps.

**Code path**: `generate_llm()` in `episodic.rs` -> `include_str!("../prompts/session_summarizer.md")`

**Key constraints**:
- Configurable output token limit via `context.summary_max_tokens` (default 1000). The higher default provides headroom for reasoning models that consume tokens on internal thinking before producing visible output.
- Uses `summary_model` if configured, otherwise falls back to the agent's default model
- Errors are logged and swallowed -- summary failure must never fail the run
- Summarizer input is sanitized: the run's extended-thinking trace is stripped from the assistant output via `strip_reasoning_from_output()` before either mode (heuristic or LLM) consumes it, so reasoning content can never leak into `session_summaries.summary` (#1098)

## Prompt Assembly Order

The full system prompt seen by the LLM is assembled in layers, with the
foundational role/identity prompt first and agent-specific personalization
appended after it:

1. **Base prompt**: Either `initial.md` (normal runs) or `bootstrap.md` (first-time
   setup). This is the foundational role/identity prompt and always comes first.

2. **Workspace prefix** (optional): If the agent has a workspace attached,
   `build_system_prompt_prefix()` reads workspace files and appends them after
   the base prompt: `{base_prompt}\n\n{workspace_prefix}`. The workspace files
   are concatenated in this internal order:
   - `personality.md` (raw content)
   - `goals.md` (prefixed with `## Current Goals`)
   - `user.md` (prefixed with `## About the User`) — **conditional**: skipped for
     non-user-facing sessions (DM, subagent, and job contexts) to save tokens
   - `memories.md` (prefixed with `## Memories`, tail-windowed at 4000 bytes — past
     the cap the agent is shown the *most recent* 4000 bytes behind a leading
     truncation marker, not the oldest; see `agent-runtime-design.md` § "Size
     management")

3. **Tool loop addendum** (after first tool round): On subsequent LLM calls in the
   same agent loop, the system message is rebuilt as:
   `{base_prompt}\n\n{workspace_prefix}\n\n{tool_loop_prompt}`

4. **DM addendum** (optional, always last): For DM sessions, `dm_recipient.md`
   (with `{peer}` replaced) is appended at the very end:
   `{base_prompt}\n\n{workspace_prefix}\n\n{dm_addendum}` on the first turn, and
   `{base_prompt}\n\n{workspace_prefix}\n\n{tool_loop_prompt}\n\n{dm_addendum}`
   on subsequent tool-loop turns. This ensures the agent retains awareness of
   the implicit-reply contract (#1154 — its final message text IS the reply to
   the peer), even after processing tool calls (fixes #346).

The base + workspace assembly is handled by `assemble_system_prompt()`. The
order matches common LLM prompting practice — role/identity first, personalization
later — and puts the most specific instructions nearer the end of the system block.

Note: this ordering does **not** in itself improve Anthropic prompt-cache hit rates.
The cache breakpoint in `anthropic.rs` attaches `cache_control` to the entire trailing
system block atomically, so any byte drift inside that block (workspace updates,
memory edits) invalidates the cached prefix regardless of the internal order. The
swap is structurally good but is cache-neutral.
