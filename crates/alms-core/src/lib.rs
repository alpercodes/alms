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
pub use registry::{AgentRecord, CreateAgentRequest, validate_agent_name};
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

impl AgentId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
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
