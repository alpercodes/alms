use crate::events::{PHASE_CALLING_LLM, PHASE_EXECUTING_TOOLS, RuntimeEvent, RuntimeEventSender};
use crate::llm_types::*;
use alms_core::{AlmsError, AlmsResult, AuditDecision, AuditEvent, TokenUsage, audit_error_string};
use alms_session::{
    Content as SessionContent, Message as SessionMessage, Role as SessionRole, SessionManager,
};
use std::collections::HashMap;
use std::sync::Mutex;
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

use super::AgentRuntime;
use super::dm::{
    DM_CONFLICT_MSG, DM_TEXT_ONLY_MAX_RETRIES, DM_TEXT_ONLY_RETRY_MSG, detect_dm_conflict,
    dm_tool_was_called, should_terminate_after_dm_send,
};
use super::helpers::tool_result_ok;
use super::types::Posture;

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

impl AgentRuntime {
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
        // Tracks how many times we have retried after a DM text-only response.
        // Capped at DM_TEXT_ONLY_MAX_RETRIES to prevent infinite loops.
        let mut dm_text_only_retries: u32 = 0;

        loop {
            // Checkpoint A: check cancellation between iterations.
            if let Some(ref token) = self.cancel_token
                && token.is_cancelled()
            {
                info!(agent_id = %self.agent_id.0, "Run cancelled by user");
                return (tool_call_records, Err(AlmsError::Cancelled));
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
            } = match self.call_llm_with_cancellation(request).await {
                Ok(result) => result,
                Err(e) => return (tool_call_records, Err(e)),
            };

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

                // Pre-execution conflict detection: send_message and
                // ignore_message are mutually exclusive. If both appear in
                // the same tool-call batch, execute neither -- return error
                // results for both so the LLM can retry with just one.
                // Other non-conflicting tools in the batch still execute
                // normally. (Fixes #364)
                let dm_check = detect_dm_conflict(&tool_calls);
                if dm_check.conflict {
                    warn!(
                        "Agent called both send_message and ignore_message in same batch -- \
                         rejecting both; the agent will retry with one"
                    );
                }

                // Emit status: list only the tool names that will actually
                // execute (exclude conflicting tools so SSE subscribers do
                // not see rejected tools listed as "executing").
                let tool_names: Vec<&str> = tool_calls
                    .iter()
                    .map(|tc| tc.function.name.as_str())
                    .filter(|name| !dm_check.conflicting_tools.contains(name))
                    .collect();
                if !tool_names.is_empty() {
                    let detail = tool_names.join(", ");
                    self.emit_status(PHASE_EXECUTING_TOOLS, Some(&detail));
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
                        dm_check.conflicting_tools,
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

                // In a DM-triggered run, terminate the loop after the agent
                // has successfully called `send_message`.  The reply has been
                // delivered; re-entering the loop would let the LLM call
                // `send_message` again, producing duplicate messages and a
                // cascade of RunTrigger events (#407 Bug 1).
                if should_terminate_after_dm_send(&tool_calls, is_dm, dm_check.conflict) {
                    info!("DM run: send_message delivered -- ending loop (one reply per DM run)");
                    let text = content.unwrap_or_default();
                    return (
                        tool_call_records,
                        Ok(AgentLoopOutput {
                            response: text,
                            usage: total_usage,
                            // The reasoning for this turn was already
                            // persisted alongside its tool calls.
                            reasoning: None,
                        }),
                    );
                }

                // Append tool_loop instructions to the system prompt for
                // subsequent iterations. The agent's identity (initial prompt +
                // workspace prefix) is preserved; tool_loop adds continuation
                // guidance on top.
                //
                // For DM sessions, re-inject the DM recipient addendum so the
                // agent remembers to use `send_message` on every iteration --
                // not just the first one (fixes #346).
                self.rebuild_system_prompt_for_tool_loop(&mut messages, include_user, dm_peer);

                continue;
            }

            // --- DM text-only response retry (#361) ---
            //
            // When a DM-triggered run ends with a text-only response and
            // neither `send_message` nor `ignore_message` was called during
            // the entire run, the agent's response will be silently dropped
            // (by design -- DM responses must go through `send_message`).
            //
            // Instead of accepting this silently, re-invoke the LLM with an
            // error message so it gets one more chance to use the correct
            // tool. We cap retries at DM_TEXT_ONLY_MAX_RETRIES to avoid
            // infinite loops.
            if is_dm
                && !dm_tool_was_called(&tool_call_records)
                && dm_text_only_retries < DM_TEXT_ONLY_MAX_RETRIES
            {
                dm_text_only_retries += 1;
                warn!(
                    agent_id = %self.agent_id.0,
                    retry = dm_text_only_retries,
                    "DM run ended with text-only response -- retrying with error prompt"
                );

                // Emit a warning event so the operator/UI is aware.
                if let Some(ref tx) = self.event_sender {
                    let _ = tx.send(crate::events::RuntimeEvent::Warning {
                        code: "DM_TEXT_ONLY_RETRY".to_string(),
                        message: "Agent responded with text only in a DM session. \
                                  Text responses are not delivered -- retrying with \
                                  instructions to use send_message or ignore_message."
                            .to_string(),
                        source_agent: None,
                    });
                }

                // Push the agent's text response as an assistant message so
                // the LLM sees what it said, then append the error as a user
                // message so it knows what went wrong.
                if let Some(ref text) = content
                    && !text.is_empty()
                {
                    messages.push(LlmMessage {
                        role: "assistant".to_string(),
                        content: Some(text.clone()),
                        reasoning_content: None,
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
                messages.push(LlmMessage::user(DM_TEXT_ONLY_RETRY_MSG));

                // Re-inject the DM addendum into the system prompt so the
                // agent is reminded of the tool requirement.
                self.rebuild_system_prompt_for_tool_loop(&mut messages, include_user, dm_peer);

                continue;
            }

            // If this is a DM run and the retry was exhausted without the
            // agent calling send_message/ignore_message, the text response
            // will be silently dropped by finish_run. Emit a warning so
            // the operator has visibility into the failure.
            if is_dm
                && !dm_tool_was_called(&tool_call_records)
                && dm_text_only_retries >= DM_TEXT_ONLY_MAX_RETRIES
            {
                warn!(
                    agent_id = %self.agent_id.0,
                    retries = dm_text_only_retries,
                    "DM text-only retry exhausted -- response will be dropped"
                );
                if let Some(ref tx) = self.event_sender {
                    let _ = tx.send(crate::events::RuntimeEvent::Warning {
                        code: "DM_TEXT_ONLY_DROPPED".to_string(),
                        message: "Agent failed to use send_message or ignore_message \
                                  after retry. The text-only response has been dropped."
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
    async fn call_llm_with_cancellation(
        &self,
        request: CompletionRequest,
    ) -> AlmsResult<StreamCallResult> {
        self.emit_status(PHASE_CALLING_LLM, None);

        // Try streaming first.
        let stream_result = if let Some(ref token) = self.cancel_token {
            tokio::select! {
                result = self.stream_llm_call(request.clone()) => result,
                _ = token.cancelled() => return Err(AlmsError::Cancelled),
            }
        } else {
            self.stream_llm_call(request.clone()).await
        };

        match stream_result {
            Ok(result) => Ok(result),
            Err(e) => {
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
        conflicting_tools: &[&str],
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
                for (tc, &inv_id) in tool_calls.iter().zip(invocation_ids) {
                    if conflicting_tools.contains(&tc.function.name.as_str()) {
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
                    conflicting_tools,
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
        conflicting_tools: &[&str],
        session_manager: &SessionManager,
        session_id: alms_core::SessionId,
        is_dm: bool,
        tool_call_records: &mut Vec<alms_core::ToolCallRecord>,
        tool_seq: &mut u32,
        inflight: &InflightTracker,
    ) -> AlmsResult<Vec<AlmsResult<serde_json::Value>>> {
        use futures::stream::{FuturesUnordered, StreamExt};

        // Indices (into `tool_calls`) of non-conflicting tools to execute.
        let exec_indices: Vec<usize> = tool_calls
            .iter()
            .enumerate()
            .filter(|(_, tc)| !conflicting_tools.contains(&tc.function.name.as_str()))
            .map(|(i, _)| i)
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
        let results: Vec<AlmsResult<serde_json::Value>> = tool_calls
            .iter()
            .map(|tc| {
                if conflicting_tools.contains(&tc.function.name.as_str()) {
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
                            "BUG: slot exhausted -- conflicting_tools filter \
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
    pub(crate) async fn stream_llm_call(
        &self,
        request: CompletionRequest,
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
                if let Some(ref sender) = self.event_sender {
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
                if let Some(ref sender) = self.event_sender {
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
