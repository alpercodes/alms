// SPDX-License-Identifier: Apache-2.0

use crate::llm_types::ToolCall;

use super::AgentRuntime;

// ---- DM conflict detection (send_message / ignore_message mutual exclusivity) ----

/// Tool name constants for the mutually exclusive DM tools.
const SEND_MESSAGE_TOOL: &str = "send_message";
const IGNORE_MESSAGE_TOOL: &str = "ignore_message";

/// Re-export the shared constant for in-crate use.
pub(crate) const DM_CONFLICT_MSG: &str = alms_core::DM_CONFLICT_MSG;

/// Result of checking a tool-call batch for DM tool conflicts.
#[derive(Debug)]
pub(crate) struct DmConflictCheck {
    /// Whether both a peer-directed `send_message` and an `ignore_message`
    /// appear in the batch.
    pub conflict: bool,
    /// Batch **indices** (into the `tool_calls` slice passed to
    /// [`detect_dm_conflict`]) of the *specific* tool calls that should be
    /// blocked — the peer-directed `send_message` call(s) plus the
    /// `ignore_message` call(s). Empty when there is no conflict.
    ///
    /// Keyed by **index** rather than by tool *name* (#1160, Codex P2): a
    /// batch can hold two `send_message` calls — one folded reply to the
    /// current peer and one legitimate hand-off to a *third* agent — and
    /// they share the name `"send_message"`. Blocking by name would also
    /// drop the third-agent hand-off, defeating the recipient-aware S3 rule
    /// that explicitly allows `send_message(to: charlie)` + `ignore_message`.
    /// Index identity is the right granularity: every site that consumes
    /// this set already iterates the batch positionally (the `invocation_ids`
    /// zip, the parallel arm's `exec_indices`/slot mapping), and indices are
    /// collision-proof within a batch where `ToolCall::id` is not guaranteed
    /// unique (or even non-empty) across providers / the streaming
    /// accumulator.
    pub conflicting_indices: Vec<usize>,
}

/// Inspect a tool-call batch for the `send_message` / `ignore_message`
/// mutual-exclusivity conflict (#364), narrowed to be **recipient-aware**
/// under implicit replies (#1154 / S3).
///
/// Originally this flagged *any* batch containing both `send_message` and
/// `ignore_message`. Under implicit replies that is too broad: replying to
/// the current peer no longer uses `send_message` at all (the final
/// assistant text IS the reply, and a `send_message` aimed at the current
/// peer is *folded*, never delivered — see `SendMessageTool::with_dm_peer`).
/// So `send_message` is now only legitimately used to address a *different*
/// agent, and "loop in Charlie, then end the conversation with the current
/// peer" — i.e. `send_message(to: charlie)` + `ignore_message` in one batch —
/// is a perfectly valid combination that the old check wrongly rejected.
///
/// The genuine contradiction that remains is `send_message` aimed at the
/// **current peer** together with `ignore_message`: that simultaneously
/// folds a reply to the peer and ends the conversation with them. Only that
/// case sets `conflict = true`.
///
/// `dm_peer` is the current DM peer's name (`None` outside a DM run, where
/// no `send_message` can target "the peer" — so no conflict is possible).
/// Recipient matching is case-insensitive, mirroring the peer-fold check in
/// [`alms_tools::SendMessageTool`].
pub(crate) fn detect_dm_conflict(
    tool_calls: &[ToolCall],
    dm_peer: Option<&str>,
) -> DmConflictCheck {
    // Single pass: collect the batch indices of the `ignore_message` call(s)
    // and the *peer-directed* `send_message` call(s) separately. A
    // `send_message` aimed at a *different* agent (the third-agent hand-off)
    // is deliberately not collected — it is never part of the conflict.
    let mut ignore_indices: Vec<usize> = Vec::new();
    let mut peer_send_indices: Vec<usize> = Vec::new();
    for (i, tc) in tool_calls.iter().enumerate() {
        if tc.function.name == IGNORE_MESSAGE_TOOL {
            ignore_indices.push(i);
        } else if tc.function.name == SEND_MESSAGE_TOOL
            && let Some(peer) = dm_peer
            && send_targets_peer(tc, peer)
        {
            // A `send_message` conflicts only when it targets the current
            // peer. Outside a DM run (`dm_peer == None`) no send can target
            // "the peer", so none are collected here.
            peer_send_indices.push(i);
        }
    }

    // The genuine contradiction is a peer-directed `send_message` *and* an
    // `ignore_message` in the same batch. Only then are the specific calls
    // blocked; the (possible) third-agent `send_message` is left to execute.
    let conflict = !peer_send_indices.is_empty() && !ignore_indices.is_empty();
    let conflicting_indices = if conflict {
        // Merge the two index lists. Both are already ascending and
        // disjoint (a call is either `ignore_message` or `send_message`,
        // never both), so concatenate and sort for a stable, deduplicated
        // ascending set.
        let mut merged = peer_send_indices;
        merged.extend(ignore_indices);
        merged.sort_unstable();
        merged
    } else {
        Vec::new()
    };

    DmConflictCheck {
        conflict,
        conflicting_indices,
    }
}

