use crate::events::{PHASE_CALLING_LLM, PHASE_EXECUTING_TOOLS, RuntimeEvent, RuntimeEventSender};
use crate::llm_types::*;
use alms_core::{AlmsError, AlmsResult, AuditDecision, AuditEvent, TokenUsage, audit_error_string};
use alms_session::{
    Content as SessionContent, Message as SessionMessage, Role as SessionRole, SessionManager,
};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

use super::AgentRuntime;
use super::dm::{
    DM_CONFLICT_MSG, DM_EMPTY_REPLY_MAX_RETRIES, DM_EMPTY_REPLY_RETRY_MSG, detect_dm_conflict,
};
use super::helpers::tool_result_ok;
use super::types::Posture;

/// The phase of an agent run for the phase-aware inactivity timer (#1150).
///
/// Replaces the old flat wall-clock `max_run_duration_secs` enforcement: a
/// run is terminated when it makes no *progress* (a streamed token / reasoning
/// delta, an LLM response, or a tool start) for the budget of the phase it is
/// in, rather than when total wall-clock elapses. The budget per phase is
/// returned by [`AgentRuntime::inactivity_budget`].
///
/// The phase is tracked on the stack in `agent_loop` and evaluated at the
/// top-of-loop checkpoint against [`ActivityClock::idle`]. An LLM call that
/// hangs *before* producing its first delta is bounded by the per-request HTTP
/// guards (#1163/#1169), not this timer — that is the deliberately-accepted
/// limitation of the minimal (no-watchdog) implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunPhase {
    /// P0 — iteration 1, before the run has produced any activity. Budget is
    /// derived (`stream_chunk_timeout_secs + 30s` slack), never a flat knob,
    /// so it scales with the LLM idle timeout.
    AwaitingFirstActivity,
    /// P1 — resting between iterations after the run has produced activity.
    /// Budget = `between_iterations_secs`.
    BetweenIterations,
    /// P3 — a tool batch is/was executing. Budget = `tool_phase_ceiling_secs`,
    /// the coarse backstop; timed tools finish first under their own timeout.
    ExecutingTools,
    /// P3b — a tool batch containing a *blocking* (foreground) `invoke_agent`
    /// is/was executing. Budget is **unbounded** (`0` / disabled).
    ///
    /// A foreground `invoke_agent` blocks the parent's `agent_loop` until the
    /// subagent completes, and the subagent's internal progress never touches
    /// the parent's stack-local [`ActivityClock`] — it forwards events to the
    /// parent's SSE channel, not the clock. Applying the P3 ceiling here would
    /// terminate the *parent* the instant a productive long subagent
    /// (legitimately past `tool_phase_ceiling_secs`) returns, discarding the
    /// subagent's completed work — re-creating, for the foreground path, the
    /// exact failure #1150 set out to fix. The subagent governs its own
    /// runtime via the in-loop phase timer it inherits (a hung one
    /// self-terminates and returns an error to the parent), and the parent's
    /// absolute `max_run_duration_secs` backstop still bounds total runtime —
    /// so the parent must not *also* wall-clock a blocking subagent call. A
    /// *background* `invoke_agent` returns immediately and stays in the normal
    /// `ExecutingTools` phase.
    ExecutingBlockingSubagent,
    /// P3c — a Guarded-posture tool batch is/was blocked on **human approval**.
    /// Budget is **unbounded** (`0` / disabled).
    ///
    /// In Guarded posture — the default for user-triggered interactive runs
    /// (only system-triggered runs are forced to Autonomous in
    /// `resolve_posture_for_run`) — a tool that is not auto-approved blocks the
    /// run at the approval gate until the human approves or denies. The human's
    /// think-time produces no progress signal on the parent's stack-local
    /// [`ActivityClock`], so applying the P3 ceiling here would stall-fail the
    /// run the instant the human took longer than `tool_phase_ceiling_secs` to
    /// decide — re-creating, for the approval path, the same false-stall the
    /// foreground-subagent exemption ([`Self::ExecutingBlockingSubagent`])
    /// avoids. The absolute `max_run_duration_secs` backstop still bounds a
    /// truly-abandoned approval. A batch of only auto-approved tools, or any
    /// FullControl / Autonomous run (no approval gate), stays in the normal
    /// [`Self::ExecutingTools`] phase.
    AwaitingApproval,
}

impl RunPhase {
    /// Human-readable phase label embedded in the stall error message.
    fn label(self) -> &'static str {
        match self {
            RunPhase::AwaitingFirstActivity => "awaiting the first response",
            RunPhase::BetweenIterations => "between iterations",
            RunPhase::ExecutingTools => "executing tools",
            RunPhase::ExecutingBlockingSubagent => "executing a blocking subagent",
            RunPhase::AwaitingApproval => "awaiting human approval",
        }
    }
}

/// Decide whether the phase-aware inactivity timer has tripped (#1150).
///
/// Pure so the per-phase trip / disable semantics are unit-testable without a
/// running loop. Returns `Some(message)` — the exact terminal-error string the
/// agent loop surfaces, which the session sanitiser maps to a "stalled" label
/// — when `budget_secs > 0` and the run has been idle for at least that long
/// in `phase`. A `budget_secs` of `0` disables the phase and always returns
/// `None`, matching the documented `0`-disables escape hatch.
fn stall_error(phase: RunPhase, idle: Duration, budget_secs: u64) -> Option<String> {
    if budget_secs == 0 || idle.as_secs() < budget_secs {
        return None;
    }
    Some(format!(
        "agent run stalled -- no activity for {}s during {}",
        idle.as_secs(),
        phase.label()
    ))
}

/// Canonical name of the subagent-spawning tool (`alms_tools::InvokeAgentTool`).
///
/// Matched as a string literal because `alms-runtime` does not (and per the
/// crate dependency graph must not) depend on `alms-tools`, where the tool is
/// defined — mirroring how `alms-core` matches `"ignore_message"` by name.
const INVOKE_AGENT_TOOL_NAME: &str = "invoke_agent";

/// Whether an `invoke_agent` tool call's arguments request *background*
/// dispatch (`background: true`).
///
/// Mirrors `InvokeAgentTool`'s own parse exactly
/// (`params.get("background").and_then(as_bool).unwrap_or(false)`): anything
/// that is not a literal boolean `true` — absent, `false`, a non-bool value,
/// or unparseable arguments — is foreground. Defaulting the unparseable case
/// to *foreground* is the safe direction: a foreground classification only
/// *disables* the P3 ceiling for the batch (see
/// [`batch_has_blocking_invoke_agent`]), whereas a real background call returns
/// immediately, so the ceiling was moot for it anyway.
fn invoke_agent_call_is_background(arguments: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|v| v.get("background").and_then(serde_json::Value::as_bool))
        .unwrap_or(false)
}

/// Whether a tool batch contains a *blocking* (foreground) `invoke_agent`
/// call (#1150).
///
/// Such a batch is excluded from the P3 tool-phase inactivity ceiling and runs
/// under [`RunPhase::ExecutingBlockingSubagent`] instead: a foreground
/// `invoke_agent` blocks the parent's `agent_loop` until the subagent
/// completes, and the subagent's internal progress never touches the parent's
/// stack-local [`ActivityClock`] — so the parent would trip P3 the instant a
/// productive long subagent returns and discard its completed work. A batch
/// stays "blocking" even when it also holds other tools: it cannot return
/// until the subagent does, so the P3 ceiling can never meaningfully bound it.
///
/// `conflicting_indices` are the DM-conflicting slots that will NOT execute
/// (mirrors the executing-set filter used for the status emit), so a rejected
/// call never counts. A *background* `invoke_agent` returns immediately and is
/// deliberately not matched here.
fn batch_has_blocking_invoke_agent(tool_calls: &[ToolCall], conflicting_indices: &[usize]) -> bool {
    tool_calls.iter().enumerate().any(|(i, tc)| {
        tc.function.name == INVOKE_AGENT_TOOL_NAME
            && !conflicting_indices.contains(&i)
            && !invoke_agent_call_is_background(&tc.function.arguments)
    })
}

/// Whether a tool batch will block on **human approval** under the run's
/// posture (#1150).
///
/// Such a batch is excluded from the P3 tool-phase inactivity ceiling and runs
/// under [`RunPhase::AwaitingApproval`] instead: in `Posture::Guarded` — the
/// default for user-triggered interactive runs — a tool that is not
/// auto-approved blocks the run at the approval gate until the human approves
/// or denies, and that think-time never touches the parent's
/// [`ActivityClock`]. Applying P3 would stall-fail the run the instant the
/// human took longer than `tool_phase_ceiling_secs` to decide; the absolute
/// `max_run_duration_secs` backstop still bounds a truly-abandoned approval.
///
/// Mirrors the approval gate's own per-call decision exactly
/// (`posture == Guarded && !is_auto_approved(name)` — see the gate in
/// `execute_tool_call`): the batch needs approval iff the run is Guarded and at
/// least one *executing* call routes through the gate. `is_auto_approved` is
/// injected (the gate reads it from the tool registry) so this stays a pure,
/// unit-testable predicate. `conflicting_indices` are the DM-conflicting slots
/// that will NOT execute, so a rejected call never counts — matching the
/// executing-set filter used for the status emit and
/// [`batch_has_blocking_invoke_agent`]. In `FullControl` / `Autonomous` there
/// is no approval gate, so this is always `false`.
fn batch_needs_approval(
    posture: Posture,
    tool_calls: &[ToolCall],
    conflicting_indices: &[usize],
    is_auto_approved: impl Fn(&str) -> bool,
) -> bool {
    posture == Posture::Guarded
        && tool_calls.iter().enumerate().any(|(i, tc)| {
            !conflicting_indices.contains(&i) && !is_auto_approved(&tc.function.name)
        })
}

/// Lock-free record of when an agent run last made progress, shared between
/// `agent_loop` (which reads it at the top-of-loop checkpoint) and
/// `stream_llm_call` (which bumps it on every streamed token / reasoning
/// delta, so a long-but-productive stream resets the timer — #1150 P2).
///
/// Stored as nanoseconds elapsed since a fixed `base` instant in a single
/// relaxed `AtomicU64`, so a per-token bump is a cheap atomic store with no
/// mutex contention on the streaming hot path. `u64` nanoseconds since `base`
/// does not wrap for ~584 years, so saturation is a non-issue.
pub(crate) struct ActivityClock {
    base: Instant,
    last_nanos: AtomicU64,
}

impl ActivityClock {
    /// Create a clock whose first activity timestamp is "now".
    pub(crate) fn new() -> Self {
        Self {
            base: Instant::now(),
            last_nanos: AtomicU64::new(0),
        }
    }

