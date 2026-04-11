use crate::events::{PHASE_CALLING_LLM, PHASE_EXECUTING_TOOLS, RuntimeEvent};
use crate::llm_types::*;
use alms_core::{
    AlmsError, AlmsResult, AuditDecision, AuditEvent, MAX_ITERATIONS_SENTINEL, TokenUsage,
};
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
    ) -> (
        Vec<alms_core::ToolCallRecord>,
        AlmsResult<(String, TokenUsage)>,
    ) {
        let mut iterations = 0;
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

            if iterations >= self.config.max_iterations {
                warn!(
                    target: "agent::loop",
                    agent_id = %self.agent_id.0,
                    iterations,
                    max_iterations = %self.config.max_iterations,
                    "Max iterations reached"
                );
                return (
                    tool_call_records,
                    Ok((MAX_ITERATIONS_SENTINEL.to_string(), total_usage)),
                );
            }
            iterations += 1;

            debug!(
                target: "agent::loop",
                agent_id = %self.agent_id.0,
                iteration = iterations,
                "Agent loop iteration"
            );

            // NOTE: `messages.clone()` is required here because
            // `CompletionRequest` takes ownership of the `Vec<LlmMessage>`,
            // but we continue to mutate `messages` after the LLM call
            // (appending tool results for the next iteration). The clone
            // cost scales with conversation length; if this becomes a
            // bottleneck, the LLM client could be changed to accept a
            // reference, but that would require upstream API changes.
            let request = CompletionRequest::new(self.llm.default_model())
                .with_messages(messages.clone())
                .with_tools(self.tools.to_definitions())
                .with_max_tokens(self.config.max_tokens);

            let (content, tool_calls, usage) = match self.call_llm_with_cancellation(request).await
            {
                Ok(result) => result,
                Err(e) => return (tool_call_records, Err(e)),
            };

            // Accumulate token usage from this LLM call
            if let Some(ref usage) = usage {
                total_usage.prompt_tokens += usage.prompt_tokens;
                total_usage.completion_tokens += usage.completion_tokens;
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
                if !is_dm {
                    self.persist_assistant_tool_calls(
                        session_manager,
                        session_id,
                        content.as_deref(),
                        &tool_calls,
                        &invocation_ids,
                    );
                }

                // Collect tool call records for per-run storage (all sessions).
                for tc in &tool_calls {
                    tool_call_records.push(alms_core::ToolCallRecord {
                        seq: tool_seq,
                        role: alms_core::ToolCallRole::Assistant,
                        tool_name: Some(tc.function.name.clone()),
                        tool_id: Some(tc.id.clone()),
                        params: Some(tc.function.arguments.clone()),
                        result: None,
                        timestamp: chrono::Utc::now(),
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
                    return (tool_call_records, Ok((String::new(), total_usage)));
                }

                // In a DM-triggered run, terminate the loop after the agent
                // has successfully called `send_message`.  The reply has been
                // delivered; re-entering the loop would let the LLM call
                // `send_message` again, producing duplicate messages and a
                // cascade of RunTrigger events (#407 Bug 1).
                if should_terminate_after_dm_send(&tool_calls, is_dm, dm_check.conflict) {
                    info!("DM run: send_message delivered -- ending loop (one reply per DM run)");
                    let text = content.unwrap_or_default();
                    return (tool_call_records, Ok((text, total_usage)));
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
                Ok((content.unwrap_or_default(), total_usage)),
            );
        }
    }

    /// Call the LLM (streaming with buffered fallback), respecting cancellation.
    ///
    /// Emits `PHASE_CALLING_LLM` status, attempts streaming first, falls back
    /// to buffered mode on streaming failure. Returns the same triple as
    /// `stream_llm_call`: (content, tool_calls, usage).
    async fn call_llm_with_cancellation(
        &self,
        request: CompletionRequest,
    ) -> AlmsResult<(Option<String>, Option<Vec<ToolCall>>, Option<Usage>)> {
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
                Ok((
                    choice.message.effective_content().map(|s| s.to_string()),
                    choice.message.tool_calls,
                    usage,
                ))
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
    fn persist_assistant_tool_calls(
        &self,
        session_manager: &SessionManager,
        session_id: alms_core::SessionId,
        content: Option<&str>,
        tool_calls: &[ToolCall],
        invocation_ids: &[Uuid],
    ) {
        // Persist assistant text content (if any) before tool calls.
        if let Some(text) = content
            && !text.is_empty()
            && let Err(e) = session_manager.append_message(
                session_id,
                SessionMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    role: SessionRole::Assistant,
                    content: SessionContent::Text(text.to_string()),
                    timestamp: alms_core::Timestamp::now(),
                    metadata: None,
                },
            )
        {
            warn!("Failed to persist assistant text to session: {}", e);
        }

        // Persist tool calls to session history.
        for (tc, invocation_id) in tool_calls.iter().zip(invocation_ids) {
            if let Err(e) = session_manager.append_message(
                session_id,
                SessionMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    role: SessionRole::Assistant,
                    content: SessionContent::ToolCall {
                        name: tc.function.name.clone(),
                        params: serde_json::from_str(&tc.function.arguments).unwrap_or_else(|_| {
                            serde_json::Value::String(tc.function.arguments.clone())
                        }),
                    },
                    timestamp: alms_core::Timestamp::now(),
                    metadata: Some(serde_json::json!({
                        "tool_call_id": tc.id,
                        "tool_invocation_id": invocation_id.to_string(),
                    })),
                },
            ) {
                warn!("Failed to persist tool call to session: {}", e);
            }
        }
    }

    /// Process tool execution results: push tool result messages into the
    /// conversation, persist to session (non-DM), and collect per-run records.
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

            // Persist tool result to session history (skip for DM sessions).
            // Intentionally fire-and-forget -- see persist_assistant_tool_calls
            // for the rationale.
            //
            // Include tool_invocation_id in the metadata so history
            // reconstruction can correlate tool results back to the same
            // invocation ID used by live SSE tool_start/tool_end events.
            // (Fixes #509)
            if !is_dm
                && let Err(e) = session_manager.append_message(
                    session_id,
                    SessionMessage {
                        id: uuid::Uuid::new_v4().to_string(),
                        role: SessionRole::Tool,
                        content: SessionContent::ToolResult {
                            tool_id: tool_call.id.clone(),
                            result: serde_json::from_str(&content)
                                .unwrap_or(serde_json::Value::String(content.clone())),
                        },
                        timestamp: alms_core::Timestamp::now(),
                        metadata: Some(serde_json::json!({
                            "ok": ok,
                            "tool_invocation_id": invocation_id.to_string(),
                        })),
                    },
                )
            {
                warn!("Failed to persist tool result to session: {}", e);
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
    /// `llm_client.rs`), controlled by `LlmConfig::stream_chunk_timeout_secs`
    /// (default 60s). If the provider stalls mid-stream, the chunk-level
    /// timeout fires and propagates an error up through this method. User-
    /// initiated cancellation is handled separately in `call_llm_with_cancellation`.
    pub(crate) async fn stream_llm_call(
        &self,
        request: CompletionRequest,
    ) -> AlmsResult<(Option<String>, Option<Vec<ToolCall>>, Option<Usage>)> {
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

            // Accumulate reasoning_content from reasoning models.
            // This is not emitted as token_delta (it's internal thinking),
            // but is preserved so we can fall back to it when content is
            // empty at the end of streaming.
            if let Some(text) = choice.delta.reasoning_content
                && !text.is_empty()
            {
                reasoning_content.push_str(&text);
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

        // Fall back to reasoning_content when the model streamed everything
        // through reasoning (common with reasoning models that exhaust
        // max_tokens before transitioning to output).
        let content = if content.is_empty() {
            if !reasoning_content.is_empty() {
                info!("Streaming: content empty, falling back to reasoning_content");
                Some(reasoning_content)
            } else {
                None
            }
        } else {
            Some(content)
        };

        Ok((content, tool_calls, usage))
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
            });
        }

        // Guarded posture: block until user approves or denies
        if self.config.posture == Posture::Guarded {
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
                    });
                }
            }
            Err(e) => {
                error!(
                    target: "agent::tool::error",
                    agent_id = %self.agent_id.0,
                    tool_name = %name,
                    tool_call_id = %tool_call.id,
                    error = %e,
                    duration_ms = %elapsed.as_millis(),
                    "Tool execution failed"
                );
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
                        result: None,
                        error: Some(e.to_string()),
                        timestamp: alms_core::Timestamp::now(),
                    },
                );
                if let Some(ref sender) = self.event_sender {
                    let _ = sender.send(RuntimeEvent::ToolEnd {
                        invocation_id,
                        ok: false,
                        result: serde_json::json!({"error": e.to_string()}),
                        source_agent: None,
                    });
                }
            }
        }

        result
    }
}
