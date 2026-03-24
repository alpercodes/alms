use crate::context::{ContextBuilder, content_to_string};
use crate::events::{RuntimeEvent, RuntimeEventSender};
use crate::get_task_result_tool::GetTaskResultTool;
use crate::invoke_agent_tool::InvokeAgentTool;
use crate::llm_client::LlmClient;
use crate::llm_types::*;
use crate::read_subagent_session_tool::ReadSubagentSessionTool;
use crate::tools::ToolRegistry;
use crate::workspace::AgentWorkspace;
use crate::workspace_tool::WorkspaceWriteTool;
use alms_core::config::ContextConfig;
use alms_core::{AgentId, AlmsError, AlmsResult, AuditDecision, AuditEvent, TokenUsage};
use alms_session::{
    Content as SessionContent, ContextSummary, Message as SessionMessage, Role as SessionRole,
    SessionManager,
};
use tokio_util::sync::CancellationToken;
use tracing::{Span, debug, error, info, instrument, warn};
use uuid::Uuid;

/// Execution posture: controls whether tools require approval before running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Posture {
    /// Execute tools directly without approval.
    FullControl,
    /// Require explicit user approval before each tool execution (default).
    #[default]
    Guarded,
    /// Fully autonomous — tools execute without approval, no human-in-the-loop
    /// expected. Suitable for background agents, scheduled jobs, DM-triggered
    /// runs, and subagents that should run completely independently.
    Autonomous,
}

impl std::fmt::Display for Posture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Posture::FullControl => write!(f, "full_control"),
            Posture::Guarded => write!(f, "guarded"),
            Posture::Autonomous => write!(f, "autonomous"),
        }
    }
}

impl std::str::FromStr for Posture {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "full_control" => Ok(Posture::FullControl),
            "guarded" => Ok(Posture::Guarded),
            "autonomous" => Ok(Posture::Autonomous),
            other => Err(format!(
                "Invalid posture '{other}'. Must be one of: full_control, guarded, autonomous"
            )),
        }
    }
}

/// Staged system prompts for different phases of the agent loop.
///
/// Developer-controlled prompt files embedded at compile time from
/// `crates/alms-runtime/prompts/`. Not user-editable — workspace files
/// (personality, goals, memories, user) are prepended to both stages.
///
/// The initial prompt comes from `AgentConfig.system_prompt` (defaults to
/// `prompts/initial.md`, overridable per-agent). `tool_loop` is appended
/// to the initial prompt after tool results — it never replaces the
/// agent's identity.
#[derive(Debug, Clone)]
pub struct SystemPrompts {
    /// Appended to the system prompt for LLM calls after tool results.
    /// The agent's initial prompt (identity, instructions) is preserved;
    /// this adds continuation guidance on top.
    pub tool_loop: String,
}

impl Default for SystemPrompts {
    fn default() -> Self {
        Self {
            tool_loop: include_str!("../prompts/tool_loop.md").trim().to_string(),
        }
    }
}

/// Agent runtime configuration
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// System prompt used for the initial LLM call. Defaults to
    /// `prompts/initial.md`. Per-agent overrides replace this value
    /// but leave `prompts.tool_loop` unchanged.
    pub system_prompt: String,
    /// Staged system prompts for the agent loop.
    pub prompts: SystemPrompts,
    /// Maximum iterations for tool loops
    pub max_iterations: u32,
    /// Maximum tokens per response
    pub max_tokens: u32,
    /// Context window management config
    pub context_config: ContextConfig,
    /// Execution posture (full_control, guarded, or autonomous)
    pub posture: Posture,
    /// Filesystem sandbox root (default "."). Empty string = unrestricted.
    pub sandbox_root: String,
    /// Shell execution policy: "sandboxed" or "unrestricted".
    pub shell_policy: String,
    /// Enabled builtin tools. Empty = all enabled (backward compatible).
    pub enabled_tools: Vec<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            system_prompt: include_str!("../prompts/initial.md").trim().to_string(),
            prompts: SystemPrompts::default(),
            max_iterations: 10,
            max_tokens: 4096,
            context_config: ContextConfig::default(),
            posture: Posture::default(),
            sandbox_root: ".".into(),
            shell_policy: "sandboxed".into(),
            enabled_tools: Vec::new(),
        }
    }
}

/// Result of a single agent run, including the response text and accumulated token usage.
#[derive(Debug, Clone)]
pub struct RunOutput {
    pub response: String,
    pub usage: TokenUsage,
    /// Tool call records collected during this run (calls + results).
    pub tool_calls: Vec<alms_core::ToolCallRecord>,
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
    /// Per-run cancellation token for cooperative cancellation.
    cancel_token: Option<CancellationToken>,
    /// Resolved sandbox root (canonicalized). Retained so `with_workspace()` can
    /// re-register shell_exec with the workspace dir as default cwd.
    resolved_sandbox_root: Option<std::path::PathBuf>,
    /// Whether shell commands bypass sandbox cwd restriction.
    shell_unrestricted: bool,
    /// Default env vars injected into shell_exec processes (e.g. ALMS_DATA_DIR).
    /// Retained so `with_workspace()` can pass them to the re-registered shell tool.
    shell_default_env: std::collections::HashMap<String, String>,
    /// Agent name for perspective mapping in DM sessions.
    /// When set and the context_id starts with "dm:", the context builder
    /// maps messages from this agent to `Role::Assistant` so the LLM sees
    /// them as its own previous responses.
    agent_name: Option<String>,
}

impl AgentRuntime {
    /// Create new agent runtime.
    ///
    /// Returns an error if `sandbox_root` is non-empty but cannot be resolved
    /// (fail-closed: refuses to widen the sandbox silently).
    pub fn new(agent_id: AgentId, config: AgentConfig, llm: LlmClient) -> AlmsResult<Self> {
        // Resolve sandbox root: empty string = unrestricted, otherwise canonicalize.
        let sandbox_root = if config.sandbox_root.is_empty() {
            None
        } else {
            let path = std::path::PathBuf::from(&config.sandbox_root);
            let canonical = std::fs::canonicalize(&path).map_err(|e| {
                AlmsError::InvalidConfig(format!(
                    "tools.sandbox_root '{}' cannot be resolved: {}. \
                     Set sandbox_root = \"\" to explicitly opt out of sandboxing.",
                    path.display(),
                    e,
                ))
            })?;
            info!(sandbox_root = %canonical.display(), "Filesystem sandbox active");
            Some(canonical)
        };
        let shell_unrestricted = config.shell_policy == "unrestricted";
        let tools = ToolRegistry::with_builtins_sandboxed(
            sandbox_root.clone(),
            shell_unrestricted,
            &config.enabled_tools,
        );

        Ok(Self {
            agent_id,
            config,
            llm,
            tools,
            workspace: None,
            event_sender: None,
            run_id: None,
            cancel_token: None,
            resolved_sandbox_root: sandbox_root,
            shell_unrestricted,
            shell_default_env: std::collections::HashMap::new(),
            agent_name: None,
        })
    }