    /// Record a progress signal at the current instant.
    pub(crate) fn touch(&self) {
        self.last_nanos
            .store(self.base.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }

    /// Time since the last recorded progress signal.
    fn idle(&self) -> Duration {
        self.base.elapsed().saturating_sub(Duration::from_nanos(
            self.last_nanos.load(Ordering::Relaxed),
        ))
    }
}

/// In-flight tool-call tracker shared between `run_tool_calls` and
/// `execute_tool_call`.
///
/// Keyed by `invocation_id` (the SSE-facing identifier carried by both
/// `ToolStart` and `ToolEnd`), value is the canonical tool name (kept for
/// log context when the outer cancel arm synthesises `ToolEnd` events).
///
/// **Protocol**:
///   1. `execute_tool_call` inserts `(invocation_id, name)` immediately
///      before emitting `ToolStart`, and removes the entry immediately
///      before emitting any of its terminal `ToolEnd` paths (success,
///      error, denied, cancel-during-approval-wait).
///   2. When the outer `select!` cancel arm fires (cancel-during-tool-
///      execution), the inner `execute_tool_call` future is dropped at an
///      `await` point — between award boundaries the steps in (1) are
///      synchronous, so any entry still in the tracker corresponds to a
///      live `tool_start` with no terminal event yet.
///   3. The cancel arm drains the tracker and emits a synthetic
///      `ToolEnd { ok: false, result: {"error": "run cancelled"} }` per
///      remaining entry, restoring the 1:1 invariant before returning
///      `AlmsError::Cancelled`. (Mirrors the result shape from the
///      cancel-during-approval-wait fix in #816 / #845.)
///
/// See issue #846 for the full bug write-up. The frontend defensive sweep
/// at `use-session-stream.js` (added in #594 to mask this bug at the UI
/// layer) is removed in the same PR — the runtime is now solely
/// responsible for terminal-event emission.
type InflightTracker = Mutex<HashMap<Uuid, String>>;

/// Remove `invocation_id` from the in-flight tracker if present. Called by
/// each terminal `ToolEnd` emission path inside `execute_tool_call` so the
/// outer cancel arm in `run_tool_calls` does not synthesise a duplicate
/// event for a tool that already terminated.
///
/// Cheap (single hashmap remove) and a no-op when no tracker is attached.
fn unregister_inflight(inflight: Option<&InflightTracker>, invocation_id: Uuid) {
    if let Some(tracker) = inflight {
        let mut guard = tracker.lock().unwrap_or_else(|p| p.into_inner());
        guard.remove(&invocation_id);
    }
}

/// Drain `inflight` and emit a synthetic `ToolEnd` for each remaining
/// entry. Used by the outer cancel arms in `run_tool_calls` after one of
/// the `select!` cancel branches wins and the inner futures are dropped.
fn synthesize_cancel_tool_ends(sender: Option<&RuntimeEventSender>, inflight: &InflightTracker) {
    let drained: Vec<(Uuid, String)> = {
        let mut guard = inflight.lock().unwrap_or_else(|p| p.into_inner());
        guard.drain().collect()
    };
    if drained.is_empty() {
        return;
    }
    let Some(sender) = sender else { return };
    for (invocation_id, tool_name) in drained {
        debug!(
            tool_name = %tool_name,
            invocation_id = %invocation_id,
            "Emitting synthetic tool_end for cancel-during-tool-execution (#846)"
        );
        let _ = sender.send(RuntimeEvent::ToolEnd {
            invocation_id,
            ok: false,
            result: serde_json::json!({"error": "run cancelled"}),
            source_agent: None,
            task_id: None,
        });
    }
}

/// Agent-visible gloss attached to a user-denied tool result (#1109).
/// Deliberately brief: the `user_denied` flag is the load-bearing
/// signal; the message is a short gloss, matching prior art in mature
/// agent tools.
pub(crate) const USER_DENIED_MESSAGE: &str =
    "The user denied this tool call. Do not retry it or work around the denial.";

/// Build the wire/persistence body for a user-denied tool result (#1109).
///
/// Uses a distinct `user_denied: true` key — NOT the `error` key used by
/// real tool runtime errors. The pre-#1109 `{"error": "denied by user"}`
/// shape was routinely read by models as a retryable failure (retry with
/// tweaked args, or pivot to an adjacent tool) rather than a deliberate
/// user policy decision.
///
/// Emitted byte-identical in three places: the `ToolEnd` SSE event, the
/// persisted `Tool`-role session row (what carries the signal into the
/// next run's context rebuild), and the per-run tool-call records.
pub(crate) fn user_denied_result() -> serde_json::Value {
    serde_json::json!({
        "user_denied": true,
        "message": USER_DENIED_MESSAGE,
    })
}

/// Output of a completed agent loop.
///
/// Rolled up from every LLM call in the loop: `response` is the final
/// assistant text (or `""` for runs that end via `ignore_message`),
/// `usage` is the summed token accounting, and `reasoning` is the
/// concatenated extended-thinking trace from the final LLM turn (the one
/// that produced `response`) — only that turn's reasoning is carried
/// forward because earlier turns' reasoning has already been persisted
/// alongside their tool-call batches.
pub(crate) struct AgentLoopOutput {
    pub response: String,
    pub usage: TokenUsage,
    pub reasoning: Option<String>,
}

/// Output of a single LLM call (streaming or buffered fallback).
///
/// Carries the user-visible `content`, any accumulated extended-thinking
/// `reasoning` trace, any tool calls the model wants to run, and the usage
/// accounting from the provider.
pub(crate) struct StreamCallResult {
    pub content: Option<String>,
    /// Extended-thinking / reasoning trace emitted by the model. For
    /// Anthropic this is the concatenation of all `thinking_delta` chunks;
    /// for OpenAI-compatible reasoning models it's the accumulated
    /// `reasoning_content`. Persisted as metadata on the assistant message;
    /// never replayed back into future LLM calls.
    pub reasoning: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub usage: Option<Usage>,
}

/// Project the streamed `content` and `reasoning_content` buffers onto the
/// `(content, reasoning)` fields of a [`StreamCallResult`], honouring the
/// wire-invariant that reasoning text is **never** laundered into the visible
/// `content` channel when tool calls are present (#767, #776).
///
/// Scenarios:
///
/// 1. `[Text]` or `[Text, ToolUse]` (`content` non-empty) — pass `content`
///    through verbatim; `reasoning_content`, if any, is surfaced on the
///    separate `reasoning` field so the caller can persist it as metadata
///    without replaying it into the next LLM call.
///
/// 2. `[Thinking]` only — pure reasoning-model turns where max_tokens was
///    exhausted before visible output materialised. Promote
///    `reasoning_content` into `content` so the run has something to say.
///    This is the legacy fallback path.
///
/// 3. `[Thinking, ToolUse]` with empty visible text (#776) — do **not**
///    promote. The ToolUse is the agent's actual next action; promoting the
///    thinking trace into `content` would cause it to be replayed as
///    assistant text on the following turn, contradicting the #767 design
///    intent that reasoning stays in a sideband channel. Reasoning is
///    preserved on the `reasoning` field and the caller is expected to
///    drop it (see the `reasoning_content: None` invariant in
///    `agent_loop`'s assistant-context-push) when replaying messages.
///
/// 4. Fully empty stream — both fields return `None`.
fn finalize_content_and_reasoning(
    content: String,
    reasoning_content: String,
    has_tool_calls: bool,
) -> (Option<String>, Option<String>) {
    if !content.is_empty() {
        let reasoning = if reasoning_content.is_empty() {
            None
        } else {
            Some(reasoning_content)
        };
        return (Some(content), reasoning);
    }

    if reasoning_content.is_empty() {
        return (None, None);
    }

    if has_tool_calls {
        // [Thinking, ToolUse] with no visible text: keep reasoning on the
        // reasoning sideband so it is persisted as metadata but never
        // replayed into the next LLM call as assistant `content`.
        return (None, Some(reasoning_content));
    }

    // [Thinking]-only turn (reasoning model hit max_tokens before
    // emitting visible content). Promote so the run still has an answer
    // for the UI to display and for `mark_run_as_completed` to persist,
    // but ALSO keep the same trace on the reasoning sideband. The dual
    // surface is what `RunOutput` carries upstream so the gateway can
    // detect this case (response == reasoning) and skip feeding the
    // trace into the episodic summarizer (#1098). Without the sideband
    // copy, the gateway has no way to distinguish "the agent's visible
    // answer happens to look like reasoning" from "the agent emitted
    // reasoning as a stand-in answer" — and the summarizer would
    // faithfully ingest the trace, polluting `session_summaries.summary`.
    info!("Streaming: content empty, falling back to reasoning_content");
    (Some(reasoning_content.clone()), Some(reasoning_content))
}

/// Build the live-reconciliation events for a buffered fallback (#1162 sym-2).
///
/// When `call_llm_with_cancellation`'s streaming attempt faults and falls back
/// to a buffered `complete()`, any partial it already painted live must be
/// reconciled against the buffered full response. This pure helper decides the
/// event sequence the caller forwards:
///
/// - `emitted == false` (the failed stream painted nothing, or there is no
///   event sender): return an empty vec — there is no partial to retract and a
///   re-emit would just be a redundant stream of text no clean stream produced.
/// - `emitted == true`: return `[StreamReset, ReasoningDelta?, TokenDelta?]` —
///   retract the abandoned partial, then re-stream the buffered `reasoning`
///   (collapsible) and `content` (visible reply) in the same order a clean
///   stream emits them. Empty `content` / `reasoning` are omitted so we never
///   emit a zero-length delta. The result is a single live render identical to
///   what a non-faulting stream would have produced — matching reload exactly.
///
/// Kept pure (takes `&str`/`bool`, returns owned events) so the
/// reset-and-re-emit contract is unit-testable without a failing-stream mock.
fn buffered_fallback_reconcile_events(
    emitted: bool,
    content: Option<&str>,
    reasoning: Option<&str>,
) -> Vec<RuntimeEvent> {
    if !emitted {
        return Vec::new();
    }
    let mut events = vec![RuntimeEvent::StreamReset { source_agent: None }];
    if let Some(text) = reasoning.filter(|t| !t.is_empty()) {
        events.push(RuntimeEvent::ReasoningDelta {
            text: text.to_string(),
            source_agent: None,
        });
    }
    if let Some(text) = content.filter(|t| !t.is_empty()) {
        events.push(RuntimeEvent::TokenDelta {
            delta: text.to_string(),
            source_agent: None,
        });
    }
    events
}

/// Whether a streaming-attempt error is a reqwest **total/connect timeout**
/// (renders `operation timed out`) — the only class where the buffered
/// `complete()` fallback is genuinely futile. A total timeout proves the whole
/// call already exceeded `timeout_secs`, so a buffered re-issue waits out the
/// same deadline and fails identically (#1162 / #1163, minimax-m3 on
/// openrouter); `call_llm_with_cancellation` short-circuits it.
///
/// Deliberately does NOT match a streaming per-chunk **stall** (`stream
/// stalled`): the buffered path is non-streaming, so its first-byte wait
/// (`req.send()`) is bounded by the full `timeout_secs`, not the per-chunk
/// guard — a slow-*generating* model whose stream went quiet can still recover
/// on the re-issue (the silence falls in the header wait, then the body bursts
/// in). Decode faults (connection reset, malformed JSON, gzip) recover too.
///
/// Gated on the `AlmsError::Runtime` transport/decode variant: every genuine
/// timeout signal (total/connect, body-stall, send-phase) is `Runtime`
/// (`streaming.rs`, `mod.rs`), while a non-2xx provider response is
/// `AlmsError::SubagentLlmError` and is excluded by construction — its `Display`
/// carries the raw provider body, which must never short-circuit. Within the
/// `Runtime` string the model's partial output (a decode error's
/// `body_prefix="…"`) is stripped before matching, so only the trusted formatter
/// prefix + reqwest error chain is inspected — closing the untrusted-content
/// class (Codex P2 on #1177). Anchored on the exact `operation timed out`
/// phrase; test-pinned.
fn stream_error_is_timeout(err: &AlmsError) -> bool {
    let AlmsError::Runtime(s) = err else {
        return false;
    };
    s.split_once("body_prefix=")
        .map_or(s.as_str(), |(head, _)| head)
        .contains("operation timed out")
}

impl AgentRuntime {
    /// Inactivity budget, in seconds, for the given run phase (#1150).
    /// `0` means the phase has no inactivity budget (disabled / not bounded by
    /// this timer).
    ///
    /// - **P0** is *derived*, not a knob: `stream_chunk_timeout_secs + 30s`
    ///   slack, so it tracks the LLM idle timeout (with the 180s default it is
    ///   ~210s). A hung first call still defers to the per-request HTTP guards
    ///   (#1163/#1169) because the checkpoint only runs *between* iterations.
    /// - **P1** = `between_iterations_secs`.
    /// - **P3** = `tool_phase_ceiling_secs`.
    /// - **P3b** (a batch with a blocking foreground `invoke_agent`) = `0`
    ///   (unbounded); the subagent's own inherited phase timer governs it, so
    ///   the parent must not wall-clock the blocking call. See
    ///   [`RunPhase::ExecutingBlockingSubagent`].
    /// - **P3c** (a Guarded-posture batch blocked on human approval) = `0`
    ///   (unbounded); a slow human approver must not be read as a stall. The
    ///   absolute `max_run_duration_secs` backstop still bounds an abandoned
    ///   approval. See [`RunPhase::AwaitingApproval`].
    fn inactivity_budget(&self, phase: RunPhase) -> u64 {
        match phase {
            RunPhase::AwaitingFirstActivity => {
                self.llm.stream_chunk_timeout_secs().saturating_add(30)
            }
            RunPhase::BetweenIterations => self.config.between_iterations_secs,
            RunPhase::ExecutingTools => self.config.tool_phase_ceiling_secs,
            // Unbounded: `0` disables the phase via `stall_error`. The absolute
            // `max_run_duration_secs` backstop still applies in the loop.
            RunPhase::ExecutingBlockingSubagent | RunPhase::AwaitingApproval => 0,
        }
    }

