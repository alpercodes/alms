# DM Run Lifecycle with `send_message` and Parallel Tool Calls

**Status**: research artifact — frozen as of 2026-04-25 against `release/0.2.2`. The source code in `crates/` is always authoritative; if this doc and the code disagree, fix the doc or open an issue.

> **Superseded in part by #1154 (implicit DM replies)**: agents no longer call `send_message` to reply to their DM peer — the run's final assistant text IS the reply, delivered by the gateway's DM completion gate (`crates/alms-gateway/src/runs/dm_lifecycle.rs`) after the run completes. `should_terminate_after_dm_send`, the text-only retry, and the `send_message`-reply round-trip described below no longer exist. `send_message` survives only for contacting a *different* agent; `ignore_message` still ends the conversation. The queueing / parallelism / depth-counter analysis below still applies to the delivery path (the gate reuses `MessageBus::send`).

**Originally filed as**: #740 (closed once landed here).

**See also**: `docs/layer2-peer-messaging-design.md` (the design doc for peer messaging), `docs/api.md` (`dm_*` SSE event payloads).

---

## Executive summary

- `send_message` is fire-and-forget. It writes the message to the shared DM session, pushes a `RunTrigger` onto an mpsc channel, and returns a `{delivered, dm_session_id}` JSON result immediately. The sender's loop keeps going. See `crates/alms-tools/src/send_message.rs:82-147` and `crates/alms-coordinator/src/message_bus/bus.rs:106-311`.
- **B gets a brand-new run.** `run_trigger_loop` consumes the trigger and calls `enqueue_triggered_run`, which creates a fresh `Run`, registers a cancel token, and enqueues at LOW priority on `agent_queue` keyed by B's `AgentId`. See `crates/alms-gateway/src/runs/notifications.rs:516-588` and `notifications.rs:787-1010`.
- **The agent queue serializes per-agent, not per-session.** `SessionQueue<AgentId>` means B will never have two runs executing at the same time. Normal-priority (user) runs always drain before low-priority (peer / notification) runs. See `crates/alms-gateway/src/server/state.rs:39` and `crates/alms-gateway/src/session_queue.rs:118-228`.
- **Parallel tool calls**: `send_message` fires concurrently with `fs_read` / `shell`. `join_all` awaits the whole batch; B may enqueue, start, and even complete a reply before A's local tools finish. But B's reply-run cannot overtake A: it is queued behind A on A's agent queue and only runs after A's current run ends.
- Depth counter and source-session tracking live in `DashMap`s on `MessageBus`. Depth increments on sender change per DM-pair; resets after 1800s of quiet or on `end_conversation`.

---

## Q1: lifecycle of a DM-triggered run

### Step 1 — A calls `send_message` during its run

In A's agent loop, inside `execute_tool_call`, the `SendMessageTool` runs (`crates/alms-tools/src/send_message.rs:82-147`):

1. Validates `to` and `message` args.
2. Resolves recipient via `store.load_agent_by_name(to)`. If missing, returns a JSON error in the tool result (no `SandboxError`).
3. Calls `MessageSender::send(sender_name, sender_agent_id, recipient_name, recipient_id, message, Some(sender_session_id))`.
4. Returns `{delivered: true, dm_session_id: ...}` to the LLM.

Inside `MessageBus::send` (`crates/alms-coordinator/src/message_bus/bus.rs:106-311`):