    /// Attach an agent workspace for persistent identity files.
    ///
    /// Also registers the `workspace_write` tool so the agent can update
    /// its `goals.md` and `memories.md` during runs, and re-registers
    /// `shell_exec` with the workspace directory as default cwd.
    ///
    /// **Note**: `workspace_write` registration goes through `ToolRegistry::register()`,
    /// which checks the `enabled_filter`. If the operator has set `tools.enabled`
    /// and `workspace_write` is not in the list, it will be silently skipped.
    /// This is intentional — the operator's allowlist should be the single
    /// source of truth for which tools are available.
    pub fn with_workspace(mut self, workspace: AgentWorkspace) -> Self {
        let tool = WorkspaceWriteTool::new(workspace.clone());
        // Subject to enabled_filter — see doc comment above.
        self.tools.register(std::sync::Arc::new(tool));

        // Ensure the workspace directory exists before canonicalizing.
        // Without this, canonicalize() fails on non-existent paths and the
        // sandbox root falls back to a relative path — causing
        // starts_with() mismatches on Windows (\\?\ prefix vs relative)
        // and potential hangs in fs_write (see #273).
        if let Err(e) = workspace.ensure_dir() {
            warn!(
                error = %e,
                workspace_dir = %workspace.dir().display(),
                "Failed to create workspace directory — fs tools may not work correctly"
            );
        }

        // Canonicalize the workspace path so the sandbox root is always
        // absolute. This prevents Windows \\?\ prefix mismatches when
        // check_sandbox_path compares the sandbox root against resolved
        // file paths.
        let ws_root = match std::fs::canonicalize(workspace.dir()) {
            Ok(canonical) => canonical,
            Err(e) => {
                warn!(
                    error = %e,
                    workspace_dir = %workspace.dir().display(),
                    "Cannot canonicalize workspace dir — using as-is"
                );
                workspace.dir().to_path_buf()
            }
        };

        let enabled = &self.config.enabled_tools;
        let tool_enabled = |name: &str| enabled.is_empty() || enabled.iter().any(|t| t == name);

        // Re-register fs_read/fs_write/fs_list sandboxed to the workspace
        // directory so file operations default to the agent's workspace
        // instead of the project root.
        if tool_enabled("fs_read") {
            self.tools
                .register(std::sync::Arc::new(alms_sandbox::FsReadTool::sandboxed(
                    ws_root.clone(),
                )));
        }
        if tool_enabled("fs_write") {
            self.tools
                .register(std::sync::Arc::new(alms_sandbox::FsWriteTool::sandboxed(
                    ws_root.clone(),
                )));
        }
        if tool_enabled("fs_list") {
            self.tools
                .register(std::sync::Arc::new(alms_sandbox::FsListTool::sandboxed(
                    ws_root.clone(),
                )));
        }

        // Re-register shell_exec with workspace dir as default cwd and
        // gateway-provided default env vars (ALMS_DATA_DIR, etc.).
        if tool_enabled("shell_exec") {
            let mut shell_tool = alms_sandbox::ShellExecTool::with_policy(
                self.resolved_sandbox_root.clone(),
                self.shell_unrestricted,
            )
            .with_default_cwd(ws_root);
            if !self.shell_default_env.is_empty() {
                shell_tool = shell_tool.with_default_env(self.shell_default_env.clone());
            }
            self.tools.register(std::sync::Arc::new(shell_tool));
        }

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

    /// Register the `read_subagent_session` tool for on-demand subagent context retrieval.
    pub fn with_read_subagent_session(self, tool: ReadSubagentSessionTool) -> Self {
        self.tools.register(std::sync::Arc::new(tool));
        self
    }

    /// Register the `send_message` tool for peer-to-peer agent messaging.
    pub fn with_send_message(self, tool: crate::send_message_tool::SendMessageTool) -> Self {
        self.tools.register(std::sync::Arc::new(tool));
        self
    }

    /// Register the `list_agents` tool for agent discovery.
    pub fn with_list_agents(self, tool: crate::list_agents_tool::ListAgentsTool) -> Self {
        self.tools.register(std::sync::Arc::new(tool));
        self
    }

    /// Register the `read_messages` tool for reading DM conversation history.
    pub fn with_read_messages(self, tool: crate::read_messages_tool::ReadMessagesTool) -> Self {
        self.tools.register(std::sync::Arc::new(tool));
        self
    }

    /// Register the `ignore_message` tool so agents can decline to respond.
    pub fn with_ignore_message(self, tool: crate::ignore_message_tool::IgnoreMessageTool) -> Self {
        self.tools.register(std::sync::Arc::new(tool));
        self
    }

    /// Set the run ID for audit event correlation.
    pub fn with_run_id(mut self, run_id: alms_core::RunId) -> Self {
        self.run_id = Some(run_id);
        self
    }

    /// Attach a cancellation token for cooperative run cancellation.
    pub fn with_cancel_token(mut self, token: CancellationToken) -> Self {
        self.cancel_token = Some(token);
        self
    }

    /// Set default environment variables for `shell_exec` processes.
    ///
    /// These are injected into every process spawned by `shell_exec` after
    /// `env_clear()`, so the spawned CLI commands can discover the gateway's
    /// data directory (`ALMS_DATA_DIR`) and workspace directory
    /// (`ALMS_WORKSPACE_DIR`) even when cwd is sandboxed elsewhere.
    ///
    /// Re-registers the `shell_exec` tool immediately so that unnamed agents
    /// (which skip `with_workspace()`) still receive the environment variables.
    pub fn with_shell_default_env(
        mut self,
        env: std::collections::HashMap<String, String>,
    ) -> Self {
        self.shell_default_env = env;

        // Re-register shell_exec with the new default env so unnamed agents
        // (which never call with_workspace()) still get ALMS_DATA_DIR injected.
        let enabled = &self.config.enabled_tools;
        let shell_enabled = enabled.is_empty() || enabled.iter().any(|t| t == "shell_exec");
        if shell_enabled && self.tools.contains("shell_exec") {
            let shell_tool = alms_sandbox::ShellExecTool::with_policy(
                self.resolved_sandbox_root.clone(),
                self.shell_unrestricted,
            )
            .with_default_env(self.shell_default_env.clone());
            self.tools.register(std::sync::Arc::new(shell_tool));
        }

        self
    }

    /// Set the agent name for perspective mapping in DM sessions.
    ///
    /// When the context_id starts with `"dm:"`, the context builder uses this
    /// name to map messages: messages where `from_agent == agent_name` become
    /// `Role::Assistant` (the LLM's own previous responses), while messages from
    /// other agents stay as `Role::User`.
    pub fn with_agent_name(mut self, name: String) -> Self {
        self.agent_name = Some(name);
        self
    }

    /// Emit a status event to the gateway layer (best-effort, never fails).
    fn emit_status(&self, phase: &str, detail: Option<&str>) {
        if let Some(ref tx) = self.event_sender {
            let _ = tx.send(crate::events::RuntimeEvent::Status {
                phase: phase.to_string(),
                detail: detail.map(|d| d.to_string()),
            });
        }
    }

    /// Create with default config
    pub fn with_defaults(agent_id: AgentId) -> AlmsResult<Self> {
        let llm = LlmClient::from_env()?;
        Self::new(agent_id, AgentConfig::default(), llm)
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

        // Build context first (reads history without current input to avoid double-counting),
        // then persist the user message so it survives agent loop failures.
        self.emit_status("building_context", None);
        let history = self
            .build_context(session_manager, &session.id, context_id, &input)
            .await;

        let user_msg = SessionMessage {
            id: uuid::Uuid::new_v4().to_string(),
            role: SessionRole::User,
            content: alms_session::Content::Text(input.clone()),
            timestamp: alms_core::Timestamp::now(),
            metadata: None,
        };
        session_manager.append_message(session.id, user_msg)?;

        self.finish_run(session_manager, session.id, context_id, history)
            .await
    }

    /// Run the agent on a pre-existing shared session (e.g. a DM session).
    ///
    /// Unlike `run()`, this method:
    /// - Looks up the session by `SessionId` directly instead of creating one
    ///   via `get_or_create(agent_id, context_id)`. This ensures the agent uses
    ///   the shared DM session created by `MessageBus`, not a new empty one.
    /// - Skips persisting the input message because the `MessageBus` already
    ///   wrote it to the session with `from_agent` metadata.
    #[instrument(
        level = "info",
        skip(self, session_manager, input),
        fields(
            agent_id = %self.agent_id.0,
            session_id = %session_id.0,
            context_id = %context_id,
            max_iterations = %self.config.max_iterations
        )
    )]
    pub async fn run_on_session(
        &self,
        session_manager: &SessionManager,
        session_id: alms_core::SessionId,
        context_id: &str,
        input: &str,
    ) -> AlmsResult<RunOutput> {
        info!(
            target: "agent::run_on_session_start",
            agent_id = %self.agent_id.0,
            session_id = %session_id.0,
            context_id = %context_id,
            input_len = input.len(),
            "Agent run_on_session started (shared session, input already persisted)"
        );

        // Verify the session exists -- fail loudly if not.
        session_manager.get(session_id)?;

        // Build context: the input message is already in the session history
        // (written by MessageBus), so we pass an empty string as the current
        // input to avoid duplicating it in the context window.
        self.emit_status("building_context", None);
        let history = self
            .build_context(session_manager, &session_id, context_id, "")
            .await;

        // Do NOT persist the input message -- it is already in the session.

        self.finish_run(session_manager, session_id, context_id, history)
            .await
    }

    /// Extract the peer agent name from a DM context_id.
    ///
    /// The context_id has the form `"dm:{name1}:{name2}"` where names are
    /// sorted alphabetically.  The peer is whichever name is NOT this agent's
    /// own `agent_name`.  Returns `None` if the context_id is malformed or
    /// `agent_name` is not set.
    fn dm_peer_name(&self, context_id: &str) -> Option<String> {
        let name = self.agent_name.as_deref()?;
        let parts: Vec<&str> = context_id.splitn(3, ':').collect();
        if parts.len() != 3 {
            return None;
        }
        // parts[0] == "dm", parts[1] and parts[2] are the two agent names.
        if parts[1] == name {
            Some(parts[2].to_string())
        } else if parts[2] == name {
            Some(parts[1].to_string())
        } else {
            None
        }
    }

    /// Build `from_agent` metadata for DM sessions so that `read_messages`
    /// can attribute system markers (errors, cancellations) to the correct
    /// agent.  Returns `None` for non-DM sessions or when `agent_name` is
    /// not set.
    fn dm_marker_metadata(&self, is_dm: bool) -> Option<serde_json::Value> {
        if is_dm {
            self.agent_name.as_ref().map(|name| {
                serde_json::json!({
                    "from_agent": name,
                    "from_agent_id": self.agent_id.0.to_string(),
                    "message_type": "dm",
                })
            })
        } else {
            None
        }
    }

    /// Shared tail for `run()` and `run_on_session()`: executes the agent loop
    /// and persists the result (assistant response, cancellation marker, or
    /// error marker) to the session.
    ///
    /// Tool call records are always returned regardless of success or failure,
    /// so the gateway can persist partial execution history for debugging.
    async fn finish_run(
        &self,
        session_manager: &SessionManager,
        session_id: alms_core::SessionId,
        context_id: &str,
        history: AlmsResult<Vec<LlmMessage>>,
    ) -> AlmsResult<RunOutput> {
        let is_dm = context_id.starts_with("dm:");
        let include_user = Self::is_user_facing_context(context_id);

        let (tool_calls, result) = match history {
            Ok(h) => {
                self.agent_loop(session_manager, session_id, h, is_dm, include_user)
                    .await
            }
            Err(e) => (Vec::new(), Err(e)),
        };

        match result {
            Ok((response, usage)) => {
                // Skip persisting an assistant message when the response is
                // empty. This happens when the agent used `ignore_message` to
                // decline responding — there is nothing to record.
                //
                // For DM sessions: do NOT persist the agent's final text
                // response. The only messages in the shared DM session should
                // be those written via `send_message` (through MessageBus) and
                // error/cancellation markers. The agent's text response is
                // internal processing noise — the agent was told to use
                // `send_message` to reply.
                if !response.is_empty() && !is_dm {
                    let reply_msg = SessionMessage {
                        id: uuid::Uuid::new_v4().to_string(),
                        role: SessionRole::Assistant,
                        content: alms_session::Content::Text(response.clone()),
                        timestamp: alms_core::Timestamp::now(),
                        metadata: None,
                    };
                    session_manager.append_message(session_id, reply_msg)?;
                } else if !response.is_empty() && is_dm {
                    debug!(
                        context_id = %context_id,
                        "DM session — skipping text response storage (agent should use send_message)"
                    );
                }

                info!(
                    "Agent {} completed for context {} (prompt={} completion={} tokens)",
                    self.agent_id.0, context_id, usage.prompt_tokens, usage.completion_tokens
                );

                Ok(RunOutput {
                    response,
                    usage,
                    tool_calls,
                })
            }
            Err(AlmsError::Cancelled) => {
                // In DM sessions, attach from_agent metadata and use Role::User
                // so read_messages perspective mapping works consistently — all
                // messages in a shared DM session must be Role::User.
                let role = if is_dm && self.agent_name.is_some() {
                    SessionRole::User
                } else {
                    SessionRole::Assistant
                };
                let cancel_msg = SessionMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    role,
                    content: alms_session::Content::Text("[Run cancelled by user]".to_string()),
                    timestamp: alms_core::Timestamp::now(),
                    metadata: self.dm_marker_metadata(is_dm),
                };
                if let Err(append_err) = session_manager.append_message(session_id, cancel_msg) {
                    warn!(
                        "Failed to persist cancellation marker to session: {}",
                        append_err
                    );
                }
                Err(AlmsError::CancelledWithToolCalls { tool_calls })
            }
            Err(e) => {
                // Write a sanitized error marker so the session reflects the failed attempt
                // without leaking sensitive details (API keys, URLs) into LLM context.
                // In DM sessions, use Role::User with from_agent metadata so
                // perspective mapping works — all messages in shared DM sessions
                // must be Role::User.
                let safe_reason = sanitize_error_for_session(&e);
                let role = if is_dm && self.agent_name.is_some() {
                    SessionRole::User
                } else {
                    SessionRole::Assistant
                };
                let error_msg = SessionMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    role,
                    content: alms_session::Content::Text(format!("[Run failed: {}]", safe_reason)),
                    timestamp: alms_core::Timestamp::now(),
                    metadata: self.dm_marker_metadata(is_dm),
                };
                if let Err(append_err) = session_manager.append_message(session_id, error_msg) {
                    warn!("Failed to persist error marker to session: {}", append_err);
                }

                Err(AlmsError::FailedWithToolCalls {
                    source: Box::new(e),
                    tool_calls,
                })
            }
        }
    }

    /// Assemble the full system prompt for a given stage, prepending workspace
    /// files if attached.
    ///
    /// When `include_user` is false, `user.md` is omitted from the workspace
    /// prefix. This is used for non-user-facing sessions (DM, subagent, job).
    fn assemble_system_prompt(&self, base_prompt: &str, include_user: bool) -> String {
        if let Some(ref ws) = self.workspace {
            let prefix = ws.build_system_prompt_prefix(include_user);
            if prefix.is_empty() {
                base_prompt.to_string()
            } else {
                format!("{}\n\n{}", prefix, base_prompt)
            }
        } else {
            base_prompt.to_string()
        }
    }

    /// Returns true if the given context_id represents a user-facing session
    /// (web chat, Telegram, etc.) where `user.md` should be included in the
    /// system prompt.  Non-user-facing contexts (DM, subagent, job) return
    /// false.
    fn is_user_facing_context(context_id: &str) -> bool {
        // These prefixes indicate non-user-facing sessions.
        !(context_id.starts_with("dm:")
            || context_id.starts_with("subagent_")
            || context_id.starts_with("job_"))
    }

    /// Build context window for LLM using ContextBuilder.
    ///
    /// For the `sliding-summary` strategy this is async because it may call the
    /// LLM to compress old messages into a rolling summary.
    ///
    /// For DM sessions (context_id starts with `"dm:"`), perspective mapping is
    /// applied: messages from this agent become `Role::Assistant` so the LLM
    /// sees them as its own previous responses.
    async fn build_context(
        &self,
        session_manager: &SessionManager,
        session_id: &alms_core::SessionId,
        context_id: &str,
        input: &str,
    ) -> AlmsResult<Vec<LlmMessage>> {
        let include_user = Self::is_user_facing_context(context_id);
        let mut system_prompt =
            self.assemble_system_prompt(&self.config.system_prompt, include_user);

        // For DM sessions, append instructions telling the agent how to reply.
        // The agent's text response will NOT be stored in the shared session,
        // so it must use `send_message` to communicate with the peer.
        if context_id.starts_with("dm:")
            && let Some(peer) = self.dm_peer_name(context_id)
        {
            let dm_template = include_str!("../prompts/dm_recipient.md");
            let dm_addendum = format!("\n\n{}", dm_template.trim().replace("{peer}", &peer));
            system_prompt.push_str(&dm_addendum);
            debug!(
                peer = %peer,
                context_id = %context_id,
                "Injected DM recipient system prompt"
            );
        }

        let history = match session_manager.get_history(*session_id) {
            Ok(h) => h,
            Err(e) => {
                error!(session_id = ?session_id, error = %e, "Failed to load session history — running without context");
                Vec::new()
            }
        };

        // For sliding-summary, attempt to compress old messages before building context.
        // On failure we log a warning and fall back (None summary → truncate behaviour).
        let summary_text: Option<String> =
            if self.config.context_config.strategy == "sliding-summary" {
                self.emit_status("summarizing", None);
                let current = session_manager.get_summary(*session_id).unwrap_or_default();
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

        // For DM sessions, apply perspective mapping so the LLM sees its own
        // previous messages as Role::Assistant instead of Role::User.
        let perspective = if context_id.starts_with("dm:") {
            if let Some(ref name) = self.agent_name {
                debug!(
                    agent_name = %name,
                    context_id = %context_id,
                    "Applying perspective mapping for DM session"
                );
                Some(name.as_str())
            } else {
                warn!(
                    context_id = %context_id,
                    "DM session detected but agent_name not set — perspective mapping skipped"
                );
                None
            }
        } else {
            None
        };

        Ok(builder.build_with_perspective(
            &system_prompt,
            &history,
            input,
            summary_text.as_deref(),
            perspective,
        ))
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
            include_str!("../prompts/summarizer.md").trim(),
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
        skip(self, session_manager, messages),
        fields(agent_id = %self.agent_id.0, session_id = %session_id.0)
    )]
    async fn agent_loop(
        &self,
        session_manager: &SessionManager,
        session_id: alms_core::SessionId,
        mut messages: Vec<LlmMessage>,
        is_dm: bool,
        include_user: bool,
    ) -> (
        Vec<alms_core::ToolCallRecord>,
        AlmsResult<(String, TokenUsage)>,
    ) {
        let mut iterations = 0;
        let mut total_usage = TokenUsage::default();
        let mut tool_call_records: Vec<alms_core::ToolCallRecord> = Vec::new();
        let mut tool_seq: u32 = 0;

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
                    Ok(("[Max iterations reached]".to_string(), total_usage)),
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
            self.emit_status("calling_llm", None);
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

                // Emit status: list tool names being executed.
                let tool_names: Vec<&str> = tool_calls
                    .iter()
                    .map(|tc| tc.function.name.as_str())
                    .collect();
                let detail = tool_names.join(", ");
                self.emit_status("executing_tools", Some(&detail));

                // Checkpoint C: tool execution with cancellation support.
                //
                // Full-control / Autonomous posture: run all tool calls concurrently
                // so background invoke_agent calls don't block each other and
                // independent tools finish in parallel.
                //
                // Guarded posture: run tool calls sequentially so the user sees one
                // approval prompt at a time rather than all at once.
                let results = match self.config.posture {
                    Posture::Guarded => {
                        // Note: cancellation during active tool execution (post-approval)
                        // is not detected until the tool completes, which is acceptable
                        // since guarded tools block on approval (which IS cancellation-aware).
                        let mut results = Vec::with_capacity(tool_calls.len());
                        for tc in &tool_calls {
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
                        let tool_future = futures::future::join_all(
                            tool_calls
                                .iter()
                                .map(|tc| self.execute_tool_call(tc, session_manager, session_id)),
                        );
                        if let Some(ref token) = self.cancel_token {
                            tokio::select! {
                                r = tool_future => r,
                                _ = token.cancelled() => return (tool_call_records, Err(AlmsError::Cancelled)),
                            }
                        } else {
                            tool_future.await
                        }
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

                // Check if any tool result is an ignore_message marker.
                // When detected, end the run early without producing a response.
                let ignored = tool_calls
                    .iter()
                    .any(|tc| tc.function.name == "ignore_message");
                if ignored {
                    info!("Agent declined to respond via ignore_message — ending run early");
                    return (tool_call_records, Ok((String::new(), total_usage)));
                }

                // Append tool_loop instructions to the system prompt for
                // subsequent iterations. The agent's identity (initial prompt +
                // workspace prefix) is preserved; tool_loop adds continuation
                // guidance on top.
                if !messages.is_empty() && messages[0].role == "system" {
                    let combined = format!(
                        "{}\n\n{}",
                        self.config.system_prompt, self.config.prompts.tool_loop
                    );
                    let tool_loop_prompt = self.assemble_system_prompt(&combined, include_user);
                    messages[0] = LlmMessage::system(tool_loop_prompt);
                }

                continue;
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
    async fn stream_llm_call(
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

    /// Get tool registry reference
    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }
}

/// Produce a safe error description for session history.
///
/// Strips details that could contain secrets (API keys, URLs, headers)
/// while preserving the error category so retries have useful context.
fn sanitize_error_for_session(err: &AlmsError) -> String {
    match err {
        AlmsError::Runtime(msg) => {
            // Runtime errors may contain API URLs, keys, or raw HTTP details.
            if msg.contains("401") || msg.contains("403") {
                "LLM authentication error".to_string()
            } else if msg.contains("429") {
                "LLM rate limit exceeded".to_string()
            } else if msg.contains("timeout") || msg.contains("timed out") {
                "LLM request timed out".to_string()
            } else if msg.contains("context") || msg.contains("summary") {
                "Context building failed".to_string()
            } else {
                "Runtime error".to_string()
            }
        }
        AlmsError::ToolExecution(msg) => {
            // Tool name is safe, but output may contain secrets.
            let safe = msg.split(':').next().unwrap_or("unknown tool");
            format!("Tool execution failed: {safe}")
        }
        AlmsError::SessionNotFound(_) => "Session not found".to_string(),
        AlmsError::InvalidConfig(_) => "Invalid configuration".to_string(),
        AlmsError::Cancelled => "Run cancelled by user".to_string(),
        AlmsError::Io(_) => "I/O error".to_string(),
        _ => "Internal error".to_string(),
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
        assert!(!config.system_prompt.is_empty());
        assert_eq!(config.posture, Posture::Guarded);
    }

    #[tokio::test]
    async fn test_stream_llm_call_emits_token_deltas() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
        let config = LlmConfig {
            mock: true,
            ..LlmConfig::default()
        };
        let runtime = AgentRuntime {
            agent_id: AgentId::new(),
            config: AgentConfig::default(),
            llm: LlmClient::new(config).unwrap(),
            tools: ToolRegistry::new(),
            workspace: None,
            event_sender: Some(tx),
            run_id: None,
            cancel_token: None,
            resolved_sandbox_root: None,
            shell_unrestricted: true,
            shell_default_env: std::collections::HashMap::new(),
            agent_name: None,
        };

        let request =
            CompletionRequest::new("test").with_messages(vec![LlmMessage::user("hello world")]);

        let (content, tool_calls, _usage) = runtime.stream_llm_call(request).await.unwrap();

        // Content should be the reassembled mock response
        assert_eq!(content.as_deref(), Some("[mock] hello world"));
        // No tool calls from mock
        assert!(tool_calls.is_none());

        // Verify TokenDelta events were emitted (one per word chunk)
        let mut deltas = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let RuntimeEvent::TokenDelta { delta, .. } = event {
                deltas.push(delta);
            }
        }
        assert!(deltas.len() >= 2, "should emit multiple token deltas");
        let reassembled: String = deltas.concat();
        assert_eq!(reassembled, "[mock] hello world");
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
            cancel_token: None,
            resolved_sandbox_root: None,
            shell_unrestricted: true,
            shell_default_env: std::collections::HashMap::new(),
            agent_name: None,
        };

        let session_config = SessionConfig::default();
        let session_manager = SessionManager::new(session_config);
        let session = session_manager.get_or_create(runtime.agent_id, "test");

        let messages = runtime
            .build_context(&session_manager, &session.id, "test", "hello")
            .await
            .unwrap();
        // system prompt + current input = 2
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
    }

    #[tokio::test]
    async fn test_build_context_dm_perspective_mapping() {
        let runtime = AgentRuntime {
            agent_id: AgentId::new(),
            config: AgentConfig::default(),
            llm: LlmClient::new(LlmConfig::default()).unwrap(),
            tools: ToolRegistry::new(),
            workspace: None,
            event_sender: None,
            run_id: None,
            cancel_token: None,
            resolved_sandbox_root: None,
            shell_unrestricted: true,
            shell_default_env: std::collections::HashMap::new(),
            agent_name: Some("bob".to_string()),
        };

        let session_config = SessionConfig::default();
        let session_manager = SessionManager::new(session_config);

        // Create a shared DM session and populate it with messages from both agents
        let dm_context = "dm:alice:bob";
        let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
        let session = session_manager.get_or_create_shared(session_id, dm_context);

        // Alice's message (from_agent = "alice") — should stay User for Bob's perspective
        session_manager
            .append_message(
                session.id,
                alms_session::Message {
                    id: uuid::Uuid::new_v4().to_string(),
                    role: alms_session::Role::User,
                    content: alms_session::Content::Text("Hello Bob!".to_string()),
                    timestamp: alms_core::Timestamp::now(),
                    metadata: Some(serde_json::json!({
                        "from_agent": "alice",
                        "message_type": "dm",
                    })),
                },
            )
            .unwrap();

        // Bob's message (from_agent = "bob") — should become Assistant for Bob's perspective
        session_manager
            .append_message(
                session.id,
                alms_session::Message {
                    id: uuid::Uuid::new_v4().to_string(),
                    role: alms_session::Role::User,
                    content: alms_session::Content::Text("Hi Alice!".to_string()),
                    timestamp: alms_core::Timestamp::now(),
                    metadata: Some(serde_json::json!({
                        "from_agent": "bob",
                        "message_type": "dm",
                    })),
                },
            )
            .unwrap();

        let messages = runtime
            .build_context(&session_manager, &session.id, dm_context, "What's up?")
            .await
            .unwrap();

        // system + 2 history + current input = 4
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, "system");
        // Alice's message stays as "user" from Bob's perspective
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[1].content_str(), "Hello Bob!");
        // Bob's own message becomes "assistant" from Bob's perspective
        assert_eq!(messages[2].role, "assistant");
        assert_eq!(messages[2].content_str(), "Hi Alice!");
        // Current input
        assert_eq!(messages[3].role, "user");
        assert_eq!(messages[3].content_str(), "What's up?");
    }

    #[tokio::test]
    async fn test_build_context_non_dm_no_perspective() {
        // When context_id does NOT start with "dm:", no perspective mapping should occur
        let runtime = AgentRuntime {
            agent_id: AgentId::new(),
            config: AgentConfig::default(),
            llm: LlmClient::new(LlmConfig::default()).unwrap(),
            tools: ToolRegistry::new(),
            workspace: None,
            event_sender: None,
            run_id: None,
            cancel_token: None,
            resolved_sandbox_root: None,
            shell_unrestricted: true,
            shell_default_env: std::collections::HashMap::new(),
            agent_name: Some("bob".to_string()),
        };

        let session_config = SessionConfig::default();
        let session_manager = SessionManager::new(session_config);
        let session = session_manager.get_or_create(runtime.agent_id, "regular-context");

        // Add a message with from_agent metadata (shouldn't matter for non-DM)
        session_manager
            .append_message(
                session.id,
                alms_session::Message {
                    id: uuid::Uuid::new_v4().to_string(),
                    role: alms_session::Role::User,
                    content: alms_session::Content::Text("Hello".to_string()),
                    timestamp: alms_core::Timestamp::now(),
                    metadata: Some(serde_json::json!({"from_agent": "bob"})),
                },
            )
            .unwrap();

        let messages = runtime
            .build_context(&session_manager, &session.id, "regular-context", "hi")
            .await
            .unwrap();

        // system + 1 history + current = 3
        assert_eq!(messages.len(), 3);
        // No perspective mapping: message stays as "user" even though from_agent == "bob"
        assert_eq!(messages[1].role, "user");
    }

    #[test]
    fn test_sanitize_error_runtime_auth() {
        let err = AlmsError::Runtime("HTTP 401 Unauthorized at https://api.example.com".into());
        assert_eq!(sanitize_error_for_session(&err), "LLM authentication error");
    }

    #[test]
    fn test_sanitize_error_runtime_rate_limit() {
        let err = AlmsError::Runtime("429 Too Many Requests".into());
        assert_eq!(sanitize_error_for_session(&err), "LLM rate limit exceeded");
    }

    #[test]
    fn test_sanitize_error_runtime_timeout() {
        let err = AlmsError::Runtime("request timed out after 60s".into());
        assert_eq!(sanitize_error_for_session(&err), "LLM request timed out");
    }

    #[test]
    fn test_sanitize_error_runtime_generic() {
        let err = AlmsError::Runtime("some secret-key=abc123 in raw error".into());
        assert_eq!(sanitize_error_for_session(&err), "Runtime error");
    }

    #[test]
    fn test_sanitize_error_tool_strips_output() {
        let err = AlmsError::ToolExecution("shell_exec: secret output here".into());
        assert_eq!(
            sanitize_error_for_session(&err),
            "Tool execution failed: shell_exec"
        );
    }

    #[test]
    fn test_sanitize_error_context_building() {
        let err = AlmsError::Runtime("failed to build context window".into());
        assert_eq!(sanitize_error_for_session(&err), "Context building failed");
    }

    #[tokio::test]
    async fn test_run_persists_user_message_on_failure() {
        // Use mock LLM that will produce a response, but we can verify
        // the user message is persisted to history.
        let config = LlmConfig {
            mock: true,
            ..LlmConfig::default()
        };
        let session_config = SessionConfig::default();
        let session_manager = SessionManager::new(session_config);
        let llm = LlmClient::new(config).unwrap();
        let agent_id = AgentId::new();
        let runtime = AgentRuntime::new(agent_id, AgentConfig::default(), llm).unwrap();

        // Run with mock LLM (succeeds)
        let result = runtime
            .run(&session_manager, "test-context", "hello agent")
            .await;
        assert!(result.is_ok());

        // Verify the user message was persisted in session history
        let session = session_manager.get_or_create(agent_id, "test-context");
        let history = session_manager.get_history(session.id).unwrap();
        assert!(
            history.iter().any(|m| m.role == alms_session::Role::User
                && matches!(&m.content, alms_session::Content::Text(t) if t == "hello agent")),
            "User message should be persisted in session history"
        );
    }

    #[tokio::test]
    async fn test_guarded_posture_sequential_approvals() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
        let config = LlmConfig {
            mock: true,
            ..LlmConfig::default()
        };
        let tools = crate::ToolRegistry::with_builtins_sandboxed(None, true, &["echo".to_string()]);
        let session_config = SessionConfig::default();
        let session_manager = SessionManager::new(session_config);
        let agent_id = AgentId::new();
        let session = session_manager.get_or_create(agent_id, "test");

        let runtime = AgentRuntime {
            agent_id,
            config: AgentConfig {
                posture: Posture::Guarded,
                ..AgentConfig::default()
            },
            llm: LlmClient::new(config).unwrap(),
            tools,
            workspace: None,
            event_sender: Some(tx),
            run_id: None,
            cancel_token: None,
            resolved_sandbox_root: None,
            shell_unrestricted: true,
            shell_default_env: std::collections::HashMap::new(),
            agent_name: None,
        };

        let tool_calls = vec![
            ToolCall {
                id: "tc1".to_string(),
                function: FunctionCall {
                    name: "echo".to_string(),
                    arguments: r#"{"text":"first"}"#.to_string(),
                },
            },
            ToolCall {
                id: "tc2".to_string(),
                function: FunctionCall {
                    name: "echo".to_string(),
                    arguments: r#"{"text":"second"}"#.to_string(),
                },
            },
        ];

        // Track the order: approval_count increments only after each approval resolves.
        let approval_count = Arc::new(AtomicUsize::new(0));
        let approval_count_clone = approval_count.clone();

        // Spawn the approval handler: approve each request, verify sequential ordering.
        let handler = tokio::spawn(async move {
            let mut approval_order = Vec::new();
            while let Some(event) = rx.recv().await {
                if let RuntimeEvent::ApprovalRequired {
                    decision_tx, tool, ..
                } = event
                {
                    let count = approval_count_clone.load(Ordering::SeqCst);
                    approval_order.push((tool, count));
                    decision_tx.send(true).unwrap();
                    approval_count_clone.fetch_add(1, Ordering::SeqCst);
                }
            }
            approval_order
        });

        // Execute tool calls sequentially (guarded path)
        let mut results = Vec::new();
        for tc in &tool_calls {
            results.push(
                runtime
                    .execute_tool_call(tc, &session_manager, session.id)
                    .await,
            );
        }

        // Both should succeed
        assert!(results[0].is_ok());
        assert!(results[1].is_ok());

        // Drop the runtime's sender to close the channel so the handler finishes
        drop(runtime);

        let approval_order = handler.await.unwrap();
        // The second approval should have seen count=1 (first was resolved),
        // proving sequential execution.
        assert_eq!(approval_order.len(), 2);
        assert_eq!(approval_order[0].1, 0, "First approval should see count=0");
        assert_eq!(
            approval_order[1].1, 1,
            "Second approval should see count=1 (first resolved)"
        );
    }

    #[test]
    fn test_invalid_sandbox_root_fails_closed() {
        let config = crate::llm_types::LlmConfig {
            mock: true,
            ..crate::llm_types::LlmConfig::default()
        };
        let llm = LlmClient::new(config).unwrap();
        let agent_config = AgentConfig {
            sandbox_root: "/nonexistent/path/that/does/not/exist".into(),
            ..AgentConfig::default()
        };
        let result = AgentRuntime::new(AgentId::new(), agent_config, llm);
        assert!(result.is_err(), "Should fail when sandbox_root is invalid");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("sandbox_root"),
            "Error should mention sandbox_root: {err}"
        );
    }

    #[test]
    fn test_empty_sandbox_root_means_unrestricted() {
        let config = crate::llm_types::LlmConfig {
            mock: true,
            ..crate::llm_types::LlmConfig::default()
        };
        let llm = LlmClient::new(config).unwrap();
        let agent_config = AgentConfig {
            sandbox_root: "".into(),
            ..AgentConfig::default()
        };
        let result = AgentRuntime::new(AgentId::new(), agent_config, llm);
        assert!(
            result.is_ok(),
            "Empty sandbox_root should mean unrestricted"
        );
    }

    /// Integration test for `run_on_session`: verifies that the agent uses
    /// the shared DM session directly and does NOT persist its text response
    /// (only `send_message`-written messages belong in the shared DM session).
    #[tokio::test]
    async fn test_run_on_session_skips_text_response_for_dm() {
        let config = LlmConfig {
            mock: true,
            ..LlmConfig::default()
        };
        let session_config = SessionConfig::default();
        let session_manager = SessionManager::new(session_config);
        let llm = LlmClient::new(config).unwrap();
        let bob_id = AgentId::new();

        let agent_config = AgentConfig {
            sandbox_root: "".into(),
            ..AgentConfig::default()
        };
        let runtime = AgentRuntime::new(bob_id, agent_config, llm)
            .unwrap()
            .with_agent_name("bob".to_string());

        // Simulate what MessageBus does: create a shared DM session and
        // write the sender's message into it.
        let dm_context = "dm:alice:bob";
        let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
        let _session = session_manager.get_or_create_shared(session_id, dm_context);

        let sender_msg = alms_session::Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: alms_session::Role::User,
            content: alms_session::Content::Text("Hello Bob!".to_string()),
            timestamp: alms_core::Timestamp::now(),
            metadata: Some(serde_json::json!({
                "from_agent": "alice",
                "from_agent_id": AgentId::new().0.to_string(),
                "message_type": "dm",
            })),
        };
        session_manager
            .append_message(session_id, sender_msg)
            .unwrap();

        // Before run: session has exactly 1 message (from alice via MessageBus).
        let history_before = session_manager.get_history(session_id).unwrap();
        assert_eq!(
            history_before.len(),
            1,
            "Pre-condition: 1 message from MessageBus"
        );

        // Run on the shared session (as the recipient agent would).
        let result = runtime
            .run_on_session(&session_manager, session_id, dm_context, "Hello Bob!")
            .await;
        assert!(result.is_ok(), "run_on_session should succeed");
        let output = result.unwrap();
        assert!(
            !output.response.is_empty(),
            "Mock LLM should produce a non-empty response"
        );

        // After run: the session should still have only 1 message.
        // The agent's text response is NOT persisted to the DM session —
        // only send_message-written messages belong there.
        let history_after = session_manager.get_history(session_id).unwrap();
        assert_eq!(
            history_after.len(),
            1,
            "DM text response should NOT be stored. Expected 1 (alice's input only). Found: {}",
            history_after.len()
        );

        // The one message is alice's original input.
        assert_eq!(history_after[0].role, alms_session::Role::User);
        let alice_meta = history_after[0].metadata.as_ref().unwrap();
        assert_eq!(alice_meta["from_agent"], "alice");
    }

    /// Verify that non-DM runs still persist the agent's text response normally.
    #[tokio::test]
    async fn test_non_dm_run_persists_text_response() {
        let config = LlmConfig {
            mock: true,
            ..LlmConfig::default()
        };
        let session_config = SessionConfig::default();
        let session_manager = SessionManager::new(session_config);
        let llm = LlmClient::new(config).unwrap();
        let agent_id = AgentId::new();

        let agent_config = AgentConfig {
            sandbox_root: "".into(),
            ..AgentConfig::default()
        };
        let runtime = AgentRuntime::new(agent_id, agent_config, llm).unwrap();

        // Run on a normal (non-DM) context.
        let result = runtime
            .run(&session_manager, "normal-context", "Hello agent")
            .await;
        assert!(result.is_ok(), "Normal run should succeed");

        // The session should have 2 messages: user input + assistant response.
        let session = session_manager.get_or_create(agent_id, "normal-context");
        let history = session_manager.get_history(session.id).unwrap();
        assert_eq!(
            history.len(),
            2,
            "Non-DM run should persist both user input and assistant response. Found: {}",
            history.len()
        );
        assert_eq!(history[0].role, alms_session::Role::User);
        assert_eq!(history[1].role, alms_session::Role::Assistant);
    }

    /// Verify that `dm_marker_metadata` returns `from_agent` metadata for DM
    /// sessions and `None` for non-DM sessions.  This metadata is attached to
    /// error/cancellation markers so `read_messages` can attribute them.
    #[test]
    fn test_dm_marker_metadata() {
        let config = crate::llm_types::LlmConfig {
            mock: true,
            ..crate::llm_types::LlmConfig::default()
        };
        let llm = LlmClient::new(config).unwrap();
        let agent_id = AgentId::new();
        let agent_config = AgentConfig {
            sandbox_root: "".into(),
            ..AgentConfig::default()
        };

        // Without agent_name: always None regardless of is_dm.
        let rt_no_name = AgentRuntime::new(agent_id, agent_config.clone(), llm.clone()).unwrap();
        assert!(rt_no_name.dm_marker_metadata(true).is_none());
        assert!(rt_no_name.dm_marker_metadata(false).is_none());

        // With agent_name: returns metadata only when is_dm is true.
        let rt_named = AgentRuntime::new(agent_id, agent_config, llm)
            .unwrap()
            .with_agent_name("bob".to_string());

        let meta = rt_named.dm_marker_metadata(true);
        assert!(
            meta.is_some(),
            "DM session with agent_name should produce metadata"
        );
        let meta = meta.unwrap();
        assert_eq!(meta["from_agent"], "bob");
        assert_eq!(meta["from_agent_id"], agent_id.0.to_string());
        assert_eq!(meta["message_type"], "dm");

        // Non-DM: no metadata even with agent_name set.
        assert!(rt_named.dm_marker_metadata(false).is_none());
    }

    /// Verify that `run_on_session` fails with SessionNotFound if the session
    /// does not exist (rather than silently creating a new empty session).
    #[tokio::test]
    async fn test_run_on_session_fails_if_session_missing() {
        let config = LlmConfig {
            mock: true,
            ..LlmConfig::default()
        };
        let session_config = SessionConfig::default();
        let session_manager = SessionManager::new(session_config);
        let llm = LlmClient::new(config).unwrap();
        let agent_id = AgentId::new();

        let agent_config = AgentConfig {
            sandbox_root: "".into(),
            ..AgentConfig::default()
        };
        let runtime = AgentRuntime::new(agent_id, agent_config, llm)
            .unwrap()
            .with_agent_name("bob".to_string());

        // Try to run on a non-existent session.
        let fake_session_id = alms_core::SessionId::new();
        let result = runtime
            .run_on_session(&session_manager, fake_session_id, "dm:alice:bob", "hello")
            .await;

        assert!(result.is_err(), "Should fail if session does not exist");
    }

    /// Verify `dm_peer_name` extracts the correct peer from a DM context_id.
    #[test]
    fn test_dm_peer_name() {
        let config = crate::llm_types::LlmConfig {
            mock: true,
            ..crate::llm_types::LlmConfig::default()
        };
        let llm = LlmClient::new(config).unwrap();
        let agent_config = AgentConfig {
            sandbox_root: "".into(),
            ..AgentConfig::default()
        };

        // Agent named "bob" in "dm:alice:bob" → peer is "alice".
        let rt = AgentRuntime::new(AgentId::new(), agent_config.clone(), llm.clone())
            .unwrap()
            .with_agent_name("bob".to_string());
        assert_eq!(rt.dm_peer_name("dm:alice:bob"), Some("alice".to_string()));

        // Agent named "alice" in "dm:alice:bob" → peer is "bob".
        let rt2 = AgentRuntime::new(AgentId::new(), agent_config.clone(), llm.clone())
            .unwrap()
            .with_agent_name("alice".to_string());
        assert_eq!(rt2.dm_peer_name("dm:alice:bob"), Some("bob".to_string()));

        // Non-DM context_id → None (not a valid DM context).
        assert_eq!(rt.dm_peer_name("some-context"), None);

        // Malformed context_id → None.
        assert_eq!(rt.dm_peer_name("dm:only-one"), None);

        // Agent name not in context_id → None.
        let rt3 = AgentRuntime::new(AgentId::new(), agent_config.clone(), llm.clone())
            .unwrap()
            .with_agent_name("charlie".to_string());
        assert_eq!(rt3.dm_peer_name("dm:alice:bob"), None);

        // No agent_name set → None.
        let rt4 = AgentRuntime::new(AgentId::new(), agent_config, llm).unwrap();
        assert_eq!(rt4.dm_peer_name("dm:alice:bob"), None);
    }

    /// Verify that the DM system prompt addendum is injected into the context
    /// when running on a DM session, telling the agent to use `send_message`.
    #[tokio::test]
    async fn test_dm_system_prompt_injection() {
        let config = LlmConfig {
            mock: true,
            ..LlmConfig::default()
        };
        let session_config = SessionConfig::default();
        let session_manager = SessionManager::new(session_config);
        let llm = LlmClient::new(config).unwrap();
        let bob_id = AgentId::new();

        let agent_config = AgentConfig {
            sandbox_root: "".into(),
            ..AgentConfig::default()
        };
        let runtime = AgentRuntime::new(bob_id, agent_config, llm)
            .unwrap()
            .with_agent_name("bob".to_string());

        // Create a shared DM session with one message from alice.
        let dm_context = "dm:alice:bob";
        let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
        let _session = session_manager.get_or_create_shared(session_id, dm_context);

        let sender_msg = alms_session::Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: alms_session::Role::User,
            content: alms_session::Content::Text("Hello Bob!".to_string()),
            timestamp: alms_core::Timestamp::now(),
            metadata: Some(serde_json::json!({
                "from_agent": "alice",
                "from_agent_id": AgentId::new().0.to_string(),
                "message_type": "dm",
            })),
        };
        session_manager
            .append_message(session_id, sender_msg)
            .unwrap();

        // Build context for the DM session.
        let context = runtime
            .build_context(&session_manager, &session_id, dm_context, "")
            .await
            .unwrap();

        // The system message should contain DM-specific instructions.
        let system_msg = &context[0];
        assert_eq!(system_msg.role, "system");
        let system_text = system_msg.content.as_deref().unwrap_or("");
        assert!(
            system_text.contains("direct message from agent \"alice\""),
            "System prompt should mention the peer agent. Got: {}",
            &system_text[system_text.len().saturating_sub(300)..]
        );
        assert!(
            system_text.contains("send_message"),
            "System prompt should instruct to use send_message"
        );
        assert!(
            system_text.contains("ignore_message"),
            "System prompt should mention ignore_message"
        );
    }

    /// Verify that non-DM sessions do NOT get the DM system prompt addendum.
    #[tokio::test]
    async fn test_non_dm_no_system_prompt_injection() {
        let config = LlmConfig {
            mock: true,
            ..LlmConfig::default()
        };
        let session_config = SessionConfig::default();
        let session_manager = SessionManager::new(session_config);
        let llm = LlmClient::new(config).unwrap();
        let agent_id = AgentId::new();

        let agent_config = AgentConfig {
            sandbox_root: "".into(),
            ..AgentConfig::default()
        };
        let runtime = AgentRuntime::new(agent_id, agent_config, llm)
            .unwrap()
            .with_agent_name("bob".to_string());

        // Create a normal session.
        let session = session_manager.get_or_create(agent_id, "normal-context");

        // Build context for a non-DM session.
        let context = runtime
            .build_context(&session_manager, &session.id, "normal-context", "Hi")
            .await
            .unwrap();

        let system_msg = &context[0];
        let system_text = system_msg.content.as_deref().unwrap_or("");
        assert!(
            !system_text.contains("direct message from agent"),
            "Non-DM session should NOT have DM prompt addendum"
        );
    }

    #[test]
    fn test_posture_from_str() {
        assert_eq!("guarded".parse::<Posture>().unwrap(), Posture::Guarded);
        assert_eq!(
            "full_control".parse::<Posture>().unwrap(),
            Posture::FullControl
        );
        assert_eq!(
            "autonomous".parse::<Posture>().unwrap(),
            Posture::Autonomous
        );
        assert!("unknown".parse::<Posture>().is_err());
        assert!("".parse::<Posture>().is_err());
    }

    #[test]
    fn test_posture_display() {
        assert_eq!(Posture::Guarded.to_string(), "guarded");
        assert_eq!(Posture::FullControl.to_string(), "full_control");
        assert_eq!(Posture::Autonomous.to_string(), "autonomous");
    }

    #[test]
    fn test_posture_roundtrip() {
        for posture in [Posture::Guarded, Posture::FullControl, Posture::Autonomous] {
            let s = posture.to_string();
            let parsed: Posture = s.parse().unwrap();
            assert_eq!(parsed, posture);
        }
    }

    #[test]
    fn test_is_user_facing_context() {
        // User-facing: web UI, Telegram
        assert!(AgentRuntime::is_user_facing_context("web-chat-123"));
        assert!(AgentRuntime::is_user_facing_context("telegram_agent_456"));

        // Non-user-facing: DM, subagent, job
        assert!(!AgentRuntime::is_user_facing_context("dm:alice:bob"));
        assert!(!AgentRuntime::is_user_facing_context("subagent_task123"));
        assert!(!AgentRuntime::is_user_facing_context(
            "subagent_task123_reviewer"
        ));
        assert!(!AgentRuntime::is_user_facing_context("job_abc"));

        // Edge cases: empty string and unknown prefix default to user-facing
        assert!(AgentRuntime::is_user_facing_context(""));
        assert!(AgentRuntime::is_user_facing_context("unknown_prefix"));

        // Near-miss prefixes must NOT match (prefix must be exact)
        assert!(AgentRuntime::is_user_facing_context("dmx:something"));
        assert!(AgentRuntime::is_user_facing_context("subagentx_something"));
        assert!(AgentRuntime::is_user_facing_context("jobs_something"));
    }
}
