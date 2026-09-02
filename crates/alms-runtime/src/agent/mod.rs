// SPDX-License-Identifier: Apache-2.0

mod context;
pub(crate) mod dm;
mod environment;
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
use alms_core::{AgentId, AlmsError, AlmsResult, sanitize_error_for_session};
use alms_session::{
    Content as SessionContent, Message as SessionMessage, Role as SessionRole, SessionManager,
};
use tokio_util::sync::CancellationToken;
use tracing::{Span, info, instrument, warn};

/// Agent runtime - executes agent loops
#[derive(Debug)]
pub struct AgentRuntime {
    pub(crate) agent_id: AgentId,
    pub(crate) config: AgentConfig,
    pub(crate) llm: LlmClient,
    /// Optional dedicated LLM client for in-loop compact-strategy summary
    /// generation (#866; strategy renamed from `sliding-summary` in #869).
    /// When `Some`, `maybe_summarize` uses this client instead of `llm` so
    /// the summary task can target a different provider than the agent
    /// (e.g. agent on Anthropic, summary on OpenRouter). When `None`,
    /// summaries inherit the agent's `llm` (pre-#866 behaviour).
    pub(crate) summary_llm: Option<LlmClient>,
    pub(crate) tools: ToolRegistry,
    pub(crate) workspace: Option<AgentWorkspace>,
    /// Optional channel for emitting runtime events to the gateway layer.
    pub(crate) event_sender: Option<RuntimeEventSender>,
    /// Run ID for audit event correlation (set by gateway before execution).
    pub(crate) run_id: Option<alms_core::RunId>,
    /// Per-run cancellation token for cooperative cancellation.
    pub(crate) cancel_token: Option<CancellationToken>,
    /// Resolved sandbox root (canonicalized). Retained because the
    /// *sandboxed* shell and fs_* re-registrations read it — the shell
    /// builders, and `refresh_fs_tools_for_extras`, which is a no-op
    /// while this is `None`. `with_unrestricted_filesystem` is the
    /// exception on both counts: it sets this to `None` and then
    /// registers unsandboxed tools that never consult it.
    pub(crate) resolved_sandbox_root: Option<std::path::PathBuf>,
    /// Whether shell commands bypass sandbox cwd restriction.
    pub(crate) shell_unrestricted: bool,
    /// Default env vars injected into shell processes (e.g. ALMS_DATA_DIR).
    /// Retained so the shell re-registrations that follow
    /// `with_shell_default_env` — `with_shell_spill`, `with_project_root`,
    /// `with_unrestricted_filesystem` — carry them onto the replacement
    /// tool instead of dropping them.
    pub(crate) shell_default_env: std::collections::HashMap<String, String>,
    /// Permission-based allow/deny patterns for shell commands.
    /// Retained so re-registrations of the shell tool preserve the policy.
    pub(crate) shell_permissions: alms_core::config::ShellPermissions,
    /// Built-in risk classification mode for shell commands.
    /// Retained so re-registrations of the shell tool preserve the mode.
    pub(crate) shell_classification_mode: alms_core::config::ShellClassificationMode,
    /// Large-output spill-to-disk policy for the shell tool (issue #756).
    /// Defaults to disabled; the gateway wires in an active policy once the
    /// per-run spill directory (`{data_dir}/shell_output/{run_id}/`) is
    /// known, via [`Self::with_shell_spill`]. Retained so the shell
    /// re-registrations that follow (`with_project_root`,
    /// `with_unrestricted_filesystem`) preserve the policy.
    pub(crate) shell_spill_policy: alms_sandbox::shell::spill::ShellSpillPolicy,
    /// Shared in-loop tool-output truncation policy (issue #851).
    ///
    /// Mirrors [`shell_spill_policy`][Self::shell_spill_policy] but applies
    /// to *every* tool's result, not just shell. Defaults to disabled; the
    /// gateway wires in an active policy via [`Self::with_tool_output_truncate`]
    /// once the per-run spill directory
    /// (`{data_dir}/tool-output/{run_id}/`) is known. The agent loop runs
    /// every tool's JSON result through
    /// [`tool_output_truncate::truncate`][crate::tool_output_truncate::truncate]
    /// before pushing it into the live `messages` vec or persisting to the
    /// session DB so a single oversized tool output cannot blow the LLM's
    /// context window.
    pub(crate) tool_output_truncate_policy: crate::tool_output_truncate::ToolOutputTruncatePolicy,
    /// Accumulator of additional read-only roots that the read-family `fs_*`
    /// tools (`fs_read`, `fs_list`, `fs_grep`, `fs_glob`) should be allowed
    /// to read from beyond `resolved_sandbox_root`.
    ///
    /// Populated by builder methods that introduce per-run spill directories
    /// outside the agent's primary sandbox root:
    /// - [`Self::with_shell_spill`] appends `{data_dir}/shell_output/{run_id}/`.
    /// - [`Self::with_tool_output_truncate`] appends `{data_dir}/tool-output/{run_id}/`.
    ///
    /// Every *sandboxed* read-family fs_* registration reads the full
    /// accumulated list via [`Self::compose_fs_extra_read_roots`], so the
    /// one that runs last in the gateway's builder order —
    /// [`Self::with_project_root`] — preserves every per-run spill dir as
    /// a read root.
    ///
    /// [`Self::with_unrestricted_filesystem`] is the other tail of that
    /// branch and works the opposite way: it **clears** this accumulator
    /// and registers `FsReadTool::new()` and friends directly, never
    /// composing extras at all. Same outcome — with no primary root there
    /// is nothing to widen — by the inverse mechanism. (An earlier
    /// version of this doc listed it alongside `with_project_root` as a
    /// preserver, which is wrong twice over; the inline comment in that
    /// builder says so correctly.) That is what makes the accumulator a
    /// single source of truth rather than a per-builder detail: without it
    /// each spill builder would re-register fs_* tools from its own local
    /// view and the last writer would silently drop the others' roots —
    /// the footgun flagged on #921 review.
    ///
    /// This used to name [`Self::with_workspace`] as the last fs_*
    /// registration. It has performed none since #945, which is why the
    /// correction matters here specifically: the claim is the stated
    /// reason this field exists, so a reader checking it against
    /// `with_workspace` would find nothing and conclude the field was
    /// vestigial. It is not — the builders above are the consumers.
    pub(crate) extra_fs_read_roots: Vec<std::path::PathBuf>,
    /// Agent name for perspective mapping in DM sessions.
    /// When set and the context_id starts with "dm:", the context builder
    /// maps messages from this agent to `Role::Assistant` so the LLM sees
    /// them as its own previous responses.
    pub(crate) agent_name: Option<String>,
    /// Implicit DM reply mode (#1154): set by the gateway for
    /// peer-triggered DM runs. When `true`, the run's final assistant text
    /// IS the message delivered to the DM peer — the gateway's DM
    /// completion gate performs the delivery (and the associated session
    /// persistence) after the run completes. The runtime reacts in two
    /// places:
    ///
    /// 1. `agent_loop` arms the bounded empty-reply nudge when the run is
    ///    about to end with no deliverable reply text.
    /// 2. `finish_run` skips persisting the final DM text as a
    ///    reasoning-type message — `MessageBus::send` persists it with
    ///    `message_type: "dm"` on delivery, and persisting it here too
    ///    would double-render the same text in the DM session.
    pub(crate) dm_implicit_reply: bool,
}

