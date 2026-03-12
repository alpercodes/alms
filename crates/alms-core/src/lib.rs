pub mod agent;
pub mod audit;
pub mod channel;
pub mod config;
pub mod error;
pub mod job;
pub mod registry;
pub mod run;

pub use channel::{Channel, ChannelConfig, ChannelMessage, IncomingMessage, OutgoingMessage};

pub use audit::{AuditDecision, AuditEvent};
pub use config::AlmsConfig;
pub use error::{AlmsError, AlmsResult};
pub use job::{CreateJobRequest, Job, JobId, JobSchedule, JobStatus};
pub use registry::{
    AgentRecord, CreateAgentRequest, UpdateAgentRequest, WORKSPACE_FILENAMES, init_workspace_files,
    validate_agent_name,
};
pub use run::{
    CreateRunRequest, CreateRunResponse, Run, RunId, RunInput, RunStatus, RunStatusResponse,
    TokenUsage,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for agents
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(pub Uuid);

/// ALMS namespace UUID for deterministic v5 derivation.
const ALMS_NAMESPACE: Uuid = Uuid::from_bytes([
    0xa1, 0xb2, 0xc3, 0xd4, 0xe5, 0xf6, 0x47, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89,
]);

impl AgentId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Derive a deterministic AgentId from a parent agent ID and a subagent name.
    /// Same inputs always produce the same output (UUID v5).
    pub fn deterministic(parent: AgentId, subagent_name: &str) -> Self {
        let input = format!("{}:{}", parent.0, subagent_name);
        Self(Uuid::new_v5(&ALMS_NAMESPACE, input.as_bytes()))
    }
}

impl Default for AgentId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for AgentId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
    }
}

/// Unique identifier for sessions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

/// Timestamp wrapper for consistent handling
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timestamp(pub DateTime<Utc>);

impl Timestamp {
    pub fn now() -> Self {
        Self(Utc::now())
    }
}

impl Default for Timestamp {
    fn default() -> Self {
        Self::now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_id_deterministic_stable() {
        let parent = AgentId::new();
        let a = AgentId::deterministic(parent, "reviewer");
        let b = AgentId::deterministic(parent, "reviewer");
        assert_eq!(a, b, "same inputs must produce same AgentId");
    }

    #[test]
    fn test_agent_id_deterministic_differs_by_name() {
        let parent = AgentId::new();
        let a = AgentId::deterministic(parent, "reviewer");
        let b = AgentId::deterministic(parent, "researcher");
        assert_ne!(a, b, "different names must produce different AgentIds");
    }

    #[test]
    fn test_agent_id_deterministic_differs_by_parent() {
        let p1 = AgentId::new();
        let p2 = AgentId::new();
        let a = AgentId::deterministic(p1, "reviewer");
        let b = AgentId::deterministic(p2, "reviewer");
        assert_ne!(a, b, "different parents must produce different AgentIds");
    }
}
