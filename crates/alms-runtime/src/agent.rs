use crate::llm_client::LlmClient;
use crate::llm_types::*;
use crate::tools::ToolRegistry;
use alms_core::{AgentId, AlmsResult, AuditEvent, AuditDecision};
use alms_session::{Message as SessionMessage, Role as SessionRole, SessionManager};
use tracing::{debug, error, info, instrument, warn, Span};

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
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            system_prompt: "You are a helpful assistant. Use tools when appropriate.".to_string(),
            max_iterations: 10,
            temperature: 0.7,
            max_tokens: 4096,
        }
    }
}

/// Agent runtime - executes agent loops
#[derive(Debug)]
pub struct AgentRuntime {
    agent_id: AgentId,
    config: AgentConfig,
    llm: LlmClient,
    tools: ToolRegistry,
}

impl AgentRuntime {
    /// Create new agent runtime
    pub fn new(agent_id: AgentId, config: AgentConfig, llm: LlmClient) -> Self {
        Self {
            agent_id,
            config,
            llm,
            tools: ToolRegistry::with_builtins(),
        }
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
    ) -> AlmsResult<String> {
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
        
        // Record input metrics
        span.record("input_len", input.len());
        
        // Get or create session
        let session = session_manager.get_or_create(self.agent_id, context_id);
        
        // Build conversation history
        let history = self.build_messages(session_manager, &session.id, &input).await?;
        
        // Run the agent loop
        let response = self.agent_loop(session_manager, session.id, history).await?;
        
        // Store messages
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
        
        info!("Agent {} completed for context {}", self.agent_id.0, context_id);
        
        Ok(response)
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
        
        // For streaming, we'd implement SSE-style response
        // For now, return a simple single-chunk stream
        let response = self.run(session_manager, context_id, input).await?;
        
        use futures::stream;
        Ok(stream::once(async move { Ok(response) }))
    }
    
    /// Build message history for LLM
    async fn build_messages(
        &self,
        session_manager: &SessionManager,
        session_id: &alms_core::SessionId,
        input: &str,
    ) -> AlmsResult<Vec<LlmMessage>> {
        let mut messages = vec![LlmMessage::system(&self.config.system_prompt)];
        
        // Add history
        match session_manager.get_history(*session_id) {
            Ok(history) => {
                for msg in history.iter().take(50) { // Limit context
                    let llm_msg = match msg.role {
                        SessionRole::System => continue, // Skip system messages
                        SessionRole::User => LlmMessage::user(self.content_to_string(&msg.content)),
                        SessionRole::Assistant => LlmMessage::assistant(self.content_to_string(&msg.content)),
                        SessionRole::Tool => continue, // Handle tool separately
                    };
                    messages.push(llm_msg);
                }
            }
            Err(e) => {
                warn!("Failed to get history: {}", e);
            }
        }
        
        // Add current input
        messages.push(LlmMessage::user(input));
        
        Ok(messages)
    }
    
    /// Convert session content to string
    fn content_to_string(&self, content: &alms_session::Content) -> String {
        match content {
            alms_session::Content::Text(text) => text.clone(),
            alms_session::Content::ToolCall { name, params } => {
                format!("Tool call: {}({})", name, params)
            }
            alms_session::Content::ToolResult { tool_id, result } => {
                format!("Tool result {}: {}", tool_id, result)
            }
            alms_session::Content::Image { url, .. } => {
                format!("[Image: {}]", url)
            }
        }
    }
    
