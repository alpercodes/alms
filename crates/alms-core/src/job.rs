//! Job types for scheduled agent runs.

use crate::AgentId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for a scheduled job
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct JobId(pub Uuid);

impl JobId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for JobId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// When and how often a job runs
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobSchedule {
    /// Run once at a specific UTC time
    Once { run_at: DateTime<Utc> },
    /// Run on a 5-field cron expression (e.g. `"0 9 * * 1-5"`)
    Recurring { cron: String },
}

/// Lifecycle status of a job
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Waiting to fire for the first time
    Pending,
    /// Recurring job that has fired at least once
    Active,
    /// Cancelled — will not fire again
    Cancelled,
}

/// A scheduled agent job
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub agent_id: AgentId,
    /// Prompt sent to the agent when this job fires
    pub prompt: String,
    pub schedule: JobSchedule,
    pub status: JobStatus,
    pub created_at: DateTime<Utc>,
    /// Next scheduled fire time (`None` for recurring until the scheduler computes it)
    pub next_run_at: Option<DateTime<Utc>>,
    /// Last time this job fired successfully
    pub last_run_at: Option<DateTime<Utc>>,
}

/// Request body for `POST /jobs`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateJobRequest {
    pub agent_id: AgentId,
    pub prompt: String,
    pub schedule: JobSchedule,
}
