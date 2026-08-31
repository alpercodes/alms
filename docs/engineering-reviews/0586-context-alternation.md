# Design analysis with options weighed and a recommendation

> Reconstructed from the review history of the ALMS project's original issue tracker.
> Cross-references like `#1246` point at that tracker and are kept verbatim for provenance.

**Source:** Issue #586 -- Context builder should ensure valid message alternation for all providers  
**Rounds:** 1  


Four candidate approaches to enforcing valid message alternation across providers, each with its failure modes, ending in a concrete recommendation, an implementation sketch, and an explicit *what I would push back on* section.

---


*Posted 2026-04-16*


## Design analysis by Tim (automated)

**Verdict on approach: C (Hybrid) — enforce a clean canonical invariant in the context builder, keep provider adapters responsible only for their own quirks.**

---

### 1. Current state audit

Tracing from AgentRuntime build_context through ContextBuilder build_with_perspective out to each provider adapter, here is what can end up on the wire:

**Order of emission in build_with_perspective:**
1. System prompt (always)
2. Optional episodic summaries (as system messages)
3. Optional sliding-summary block (as system message)
4. Selected history from truncate / full / sliding-summary strategy
5. group_tool_calls post-processing merges consecutive assistant-tool-call messages
6. Current input, only appended when non-empty

**Known ways the array can end with NOT a user message:**

