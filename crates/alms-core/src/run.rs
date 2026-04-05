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
    /// Set when this run is a subagent execution spawned by another run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<RunId>,
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
            parent_run_id: None,
        }
    }

    /// Create a run triggered by a scheduled job.
    pub fn for_job(session_id: SessionId, agent_id: AgentId, input: String, job_id: JobId) -> Self {
        Self {
            job_id: Some(job_id),
            ..Self::new(session_id, agent_id, input)
        }
    }

    /// Create a run for a subagent spawned by another run.
    pub fn for_subagent(
        session_id: SessionId,
        agent_id: AgentId,
        input: String,
        parent_run_id: RunId,
    ) -> Self {
        Self {
            parent_run_id: Some(parent_run_id),
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
    /// Optional posture override: "full_control" | "guarded" | "autonomous".
    #[serde(default)]
    pub posture: Option<String>,
    /// Optional provider override: "openai" | "anthropic" | "openrouter".
    #[serde(default)]
    pub provider: Option<String>,
    /// When true, the runtime emits a `context_debug` SSE event with the
    /// full assembled context window before calling the LLM. Used by the
    /// web UI to inspect exactly what the LLM sees.
    #[serde(default)]
    pub debug_mode: Option<bool>,
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
    /// Set when this run is a subagent execution spawned by another run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<RunId>,
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
            parent_run_id: run.parent_run_id,
            tool_call_count: None,
        }
    }
}

/// Trait for registering and updating runs from outside the gateway.
///
/// The Coordinator uses this to make subagent runs visible in the RunManager
/// without depending on `alms-gateway`. The gateway implements this trait
/// on its `RunManager`.
pub trait RunRegistrar: Send + Sync + std::fmt::Debug {
    /// Register a new run (insert into the run store and persist to SQLite).
    fn register_run(&self, run: Run);
    /// Update an existing run (e.g. mark as completed/failed).
    fn update_run(&self, run: Run);
}