1. Self-message guard (`SendError::SelfMessage`).
2. Depth check: looks up `depths` key for the DM pair. If `last_sender` differs from current sender, increments. If `> MAX_DM_DEPTH (20)`, calls `end_conversation` and returns `DepthExceeded`.
3. Depth-expiry sweep: `DEPTH_EXPIRY_SECS = 1800`. Stale entries in `depths`, `last_activity`, `source_sessions` are reaped opportunistically on every send.
4. Source-session tracking (`bus.rs:209-234`): records the sender's current `SessionId` as their source session for this DM pair, unless the session is internal (`notification|subagent|episodic|job`) or is the DM session itself. `or_insert` preserves the first valid entry.
5. Computes the deterministic DM session ID: `SessionId::deterministic_dm(sender_name, recipient_name)`. Both agents produce the same UUID regardless of who calls first.
6. `session_manager.get_or_create_shared(session_id, dm_context)` — creates the shared DM session lazily.
7. Appends the message as `Role::User` with metadata `{from_agent, from_agent_id, message_type: dm}`.
8. Sends a `DmEvent` on `dm_event_tx` — picked up by `dm_event_loop` (`notifications.rs:1022-1051`) which fans it out as a `dm_message` SSE event to anyone watching the DM session.
9. Updates `last_activity` for depth expiry.
10. Pushes a `RunTrigger` for the recipient on `run_trigger_tx`.
11. Returns `Ok(DeliveryReceipt)`.

