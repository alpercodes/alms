//! ignore_message tool -- lets an agent decline to respond.
//!
//! When called, this tool signals that the current run should end early
//! without producing a response. Only valid in DM sessions -- calling it
//! from a non-DM context returns an error so the agent knows it cannot
//! use this tool there.

use alms_sandbox::{SandboxError, Tool, error::SandboxResult};
use serde_json::Value;
use tracing::{info, warn};

/// Built-in tool that lets an agent decline to respond to a message.
///
/// The tool returns a JSON marker `{"ignored": true, "reason": "..."}`.
/// The agent loop in `agent.rs` checks for this marker after processing
/// tool results and, when detected, breaks without appending an assistant
/// response to the session.
///
/// If called outside a DM session, the tool returns an error explaining
/// that `ignore_message` can only be used in DM sessions.
#[derive(Debug)]
pub struct IgnoreMessageTool {
    /// The context ID of the current session, used to verify this is a DM.
    context_id: String,
}

impl IgnoreMessageTool {
    pub fn new(context_id: String) -> Self {
        Self { context_id }
    }
}

#[async_trait::async_trait]
impl Tool for IgnoreMessageTool {
    fn name(&self) -> &str {
        "ignore_message"
    }

    fn description(&self) -> &str {
        "Decline to respond to this message. Only usable in DM sessions. \
         Use when you have nothing meaningful to add to the conversation. \
         The run ends early -- no response is sent or broadcast."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "description": "Brief reason for ignoring (logged internally, not sent to the other agent)"
                }
            }
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        // Guard: only allow in DM sessions.
        if !self.context_id.starts_with("dm:") {
            warn!(
                context_id = %self.context_id,
                "ignore_message called from non-DM session — rejecting"
            );
            return Err(SandboxError::InvalidParameters(
                "ignore_message can only be used in DM sessions. \
                 You are currently in a non-DM session."
                    .to_string(),
            ));
        }

        let reason = params
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("no reason given");

        info!(reason = %reason, "Agent chose to ignore message");

        Ok(serde_json::json!({
            "ignored": true,
            "reason": reason,
        }))
    }

    fn is_builtin(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a tool wired to a DM context (the happy path).
    fn make_dm_tool() -> IgnoreMessageTool {
        IgnoreMessageTool::new("dm:alice:bob".to_string())
    }

    /// Helper: create a tool wired to a non-DM context.
    fn make_non_dm_tool() -> IgnoreMessageTool {
        IgnoreMessageTool::new("web-chat-session".to_string())
    }

    #[tokio::test]
    async fn test_returns_ignored_marker_in_dm() {
        let tool = make_dm_tool();
        let result = tool
            .execute(serde_json::json!({ "reason": "nothing to add" }))
            .await
            .unwrap();
        assert_eq!(result["ignored"], true);
        assert_eq!(result["reason"], "nothing to add");
    }

    #[tokio::test]
    async fn test_default_reason_when_omitted() {
        let tool = make_dm_tool();
        let result = tool.execute(serde_json::json!({})).await.unwrap();
        assert_eq!(result["ignored"], true);
        assert_eq!(result["reason"], "no reason given");
    }

    #[tokio::test]
    async fn test_rejects_non_dm_context() {
        let tool = make_non_dm_tool();
        let result = tool
            .execute(serde_json::json!({ "reason": "whatever" }))
            .await;
        assert!(result.is_err(), "Should fail in non-DM context");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("only be used in DM sessions"),
            "Error should mention DM sessions, got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_rejects_job_context() {
        let tool = IgnoreMessageTool::new("job_abc123".to_string());
        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_err(), "Should fail in job context");
    }

    #[tokio::test]
    async fn test_rejects_subagent_context() {
        let tool = IgnoreMessageTool::new("subagent_task123".to_string());
        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_err(), "Should fail in subagent context");
    }

    #[test]
    fn test_schema_has_reason_property() {
        let tool = make_dm_tool();
        let schema = tool.parameters();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["reason"].is_object());
        // reason is optional -- no required field
        assert!(schema.get("required").is_none());
    }

    #[test]
    fn test_tool_name() {
        let tool = make_dm_tool();
        assert_eq!(tool.name(), "ignore_message");
    }

    #[test]
    fn test_is_builtin() {
        let tool = make_dm_tool();
        assert!(tool.is_builtin());
    }
}
