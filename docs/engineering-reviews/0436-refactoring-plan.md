# Reviewing a refactoring plan before any code was written

> Reconstructed from the review history of the ALMS project's original issue tracker.
> Cross-references like `#1246` point at that tracker and are kept verbatim for provenance.

**Source:** Issue #436 -- Refactoring plan: agent.rs split + alms-tools extraction  
**Rounds:** 1  


A design-stage review of the proposed split of the agent module and the extraction of the tools crate: module boundaries, dependency direction, migration order, PR granularity, and the risks the plan had not accounted for.

---


*Posted 2026-03-30*


## Review by Tim (automated) -- Refactoring Plan #436

**Verdict:** Sound overall. The module boundaries are well-chosen and the dependency analysis is careful. I have found several inaccuracies in the line estimates, one structural concern with the ToolRegistry split, and concrete recommendations for each open question. Details below.

---

### 1. agent.rs Split (Phase 1) -- Module Boundary Verification

I read the full 3,401-line agent.rs against the proposed split. The seven modules are the right conceptual boundaries, but the **line estimates are significantly off** for several modules. Corrected numbers from the actual file:

| Module | Plan estimate | Actual lines | Delta |
|--------|--------------|--------------|-------|
| types.rs | ~130 | ~113 (L28-140) | Minor |
| mod.rs | ~350 | ~585 (L142-722 + L1815-1818) | **+67%** |
| loop_impl.rs | ~450 | ~766 (L1048-1813) | **+70%** |
| context.rs | ~200 | ~323 (L724-1046) | **+62%** |
| dm.rs | ~200 | ~154 (L540-588 + L1854-1958) | Minor |
| helpers.rs | ~60 | ~32 (L1821-1852) | Minor |
| tests.rs | ~1,100 | **1,442** (L1960-3401) | **+31%** |

The claim that no production file exceeds 500 lines is **false**: mod.rs would be ~585 lines and loop_impl.rs would be ~766 lines.

**Recommendation:** This does not change the plan validity -- those are still massive improvements over a single 3,401-line file -- but the estimates should be corrected before execution so PR reviewers know what to expect. For loop_impl.rs at 766 lines, consider whether stream_llm_call (L1515-1603, ~88 lines) should live in a separate file (it is conceptually distinct from the tool execution loop), which would bring loop_impl.rs under ~680 lines.

**Cross-cutting concern -- finish_run:** The plan puts finish_run in mod.rs because it coordinates between context building, the agent loop, and session persistence. This is correct. However, finish_run (L596-722) contains substantial DM-specific logic: dm_peer_name, dm_marker_metadata, DM-specific session skip logic, DM role mapping. It calls into dm.rs helpers. This is fine architecturally (it is the orchestrator), but the DM branching makes mod.rs harder to follow than it appears. Something to be aware of during review.

**Cross-cutting concern -- builder methods and DM methods interleaved:** Lines 540-588 contain dm_peer_name, dm_addendum, dm_marker_metadata -- these are impl AgentRuntime methods sandwiched between run_on_session (L507-538) and finish_run (L596). In the split, these move to dm.rs while the surrounding methods stay in mod.rs. The split is clean because they are separate impl AgentRuntime blocks, but the implementer should verify that dm.rs references the struct correctly via Rust multi-file impl block pattern.

**No issues with include_str!:** All four include_str! calls (initial.md, tool_loop.md, dm_recipient.md, summarizer.md) are in code that stays in agent/ sub-modules (types.rs, dm.rs, context.rs). Relative paths (../prompts/...) will need to adjust to ../../prompts/... when agent.rs becomes agent/mod.rs one directory deeper. The plan does not mention this. It is a mechanical fix, but forgetting it will cause a compile error.

### 2. Utility Migrations (Phase 3, executed as Step 2)

**content_to_string:** Currently pub(crate) in context.rs, used by 4 callers within alms-runtime:
- agent.rs (L1001, in maybe_summarize)
- context.rs (L312-316, in convert_message)
- read_messages_tool.rs, read_session_tool.rs, read_subagent_session_tool.rs

The plan to move it to alms-session as Content::to_display_string() is the right call. Content is defined in alms-session, so adding an inherent method there is natural. The only concern: the current implementation has a 2000-byte truncation for ToolResult content. Is that truncation behavior appropriate for alms-session (a persistence/data layer), or should it remain in a presentation-layer caller? I would keep the truncation in to_display_string() since all current callers want it, but document it clearly.

