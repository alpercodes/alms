# System Prompts

All developer-controlled system prompts live in `crates/alms-runtime/prompts/` as
Markdown files, embedded at compile time via `include_str!()`. They are not
user-editable at runtime; user-facing customization happens through the workspace
files (`personality.md`, `goals.md`, `memories.md`, `user.md`).

## Prompt Files

| File | Purpose | Used By |
|------|---------|---------|
| `initial.md` | Base system prompt for all top-level agents. Sets the agent's default behavior and mentions the `alms --help` CLI discovery hint. | `AgentConfig::default()` in `crates/alms-runtime/src/agent.rs` |
| `tool_loop.md` | Continuation prompt appended to the system message after tool results. Tells the LLM to analyze results and decide whether to use more tools or respond. | `SystemPrompts::default()` in `crates/alms-runtime/src/agent.rs` |
| `bootstrap.md` | First-time agent onboarding prompt. Replaces the initial prompt when `personality.md` does not exist. Guides the agent through an interview to populate workspace files. | `AgentWorkspace::bootstrap_prompt()` in `crates/alms-runtime/src/workspace.rs` |
| `dm_recipient.md` | Template appended to the system prompt when the agent receives a direct message. Contains a `{peer}` placeholder replaced at runtime with the sender's name. | `build_context()` in `crates/alms-runtime/src/agent.rs` |
| `subagent.md` | Default system prompt for ephemeral (unnamed) subagents spawned via `invoke_agent`. | `DEFAULT_SUBAGENT_PROMPT` constant in `crates/alms-coordinator/src/lib.rs` |
| `summarizer.md` | System prompt for the sliding-summary LLM call that compresses old conversation history into a rolling summary. | `maybe_summarize()` in `crates/alms-runtime/src/agent.rs` |
| `session_summarizer.md` | System prompt for the episodic memory LLM call that generates cross-session summaries after each run. Focus on *what was accomplished*, not how. 1-3 sentences, past tense. | `generate_llm()` in `crates/alms-runtime/src/episodic.rs` |

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

**Code path**: `agent_loop()` in `agent.rs` -- after processing tool results, the
system message at `messages[0]` is rebuilt as:
```
assemble_system_prompt(initial_prompt + "\n\n" + tool_loop_prompt)
```

### `dm_recipient.md` -- Direct Message Sessions

When the context ID starts with `"dm:"`, the `build_context()` method extracts the
peer agent's name and appends the DM addendum to the system prompt. The `{peer}`
placeholder in the template is replaced with the actual peer name.

**Code path**: `build_context()` in `agent.rs` -- after assembling the base system
prompt with workspace prefix, if the context is a DM session. Additionally,
`agent_loop()` re-injects the addendum via the `dm_addendum()` helper on every
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

**Code path**: `maybe_summarize()` in `agent.rs` -- builds a separate LLM request
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
- Max 150 output tokens
- Uses `summary_model` if configured, otherwise falls back to the agent's default model
- Errors are logged and swallowed -- summary failure must never fail the run

## Prompt Assembly Order

The full system prompt seen by the LLM is assembled in layers:

1. **Workspace prefix** (optional): If the agent has a workspace attached,
   `build_system_prompt_prefix()` reads workspace files in this order:
   - `personality.md` (raw content)
   - `goals.md` (prefixed with `## Current Goals`)
   - `user.md` (prefixed with `## About the User`) — **conditional**: skipped for
     non-user-facing sessions (DM, subagent, and job contexts) to save tokens
   - `memories.md` (prefixed with `## Memories`, truncated at 4000 chars)

2. **Base prompt**: Either `initial.md` (normal runs) or `bootstrap.md` (first-time
   setup). These are joined as: `{workspace_prefix}\n\n{base_prompt}`

3. **DM addendum** (optional): For DM sessions, `dm_recipient.md` (with `{peer}`
   replaced) is appended after the base prompt: `{assembled}\n\n{dm_addendum}`

4. **Tool loop addendum** (after first tool round): On subsequent LLM calls in the
   same agent loop, the system message is rebuilt as:
   `{workspace_prefix}\n\n{base_prompt}\n\n{tool_loop_prompt}`
   For DM sessions, the DM addendum is also re-injected after the tool loop prompt:
   `{workspace_prefix}\n\n{base_prompt}\n\n{tool_loop_prompt}\n\n{dm_addendum}`
   This ensures the agent retains awareness that it must use `send_message` to reply,
   even after processing tool calls (fixes #346).

The assembly is handled by `assemble_system_prompt()` which prepends the workspace
prefix to any base prompt string.
