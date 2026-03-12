# Autonomous Subagents — Design Document

## Problem Statement

Today, subagents in ALMS are **request-response workers**. A parent agent calls `invoke_agent(task)`, the subagent runs its `agent_loop` (multi-iteration tool use), and returns a single text response. This works for simple delegation but falls short of the model we actually want:

**A subagent should behave like a colleague on a chat thread.** It has its own persistent session with the parent. It runs autonomously — reading files, using tools, reasoning across multiple steps — and decides itself when it's done. The parent gets notified on completion, not via polling. The parent can send follow-up messages to continue the conversation.

This is exactly how Claude Code's subagents work: launch a reviewer agent, it reads 25 files, runs analysis, builds a structured report, and returns when it decides it has a complete answer. The parent never micromanages the subagent's internal loop.

## What Already Works

| Capability | Status | How |
|---|---|---|
| Multi-iteration tool use | **Done** | `agent_loop` loops up to `max_iterations` — subagent already uses tools autonomously |
| Persistent session across invocations | **Done** | `name` param on `invoke_agent` → UUID v5 deterministic identity → same session reused |
| Background execution | **Done** | `background=true` → `dispatch_background()` → poll via `get_task_result` |
| SSE event forwarding | **Done** | Subagent tool events forwarded into parent run's event stream |
| Foreground blocking | **Done** | Parent's `dispatch()` awaits the oneshot channel until subagent completes |

The `agent_loop` is already multi-turn. A subagent getting the task "Review this code for security issues" will:
1. LLM call → decides to use `fs_read` to read files
2. Reads files, LLM processes results → decides to read more files
3. Reads more files → decides it has enough info → returns final analysis

This is autonomous multi-step execution. The subagent decides what tools to use and when it's done.

## What's Missing

### 1. Subagent Workspaces

**Problem:** Top-level agents have `AgentWorkspace` (personality.md, goals.md, memories.md, user.md) that shape their behavior. Subagents have none — they get a bare system prompt and no persistent identity files.

**Impact:** A named subagent like "reviewer" can't accumulate knowledge about the codebase it reviews. Every invocation starts with the same generic prompt, even though the session history gives it conversation context.

**Design:**

Subagents are created via the CLI (`alms agent create --name reviewer`), which registers them in the agent registry (SQLite `agents` table) and creates their workspace directory. The parent agent then calls `invoke_agent(name="reviewer", task="...")` — the coordinator looks up the agent record for config (system_prompt, model, posture) and attaches the workspace.

The parent can write to the subagent's workspace files (personality.md, goals.md, etc.) using `fs_write` or `shell_exec` — no special tools needed. The workspace path is `{workspace_dir}/{name}/`.

When `invoke_agent(name="reviewer")` is called, the coordinator:

1. Looks up the agent record in the registry (`load_agent_by_name`)
2. Uses the record's `system_prompt` for config (falls back to default if None)
3. Derives workspace directory: `{workspace_dir}/{name}/`
4. Creates `AgentWorkspace` pointing at that directory
5. Calls `runtime.with_workspace(workspace)` before running the agent loop
6. On first invocation, `needs_bootstrap()` returns true → the agent bootstraps itself

The workspace is tied to the **name**, not the task ID. Named subagent "reviewer" always uses the same workspace directory and the same session — it accumulates identity across invocations.

Ephemeral (unnamed) subagents do NOT get workspaces. They're one-shot workers.

**Note:** `system_prompt` was removed from the `invoke_agent` tool parameters. Subagent identity is defined at creation time (via CLI/API), not ad-hoc per invocation.

```
data/
  workspaces/
    reviewer/
      personality.md   ← "I am a code reviewer..."
      goals.md         ← "Review for security, correctness..."
      memories.md      ← "The codebase uses thiserror for errors..."
    researcher/
      personality.md
      goals.md
      memories.md
```

### 2. Recursive Subagent Spawning

**Problem:** Subagents can't spawn their own subagents. In `run_agent_loop`, the runtime is built with `AgentRuntime::new()` but `.with_invoke_agent()` and `.with_get_task_result()` are never called. A subagent that needs to delegate (e.g., a "project manager" agent that spawns "researcher" and "implementer" agents) can't do so.

**Design:**

Wire `invoke_agent` and `get_task_result` tools into the subagent's runtime, exactly as `execute_run()` does for top-level agents:

```rust
// In run_agent_loop (coordinator/lib.rs):
let invoke_tool = InvokeAgentTool::new(
    coordinator.clone(),  // self as Arc<Coordinator>
    session_id,           // subagent's own session as parent
    Some(run_id),
    Some(sub_tx.clone()),
);
let poll_tool = GetTaskResultTool::new(coordinator.clone());
let runtime = runtime
    .with_invoke_agent(invoke_tool)
    .with_get_task_result(poll_tool);
```