    /// Main agent loop with tool execution
    #[instrument(
        level = "debug",
        skip(self, session_manager, messages),
        fields(agent_id = %self.agent_id.0, session_id = %session_id.0)
    )]
    pub(crate) async fn agent_loop(
        &self,
        session_manager: &SessionManager,
        session_id: alms_core::SessionId,
        mut messages: Vec<LlmMessage>,
        is_dm: bool,
        include_user: bool,
        dm_peer: Option<&str>,
    ) -> (Vec<alms_core::ToolCallRecord>, AlmsResult<AgentLoopOutput>) {
        let mut total_usage = TokenUsage::default();
        let mut tool_call_records: Vec<alms_core::ToolCallRecord> = Vec::new();
        let mut tool_seq: u32 = 0;
        // Tracks how many times we have nudged the agent after a DM run
        // ended with no deliverable reply text (#1154 design default #3).
        // Capped at DM_EMPTY_REPLY_MAX_RETRIES to prevent infinite loops.
        let mut dm_empty_reply_retries: u32 = 0;
        // Agent-loop hard caps (#987 / B3, reworked in #1150). Per-step
        // timeouts bound how long any one LLM/tool step can take, but nothing
        // bounded the *count* of steps or made the run-duration cap
        // progress-aware — so an agent that kept calling tools without ever
        // producing a deliverable reply ran forever, and a *productive* long
        // run risked being clipped by the old flat wall-clock cap. On a
        // peer-triggered DM run a wedge left the peer stranded on "Chatting
        // with…" indefinitely; tripping any cap returns an `Err`, which
        // `finish_run` wraps into `FailedWithToolCalls` and the gateway's
        // `handle_dm_run_failure` converts into an `Errored` conversation end
        // so the peer is notified. `0` disables a cap.
        let mut iterations: u32 = 0;
        let run_start = std::time::Instant::now();

        // Phase-aware inactivity timer (#1150). `activity` records the last
        // progress signal (LLM response / streamed delta / tool start) and is
        // shared with `stream_llm_call` so a long-but-productive stream keeps
        // resetting it. `phase` tracks what the run was doing across the most
        // recent idle window; the top-of-loop checkpoint terminates the run
        // when `activity.idle()` exceeds the budget for `phase`. Starts in P0
        // (awaiting the first response) until the run produces any activity.
        //
        // NOTE: P0 is structurally inert as a *trip*. It is only ever evaluated
        // at iteration 1's checkpoint, where `activity.idle()` ≈ 0, and the
        // phase advances to P1/P3 after the first LLM call and never returns to
        // P0 — so its derived budget can never actually fire. A first LLM call
        // that hangs before its first byte is bounded by the per-chunk HTTP
        // guard (#1169), not this phase; the P0 budget exists only as a
        // belt-and-suspenders ceiling that the no-watchdog design never reaches.
        let activity = ActivityClock::new();
        let mut phase = RunPhase::AwaitingFirstActivity;

        loop {
            // Checkpoint A: check cancellation between iterations.
            if let Some(ref token) = self.cancel_token
                && token.is_cancelled()
            {
                info!(agent_id = %self.agent_id.0, "Run cancelled by user");
                return (tool_call_records, Err(AlmsError::Cancelled));
            }

            // Checkpoint A2: enforce the agent-loop hard caps (#987 / B3 /
            // #1150) between iterations, before spending another LLM call. The
            // iteration cap bounds the step count; the phase-aware inactivity
            // check (below) terminates a run that stops making progress; and
            // `max_run_duration_secs` is the absolute wall-clock backstop. An
            // in-flight step is bounded by its own per-step timeout, so the
            // effective ceiling is the budget plus at most one step.
            iterations += 1;
            if self.config.max_iterations > 0 && iterations > self.config.max_iterations {
                warn!(
                    agent_id = %self.agent_id.0,
                    max_iterations = self.config.max_iterations,
                    "Agent loop hit the maximum iteration cap without completing -- \
                     terminating with an error (#987 / B3)"
                );
                return (
                    tool_call_records,
                    Err(AlmsError::Runtime(format!(
                        "agent loop exceeded the maximum of {} iterations \
                         without producing a final response",
                        self.config.max_iterations
                    ))),
                );
            }
            // Absolute wall-clock backstop (#987 / B3, raised to 24h in
            // #1150). Inactivity is the primary guard now, so this only ever
            // catches a run that pings activity forever (a bug). Kept
            // alongside the phase check; `0` disables it.
            if self.config.max_run_duration_secs > 0
                && run_start.elapsed().as_secs() >= self.config.max_run_duration_secs
            {
                warn!(
                    agent_id = %self.agent_id.0,
                    max_run_duration_secs = self.config.max_run_duration_secs,
                    elapsed_secs = run_start.elapsed().as_secs(),
                    "Agent loop exceeded the maximum run duration without completing -- \
                     terminating with an error (#987 / B3 / #1150)"
                );
                return (
                    tool_call_records,
                    Err(AlmsError::Runtime(format!(
                        "agent run exceeded the maximum duration of {} seconds \
                         without producing a final response",
                        self.config.max_run_duration_secs
                    ))),
                );
            }

            // Phase-aware inactivity check (#1150). Terminate the run when it
            // has made no progress for the budget of the phase it spent the
            // most recent idle window in. `activity` is reset on every
            // progress signal (LLM response / streamed delta / tool start), so
            // a productive run — however long — is never clipped here; only a
            // genuinely stalled run trips. `0` budget disables the phase. A
            // hung *first* LLM call defers to the per-request HTTP guards
            // (#1163/#1169) rather than this check, since the checkpoint only
            // runs between iterations (the accepted no-watchdog limitation).
            let budget = self.inactivity_budget(phase);
            // Read the idle window once so the error message and the log field
            // report the exact same figure (a second `activity.idle()` call
            // would drift by a few µs).
            let idle = activity.idle();
            if let Some(msg) = stall_error(phase, idle, budget) {
                warn!(
                    agent_id = %self.agent_id.0,
                    phase = phase.label(),
                    idle_secs = idle.as_secs(),
                    budget_secs = budget,
                    "Agent run stalled -- no activity within the phase budget; \
                     terminating with an error (#1150)"
                );
                return (tool_call_records, Err(AlmsError::Runtime(msg)));
            }

            debug!(
                target: "agent::loop",
                agent_id = %self.agent_id.0,
                "Agent loop iteration"
            );

            // NOTE: `messages.clone()` is required here because
            // `CompletionRequest` takes ownership of the `Vec<LlmMessage>`,
            // but we continue to mutate `messages` after the LLM call
            // (appending tool results for the next iteration). The clone
            // cost scales with conversation length; if this becomes a
            // bottleneck, the LLM client could be changed to accept a
            // reference, but that would require upstream API changes.
            let mut request = CompletionRequest::new(self.llm.default_model())
                .with_messages(messages.clone())
                .with_tools(self.tools.to_definitions())
                .with_max_tokens(self.config.max_tokens);
            // Attach the Anthropic extended-thinking budget so the adapter
            // can rewrite it into the `thinking` field. Non-Anthropic
            // providers silently ignore it.
            if self.config.anthropic_thinking_budget > 0 {
                request = request.with_thinking_budget(self.config.anthropic_thinking_budget);
            }
            // Attach the OpenAI-compat reasoning effort (#768). The
            // adapter in `llm_client` strips it for non-OpenAI wire
            // protocols, DeepSeek R1, and non-reasoning OpenAI models
            // (see `is_openai_reasoning_model`).
            if let Some(effort) = self.config.openai_reasoning_effort {
                request = request.with_reasoning_effort(effort.as_wire_str());
            }
            // Attach the Anthropic prompt-caching flag (#766). The
            // Anthropic adapter emits `cache_control` markers on the
            // trailing system block and the trailing tool when `true`;
            // other providers ignore the field entirely.
            request = request.with_prompt_cache_enabled(self.config.anthropic_prompt_cache_enabled);

            // Attach Gemini knobs (#769): the thinking budget routes
            // into `generationConfig.thinkingConfig` when non-zero, and
            // the caching flag / TTL / session_id let the Gemini adapter
            // create & reference a `cachedContents` resource for the
            // stable prefix. All four are silently ignored by non-Gemini
            // providers.
            if let Some(budget) = self.config.gemini_thinking_budget
                && budget > 0
            {
                request = request.with_gemini_thinking_budget(budget);
            }
            request = request
                .with_gemini_cache_enabled(self.config.gemini_cache_enabled)
                .with_gemini_cache_ttl(self.config.gemini_cache_ttl_seconds)
                .with_session_id(session_id);

            let StreamCallResult {
                content,
                reasoning,
                tool_calls,
                usage,
            } = match self.call_llm_with_cancellation(request, &activity).await {
                Ok(result) => result,
                Err(e) => return (tool_call_records, Err(e)),
            };

            // Record progress for the phase-aware inactivity timer (#1150): an
            // LLM call that produced any content, reasoning, or tool calls
            // counts as activity (this is also what `stream_llm_call` bumps on
            // each streamed delta). After it returns the run rests
            // `BetweenIterations` (P1); a tool batch below overrides the phase
            // to `ExecutingTools` (P3) and re-touches the clock at batch start.
            if content.is_some() || reasoning.is_some() || tool_calls.is_some() {
                activity.touch();
            }
            phase = RunPhase::BetweenIterations;

            // Accumulate token usage from this LLM call
            if let Some(ref usage) = usage {
                total_usage.prompt_tokens += usage.prompt_tokens;
                total_usage.completion_tokens += usage.completion_tokens;
                // Reasoning tokens (#768): OpenAI o-series nests the
                // count under `completion_tokens_details.reasoning_tokens`
                // while DeepSeek / xAI put it flat. `reasoning_tokens_effective`
                // picks the first non-`None` of the two. Accumulate across
                // iterations so the final `RunOutput.usage` reflects the
                // sum over all turns of a run.
                if let Some(r) = usage.reasoning_tokens_effective() {
                    let acc = total_usage.reasoning_tokens.unwrap_or(0);
                    total_usage.reasoning_tokens = Some(acc + r);
                }
                // Cache tokens (#766): Anthropic-only today. Accumulate
                // across iterations so a multi-turn run surfaces its full
                // cache creation + read counts in `RunOutput.usage`.
                // Once any iteration reports cache metrics, the
                // accumulator becomes `Some(n)` — zero is meaningful
                // (cache miss on that turn) and distinct from `None`
                // (provider did not report the field at all).
                if let Some(c) = usage.cache_creation_input_tokens {
                    let acc = total_usage.cache_creation_input_tokens.unwrap_or(0);
                    total_usage.cache_creation_input_tokens = Some(acc + c);
                }
                if let Some(c) = usage.cache_read_input_tokens {
                    let acc = total_usage.cache_read_input_tokens.unwrap_or(0);
                    total_usage.cache_read_input_tokens = Some(acc + c);
                }
            }

            if let Some(tool_calls) = tool_calls {
                messages.push(LlmMessage {
                    role: "assistant".to_string(),
                    content: content.clone(),
                    reasoning_content: None,
                    tool_calls: Some(tool_calls.clone()),
                    tool_call_id: None,
                });

                // Pre-compute stable invocation IDs for each tool call.
                // These correlate tool_start / tool_end SSE events with
                // persisted session history, so history reconstruction uses
                // the same IDs as live streaming.
                let invocation_ids: Vec<Uuid> = tool_calls.iter().map(|_| Uuid::new_v4()).collect();

                // Persist assistant text and tool call entries to session
                // history. Intentionally fire-and-forget: session persistence
                // failures are logged as warnings but do not abort the run.
                // This is a deliberate design choice -- the LLM loop should
                // be resilient to transient SQLite errors, and the in-memory
                // `messages` vec is the authoritative state for the current
                // run. If persistence is critical for your deployment, monitor
                // these warnings and consider promoting them to errors.
                //
                // `reasoning` carries the extended-thinking trace (when the
                // model emitted any). It's attached as metadata on the
                // assistant turn so the UI can render a collapsible
                // reasoning panel after page reload; it is NOT replayed
                // back into future LLM context.
                self.persist_assistant_tool_calls(
                    session_manager,
                    session_id,
                    content.as_deref(),
                    reasoning.as_deref(),
                    &tool_calls,
                    &invocation_ids,
                    is_dm,
                );

                // Collect tool call records for per-run storage (all sessions).
                // `from_agent` mirrors the DM message metadata so the
                // frontend fallback merge path can attribute reasoning
                // blocks to the correct agent when session-level
                // persistence is missing. (#696)
                for tc in &tool_calls {
                    tool_call_records.push(alms_core::ToolCallRecord {
                        seq: tool_seq,
                        role: alms_core::ToolCallRole::Assistant,
                        tool_name: Some(tc.function.name.clone()),
                        tool_id: Some(tc.id.clone()),
                        params: Some(tc.function.arguments.clone()),
                        result: None,
                        timestamp: chrono::Utc::now(),
                        from_agent: self.agent_name.clone(),
                    });
                    tool_seq += 1;
                }

                // Pre-execution conflict detection (#364), recipient-aware
                // under implicit replies (#1154 / S3): reject a batch only
                // when `ignore_message` is paired with a `send_message`
                // aimed at the *current peer* (folding a reply AND ending the
                // conversation with them is contradictory). A `send_message`
                // to a *different* agent alongside `ignore_message` ("loop in
                // Charlie, then end with the peer") is legitimate and no
                // longer rejected. Conflicting tools get error results so the
                // LLM can retry; other tools in the batch still execute.
                let dm_check = detect_dm_conflict(&tool_calls, dm_peer);
                if dm_check.conflict {
                    warn!(
                        "Agent paired ignore_message with a send_message aimed at the \
                         current DM peer -- rejecting both; the agent will retry with one"
                    );
                }

                // Emit status: list only the tool names that will actually
                // execute (exclude conflicting tools so SSE subscribers do
                // not see rejected tools listed as "executing"). Filter by
                // batch *index* (#1160) so a third-agent `send_message` that
                // shares the name of a folded peer-directed one is still
                // listed as executing.
                let tool_names: Vec<&str> = tool_calls
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !dm_check.conflicting_indices.contains(i))
                    .map(|(_, tc)| tc.function.name.as_str())
                    .collect();
                if !tool_names.is_empty() {
                    let detail = tool_names.join(", ");
                    self.emit_status(PHASE_EXECUTING_TOOLS, Some(&detail));

                    // Enter the tool-execution phase for the inactivity timer
                    // (#1150 P3). Touch the clock at batch start so the next
                    // checkpoint measures this batch against the coarse
                    // `tool_phase_ceiling_secs` backstop — the ToolStart that
                    // decision-#3 counts as activity. A batch that is entirely
                    // DM-conflicting (no tool actually runs) leaves the phase
                    // unchanged, so it is still governed by P0/P1.
                    activity.touch();
                    // EXCEPTIONS (#1150): two kinds of batch block on something
                    // whose progress never reaches this clock, so the P3
                    // ceiling would false-stall the run the instant the blocking
                    // thing legitimately overran it. Both run under an unbounded
                    // phase (budget 0); the absolute `max_run_duration_secs`
                    // backstop still bounds them.
                    //
                    // P3b — a *blocking* (foreground) `invoke_agent`: the parent
                    // blocks on the subagent for its full (possibly
                    // long-but-productive) runtime, and the subagent's own
                    // inherited phase timer does the bounding. A background
                    // `invoke_agent` returns immediately and stays in P3.
                    //
                    // P3c — a Guarded-posture batch that routes through the
                    // human-approval gate: the run blocks until the human
                    // approves or denies, and a human slower than
                    // `tool_phase_ceiling_secs` must not be read as a stall. The
                    // predicate mirrors the gate's own per-call decision
                    // (`Guarded && !is_auto_approved`). A batch of only
                    // auto-approved tools, or any FullControl / Autonomous run,
                    // has no approval gate and stays in P3.
                    phase = if batch_has_blocking_invoke_agent(
                        &tool_calls,
                        &dm_check.conflicting_indices,
                    ) {
                        RunPhase::ExecutingBlockingSubagent
                    } else if batch_needs_approval(
                        self.config.posture,
                        &tool_calls,
                        &dm_check.conflicting_indices,
                        |name| self.tools.is_auto_approved(name),
                    ) {
                        RunPhase::AwaitingApproval
                    } else {
                        RunPhase::ExecutingTools
                    };
                }

                // Execute tools with posture-aware concurrency and cancellation.
                //
                // `tool_call_records` and `tool_seq` are passed mutably so
                // the cancel arm of the parallel branch can persist any
                // tools that finished before the cancel landed (#1078) —
                // without this, already-emitted `tool_end` SSE events and
                // audit-`Allow` rows have no matching `Tool`-role row on
                // disk, and the next run's rebuild synthesises a bogus
                // `INTERRUPTED_TOOL_RESULT` for the completed tools.
                let results = match self
                    .run_tool_calls(
                        &tool_calls,
                        &invocation_ids,
                        &dm_check.conflicting_indices,
                        session_manager,
                        session_id,
                        is_dm,
                        &mut tool_call_records,
                        &mut tool_seq,
                    )
                    .await
                {
                    Ok(results) => results,
                    Err(e) => return (tool_call_records, Err(e)),
                };

                // Process results: push tool result messages into the
                // conversation, persist to session, and collect records.
                self.process_tool_results(
                    &tool_calls,
                    results,
                    &invocation_ids,
                    &mut messages,
                    &mut tool_call_records,
                    &mut tool_seq,
                    session_manager,
                    session_id,
                    is_dm,
                );

                // Check if `ignore_message` was called AND succeeded.
                // We inspect the actual tool-call records (which include
                // execution results), not just the LLM's requested calls.
                // This prevents early termination when ignore_message fails
                // (e.g. called from a non-DM session, or blocked by conflict).
                if alms_core::ran_ignore_message_successfully(&tool_call_records) {
                    info!("Agent declined to respond via ignore_message -- ending run early");
                    return (
                        tool_call_records,
                        Ok(AgentLoopOutput {
                            response: String::new(),
                            usage: total_usage,
                            // ignore_message short-circuits before any
                            // follow-up LLM turn, so whatever reasoning was
                            // emitted in this turn was already persisted
                            // with the tool call batch above — don't
                            // double-attach it to the final output.
                            reasoning: None,
                        }),
                    );
                }

                // NOTE (#1154): pre-implicit-reply, the loop terminated here
                // when `send_message` appeared in the batch (the old
                // `should_terminate_after_dm_send` presence check — #407
                // Bug 1). That check did not verify the send succeeded or
                // that the recipient was the DM peer, so a bad-recipient /
                // depth-exceeded / self-message "soft error" result
                // terminated the run as if a reply had been delivered,
                // stranding the peer. Under implicit replies the agent's
                // final assistant text IS the reply (delivered by the
                // gateway's DM completion gate), `send_message` is only for
                // contacting a *different* agent, and the loop simply
                // continues like any other tool turn.

                // Append tool_loop instructions to the system prompt for
                // subsequent iterations. The agent's identity (initial prompt +
                // workspace prefix) is preserved; tool_loop adds continuation
                // guidance on top.
                //
                // For DM sessions, re-inject the DM recipient addendum so the
                // agent remembers the implicit-reply contract on every
                // iteration -- not just the first one (fixes #346).
                self.rebuild_system_prompt_for_tool_loop(&mut messages, include_user, dm_peer);

                continue;
            }

            // --- DM empty-reply nudge (#1154 design default #3) ---
            //
            // Under implicit replies the final assistant text IS the message
            // delivered to the DM peer (by the gateway's completion gate).
            // When a peer-triggered DM run is about to end with no
            // deliverable reply text — empty/whitespace-only content, or a
            // `[Thinking]`-only promotion where `content == reasoning` —
            // give the agent one bounded nudge to either write a real reply
            // or call `ignore_message`. This replaces the old text-only
            // retry machinery (#361), which retried in the OPPOSITE case
            // (text present but no `send_message`) and ended with the
            // silent `DM_TEXT_ONLY_DROPPED` drop.
            //
            // After the nudge is exhausted, the run completes with whatever
            // (non-deliverable) text it has; the gateway's DM completion
            // gate then ends the conversation with an `Errored` reason so
            // the peer is notified instead of stranded.
            let dm_reply_missing = is_dm
                && self.dm_implicit_reply
                && alms_core::deliverable_dm_reply(
                    content.as_deref().unwrap_or(""),
                    reasoning.as_deref(),
                )
                .is_none();

            if dm_reply_missing && dm_empty_reply_retries < DM_EMPTY_REPLY_MAX_RETRIES {
                dm_empty_reply_retries += 1;
                warn!(
                    agent_id = %self.agent_id.0,
                    retry = dm_empty_reply_retries,
                    "DM run ended with no deliverable reply text -- nudging once"
                );

                // Emit a warning event so the operator/UI is aware.
                if let Some(ref tx) = self.event_sender {
                    let _ = tx.send(crate::events::RuntimeEvent::Warning {
                        code: "DM_EMPTY_REPLY_RETRY".to_string(),
                        message: "Agent produced no reply text in a DM session. \
                                  Retrying once with instructions to reply with \
                                  text or use ignore_message."
                            .to_string(),
                        source_agent: None,
                    });
                }

                // Append the nudge as a user message. The (non-deliverable)
                // content is intentionally NOT replayed as assistant text:
                // it is either empty or a promoted reasoning trace, and
                // replaying a reasoning trace as assistant content would
                // launder it into the next turn's context (#776 invariant).
                messages.push(LlmMessage::user(DM_EMPTY_REPLY_RETRY_MSG));

                // Re-inject the DM addendum into the system prompt so the
                // agent is reminded of the implicit-reply contract.
                self.rebuild_system_prompt_for_tool_loop(&mut messages, include_user, dm_peer);

                continue;
            }

            // Nudge exhausted and still no deliverable reply: surface a
            // warning for operator visibility. The gateway's DM completion
            // gate converts this into an `Errored` conversation end for the
            // peer (replaces the silent `DM_TEXT_ONLY_DROPPED` outcome).
            if dm_reply_missing {
                warn!(
                    agent_id = %self.agent_id.0,
                    retries = dm_empty_reply_retries,
                    "DM empty-reply nudge exhausted -- gateway will end the \
                     conversation with an error so the peer is notified"
                );
                if let Some(ref tx) = self.event_sender {
                    let _ = tx.send(crate::events::RuntimeEvent::Warning {
                        code: "DM_EMPTY_REPLY".to_string(),
                        message: "Agent failed to produce reply text after the \
                                  nudge. The DM conversation will be ended with \
                                  an error so the peer is notified."
                            .to_string(),
                        source_agent: None,
                    });
                }
            }

            return (
                tool_call_records,
                Ok(AgentLoopOutput {
                    response: content.unwrap_or_default(),
                    usage: total_usage,
                    // Reasoning for the final (text-only) LLM turn. Persisted
                    // as metadata on the assistant message by `finish_run`
                    // so it's recoverable on page reload.
                    reasoning,
                }),
            );
        }
    }

    /// Call the LLM (streaming with buffered fallback), respecting cancellation.
    ///
    /// Emits `PHASE_CALLING_LLM` status, attempts streaming first, falls back
    /// to buffered mode on streaming failure. Returns a [`StreamCallResult`]
    /// carrying content, reasoning trace (extended thinking, if any), tool
    /// calls, and usage.
    ///
    /// `activity` is the run's [`ActivityClock`] (#1150): the streaming path
    /// bumps it on every visible delta so a long-but-productive stream keeps
    /// the phase-aware inactivity timer from tripping at the next checkpoint.
    async fn call_llm_with_cancellation(
        &self,
        request: CompletionRequest,
        activity: &ActivityClock,
    ) -> AlmsResult<StreamCallResult> {
        self.emit_status(PHASE_CALLING_LLM, None);

        // Tracks whether the streaming attempt painted any partial text live
        // (`TokenDelta` / `ReasoningDelta`) before it (possibly) faulted. Read
        // only on the buffered-fallback path to decide whether the abandoned
        // partial must be retracted before the buffered full response is
        // re-emitted. See `stream_llm_call` and `RuntimeEvent::StreamReset`.
        let emitted = std::sync::atomic::AtomicBool::new(false);

        // Try streaming first.
        let stream_result = if let Some(ref token) = self.cancel_token {
            tokio::select! {
                result = self.stream_llm_call(request.clone(), &emitted, activity) => result,
                _ = token.cancelled() => return Err(AlmsError::Cancelled),
            }
        } else {
            self.stream_llm_call(request.clone(), &emitted, activity)
                .await
        };

        match stream_result {
            Ok(result) => Ok(result),
            Err(e) => {
                // A reqwest *total* timeout re-stalls the same way on a buffered
                // re-issue (it waits out the same `.timeout()` deadline), so skip
                // the futile fallback and surface the diagnostic now. Everything
                // else keeps the fallback — a per-chunk *stall* is recoverable
                // (the buffered first-byte wait absorbs mid-generation silence)
                // and decode faults can succeed fresh (#1162 / #1163). Any
                // already-painted partial is left to the run-failure path, as it
                // is today when the buffered retry also errors before the
                // reconcile.
                if stream_error_is_timeout(&e) {
                    warn!(
                        "Streaming timed out/stalled; skipping the buffered \
                         fallback (a re-issue would re-stall the same way): {e}"
                    );
                    return Err(e);
                }
                warn!("Streaming failed, falling back to buffered: {}", e);
                let response = if let Some(ref token) = self.cancel_token {
                    tokio::select! {
                        result = self.llm.complete(request) => result?,
                        _ = token.cancelled() => return Err(AlmsError::Cancelled),
                    }
                } else {
                    self.llm.complete(request).await?
                };
                let usage = response.usage.clone();
                let choice = response.choices.into_iter().next().ok_or_else(|| {
                    AlmsError::Runtime("LLM returned empty choices array".to_string())
                })?;
                // In buffered fallback we also carry the `reasoning_content`
                // field through so Anthropic non-streaming responses (and
                // OpenAI reasoning models that return the field non-stream)
                // still surface their thinking trace for persistence.
                //
                // Route through `finalize_content_and_reasoning` so the
                // buffered-fallback projection honours the same #767/#776
                // invariant as the streaming path: when tool calls are
                // present and visible content is empty, reasoning stays on
                // the sideband and is NOT laundered into `content` (which
                // would be replayed as assistant text on the next turn).
                let tool_calls = choice.message.tool_calls;
                let has_tool_calls = tool_calls.is_some();
                let (content, reasoning) = finalize_content_and_reasoning(
                    choice.message.content.unwrap_or_default(),
                    choice.message.reasoning_content.unwrap_or_default(),
                    has_tool_calls,
                );

                // Reconcile the live render with the buffered result (#1162
                // sym-2). The streaming attempt may have already painted a
                // *partial* of the reply (minimax-m3 on OpenRouter emits a few
                // chunks, then its stream faults — see #1163). The buffered
                // retry returns the FULL response, delivered separately (for a
                // DM run, as the `dm_message` bubble), so the abandoned partial
                // would otherwise linger as the cut-off-then-full duplicate.
                //
                // `buffered_fallback_reconcile_events` decides the sequence:
                // when (and only when) the failed stream emitted ≥1 delta, the
                // UI is told to drop the run's partial (`StreamReset`) and the
                // buffered `content` / `reasoning` are re-emitted as fresh
                // deltas — making the fallback indistinguishable from a clean
                // stream (a single live render that matches reload, the #1164
                // invariant). When nothing was emitted the vec is empty and no
                // delta was ever shown, so no spurious reset / re-stream fires.
                if let Some(ref sender) = self.event_sender {
                    for ev in buffered_fallback_reconcile_events(
                        emitted.load(std::sync::atomic::Ordering::Relaxed),
                        content.as_deref(),
                        reasoning.as_deref(),
                    ) {
                        let _ = sender.send(ev);
                    }
                }

                Ok(StreamCallResult {
                    content,
                    reasoning,
                    tool_calls,
                    usage,
                })
            }
        }
    }

    /// Execute tool calls with posture-aware concurrency and cancellation.
    ///
    /// - **Guarded**: runs tools sequentially so the user sees one approval
    ///   prompt at a time. Cancellation is checked between each tool.
    /// - **FullControl / Autonomous**: runs non-conflicting tools concurrently
    ///   via [`FuturesUnordered`]. Cancellation races against the entire
    ///   batch.
    ///
    /// Conflicting tools (from DM conflict detection) receive error results
    /// instead of executing.
    ///
    /// On the cancel arm of the parallel branch, any tool that *had already
    /// completed* by the time the cancel landed has its result persisted to
    /// the session message log and added to `tool_call_records` before
    /// `Err(AlmsError::Cancelled)` propagates — closes the silent-data-loss
    /// gap from #1078 where already-emitted `tool_end` SSE events and audit-
    /// `Allow` rows had no matching `Tool`-role row on disk. The next run's
    /// rebuild now sees real results for the completed subset and only
    /// synthesises `INTERRUPTED_TOOL_RESULT` markers for tools that were
    /// genuinely still in-flight when the cancel arrived.
    ///
    /// `tool_call_records` and `tool_seq` are mutable so the cancel-arm
    /// persistence can append in the same shape as `process_tool_results`
    /// does on the Ok path. `is_dm` flips the persisted message role to
    /// `Role::User` for DM sessions (preserves the DM invariant).
    ///
    /// Returns `Err(AlmsError::Cancelled)` if the run is cancelled during
    /// execution; otherwise returns the result vector in tool_calls order.
    #[allow(clippy::too_many_arguments)] // Private helper; the extra params (#1078) carry the persistence cursor through to the cancel arm without introducing a wrapper struct.
    pub(crate) async fn run_tool_calls(
        &self,
        tool_calls: &[ToolCall],
        invocation_ids: &[Uuid],
        conflicting_indices: &[usize],
        session_manager: &SessionManager,
        session_id: alms_core::SessionId,
        is_dm: bool,
        tool_call_records: &mut Vec<alms_core::ToolCallRecord>,
        tool_seq: &mut u32,
    ) -> AlmsResult<Vec<AlmsResult<serde_json::Value>>> {
        // Tracker for in-flight tool calls (#846). Shared with each
        // `execute_tool_call` invocation so the outer cancel arms below can
        // synthesise matching `ToolEnd` events for any tool whose inner
        // future was dropped mid-flight by `tokio::select!`.
        let inflight: InflightTracker = Mutex::new(HashMap::new());
        match self.config.posture {
            Posture::Guarded => {
                // Sequential execution with cancellation support during each tool.
                // Cancellation is checked between tools AND races against each
                // individual tool execution so that long-running tools (e.g. shell
                // commands) can be interrupted mid-flight.
                //
                // Cancel arms in this loop (all unwind through the shared
                // persistence pass below):
                //
                // 1. Inter-tool cancel check (`is_cancelled()` before the next
                //    iteration's tool fires). Reached when cancel landed
                //    between tools OR when the prior iteration's
                //    `execute_tool_call` returned `Err(Cancelled)` via its
                //    internal approval-wait cancel handler (#816). `inflight`
                //    is empty at this point — the prior iteration either
                //    completed normally (removed itself) or unwound through
                //    the approval-wait handler (also removes itself).
                //
                // 2. Outer-`select!` cancel-during-tool-execution arm (#846).
                //    The inner future was dropped mid-await. `inflight`
                //    still holds the in-flight tool's entry, so
                //    `synthesize_cancel_tool_ends` synthesises its `ToolEnd`.
                //
                // Both arms must persist whatever is already in `results`
                // before returning `Err(Cancelled)` — bypassing the
                // `process_tool_results` persistence site silently drops
                // completed tool results and the next run's rebuild
                // synthesises `INTERRUPTED_TOOL_RESULT` markers for tools
                // that actually succeeded. Same shape as the parallel arm
                // fix in #1078 / #1089. (#1090)
                let mut results: Vec<AlmsResult<serde_json::Value>> =
                    Vec::with_capacity(tool_calls.len());
                // Resolve workspace root once per batch — matches the Ok
                // path in `process_tool_results` and the parallel-arm
                // cancel pass in `run_tool_calls_parallel` (Tim's #1089
                // review suggestion, carried over for #1090).
                let workspace_root = self.workspace_root_for_truncate();
                for (i, (tc, &inv_id)) in tool_calls.iter().zip(invocation_ids).enumerate() {
                    if conflicting_indices.contains(&i) {
                        results.push(Err(AlmsError::ToolExecution(DM_CONFLICT_MSG.to_string())));
                        continue;
                    }
                    if let Some(ref token) = self.cancel_token
                        && token.is_cancelled()
                    {
                        // Branch 1 above. Persist any tools that completed
                        // earlier in the loop, then unwind. `inflight` is
                        // empty here, so `synthesize_cancel_tool_ends`
                        // would be a no-op — we call it anyway for
                        // structural parity with the parallel arm and as
                        // defence-in-depth against future code paths that
                        // could leave dangling entries. (#1090)
                        self.persist_completed_guarded_results_on_cancel(
                            tool_calls,
                            invocation_ids,
                            results,
                            tool_call_records,
                            tool_seq,
                            session_manager,
                            session_id,
                            is_dm,
                            workspace_root.as_deref(),
                        );
                        synthesize_cancel_tool_ends(self.event_sender.as_ref(), &inflight);
                        return Err(AlmsError::Cancelled);
                    }
                    let result = if let Some(ref token) = self.cancel_token {
                        tokio::select! {
                            r = self.execute_tool_call(tc, inv_id, session_manager, session_id, Some(&inflight)) => r,
                            _ = token.cancelled() => {
                                // Branch 2 above. Cancel-during-tool-
                                // execution (#846): the inner future was
                                // dropped at an await point inside
                                // `execute_tool_call`. Persist whatever
                                // completed before this point, then
                                // synthesise `ToolEnd`s for the in-flight
                                // subset before unwinding. Persistence
                                // runs BEFORE synthesis so the on-disk
                                // ordering matches the Ok path. (#1090)
                                self.persist_completed_guarded_results_on_cancel(
                                    tool_calls,
                                    invocation_ids,
                                    results,
                                    tool_call_records,
                                    tool_seq,
                                    session_manager,
                                    session_id,
                                    is_dm,
                                    workspace_root.as_deref(),
                                );
                                synthesize_cancel_tool_ends(self.event_sender.as_ref(), &inflight);
                                return Err(AlmsError::Cancelled);
                            }
                        }
                    } else {
                        self.execute_tool_call(tc, inv_id, session_manager, session_id, None)
                            .await
                    };
                    results.push(result);
                }
                Ok(results)
            }
            Posture::FullControl | Posture::Autonomous => {
                self.run_tool_calls_parallel(
                    tool_calls,
                    invocation_ids,
                    conflicting_indices,
                    session_manager,
                    session_id,
                    is_dm,
                    tool_call_records,
                    tool_seq,
                    &inflight,
                )
                .await
            }
        }
    }

    /// Parallel arm of `run_tool_calls` for `Posture::FullControl` and
    /// `Posture::Autonomous`. Split out so the cancel-arm bookkeeping
    /// (collecting completed results, persisting them before returning
    /// `Err(Cancelled)`) stays readable. (#1078)
    ///
    /// Uses [`FuturesUnordered`] rather than `futures::future::join_all`
    /// because we need to preserve results as they complete — `join_all`
    /// buffers them inside the `JoinAll` future, which is dropped when the
    /// outer `select!` cancel arm wins, taking already-completed results
    /// down with it. With `FuturesUnordered` we push each
    /// `(slot, result)` pair into an outer `completed_results` vec the
    /// moment the inner future finishes; if the cancel arm then wins, the
    /// vec still holds every result the runtime had observed up to that
    /// point.
    #[allow(clippy::too_many_arguments)] // Private helper; the params mirror `run_tool_calls`.
    async fn run_tool_calls_parallel(
        &self,
        tool_calls: &[ToolCall],
        invocation_ids: &[Uuid],
        conflicting_indices: &[usize],
        session_manager: &SessionManager,
        session_id: alms_core::SessionId,
        is_dm: bool,
        tool_call_records: &mut Vec<alms_core::ToolCallRecord>,
        tool_seq: &mut u32,
        inflight: &InflightTracker,
    ) -> AlmsResult<Vec<AlmsResult<serde_json::Value>>> {
        use futures::stream::{FuturesUnordered, StreamExt};

        // Indices (into `tool_calls`) of non-conflicting tools to execute.
        // Membership is tested by batch *index* (#1160), not tool name, so a
        // third-agent `send_message` that shares the name of a folded
        // peer-directed one still executes.
        let exec_indices: Vec<usize> = (0..tool_calls.len())
            .filter(|i| !conflicting_indices.contains(i))
            .collect();
        let exec_len = exec_indices.len();

        // Results indexed by *slot* (position within `exec_indices`), not
        // by position within `tool_calls`. `slot -> tool_calls index` via
        // `exec_indices[slot]`. `None` = not-yet-completed.
        let mut completed_results: Vec<Option<AlmsResult<serde_json::Value>>> =
            (0..exec_len).map(|_| None).collect();

        if exec_len > 0 {
            let mut fu: FuturesUnordered<_> = exec_indices
                .iter()
                .enumerate()
                .map(|(slot, &i)| {
                    let fut = self.execute_tool_call(
                        &tool_calls[i],
                        invocation_ids[i],
                        session_manager,
                        session_id,
                        Some(inflight),
                    );
                    async move { (slot, fut.await) }
                })
                .collect();

            // Drain `fu` into `completed_results` as each inner future
            // resolves. Wrapping the drain loop in a `{ ... }` block scopes
            // the `&mut completed_results` borrow that the async block
            // captures: the moment the block returns (either drain
            // exhausted or cancel arm won), the borrow is released and
            // `completed_results` is available again for the post-cancel
            // persistence pass below.
            //
            // We use `FuturesUnordered` rather than `futures::future::join_all`
            // for the parallel batch precisely because we need to recover
            // partial results on cancel. `join_all` buffers each completed
            // result inside its own `JoinAll` future; when the outer
            // `tokio::select!` cancel arm wins and drops that future, the
            // buffered results are lost. `FuturesUnordered` lets us push
            // each `(slot, result)` into the outer-scope `completed_results`
            // vec synchronously as it resolves, so the vec survives the
            // cancel-arm drop. (#1078)
            let cancelled = {
                let drain_fut = async {
                    while let Some((slot, res)) = fu.next().await {
                        completed_results[slot] = Some(res);
                    }
                };

                if let Some(ref token) = self.cancel_token {
                    tokio::select! {
                        _ = drain_fut => false,
                        _ = token.cancelled() => true,
                    }
                } else {
                    drain_fut.await;
                    false
                }
            };

            if cancelled {
                // Persist whatever finished before the cancel landed.
                // The matching `tool_end` SSE event was already emitted
                // synchronously inside `execute_tool_call` BEFORE the
                // future yielded its result — without this persistence
                // pass the next run's rebuild would synthesise
                // `INTERRUPTED_TOOL_RESULT` markers for these tools,
                // silently overwriting real work with "cancelled"
                // placeholders. (#1078)
                //
                // We persist in `tool_calls` order (not completion
                // order) so the on-disk `tool_seq` numbering matches
                // what the Ok path would have produced — the rebuild
                // pipeline assumes tool results are ordered consistently
                // with their corresponding tool_use blocks.
                //
                // Resolve workspace root once per batch — matches the
                // Ok path in `process_tool_results` (Tim's #1089 review
                // suggestion).
                let workspace_root = self.workspace_root_for_truncate();
                for (slot, slot_result) in completed_results.into_iter().enumerate() {
                    let Some(res) = slot_result else { continue };
                    let i = exec_indices[slot];
                    let _ = self.persist_one_tool_result(
                        &tool_calls[i],
                        res,
                        invocation_ids[i],
                        tool_call_records,
                        tool_seq,
                        session_manager,
                        session_id,
                        is_dm,
                        workspace_root.as_deref(),
                    );
                }

                // Synthesise `ToolEnd`s for any inner future that was
                // dropped mid-flight (#846 protocol). The tracker's
                // unregister-before-emit invariant means this only
                // fires for tools that genuinely had not reached their
                // terminal section yet — completed tools have already
                // removed themselves and won't be double-emitted.
                synthesize_cancel_tool_ends(self.event_sender.as_ref(), inflight);
                return Err(AlmsError::Cancelled);
            }
        }

        // All inner futures completed (cancel arm did not fire). Assemble
        // the final result vector in `tool_calls` order: conflicting tools
        // get the DM conflict error, executed tools get their slot's
        // result. By construction every slot is `Some(_)` here.
        let mut slot_iter = completed_results.into_iter();
        let results: Vec<AlmsResult<serde_json::Value>> = (0..tool_calls.len())
            .map(|i| {
                if conflicting_indices.contains(&i) {
                    Err(AlmsError::ToolExecution(DM_CONFLICT_MSG.to_string()))
                } else {
                    slot_iter.next().flatten().unwrap_or_else(|| {
                        // Structurally unreachable: every non-conflicting
                        // slot was populated by the drain loop above.
                        // Mirrors the pre-#1078 `debug_assert` guard on the
                        // `join_all` exec_iter — if this fires the
                        // exec_indices / drain accounting has diverged.
                        debug_assert!(
                            false,
                            "slot_iter exhausted or slot was None: \
                             expected {} populated slots for non-conflicting tools",
                            exec_len
                        );
                        Err(AlmsError::Runtime(
                            "BUG: slot exhausted -- conflicting_indices filter \
                             diverged from exec_indices"
                                .into(),
                        ))
                    })
                }
            })
            .collect();
        Ok(results)
    }

    /// Persist assistant text content and tool call entries to session history.
    ///
    /// This is intentionally fire-and-forget: persistence failures are logged
    /// as warnings but do not abort the run. The in-memory `messages` vec is
    /// the authoritative state for the current run; session persistence is a
    /// best-effort durability layer so that conversation history survives
    /// across runs. If this becomes a reliability concern, these warnings
    /// should be monitored and potentially escalated.
    ///
    /// For DM sessions (`is_dm = true`), messages are persisted as
    /// `Role::User` with `message_type: "reasoning"` metadata so they can be
    /// reconstructed into collapsible reasoning blocks in the UI. This
    /// preserves the DM invariant that all shared-session messages are
    /// `Role::User` (see `apply_perspective()` in context.rs).
    ///
    /// `reasoning_trace` carries the extended-thinking / reasoning text
    /// emitted by the model for this turn, when any. It is attached as
    /// `reasoning_blocks` metadata on the assistant-text message so the
    /// UI can render a collapsible reasoning panel after page reload.
    /// Never replayed back into future LLM context — per Anthropic's
    /// standard mode, prior thinking blocks are not required for
    /// subsequent tool-use turns.
    #[allow(clippy::too_many_arguments)] // Private helper; grouping into a struct would add indirection.
    pub(crate) fn persist_assistant_tool_calls(
        &self,
        session_manager: &SessionManager,
        session_id: alms_core::SessionId,
        content: Option<&str>,
        reasoning_trace: Option<&str>,
        tool_calls: &[ToolCall],
        invocation_ids: &[Uuid],
        is_dm: bool,
    ) {
        let reasoning_meta = self.dm_reasoning_metadata(is_dm);

        // Persist assistant text content (if any) before tool calls.
        // For DM sessions: store as Role::User with reasoning metadata
        // to preserve the DM invariant.
        if let Some(text) = content
            && !text.is_empty()
        {
            let (role, metadata) = if reasoning_meta.is_some() {
                (
                    SessionRole::User,
                    merge_reasoning_blocks(reasoning_meta.clone(), reasoning_trace),
                )
            } else {
                (
                    SessionRole::Assistant,
                    merge_reasoning_blocks(None, reasoning_trace),
                )
            };
            if let Err(e) = session_manager.append_message(
                session_id,
                SessionMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    role,
                    content: SessionContent::Text(text.to_string()),
                    timestamp: alms_core::Timestamp::now(),
                    metadata,
                },
            ) {
                warn!("Failed to persist assistant text to session: {}", e);
            }
        } else if let Some(trace) = reasoning_trace.filter(|t| !t.is_empty()) {
            // Edge case: extended thinking emitted content but the model
            // transitioned straight to a tool-use block with no visible
            // text. Persist a text-less assistant message carrying only
            // the reasoning blocks so the UI can still render the trace.
            let (role, metadata) = if reasoning_meta.is_some() {
                (
                    SessionRole::User,
                    merge_reasoning_blocks(reasoning_meta.clone(), Some(trace)),
                )
            } else {
                (
                    SessionRole::Assistant,
                    merge_reasoning_blocks(None, Some(trace)),
                )
            };
            if let Err(e) = session_manager.append_message(
                session_id,
                SessionMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    role,
                    content: SessionContent::Text(String::new()),
                    timestamp: alms_core::Timestamp::now(),
                    metadata,
                },
            ) {
                warn!("Failed to persist reasoning-only assistant turn: {}", e);
            }
        }

        // Persist tool calls to session history.
        // For DM sessions: store as Role::User with reasoning metadata
        // merged with the existing tool_call_id/tool_invocation_id fields.
        for (tc, invocation_id) in tool_calls.iter().zip(invocation_ids) {
            let base_meta = serde_json::json!({
                "tool_call_id": tc.id,
                "tool_invocation_id": invocation_id.to_string(),
            });
            let (role, metadata) = if reasoning_meta.is_some() {
                (
                    SessionRole::User,
                    Some(self.merge_reasoning_metadata(base_meta, is_dm)),
                )
            } else {
                (SessionRole::Assistant, Some(base_meta))
            };
            if let Err(e) = session_manager.append_message(
                session_id,
                SessionMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    role,
                    content: SessionContent::ToolCall {
                        name: tc.function.name.clone(),
                        params: normalize_tool_args(&tc.function.arguments),
                    },
                    timestamp: alms_core::Timestamp::now(),
                    metadata,
                },
            ) {
                warn!("Failed to persist tool call to session: {}", e);
            }
        }
    }

    /// Process tool execution results: push tool result messages into the
    /// conversation, persist to session, and collect per-run records.
    ///
    /// Every result is routed through the shared in-loop tool-output
    /// truncation service (issue #851) before it lands in `messages` or
    /// the session DB. When truncation fires, the *truncated* preview
    /// (head + tail + spill-path hint) is what enters the conversation
    /// AND what is persisted, so the rebuild path on the next run
    /// observes the same bytes the live agent saw — no asymmetry between
    /// "in-loop view" and "session-history view". The
    /// `truncated_in_loop: true` flag in the message metadata tells
    /// `session_msg_to_llm` to skip its own (smaller) re-truncation pass.
    ///
    /// Visible to test code (in this crate and `alms-coordinator`) so
    /// integration tests can drive the truncation path without spinning
    /// up a full LLM round trip. Not part of the public API surface —
    /// `#[doc(hidden)]` keeps it out of generated docs.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)] // Private helper; the parameters are clear and grouping them into a struct would add indirection without real benefit.
    pub fn process_tool_results(
        &self,
        tool_calls: &[ToolCall],
        results: Vec<AlmsResult<serde_json::Value>>,
        invocation_ids: &[Uuid],
        messages: &mut Vec<LlmMessage>,
        tool_call_records: &mut Vec<alms_core::ToolCallRecord>,
        tool_seq: &mut u32,
        session_manager: &SessionManager,
        session_id: alms_core::SessionId,
        is_dm: bool,
    ) {
        // Resolve once per batch — workspace root doesn't change between
        // tool results in the same call (Tim's #1089 review suggestion).
        let workspace_root = self.workspace_root_for_truncate();
        for ((tool_call, result), invocation_id) in
            tool_calls.iter().zip(results).zip(invocation_ids)
        {
            let content = self.persist_one_tool_result(
                tool_call,
                result,
                *invocation_id,
                tool_call_records,
                tool_seq,
                session_manager,
                session_id,
                is_dm,
                workspace_root.as_deref(),
            );
            messages.push(LlmMessage::tool_result(&tool_call.id, content));
        }
    }

    /// Persist a single tool execution result to the session DB and append
    /// a matching record to the per-run tool-call records vec. Returns the
    /// (possibly truncated) content string so callers that also need to
    /// push it into the in-memory LLM-message vec can do so without
    /// re-running the truncation pass.
    ///
    /// Extracted from `process_tool_results` (#1078) so the cancel arm in
    /// `run_tool_calls` can persist the subset of tools that completed
    /// before the cancel landed. Pre-#1078, `process_tool_results` was the
    /// sole persistence site for `Tool`-role rows; cancel-during-parallel-
    /// tools would silently drop already-completed results, and the next
    /// run's rebuild would synthesise `INTERRUPTED_TOOL_RESULT` markers
    /// for tools that had actually succeeded.
    ///
    /// `workspace_root` is the resolved root used for relativising spill
    /// paths in the LLM-visible hint. Callers compute it once per batch via
    /// `workspace_root_for_truncate()` and pass it in so we don't re-resolve
    /// it per tool call (Tim's #1089 review suggestion).
    #[allow(clippy::too_many_arguments)] // Sibling of `process_tool_results`; same rationale.
    fn persist_one_tool_result(
        &self,
        tool_call: &ToolCall,
        result: AlmsResult<serde_json::Value>,
        invocation_id: Uuid,
        tool_call_records: &mut Vec<alms_core::ToolCallRecord>,
        tool_seq: &mut u32,
        session_manager: &SessionManager,
        session_id: alms_core::SessionId,
        is_dm: bool,
        workspace_root: Option<&std::path::Path>,
    ) -> String {
        let (raw_content, ok) = match result {
            Ok(value) => {
                let ok = tool_result_ok(&value);
                (value.to_string(), ok)
            }
            Err(e) => (format!("Error: {}", e), false),
        };

        // Apply the shared in-loop truncation policy. When the policy
        // is disabled (e.g. unit tests, gateway with the feature
        // turned off in TOML), `truncate` is a pass-through.
        let outcome = crate::tool_output_truncate::truncate(
            &raw_content,
            &self.tool_output_truncate_policy,
            &tool_call.id,
            workspace_root,
        );
        let content = outcome.content;
        let truncated_in_loop = outcome.truncated;

        if truncated_in_loop {
            debug!(
                target: "agent::loop",
                tool = %tool_call.function.name,
                tool_call_id = %tool_call.id,
                original_bytes = outcome.original_bytes,
                original_lines = outcome.original_lines,
                preview_bytes = content.len(),
                spill_path = ?outcome.output_path,
                "Tool output truncated by in-loop service (#851)"
            );
        }

        // Persist tool result to session history.
        // Intentionally fire-and-forget -- see persist_assistant_tool_calls
        // for the rationale.
        //
        // Include tool_invocation_id in the metadata so history
        // reconstruction can correlate tool results back to the same
        // invocation ID used by live SSE tool_start/tool_end events.
        // (Fixes #509)
        //
        // For DM sessions: store as Role::User with reasoning metadata
        // merged with the existing ok/tool_invocation_id fields. This
        // preserves the DM invariant (all shared-session messages are
        // Role::User) and enables UI reasoning block reconstruction.
        //
        // When the in-loop truncate fired, we mark the persisted row
        // with `truncated_in_loop: true` so `session_msg_to_llm` skips
        // its own 2000-byte re-truncation — the bytes already on disk
        // are the bytes the agent saw live.
        {
            let mut base_meta = serde_json::json!({
                "ok": ok,
                "tool_invocation_id": invocation_id.to_string(),
            });
            if truncated_in_loop && let Some(obj) = base_meta.as_object_mut() {
                obj.insert(
                    "truncated_in_loop".to_string(),
                    serde_json::Value::Bool(true),
                );
                obj.insert(
                    "original_bytes".to_string(),
                    serde_json::Value::Number(outcome.original_bytes.into()),
                );
                obj.insert(
                    "original_lines".to_string(),
                    serde_json::Value::Number(outcome.original_lines.into()),
                );
                if let Some(ref path) = outcome.output_path {
                    obj.insert(
                        "spill_path".to_string(),
                        serde_json::Value::String(path.clone()),
                    );
                }
            }
            let (role, metadata) = if is_dm {
                (
                    SessionRole::User,
                    Some(self.merge_reasoning_metadata(base_meta, is_dm)),
                )
            } else {
                (SessionRole::Tool, Some(base_meta))
            };
            if let Err(e) = session_manager.append_message(
                session_id,
                SessionMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    role,
                    content: SessionContent::ToolResult {
                        tool_id: tool_call.id.clone(),
                        result: serde_json::from_str(&content)
                            .unwrap_or(serde_json::Value::String(content.clone())),
                    },
                    timestamp: alms_core::Timestamp::now(),
                    metadata,
                },
            ) {
                warn!("Failed to persist tool result to session: {}", e);
            }
        }

        // Collect tool result record for per-run storage (all sessions).
        tool_call_records.push(alms_core::ToolCallRecord {
            seq: *tool_seq,
            role: alms_core::ToolCallRole::Tool,
            tool_name: Some(tool_call.function.name.clone()),
            tool_id: Some(tool_call.id.clone()),
            params: None,
            result: Some(content.clone()),
            timestamp: chrono::Utc::now(),
            from_agent: self.agent_name.clone(),
        });
        *tool_seq += 1;

        content
    }

    /// Persist the subset of `results` accumulated by the Guarded
    /// sequential arm of `run_tool_calls` before unwinding on cancel.
    ///
    /// `results` is a positional vec: `results[i]` holds the outcome of
    /// `tool_calls[i]`. Conflicting tools and tools that ran to
    /// completion (Ok or tool-execution Err) are persisted via
    /// [`persist_one_tool_result`] in `tool_calls` order so the on-disk
    /// `tool_seq` numbering matches the Ok path.
    ///
    /// `Err(AlmsError::Cancelled)` entries are skipped: those represent
    /// approval-wait cancellation inside `execute_tool_call` (#816) — the
    /// tool body never ran, no real result exists, and the next run's
    /// rebuild correctly synthesises `INTERRUPTED_TOOL_RESULT` for the
    /// orphan `tool_use` block. Persisting a fabricated "Error: Cancelled"
    /// row would mask that and feed the model a misleading "this tool
    /// errored" signal for work that genuinely never started. (#1090)
    #[allow(clippy::too_many_arguments)] // Mirrors `persist_one_tool_result`; same rationale.
    fn persist_completed_guarded_results_on_cancel(
        &self,
        tool_calls: &[ToolCall],
        invocation_ids: &[Uuid],
        results: Vec<AlmsResult<serde_json::Value>>,
        tool_call_records: &mut Vec<alms_core::ToolCallRecord>,
        tool_seq: &mut u32,
        session_manager: &SessionManager,
        session_id: alms_core::SessionId,
        is_dm: bool,
        workspace_root: Option<&std::path::Path>,
    ) {
        for (i, res) in results.into_iter().enumerate() {
            if matches!(res, Err(AlmsError::Cancelled)) {
                continue;
            }
            let _ = self.persist_one_tool_result(
                &tool_calls[i],
                res,
                invocation_ids[i],
                tool_call_records,
                tool_seq,
                session_manager,
                session_id,
                is_dm,
                workspace_root,
            );
        }
    }

    /// Resolve the workspace root for spill-path relativisation.
    ///
    /// Falls back to `resolved_sandbox_root` when no workspace is attached
    /// (e.g. unnamed agents) and to `None` when the agent has neither (the
    /// truncate service emits the absolute path in that case). Centralised so
    /// `process_tool_results` and `truncate_for_emit` see the same root.
    pub(crate) fn workspace_root_for_truncate(&self) -> Option<std::path::PathBuf> {
        self.workspace
            .as_ref()
            .map(|w| w.dir().to_path_buf())
            .or_else(|| self.resolved_sandbox_root.clone())
    }

    /// Truncate a tool result `Value` for emission on the audit log + the
    /// `ToolEnd` SSE event (#921 review fix #4).
    ///
    /// The pre-#921 paths emitted the full untruncated `Value` regardless of
    /// size — context-window protection was the only consumer of the in-loop
    /// truncation service. Tim flagged that this still let a 10 MB tool
    /// output saturate SSE bandwidth and audit-log disk even after the
    /// in-loop messages vec was capped. Routing the audit + SSE payloads
    /// through the same `truncate` service closes that gap.
    ///
    /// Behaviour:
    /// - When the truncate policy is disabled OR the JSON-stringified value
    ///   is inside both caps, the original `Value` is returned unchanged
    ///   (preserves structured-JSON wire shape for small results).
    /// - When truncation fires, returns a `Value::String(preview)` carrying
    ///   the head+tail preview with the spill-path hint appended. Wire-shape
    ///   for oversized results becomes a JSON string instead of a structured
    ///   object — consumers that want the full bytes use the spill file.
    ///
    /// Visible across crates (`alms-coordinator` integration tests) but
    /// `#[doc(hidden)]` so it stays off the public API surface.
    #[doc(hidden)]
    pub fn truncate_for_emit(
        &self,
        tool_call_id: &str,
        value: &serde_json::Value,
    ) -> serde_json::Value {
        if !self.tool_output_truncate_policy.is_active() {
            return value.clone();
        }
        let raw = value.to_string();
        let workspace_root = self.workspace_root_for_truncate();
        let outcome = crate::tool_output_truncate::truncate(
            &raw,
            &self.tool_output_truncate_policy,
            tool_call_id,
            workspace_root.as_deref(),
        );
        if outcome.truncated {
            serde_json::Value::String(outcome.content)
        } else {
            value.clone()
        }
    }

    /// Stream an LLM call, emitting `TokenDelta` events as text chunks arrive.
    ///
    /// Accumulates the full response (content + tool calls + usage) from the
    /// streaming chunks and returns them in the same shape as `complete()`.
    ///
    /// **Timeout**: Per-chunk timeout is enforced inside the LLM client's
    /// `complete_stream` implementation (see `LlmClient::complete_stream` in
    /// `llm_client/`), controlled by `LlmConfig::stream_chunk_timeout_secs`
    /// (default 60s). If the provider stalls mid-stream, the chunk-level
    /// timeout fires and propagates an error up through this method. User-
    /// initiated cancellation is handled separately in `call_llm_with_cancellation`.
    /// `emitted` is set to `true` the first time this call forwards a visible
    /// `TokenDelta` or `ReasoningDelta` to the UI. The caller
    /// (`call_llm_with_cancellation`) reads it on the buffered-fallback path:
    /// a stream that faulted *after* painting a partial must be retracted with
    /// `RuntimeEvent::StreamReset` before the buffered full response is
    /// re-emitted, otherwise the abandoned partial double-renders against the
    /// buffered result (#1162 sym-2). A stream that faulted before emitting
    /// anything needs no reset.
    ///
    /// `activity` is the run's [`ActivityClock`] (#1150): it is touched on
    /// every visible `TokenDelta` / `ReasoningDelta` so the phase-aware
    /// inactivity timer in `agent_loop` treats a steadily-streaming response
    /// as progress and never trips mid-reply. A *stalled* stream (no chunk
    /// for `stream_chunk_timeout_secs`) is already faulted by the per-chunk
    /// guard above, so this timer does not need to police it.
    pub(crate) async fn stream_llm_call(
        &self,
        request: CompletionRequest,
        emitted: &std::sync::atomic::AtomicBool,
        activity: &ActivityClock,
    ) -> AlmsResult<StreamCallResult> {
        use futures::StreamExt;

        let mut stream = self.llm.complete_stream(request).await?;

        let mut content = String::new();
        let mut reasoning_content = String::new();
        let mut tool_call_acc: Vec<(String, String, String)> = Vec::new(); // (id, name, arguments)
        let mut usage: Option<Usage> = None;

        while let Some(result) = stream.next().await {
            let chunk = result?;

            // Accumulate usage across chunks. Anthropic streaming sends
            // input_tokens in `message_start` and output_tokens in
            // `message_delta` as separate events, so we merge by taking
            // the max of each field rather than overwriting the struct.
            //
            // NOTE: Anthropic sends each token count exactly once (not
            // incrementally), so max() is equivalent to "take the non-zero
            // value". If the protocol ever switches to incremental
            // reporting, this would need to become additive.
            if let Some(chunk_usage) = chunk.usage {
                usage = Some(match usage {
                    Some(prev) => {
                        let p = prev.prompt_tokens.max(chunk_usage.prompt_tokens);
                        let c = prev.completion_tokens.max(chunk_usage.completion_tokens);
                        Usage {
                            prompt_tokens: p,
                            completion_tokens: c,
                            total_tokens: p + c,
                            // Reasoning tokens are captured from the
                            // incoming chunk's effective count (nested or
                            // flat — whichever the provider emits) and
                            // persisted on the Usage; we take the max
                            // across chunks for the same "report-once"
                            // reason as prompt/completion.
                            reasoning_tokens: {
                                let prev_r = prev.reasoning_tokens_effective();
                                let chunk_r = chunk_usage.reasoning_tokens_effective();
                                match (prev_r, chunk_r) {
                                    (Some(a), Some(b)) => Some(a.max(b)),
                                    (a, b) => a.or(b),
                                }
                            },
                            completion_tokens_details: None,
                            // Cache tokens (#766) — same "report-once"
                            // semantics as prompt/completion. Anthropic
                            // emits the creation count on `message_start`
                            // and repeats it on `message_delta`; max()
                            // across chunks handles either order.
                            cache_creation_input_tokens: match (
                                prev.cache_creation_input_tokens,
                                chunk_usage.cache_creation_input_tokens,
                            ) {
                                (Some(a), Some(b)) => Some(a.max(b)),
                                (a, b) => a.or(b),
                            },
                            cache_read_input_tokens: match (
                                prev.cache_read_input_tokens,
                                chunk_usage.cache_read_input_tokens,
                            ) {
                                (Some(a), Some(b)) => Some(a.max(b)),
                                (a, b) => a.or(b),
                            },
                        }
                    }
                    None => chunk_usage,
                });
            }

            let Some(choice) = chunk.choices.into_iter().next() else {
                continue;
            };

            // Accumulate text content and emit token_delta events
            if let Some(text) = choice.delta.content
                && !text.is_empty()
            {
                content.push_str(&text);
                // Progress signal for the inactivity timer (#1150). Touched
                // unconditionally — a streaming reply with no UI subscriber is
                // still making forward progress and must reset the timer.
                activity.touch();
                if let Some(ref sender) = self.event_sender {
                    emitted.store(true, std::sync::atomic::Ordering::Relaxed);
                    let _ = sender.send(RuntimeEvent::TokenDelta {
                        delta: text,
                        source_agent: None,
                    });
                }
            }

            // Accumulate reasoning_content from reasoning models (OpenAI
            // o-series, DeepSeek R1, etc.) and Anthropic extended thinking
            // (routed through the same channel by `parse_anthropic_sse`).
            //
            // Emit as `RuntimeEvent::ReasoningDelta` so the gateway can
            // forward a `reasoning_delta` SSE event and the UI can render
            // it in a collapsible panel. Also preserved in-process so we
            // can fall back to it when the final `content` stream is
            // empty (some reasoning models exhaust max_tokens before
            // transitioning to visible output).
            if let Some(text) = choice.delta.reasoning_content
                && !text.is_empty()
            {
                reasoning_content.push_str(&text);
                // Progress signal for the inactivity timer (#1150) — reasoning
                // deltas count as activity too, so a long extended-thinking
                // stretch keeps the timer reset. Touched unconditionally for
                // the same reason as the content branch above.
                activity.touch();
                if let Some(ref sender) = self.event_sender {
                    emitted.store(true, std::sync::atomic::Ordering::Relaxed);
                    let _ = sender.send(RuntimeEvent::ReasoningDelta {
                        text,
                        source_agent: None,
                    });
                }
            }

            // Accumulate tool call deltas
            if let Some(deltas) = choice.delta.tool_calls {
                for delta in deltas {
                    let idx = delta.index as usize;
                    // Grow the accumulator if needed
                    while tool_call_acc.len() <= idx {
                        tool_call_acc.push((String::new(), String::new(), String::new()));
                    }
                    if let Some(id) = delta.id {
                        tool_call_acc[idx].0 = id;
                    }
                    if let Some(ref func) = delta.function {
                        if let Some(ref name) = func.name {
                            tool_call_acc[idx].1 = name.clone();
                        }
                        if let Some(ref args) = func.arguments {
                            tool_call_acc[idx].2.push_str(args);
                        }
                    }
                }
            }
        }

        // Build final tool_calls from accumulated deltas.
        // Filter out ghost entries that can appear if the accumulator was
        // grown by index but no actual data arrived. Check both id and name:
        // a non-empty id with an empty name would produce a ToolCall that
        // fails at the tools.contains(name) check in execute_tool_call.
        let tool_calls: Vec<ToolCall> = tool_call_acc
            .into_iter()
            .filter(|(id, name, _)| !id.is_empty() && !name.is_empty())
            .map(|(id, name, arguments)| ToolCall::new(id, name, arguments))
            .collect();
        let tool_calls = if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        };

        // Decide how to project the accumulated `content` and
        // `reasoning_content` onto the `StreamCallResult`. See
        // `finalize_content_and_reasoning` for the full rationale.
        let has_tool_calls = tool_calls.is_some();
        let (content, reasoning_out) =
            finalize_content_and_reasoning(content, reasoning_content, has_tool_calls);

        Ok(StreamCallResult {
            content,
            reasoning: reasoning_out,
            tool_calls,
            usage,
        })
    }

    /// Execute a tool call, emitting tool_start/tool_end events and handling approvals.
    ///
    /// `inflight` is the in-flight tool tracker shared with `run_tool_calls`
    /// (see [`InflightTracker`]). When `Some`, this method registers the
    /// `(invocation_id, tool_name)` pair before emitting `ToolStart` and
    /// removes it before any of its terminal `ToolEnd` paths so the outer
    /// `select!` cancel arm can synthesise events for any future that was
    /// dropped mid-flight. `None` is used by the non-cancel-token branch
    /// and by tests that do not care about cancel-during-execution
    /// bookkeeping.
    #[instrument(
        level = "info",
        skip(self, tool_call, invocation_id, session_manager, inflight),
        fields(
            agent_id = %self.agent_id.0,
            tool_name = %tool_call.function.name,
            tool_call_id = %tool_call.id,
            invocation_id = %invocation_id,
            session_id = %session_id.0
        )
    )]
    pub(crate) async fn execute_tool_call(
        &self,
        tool_call: &ToolCall,
        invocation_id: Uuid,
        session_manager: &SessionManager,
        session_id: alms_core::SessionId,
        inflight: Option<&InflightTracker>,
    ) -> AlmsResult<serde_json::Value> {
        let name = &tool_call.function.name;
        let args_str = &tool_call.function.arguments;

        info!(
            target: "agent::tool::start",
            agent_id = %self.agent_id.0,
            tool_name = %name,
            tool_call_id = %tool_call.id,
            "Executing tool"
        );

        // Wall-clock start time. In Guarded mode, `elapsed` will include
        // however long the user took to approve the tool call. If pure
        // execution-only timing is ever needed, reset `start` after the
        // approval check below.
        let start = std::time::Instant::now();

        // Parse arguments.
        //
        // Anthropic's streaming protocol omits the `input_json_delta`
        // event entirely when the model calls a no-args tool (#967), so
        // we observe `args_str == ""`. `serde_json::from_str("")` is an
        // EOF error which would falsely deny an otherwise valid no-args
        // invocation. Normalize empty / whitespace-only strings to an
        // empty object up front. All other parse failures (malformed
        // JSON, non-object literals) keep the original deny path — those
        // are genuine model errors the tool's per-arg deserializer
        // should surface to the agent rather than silently paper over.
        let args: serde_json::Value = if args_str.trim().is_empty() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            match serde_json::from_str(args_str) {
                Ok(value) => value,
                Err(e) => {
                    let err =
                        alms_core::AlmsError::ToolExecution(format!("Invalid arguments: {}", e));
                    let _ = session_manager.append_audit(
                        session_id,
                        AuditEvent {
                            session_id,
                            run_id: self.run_id,
                            tool: name.to_string(),
                            decision: AuditDecision::Deny,
                            params: serde_json::Value::String(args_str.to_string()),
                            result: None,
                            // #997: route every audit emission carrying an
                            // `AlmsError` value through the variant-dispatch
                            // helper so future `SubagentLlmError`-shaped
                            // errors that surface here cannot leak the raw
                            // provider response body. For the
                            // `ToolExecution` value built immediately above
                            // the helper is a no-op pass-through.
                            error: Some(audit_error_string(&err)),
                            timestamp: alms_core::Timestamp::now(),
                        },
                    );
                    return Err(err);
                }
            }
        };

        // Policy gate: deny unknown tools before execution
        if !self.tools.contains(name) {
            let err = alms_core::AlmsError::ToolExecution(format!("Tool '{}' not allowed", name));
            let _ = session_manager.append_audit(
                session_id,
                AuditEvent {
                    session_id,
                    run_id: self.run_id,
                    tool: name.to_string(),
                    decision: AuditDecision::Deny,
                    params: args,
                    result: None,
                    // #997: route through the variant-dispatch helper for
                    // consistency with the other `AlmsError`-bearing audit
                    // emissions in this function. No-op pass-through for
                    // the `ToolExecution` value built immediately above.
                    error: Some(audit_error_string(&err)),
                    timestamp: alms_core::Timestamp::now(),
                },
            );
            return Err(err);
        }

        // Hoisted guard (Tim's nit on #846): the Guarded-posture branch
        // below requires an event sender to emit `ApprovalRequired`. If
        // we discover that requirement AFTER inserting into the in-flight
        // tracker, the early-return via `?` would leave a dangling
        // tracker entry, and a subsequent outer cancel arm would
        // synthesise a `ToolEnd` for an `invocation_id` that never had a
        // matching `ToolStart` (the emit below is itself gated on
        // `Some(ref sender)`). Validating up-front keeps the
        // tracker-insert + tool_start-emit + approval-gate trio
        // structurally consistent.
        let auto_approved = self.tools.is_auto_approved(name);
        let needs_approval_gate = self.config.posture == Posture::Guarded && !auto_approved;
        if needs_approval_gate && self.event_sender.is_none() {
            return Err(alms_core::AlmsError::Runtime(
                "Guarded posture requires an event sender for approvals".to_string(),
            ));
        }

        // Register in the in-flight tracker BEFORE emitting tool_start so
        // that if the next await is dropped by an outer cancel arm (#846),
        // the cancel arm can find this entry and synthesise a matching
        // ToolEnd. Insert + emit are both synchronous, so no future
        // cancellation can fire between them.
        if let Some(tracker) = inflight {
            let mut guard = tracker.lock().unwrap_or_else(|p| p.into_inner());
            guard.insert(invocation_id, name.to_string());
        }

        // Emit tool_start
        if let Some(ref sender) = self.event_sender {
            let _ = sender.send(RuntimeEvent::ToolStart {
                invocation_id,
                tool: name.to_string(),
                params: args.clone(),
                source_agent: None,
                task_id: None,
            });
        }

        // Guarded posture: block until user approves or denies.
        // Auto-approved tools (datetime, echo, read-only tools) bypass this
        // gate — they are inherently safe and requiring approval adds friction
        // with zero security benefit.
        if self.config.posture == Posture::Guarded && auto_approved {
            debug!(
                tool_name = %name,
                "Auto-approved tool — skipping approval gate in guarded posture"
            );
        } else if needs_approval_gate {
            // Sender presence already validated above; unwrap is safe.
            let sender = self
                .event_sender
                .as_ref()
                .expect("event_sender presence validated above");
            let approval_id = Uuid::new_v4();
            let (decision_tx, decision_rx) = tokio::sync::oneshot::channel();
            let _ = sender.send(RuntimeEvent::ApprovalRequired {
                approval_id,
                tool: name.to_string(),
                params: args.clone(),
                decision_tx,
                source_agent: None,
            });
            // Checkpoint D: approval wait with cancellation support.
            //
            // If the run is cancelled while we're blocked here, we MUST emit a
            // matching `ToolEnd` for the `tool_start` we already fired above —
            // the frontend's spinner-cleanup, group-collapsing, and
            // persisted-state-parity logic all rely on that 1:1 invariant. See
            // #816 (the cancel-during-approval-wait counterpart of the
            // post-approval-resolve fix in #800/#803).
            let approved = if let Some(ref token) = self.cancel_token {
                tokio::select! {
                    result = decision_rx => result.unwrap_or(false),
                    _ = token.cancelled() => {
                        // Unregister before emit so the outer cancel arm in
                        // `run_tool_calls` does not synthesise a duplicate
                        // (#846 protocol).
                        unregister_inflight(inflight, invocation_id);
                        // #893: persist the cancellation to the audit log.
                        // Sibling gap to #815 in the same approval gate. Without
                        // this append, the audit trail cannot distinguish
                        // "approval pending then run cancelled" from "approval
                        // never happened" — operators see a `tool_start` with
                        // no corresponding audit row. Using `AuditDecision::Deny`
                        // (rather than introducing a new variant) keeps the
                        // schema/wire shape stable; the error string is the
                        // discriminator that lets log queries separate
                        // user-denial (#815) from cancellation here.
                        let cancel_reason = format!(
                            "Tool '{}' approval cancelled by run cancellation",
                            name
                        );
                        let _ = session_manager.append_audit(
                            session_id,
                            AuditEvent {
                                session_id,
                                run_id: self.run_id,
                                tool: name.to_string(),
                                decision: AuditDecision::Deny,
                                params: args.clone(),
                                result: None,
                                error: Some(cancel_reason),
                                timestamp: alms_core::Timestamp::now(),
                            },
                        );
                        let _ = sender.send(RuntimeEvent::ToolEnd {
                            invocation_id,
                            ok: false,
                            result: serde_json::json!({"error": "run cancelled"}),
                            source_agent: None,
                            task_id: None,
                        });
                        return Err(AlmsError::Cancelled);
                    }
                }
            } else {
                match decision_rx.await {
                    Ok(v) => v,
                    Err(_) => {
                        // Unregister before emit so the outer cancel arm in
                        // `run_tool_calls` cannot synthesise a duplicate
                        // (#846 protocol), matching the cancel and deny sibling
                        // branches. In the Guarded sequential path that reaches
                        // this arm, `cancel_token` is `None` and therefore
                        // `inflight` is `None` too (the two are coupled at the
                        // `execute_tool_call` call site), so this is a no-op
                        // today — but it keeps the branch symmetric with its
                        // siblings and correct if the coupling ever changes.
                        unregister_inflight(inflight, invocation_id);
                        // #894: persist the channel-closed unwind to the audit
                        // log. Sibling gap to #815 / #893. Without this append,
                        // a closed approval channel (approver disconnected,
                        // gateway shutting down, channel adapter dropped the
                        // receiver) leaves a `tool_start` with no audit row and
                        // operators have to dig through logs to figure out why.
                        // Using `AuditDecision::Deny` mirrors the pattern from
                        // #815 (user denial) and #893 (cancellation) — the
                        // error string is the discriminator.
                        //
                        // The error string mirrors the `Tool '{name}'` prefix
                        // shape used by #815 and #893 so log queries that
                        // filter by tool-name prefix catch all three audit-
                        // Deny paths (Tim's review on PR #925).
                        let closed_reason = format!("Tool '{}' approval channel closed", name);
                        let _ = session_manager.append_audit(
                            session_id,
                            AuditEvent {
                                session_id,
                                run_id: self.run_id,
                                tool: name.to_string(),
                                decision: AuditDecision::Deny,
                                params: args.clone(),
                                result: None,
                                error: Some(closed_reason.clone()),
                                timestamp: alms_core::Timestamp::now(),
                            },
                        );
                        // A2-1 (#1125): emit the matching `ToolEnd` for the
                        // `tool_start` we already fired above. Without this the
                        // frontend spinner sticks — the cancel and deny sibling
                        // branches in this same gate both emit a terminal
                        // `ToolEnd`, and the 1:1 `tool_start`/`tool_end`
                        // invariant (#816/#846) requires it here too. Mirrors
                        // the cancel/deny `ToolEnd` shape field-for-field.
                        let _ = sender.send(RuntimeEvent::ToolEnd {
                            invocation_id,
                            ok: false,
                            result: serde_json::json!({"error": "approval channel closed"}),
                            source_agent: None,
                            task_id: None,
                        });
                        return Err(alms_core::AlmsError::ToolExecution(closed_reason));
                    }
                }
            };
            if !approved {
                unregister_inflight(inflight, invocation_id);
                // #815: persist the denial to the audit log. Without this
                // append, the audit trail is one-sided — it captures every
                // approved tool call but silently drops user-rejected ones,
                // so operators cannot retroactively answer "did I deny X
                // at this time?". Mirror the structure of the approve+execute
                // success path's audit emission, but with `Decision::Deny`
                // and a denial-shaped error string. The deny path for
                // unknown-tools / argument-parse failures earlier in this
                // function already uses `AuditDecision::Deny`; user denials
                // belong in the same bucket — they are a policy decision,
                // not a runtime execution failure (`AuditDecision::Error`).
                let denial_reason = format!("Tool '{}' denied by user", name);
                let _ = session_manager.append_audit(
                    session_id,
                    AuditEvent {
                        session_id,
                        run_id: self.run_id,
                        tool: name.to_string(),
                        decision: AuditDecision::Deny,
                        params: args.clone(),
                        result: None,
                        error: Some(denial_reason),
                        timestamp: alms_core::Timestamp::now(),
                    },
                );
                let _ = sender.send(RuntimeEvent::ToolEnd {
                    invocation_id,
                    ok: false,
                    result: user_denied_result(),
                    source_agent: None,
                    task_id: None,
                });
                // #1109: a denial means "stop" — drive the run to
                // `cancelled` instead of surfacing a tool error the loop
                // keeps iterating on. Cancelling the run's own token lets
                // the existing cancel machinery do the rest: the Guarded
                // inter-tool check (Branch 1 in `run_tool_calls`) unwinds
                // before the next tool can prompt, loop-top Checkpoint A
                // unwinds before the next LLM turn, and the gateway's
                // terminal `Err(Cancelled[WithToolCalls])` arm flips the
                // run to `Cancelled` (#895 / #1046 / #1050). With no token
                // attached (direct unit-test invocations) the loop simply
                // continues with the `user_denied` result below. Also
                // fires for the coordinator's auto-deny of unroutable
                // subagent approvals — the subagent cancels instead of
                // plowing on with every tool denied; the child token does
                // not propagate to the parent.
                if let Some(ref token) = self.cancel_token {
                    token.cancel();
                }
                // `Ok` (not `Err`) so the denial body persists as a real
                // `Tool`-role row (`process_tool_results` /
                // `persist_completed_guarded_results_on_cancel`) — the
                // next run's rebuild replays the explicit `user_denied`
                // signal instead of an `INTERRUPTED_TOOL_RESULT` marker
                // for an orphan tool_use block. `tool_result_ok` maps the
                // body to `ok: false` on the persisted row.
                return Ok(user_denied_result());
            }
        }

        // Execute. Thread the parent's `invocation_id` into the per-call
        // `ToolContext` so `InvokeAgentTool` can carry it to the
        // coordinator, which emits the `subagent_started` SSE event
        // with the parent's `tool_invocation_id` for the UI's
        // SubagentBar resolver (#1105). All other tools fall through
        // the default `Tool::execute_with_context` impl, which
        // discards the context and runs `Tool::execute` unchanged.
        let result = self
            .tools
            .execute_with_context(
                name,
                args.clone(),
                alms_sandbox::ToolContext::new(invocation_id),
            )
            .await;
        let elapsed = start.elapsed();

        // The inner future has finished and we are now in synchronous code
        // that emits the matching ToolEnd. Unregister from the in-flight
        // tracker BEFORE the emission below so the outer cancel arm in
        // `run_tool_calls` cannot synthesise a duplicate ToolEnd.
        //
        // Safety: from this point until the ToolEnd `sender.send(..)` call
        // there are no `await`s, so `tokio::select!` cannot drop this
        // future and the cancel arm cannot fire. (#846 protocol)
        unregister_inflight(inflight, invocation_id);

        match &result {
            Ok(value) => {
                info!(
                    target: "agent::tool::success",
                    agent_id = %self.agent_id.0,
                    tool_name = %name,
                    tool_call_id = %tool_call.id,
                    duration_ms = %elapsed.as_millis(),
                    "Tool execution succeeded"
                );
                // Route the audit-log + ToolEnd SSE payloads through the
                // shared in-loop truncation service (#921 review fix #4).
                // Pre-fix, an oversized tool result hit both the audit log
                // and the SSE channel uncapped — eating audit-log disk and
                // SSE bandwidth even though the in-loop messages vec was
                // already capped by `process_tool_results`. The agent-visible
                // preview, the audit log, and the SSE event now see the same
                // truncated content + spill-path hint.
                //
                // The spill file is written here (and possibly again in
                // `process_tool_results`); the second write is idempotent
                // because both paths use the same deterministic
                // `tool_<sanitized_call_id>.txt` filename and the same raw
                // bytes. The `truncate` service falls through cheaply when
                // the policy is disabled or the value is already inside the
                // caps.
                let emit_value = self.truncate_for_emit(&tool_call.id, value);
                let _ = session_manager.append_audit(
                    session_id,
                    AuditEvent {
                        session_id,
                        run_id: self.run_id,
                        tool: name.to_string(),
                        decision: AuditDecision::Allow,
                        params: args,
                        result: Some(emit_value.clone()),
                        error: None,
                        timestamp: alms_core::Timestamp::now(),
                    },
                );
                let ok = tool_result_ok(value);

                if let Some(ref sender) = self.event_sender {
                    let _ = sender.send(RuntimeEvent::ToolEnd {
                        invocation_id,
                        ok,
                        result: emit_value,
                        source_agent: None,
                        task_id: None,
                    });
                }
            }
            Err(e) => {
                // Surface the classifier-extracted target path (#758) when
                // the error is a `ToolBlocked`, so operators can see *what*
                // was targeted in logs, audit entries, and the UI.
                let blocked_target: Option<String> = match e {
                    AlmsError::ToolBlocked { target, .. } => target.clone(),
                    _ => None,
                };
                error!(
                    target: "agent::tool::error",
                    agent_id = %self.agent_id.0,
                    tool_name = %name,
                    tool_call_id = %tool_call.id,
                    error = %e,
                    blocked_target = ?blocked_target,
                    duration_ms = %elapsed.as_millis(),
                    "Tool execution failed"
                );
                // Expose the structured classifier target on the AuditEvent
                // so downstream audit-log queries can filter on it without
                // regexing the error message. Omit the `result` field entirely
                // when no target is present (don't emit `{"target": null}`).
                let audit_result = blocked_target
                    .as_deref()
                    .map(|t| serde_json::json!({"target": t}));
                // Use `Error` (not `Deny`) to distinguish runtime failures
                // from policy denials in audit log queries.
                //
                // #997: route the audit `error` field through
                // `audit_error_string` (variant dispatch) so a
                // `SubagentLlmError` here — which carries the raw
                // provider response body and can echo prompt content,
                // model output, or API-key-shaped tokens — is collapsed
                // to its status-class category label before it lands in
                // the audit log. All other `AlmsError` variants pass
                // through verbatim, preserving the pre-#997 audit-log
                // shape for operator debugging. Tim's flag on PR #995
                // (https://github.com/alpercodes/alms/pull/995#issuecomment-4395490137).
                let _ = session_manager.append_audit(
                    session_id,
                    AuditEvent {
                        session_id,
                        run_id: self.run_id,
                        tool: name.to_string(),
                        decision: AuditDecision::Error,
                        params: args,
                        result: audit_result,
                        error: Some(audit_error_string(e)),
                        timestamp: alms_core::Timestamp::now(),
                    },
                );
                if let Some(ref sender) = self.event_sender {
                    // Include the structured `target` field in the tool_end
                    // payload so the web UI can render it prominently next
                    // to the error message without string-parsing.
                    let result_json = match &blocked_target {
                        Some(t) => serde_json::json!({
                            "error": e.to_string(),
                            "target": t,
                        }),
                        None => serde_json::json!({"error": e.to_string()}),
                    };
                    let _ = sender.send(RuntimeEvent::ToolEnd {
                        invocation_id,
                        ok: false,
                        result: result_json,
                        source_agent: None,
                        task_id: None,
                    });
                }
            }
        }

        result
    }
}