- **Notification runs (#584):** run_on_session is called with empty input, so step 6 is skipped. A pre-persisted notification_input message is relied on as the trailing turn — fragile. If nothing was pre-persisted, step 4 truncate walks over history that may already end with an assistant turn from a prior conversation.
- **Subagent / job / DM contexts with empty input:** Same pattern. Any place the agent loop re-enters without fresh input can produce an assistant-terminated array. loop_impl.rs lines 102-109 always push an assistant message after each iteration. Fine on iteration 2 (tool-result follows), but NOT fine if the previous run terminated on a text-only assistant message and the next run has empty input.
- **Synthetic Role::System markers:** persist_lifecycle_marker writes Role::System + synthetic metadata for DM-ended and job notifications. These pass through session_msg_to_llm as LlmMessage::system. When they are the last session message and input is empty, the array ends with a system message.
- **Summarization artifacts:** Not currently a live bug, but if recent_window is 0 or history fits zero messages after budget clamp, the array could land on a trailing system (summary block + empty input).
- **Sliding-summary degenerate:** When history budget is exhausted and only the summary block fits, with empty input the array ends in system.
- **Consecutive same-role messages:** The builder has group_tool_calls but no general same-role merge. Two consecutive Role::User messages (DM reasoning + notification) pass through as two separate user messages. OpenAI tolerates this; Anthropic rejects it — currently mitigated only inside the Anthropic adapter.

**Per-provider handling after build:**

- **OpenAI/OpenRouter (llm_client.rs lines 72-81):** Serializes CompletionRequest directly as JSON. No normalization. Protocol accepts consecutive same-role and trailing system messages, but many models respond poorly. OpenRouter-to-Claude proxying performs the SAME system extraction as direct Anthropic, so it inherits the Anthropic failure mode silently — the client thinks it is talking to OpenAI.
- **Anthropic (anthropic.rs lines 136-248):** Extracts all system-role messages into top-level system field, so any system message interspersed in history is pulled out. Calls merge_consecutive_roles to fix same-role runs. Does NOT ensure the messages array is non-empty or that it ends with user. If all messages were system (edge case), an empty array is sent — Anthropic rejects it.

**Bottom line:** The current architecture has two defenses — merge_consecutive_roles inside the Anthropic adapter, and the notification_input workaround in lifecycle.rs. Neither covers all failure modes above, and the workaround leaks internal session-persistence concerns into the notification lifecycle layer.

---

### 2. Options analysis

**A. Provider-side post-processing.** Each adapter sanitizes right before the wire call.
- Pro: Localizes provider quirks; does not couple ContextBuilder to provider knowledge.
- Con: Two copies of the invariant (Anthropic partial, OpenAI absent). OpenAI does not strictly require trailing-user so we would end up adding it defensively there too — which is really a cross-provider concern that belongs upstream. Does not help OpenRouter-to-Claude proxying.

**B. Context-builder-side normalization (everywhere).** Builder emits a canonical shape and providers trust it.
- Pro: Single source of truth, testable in isolation.
- Con: System extraction for Anthropic is genuinely provider-specific. Forcing all system messages inline would lose Anthropic native system-field semantics (prompt caching, separate token accounting).

**C. Hybrid.** Builder guarantees a provider-agnostic canonical shape; adapters do only the adjustments that are actually provider-specific.
- Pro: Matches the existing ALMS layering — ContextBuilder already produces Vec<LlmMessage> consumed by two adapters. The invariants most failure modes need (trailing user turn, non-empty array, same-role merging) are NOT provider-specific — they are LLM-API-family-general. System-field extraction IS provider-specific and stays in to_anthropic_request.
- Con: Requires being clear about which invariants live where. Solved via docs + tests.

**D. Role redesign.** Split Role::System into inline vs. out-of-band buckets.
- Pro: Makes the extraction behavior explicit at the type level.
- Con: Large blast radius (session storage schema, tool filters, UI filters, every adapter). The real problem is not that Role::System is ambiguous — it is that nothing enforces what a valid send-to-LLM message list looks like. Role redesign is a wrench where you need a ratchet.

**Pick: C.** The ALMS layering already supports it, and the evidence from #584 is that the current split of responsibility is unclear — fixing that clarity is more valuable than rearranging types.

---

### 3. Lightweight external context

OpenClaw source is not accessible to me here. Pattern that has become standard across other frameworks (LangChain message combining, llama.cpp chat-template handling, the official Anthropic SDK implicit behavior): keep a canonical [system?, alternating user/assistant, user] shape and let the provider layer do the narrow translation (Anthropic system extraction, OpenAI tool-message format). Nothing exotic about the hybrid approach for ALMS.

---

### 4. Concrete recommendation

**Invariants ContextBuilder must guarantee on its output Vec<LlmMessage>:**

1. **Non-empty.** At least one message.
2. **System messages only at the front.** Any system-role message appears before the first non-system message. Synthetic markers from mid-history are moved or dropped — see migration below.
3. **Alternating user/assistant after the system prefix.** Consecutive same-role messages are merged (content concatenated, tool_calls appended). Tool-call-only assistant messages and tool-result messages are treated as part of the alternation per existing tool-grouping rules — specifically: assistant with tool_calls then tool (one per call) counts as ONE logical turn pair, not a role violation.
4. **Last non-system message is user.** If the tail is assistant, a tool-call group, or an orphan tool-result: either (a) the current input provides the closing user turn, or (b) a synthesized placeholder user message is appended.

**Placeholder user message policy:**

- Prefer the existing notification text (from notification_input metadata) as the placeholder when present.
- Otherwise append a short, context-aware stub such as "Please respond to the notification above." or "Please continue."
- Log at warn! when a placeholder is synthesized — that is a signal something upstream should have supplied input and did not.

**What each provider adapter should do (and ONLY this):**

- **OpenAI/OpenRouter:** Trust the canonical shape. Serialize directly. No post-processing.
- **Anthropic:** Extract the leading run of system messages into the top-level system field (join with double newline). Because the builder already guarantees systems are only at the front and the tail is user, extraction cannot leave the array ending in assistant. merge_consecutive_roles can be removed, or kept as a debug_assert! defensive check.

**Migration story for the #584 notification_input hack:**

The notification_input: true metadata flag stays — it is doing real work as a UI filter and a persistence marker. What changes:

- The REASON it is Role::User becomes a consequence of the invariant, not a workaround. The comment block in lifecycle.rs lines 834-867 can shrink to: "Persist notification as Role::User so it is included in the context as a user turn; context builder guarantees the trailing-user invariant independently."
- Can we revert the Role::User choice? **No — keep it.** Two reasons: (1) storing as Role::System would force the context builder to promote-system-to-user in the synthesis path, which is ugly cross-role coercion; (2) the API filter in routes.rs lines 404-411 already hides notification_input from the UI, so there is no downside. Leave it.

**Edge cases to cover in tests:**

- Empty session + empty input -> builder emits [system, placeholder_user], NOT [system] alone.
- **Tool-call-only tail:** Session ends with assistant tool_call, no tool_result yet, empty input -> should NOT append placeholder (tool_call awaits tool_result, not user). This is the one case where the tail is not strictly user but is valid — the invariant needs an explicit exception for "pending tool calls."
- **DM perspective remapping:** After apply_perspective, mapped-to-assistant messages may break alternation with adjacent original-role messages. Normalization runs AFTER perspective mapping.
- **Synthetic system markers in mid-history:** Currently they flow through as inline LlmMessage::system. Under the new invariant they must be either (a) stripped from LLM context (UI/SSE still shows them), or (b) converted to an inline quoted-user format. I recommend (a) strip — markers already emit SSE events and persist as visible UI elements; they do not need to appear in the LLM context twice. This is a behavior change worth calling out in the PR.
- **Consecutive user messages from DM reasoning + peer inbound:** Merging must preserve from_agent metadata for the newer message. The merged message becomes a single logical turn from "multiple speakers" — acceptable because the DM filter already handles this downstream.

---

### 5. Implementation sketch

**Files to change:**

1. **crates/alms-runtime/src/context.rs** — core change.
   - Add normalize_for_llm(messages, has_fresh_input) as the last step inside build_with_perspective, after group_tool_calls and the optional current_input push.
   - Normalize steps in order: (i) move/strip mid-history system messages; (ii) merge consecutive same-role (respecting tool_call/tool_result pairings); (iii) ensure trailing-user invariant (skip if tail is a pending-tool-call group).
   - Private helper is_pending_tool_group(msgs) -> bool to detect the tool-call-awaiting-result case.

2. **crates/alms-runtime/src/anthropic.rs** — simplify.
   - Downgrade merge_consecutive_roles to a debug_assert! check, or remove.
   - Keep system-extraction logic (still provider-specific).
   - Add debug_assert! that messages is non-empty and ends with user role after extraction.

3. **crates/alms-gateway/src/runs/lifecycle.rs** — documentation cleanup only.
   - Shrink the 35-line explanation at line 834-867 to 5-6 lines referring to the invariant.

4. **crates/alms-gateway/src/runs/markers.rs** — no code change, but a doc note that markers are UI/persistence artifacts and NOT included in LLM context.

**Tests — where the invariant gets pinned:**

Unit tests in context.rs, one integration test in alms-runtime/tests/ that exercises the Anthropic adapter specifically. Specific cases worth locking down:

- test_normalize_empty_input_synthesizes_trailing_user
- test_normalize_empty_input_assistant_tail_gets_placeholder
- test_normalize_empty_input_pending_tool_calls_no_placeholder
- test_normalize_strips_mid_history_system_markers
- test_normalize_merges_consecutive_user_messages
- test_normalize_preserves_system_prefix_block
- test_anthropic_adapter_trusts_canonical_shape — canonical-shape input, assert extracted system is correct and messages ends with user
- test_anthropic_adapter_never_sends_empty_messages_array — regression
- test_dm_perspective_then_normalize — perspective mapping + normalization composes correctly
- Extend test_notification_run_context_ends_with_user_message to assert the invariant holds even when the notification_input message is absent (simulate a lifecycle failure).

**Rollout — one PR.**

The change is self-contained, CI-verifiable, and has no config surface. Staging across PRs would leave the tree in a half-enforced state. One PR with:

- Invariant enforcement in ContextBuilder
- Tests pinning each invariant
- Anthropic adapter simplification
- Doc cleanup in lifecycle.rs
- Brief note in docs/agent-runtime-design.md describing the invariant

After merge, file a follow-up issue to audit other Role::System write-sites (job notifications, synthetic markers) and confirm none rely on being visible in LLM context.

---

### What I would push back on

- Do **not** make normalize_for_llm configurable. There is no reasonable operator toggle here — it is either correct or broken.
- Do **not** try to preserve mid-history system markers in LLM context via role coercion. That is the direction that got us #584. Strip them and rely on the SSE/UI side to surface them.
- Do **not** remove the notification_input metadata flag as part of this work. It is doing distinct jobs in persistence, UI filtering, and future auditing; coupling its fate to the alternation fix would bloat the PR for no gain.
