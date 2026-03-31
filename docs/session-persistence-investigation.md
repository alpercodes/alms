# Session Persistence Investigation

**Issue**: #310 -- Verify session history is properly persisted and prepended to system prompt
**Investigator**: Tim (automated)
**Date**: 2026-03-23
**Status**: Investigation complete -- no critical bugs found. Two notable gaps identified (see Recommendations).

---

## 1. Message Persistence After Runs

### 1.1 Persistence Path per Message Type

Each message type follows a clear write-through path:

| Message Type | Where Created | Session Persistence | Per-Run Persistence |
|---|---|---|---|
| **User input** | agent.rs:437-444 in run() | YES -- append_message() immediately after context built | N/A (stored in runs.input) |
| **Assistant text** (final response) | agent.rs:572-580 in finish_run() | YES -- unless empty or DM session | N/A (stored in runs.response) |
| **Assistant tool call** | agent.rs:1008-1027 in agent_loop() | YES (non-DM) -- one message per tool call with tool_call_id metadata | YES -- run_tool_calls table |
| **Tool result** | agent.rs:1104-1121 in agent_loop() | YES (non-DM) -- Content::ToolResult with tool_id and ok metadata | YES -- run_tool_calls table |
| **Assistant text before tool calls** | agent.rs:990-1005 in agent_loop() | YES (non-DM) -- persisted as Content::Text | N/A |
| **Cancellation marker** | agent.rs:608-620 in finish_run() | YES -- [Run cancelled by user] | N/A |
| **Error marker** | agent.rs:635-644 in finish_run() | YES -- [Run failed: reason] (sanitized) | N/A |

**Conclusion**: All message types are persisted for non-DM sessions. DM sessions intentionally skip session-level tool call persistence (tool calls go to run_tool_calls only, per design documented in CLAUDE.md). This is correct behavior.

### 1.2 Message Ordering

Messages are ordered by a seq column, not by timestamp:

- **INSERT logic** (sqlite/messages.rs:26-29): seq is computed as COALESCE(MAX(seq), 0) + 1 for the session via a subquery. Each new message gets the next sequence number.
- **SELECT logic** (sqlite/messages.rs:49): ORDER BY seq -- stable and deterministic.
- **ON CONFLICT** (sqlite/messages.rs:29): Re-inserting the same message ID preserves the original seq (only updates content and metadata). Test test_reinsert_message_preserves_ordering confirms this.
- **Auto-migration** (sqlite/mod.rs:179-181): Existing databases without seq get seq = rowid backfill and an index on (session_id, seq).

**Conclusion**: Ordering is correct and stable. No timestamp-based races possible.

### 1.3 Concurrency -- Race Conditions Between Concurrent Runs

The SessionManager uses DashMap with careful lock ordering:

- get_or_create() (lib.rs:107-130): Uses DashMap::entry() for atomic check-and-insert. Two runs on the same (agent_id, context_id) will never create duplicate sessions.
- append_message() (lib.rs:223-264): Scopes the history write lock and releases it before acquiring sessions / session_by_id locks. Lock ordering comment explicitly documents this.
- **The seq subquery** in SQLite is protected by parking_lot::Mutex, so concurrent save_message calls are serialized at the database level.

**However**: The AgentQueue in the gateway serializes runs per-agent (one at a time), so two runs on the same session will not actually execute concurrently. This is the primary protection against interleaving.

**Conclusion**: No race conditions exist in the current architecture.

---

## 2. Context Building -- What the LLM Actually Sees

### 2.1 Full Trace: build_context() to LLM Call

The flow is:

1. **run() calls build_context()** (agent.rs:433-435) with the current input *before* persisting the user message.
2. **build_context()** (agent.rs:677-762):
   - Calls assemble_system_prompt() which reads workspace files fresh via build_system_prompt_prefix() and prepends to base prompt.
   - For DM sessions, appends dm_recipient.md addendum.
   - Calls session_manager.get_history(session_id) -- returns in-memory Vec of Message (loaded from SQLite on startup).
   - For sliding-summary, calls maybe_summarize() which may compress old messages via an LLM call.
   - Calls ContextBuilder::build_with_perspective() to produce Vec of LlmMessage.