/// Attach a `reasoning_blocks` array to a message's metadata object.
///
/// The output shape is `{"reasoning_blocks": [{"text": "..."}]}` merged
/// into any existing base metadata. When `reasoning_trace` is `None` or
/// empty the base metadata passes through unchanged — we never write an
/// empty `reasoning_blocks` array, but we also don't drop the caller's
/// other fields (e.g. DM `message_type`/`from_agent`).
///
/// Kept provider-agnostic so that issues #768 (OpenAI / DeepSeek R1 / xAI)
/// and #769 (Gemini) can reuse the same persistence shape. For Anthropic
/// today this stores a single concatenated block; future providers that
/// stream multiple discrete reasoning blocks can push additional entries
/// into the array without a migration.
pub(crate) fn merge_reasoning_blocks(
    base: Option<serde_json::Value>,
    reasoning_trace: Option<&str>,
) -> Option<serde_json::Value> {
    let trace = reasoning_trace.filter(|t| !t.is_empty());
    match (base, trace) {
        (None, None) => None,
        (Some(b), None) => Some(b),
        (base, Some(text)) => {
            let blocks = serde_json::json!([{"text": text}]);
            // Invariant: callers today always pass either `None` or
            // `Some(Value::Object(..))` (see `dm_reasoning_metadata` in
            // `dm.rs`, which is the only producer of non-`None` bases).
            // The non-Object fall-through below is defensive "drop + rebuild"
            // — if that ever fires we'd silently lose caller-supplied
            // metadata, so pin the invariant in debug builds so a future
            // caller mistake trips tests rather than corrupts persistence.
            debug_assert!(
                matches!(base, None | Some(serde_json::Value::Object(_))),
                "merge_reasoning_blocks: non-Object base is unreachable today \
                 — only `dm_reasoning_metadata` feeds this path and it always \
                 returns Some(Object(..))"
            );
            match base {
                Some(serde_json::Value::Object(mut map)) => {
                    map.insert("reasoning_blocks".to_string(), blocks);
                    Some(serde_json::Value::Object(map))
                }
                _ => Some(serde_json::json!({"reasoning_blocks": blocks})),
            }
        }
    }
}