/// Check whether `ignore_message` was **successfully** called during a run.
///
/// A call is only counted as successful if:
/// 1. An `Assistant`-role record exists with `tool_name == "ignore_message"`, AND
/// 2. A corresponding `Tool`-role result record (matched by `tool_id`) exists
///    whose result is NOT an error (does not start with `"Error:"`).
///
/// This prevents false positives when:
/// - Both `send_message` and `ignore_message` appear in a conflict batch
///   (PR #365) -- both are blocked and receive error results.
/// - `ignore_message` is called from a non-DM session (PR #415) -- the tool
///   returns an `InvalidParameters` error.
/// - Any other tool execution failure occurs.
///
/// Shared between `alms-runtime` (agent loop termination) and `alms-gateway`
/// (`execute_run` ignore-message detection).
pub fn ran_ignore_message_successfully(records: &[ToolCallRecord]) -> bool {
    records.iter().any(|r| {
        r.role == ToolCallRole::Assistant
            && r.tool_name.as_deref() == Some("ignore_message")
            && r.tool_id.as_ref().is_some_and(|call_id| {
                records.iter().any(|result| {
                    result.role == ToolCallRole::Tool
                        && result.tool_id.as_deref() == Some(call_id.as_str())
                        && !result
                            .result
                            .as_deref()
                            .is_some_and(|res| res.starts_with("Error:"))
                })
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_record(
        role: ToolCallRole,
        tool_name: &str,
        tool_id: &str,
        result: Option<&str>,
    ) -> ToolCallRecord {
        ToolCallRecord {
            seq: 0,
            role,
            tool_name: Some(tool_name.to_string()),
            tool_id: Some(tool_id.to_string()),
            params: None,
            result: result.map(String::from),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn test_ran_ignore_successfully_clean_call() {
        let records = vec![
            make_record(ToolCallRole::Assistant, "ignore_message", "call_1", None),
            make_record(
                ToolCallRole::Tool,
                "ignore_message",
                "call_1",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(ran_ignore_message_successfully(&records));
    }

    #[test]
    fn test_ran_ignore_conflict_blocked() {
        let conflict_error = format!("Error: {}", crate::DM_CONFLICT_MSG);
        let records = vec![
            make_record(ToolCallRole::Assistant, "send_message", "tc_send", None),
            make_record(ToolCallRole::Assistant, "ignore_message", "tc_ignore", None),
            make_record(
                ToolCallRole::Tool,
                "send_message",
                "tc_send",
                Some(&conflict_error),
            ),
            make_record(
                ToolCallRole::Tool,
                "ignore_message",
                "tc_ignore",
                Some(&conflict_error),
            ),
        ];
        assert!(
            !ran_ignore_message_successfully(&records),
            "conflict-blocked ignore_message should not count as successful"
        );
    }

    #[test]
    fn test_ran_ignore_conflict_then_clean_ignore() {
        // First batch: conflict -- both tools blocked
        let conflict_error = format!("Error: {}", crate::DM_CONFLICT_MSG);
        let records = vec![
            make_record(ToolCallRole::Assistant, "send_message", "tc_send_1", None),
            make_record(
                ToolCallRole::Assistant,
                "ignore_message",
                "tc_ignore_1",
                None,
            ),
            make_record(
                ToolCallRole::Tool,
                "send_message",
                "tc_send_1",
                Some(&conflict_error),
            ),
            make_record(
                ToolCallRole::Tool,
                "ignore_message",
                "tc_ignore_1",
                Some(&conflict_error),
            ),
            // Second batch: agent retried with just ignore_message -- success
            make_record(
                ToolCallRole::Assistant,
                "ignore_message",
                "tc_ignore_2",
                None,
            ),
            make_record(
                ToolCallRole::Tool,
                "ignore_message",
                "tc_ignore_2",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(
            ran_ignore_message_successfully(&records),
            "after conflict resolution, a successful ignore_message should be detected"
        );
    }

    #[test]
    fn test_ran_ignore_conflict_then_send_message() {
        // First batch: conflict -- both tools blocked
        let conflict_error = format!("Error: {}", crate::DM_CONFLICT_MSG);
        let records = vec![
            make_record(ToolCallRole::Assistant, "send_message", "tc_send_1", None),
            make_record(
                ToolCallRole::Assistant,
                "ignore_message",
                "tc_ignore_1",
                None,
            ),
            make_record(
                ToolCallRole::Tool,
                "send_message",
                "tc_send_1",
                Some(&conflict_error),
            ),
            make_record(
                ToolCallRole::Tool,
                "ignore_message",
                "tc_ignore_1",
                Some(&conflict_error),
            ),
            // Second batch: agent retried with just send_message -- success
            make_record(ToolCallRole::Assistant, "send_message", "tc_send_2", None),
            make_record(
                ToolCallRole::Tool,
                "send_message",
                "tc_send_2",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(
            !ran_ignore_message_successfully(&records),
            "conflict-batch followed by send_message should NOT detect ignore_message"
        );
    }

    #[test]
    fn test_ran_ignore_no_tool_result() {
        // Assistant record exists but no corresponding Tool result
        let records = vec![make_record(
            ToolCallRole::Assistant,
            "ignore_message",
            "call_1",
            None,
        )];
        assert!(
            !ran_ignore_message_successfully(&records),
            "Assistant record without Tool result should not count"
        );
    }

    #[test]
    fn test_ran_ignore_only_send_message() {
        let records = vec![
            make_record(ToolCallRole::Assistant, "send_message", "call_1", None),
            make_record(
                ToolCallRole::Tool,
                "send_message",
                "call_1",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(
            !ran_ignore_message_successfully(&records),
            "send_message should not be detected as ignore_message"
        );
    }

    #[test]
    fn test_ran_ignore_empty_records() {
        let records: Vec<ToolCallRecord> = vec![];
        assert!(!ran_ignore_message_successfully(&records));
    }

    /// When `ignore_message` is called from a non-DM session, the tool
    /// returns an `InvalidParameters` error. The formatted result starts
    /// with `"Error:"` and must NOT be treated as a successful call.
    #[test]
    fn test_ran_ignore_non_dm_error_not_successful() {
        let records = vec![
            make_record(ToolCallRole::Assistant, "ignore_message", "tc_ign", None),
            make_record(
                ToolCallRole::Tool,
                "ignore_message",
                "tc_ign",
                Some(
                    "Error: Invalid parameters: ignore_message can only be used in DM sessions. \
                     You are currently in a non-DM session.",
                ),
            ),
        ];
        assert!(
            !ran_ignore_message_successfully(&records),
            "ignore_message that returned an error should not count as successful"
        );
    }

    /// Any `"Error:"` prefix in the tool result should prevent the call
    /// from being treated as successful, regardless of the error content.
    #[test]
    fn test_ran_ignore_generic_error_not_successful() {
        let records = vec![
            make_record(ToolCallRole::Assistant, "ignore_message", "tc_ign", None),
            make_record(
                ToolCallRole::Tool,
                "ignore_message",
                "tc_ign",
                Some("Error: some unexpected failure"),
            ),
        ];
        assert!(
            !ran_ignore_message_successfully(&records),
            "any tool error should prevent ignore_message from being treated as successful"
        );
    }
}
