//! list_agents tool -- lets agents discover who they can talk to.
//!
//! Reads from the agent registry (SqliteStore) and returns each agent's
//! name, description, and last activity timestamp.

use alms_sandbox::{SandboxError, Tool, error::SandboxResult};
use alms_session::SessionManager;
use serde_json::Value;
use std::sync::Arc;

/// Built-in tool that lists all registered agents in the system.
///
/// Agents need this to discover who they can send messages to via
/// `send_message`. Returns name, description, and last activity for
/// each registered agent.
#[derive(Debug)]
pub struct ListAgentsTool {
    session_manager: Arc<SessionManager>,
    /// The calling agent's name -- excluded from the list (you don't need
    /// to discover yourself).
    self_name: String,
}

impl ListAgentsTool {
    pub fn new(session_manager: Arc<SessionManager>, self_name: String) -> Self {
        Self {
            session_manager,
            self_name,
        }
    }
}

#[async_trait::async_trait]
impl Tool for ListAgentsTool {
    fn name(&self) -> &str {
        "list_agents"
    }

    fn description(&self) -> &str {
        "List all registered agents in the system. Returns each agent's \
         name and description. Use this to discover agents you can \
         communicate with via send_message."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
        })
    }

    async fn execute(&self, _params: Value) -> SandboxResult<Value> {
        let store = self.session_manager.store().ok_or_else(|| {
            SandboxError::Io("No agent store available (SQLite not configured)".into())
        })?;

        let agents = store
            .list_agents()
            .map_err(|e| SandboxError::Io(format!("Failed to list agents: {e}")))?;

        let agent_list: Vec<Value> = agents
            .iter()
            .filter(|a| a.name != self.self_name)
            .map(|a| {
                serde_json::json!({
                    "name": a.name,
                    "description": a.description,
                    "last_active": a.last_active.to_rfc3339(),
                })
            })
            .collect();

        Ok(serde_json::json!({
            "agents": agent_list,
            "count": agent_list.len(),
        }))
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn is_auto_approved(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alms_session::SessionConfig;

    fn make_tool() -> ListAgentsTool {
        let mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        ListAgentsTool::new(mgr, "self-agent".into())
    }

    #[tokio::test]
    async fn test_no_store_returns_error() {
        let tool = make_tool();
        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_schema_is_empty_object() {
        let tool = make_tool();
        let schema = tool.parameters();
        assert_eq!(schema["type"], "object");
        // No required fields
        assert!(schema.get("required").is_none());
    }
}