**Depth limit:** Add `max_depth: u32` to `SubagentRequest`. Decrement on each spawn. Reject at depth 0. Default: 3. Prevents unbounded recursive spawning.

**Coordinator access:** `run_agent_loop` currently takes `&Coordinator` implicitly via the trait. For recursive spawning, the coordinator needs to be `Arc<Coordinator>` passed into `run_subagent` and threaded into the subagent's tool registration. This is a refactor of the existing `run_subagent` function.

### 3. Completion Notification (Replace Polling)

**Problem:** Background subagents require the parent to poll via `get_task_result`. This wastes LLM iterations on polling calls and adds latency (parent doesn't know subagent is done until its next loop iteration happens to poll).

**Design:** Replace polling with an event-driven notification model:

```
Parent calls invoke_agent(task, name="reviewer", background=true)
  → Returns { task_id }
  → Subagent runs autonomously in background

Subagent finishes
  → Coordinator stores result (already done)
  → Coordinator injects a synthetic tool_result message into the PARENT's
    next context build, as if the parent had made a tool call that just resolved

Parent's next agent_loop iteration sees the injected result
  → LLM processes it naturally: "The reviewer finished. Here's what it found: ..."
```

**Implementation:**

a) Add a `pending_results: Arc<DashMap<RunId, Vec<CompletedSubagentResult>>>` to `Coordinator`.

b) When a background subagent completes, push its result into the parent's pending queue:
```rust
struct CompletedSubagentResult {
    task_id: TaskId,
    name: Option<String>,
    response: String,
    tokens_used: Option<usize>,
}
```

c) In `agent_loop` (or a new hook point), before building context for the next LLM call, check for pending results and inject them as system messages:
```
[System: Background subagent "reviewer" (task {id}) has completed.
Result: {response}]
```

d) The `get_task_result` tool remains available as a fallback for explicit polling, but the primary path is automatic injection.

**Alternative (simpler, phase 1):** Keep polling but make it cheaper. Instead of the LLM deciding to poll, add an automatic `get_task_result` check at the start of each `agent_loop` iteration for any outstanding background tasks. If any have completed, inject their results before the LLM call. No LLM iterations wasted on polling.

### 4. Progress Reporting

**Problem:** While SSE events are forwarded (tool_start, tool_end, token_delta), there's no structured way for a subagent to report intermediate progress back to the parent agent's reasoning. The parent is either blocking (foreground) or blind (background + polling).

**Design:** Add a `report_progress` tool that named subagents can use:

```json
{
  "name": "report_progress",
  "parameters": {
    "status": "string — current status (e.g., 'analyzing', 'found 3 issues', 'writing report')",
    "progress_pct": "number — optional, 0-100"
  }
}
```

**For background subagents:** Progress is stored on the `SubagentHandle` and retrievable via `poll_task` (which returns `Running` with an optional progress string) or via the pending-results injection mechanism.

**For foreground subagents:** Progress is emitted as a `RuntimeEvent::SubagentProgress` which gets forwarded to the parent's SSE stream. The UI can display it.

**For the parent agent's reasoning:** When a background subagent reports progress, it's injected as a system message in the parent's next context build (same mechanism as completion notification).

### 5. Context Isolation — Parent Sees Its Own Session, Not Subagent Internals

**Problem:** Today, when the parent calls `invoke_agent`, the subagent's full response comes back as a `tool_result` message in the parent's own session. This means:

- The parent's context window fills up with large subagent responses
- The parent carries ALL historical subagent results in its context, even for questions unrelated to those subagents
- If the reviewer wrote a 2000-token analysis three conversations ago, it's still in the parent's context window eating tokens
- The parent has no way to selectively access subagent conversations — it either has everything (bloated) or nothing

**The right model:** The parent's context should be its **own conversation with the user**. Subagent sessions are separate threads. The parent can pull in subagent context **on demand** when it decides it needs it.

Think of it like a manager:
- Your main chat is with the user
- You have separate chat threads with reviewer, researcher, implementer
- You don't carry all team conversations in your head at all times
- When the user asks "what did the reviewer find?", you pull up the reviewer's thread

**Design:**

#### A. Subagent results in parent context = short summaries

When `invoke_agent` returns, the tool_result stored in the parent's session should be a **short summary**, not the full subagent response:

```
Foreground invoke_agent returns:
  tool_result: {
    "subagent": "reviewer",
    "status": "completed",
    "summary": "Found 3 security issues in auth middleware. Use read_subagent_session('reviewer') for full details.",
    "tokens_used": 1247
  }
```

The full response lives in the subagent's own session. The parent gets enough to know what happened and decide whether to dig deeper.

**How:** The parent's LLM is instructed (via system prompt addition when subagents are available) that `invoke_agent` returns summaries, and `read_subagent_session` provides full context. The subagent's final response is stored in both places: full in subagent session, summarized in parent tool_result.

For short responses (< 500 tokens), the full response IS the summary — no truncation. The summary path only kicks in when the subagent returns a large response.

#### B. `read_subagent_session` tool — on-demand context retrieval

New tool that lets the parent selectively read a named subagent's conversation history:

```json
{
  "name": "read_subagent_session",
  "parameters": {
    "name": "string — the subagent's persistent name (e.g., 'reviewer')",
    "last_n": "number — optional, read only the last N messages (default: 20)",
    "summary_only": "boolean — optional, return only the session summary if one exists (default: false)"
  }
}
```

**Returns:**
```json
{
  "subagent": "reviewer",
  "message_count": 47,
  "messages": [
    {"role": "user", "content": "Review this PR for security issues"},
    {"role": "assistant", "content": "I'll start by reading the changed files..."},
    {"role": "assistant", "content": "[tool_call: fs_read('src/auth.rs')]"},
    ...
  ],
  "summary": "Optional rolling summary if sliding-summary is active"
}
```

**Implementation:** The tool derives the subagent's deterministic session ID (same UUID v5 logic as `invoke_agent`), then reads from `SessionManager`:

```rust
let parent_as_agent = AgentId(parent_session_id.0);
let stable_id = AgentId::deterministic(parent_as_agent, &name);
let stable_ctx = format!("subagent_{}_{}", parent_session_id.0, name);
let session = session_manager.get_or_create(stable_id, &stable_ctx);
let messages = session_manager.get_history(session.id)?;
```

This is cheap — no LLM call, just a session read. The parent's LLM decides when it needs context from a subagent and pulls it in.

#### C. Context flow diagram

```
User ←→ [Parent Agent]
              │
              │  Parent's session: user messages + assistant responses
              │  + invoke_agent tool_results (SHORT SUMMARIES only)
              │
              ├── invoke_agent(name="reviewer") ──→ [Reviewer session]
              │   tool_result: "Found 3 issues..."     │ Full conversation
              │                                         │ (47 messages, tool calls, etc.)
              │
              ├── invoke_agent(name="researcher") ──→ [Researcher session]
              │   tool_result: "Analyzed 5 papers..."   │ Full conversation
              │
              └── read_subagent_session("reviewer", last_n=5)
                  → Returns last 5 messages from reviewer's session
                  → Parent LLM uses this to answer user's follow-up question
```

#### D. When the parent reads subagent context

The parent's LLM naturally decides when to use `read_subagent_session`:

- User asks: "what did the reviewer find?" → parent calls `read_subagent_session("reviewer")`
- User asks: "tell the reviewer to look at auth.rs" → parent calls `invoke_agent(name="reviewer", task="Focus on auth.rs")` (no need to read first — the reviewer has its own history)
- User asks: "compare the reviewer's and researcher's findings" → parent reads both sessions, synthesizes

The parent doesn't need to be told when to read — it's a tool like any other. The LLM will use it when it needs information it doesn't have in its own context.

#### E. System prompt addition

When subagent tools are registered, add to the parent's system prompt:

```
You have access to named subagents via invoke_agent(). Each subagent has its own
persistent session — it remembers previous conversations. When you invoke a subagent,
you receive a summary of its response. To read the full conversation history of a
subagent, use read_subagent_session(name). You don't need to carry subagent details
in your own reasoning — just read their sessions when you need the context.
```

### 6. Parent-Subagent Conversational Flow (Already Works)

**Already works** via the `name` parameter:

```
Parent iteration 1:
  invoke_agent(task="Review this PR", name="reviewer")
  → Reviewer runs full loop, returns: "Found 3 issues: ..."

Parent iteration 2 (later, triggered by user asking a follow-up):
  invoke_agent(task="Can you elaborate on issue #2?", name="reviewer")
  → Reviewer has full conversation history from iteration 1
  → Returns detailed analysis of issue #2
```

The session persistence means the reviewer remembers everything from previous conversations. The parent doesn't need to re-explain context.