#[cfg(test)]
mod reasoning_tests {
    use super::merge_reasoning_blocks;

    #[test]
    fn test_merge_reasoning_blocks_none_when_empty_trace_and_no_base() {
        assert!(merge_reasoning_blocks(None, None).is_none());
        assert!(merge_reasoning_blocks(None, Some("")).is_none());
    }

    #[test]
    fn test_merge_reasoning_blocks_passes_through_base_when_no_trace() {
        // Regression guard: the DM metadata path calls this with
        // `Some({message_type, from_agent, run_id})` and no reasoning.
        // The base metadata must survive verbatim.
        let base = serde_json::json!({"message_type": "reasoning", "from_agent": "bob"});
        let result = merge_reasoning_blocks(Some(base.clone()), None).unwrap();
        assert_eq!(result, base);
        let result_empty = merge_reasoning_blocks(Some(base.clone()), Some("")).unwrap();
        assert_eq!(result_empty, base);
    }

    #[test]
    fn test_merge_reasoning_blocks_creates_object() {
        let meta = merge_reasoning_blocks(None, Some("thinking...")).unwrap();
        let blocks = meta.get("reasoning_blocks").unwrap().as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].get("text").unwrap().as_str(), Some("thinking..."));
    }

    #[test]
    fn test_merge_reasoning_blocks_preserves_existing_meta() {
        let base = serde_json::json!({"message_type": "reasoning", "from_agent": "atlas"});
        let meta = merge_reasoning_blocks(Some(base), Some("step 1")).unwrap();
        assert_eq!(
            meta.get("message_type").unwrap().as_str(),
            Some("reasoning")
        );
        assert_eq!(meta.get("from_agent").unwrap().as_str(), Some("atlas"));
        let blocks = meta.get("reasoning_blocks").unwrap().as_array().unwrap();
        assert_eq!(blocks[0].get("text").unwrap().as_str(), Some("step 1"));
    }
}