3. **run() then persists the user message** (agent.rs:437-444) -- intentional to avoid double-counting the current input in context.
4. **finish_run() calls agent_loop()** (agent.rs:875+): uses the built context, loops calling the LLM.

### 2.2 Context Window Assembly Order

The ContextBuilder::build_with_perspective() produces this array:

    [system_prompt]                           <- workspace prefix + base prompt (+ DM addendum)
    [summary_block]?                          <- only for sliding-summary, if summary exists
    [history messages within budget/window]   <- from session history
    [current_input]                           <- the new user message (skipped if empty)

### 2.3 Strategy Behavior

| Strategy | What Gets Included | What Gets Dropped |
|---|---|---|
| full | All history oldest to newest until budget exhausted | Messages beyond token budget (with warning) |
| truncate (default) | Most recent recent_window messages within token budget | Older messages silently dropped |
| sliding-summary | Summary block + most recent recent_window messages | Old messages compressed into summary |

### 2.4 Tool Call Reconstruction

The session_msg_to_llm() method (context.rs:256-302) correctly reconstructs structured messages:

- Content::ToolCall becomes LlmMessage with role assistant, content None, and tool_calls array with tool_call_id from metadata.
- Content::ToolResult becomes LlmMessage::tool_result(tool_id, content) with truncation at 2000 chars.
- group_tool_calls() (context.rs:310-339) merges consecutive assistant tool-call-only messages into a single message with multiple tool_calls. Correctly handles persisted format (one msg per tool call) to LLM format (one msg with N tool calls).

**Conclusion**: History is correctly loaded, reconstructed, and included in the context window. Tool calls survive across runs.

---

## 3. Workspace Files in System Prompt

### 3.1 Are Files Read Fresh Each Run?

**Yes.** build_system_prompt_prefix() (workspace.rs:160-194) calls read_file() for each workspace file (personality, goals, user, memories). read_file() (workspace.rs:91-101) calls std::fs::read_to_string() on each invocation -- there is no caching.

### 3.2 Does workspace_write Update Take Effect Next Run?

**Yes.** Since read_file() reads from the filesystem on every call, if an agent calls workspace_write during run N, run N+1 will see the updated file. Even within the same run, workspace changes take effect at the next assemble_system_prompt() call (which happens in the tool_loop when the system prompt is refreshed at agent.rs:1150-1157).

### 3.3 Prompt Assembly Order

Normal run (non-DM):

    {personality.md}
    ## Current Goals
    {goals.md}
    ## About the User
    {user.md}
    ## Memories
    {memories.md -- truncated at 4000 chars}

    {initial.md content}

DM run adds dm_recipient.md with {peer} replaced by peer agent name.

Tool loop iterations (after first tool result) replace the system message with:

    {workspace prefix (re-read fresh from disk)}
    {initial.md content}

    {tool_loop.md content}

**Conclusion**: Workspace files are always fresh. No caching bug exists.

---

## 4. Multi-Turn Continuity

### 4.1 Session Reuse Across Turns

When a user sends a second message in the same session:

1. Web UI sends POST /runs with session_id set to the same session.
2. create_run() (runs.rs:264) looks up the session by ID, extracts context_id.
3. execute_run() calls runtime.run(&session_manager, &context_id, input).
4. run() calls session_manager.get_or_create(agent_id, context_id) -- finds the existing session because the (agent_id, context_id) key already exists in the DashMap.
5. get_history() returns all previously persisted messages.

**This works correctly.** The second run sees the full history from the first run.

### 4.2 Context ID Stability

**Finding: The context_id is stable for a given session.** The web UI uses web-chat-{timestamp} when creating a *new* session (session-list.js:49, use-boot.js:62). This timestamp is part of session creation -- it does NOT change per message. Subsequent runs reference the session by session_id, and the context_id is extracted from the existing session object at runs.rs:292.

For other channels:
- Telegram: telegram_{agent_name}_{chat_id} -- stable per chat.
- Jobs: job_{job_id} -- stable per job.
- Subagents: deterministic UUID v5 -- stable per parent/child pair.

**Conclusion**: Context IDs are stable. No multi-turn continuity bugs.

