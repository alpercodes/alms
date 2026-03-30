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
    /// Whether both `send_message` and `ignore_message` appear in the batch.
    pub conflict: bool,
    /// Tool names that should be blocked (empty slice when no conflict).
    pub conflicting_tools: &'static [&'static str],
}

/// Inspect a tool-call batch for the `send_message` / `ignore_message`
/// mutual-exclusivity conflict.  When both tools appear in the same batch,
/// `conflict` is `true` and `conflicting_tools` lists the two names that
/// should receive error results instead of executing.
///
/// When there is no conflict, `conflicting_tools` is empty and all tools
/// can execute normally.
pub(crate) fn detect_dm_conflict(tool_calls: &[ToolCall]) -> DmConflictCheck {
    let has_send = tool_calls
        .iter()
        .any(|tc| tc.function.name == SEND_MESSAGE_TOOL);
    let has_ignore = tool_calls
        .iter()
        .any(|tc| tc.function.name == IGNORE_MESSAGE_TOOL);
    let conflict = has_send && has_ignore;
    DmConflictCheck {
        conflict,
        conflicting_tools: if conflict {
            &[SEND_MESSAGE_TOOL, IGNORE_MESSAGE_TOOL]
        } else {
            &[]
        },
    }
}

/// Returns `true` when `send_message` was called without a conflict in a
/// DM-triggered run, meaning the agent has delivered its reply and the loop
/// should terminate.  Without this check the loop re-enters, the LLM calls
/// `send_message` again, and the result is duplicate messages and a cascade
/// of `RunTrigger` events (#407 Bug 1).
pub(crate) fn should_terminate_after_dm_send(
    tool_calls: &[ToolCall],
    is_dm: bool,
    dm_conflict: bool,
) -> bool {
    is_dm
        && !dm_conflict
        && tool_calls
            .iter()
            .any(|tc| tc.function.name == SEND_MESSAGE_TOOL)
}

/// Maximum number of times the agent loop will retry when a DM-triggered
/// run ends with a text-only response (no `send_message` / `ignore_message`).
pub(crate) const DM_TEXT_ONLY_MAX_RETRIES: u32 = 1;

/// Error message injected into the conversation when the agent responds
/// with text only in a DM session instead of using `send_message` or
/// `ignore_message`.
pub(crate) const DM_TEXT_ONLY_RETRY_MSG: &str = "ERROR: Your text-only response was NOT delivered to the other agent. \
     In a direct message conversation you MUST use either the `send_message` tool \
     to reply or the `ignore_message` tool to explicitly decline. \
     Plain text responses are discarded. Please try again using the correct tool.";

/// Check whether `send_message` or `ignore_message` was **successfully**
/// called at any point during the run by inspecting the accumulated tool
/// call records.
///
/// A DM tool is only counted as "called" if:
/// 1. An `Assistant`-role record exists for `send_message` or `ignore_message`, AND
/// 2. A corresponding `Tool`-role result record (matched by `tool_id`) exists
///    whose result is NOT an error (does not start with `"Error:"`).
///
/// This prevents false positives when:
/// - Both tools appear in a conflict batch (PR #365) — both are blocked
///   and receive error results, so the text-only retry can trigger.
/// - `ignore_message` returns an error in a non-DM context (defense-in-depth).
pub(crate) fn dm_tool_was_called(records: &[alms_core::ToolCallRecord]) -> bool {
    records.iter().any(|r| {
        r.role == alms_core::ToolCallRole::Assistant
            && r.tool_name
                .as_deref()
                .is_some_and(|n| n == SEND_MESSAGE_TOOL || n == IGNORE_MESSAGE_TOOL)
            && r.tool_id.as_ref().is_some_and(|call_id| {
                // Find the matching Tool-role result record and verify it
                // was not an error.
                records.iter().any(|result| {
                    result.role == alms_core::ToolCallRole::Tool
                        && result.tool_id.as_deref() == Some(call_id.as_str())
                        && !result
                            .result
                            .as_deref()
                            .is_some_and(|res| res.starts_with("Error:"))
                })
            })
    })
}

// ---- impl AgentRuntime methods for DM handling ----

impl AgentRuntime {
    /// Extract the peer agent name from a DM context_id.
    ///
    /// The context_id has the form `"dm:{name1}:{name2}"` where names are
    /// sorted alphabetically.  The peer is whichever name is NOT this agent's
    /// own `agent_name`.  Returns `None` if the context_id is malformed or
    /// `agent_name` is not set.
    pub(crate) fn dm_peer_name(&self, context_id: &str) -> Option<String> {
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

    /// Build the DM recipient addendum for a given peer name.
    ///
    /// Returns the formatted template from `dm_recipient.md` with the peer
    /// name substituted.  This is appended to the system prompt so the agent
    /// knows it must use `send_message` to reply.
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

    /// Rebuild the system prompt for tool-loop continuation or DM retry.
    ///
    /// Combines the agent's initial prompt with the `tool_loop` continuation
    /// guidance, assembles workspace prefix, and (for DM sessions) appends the
    /// DM addendum so the agent remembers to use `send_message`.
    ///
    /// This is extracted as a helper to avoid three copies of the same pattern
    /// (initial tool-loop rebuild, DM text-only retry rebuild).
    pub(crate) fn rebuild_system_prompt_for_tool_loop(
        &self,
        messages: &mut [crate::llm_types::LlmMessage],
        include_user: bool,
        dm_peer: Option<&str>,
    ) {
        if !messages.is_empty() && messages[0].role == "system" {
            let combined = format!(
                "{}\n\n{}",
                self.config.system_prompt, self.config.prompts.tool_loop
            );
            let mut prompt = self.assemble_system_prompt(&combined, include_user);
            if let Some(peer) = dm_peer {
                prompt.push_str(&Self::dm_addendum(peer));
            }
            messages[0] = crate::llm_types::LlmMessage::system(prompt);
        }
    }
}