/// Return `true` when a `send_message` tool call's `to` argument names the
/// current DM peer (case-insensitive). Malformed / missing arguments parse
/// to `false` — an unparseable recipient cannot be proven to target the
/// peer, so it is not treated as a conflict (the send tool will surface its
/// own error when it runs).
///
/// Scope note (S3, PR #1160): this mirrors only the *raw `to`* peer-fold in
/// [`alms_tools::SendMessageTool`] (the pre-resolution check), not its
/// second, post-resolution fold that re-checks the registry-resolved
/// canonical name. So a `send_message` addressing the peer by a non-`to`
/// spelling that only resolves to the peer after a registry lookup — e.g. an
/// agent **ID** or an alias — slips past this conflict gate while still being
/// folded at execution time. That asymmetry is deliberate and harmless:
/// `detect_dm_conflict` is a pure, pre-execution check with no agent-store
/// handle (resolving an ID to a name here would couple it to SQLite), and the
/// end state is identical either way — the paired `ignore_message` ends the
/// conversation and the folded `send_message` reply is discarded, so the only
/// thing the missed conflict would have changed is whether the batch was
/// rejected up front versus folded during execution.
fn send_targets_peer(tc: &ToolCall, peer: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
        .ok()
        .as_ref()
        .and_then(|v| v.get("to"))
        .and_then(|to| to.as_str())
        .is_some_and(|to| to.eq_ignore_ascii_case(peer))
}

/// Maximum number of times the agent loop will retry when a peer-triggered
/// DM run ends with no deliverable reply text (empty/whitespace-only final
/// content, or a `[Thinking]`-only promotion — see
/// [`alms_core::deliverable_dm_reply`]) and no successful `ignore_message`.
///
/// One bounded nudge per #1154 design default #3: after the retry the run
/// completes with an empty response and the gateway's DM completion gate
/// ends the conversation with an `Errored` reason so the peer is notified
/// instead of stranded.
pub(crate) const DM_EMPTY_REPLY_MAX_RETRIES: u32 = 1;

/// Nudge injected into the conversation when a peer-triggered DM run is
/// about to end with no deliverable reply text. Reminds the agent that its
/// final message text IS the reply (implicit delivery, #1154) and that
/// `ignore_message` is the explicit way to end the conversation.
///
/// Loaded at compile time from `crates/alms-runtime/prompts/dm_empty_reply_retry.md`.
pub(crate) const DM_EMPTY_REPLY_RETRY_MSG: &str =
    include_str!("../../prompts/dm_empty_reply_retry.md").trim_ascii();

// ---- impl AgentRuntime methods for DM handling ----

impl AgentRuntime {
    /// Extract the peer agent name from a DM context_id.
    ///
    /// Delegates to [`alms_core::dm_peer`] for parsing.  Returns `None` if the
    /// context_id is malformed or `agent_name` is not set.
    pub(crate) fn dm_peer_name(&self, context_id: &str) -> Option<String> {
        let name = self.agent_name.as_deref()?;
        alms_core::dm_peer(context_id, name).map(|s| s.to_string())
    }

