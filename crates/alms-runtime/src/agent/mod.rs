mod context;
pub(crate) mod dm;
pub(crate) mod helpers;
mod loop_impl;
pub mod types;

#[cfg(test)]
mod tests;

// Re-export public types so external callers see the same API.
pub use types::{AgentConfig, Posture, RunOutput, SystemPrompts};

use crate::events::{PHASE_BUILDING_CONTEXT, RuntimeEventSender};
use crate::llm_client::LlmClient;
use crate::llm_types::*;
use crate::tools::ToolRegistry;
use crate::workspace::AgentWorkspace;
use crate::workspace_tool::WorkspaceWriteTool;
use alms_core::{AgentId, AlmsError, AlmsResult};
use alms_session::{
    Content as SessionContent, Message as SessionMessage, Role as SessionRole, SessionManager,
};
use tokio_util::sync::CancellationToken;
use tracing::{Span, info, instrument, warn};

use helpers::sanitize_error_for_session;

/// Agent runtime - executes agent loops
#[derive(Debug)]
pub struct AgentRuntime {
    pub(crate) agent_id: AgentId,
    pub(crate) config: AgentConfig,
    pub(crate) llm: LlmClient,
    pub(crate) tools: ToolRegistry,
    pub(crate) workspace: Option<AgentWorkspace>,
    /// Optional channel for emitting runtime events to the gateway layer.
    pub(crate) event_sender: Option<RuntimeEventSender>,
    /// Run ID for audit event correlation (set by gateway before execution).
    pub(crate) run_id: Option<alms_core::RunId>,
    /// Per-run cancellation token for cooperative cancellation.
    pub(crate) cancel_token: Option<CancellationToken>,
    /// Resolved sandbox root (canonicalized). Retained so `with_workspace()` can
    /// re-register the shell tool with the workspace dir as default cwd.
    pub(crate) resolved_sandbox_root: Option<std::path::PathBuf>,
    /// Whether shell commands bypass sandbox cwd restriction.
    pub(crate) shell_unrestricted: bool,
    /// Default env vars injected into shell processes (e.g. ALMS_DATA_DIR).
    /// Retained so `with_workspace()` can pass them to the re-registered shell tool.
    pub(crate) shell_default_env: std::collections::HashMap<String, String>,
    /// Permission-based allow/deny patterns for shell commands.
    /// Retained so re-registrations of the shell tool preserve the policy.
    pub(crate) shell_permissions: alms_core::config::ShellPermissions,
    /// Agent name for perspective mapping in DM sessions.
    /// When set and the context_id starts with "dm:", the context builder
    /// maps messages from this agent to `Role::Assistant` so the LLM sees
    /// them as its own previous responses.
    pub(crate) agent_name: Option<String>,
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

            // Platform-specific sandboxing information
            #[cfg(target_os = "linux")]
            if config.shell_policy == "sandboxed" {
                info!(
                    sandbox_root = %canonical.display(),
                    "Landlock filesystem sandbox will be applied to shell commands (Linux 5.13+)"
                );
            }
            #[cfg(not(target_os = "linux"))]
            if config.shell_policy == "sandboxed" {
                warn!(
                    sandbox_root = %canonical.display(),
                    "Shell sandbox restricts cwd only on this platform (non-Linux). \
                     Shell commands can access files outside the sandbox root at the OS level. \
                     For true filesystem isolation, deploy on Linux 5.13+ (Landlock) \
                     or run the daemon as a restricted OS user."
                );
            }