#[cfg(test)]
mod finalize_content_tests {
    use super::finalize_content_and_reasoning;

    /// `[Text]`-only turn: visible content passes through, no reasoning.
    #[test]
    fn visible_text_only_passes_content_through() {
        let (content, reasoning) =
            finalize_content_and_reasoning("hello".to_string(), String::new(), false);
        assert_eq!(content.as_deref(), Some("hello"));
        assert!(reasoning.is_none());
    }

    /// `[Text, ToolUse]`: visible content passes through; reasoning is
    /// absent because none was streamed. `has_tool_calls=true` does not
    /// change the outcome when `content` is non-empty.
    #[test]
    fn visible_text_with_tool_use_passes_content_through() {
        let (content, reasoning) =
            finalize_content_and_reasoning("thinking out loud".to_string(), String::new(), true);
        assert_eq!(content.as_deref(), Some("thinking out loud"));
        assert!(reasoning.is_none());
    }

    /// `[Text + Thinking]` (Anthropic extended-thinking supplement path):
    /// visible content stays in `content`, reasoning is surfaced on the
    /// separate sideband.
    #[test]
    fn visible_text_plus_reasoning_keeps_reasoning_sideband() {
        let (content, reasoning) = finalize_content_and_reasoning(
            "final answer".to_string(),
            "step 1... step 2...".to_string(),
            false,
        );
        assert_eq!(content.as_deref(), Some("final answer"));
        assert_eq!(reasoning.as_deref(), Some("step 1... step 2..."));
    }