### 4.3 Note: Context Build Happens Before User Message Persistence

In run() (agent.rs:431-444), build_context is called before append_message for the user input. This is intentional (comment on line 431-432: avoids double-counting the current input in context). If build_context fails, the user message is still persisted, and then finish_run persists an error marker. The session shows [user input] [Run failed: ...] which is correct -- the user message should survive in history even if the run failed.

---

## 5. Edge Cases

### 5.1 Run Errors -- Partial Message Persistence

When a run errors (finish_run, line 623-651):
- An error marker [Run failed: reason] is persisted to the session.
- Error messages are sanitized via sanitize_error_for_session() to avoid leaking API keys/URLs.
- Tool call records collected up to the point of failure are returned in AlmsError::FailedWithToolCalls and persisted by the gateway at runs.rs:715.

**Conclusion**: Partial state is correctly persisted on errors.

### 5.2 Run Cancellation -- Partial Message Persistence

When a run is cancelled (finish_run, line 599-621):
- A cancellation marker [Run cancelled by user] is persisted.
- Tool call records are returned in AlmsError::CancelledWithToolCalls and persisted by the gateway at runs.rs:696.
- Cancellation can happen at three checkpoints (A: between iterations, B: during LLM call, C: during tool execution).

**Conclusion**: Cancellation is handled correctly with partial persistence.

### 5.3 Failed Tool Calls

When a tool returns an error:
- The error string is stored as the tool result content (agent.rs:1099).
- The ok metadata field is set to false (agent.rs:1116).
- The message is persisted to the session normally.
- The LLM sees the error in context on the next iteration.

**Conclusion**: Failed tools are correctly persisted and visible to the LLM.

### 5.4 DM Sessions -- Tool Call Exclusion

For DM sessions (context_id starting with dm:):
- Tool calls and results are NOT persisted to the session (if !is_dm guards at lines 990, 1104).
- Tool calls ARE recorded in tool_call_records for per-run storage (run_tool_calls table).
- The final text response is NOT persisted to the DM session (line 572).
- Error/cancellation markers ARE persisted with from_agent metadata for perspective mapping.
- This is by design: shared DM sessions should only contain explicit send_message messages and system markers.

**Conclusion**: DM session behavior is correct and intentional.

### 5.5 Max Iterations

When the agent loop hits max_iterations:
- Returns [Max iterations reached] as the response text (line 909).
- This is the normal response path, so it gets persisted as an assistant message by finish_run.

**Conclusion**: Correct behavior.

---

## 6. Summary of Findings

### Confirmed Working

1. All message types (user, assistant, tool calls, tool results, error/cancel markers) are persisted correctly for non-DM sessions.
2. Message ordering is stable via seq column, not timestamps.
3. Session reuse across multi-turn conversations works -- context_id is stable.
4. Workspace files are read fresh on every run and every tool-loop iteration.
5. Tool calls are reconstructed from persisted format to structured LLM messages, including parallel tool call grouping.
6. Sliding-summary compression works with graceful fallback on failure.
7. DM sessions correctly exclude tool calls from session storage while retaining them in per-run storage.
8. Partial state (tool calls, error/cancel markers) is persisted on errors and cancellations.
9. Context window token budgeting respects the configured limits across all strategies.

### No Bugs Found

The session persistence and context building systems are well-implemented. The write-through pattern (in-memory DashMap + SQLite) ensures both fast reads and durable storage. Lock ordering is documented and consistent. The separation between session-level persistence (for context building) and per-run persistence (for debugging) is clean.

### Recommendations

1. **Test coverage for multi-run context building**: There are no integration tests that verify a second run sees the first run messages in its context window. This is the most important gap -- the code looks correct on inspection, but a regression here would be invisible without a test.

2. **max_messages config is not enforced**: SessionConfig.max_messages (default 10,000) is defined but never checked during append_message(). If a session accumulates more than 10,000 messages, they all stay in memory. This is not a persistence bug but could become a memory issue for long-running sessions.

3. **Token estimation accuracy**: The estimate_tokens() function uses chars / 3 which overestimates for English text and may underestimate for CJK/emoji-heavy content. This is documented and acceptable for MVP, but a proper tokenizer would improve context utilization.