A's loop keeps running — the tool result (small JSON) goes into A's message history and the LLM is called again (or A's loop terminates early if the DM-sender-terminate guard in `crates/alms-runtime/src/agent/dm.rs:53-63` fires for a DM-triggered run).

### Step 2 — `run_trigger_loop` turns the trigger into a run on B

`run_trigger_loop` (`notifications.rs:787-1010`) dequeues the trigger:

- Source label: `peer:alice`, `is_peer = true`, `input` = original message.
- Calls `enqueue_triggered_run(state, agent_id: bob, session_id: dm_session, input, context_id: dm:alice:bob, peer:alice, is_peer=true)`.
- After enqueue, calls `notify_dm_started_to_webchat(state, bob, alice, dm:alice:bob)` — sends a `dm_activity_started` SSE to Bob's most recent user-facing session (found via `find_user_facing_session`). This is how the UI status bar shows "Chatting with Alice".

`enqueue_triggered_run` (`notifications.rs:516-588`):

1. `Run::new(dm_session_id, bob_agent_id, input)`; `insert_run` persists to SQLite.
2. Computes `queued_behind` (`agent_queue.pending_count(bob) + (agent_has_running_run(bob) ? 1 : 0)`).
3. Emits `run_created` SSE event on the DM session with `system_triggered: true, source: peer:alice, queued_behind`.
4. Registers cancel token.
5. `agent_queue.enqueue_low(bob, ...)` — LOW priority. Peer runs yield to any user-initiated run on Bob.
6. The enqueued future calls `execute_run` with `RunParams { is_peer_message: true, is_system_triggered: true, input_pre_persisted: false, ... }`.

### Step 3 — `execute_run` on Bob

`execute_run` (`crates/alms-gateway/src/runs/lifecycle.rs:492-1000`) for `is_peer_message=true`:

- Emits `run_started` SSE.
- `mark_run_as_running`.
- Resolves Bob's config and applies overrides.
- `resolve_posture_for_run` forces posture to `Autonomous` because `is_system_triggered=true` (Guarded would hang — no human to approve).
- Dispatches to `runtime.run_on_session(&session_manager, dm_session_id, dm:alice:bob, &input)` (`lifecycle.rs:930-937`) — uses the already-written DM message as the last user turn instead of double-writing.
- Bob's agent loop runs. Because `context_id` starts with `dm:`, `is_dm=true` in the loop: Bob's assistant text and tool calls are persisted as `Role::User` with `message_type: reasoning` metadata to preserve the DM invariant (see `loop_impl.rs:509-579`, `dm.rs:159-181`).
- If Bob's final assistant output calls `send_message` back to Alice, the DM-sender-terminate guard (`dm.rs:53-63`) fires: `should_terminate_after_dm_send` returns `true`, and Bob's loop exits after the batch completes — preventing a second round-trip inside the same run.
- If Bob calls `ignore_message`, the post-run `handle_dm_run_completion` in `dm_lifecycle.rs:87-200` detects it via `ToolCallRecord` inspection and calls `MessageBus::end_conversation`. That writes a `dm_ended` marker, resets depth, and emits `ConversationEnded` triggers to both Bob and Alice.

### Step 4 — Bob replies via `send_message` back to Alice (the flip side)

Bob's `SendMessageTool` runs again — same path as Step 1, but now:

- The depth entry for this DM pair goes from 1 to 2 (sender changed).
- A new `RunTrigger` is pushed for Alice on the same DM session.
- `run_trigger_loop` enqueues a low-priority run on Alice's `agent_queue`.

**Crucial point about A's concurrent work**: Alice is still inside her original run, possibly still awaiting her `fs_read` / `shell`. Alice's loop does NOT observe Bob's reply directly — the reply-run is a separate `Run` object queued behind Alice's current run. Alice's current run's tool result for `send_message` was already returned in Step 1 (delivery receipt only). Alice's run continues with the remaining parallel tool results, finishes the LLM turn, and eventually terminates. Only after it terminates does the low-priority queue handler pick up Bob's reply-run and begin processing.

This is the central design guarantee: **an agent only runs one run at a time. Incoming DMs queue.**

### What B's UI sees (SSE events)

On the DM session stream (both agents can watch):

- `dm_message` (text Alice sent)
- `run_created` (`queued_behind: N`, `source: peer:alice`)
- `run_started`
- `token_delta` events for Bob's LLM stream
- `tool_start` / `tool_end` events for Bob's tool calls
- `dm_message` (when Bob calls `send_message` back — on Step 4)
- `run_finished`
- Eventually, if ended: `dm_conversation_ended`

On Bob's user-facing web-chat stream:

- `dm_activity_started` (`peer=alice`) — emitted right after enqueue
- `dm_activity_status` events during Bob's run (`executing_tools` / `calling_llm`) — via `forward_runtime_events` in `runs/tools.rs`
- `dm_activity_ended` when the run completes

---

## Q2: parallel tool calls with `send_message`

Agent A emits a batch of three tool calls: `send_message`, `fs_read`, `shell`. With Autonomous posture, `run_tool_calls` in `loop_impl.rs:427-491` runs the batch via `join_all`.

### Timing

- All three futures start concurrently.
- `send_message.execute` performs a DB lookup plus `MessageBus::send`: SQLite
  persistence, non-blocking reservation of bounded run-trigger capacity, and
  a best-effort bounded DM SSE event. Trigger saturation returns an explicit
  error before DM state changes; SSE-event saturation drops only the live
  decoration. A successful call returns after persistence and trigger commit,
  without waiting for the recipient run to start.
- Side effects on B happen synchronously inside `send_message.execute`: by the time `send_message` resolves, the DM message is already in the DB and the `RunTrigger` is already on the channel. There is no waiting for B to accept anything.

### Does B's run overtake A's still-running batch?

The enqueue on B's agent_queue happens essentially immediately after `send_message.execute` returns (the `run_trigger_loop` is always awaiting `rx.recv()`). B's run starts executing in parallel with A's still-running `fs_read` and `shell` — they are different agents with different queues. So yes: **B can finish its entire LLM turn, send a reply, and enqueue a follow-up run for A while A is still inside `join_all` waiting on `shell`.**

But A's reply-run does NOT interfere with A's current run. A's reply-run sits in A's low-priority queue and only runs after A's current run finishes (because `SessionQueue<AgentId>` is per-agent-serial). So:

1. A's current run completes the batch and pushes three `tool_results` into its context.
2. A's loop calls the LLM again with those results; completes naturally.
3. A's `agent_queue` handler then dequeues B's reply-run.
4. A receives B's reply in a fresh run on the same DM session — with whatever DM context and transcript the `ContextBuilder` reconstructs on reload.

### DM conflict guard

If the batch contains both `send_message` and `ignore_message`, `detect_dm_conflict` (`crates/alms-runtime/src/agent/dm.rs:30-46`) returns `conflict=true`. Both tools get blocked with the `DM_CONFLICT_MSG` error result; other tools in the batch still execute. This is the only runtime constraint on what can be parallel with `send_message`.

### Does B see one run per message or a batched run?

**One run per message.** Each `SendMessageTool::execute` pushes one `RunTrigger`. The `run_trigger_loop` creates one `Run` per trigger. If A emits two `send_message` calls in the same batch targeting the same recipient, B gets two runs, processed serially on B's queue. They are ordered by channel enqueue order (FIFO mpsc).

### Ordering considerations

- Inside a single batch: `join_all` does not guarantee completion order, but since `SendMessageTool` only writes (it does not read DM state), concurrent sends in the same batch are fine for the DM session message ordering. They will be appended in whatever order `session_manager.append_message` serializes them — the SQLite session manager serializes writes.
- Depth counter under concurrent sends to the same pair: `DashMap::entry` is atomic per-key, so depth increments are safe. But if both agents call `send_message` to each other within microseconds — e.g. both trying to send in parallel from overlapping runs — depth may count them in arbitrary order. In the worst case you end up with sender-changed increments that over-count. Not a correctness bug, but can tighten `MAX_DM_DEPTH` in aggressive back-and-forth scenarios.
- Fire-and-forget from A's perspective: A has no way to know from the tool result whether B actually processed the message. A only knows the message was persisted and a trigger was emitted. Any acknowledgement flows back as a new DM message.

---

## Known hazards and open questions

1. **Bounded trigger saturation is explicit.** The run-trigger channel is
   capped at 1,024 entries. `MessageBus` reserves the exact trigger
   cardinality before mutating DM state, so saturation returns a retryable
   internal tool error without persisting a message, consuming depth, or
   writing an end marker. The DM-event channel is also bounded, but its SSE
   decoration is best-effort and drops on saturation.
2. **DM-pair transactions serialize send/end/expiry.** A per-pair async mutex
   covers depth/source state, marker persistence, and trigger commit. Concurrent
   ends therefore produce one marker/trigger transaction; the later caller
   observes the consumed state and returns `Ok`. Idle mutex entries are
   identity-pruned after the last user releases them, so retaining this safety
   boundary does not create an ever-growing pair map.
3. **`queued_behind` undercount TOCTOU** (`run_manager.rs:363-376`, also cited in `lifecycle.rs:383-392`). Narrow window between `pending.fetch_sub` and `mark_run_as_running` where both read `false`. A new `create_run` during that window reports `0 queued` — UI shows "Thinking" instead of "Queued". Documented, bounded by executor dispatch latency.
4. **Notification-run ordering when A and B send to each other simultaneously.** Alice-sends-during-Bob-run and Bob-sends-during-Alice-run can interleave such that Alice processes Bob's reply before Alice's current run even finishes emitting its own `send_message` tool result. No correctness issue (runs on A are serialized), but the DM session log reads chronologically out-of-intent. Tracked separately as #843.
5. **Recipient missing vs. error**: `SendMessageTool::execute` returns a `Value` error object (not `Err`) when the recipient is not found. This lets the LLM recover gracefully. But `MessageBus::send` returns `SendError::SelfMessage` / `DepthExceeded` as real errors, which are mapped to JSON objects in the tool too. Internal errors become `SandboxError::Io` — a real tool failure. This asymmetry is intentional but worth calling out for anyone adding new error variants.

   **Addendum (#920 / PR #995)**: the `SandboxError::Io(format!(...))` shape above is still accurate for `SendMessageTool` — that tool constructs `Io` explicitly. However, the broader `From<AlmsError> for SandboxError` impl has changed: it now produces `SandboxError::Subagent(Box<AlmsError>)` instead of stringifying into `SandboxError::Internal`, so the typed `AlmsError` (notably `SubagentLlmError`) survives the sandbox boundary verbatim. `ToolRegistry::execute`'s catch-all unwraps `Subagent` back to the inner `AlmsError`, and the coordinator carries the typed value through a parallel `error_tx`/`error_rx` oneshot alongside the JSON `TaskResult`. Net effect: a subagent's LLM 400 reaches the parent agent's `tool_result` as one tractable line (`Subagent LLM error (anthropic 400): {body}`) instead of the legacy 4-prefix wrap. New tools that need to surface a typed `AlmsError` across the sandbox boundary should rely on the `From` impl rather than reaching for `SandboxError::Io` or `Internal`.
6. **DM-end self-notification** (#556, made symmetric in #1215; scoped by #1258 — see hazard 8): when a DM ends (depth or ignore), the **ender** gets a notification run too — not just the peer — whenever EITHER agent has a source session. It routes to the ender's own source session when it has one (#556 initiator-ends / #1198 D3 job-ends), otherwise to `notifications:{ender}` (the #1215 receiver-ends case: a source-less recipient that ends a DM whose peer has a source session). When NEITHER agent has a source session the ender gets no self-notification — the both-source-less gate keeps the "exactly one trigger per end" atomicity intact (hazard #2). If the sender is currently running (the usual case), that notification queues behind the current run at low priority. If the sender's current run already sent and moved on, the notification lands naturally. If the sender's source session no longer exists (deleted while DM was in-flight), `session_manager.get` fails and `context_id` falls back to `notifications:{sender_name}` — the notification shows in an invisible session. The gateway forwards the `dm_conversation_ended` phase-clear SSE to the ender's web-chat regardless; it persists the reloadable `dm_ended_notification` banner marker (and lets the frontend render the live banner) UNLESS the notification run lands on that same web-chat session — i.e. it suppresses both marker and live banner (via the SSE `suppress_banner` flag) only when the run is already the visible notification there. This covers a user-facing initiator/receiver whose run routes to their own web-chat, and correctly still persists for a job-source run, a different user-facing chat, or a #1205 episode-rerouted run (#1218 P2). See Scenario B.
7. **Ephemeral subagent + DM**: `with_workspace` is only set for named registered agents. An ephemeral subagent does not own a registry record, so it cannot receive `send_message` (recipient lookup fails at `send_message.rs:107`). This is enforced by the name-based lookup. Worth a doc note in `layer2-peer-messaging-design.md`.
8. **An INTERRUPTED end gets no notification run at all** (#1258 — authoritative description; `layer2-peer-messaging-design.md` cross-references this entry). Hazard 6 and Scenario B below describe the notification runs for a DM that *ended*. A DM that was **cut short** no longer gets one on the trigger's own target: `run_trigger_loop` skips `enqueue_triggered_run` when `ConversationEndReason::is_interrupted()`. The persisted `dm_ended_notification` marker plus its live banner (carrying a `detail` line with the failure text) are the **operator's** delivery; since #1300 the **agent's** is a `persist_error_marker` record on the session the run would have used — see the last paragraph of this hazard.

   Interrupted means `user_cancelled`, or `errored` where the run **died** mid-turn (LLM/tool failure, panic, setup failure, teardown persistence failure). It deliberately does **not** include `errored` from a run that *completed* with an unusable result — `dm_lifecycle`'s Exit 3 ("no deliverable reply", #1154) and its delivery-failure sibling. Those may follow several delivered turns whose only copy lives in the `dm:` session, and the notification run's #429 transcript is what carries them to the operator's chat; suppressing them would trade a spurious spinner for a silently dropped answer. The predicate is therefore "was a turn cut short", **not** "is the transcript empty" — a DM cannot end at all without at least the initiating message in its session, so the transcript is never empty.

   Two carve-outs: #1198 job-episode continuations still fire (they resume the *job*, not the DM — dropping one stalls the episode until its deadline), and the bus's `dm_ended` marker, depth reset and tombstone are unaffected.

   The agent is told too, without being woken (#1300). Until #1300 it was not: the `dm_ended_notification` marker is synthetic and stripped before the provider, and the bus's `dm_ended` row is empty-text so `dm_filter::is_synthetic_marker` hides it from `read_messages` / `read_session` — so an interrupted end left no signal the agent could observe by any means. `plan_triggered_runs` now returns an `InterruptedEndRecord` in place of the suppressed run, and `run_trigger_loop` persists it with `persist_error_marker` (#874) onto **the same session the run would have used** — not the operator's chat, which already has its own display-only copy. `kind: "error"` is the one marker shape `session_msg_to_llm` rewrites into a surviving `[Error] …` user message, so the record reaches the model on that session's next turn. Its text names the peer, states the cause and that no reply is coming, and points at `read_messages`; the transcript is deliberately not inlined, since unlike a run input this text is re-injected on every later turn. No run is created, so #1258 is untouched. The record fires only on the arm that produces no run — a job-episode continuation is handed `format_dm_ended_notification`'s prose, which already states the interruption.

---

## Reproduction notes

### Scenario A — parallel batch with `send_message`

Prompt Alice such that she emits:

```json
[
  { "tool": "send_message", "to": "bob", "content": "ping" },
  { "tool": "shell", "command": "sleep 3; echo done" }
]
```

Expected UI / SSE:

- On Alice's session: `tool_start` events for both tools at nearly the same timestamp.
- On Alice's web-chat: `dm_activity_started` event (`peer=bob`) within milliseconds of the shell starting.
- On the Alice-Bob DM session: a `dm_message` event within milliseconds.
- Bob's `run_created` (low priority, on DM session) fires right after.
- Bob's LLM stream can finish, emit a reply `dm_message`, and enqueue an Alice-reply-run — all while Alice's shell is still sleeping.
- After Alice's shell finishes and Alice's run completes, Alice's low-priority queue dequeues the reply-run; Alice's UI emits another `run_created` / `run_started` cycle on the DM session.

### Scenario B — depth exceeded

Have Alice and Bob ping-pong via `send_message` in a tight loop (20+ bounces). On bounce 21:

- `MessageBus::send` detects `depth > MAX_DM_DEPTH`, calls `end_conversation`, returns `DepthExceeded`.
- Tool result on Alice's side: `{error: "Message depth exceeded maximum..."}`.
- `end_conversation` writes a `dm_ended` marker to the DM session, emits two `RunTrigger`s with `ConversationEnded`: one for Alice, one for Bob (if sources recorded).
- `run_trigger_loop` emits the `dm_conversation_ended` SSE on the DM session with `reason: depth_exceeded`, forwards the phase-clear `dm_conversation_ended` SSE to each agent's web-chat via `notify_dm_ended_to_webchat`, and enqueues two notification runs. That forward is deferred until the final run routing is known: it persists the `dm_ended_notification` banner marker (and lets the frontend render the live banner) **unless the notification run lands on that same web-chat session** — i.e. it suppresses both only when the run is already the visible notification there (the `suppress_banner` SSE flag also gates the live banner). So it persists for a source-less agent (run on the invisible `notifications:` session), a job-source run (run on the internal `job_*` session), a different user-facing chat than the marker target, or a #1205 episode-rerouted run; and it suppresses only for the run's own user-facing web-chat (#1215 / #1218 — avoids the "initiator gets both" duplicate, both reloadable and live).
- Both agents receive a formatted "DM ended" prompt with the transcript inline (so they do not need to call `read_messages`).

### SQLite inspection tips

- `sessions` table: DM session has `context_id` like `dm:alice:bob`.
- `messages` table: all DM messages are `role=user`, `metadata.from_agent` discriminates author.
- `runs` table: each DM round-trip creates one row per agent per message.
- `run_tool_calls` table: tool calls executed in DM runs are stored here (DM sessions excluded from session-level tool_call persistence).