    /// `[Text + Thinking, ToolUse]`: same as above — visible text wins,
    /// reasoning stays on the sideband. Tool-call presence is irrelevant
    /// because content is non-empty.
    #[test]
    fn visible_text_plus_reasoning_with_tool_use_keeps_reasoning_sideband() {
        let (content, reasoning) = finalize_content_and_reasoning(
            "ok".to_string(),
            "thinking".to_string(),
            /* has_tool_calls */ true,
        );
        assert_eq!(content.as_deref(), Some("ok"));
        assert_eq!(reasoning.as_deref(), Some("thinking"));
    }

    /// `[Thinking]`-only turn (reasoning model exhausted max_tokens before
    /// emitting visible output): reasoning is promoted into `content` so
    /// the run still has something to surface, AND the same trace is kept
    /// on the reasoning sideband so the gateway can detect this case (per
    /// #1098 — the dual surface is the signal that lets the episodic
    /// summarizer drop the response and avoid ingesting reasoning text).
    #[test]
    fn thinking_only_promotes_reasoning_into_content() {
        let (content, reasoning) =
            finalize_content_and_reasoning(String::new(), "long deliberation".to_string(), false);
        assert_eq!(content.as_deref(), Some("long deliberation"));
        assert_eq!(
            reasoning.as_deref(),
            Some("long deliberation"),
            "fallback must also surface reasoning on the sideband so the gateway can identify reasoning-as-response (#1098)"
        );
    }

    /// `[Thinking, ToolUse]` with empty visible text (#776 regression):
    /// reasoning must NOT be promoted into `content` — doing so would
    /// launder thinking text into the visible channel, which the loop
    /// would replay as assistant content on the next turn, violating
    /// the #767 invariant that reasoning is never replayed. Instead,
    /// reasoning stays on the sideband and the ToolUse carries the turn.
    #[test]
    fn thinking_plus_tool_use_empty_text_does_not_promote() {
        let (content, reasoning) = finalize_content_and_reasoning(
            String::new(),
            "secret chain of thought".to_string(),
            /* has_tool_calls */ true,
        );
        assert!(
            content.is_none(),
            "reasoning must not be laundered into content when tool_calls are present"
        );
        assert_eq!(
            reasoning.as_deref(),
            Some("secret chain of thought"),
            "reasoning is preserved on the sideband for metadata persistence"
        );
    }

    /// Fully empty stream: both fields are `None`.
    #[test]
    fn fully_empty_stream_returns_none_none() {
        let (content, reasoning) =
            finalize_content_and_reasoning(String::new(), String::new(), false);
        assert!(content.is_none());
        assert!(reasoning.is_none());
    }

    /// Fully empty stream with a tool call (rare but possible: model
    /// emits only a ToolUse block with no thinking and no text): still
    /// `None`/`None`. The tool call itself lives on the `tool_calls`
    /// field and is unaffected by this helper.
    #[test]
    fn empty_stream_with_tool_use_returns_none_none() {
        let (content, reasoning) =
            finalize_content_and_reasoning(String::new(), String::new(), true);
        assert!(content.is_none());
        assert!(reasoning.is_none());
    }
}

#[cfg(test)]
mod buffered_fallback_reconcile_tests {
    use super::buffered_fallback_reconcile_events;
    use crate::events::RuntimeEvent;

    /// The failed stream painted nothing (no delta emitted, or no event
    /// sender): no reset and no re-emit — there is no partial to retract and a
    /// re-stream would surface text a clean stream never did. (#1162 sym-2)
    #[test]
    fn no_emission_yields_no_events() {
        let events =
            buffered_fallback_reconcile_events(false, Some("Hello Alice!"), Some("thinking"));
        assert!(
            events.is_empty(),
            "no partial was painted, so nothing must be retracted or re-emitted"
        );
    }

    /// The classic minimax-m3 shape (#1162 sym-2): the stream emitted a
    /// partial, then faulted; the buffered retry returns distinct visible
    /// content AND a reasoning trace. The reconciliation is
    /// `StreamReset` → `ReasoningDelta(full)` → `TokenDelta(full)` so the live
    /// render is rebuilt exactly as a clean stream would have produced it
    /// (reasoning into the collapsible first, then the visible reply).
    #[test]
    fn emission_with_content_and_reasoning_resets_then_reemits_in_order() {
        let events =
            buffered_fallback_reconcile_events(true, Some("Hello Alice!"), Some("pondering"));
        assert_eq!(events.len(), 3, "reset + reasoning + content");
        assert!(
            matches!(events[0], RuntimeEvent::StreamReset { source_agent: None }),
            "the partial is retracted first"
        );
        assert!(
            matches!(&events[1], RuntimeEvent::ReasoningDelta { text, source_agent: None } if text == "pondering"),
            "reasoning is re-emitted before content (collapsible first)"
        );
        assert!(
            matches!(&events[2], RuntimeEvent::TokenDelta { delta, source_agent: None } if delta == "Hello Alice!"),
            "the full visible reply is re-emitted last"
        );
    }

    /// A reply-only buffered result (no reasoning trace): reset + the visible
    /// reply, no empty `ReasoningDelta`.
    #[test]
    fn emission_with_content_only_resets_then_reemits_content() {
        let events = buffered_fallback_reconcile_events(true, Some("just the reply"), None);
        assert_eq!(events.len(), 2, "reset + content");
        assert!(matches!(events[0], RuntimeEvent::StreamReset { .. }));
        assert!(
            matches!(&events[1], RuntimeEvent::TokenDelta { delta, .. } if delta == "just the reply")
        );
    }

    /// Empty `content` / `reasoning` strings are never re-emitted as
    /// zero-length deltas — only the reset fires. (A reasoning-as-response
    /// promotion that the gateway folds without a `dm_message` still resets
    /// the abandoned partial; it just has nothing to re-stream here.)
    #[test]
    fn empty_strings_emit_only_the_reset() {
        let events = buffered_fallback_reconcile_events(true, Some(""), Some(""));
        assert_eq!(events.len(), 1, "only the reset — no zero-length deltas");
        assert!(matches!(events[0], RuntimeEvent::StreamReset { .. }));

        let events_none = buffered_fallback_reconcile_events(true, None, None);
        assert_eq!(events_none.len(), 1, "only the reset when both are None");
        assert!(matches!(events_none[0], RuntimeEvent::StreamReset { .. }));
    }
}

#[cfg(test)]
mod stream_error_classification_tests {
    use super::stream_error_is_timeout;
    use alms_core::AlmsError;

