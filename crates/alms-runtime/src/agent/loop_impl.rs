use crate::events::{PHASE_CALLING_LLM, PHASE_EXECUTING_TOOLS, RuntimeEvent};
use crate::llm_types::*;
use alms_core::{AlmsError, AlmsResult, AuditDecision, AuditEvent, TokenUsage};
use alms_session::{
    Content as SessionContent, Message as SessionMessage, Role as SessionRole, SessionManager,
};
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

use super::AgentRuntime;
use super::dm::{
    DM_CONFLICT_MSG, DM_TEXT_ONLY_MAX_RETRIES, DM_TEXT_ONLY_RETRY_MSG, detect_dm_conflict,
    dm_tool_was_called, should_terminate_after_dm_send,
};
use super::helpers::tool_result_ok;
use super::types::Posture;

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
    // emitting visible content). Promote so the run still has an answer.
    info!("Streaming: content empty, falling back to reasoning_content");
    (Some(reasoning_content), None)
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
                let results = match self
                    .run_tool_calls(
                        &tool_calls,
                        &invocation_ids,
                        dm_check.conflicting_tools,
                        session_manager,
                        session_id,
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
    ///   via `join_all`. Cancellation races against the entire batch.
    ///
    /// Conflicting tools (from DM conflict detection) receive error results
    /// instead of executing.
    ///
    /// Returns `Err(AlmsError::Cancelled)` if the run is cancelled during
    /// execution; otherwise returns the result vector in tool_calls order.
    async fn run_tool_calls(
        &self,
        tool_calls: &[ToolCall],
        invocation_ids: &[Uuid],
        conflicting_tools: &[&str],
        session_manager: &SessionManager,
        session_id: alms_core::SessionId,
    ) -> AlmsResult<Vec<AlmsResult<serde_json::Value>>> {
        match self.config.posture {
            Posture::Guarded => {
                // Sequential execution with cancellation support during each tool.
                // Cancellation is checked between tools AND races against each
                // individual tool execution so that long-running tools (e.g. shell
                // commands) can be interrupted mid-flight.
                let mut results = Vec::with_capacity(tool_calls.len());
                for (tc, &inv_id) in tool_calls.iter().zip(invocation_ids) {
                    if conflicting_tools.contains(&tc.function.name.as_str()) {
                        results.push(Err(AlmsError::ToolExecution(DM_CONFLICT_MSG.to_string())));
                        continue;
                    }
                    if let Some(ref token) = self.cancel_token
                        && token.is_cancelled()
                    {
                        return Err(AlmsError::Cancelled);
                    }
                    let result = if let Some(ref token) = self.cancel_token {
                        tokio::select! {
                            r = self.execute_tool_call(tc, inv_id, session_manager, session_id) => r,
                            _ = token.cancelled() => return Err(AlmsError::Cancelled),
                        }
                    } else {
                        self.execute_tool_call(tc, inv_id, session_manager, session_id)
                            .await
                    };
                    results.push(result);
                }
                Ok(results)
            }
            Posture::FullControl | Posture::Autonomous => {
                // Indices of non-conflicting tools to execute.
                let exec_indices: Vec<usize> = tool_calls
                    .iter()
                    .enumerate()
                    .filter(|(_, tc)| !conflicting_tools.contains(&tc.function.name.as_str()))
                    .map(|(i, _)| i)
                    .collect();

                // Execute non-conflicting tools concurrently.
                let exec_results = if exec_indices.is_empty() {
                    Vec::new()
                } else {
                    let exec_futures = exec_indices.iter().map(|&i| {
                        self.execute_tool_call(
                            &tool_calls[i],
                            invocation_ids[i],
                            session_manager,
                            session_id,
                        )
                    });
                    if let Some(ref token) = self.cancel_token {
                        tokio::select! {
                            r = futures::future::join_all(exec_futures) => r,
                            _ = token.cancelled() => return Err(AlmsError::Cancelled),
                        }
                    } else {
                        futures::future::join_all(exec_futures).await
                    }
                };

                // Assemble final results: conflict errors for conflicting
                // tools, execution results for the rest. The exec_iter
                // produces exactly `exec_indices.len()` items (one per
                // non-conflicting tool), which is consumed in order.
                let mut exec_iter = exec_results.into_iter();
                let non_conflicting_count = exec_indices.len();
                let results: Vec<_> = tool_calls
                    .iter()
                    .map(|tc| {
                        if conflicting_tools.contains(&tc.function.name.as_str()) {
                            Err(AlmsError::ToolExecution(DM_CONFLICT_MSG.to_string()))
                        } else {
                            exec_iter.next().unwrap_or_else(|| {
                                // This branch is structurally unreachable: exec_results
                                // has exactly one entry per non-conflicting tool, and we
                                // consume them in the same order. If this fires, the
                                // conflict-filter / exec logic has diverged.
                                debug_assert!(
                                    false,
                                    "exec_iter exhausted prematurely: expected {} results \
                                     for non-conflicting tools but ran out",
                                    non_conflicting_count,
                                );
                                Err(AlmsError::Runtime(
                                    "BUG: exec_iter exhausted -- conflicting_tools filter \
                                     diverged from exec_indices"
                                        .into(),
                                ))
                            })
                        }
                    })
                    .collect();
                Ok(results)
            }
        }
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
                        params: serde_json::from_str(&tc.function.arguments).unwrap_or_else(|_| {
                            serde_json::Value::String(tc.function.arguments.clone())
                        }),
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
    #[allow(clippy::too_many_arguments)] // Private helper; the parameters are clear and grouping them into a struct would add indirection without real benefit.
    fn process_tool_results(
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
        for ((tool_call, result), invocation_id) in
            tool_calls.iter().zip(results).zip(invocation_ids)
        {
            let (content, ok) = match result {
                Ok(value) => {
                    let ok = tool_result_ok(&value);
                    (value.to_string(), ok)
                }
                Err(e) => (format!("Error: {}", e), false),
            };
            messages.push(LlmMessage::tool_result(&tool_call.id, content.clone()));

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
            {
                let base_meta = serde_json::json!({
                    "ok": ok,
                    "tool_invocation_id": invocation_id.to_string(),
                });
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
    #[instrument(
        level = "info",
        skip(self, tool_call, invocation_id, session_manager),
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

        // Parse arguments
        let args: serde_json::Value = match serde_json::from_str(args_str) {
            Ok(value) => value,
            Err(e) => {
                let err = alms_core::AlmsError::ToolExecution(format!("Invalid arguments: {}", e));
                let _ = session_manager.append_audit(
                    session_id,
                    AuditEvent {
                        session_id,
                        run_id: self.run_id,
                        tool: name.to_string(),
                        decision: AuditDecision::Deny,
                        params: serde_json::Value::String(args_str.to_string()),
                        result: None,
                        error: Some(err.to_string()),
                        timestamp: alms_core::Timestamp::now(),
                    },
                );
                return Err(err);
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
                    error: Some(err.to_string()),
                    timestamp: alms_core::Timestamp::now(),
                },
            );
            return Err(err);
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
        let auto_approved = self.tools.is_auto_approved(name);
        if self.config.posture == Posture::Guarded && auto_approved {
            debug!(
                tool_name = %name,
                "Auto-approved tool — skipping approval gate in guarded posture"
            );
        } else if self.config.posture == Posture::Guarded {
            let sender = self.event_sender.as_ref().ok_or_else(|| {
                alms_core::AlmsError::Runtime(
                    "Guarded posture requires an event sender for approvals".to_string(),
                )
            })?;
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
            let approved = if let Some(ref token) = self.cancel_token {
                tokio::select! {
                    result = decision_rx => result.unwrap_or(false),
                    _ = token.cancelled() => return Err(AlmsError::Cancelled),
                }
            } else {
                match decision_rx.await {
                    Ok(v) => v,
                    Err(_) => {
                        return Err(alms_core::AlmsError::ToolExecution(
                            "Approval channel closed".to_string(),
                        ));
                    }
                }
            };
            if !approved {
                let _ = sender.send(RuntimeEvent::ToolEnd {
                    invocation_id,
                    ok: false,
                    result: serde_json::json!({"error": "denied by user"}),
                    source_agent: None,
                    task_id: None,
                });
                return Err(alms_core::AlmsError::ToolExecution(format!(
                    "Tool '{}' denied by user",
                    name
                )));
            }
        }

        // Execute
        let result = self.tools.execute(name, args.clone()).await;
        let elapsed = start.elapsed();

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
                let _ = session_manager.append_audit(
                    session_id,
                    AuditEvent {
                        session_id,
                        run_id: self.run_id,
                        tool: name.to_string(),
                        decision: AuditDecision::Allow,
                        params: args,
                        result: Some(value.clone()),
                        error: None,
                        timestamp: alms_core::Timestamp::now(),
                    },
                );
                let ok = tool_result_ok(value);

                if let Some(ref sender) = self.event_sender {
                    let _ = sender.send(RuntimeEvent::ToolEnd {
                        invocation_id,
                        ok,
                        result: value.clone(),
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
                let _ = session_manager.append_audit(
                    session_id,
                    AuditEvent {
                        session_id,
                        run_id: self.run_id,
                        tool: name.to_string(),
                        decision: AuditDecision::Error,
                        params: args,
                        result: audit_result,
                        error: Some(e.to_string()),
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
    /// the run still has something to surface.
    #[test]
    fn thinking_only_promotes_reasoning_into_content() {
        let (content, reasoning) =
            finalize_content_and_reasoning(String::new(), "long deliberation".to_string(), false);
        assert_eq!(content.as_deref(), Some("long deliberation"));
        assert!(reasoning.is_none());
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