**derive_source_label + SourceLabel:** Used by list_my_sessions_tool.rs and alms-gateway/runs.rs. The function only does string parsing on context_id prefixes (dm:, subagent_, job_, etc.). Moving to alms-core is correct -- it has no runtime dependencies. Note: it also calls a local helper truncate_to_char_boundary in episodic.rs (L104). Make sure that helper either moves with it or is inlined.

**Both migrations are low risk and well-scoped.**

### 3. alms-tools Extraction (Phase 2, executed as Step 3)

**Option A feasibility (no alms-runtime -> alms-tools dependency):** I verified this is feasible. The gateway runs.rs is the single construction site for all tool instances (lines 649-775). Tool registration currently goes through typed with_* builder methods that are thin wrappers around self.tools.register(Arc::new(tool)). Replacing these with a generic register_tool() or direct runtime.tools().register() is straightforward.

However, there is a complication the plan identifies but underestimates:

**SubagentDispatcher depends on RuntimeEventSender:** The SubagentDispatcher trait (line 44 in subagent.rs) takes Option of RuntimeEventSender as a parameter in dispatch() and dispatch_background(). RuntimeEventSender is a type alias for tokio::sync::mpsc::UnboundedSender of RuntimeEvent. If SubagentDispatcher moves to alms-tools, then RuntimeEvent must also be accessible from alms-tools. This creates a cascade:
- RuntimeEvent::ApprovalRequired contains a tokio::sync::oneshot::Sender of bool (runtime concern)
- RuntimeEvent also references serde_json::Value and uuid::Uuid

This is the single hardest dependency to untangle. See my recommendation in section 4 below.

**ToolRegistry wrapper in tools.rs:** The plan risk assessment item 4 correctly identifies that to_definitions() references crate::llm_types::ToolDefinition. This is the LLM serialization format, not a tool concern. The plan says the wrapper might split but does not propose a concrete solution. My recommendation: ToolRegistry (the runtime wrapper) stays in alms-runtime. It adds LLM-specific behavior (to_definitions()) on top of the sandbox registry. Tools do not need the wrapper -- they are registered on the sandbox ToolRegistry directly. The alms-tools crate only needs alms-sandbox::ToolRegistry for registration, not the runtime wrapper. This avoids the split entirely.

**workspace_write exception:** The plan identifies that WorkspaceWriteTool depends on AgentWorkspace from alms-runtime::workspace. Of the three options, I recommend **option (b): keep workspace_write in alms-runtime** as the one exception. Reasons:
- AgentWorkspace has file I/O logic (read/write/ensure_dir), prompt building, and bootstrap detection. This is runtime-level logic, not a simple data type suitable for alms-core.
- Moving it to alms-core would pull tracing debug/info logging, filesystem operations, and prompt-related string formatting into the core crate -- an abstraction leak.
- Keeping one tool in alms-runtime is a pragmatic exception that avoids polluting the dependency graph.

**Dependency graph:** The proposed graph is cycle-free. One addition needed: alms-coordinator currently depends on alms-runtime for SubagentDispatcher, MessageSender, PollResult, RuntimeEventSender, AgentConfig, AgentRuntime, LlmClient, and RunOutput. After extraction, coordinator would depend on alms-tools for traits + alms-runtime for AgentRuntime/LlmClient/AgentConfig. The plan correctly shows this.

### 4. Open Questions -- My Recommendations

**Q1: Should RuntimeEvent move to alms-core?**

No. RuntimeEvent is a runtime concern:
- ApprovalRequired contains a oneshot::Sender of bool -- a communication channel, not data
- The enum variants map directly to SSE events, which is gateway-level behavior
- Moving it to alms-core weakens the purpose of alms-core (pure types and config)

Instead, I recommend: **keep RuntimeEvent in alms-runtime**. Have the SubagentDispatcher trait accept a type-erased event sender. Concretely, define an EventForwarder trait in alms-tools with methods like forward_tool_start and forward_tool_end that accept the same data fields (invocation_id, tool name, params, source_agent) but without exposing the RuntimeEvent enum.

Then SubagentDispatcher::dispatch() takes Option of Arc dyn EventForwarder instead of Option of RuntimeEventSender. The gateway provides a concrete implementation that wraps RuntimeEventSender. This keeps RuntimeEvent in alms-runtime where it belongs and alms-tools completely decoupled from runtime event types.

This is more work than moving RuntimeEvent, but it is architecturally cleaner and avoids the oneshot::Sender in a data crate problem.

**Q2: Should ToolRegistry (the wrapper) stay in alms-runtime or move to alms-tools?**