            Some(canonical)
        };
        let shell_unrestricted = config.shell_policy == "unrestricted";
        let tools = ToolRegistry::with_builtins_sandboxed(
            sandbox_root.clone(),
            shell_unrestricted,
            &config.enabled_tools,
        );

        let shell_permissions = config.shell_permissions.clone();

        let mut runtime = Self {
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
            shell_permissions: shell_permissions.clone(),
            agent_name: None,
        };

        // Apply shell permissions to the initially registered shell tool.
        // The shell tool is re-registered (with permissions) whenever
        // with_workspace() or with_shell_default_env() is called, but we
        // also need it on the initial tool so unnamed agents get the policy.
        if !shell_permissions.is_empty() {
            runtime.apply_shell_permissions();
        }

        Ok(runtime)
    }

    /// Attach an agent workspace for persistent identity files.
    ///
    /// Also registers the `workspace_write` tool so the agent can update
    /// its `goals.md` and `memories.md` during runs, and re-registers
    /// the shell tool with the workspace directory as default cwd.
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

        // Re-register fs_read/fs_write/fs_list/fs_edit sandboxed to the
        // workspace directory so file operations default to the agent's
        // workspace instead of the project root.
        // fs_read/fs_write/fs_edit get the file state cache for read-before-write guard.
        let cache = self.tools.file_state_cache().clone();
        if tool_enabled("fs_read") {
            self.tools.register(std::sync::Arc::new(
                alms_sandbox::FsReadTool::sandboxed(ws_root.clone()).with_cache(cache.clone()),
            ));
        }
        if tool_enabled("fs_write") {
            self.tools.register(std::sync::Arc::new(
                alms_sandbox::FsWriteTool::sandboxed(ws_root.clone()).with_cache(cache.clone()),
            ));
        }
        if tool_enabled("fs_list") {
            self.tools
                .register(std::sync::Arc::new(alms_sandbox::FsListTool::sandboxed(
                    ws_root.clone(),
                )));
        }
        if tool_enabled("fs_edit") {
            self.tools.register(std::sync::Arc::new(
                alms_sandbox::FsEditTool::sandboxed(ws_root.clone()).with_cache(cache.clone()),
            ));
        }
        if tool_enabled("fs_grep") {
            self.tools
                .register(std::sync::Arc::new(alms_sandbox::FsGrepTool::sandboxed(
                    ws_root.clone(),
                )));
        }
        if tool_enabled("fs_glob") {
            self.tools
                .register(std::sync::Arc::new(alms_sandbox::FsGlobTool::sandboxed(
                    ws_root.clone(),
                )));
        }

        // Re-register shell tool with workspace dir as default cwd and
        // gateway-provided default env vars (ALMS_DATA_DIR, etc.).
        // The tool is registered under both "shell" (primary) and "shell_exec" (alias).
        let shell_enabled = tool_enabled("shell") || tool_enabled("shell_exec");
        if shell_enabled {
            let mut shell_tool = alms_sandbox::ShellTool::with_policy(
                self.resolved_sandbox_root.clone(),
                self.shell_unrestricted,
            )
            .with_default_cwd(ws_root)
            .with_permissions(&self.shell_permissions);
            if !self.shell_default_env.is_empty() {
                shell_tool = shell_tool.with_default_env(self.shell_default_env.clone());
            }
            let tool_arc: std::sync::Arc<dyn alms_sandbox::Tool> = std::sync::Arc::new(shell_tool);
            self.tools.register(std::sync::Arc::clone(&tool_arc));
            // Also register under legacy alias for backward compatibility
            self.tools
                .register_arc_as(alms_sandbox::shell::SHELL_TOOL_ALIAS, tool_arc);
        }

        self.workspace = Some(workspace);
        self
    }

    /// Attach a runtime event sender so the gateway can observe tool events.
    pub fn with_event_sender(mut self, sender: RuntimeEventSender) -> Self {
        self.event_sender = Some(sender);
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

    /// Set default environment variables for shell processes.
    ///
    /// These are injected into every process spawned by the shell tool after
    /// `env_clear()`, so the spawned CLI commands can discover the gateway's
    /// data directory (`ALMS_DATA_DIR`) and workspace directory
    /// (`ALMS_WORKSPACE_DIR`) even when cwd is sandboxed elsewhere.
    ///
    /// Re-registers the shell tool immediately so that unnamed agents
    /// (which skip `with_workspace()`) still receive the environment variables.
    pub fn with_shell_default_env(
        mut self,
        env: std::collections::HashMap<String, String>,
    ) -> Self {
        self.shell_default_env = env;

        // Re-register shell tool with the new default env so unnamed agents
        // (which never call with_workspace()) still get ALMS_DATA_DIR injected.
        let enabled = &self.config.enabled_tools;
        let shell_enabled =
            enabled.is_empty() || enabled.iter().any(|t| t == "shell" || t == "shell_exec");
        if shell_enabled && (self.tools.contains("shell") || self.tools.contains("shell_exec")) {
            let shell_tool = alms_sandbox::ShellTool::with_policy(
                self.resolved_sandbox_root.clone(),
                self.shell_unrestricted,
            )
            .with_permissions(&self.shell_permissions)
            .with_default_env(self.shell_default_env.clone());
            let tool_arc: std::sync::Arc<dyn alms_sandbox::Tool> = std::sync::Arc::new(shell_tool);
            self.tools.register(std::sync::Arc::clone(&tool_arc));
            self.tools
                .register_arc_as(alms_sandbox::shell::SHELL_TOOL_ALIAS, tool_arc);
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

    /// Re-register the shell tool with the current `shell_permissions`.
    ///
    /// Used at construction time to apply permissions to the initial shell
    /// tool registration. Later registrations (via `with_workspace()`,
    /// `with_shell_default_env()`) also apply permissions explicitly.
    fn apply_shell_permissions(&mut self) {
        let enabled = &self.config.enabled_tools;
        let shell_enabled =
            enabled.is_empty() || enabled.iter().any(|t| t == "shell" || t == "shell_exec");
        if shell_enabled && (self.tools.contains("shell") || self.tools.contains("shell_exec")) {
            let shell_tool = alms_sandbox::ShellTool::with_policy(
                self.resolved_sandbox_root.clone(),
                self.shell_unrestricted,
            )
            .with_permissions(&self.shell_permissions);
            let tool_arc: std::sync::Arc<dyn alms_sandbox::Tool> = std::sync::Arc::new(shell_tool);
            self.tools.register(std::sync::Arc::clone(&tool_arc));
            self.tools
                .register_arc_as(alms_sandbox::shell::SHELL_TOOL_ALIAS, tool_arc);
        }
    }

    /// Emit a status event to the gateway layer (best-effort, never fails).
    pub(crate) fn emit_status(&self, phase: &str, detail: Option<&str>) {
        if let Some(ref tx) = self.event_sender {
            let _ = tx.send(crate::events::RuntimeEvent::Status {
                phase: phase.to_string(),
                detail: detail.map(|d| d.to_string()),
            });
        }
    }

    /// Emit a `ContextDebug` event containing the full assembled context
    /// window that is about to be sent to the LLM.
    ///
    /// Only called when `debug_mode` is enabled. Best-effort: failures are
    /// silently dropped since debug events are informational only.
    fn emit_context_debug(&self, messages: &[LlmMessage]) {
        use crate::context::{estimate_llm_message_tokens, estimate_tokens};

        let Some(ref tx) = self.event_sender else {
            return;
        };

        // Compute token estimates per message for the debug payload.
        let total_tokens: usize = messages
            .iter()
            .map(|m| estimate_llm_message_tokens(m) + 4)
            .sum();

        // System prompt tokens (first message is always the system prompt).
        let system_tokens = messages
            .first()
            .filter(|m| m.role == "system")
            .map(|m| estimate_tokens(m.content_str()))
            .unwrap_or(0);

        // History message count: everything except the first system message
        // and the last user message (which is the current input).
        let history_message_count = messages
            .len()
            .saturating_sub(1) // system prompt
            .saturating_sub(
                messages
                    .last()
                    .filter(|m| m.role == "user")
                    .map(|_| 1)
                    .unwrap_or(0),
            );

        let tool_names: Vec<String> = self
            .tools
            .to_definitions()
            .into_iter()
            .map(|td| td.function.name)
            .collect();

        // Serialize messages for the debug payload. Use serde_json::to_value
        // which handles the full LlmMessage structure including tool_calls.
        let messages_json = serde_json::to_value(messages).unwrap_or_default();

        let _ = tx.send(crate::events::RuntimeEvent::ContextDebug {
            messages: messages_json,
            tool_names,
            total_tokens,
            system_tokens,
            history_message_count,
        });
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
        self.emit_status(PHASE_BUILDING_CONTEXT, None);
        let history = self
            .build_context(session_manager, &session.id, context_id, &input)
            .await;

        let user_msg = SessionMessage {
            id: uuid::Uuid::new_v4().to_string(),
            role: SessionRole::User,
            content: SessionContent::Text(input.clone()),
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
        self.emit_status(PHASE_BUILDING_CONTEXT, None);
        let history = self
            .build_context(session_manager, &session_id, context_id, "")
            .await;

        // Do NOT persist the input message -- it is already in the session.

        self.finish_run(session_manager, session_id, context_id, history)
            .await
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
        let dm_peer = if is_dm {
            self.dm_peer_name(context_id)
        } else {
            None
        };

        let (tool_calls, result) = match history {
            Ok(h) => {
                // Emit context debug snapshot when debug_mode is enabled.
                // This happens after build_context() and before agent_loop()
                // so the UI sees exactly what the LLM will receive on the
                // first iteration.
                if self.config.debug_mode {
                    self.emit_context_debug(&h);
                }

                self.agent_loop(
                    session_manager,
                    session_id,
                    h,
                    is_dm,
                    include_user,
                    dm_peer.as_deref(),
                )
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
                // Also skip persisting the max-iterations sentinel — it is
                // surfaced as a `run_warning` SSE event by the gateway and
                // should not appear as a normal assistant bubble on reload.
                //
                // For DM sessions: persist the final text response as
                // reasoning (Role::User with message_type="reasoning") so
                // the UI can display it in a collapsible reasoning block
                // after page reload.
                //
                // However, when the agent loop executed tool calls, the
                // thinking text was already persisted by
                // `persist_assistant_tool_calls` as part of the tool call
                // batch.  Persisting it again here would produce duplicate
                // reasoning text entries that `groupDmReasoningBlocks()`
                // concatenates, resulting in doubled text on page reload.
                // Skip persistence when tool calls were present.  (Fixes #687)
                let dm_text_already_persisted = is_dm && !tool_calls.is_empty();
                if !response.is_empty()
                    && response != alms_core::MAX_ITERATIONS_SENTINEL
                    && !dm_text_already_persisted
                {
                    let (role, metadata) = if is_dm {
                        if let Some(meta) = self.dm_reasoning_metadata(is_dm) {
                            (SessionRole::User, Some(meta))
                        } else {
                            (SessionRole::Assistant, None)
                        }
                    } else {
                        (SessionRole::Assistant, None)
                    };
                    let reply_msg = SessionMessage {
                        id: uuid::Uuid::new_v4().to_string(),
                        role,
                        content: SessionContent::Text(response.clone()),
                        timestamp: alms_core::Timestamp::now(),
                        metadata,
                    };
                    session_manager.append_message(session_id, reply_msg)?;
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
                    content: SessionContent::Text("[Run cancelled by user]".to_string()),
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
                    content: SessionContent::Text(format!("[Run failed: {}]", safe_reason)),
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

    /// Get tool registry reference
    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }
}
