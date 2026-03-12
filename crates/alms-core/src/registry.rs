//! Agent registry types — persistent named agents.
//!
//! An `AgentRecord` represents a named, persistent agent registered in the system.
//! Each agent has a unique slug name, optional per-agent config overrides, and
//! its own workspace/sessions/jobs.

use crate::{AgentId, AlmsError, AlmsResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A persistent agent registered in the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRecord {
    pub id: AgentId,
    pub name: String,
    pub description: String,
    /// Per-agent model override (None = use server default).
    pub model: Option<String>,
    /// Per-agent system prompt override (None = use server default).
    pub system_prompt: Option<String>,
    /// Per-agent posture override (None = use server default).
    pub posture: Option<String>,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
}

/// Request body for creating a new agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateAgentRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub posture: Option<String>,
    #[serde(default)]
    pub is_default: Option<bool>,
}

/// Validate an agent name slug.
///
/// Rules: 1–64 chars, lowercase alphanumeric + hyphens, no leading/trailing hyphens.
pub fn validate_agent_name(name: &str) -> AlmsResult<()> {
    if name.is_empty() || name.len() > 64 {
        return Err(AlmsError::InvalidConfig(format!(
            "agent name must be 1–64 characters, got {} characters",
            name.len()
        )));
    }

    let bytes = name.as_bytes();

    // No leading or trailing hyphens
    if bytes[0] == b'-' || bytes[bytes.len() - 1] == b'-' {
        return Err(AlmsError::InvalidConfig(format!(
            "agent name must not start or end with a hyphen, got '{name}'"
        )));
    }

    // Only lowercase alphanumeric + hyphens
    for ch in name.chars() {
        if !ch.is_ascii_lowercase() && !ch.is_ascii_digit() && ch != '-' {
            return Err(AlmsError::InvalidConfig(format!(
                "agent name must contain only lowercase letters, digits, and hyphens, got '{ch}' in '{name}'"
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_names() {
        assert!(validate_agent_name("default").is_ok());
        assert!(validate_agent_name("atlas").is_ok());
        assert!(validate_agent_name("my-agent-1").is_ok());
        assert!(validate_agent_name("a").is_ok());
        assert!(validate_agent_name("agent-v2").is_ok());
        assert!(validate_agent_name("a1b2c3").is_ok());
    }

    #[test]
    fn test_invalid_empty() {
        assert!(validate_agent_name("").is_err());
    }

    #[test]
    fn test_invalid_too_long() {
        let name = "a".repeat(65);
        assert!(validate_agent_name(&name).is_err());
    }

    #[test]
    fn test_invalid_uppercase() {
        assert!(validate_agent_name("MyAgent").is_err());
    }

    #[test]
    fn test_invalid_underscore() {
        assert!(validate_agent_name("my_agent").is_err());
    }

    #[test]
    fn test_invalid_leading_hyphen() {
        assert!(validate_agent_name("-agent").is_err());
    }

    #[test]
    fn test_invalid_trailing_hyphen() {
        assert!(validate_agent_name("agent-").is_err());
    }

    #[test]
    fn test_invalid_spaces() {
        assert!(validate_agent_name("my agent").is_err());
    }

    #[test]
    fn test_max_length_ok() {
        let name = "a".repeat(64);
        assert!(validate_agent_name(&name).is_ok());
    }
}