    /// Main agent loop with tool execution
    #[instrument(
        level = "debug",
        skip(self, messages),
        fields(agent_id = %self.agent_id.0)
    )]
    async fn agent_loop(&self, session_manager: &SessionManager, session_id: alms_core::SessionId, mut messages: Vec<LlmMessage>) -> AlmsResult<String> {
        let mut iterations = 0;
        
        loop {
            if iterations >= self.config.max_iterations {
                warn!(
                    target: "agent::loop",
                    agent_id = %self.agent_id.0,
                    iterations,
                    max_iterations = %self.config.max_iterations,
                    "Max iterations reached"
                );
                return Ok("[Max iterations reached]".to_string());
            }
            iterations += 1;
            
            debug!(
                target: "agent::loop",
                agent_id = %self.agent_id.0,
                iteration = iterations,
                "Agent loop iteration"
            );
            
            // Build request
            let request = CompletionRequest::new(self.llm.default_model())
                .with_messages(messages.clone())
                .with_tools(self.tools.to_definitions())
                .with_temperature(self.config.temperature)
                .with_max_tokens(self.config.max_tokens);
            
            // Get completion
            let response = self.llm.complete(request).await?;
            
            let choice = response
                .choices
                .into_iter()
                .next()
                .ok_or_else(|| alms_core::AlmsError::Runtime("No response from LLM".to_string()))?;
            
            let message = choice.message;
            
            // Check if there are tool calls
            if let Some(tool_calls) = message.tool_calls {
                // Add assistant message with tool calls
                messages.push(LlmMessage {
                    role: "assistant".to_string(),
                    content: message.content.clone(),
                    tool_calls: Some(tool_calls.clone()),
                    tool_call_id: None,
                });
                
                // Execute tools
                for tool_call in tool_calls {
                    let result = self.execute_tool_call(&tool_call, session_manager, session_id).await;
                    
                    let content = match result {
                        Ok(value) => value.to_string(),
                        Err(e) => format!("Error: {}", e),
                    };
                    
                    messages.push(LlmMessage::tool_result(&tool_call.id, content));
                }
                
                // Continue loop for final response
                continue;
            }
            
            // No tool calls - return the response
            return Ok(message.content.unwrap_or_default());
        }
    }
    
    /// Execute a tool call
    #[instrument(
        level = "info",
        skip(self, tool_call),
        fields(
            agent_id = %self.agent_id.0,
            tool_name = %tool_call.function.name,
            tool_call_id = %tool_call.id
        )
    )]
    async fn execute_tool_call(&self, tool_call: &ToolCall, session_manager: &SessionManager, session_id: alms_core::SessionId) -> AlmsResult<serde_json::Value> {
        let name = &tool_call.function.name;
        let args_str = &tool_call.function.arguments;
        
        info!(
            target: "agent::tool::start",
            agent_id = %self.agent_id.0,
            tool_name = %name,
            tool_call_id = %tool_call.id,
            "Executing tool"
        );
        
        // Parse arguments
        let start = std::time::Instant::now();
        let args: serde_json::Value = match serde_json::from_str(args_str) {
            Ok(value) => value,
            Err(e) => {
                let err = alms_core::AlmsError::ToolExecution(format!("Invalid arguments: {}", e));
                let _ = session_manager.append_audit(
                    session_id,
                    AuditEvent {
                        session_id,
                        run_id: None,
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
                    run_id: None,
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
                        run_id: None,
                        tool: name.to_string(),
                        decision: AuditDecision::Allow,
                        params: args,
                        result: Some(value.clone()),
                        error: None,
                        timestamp: alms_core::Timestamp::now(),
                    },
                );
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
                        run_id: None,
                        tool: name.to_string(),
                        decision: AuditDecision::Deny,
                        params: args,
                        result: None,
                        error: Some(e.to_string()),
                        timestamp: alms_core::Timestamp::now(),
                    },
                );
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
    }
    
    #[test]
    fn test_content_to_string() {
        let runtime = AgentRuntime {
            agent_id: AgentId::new(),
            config: AgentConfig::default(),
            llm: LlmClient::new(LlmConfig::default()).unwrap(),
            tools: ToolRegistry::new(),
        };
        
        let text = runtime.content_to_string(&alms_session::Content::Text("hello".to_string()));
        assert_eq!(text, "hello");
    }
}
