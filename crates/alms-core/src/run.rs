//! Run types and status for ALMS

use crate::{AgentId, SessionId, job::JobId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Accumulated token usage for a run
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

/// Discriminates tool call records: the LLM requesting a tool call vs. the
/// tool returning a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolCallRole {
    /// The LLM (assistant) requested a tool invocation.
    #[serde(rename = "assistant")]
    Assistant,
    /// A tool returned its result.
    #[serde(rename = "tool")]
    Tool,
}

impl std::fmt::Display for ToolCallRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolCallRole::Assistant => write!(f, "assistant"),
            ToolCallRole::Tool => write!(f, "tool"),
        }
    }
}

impl std::str::FromStr for ToolCallRole {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "assistant" => Ok(ToolCallRole::Assistant),
            "tool" => Ok(ToolCallRole::Tool),
            other => Err(format!("unknown ToolCallRole: {other:?}")),
        }
    }
}

/// A record of a single tool call or tool result within a run.
///
/// Stored in the `run_tool_calls` table so that tool execution history is
/// scoped to the run rather than polluting the (potentially shared) session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    /// Sequence number within the run (monotonically increasing).
    pub seq: u32,
    /// Whether this record is a tool call (assistant) or tool result.
    pub role: ToolCallRole,
    /// Tool name — always set in practice; optional to allow future extension.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Provider-assigned tool call ID (correlates call to result).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_id: Option<String>,
    /// JSON-encoded tool parameters (for calls).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<String>,
    /// JSON-encoded tool result (for results).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// When this record was created.
    pub timestamp: DateTime<Utc>,
}

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
    pub usage: Option<TokenUsage>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    /// Set when this run was triggered by a scheduled job.
    pub job_id: Option<JobId>,
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
            usage: None,
            created_at: Utc::now(),
            started_at: None,
            ended_at: None,
            job_id: None,
        }
    }

    /// Create a run triggered by a scheduled job.
    pub fn for_job(session_id: SessionId, agent_id: AgentId, input: String, job_id: JobId) -> Self {
        Self {
            job_id: Some(job_id),
            ..Self::new(session_id, agent_id, input)
        }
    }

    pub fn mark_running(&mut self) {
        self.status = RunStatus::Running;
        self.started_at = Some(Utc::now());
    }

    pub fn mark_completed(&mut self, output: String, usage: TokenUsage) {
        self.status = RunStatus::Completed;
        self.output = Some(output);
        self.usage = Some(usage);
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
#[derive(Debug, Serialize, Deserialize)]
pub struct CreateRunRequest {
    pub session_id: SessionId,
    pub input: RunInput,
    /// Optional model override — uses server default when absent.
    #[serde(default)]
    pub model: Option<String>,
    /// Optional max_tokens override.
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Optional posture override: "full_control" | "guarded".
    #[serde(default)]
    pub posture: Option<String>,
}

/// Input to a run
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunInput {
    Text { text: String },
}

/// Response when creating a run
#[derive(Debug, Serialize, Deserialize)]
pub struct CreateRunResponse {
    pub run_id: RunId,
    pub session_id: SessionId,
    pub status: RunStatus,
    pub ts: DateTime<Utc>,
}

/// Run status response
#[derive(Debug, Serialize, Deserialize)]
pub struct RunStatusResponse {
    pub run_id: RunId,
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub status: RunStatus,
    /// The agent's text response (populated when run completes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    /// Error message (populated when run fails).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub usage: Option<TokenUsage>,
    pub ts: DateTime<Utc>,
    /// Set when this run was triggered by a scheduled job.
    pub job_id: Option<JobId>,
    /// Number of tool call records stored for this run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_count: Option<u32>,
}

impl From<Run> for RunStatusResponse {
    fn from(run: Run) -> Self {
        Self {
            run_id: run.run_id,
            session_id: run.session_id,
            agent_id: run.agent_id,
            status: run.status,
            response: run.output.clone(),
            error: run.error.clone(),
            started_at: run.started_at,
            ended_at: run.ended_at,
            usage: run.usage,
            ts: run
                .ended_at
                .unwrap_or_else(|| run.started_at.unwrap_or(run.created_at)),
            job_id: run.job_id,
            tool_call_count: None,
        }
    }
}