    /// Reported #1163 case: reqwest's total `.timeout()` tripped mid-body →
    /// `operation timed out`. Timeout-class.
    #[test]
    fn reqwest_total_deadline_timeout_is_timeout_class() {
        let err = AlmsError::Runtime(
            "LLM stream decode failed [provider=openrouter model=minimax/minimax-m3 \
             bytes_read=221629]: error decoding response body: request or response \
             body error: operation timed out"
                .to_string(),
        );
        assert!(stream_error_is_timeout(&err));
    }

    /// A streaming per-chunk stall (`LLM stream stalled … partial response
    /// discarded`) is **not** the total-timeout class — it can recover on a
    /// non-streaming re-issue, so it must NOT short-circuit (Codex P2). Note it
    /// carries no `operation timed out` phrase.
    #[test]
    fn synthetic_per_chunk_stall_is_not_timeout_class() {
        let err = AlmsError::Runtime(
            "LLM stream stalled [provider=openrouter model=minimax/minimax-m3 \
             bytes_read=4096] (no data for 180s) — partial response discarded"
                .to_string(),
        );
        assert!(!stream_error_is_timeout(&err));
    }

    /// A `send()`-phase (header-wait) timeout → `HTTP request failed: …
    /// operation timed out`. Timeout-class.
    #[test]
    fn send_phase_timeout_is_timeout_class() {
        let err = AlmsError::Runtime(
            "HTTP request failed: error sending request for url (https://openrouter.ai): \
             operation timed out"
                .to_string(),
        );
        assert!(stream_error_is_timeout(&err));
    }

    /// A mid-stream connection reset is a *decode* fault — keep the fallback.
    #[test]
    fn connection_reset_is_decode_class() {
        let err = AlmsError::Runtime(
            "LLM stream decode failed [provider=openrouter model=minimax/minimax-m3 \
             bytes_read=10]: error decoding response body: error reading a body from \
             connection: connection reset by peer"
                .to_string(),
        );
        assert!(!stream_error_is_timeout(&err));
    }

    /// A malformed/truncated JSON body (#1162 sym-2): decode-class, keep the
    /// fallback.
    #[test]
    fn malformed_json_parse_is_decode_class() {
        let err = AlmsError::Runtime(
            "LLM response parse failed (OpenAI) [provider=openai model=gpt-4o status=200]: \
             expected value at line 1 column 1"
                .to_string(),
        );
        assert!(!stream_error_is_timeout(&err));
    }

    /// Empty-choices is decode-class (non-timeout) — must not short-circuit.
    #[test]
    fn empty_choices_is_decode_class() {
        let err = AlmsError::Runtime("LLM returned empty choices array".to_string());
        assert!(!stream_error_is_timeout(&err));
    }

    /// Anchor precision: a decode fault whose `body_prefix` merely contains the
    /// word "timeout" stays non-timeout (the anchor is the exact `operation
    /// timed out` phrase).
    #[test]
    fn decode_fault_with_timeout_word_in_body_prefix_is_decode_class() {
        let err = AlmsError::Runtime(
            "LLM stream decode failed [provider=openrouter model=minimax/minimax-m3 \
             bytes_read=42]: error decoding response body: connection reset by peer; \
             body_prefix=\"data: {\\\"content\\\":\\\"the request will timeout soon\\\"}\""
                .to_string(),
        );
        assert!(!stream_error_is_timeout(&err));
    }

    /// Codex P2 on #1177: a decode fault (connection reset) whose `body_prefix`
    /// carries the **exact** phrase `operation timed out` — the model was
    /// discussing timeouts — must stay non-timeout so the recoverable buffered
    /// fallback is NOT skipped. The transport portion (before `body_prefix=`)
    /// has no timeout, so stripping it is what makes this `false`; the test
    /// fails if the strip is removed.
    #[test]
    fn decode_fault_with_exact_timeout_phrase_in_body_prefix_is_not_timeout_class() {
        let err = AlmsError::Runtime(
            "LLM stream decode failed [provider=openrouter model=minimax/minimax-m3 \
             bytes_read=64]: error decoding response body: error reading a body from \
             connection: connection reset by peer; \
             body_prefix=\"data: {\\\"content\\\":\\\"the operation timed out, i think\\\"}\""
                .to_string(),
        );
        assert!(!stream_error_is_timeout(&err));
    }

    /// Codex P2 on #1177: a non-2xx provider response is
    /// `AlmsError::SubagentLlmError`, whose `Display` carries the raw provider
    /// body. Even if that body says `operation timed out` (a 504 / proxy timeout
    /// page) it must classify NOT-timeout so the buffered fallback still runs —
    /// only `AlmsError::Runtime` transport errors may short-circuit. Fails if the
    /// variant gate is removed.
    #[test]
    fn subagent_llm_error_with_timeout_phrase_in_body_is_not_timeout_class() {
        let err = AlmsError::subagent_llm_error("openrouter", 504, "upstream operation timed out");
        assert!(!stream_error_is_timeout(&err));
    }
}

#[cfg(test)]
mod inactivity_timer_tests {
    use super::{
        ActivityClock, Posture, RunPhase, batch_has_blocking_invoke_agent, batch_needs_approval,
        invoke_agent_call_is_background, stall_error,
    };
    use crate::llm_types::ToolCall;
    use std::time::Duration;

    /// Below the budget, no phase trips — `stall_error` returns `None` so the
    /// loop keeps running. Covers all three phase labels since the decision is
    /// purely `idle < budget`.
    #[test]
    fn under_budget_never_trips() {
        for phase in [
            RunPhase::AwaitingFirstActivity,
            RunPhase::BetweenIterations,
            RunPhase::ExecutingTools,
        ] {
            assert!(
                stall_error(phase, Duration::from_secs(5), 180).is_none(),
                "{phase:?}: idle (5s) < budget (180s) must not trip"
            );
        }
    }

    /// At or past the budget the phase trips, and the message embeds both the
    /// idle seconds and the phase label so the session sanitiser can map it to
    /// the distinct "stalled" label.
    #[test]
    fn at_or_over_budget_trips_with_labelled_message() {
        // Exactly at the budget trips (`idle.as_secs() < budget` is false).
        let at = stall_error(RunPhase::BetweenIterations, Duration::from_secs(180), 180)
            .expect("idle == budget must trip");
        assert!(at.contains("stalled"), "message must say 'stalled': {at}");
        assert!(at.contains("180s"), "message must embed idle seconds: {at}");
        assert!(
            at.contains("between iterations"),
            "message must embed the P1 phase label: {at}"
        );

        // Past the budget trips with the larger idle figure.
        let over = stall_error(RunPhase::ExecutingTools, Duration::from_secs(750), 600)
            .expect("idle > budget must trip");
        assert!(
            over.contains("750s"),
            "message must embed idle seconds: {over}"
        );
        assert!(
            over.contains("executing tools"),
            "message must embed the P3 phase label: {over}"
        );

        // P0 phase label flows through too (derived budget, here passed
        // explicitly as the derived value would be).
        let p0 = stall_error(RunPhase::AwaitingFirstActivity, Duration::from_secs(90), 90)
            .expect("P0 idle == derived budget must trip");
        assert!(
            p0.contains("awaiting the first response"),
            "message must embed the P0 phase label: {p0}"
        );
    }

    /// A `0` budget is the documented escape hatch: the phase is disabled and
    /// never trips, no matter how large the idle window — matching the
    /// `value > 0` gate the loop's other hard caps use. Locks the semantics so
    /// a refactor can't turn `0` into an instant trip.
    #[test]
    fn zero_budget_disables_the_phase() {
        assert!(
            stall_error(RunPhase::BetweenIterations, Duration::from_secs(86_400), 0).is_none(),
            "a 0 budget must disable the phase even after a full day idle"
        );
        assert!(
            stall_error(
                RunPhase::ExecutingTools,
                Duration::from_secs(u64::MAX / 2),
                0
            )
            .is_none(),
            "a 0 budget disables P3 regardless of idle"
        );
        // The blocking-subagent phase (P3b) is always unbounded: `agent_loop`
        // feeds it `inactivity_budget(ExecutingBlockingSubagent) == 0`, so the
        // parent never stall-fails on a blocking foreground `invoke_agent`,
        // however long the subagent runs. (#1150)
        assert!(
            stall_error(
                RunPhase::ExecutingBlockingSubagent,
                Duration::from_secs(u64::MAX / 2),
                0
            )
            .is_none(),
            "the blocking-subagent phase must never trip (unbounded budget)"
        );
        // The approval-wait phase (P3c) is likewise unbounded: `agent_loop`
        // feeds it `inactivity_budget(AwaitingApproval) == 0`, so a Guarded run
        // blocked on human approval never stall-fails however long the human
        // takes to decide. (#1150)
        assert!(
            stall_error(
                RunPhase::AwaitingApproval,
                Duration::from_secs(u64::MAX / 2),
                0
            )
            .is_none(),
            "the approval-wait phase must never trip (unbounded budget)"
        );
    }

    /// A foreground `invoke_agent` in the batch makes it "blocking" — the batch
    /// cannot return until the subagent completes, so it must be excluded from
    /// the P3 ceiling. Absent / `false` / unparseable `background` all count as
    /// foreground (the safe direction). (#1150)
    #[test]
    fn foreground_invoke_agent_batch_is_blocking() {
        for args in [
            r#"{"task":"x"}"#,                     // background absent
            r#"{"task":"x","background":false}"#,  // explicit false
            r#"{"task":"x","background":"true"}"#, // non-bool -> foreground
            "{}",                                  // empty object
            "",                                    // empty / no args
            "not json",                            // unparseable -> foreground
        ] {
            let calls = vec![ToolCall::new("c1", "invoke_agent", args)];
            assert!(
                batch_has_blocking_invoke_agent(&calls, &[]),
                "args {args:?} must classify as a blocking foreground invoke_agent"
            );
        }
    }

    /// A background (`background: true`) `invoke_agent` returns immediately, so
    /// it does NOT make the batch blocking — the batch stays in the normal P3
    /// phase.
    #[test]
    fn background_invoke_agent_batch_is_not_blocking() {
        let calls = vec![ToolCall::new(
            "c1",
            "invoke_agent",
            r#"{"task":"x","background":true}"#,
        )];
        assert!(!batch_has_blocking_invoke_agent(&calls, &[]));
    }

    /// A batch with no `invoke_agent` is never blocking, however long its tools
    /// run — that is exactly what the P3 ceiling is for.
    #[test]
    fn non_invoke_agent_batch_is_not_blocking() {
        let calls = vec![
            ToolCall::new("c1", "echo", r#"{"message":"hi"}"#),
            ToolCall::new("c2", "sleep_tool", "{}"),
        ];
        assert!(!batch_has_blocking_invoke_agent(&calls, &[]));
    }

    /// A foreground `invoke_agent` alongside other tools still makes the whole
    /// batch blocking — `run_tool_calls` does not return until every tool
    /// (including the subagent) completes, so the P3 ceiling can never bound it.
    #[test]
    fn mixed_batch_with_foreground_invoke_agent_is_blocking() {
        let calls = vec![
            ToolCall::new("c1", "echo", r#"{"message":"hi"}"#),
            ToolCall::new("c2", "invoke_agent", r#"{"task":"x"}"#),
        ];
        assert!(batch_has_blocking_invoke_agent(&calls, &[]));
    }

    /// A DM-conflicting `invoke_agent` slot will not execute, so it must not
    /// count as a blocking call — mirrors the executing-set filter the loop
    /// uses for the status emit.
    #[test]
    fn conflicting_foreground_invoke_agent_is_excluded() {
        let calls = vec![ToolCall::new("c1", "invoke_agent", r#"{"task":"x"}"#)];
        assert!(!batch_has_blocking_invoke_agent(&calls, &[0]));
    }

    /// The background-flag parse matches `InvokeAgentTool`'s own
    /// (`background` must be a literal boolean `true`); everything else is
    /// foreground.
    #[test]
    fn background_flag_parse_matches_invoke_agent_tool() {
        assert!(invoke_agent_call_is_background(r#"{"background":true}"#));
        assert!(!invoke_agent_call_is_background(r#"{"background":false}"#));
        assert!(!invoke_agent_call_is_background(r#"{"background":"true"}"#));
        assert!(!invoke_agent_call_is_background(r#"{"background":1}"#));
        assert!(!invoke_agent_call_is_background(r#"{"task":"x"}"#));
        assert!(!invoke_agent_call_is_background("not json"));
        assert!(!invoke_agent_call_is_background(""));
    }

    /// A realistic auto-approved set for the predicate tests — mirrors the
    /// inherently-safe tools the sandbox auto-approves (`echo`, `datetime`,
    /// read-only tools). `shell` / `fs_write` are NOT auto-approved and so
    /// route through the Guarded approval gate.
    fn is_auto_approved(name: &str) -> bool {
        matches!(name, "echo" | "datetime" | "read_session")
    }

    /// Regression (#1150): a Guarded-posture run whose tool batch blocks on
    /// **human approval** must NOT stall-fail when the human takes longer than
    /// the P3 tool-phase ceiling to approve. The batch is classified as needing
    /// approval, so the call site selects the unbounded `AwaitingApproval`
    /// phase (budget 0) and an idle window far past `tool_phase_ceiling_secs`
    /// (here an hour, well over the 900s default) still never trips. The 24h
    /// `max_run_duration_secs` backstop still bounds a truly-abandoned
    /// approval. Mirrors the foreground-`invoke_agent` exemption.
    #[test]
    fn guarded_approval_wait_past_p3_ceiling_does_not_stall() {
        // A Guarded batch with a gated (non-auto-approved) tool needs approval.
        let calls = vec![ToolCall::new("c1", "shell", r#"{"command":"ls"}"#)];
        assert!(
            batch_needs_approval(Posture::Guarded, &calls, &[], is_auto_approved),
            "a Guarded batch with a gated tool must route through the approval gate"
        );
        // So the run sits in the unbounded AwaitingApproval phase (budget 0),
        // and an approval wait an hour past the 900s P3 ceiling does not stall.
        assert!(
            stall_error(RunPhase::AwaitingApproval, Duration::from_secs(3600), 0).is_none(),
            "an approval wait an hour past the P3 ceiling must not stall-fail"
        );
    }

    /// A Guarded batch of only auto-approved tools never blocks on a human, so
    /// it does NOT need approval and stays under the normal P3 ceiling.
    #[test]
    fn guarded_batch_of_auto_approved_tools_does_not_need_approval() {
        let calls = vec![
            ToolCall::new("c1", "echo", r#"{"message":"hi"}"#),
            ToolCall::new("c2", "datetime", "{}"),
        ];
        assert!(!batch_needs_approval(
            Posture::Guarded,
            &calls,
            &[],
            is_auto_approved
        ));
    }

    /// Only Guarded posture has a human-approval gate; FullControl and
    /// Autonomous execute tools without approval, so their batches are never
    /// approval-blocked and stay under the normal P3 ceiling — even with a
    /// gated tool present.
    #[test]
    fn non_guarded_postures_never_need_approval() {
        let calls = vec![ToolCall::new("c1", "shell", r#"{"command":"ls"}"#)];
        for posture in [Posture::FullControl, Posture::Autonomous] {
            assert!(
                !batch_needs_approval(posture, &calls, &[], is_auto_approved),
                "{posture:?} has no approval gate, so the batch must not need approval"
            );
        }
    }

    /// A mixed Guarded batch needs approval as soon as *one* executing call is
    /// gated — exactly the gate's per-call decision applied across the batch.
    #[test]
    fn guarded_mixed_batch_needs_approval_if_any_tool_is_gated() {
        let calls = vec![
            ToolCall::new("c1", "echo", r#"{"message":"hi"}"#), // auto-approved
            ToolCall::new("c2", "fs_write", r#"{"path":"a","content":"b"}"#), // gated
        ];
        assert!(batch_needs_approval(
            Posture::Guarded,
            &calls,
            &[],
            is_auto_approved
        ));
    }

    /// A DM-conflicting gated slot will not execute, so it must not count as
    /// needing approval — mirrors the executing-set filter the loop uses for
    /// the status emit and `batch_has_blocking_invoke_agent`.
    #[test]
    fn conflicting_gated_tool_is_excluded_from_approval() {
        let calls = vec![ToolCall::new("c1", "shell", r#"{"command":"ls"}"#)];
        assert!(!batch_needs_approval(
            Posture::Guarded,
            &calls,
            &[0],
            is_auto_approved
        ));
    }

    /// The clock starts "now" (≈0 idle) and `touch` resets the idle window —
    /// the mechanism that lets a long-but-productive run (steady token /
    /// reasoning / tool-start signals) avoid tripping the phase timer.
    #[tokio::test]
    async fn touch_resets_idle_window() {
        let clock = ActivityClock::new();
        // Fresh clock: effectively no idle time has accumulated.
        assert!(
            clock.idle() < Duration::from_secs(1),
            "a fresh clock must report ~0 idle"
        );

        // Let some idle accrue, then touch and confirm the window collapsed.
        tokio::time::sleep(Duration::from_millis(40)).await;
        let before = clock.idle();
        clock.touch();
        let after = clock.idle();
        assert!(
            after < before,
            "touch must shrink the idle window (before={before:?}, after={after:?})"
        );
        assert!(
            after < Duration::from_millis(20),
            "after touch the idle window must be near-zero, got {after:?}"
        );
    }
}
