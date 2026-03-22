//! ignore_message tool -- lets an agent decline to respond.
//!
//! When called, this tool signals that the current run should end early
//! without producing a response. Useful for DM conversations where an
//! agent has nothing meaningful to add (avoids burning through max-depth),
//! and for future group chats with `@everyone` where not every agent needs
//! to respond.

use alms_sandbox::{Tool, error::SandboxResult};
use serde_json::Value;
use tracing::info;

/// Built-in tool that lets an agent decline to respond to a message.
///
/// The tool returns a JSON marker `{"ignored": true, "reason": "..."}`.
/// The agent loop in `agent.rs` checks for this marker after processing
/// tool results and, when detected, breaks without appending an assistant
/// response to the session.
#[derive(Debug)]
pub struct IgnoreMessageTool;

impl Default for IgnoreMessageTool {
    fn default() -> Self {
        Self
    }
}

impl IgnoreMessageTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Tool for IgnoreMessageTool {
    fn name(&self) -> &str {
        "ignore_message"
    }

    fn description(&self) -> &str {
        "Decline to respond to this message. Use when you have nothing \
         meaningful to add to the conversation. The run ends early -- no \
         response is sent or broadcast."
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

    fn make_tool() -> IgnoreMessageTool {
        IgnoreMessageTool::new()
    }

    #[tokio::test]
    async fn test_returns_ignored_marker() {
        let tool = make_tool();
        let result = tool
            .execute(serde_json::json!({ "reason": "nothing to add" }))
            .await
            .unwrap();
        assert_eq!(result["ignored"], true);
        assert_eq!(result["reason"], "nothing to add");
    }

    #[tokio::test]
    async fn test_default_reason_when_omitted() {
        let tool = make_tool();
        let result = tool.execute(serde_json::json!({})).await.unwrap();
        assert_eq!(result["ignored"], true);
        assert_eq!(result["reason"], "no reason given");
    }

    #[test]
    fn test_schema_has_reason_property() {
        let tool = make_tool();
        let schema = tool.parameters();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["reason"].is_object());
        // reason is optional -- no required field
        assert!(schema.get("required").is_none());
    }

    #[test]
    fn test_tool_name() {
        let tool = make_tool();
        assert_eq!(tool.name(), "ignore_message");
    }

    #[test]
    fn test_is_builtin() {
        let tool = make_tool();
        assert!(tool.is_builtin());
    }
}