**Enhancement:** With subagent workspaces (#1 above), the reviewer also has `memories.md` where it can store cross-session knowledge like "this codebase uses thiserror" or "the team prefers explicit error types over anyhow".

---

## Implementation Plan

### Phase 1 — Complete the autonomous flow

| # | Task | Status | Notes |
|---|------|--------|-------|
| 70 | Registry lookup + workspace attach | **Done** | system_prompt/model/posture from registry, workspace at `{workspace_dir}/{name}/` |
| 81 | `read_subagent_session` tool | **Done** | On-demand context retrieval, 8 tests |
| 84 | `alms agent create` creates workspace dir | Todo | Parent can write files before first invocation |
| 82 | Truncate invoke_agent results in parent | Todo | Short summary in parent, full in subagent session |
| 83 | System prompt addition for context model | Todo | Instruct parent LLM about read_subagent_session |
| 75 | Validate `name` in invoke_agent | Todo | Validate against `validate_agent_name()` rules |

### Phase 2 — Autonomous polish

| # | Task | Status | Notes |
|---|------|--------|-------|
| 71 | Recursive subagent spawning + max_depth | Todo | Subagents spawn sub-subagents |
| 72 | Auto-inject completed background results | Todo | Event-driven completion, replace polling |
| 77 | Guard concurrent same-name invocations | Todo | Reject/queue duplicate invocations |

### Phase 3 — Advanced orchestration (future)

| # | Task | Status | Notes |
|---|------|--------|-------|
| 73 | `report_progress` tool | Todo | Intermediate status updates |
| 74 | SubagentProgress SSE event | Todo | UI shows subagent activity inline |
| 78 | Task decomposition | Todo | Plans → subtasks |
| 79 | Subagent clarification requests | Todo | Subagent asks parent mid-loop |
| 80 | Cost budget per tree | Todo | Token budget enforcement |

---

## How This Compares to Claude Code

| Feature | Claude Code | ALMS today | After Phase 1 |
|---|---|---|---|
| Subagent runs full multi-step loop | Yes | Yes (agent_loop) | Yes |
| Persistent session across invocations | N/A (ephemeral) | Yes (name param) | Yes |
| Subagent has own workspace/memory | No | Yes (#70) | Yes |
| Recursive subagents | Yes (agents spawn agents) | No | Yes (#71) |
| Completion notification | Automatic (foreground block) | Polling (background) | Auto-inject (#72) |
| Progress reporting | Via SSE events | Tool events forwarded | Structured progress (#73) |
| Parent-subagent multi-turn | No (one-shot) | Yes (via name param) | Yes |
| Context isolation | Implicit (ephemeral) | Partial (#81 done, #82-83 todo) | Yes — summaries + `read_subagent_session` |
| Task decomposition | Manual (parent decides) | Manual | Manual (Phase 3) |

ALMS's model is actually more capable than Claude Code's in two respects:
1. **Persistent named subagents with conversational memory.** Claude Code's subagents are ephemeral — they start fresh every time. ALMS subagents with `name` accumulate conversation history and (after Phase 1) workspace memories across invocations.
2. **Context isolation with on-demand access.** The parent's context stays lean (summaries only) and can selectively pull in subagent session detail when needed. This is more token-efficient than carrying full subagent transcripts.

---

## Key Design Decisions

1. **Workspaces only for named subagents.** Ephemeral subagents don't need identity files — they run once and are discarded. Named subagents are the ones that benefit from accumulated knowledge.

2. **Auto-injection over polling.** Background results should flow to the parent automatically, not require the parent to waste LLM iterations on polling. The `get_task_result` tool remains as an explicit fallback.

3. **Depth limit, not complexity limit.** We limit recursion depth (default 3) rather than trying to limit "complexity" — depth is easy to reason about and enforce. Token budgets (Phase 3) add cost control.

4. **Same `agent_loop` for all levels.** No special "orchestrator loop" vs "worker loop". Every agent — top-level, subagent, sub-subagent — runs the same `agent_loop` with the same tool execution pipeline. This is the current design and it's correct.

5. **Workspace directory is name-based, not UUID-based.** Named subagent workspaces live at `{workspace_dir}/{name}/` — a human-readable path the parent can write to via `fs_write` or `shell_exec`. The `AgentWorkspace::with_dir()` constructor skips the UUID subdirectory that top-level agents use.

6. **Context isolation by default, detail on demand.** The parent's context window contains only summaries of subagent results, not full transcripts. The `read_subagent_session` tool lets the parent pull in full context from any named subagent when it decides it needs it. This keeps the parent's context lean while preserving access to everything. Short responses (< 500 tokens) pass through unsummarized.

---

*Design Date: 2026-03-12*
*Status: In progress — Phase 1: #70 and #81 done, #84/#82/#83/#75 remaining*
*Author: Atlas + Alper*
