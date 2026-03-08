use crate::context::{ContextBuilder, content_to_string};
use crate::events::{RuntimeEvent, RuntimeEventSender};
use crate::get_task_result_tool::GetTaskResultTool;
use crate::invoke_agent_tool::InvokeAgentTool;
use crate::llm_client::LlmClient;
use crate::llm_types::*;
use crate::tools::ToolRegistry;
use crate::workspace::AgentWorkspace;
use crate::workspace_tool::WorkspaceWriteTool;
use alms_core::config::ContextConfig;
use alms_core::{AgentId, AlmsResult, AuditDecision, AuditEvent, TokenUsage};
use alms_session::{
    ContextSummary, Message as SessionMessage, Role as SessionRole, SessionManager,
};
use tracing::{Span, debug, error, info, instrument, warn};
use uuid::Uuid;

/// Execution posture: controls whether tools require approval before running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Posture {
    /// Execute tools directly without approval (default).
    #[default]
    FullControl,
    /// Require explicit user approval before each tool execution.
    Guarded,
}

/// Agent runtime configuration
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// System prompt
    pub system_prompt: String,
    /// Maximum iterations for tool loops
    pub max_iterations: u32,
    /// Temperature for LLM
    pub temperature: f32,
    /// Maximum tokens per response
    pub max_tokens: u32,
    /// Context window management config
    pub context_config: ContextConfig,
    /// Execution posture (full_control or guarded)
    pub posture: Posture,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            system_prompt: "You are a helpful assistant. Use tools when appropriate.".to_string(),
            max_iterations: 10,
            temperature: 0.7,
            max_tokens: 4096,
            context_config: ContextConfig::default(),
            posture: Posture::FullControl,
        }
    }
}

/// Result of a single agent run, including the response text and accumulated token usage.
#[derive(Debug, Clone)]
pub struct RunOutput {
    pub response: String,
    pub usage: TokenUsage,
}

/// Agent runtime - executes agent loops
#[derive(Debug)]
pub struct AgentRuntime {
    agent_id: AgentId,
    config: AgentConfig,
    llm: LlmClient,
    tools: ToolRegistry,
    workspace: Option<AgentWorkspace>,
    /// Optional channel for emitting runtime events to the gateway layer.
    event_sender: Option<RuntimeEventSender>,
    /// Run ID for audit event correlation (set by gateway before execution).
    run_id: Option<alms_core::RunId>,
}

impl AgentRuntime {
    /// Create new agent runtime
    pub fn new(agent_id: AgentId, config: AgentConfig, llm: LlmClient) -> Self {
        Self {
            agent_id,
            config,
            llm,
            tools: ToolRegistry::with_builtins(),
            workspace: None,
            event_sender: None,
            run_id: None,
        }
    }

    /// Attach an agent workspace for persistent identity files.
    ///
    /// Also registers the `workspace_write` tool so the agent can update
    /// its `goals.md` and `memories.md` during runs.
    pub fn with_workspace(mut self, workspace: AgentWorkspace) -> Self {
        let tool = WorkspaceWriteTool::new(workspace.clone());
        self.tools.register(std::sync::Arc::new(tool));
        self.workspace = Some(workspace);
        self
    }

    /// Attach a runtime event sender so the gateway can observe tool events.
    pub fn with_event_sender(mut self, sender: RuntimeEventSender) -> Self {
        self.event_sender = Some(sender);
        self
    }

    /// Register the `invoke_agent` tool so the agent can spawn subagents.
    pub fn with_invoke_agent(self, tool: InvokeAgentTool) -> Self {
        self.tools.register(std::sync::Arc::new(tool));
        self
    }

    /// Register the `get_task_result` tool for polling background subagents.
    pub fn with_get_task_result(self, tool: GetTaskResultTool) -> Self {
        self.tools.register(std::sync::Arc::new(tool));
        self
    }

    /// Set the run ID for audit event correlation.
    pub fn with_run_id(mut self, run_id: alms_core::RunId) -> Self {
        self.run_id = Some(run_id);
        self
    }

