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

            let request = CompletionRequest::new(self.llm.default_model())
                .with_messages(messages.clone())
                .with_tools(self.tools.to_definitions())
                .with_max_tokens(self.config.max_tokens);

            // Checkpoint B: LLM call with cancellation support.
            // Stream the LLM call, emitting token_delta events as chunks arrive.
            // Falls back to buffered mode if streaming fails.
            self.emit_status(PHASE_CALLING_LLM, None);
            let streaming_future = self.stream_llm_call(request.clone());
            let stream_result = if let Some(ref token) = self.cancel_token {
                tokio::select! {
                    result = streaming_future => result,
                    _ = token.cancelled() => return (tool_call_records, Err(AlmsError::Cancelled)),
                }
            } else {
                streaming_future.await
            };
            let (content, tool_calls, usage) = match stream_result {
                Ok(result) => result,
                Err(e) => {
                    warn!("Streaming failed, falling back to buffered: {}", e);
                    let buffered_future = self.llm.complete(request);
                    let response = if let Some(ref token) = self.cancel_token {
                        tokio::select! {
                            result = buffered_future => match result {
                                Ok(r) => r,
                                Err(e) => return (tool_call_records, Err(e)),
                            },
                            _ = token.cancelled() => return (tool_call_records, Err(AlmsError::Cancelled)),
                        }
                    } else {
                        match buffered_future.await {
                            Ok(r) => r,
                            Err(e) => return (tool_call_records, Err(e)),
                        }
                    };
                    let usage = response.usage.clone();
                    let choice = match response.choices.into_iter().next() {
                        Some(c) => c,
                        None => {
                            return (
                                tool_call_records,
                                Err(AlmsError::Runtime(
                                    "LLM returned empty choices array".to_string(),
                                )),
                            );
                        }
                    };
                    (choice.message.content, choice.message.tool_calls, usage)
                }
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
                    tool_calls: Some(tool_calls.clone()),
                    tool_call_id: None,
                });

                // Persist assistant text content (if any) before tool calls.
                // For DM sessions, skip session persistence — tool calls stay
                // in-memory for the current run's multi-turn loop only.
                if !is_dm {
                    if let Some(ref text) = content
                        && !text.is_empty()
                        && let Err(e) = session_manager.append_message(
                            session_id,
                            SessionMessage {
                                id: uuid::Uuid::new_v4().to_string(),
                                role: SessionRole::Assistant,
                                content: SessionContent::Text(text.clone()),
                                timestamp: alms_core::Timestamp::now(),
                                metadata: None,
                            },
                        )
                    {
                        warn!("Failed to persist assistant text to session: {}", e);
                    }

                    // Persist tool calls to session history
                    for tc in &tool_calls {
                        if let Err(e) = session_manager.append_message(
                            session_id,
                            SessionMessage {
                                id: uuid::Uuid::new_v4().to_string(),
                                role: SessionRole::Assistant,
                                content: SessionContent::ToolCall {
                                    name: tc.function.name.clone(),
                                    params: serde_json::from_str(&tc.function.arguments)
                                        .unwrap_or_else(|_| {
                                            serde_json::Value::String(tc.function.arguments.clone())
                                        }),
                                },
                                timestamp: alms_core::Timestamp::now(),
                                metadata: Some(serde_json::json!({ "tool_call_id": tc.id })),
                            },
                        ) {
                            warn!("Failed to persist tool call to session: {}", e);
                        }
                    }
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
                // the same tool-call batch, execute neither — return error
                // results for both so the LLM can retry with just one.
                // Other non-conflicting tools in the batch still execute
                // normally. (Fixes #364)
                let dm_check = detect_dm_conflict(&tool_calls);
                if dm_check.conflict {
                    warn!(
                        "Agent called both send_message and ignore_message in same batch — \
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

                // Checkpoint C: tool execution with cancellation support.
                //
                // Full-control / Autonomous posture: run all tool calls concurrently
                // so background invoke_agent calls don't block each other and
                // independent tools finish in parallel.
                //
                // Guarded posture: run tool calls sequentially so the user sees one
                // approval prompt at a time rather than all at once.
                //
                // When dm_check.conflict is true, send_message and
                // ignore_message are skipped (replaced with error results);
                // other tools in the batch still execute.
                let conflicting_tools = dm_check.conflicting_tools;

                let results = match self.config.posture {
                    Posture::Guarded => {
                        // Note: cancellation during active tool execution (post-approval)
                        // is not detected until the tool completes, which is acceptable
                        // since guarded tools block on approval (which IS cancellation-aware).
                        let mut results = Vec::with_capacity(tool_calls.len());
                        for tc in &tool_calls {
                            if conflicting_tools.contains(&tc.function.name.as_str()) {
                                results.push(Err(AlmsError::ToolExecution(
                                    DM_CONFLICT_MSG.to_string(),
                                )));
                                continue;
                            }
                            if let Some(ref token) = self.cancel_token
                                && token.is_cancelled()
                            {
                                return (tool_call_records, Err(AlmsError::Cancelled));
                            }
                            results.push(
                                self.execute_tool_call(tc, session_manager, session_id)
                                    .await,
                            );
                        }
                        results
                    }
                    Posture::FullControl | Posture::Autonomous => {
                        // Indices of non-conflicting tools to execute.
                        let exec_indices: Vec<usize> = tool_calls
                            .iter()
                            .enumerate()
                            .filter(|(_, tc)| {
                                !conflicting_tools.contains(&tc.function.name.as_str())
                            })
                            .map(|(i, _)| i)
                            .collect();

                        // Execute non-conflicting tools concurrently.
                        let exec_results = if exec_indices.is_empty() {
                            Vec::new()
                        } else {
                            let exec_futures = exec_indices.iter().map(|&i| {
                                self.execute_tool_call(&tool_calls[i], session_manager, session_id)
                            });
                            if let Some(ref token) = self.cancel_token {
                                tokio::select! {
                                    r = futures::future::join_all(exec_futures) => r,
                                    _ = token.cancelled() => return (tool_call_records, Err(AlmsError::Cancelled)),
                                }
                            } else {
                                futures::future::join_all(exec_futures).await
                            }
                        };

                        // Assemble final results: conflict errors for
                        // conflicting tools, execution results for the rest.
                        let mut exec_iter = exec_results.into_iter();
                        tool_calls
                            .iter()
                            .map(|tc| {
                                if conflicting_tools.contains(&tc.function.name.as_str()) {
                                    Err(AlmsError::ToolExecution(DM_CONFLICT_MSG.to_string()))
                                } else {
                                    exec_iter.next().unwrap_or_else(|| {
                                        Err(AlmsError::Runtime("missing tool result".into()))
                                    })
                                }
                            })
                            .collect()
                    }
                };

                for (tool_call, result) in tool_calls.iter().zip(results) {
                    let (content, ok) = match result {
                        Ok(value) => {
                            // shell_exec returns Ok even for non-zero exit codes;
                            // check exit_code so persisted metadata matches SSE events.
                            let ok = value
                                .get("exit_code")
                                .and_then(|v| v.as_i64())
                                .is_none_or(|code| code == 0);
                            (value.to_string(), ok)
                        }
                        Err(e) => (format!("Error: {}", e), false),
                    };
                    messages.push(LlmMessage::tool_result(&tool_call.id, content.clone()));

                    // Persist tool result to session history (skip for DM sessions).
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
                                metadata: Some(serde_json::json!({ "ok": ok })),
                            },
                        )
                    {
                        warn!("Failed to persist tool result to session: {}", e);
                    }

                    // Collect tool result record for per-run storage (all sessions).
                    tool_call_records.push(alms_core::ToolCallRecord {
                        seq: tool_seq,
                        role: alms_core::ToolCallRole::Tool,
                        tool_name: Some(tool_call.function.name.clone()),
                        tool_id: Some(tool_call.id.clone()),
                        params: None,
                        result: Some(content.clone()),
                        timestamp: chrono::Utc::now(),
                    });
                    tool_seq += 1;
                }

                // Check if `ignore_message` was called AND succeeded.
                // We inspect the actual tool-call records (which include
                // execution results), not just the LLM's requested calls.
                // This prevents early termination when ignore_message fails
                // (e.g. called from a non-DM session, or blocked by conflict).
                if alms_core::ran_ignore_message_successfully(&tool_call_records) {
                    info!("Agent declined to respond via ignore_message — ending run early");
                    return (tool_call_records, Ok((String::new(), total_usage)));
                }

                // In a DM-triggered run, terminate the loop after the agent
                // has successfully called `send_message`.  The reply has been
                // delivered; re-entering the loop would let the LLM call
                // `send_message` again, producing duplicate messages and a
                // cascade of RunTrigger events (#407 Bug 1).
                if should_terminate_after_dm_send(&tool_calls, is_dm, dm_check.conflict) {
                    info!("DM run: send_message delivered — ending loop (one reply per DM run)");
                    // Return the last text content (if any) alongside the
                    // accumulated usage.  In most DM runs the LLM returns
                    // tool calls only (no text), so `content` is typically
                    // None/empty here; the actual message is delivered via
                    // the `send_message` tool execution.
                    let text = content.unwrap_or_default();
                    return (tool_call_records, Ok((text, total_usage)));
                }

                // Append tool_loop instructions to the system prompt for
                // subsequent iterations. The agent's identity (initial prompt +
                // workspace prefix) is preserved; tool_loop adds continuation
                // guidance on top.
                //
                // For DM sessions, re-inject the DM recipient addendum so the
                // agent remembers to use `send_message` on every iteration —
                // not just the first one (fixes #346).
                self.rebuild_system_prompt_for_tool_loop(&mut messages, include_user, dm_peer);

                continue;
            }

            // --- DM text-only response retry (#361) ---
            //
            // When a DM-triggered run ends with a text-only response and
            // neither `send_message` nor `ignore_message` was called during
            // the entire run, the agent's response will be silently dropped
            // (by design — DM responses must go through `send_message`).
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
                    "DM run ended with text-only response — retrying with error prompt"
                );

                // Emit a warning event so the operator/UI is aware.
                if let Some(ref tx) = self.event_sender {
                    let _ = tx.send(crate::events::RuntimeEvent::Warning {
                        code: "DM_TEXT_ONLY_RETRY".to_string(),
                        message: "Agent responded with text only in a DM session. \
                                  Text responses are not delivered — retrying with \
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
                    "DM text-only retry exhausted — response will be dropped"
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

    /// Stream an LLM call, emitting `TokenDelta` events as text chunks arrive.
    ///
    /// Accumulates the full response (content + tool calls + usage) from the
    /// streaming chunks and returns them in the same shape as `complete()`.
    pub(crate) async fn stream_llm_call(
        &self,
        request: CompletionRequest,
    ) -> AlmsResult<(Option<String>, Option<Vec<ToolCall>>, Option<Usage>)> {
        use futures::StreamExt;

        let mut stream = self.llm.complete_stream(request).await?;

        let mut content = String::new();
        let mut tool_call_acc: Vec<(String, String, String)> = Vec::new(); // (id, name, arguments)
        let mut usage: Option<Usage> = None;

        while let Some(result) = stream.next().await {
            let chunk = result?;

            // Capture usage from final chunk
            if chunk.usage.is_some() {
                usage = chunk.usage;
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
        // Filter out ghost entries (empty id) that can appear if the
        // accumulator was grown by index but no actual data arrived.
        let tool_calls: Vec<ToolCall> = tool_call_acc
            .into_iter()
            .filter(|(id, _, _)| !id.is_empty())
            .map(|(id, name, arguments)| ToolCall {
                id,
                function: FunctionCall { name, arguments },
            })
            .collect();
        let tool_calls = if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        };

        let content = if content.is_empty() {
            None
        } else {
            Some(content)
        };

        Ok((content, tool_calls, usage))
    }

    /// Execute a tool call, emitting tool_start/tool_end events and handling approvals.
    #[instrument(
        level = "info",
        skip(self, tool_call, session_manager),
        fields(
            agent_id = %self.agent_id.0,
            tool_name = %tool_call.function.name,
            tool_call_id = %tool_call.id,
            session_id = %session_id.0
        )
    )]
    pub(crate) async fn execute_tool_call(
        &self,
        tool_call: &ToolCall,
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

        // Stable ID for correlating tool_start / tool_end SSE events
        let invocation_id = Uuid::new_v4();

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
                // shell_exec returns Ok even for non-zero exit codes;
                // surface the exit code so the UI shows failure (red X).
                let ok = value
                    .get("exit_code")
                    .and_then(|v| v.as_i64())
                    .is_none_or(|code| code == 0);

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
                let _ = session_manager.append_audit(
                    session_id,
                    AuditEvent {
                        session_id,
                        run_id: self.run_id,
                        tool: name.to_string(),
                        decision: AuditDecision::Deny,
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