Stay in alms-runtime. Rationale:
- The wrapper to_definitions() method converts Tool trait objects to ToolDefinition (LLM serialization format defined in llm_types.rs). This is LLM-specific logic.
- The sandbox ToolRegistry (in alms-sandbox) handles registration, lookup, and execution. That is the layer tools interact with.
- alms-tools should depend on alms-sandbox for the Tool trait and sandbox ToolRegistry, not on the runtime wrapper.
- The runtime wrapper is a thin adapter (~99 lines). It does not need to move.

**Q3: Where should workspace_write live?**

Keep it in alms-runtime (option b). See rationale in section 3 above. One tool staying behind is much simpler than moving AgentWorkspace to alms-core or accepting a reverse dependency.

### 5. Risks the Plan Missed

**R1: include_str! relative path change.** When agent.rs becomes agent/mod.rs (or sub-modules like agent/types.rs), the relative include_str! paths need to change from ../prompts/... to ../../prompts/.... The plan does not mention this. It is trivial to fix but will cause a compilation failure if missed.

**R2: Tool file tests have internal dependencies.** All 10 tool files have cfg(test) mod tests with use super::*. Some tests construct mock implementations of traits like MessageSender and SubagentDispatcher. When these tool files move to alms-tools, the mock impls will need to reference the trait from the new location. If SubagentDispatcher moves to alms-tools this is seamless, but the mock Tool impls in test code may need use alms_sandbox::Tool instead of use super::* depending on re-exports.

**R3: Gateway runs.rs constructs IgnoreMessageTool conditionally (only for dm: contexts, L772-774).** The approach of registering all tools from alms-gateway works, but the conditional logic should stay in the gateway, not move to a registration helper. This is fine, just worth noting.

**R4: send_message_tool.rs imports ConversationEndReason from crate::message_sender.** After extraction, this import path changes. The ConversationEndReason type is also used by alms-gateway/runs.rs (imported as alms_runtime::message_sender::ConversationEndReason). If MessageSender + ConversationEndReason move to alms-tools, the gateway import path changes. The plan mentions MessageSender but does not explicitly list ConversationEndReason, DeliveryReceipt, and SendError as types that move with it. All four types in message_sender.rs must move together.

**R5: send_message_tool.rs line count.** The plan says 228 lines but the actual file is 243 lines. Minor, but contributes to the total estimate being off.

**R6: truncate_to_char_boundary helper in episodic.rs.** The derive_source_label function calls this helper (used for truncating long job IDs). When derive_source_label moves to alms-core, this helper must either move with it or be inlined. The plan does not mention it.

### 6. Migration Order and PR Granularity

The proposed 4-step sequence (split agent.rs -> migrate utilities -> extract crate -> cleanup) is correct. The ordering respects dependency flow: each step is independently merge-able and testable.

**Should any steps be combined?** No. Steps 1 and 2 are distinct enough (intra-crate restructure vs. cross-crate utility moves) that combining them would make the diff harder to review and harder to revert if something goes wrong.

**Should any steps be further split?** Consider splitting Step 3 into two PRs:
- **3a:** Create alms-tools crate, move tool files and trait definitions, update imports.
- **3b:** Remove with_* builder methods from AgentRuntime, replace with generic register_tool(), update runs.rs construction site.

Reason: 3a is mechanical (file moves + import updates), while 3b changes the AgentRuntime public API. Separating them means if the API change causes issues, 3a does not need to be reverted.

However, this is a judgment call. If Atlas is confident and the full CI passes, a single PR for Step 3 is acceptable.

**Duration estimates:** The plan estimates ~2h, ~1h, ~3h, cleanup for steps 1-4. These seem reasonable for an experienced developer who knows the codebase. Step 3 could take longer if the EventForwarder trait approach is adopted for SubagentDispatcher decoupling.

### Summary of Recommendations

1. **Correct the line estimates** before execution -- the plan understates several modules by 30-70%.
2. **Use ../../prompts/... paths in include_str!** in agent sub-modules (trivial but easy to forget).
3. **Keep RuntimeEvent in alms-runtime**, introduce an EventForwarder trait in alms-tools for type-erased event forwarding.
4. **Keep ToolRegistry wrapper in alms-runtime** -- tools interact with the sandbox registry, not the runtime wrapper.
5. **Keep workspace_write in alms-runtime** as the one exception.
6. **Move truncate_to_char_boundary** with derive_source_label to alms-core.
7. **Move ConversationEndReason, DeliveryReceipt, SendError** together with MessageSender -- they are a cohesive unit.
8. Consider splitting Step 3 into 3a (mechanical file moves) and 3b (API change) for safer review.

The plan is well-researched and the overall approach is correct. The risks are manageable. My main concern is the SubagentDispatcher / RuntimeEvent dependency chain, which is the hardest part of the extraction and deserves the most design attention before coding begins.
