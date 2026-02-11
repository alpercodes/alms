//! Run types and status for ALMS

use crate::{AgentId, SessionId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for runs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RunId(pub Uuid);

impl RunId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for RunId {
    fn default() -> Self {
        Self::new()
    }
}

/// Status of a run
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Run is queued and waiting to start
    Queued,
    /// Run is currently executing
    Running,
    /// Run completed successfully
    Completed,
    /// Run failed due to error
    Failed,
    /// Run was cancelled by user
    Cancelled,
}

/// A run represents a single agent execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub run_id: RunId,
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub status: RunStatus,
    pub input: String,
    pub output: Option<String>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
}

impl Run {
    pub fn new(session_id: SessionId, agent_id: AgentId, input: String) -> Self {
        Self {
            run_id: RunId::new(),
            session_id,
            agent_id,
            status: RunStatus::Queued,
            input,
            output: None,
            error: None,
            created_at: Utc::now(),
            started_at: None,
            ended_at: None,
        }
    }

    pub fn mark_running(&mut self) {
        self.status = RunStatus::Running;
        self.started_at = Some(Utc::now());
    }

    pub fn mark_completed(&mut self, output: String) {
        self.status = RunStatus::Completed;
        self.output = Some(output);
        self.ended_at = Some(Utc::now());
    }

    pub fn mark_failed(&mut self, error: String) {
        self.status = RunStatus::Failed;
        self.error = Some(error);
        self.ended_at = Some(Utc::now());
    }

    pub fn mark_cancelled(&mut self) {
        self.status = RunStatus::Cancelled;
        self.ended_at = Some(Utc::now());
    }
}

/// Request to create a run
#[derive(Debug, Deserialize)]
pub struct CreateRunRequest {
    pub session_id: SessionId,
    pub input: RunInput,
}

/// Input to a run
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunInput {
    Text { text: String },
}

/// Response when creating a run
#[derive(Debug, Serialize)]
pub struct CreateRunResponse {
    pub run_id: RunId,
    pub session_id: SessionId,
    pub status: RunStatus,
    pub ts: DateTime<Utc>,
}

/// Run status response
#[derive(Debug, Serialize)]
pub struct RunStatusResponse {
    pub run_id: RunId,
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub status: RunStatus,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub ts: DateTime<Utc>,
}

impl From<Run> for RunStatusResponse {
    fn from(run: Run) -> Self {
        Self {
            run_id: run.run_id,
            session_id: run.session_id,
            agent_id: run.agent_id,
            status: run.status,
            started_at: run.started_at,
            ended_at: run.ended_at,
            ts: Utc::now(),
        }
    }
}