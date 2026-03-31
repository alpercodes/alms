use crate::context::ContextBuilder;
use crate::events::PHASE_SUMMARIZING;
use crate::llm_types::*;
use alms_core::AlmsResult;
use alms_core::config::RunSummaryMode;
use alms_session::{ContextSummary, Role as SessionRole, SessionManager};
use tracing::{debug, error, info, warn};

use super::AgentRuntime;

impl AgentRuntime {
    /// Assemble the full system prompt for a given stage, prepending workspace
    /// files if attached.
    ///
    /// When `include_user` is false, `user.md` is omitted from the workspace
    /// prefix. This is used for non-user-facing sessions (DM, subagent, job).
    pub(crate) fn assemble_system_prompt(&self, base_prompt: &str, include_user: bool) -> String {
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
    /// system prompt.  Non-user-facing contexts (DM, subagent, job,
    /// notification) return false.
    ///
    /// NOTE: This function is **default-open** — unknown context_id prefixes
    /// are treated as user-facing.  When adding a new non-user-facing context
    /// type, add its prefix to the exclusion list below.
    pub(crate) fn is_user_facing_context(context_id: &str) -> bool {
        // These prefixes indicate non-user-facing sessions.
        !(context_id.starts_with("dm:")
            || context_id.starts_with("subagent_")
            || context_id.starts_with("job_")
            || context_id.starts_with("notifications:"))
    }

    /// Build context window for LLM using ContextBuilder.
    ///
    /// For the `sliding-summary` strategy this is async because it may call the
    /// LLM to compress old messages into a rolling summary.
    ///
    /// For DM sessions (context_id starts with `"dm:"`), perspective mapping is
    /// applied: messages from this agent become `Role::Assistant` so the LLM
    /// sees them as its own previous responses.
    pub(crate) async fn build_context(
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
            system_prompt.push_str(&Self::dm_addendum(&peer));
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
                self.emit_status(PHASE_SUMMARIZING, None);
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

        // Load episodic summaries from other sessions when enabled.
        // This gives the agent cross-session awareness — it can see what it was
        // doing in other conversations without re-reading full transcripts.
        let episodic_text: Option<String> =
            if self.config.context_config.run_summary_mode != RunSummaryMode::Off {
                self.load_episodic_summaries(session_manager, session_id)
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
            episodic_text.as_deref(),
        ))
    }

    /// Load episodic summaries from other sessions and format them for
    /// injection into the context window.
    ///
    /// Returns `None` when no summaries are available, the feature is off,
    /// or no SQLite store is configured.
    fn load_episodic_summaries(
        &self,
        session_manager: &SessionManager,
        current_session_id: &alms_core::SessionId,
    ) -> Option<String> {
        let budget = self.config.context_config.run_summary_budget;

        // S5: Derive the DB limit from the budget instead of a hardcoded 50.
        // A typical formatted entry is ~50-100 tokens.  We use a conservative
        // 30 tokens-per-entry estimate (plus some margin) so we fetch enough
        // rows but avoid pulling far more than the formatter can use.
        const MIN_TOKENS_PER_ENTRY: usize = 30;
        const MARGIN: usize = 5;
        let db_limit = (budget / MIN_TOKENS_PER_ENTRY) + MARGIN;

        let summaries = match session_manager.load_session_summaries(
            self.agent_id,
            db_limit,
            Some(current_session_id),
        ) {
            Ok(s) => s,
            Err(e) => {
                warn!("Failed to load episodic summaries: {e}");
                return None;
            }
        };

        if summaries.is_empty() {
            return None;
        }

        // S3: Subtract 4 tokens from the budget to account for the per-message
        // overhead that build_with_perspective adds when injecting the episodic
        // text as a system message (+4 for message framing).
        let effective_budget = budget.saturating_sub(4);

        debug!(
            summary_count = summaries.len(),
            budget_tokens = effective_budget,
            db_limit = db_limit,
            "Formatting episodic summaries for injection"
        );

        crate::episodic::format_episodic_for_injection(&summaries, effective_budget)
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
            include_str!("../../prompts/summarizer.md").trim(),
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

        // For DM sessions, use from_agent metadata to label messages with agent
        // names instead of raw roles (which are all Role::User in DM sessions).
        // When from_agent matches this agent's name, label as "You ({name})" so
        // the summarizer preserves self-attribution in the summary.
        let self_name = self.agent_name.as_deref();
        let mut has_agent_labels = false;

        let transcript: String = to_compress
            .iter()
            .map(|m| {
                let from = m
                    .metadata
                    .as_ref()
                    .and_then(|meta| meta.get("from_agent"))
                    .and_then(|v| v.as_str());

                let role_label: std::borrow::Cow<'_, str> = match from {
                    Some(sender) if self_name == Some(sender) => {
                        has_agent_labels = true;
                        format!("You ({})", sender).into()
                    }
                    Some(sender) => {
                        has_agent_labels = true;
                        sender.to_string().into()
                    }
                    None => match m.role {
                        SessionRole::User => "User".into(),
                        SessionRole::Assistant => "Assistant".into(),
                        SessionRole::System => "System".into(),
                        SessionRole::Tool => "Tool".into(),
                    },
                };
                format!("{}: {}", role_label, m.content.to_display_string())
            })
            .collect::<Vec<_>>()
            .join("\n");

        // When agent labels are present (DM session), prepend an instruction
        // to the transcript so the summarizer preserves attribution.
        if has_agent_labels {
            sum_messages.push(LlmMessage::user(format!(
                "This is a conversation between agents. \
                 Preserve who said what — use agent names \
                 (e.g., 'Alice requested X, Bob agreed to Y'). \
                 Lines starting with 'You (name):' are from the perspective agent.\n\n{}",
                transcript
            )));
        } else {
            sum_messages.push(LlmMessage::user(transcript));
        }

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
}