    /// Build the DM recipient addendum for a given peer name.
    ///
    /// Returns the formatted template from `dm_recipient.md` with the peer
    /// name substituted.  This is appended to the system prompt so the agent
    /// knows its final message text is delivered to the peer automatically
    /// (implicit reply, #1154) and that `ignore_message` ends the
    /// conversation.
    pub(crate) fn dm_addendum(peer: &str) -> String {
        let dm_template = include_str!("../../prompts/dm_recipient.md");
        format!("\n\n{}", dm_template.trim().replace("{peer}", peer))
    }

    /// Build `from_agent` metadata for DM sessions so that `read_messages`
    /// can attribute system markers (errors, cancellations) to the correct
    /// agent.  Returns `None` for non-DM sessions or when `agent_name` is
    /// not set.
    pub(crate) fn dm_marker_metadata(&self, is_dm: bool) -> Option<serde_json::Value> {
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

    /// Build reasoning metadata for DM sessions.
    ///
    /// Returns a JSON object with `message_type: "reasoning"` plus agent
    /// identity and run ID.  Used when persisting assistant text, tool calls,
    /// and tool results to a DM session so the UI can reconstruct collapsible
    /// reasoning blocks after a page reload.
    ///
    /// Returns `None` for non-DM sessions or when `agent_name` is not set.
    pub(crate) fn dm_reasoning_metadata(&self, is_dm: bool) -> Option<serde_json::Value> {
        if !is_dm {
            return None;
        }
        // Always return reasoning metadata for DM sessions, even when
        // agent_name is None.  DM sessions require Role::User for all
        // persisted messages (the DM invariant), so falling back to
        // Role::Assistant when agent_name is missing would be wrong.
        // Use "unknown" as the from_agent fallback to preserve the
        // message_type marker that dm_filter relies on.
        let name = self.agent_name.as_deref().unwrap_or("unknown");
        let run_id_str = self
            .run_id
            .as_ref()
            .map(|r| r.0.to_string())
            .unwrap_or_default();
        Some(serde_json::json!({
            "message_type": "reasoning",
            "from_agent": name,
            "from_agent_id": self.agent_id.0.to_string(),
            "run_id": run_id_str,
        }))
    }

    /// Merge reasoning metadata into an existing metadata object.
    ///
    /// Used when tool call/result entries already carry their own metadata
    /// fields (e.g. `tool_call_id`, `tool_invocation_id`) and the reasoning
    /// fields need to be added alongside them.
    pub(crate) fn merge_reasoning_metadata(
        &self,
        base: serde_json::Value,
        is_dm: bool,
    ) -> serde_json::Value {
        if let Some(reasoning) = self.dm_reasoning_metadata(is_dm) {
            if let (serde_json::Value::Object(mut base_map), serde_json::Value::Object(r_map)) =
                (base, reasoning)
            {
                for (k, v) in r_map {
                    base_map.insert(k, v);
                }
                serde_json::Value::Object(base_map)
            } else {
                serde_json::Value::Null
            }
        } else {
            base
        }
    }

    /// Rebuild the system prompt for tool-loop continuation or DM retry.
    ///
    /// Assembles the base prompt with the workspace prefix, then appends the
    /// `tool_loop` continuation guidance, and (for DM sessions) appends the
    /// DM addendum so the agent remembers the implicit-reply contract.
    ///
    /// Layer order is `base -> workspace -> tool_loop -> dm_addendum`, matching
    /// the assembly order documented in `docs/system-prompts.md`.
    ///
    /// This is extracted as a helper to avoid three copies of the same pattern
    /// (initial tool-loop rebuild, DM empty-reply retry rebuild).
    pub(crate) fn rebuild_system_prompt_for_tool_loop(
        &self,
        messages: &mut [crate::llm_types::LlmMessage],
        include_user: bool,
        dm_peer: Option<&str>,
    ) {
        if !messages.is_empty() && messages[0].role == "system" {
            let mut prompt = self.assemble_system_prompt(&self.config.system_prompt, include_user);
            prompt.push_str("\n\n");
            prompt.push_str(&self.config.prompts.tool_loop);
            if let Some(peer) = dm_peer {
                prompt.push_str(&Self::dm_addendum(peer));
            }
            messages[0] = crate::llm_types::LlmMessage::system(prompt);
        }
    }
}