    /// Create with default config
    pub fn with_defaults(agent_id: AgentId) -> AlmsResult<Self> {
        let llm = LlmClient::from_env()?;
        Ok(Self::new(agent_id, AgentConfig::default(), llm))
    }

    /// Run the agent on a single input
    #[instrument(
        level = "info",
        skip(self, session_manager, context_id, input),
        fields(
            agent_id = %self.agent_id.0,
            context_id = %context_id.as_ref(),
            max_iterations = %self.config.max_iterations
        )
    )]
    pub async fn run(
        &self,
        session_manager: &SessionManager,
        context_id: impl AsRef<str>,
        input: impl Into<String>,
    ) -> AlmsResult<RunOutput> {
        let context_id = context_id.as_ref();
        let input = input.into();
        let span = Span::current();

        info!(
            target: "agent::run_start",
            agent_id = %self.agent_id.0,
            context_id = %context_id,
            input_len = input.len(),
            "Agent run started"
        );

        span.record("input_len", input.len());

        let session = session_manager.get_or_create(self.agent_id, context_id);
        let history = self.build_context(session_manager, &session.id, &input).await?;
        let (response, usage) = self
            .agent_loop(session_manager, session.id, history)
            .await?;

        let user_msg = SessionMessage {
            id: uuid::Uuid::new_v4().to_string(),
            role: SessionRole::User,
            content: alms_session::Content::Text(input.clone()),
            timestamp: alms_core::Timestamp::now(),
            metadata: None,
        };

        let assistant_msg = SessionMessage {
            id: uuid::Uuid::new_v4().to_string(),
            role: SessionRole::Assistant,
            content: alms_session::Content::Text(response.clone()),
            timestamp: alms_core::Timestamp::now(),
            metadata: None,
        };

        session_manager.append_message(session.id, user_msg)?;
        session_manager.append_message(session.id, assistant_msg)?;

        info!(
            "Agent {} completed for context {} (prompt={} completion={} tokens)",
            self.agent_id.0, context_id, usage.prompt_tokens, usage.completion_tokens
        );

        Ok(RunOutput { response, usage })
    }

    /// Run with streaming response
    pub async fn run_stream(
        &self,
        session_manager: &SessionManager,
        context_id: impl AsRef<str>,
        input: impl Into<String>,
    ) -> AlmsResult<impl futures::Stream<Item = AlmsResult<String>>> {
        let context_id = context_id.as_ref();
        let input = input.into();

        info!(
            "Running agent {} (streaming) for context {}",
            self.agent_id.0, context_id
        );

        let output = self.run(session_manager, context_id, input).await?;

        use futures::stream;
        Ok(stream::once(async move { Ok(output.response) }))
    }

    /// Build context window for LLM using ContextBuilder.
    ///
    /// For the `sliding-summary` strategy this is async because it may call the
    /// LLM to compress old messages into a rolling summary.
    async fn build_context(
        &self,
        session_manager: &SessionManager,
        session_id: &alms_core::SessionId,
        input: &str,
    ) -> AlmsResult<Vec<LlmMessage>> {
        let system_prompt = if let Some(ref ws) = self.workspace {
            let prefix = ws.build_system_prompt_prefix();
            if prefix.is_empty() {
                self.config.system_prompt.clone()
            } else {
                format!("{}\n\n{}", prefix, self.config.system_prompt)
            }
        } else {
            self.config.system_prompt.clone()
        };

        let history = match session_manager.get_history(*session_id) {
            Ok(h) => h,
            Err(e) => {
                warn!("Failed to get history: {}", e);
                Vec::new()
            }
        };

        // For sliding-summary, attempt to compress old messages before building context.
        // On failure we log a warning and fall back (None summary → truncate behaviour).
        let summary_text: Option<String> =
            if self.config.context_config.strategy == "sliding-summary" {
                let current = session_manager
                    .get_summary(*session_id)
                    .unwrap_or_default();
                match self
                    .maybe_summarize(session_manager, *session_id, &history, current)
                    .await
                {
                    Ok(s) => Some(s.text).filter(|t| !t.is_empty()),
                    Err(e) => {
                        warn!(
                            "Sliding-summary compression failed, falling back to truncation: {}",
                            e
                        );
                        None
                    }
                }
            } else {
                None
            };

        let builder = ContextBuilder::new(self.config.context_config.clone());
        Ok(builder.build(&system_prompt, &history, input, summary_text.as_deref()))
    }

    /// Check whether history has grown past the summarization threshold and, if so,
    /// call the LLM to extend the rolling summary with the oldest uncovered messages.
    ///
    /// Returns the (possibly updated) `ContextSummary`. On success the updated
    /// summary is also persisted via `session_manager.update_summary()`.
    async fn maybe_summarize(
        &self,
        session_manager: &SessionManager,
        session_id: alms_core::SessionId,
        history: &[alms_session::Message],
        mut current: ContextSummary,
    ) -> AlmsResult<ContextSummary> {
        let recent_window = self.config.context_config.recent_window;
        let summary_interval = self.config.context_config.summary_interval;

        // Guard against corrupt messages_covered value.
        current.messages_covered = current.messages_covered.min(history.len());

        let uncovered = history.len().saturating_sub(current.messages_covered);
        let compressible = uncovered.saturating_sub(recent_window);

        if compressible < summary_interval {
            return Ok(current); // not enough new material to justify a summary call
        }

        // Compress everything from messages_covered up to (history.len() - recent_window)
        // so we always keep the recent window verbatim.
        let compress_end = history.len() - recent_window;
        let to_compress = &history[current.messages_covered..compress_end];
        if to_compress.is_empty() {
            return Ok(current);
        }

        // Build summarization prompt
        let mut sum_messages = vec![LlmMessage::system(
            "You are a conversation summarizer. \
             Given a sequence of messages, produce a concise factual summary \
             (3–7 sentences) capturing key decisions, facts learned, and actions taken. \
             No pleasantries or meta-commentary.",
        )];

        let user_prefix = if current.text.is_empty() {
            "Summarize the following conversation:".to_string()
        } else {
            format!(
                "Extend this existing summary with the new messages below.\n\
                 Existing summary:\n{}\n\nNew messages to incorporate:",
                current.text
            )
        };
        sum_messages.push(LlmMessage::user(user_prefix));

        let transcript: String = to_compress
            .iter()
            .map(|m| {
                let role_label = match m.role {
                    SessionRole::User => "User",
                    SessionRole::Assistant => "Assistant",
                    SessionRole::System => "System",
                    SessionRole::Tool => "Tool",
                };
                format!("{}: {}", role_label, content_to_string(&m.content))
            })
            .collect::<Vec<_>>()
            .join("\n");
        sum_messages.push(LlmMessage::user(transcript));

        let model = self
            .config
            .context_config
            .summary_model
            .as_deref()
            .unwrap_or_else(|| self.llm.default_model());

        let request = CompletionRequest::new(model)
            .with_messages(sum_messages)
            .with_temperature(0.3) // lower temp for factual compression
            .with_max_tokens(512);

        let response = self.llm.complete(request).await?;

        let new_text = response
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| {
                alms_core::AlmsError::Runtime(
                    "Summarization LLM returned empty response".to_string(),
                )
            })?;

        current.text = new_text;
        current.messages_covered = compress_end;
        current.updated_at = Some(alms_core::Timestamp::now());

        session_manager.update_summary(session_id, current.clone())?;

        info!(
            "Sliding-summary: compressed {} messages (now {} covered, {} in recent window)",
            to_compress.len(),
            compress_end,
            recent_window,
        );

        Ok(current)
    }

    /// Main agent loop with tool execution
    #[instrument(
        level = "debug",
        skip(self, messages),
        fields(agent_id = %self.agent_id.0)
    )]
    async fn agent_loop(
        &self,
        session_manager: &SessionManager,
        session_id: alms_core::SessionId,
        mut messages: Vec<LlmMessage>,
    ) -> AlmsResult<(String, TokenUsage)> {
        let mut iterations = 0;
        let mut total_usage = TokenUsage::default();

        loop {
            if iterations >= self.config.max_iterations {
                warn!(
                    target: "agent::loop",
                    agent_id = %self.agent_id.0,
                    iterations,
                    max_iterations = %self.config.max_iterations,
                    "Max iterations reached"
                );
                return Ok(("[Max iterations reached]".to_string(), total_usage));
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
                .with_temperature(self.config.temperature)
                .with_max_tokens(self.config.max_tokens);

            let response = self.llm.complete(request).await?;

            // Accumulate token usage from this LLM call
            if let Some(usage) = &response.usage {
                total_usage.prompt_tokens += usage.prompt_tokens;
                total_usage.completion_tokens += usage.completion_tokens;
            }

            let choice =
                response.choices.into_iter().next().ok_or_else(|| {
                    alms_core::AlmsError::Runtime("No response from LLM".to_string())
                })?;

            let message = choice.message;

            if let Some(tool_calls) = message.tool_calls {
                messages.push(LlmMessage {
                    role: "assistant".to_string(),
                    content: message.content.clone(),
                    tool_calls: Some(tool_calls.clone()),
                    tool_call_id: None,
                });

                // Run all tool calls concurrently so background invoke_agent calls
                // don't block each other, and independent tools finish in parallel.
                let results = futures::future::join_all(
                    tool_calls
                        .iter()
                        .map(|tc| self.execute_tool_call(tc, session_manager, session_id)),
                )
                .await;

                for (tool_call, result) in tool_calls.iter().zip(results) {
                    let content = match result {
                        Ok(value) => value.to_string(),
                        Err(e) => format!("Error: {}", e),
                    };
                    messages.push(LlmMessage::tool_result(&tool_call.id, content));
                }

                continue;
            }

            return Ok((message.content.unwrap_or_default(), total_usage));
        }
    }

    /// Execute a tool call, emitting tool_start/tool_end events and handling approvals.
    #[instrument(
        level = "info",
        skip(self, tool_call),
        fields(
            agent_id = %self.agent_id.0,
            tool_name = %tool_call.function.name,
            tool_call_id = %tool_call.id
        )
    )]
    async fn execute_tool_call(
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
            });
            match decision_rx.await {
                Ok(true) => {} // approved — proceed
                Ok(false) => {
                    let _ = sender.send(RuntimeEvent::ToolEnd {
                        invocation_id,
                        ok: false,
                        result: serde_json::json!({"error": "denied by user"}),
                    });
                    return Err(alms_core::AlmsError::ToolExecution(format!(
                        "Tool '{}' denied by user",
                        name
                    )));
                }
                Err(_) => {
                    return Err(alms_core::AlmsError::ToolExecution(
                        "Approval channel closed".to_string(),
                    ));
                }
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
                if let Some(ref sender) = self.event_sender {
                    let _ = sender.send(RuntimeEvent::ToolEnd {
                        invocation_id,
                        ok: true,
                        result: value.clone(),
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
                    });
                }
            }
        }

        result
    }

    /// Get tool registry reference
    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alms_session::{SessionConfig, SessionManager};

    #[tokio::test]
    async fn test_agent_config_default() {
        let config = AgentConfig::default();
        assert_eq!(config.max_iterations, 10);
        assert_eq!(config.temperature, 0.7);
        assert!(!config.system_prompt.is_empty());
        assert_eq!(config.posture, Posture::FullControl);
    }

    #[tokio::test]
    async fn test_build_context() {
        let runtime = AgentRuntime {
            agent_id: AgentId::new(),
            config: AgentConfig::default(),
            llm: LlmClient::new(LlmConfig::default()).unwrap(),
            tools: ToolRegistry::new(),
            workspace: None,
            event_sender: None,
            run_id: None,
        };

        let session_config = SessionConfig::default();
        let session_manager = SessionManager::new(session_config);
        let session = session_manager.get_or_create(runtime.agent_id, "test");

        let messages = runtime
            .build_context(&session_manager, &session.id, "hello")
            .await
            .unwrap();
        // system prompt + current input = 2
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
    }
}