impl AgentRuntime {
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
            // #1003: attribute the snapshot to the agent whose
            // perspective produced it. For DM sessions this is the
            // agent whose turn is about to fire — the UI uses the
            // pair to label the panel and group concurrent DM
            // perspectives. For webchat sessions this is just the
            // active agent.
            agent_id: self.agent_id.0.to_string(),
            agent_name: self.agent_name.clone(),
        });
    }

    /// Run the agent on a single input
    #[instrument(
        level = "info",
        skip(self, session_manager, context_id, input),
        fields(
            agent_id = %self.agent_id.0,
            context_id = %context_id.as_ref(),
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
        // `dm_peer` drives the in-loop re-injection of the implicit-reply
        // addendum (`rebuild_system_prompt_for_tool_loop`). Gated on
        // `dm_implicit_reply` (#1156 defense-in-depth) so the loop-rebuild
        // path follows the same invariant as `build_context`: the addendum
        // is only injected for peer-triggered DM runs, where the gateway's
        // completion gate actually delivers the final text.
        let dm_peer = if is_dm && self.dm_implicit_reply {
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
            Ok(loop_output) => {
                let crate::agent::loop_impl::AgentLoopOutput {
                    response,
                    usage,
                    reasoning,
                } = loop_output;

                // Skip persisting an assistant message when the response is
                // empty. This happens when the agent used `ignore_message` to
                // decline responding — there is nothing to record.
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
                //
                // Implicit DM reply (#1154): for peer-triggered DM runs the
                // final text IS the reply, and the gateway's completion gate
                // persists it via `MessageBus::send` as a `message_type:
                // "dm"` message when it delivers. Persisting it here as a
                // reasoning-type message too would double-render the same
                // text in the DM session, so skip — only the final turn's
                // extended-thinking trace (when distinct from the response)
                // is persisted below so the UI keeps the collapsible
                // reasoning panel.
                let dm_text_already_persisted = is_dm && !tool_calls.is_empty();
                let implicit_dm_reply = is_dm && self.dm_implicit_reply;
                if implicit_dm_reply {
                    if let Some(trace) = reasoning
                        .as_deref()
                        .filter(|t| !t.is_empty() && *t != response)
                    {
                        let metadata = crate::agent::loop_impl::merge_reasoning_blocks(
                            self.dm_reasoning_metadata(is_dm),
                            Some(trace),
                        );
                        let reasoning_msg = SessionMessage {
                            id: uuid::Uuid::new_v4().to_string(),
                            role: SessionRole::User,
                            content: SessionContent::Text(String::new()),
                            timestamp: alms_core::Timestamp::now(),
                            metadata,
                        };
                        if let Err(e) = session_manager.append_message(session_id, reasoning_msg) {
                            warn!("Failed to persist final-turn reasoning trace: {}", e);
                        }
                    }
                } else if !response.is_empty() && !dm_text_already_persisted {
                    let base_meta = if is_dm {
                        self.dm_reasoning_metadata(is_dm)
                    } else {
                        None
                    };
                    let role = if is_dm && base_meta.is_some() {
                        SessionRole::User
                    } else {
                        SessionRole::Assistant
                    };
                    // Attach the extended-thinking trace (if any) as
                    // `reasoning_blocks` metadata on the final assistant
                    // message, merged with any existing DM metadata.
                    let metadata = crate::agent::loop_impl::merge_reasoning_blocks(
                        base_meta,
                        reasoning.as_deref(),
                    );
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
                    reasoning,
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

    /// Register an additional tool for this runtime.
    pub fn register_tool(&self, tool: std::sync::Arc<dyn alms_sandbox::Tool>) {
        self.tools.register(tool);
    }

    /// Get tool registry reference for runtime-internal tests.
    #[cfg(test)]
    pub(crate) fn tools(&self) -> &ToolRegistry {
        &self.tools
    }
}
