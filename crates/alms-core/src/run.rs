//! Run types and status for ALMS

use crate::{AgentId, SessionId, job::JobId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Accumulated token usage for a run.
///
/// `reasoning_tokens` is a separate counter populated by providers that
/// split chain-of-thought usage out of the standard `completion_tokens`
/// bucket — notably OpenAI o-series via
/// `usage.completion_tokens_details.reasoning_tokens` (#768). Other
/// providers (DeepSeek R1, xAI Grok) may or may not surface the split;
/// when they don't, the field stays `None` and reasoning cost is
/// implicitly folded into `completion_tokens`.
///
/// `cache_creation_input_tokens` / `cache_read_input_tokens` carry
/// prompt-caching metrics. `cache_creation_input_tokens` is Anthropic-only
/// (#766) — Gemini does not distinguish creation cost from input cost in
/// its `usage` surface. `cache_read_input_tokens` is shared across
/// providers: Anthropic populates it from `cache_read_input_tokens` on
/// cache-hit responses; Gemini populates it from
/// `usageMetadata.cachedContentTokenCount` when a `cachedContents` entry
/// is referenced via `cachedContent: "cachedContents/<id>"` (#769).
/// Both providers leave the fields as `None` when caching is disabled or
/// not applicable. All fields use `skip_serializing_if` so pre-#766
/// payloads remain byte-identical.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    /// Reasoning (chain-of-thought) tokens, when the provider reports them
    /// separately. `None` means "not reported" — NOT "zero".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
    /// Anthropic prompt-caching: tokens written to the cache on this
    /// request (billed at ~1.25x standard input rate, see Anthropic docs).
    /// `None` when the provider does not report the field (non-Anthropic
    /// providers, or Anthropic requests that did not carry cache markers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
    /// Prompt-caching: tokens served from the cache on this request.
    ///
    /// **Provider-neutral field.** The name is historically Anthropic-coloured
    /// but this slot is shared across providers that report "tokens served
    /// from cache". Operators seeing this populated in a `run_finished`
    /// event should cross-reference `llm.provider` or the run's model name
    /// to disambiguate which provider's cache served the tokens.
    ///
    /// - Anthropic (#766): billed at ~0.1x standard input rate; populated
    ///   from the `cache_read_input_tokens` field on the response.
    /// - Gemini (#769): populated from `usageMetadata.cachedContentTokenCount`
    ///   when a `cachedContents` entry is referenced.
    ///
    /// `None` when the provider does not report the field (cache disabled,
    /// cache miss on a request without markers, or non-caching provider).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
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
    /// Name of the agent that issued this tool call (or returned this result).
    ///
    /// Mirrors the `from_agent` metadata stored on DM session messages so
    /// that the frontend fallback merge path (which reconstructs tool rows
    /// from `run_tool_calls` when session-level persistence is missing) can
    /// attribute each reasoning block to the correct agent. Fixes #696.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_agent: Option<String>,
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
    /// Optional per-run Anthropic extended-thinking budget override.
    ///
    /// `Some(0)` explicitly disables extended thinking for just this run
    /// even when per-agent or server config would enable it. Omitting the
    /// field falls through to the per-agent override (or server default).
    /// Silently ignored when the effective provider is not Anthropic.
    #[serde(default)]
    pub thinking_budget_tokens: Option<u32>,
    /// Optional per-run OpenAI-compat reasoning-effort override (#768).
    ///
    /// Three-layer precedence: per-run > per-agent > server default from
    /// `[llm.openai].reasoning_effort`. Silently ignored when the effective
    /// provider is not OpenAI-compatible or the model isn't a reasoning
    /// model.
    #[serde(default)]
    pub reasoning_effort: Option<crate::config::ReasoningEffort>,
    /// Optional per-run Gemini extended-thinking budget override (#794).
    ///
    /// `Some(0)` explicitly disables extended thinking for just this run
    /// even when per-agent or server config would enable it. Omitting the
    /// field falls through to the per-agent override (or server default).
    /// Silently ignored when the effective provider is not Gemini.
    ///
    /// Three-layer precedence: per-run > per-agent > server default from
    /// `[llm.gemini].thinking_budget`.
    #[serde(default)]
    pub gemini_thinking_budget: Option<u32>,
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

    /// `TokenUsage` with no reasoning or cache fields set must serialize
    /// byte-identically to the pre-#766/#768 shape. Relied on by the
    /// SSE `run_finished` event and subagent-completion markers whose
    /// `skip_serializing_if = "Option::is_none"` contract depends on
    /// `None` fields being omitted entirely rather than emitted as
    /// `null`.
    #[test]
    fn token_usage_serializes_without_optional_fields_when_none() {
        let usage = TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            ..TokenUsage::default()
        };
        let value = serde_json::to_value(usage).unwrap();
        let obj = value.as_object().unwrap();

        // Required fields present.
        assert_eq!(obj.get("prompt_tokens").and_then(|v| v.as_u64()), Some(100));
        assert_eq!(
            obj.get("completion_tokens").and_then(|v| v.as_u64()),
            Some(50)
        );
        // Optional fields absent from the serialized JSON, NOT serialized
        // as `null`.
        assert!(
            obj.get("reasoning_tokens").is_none(),
            "reasoning_tokens must be skipped when None"
        );
        assert!(
            obj.get("cache_creation_input_tokens").is_none(),
            "cache_creation_input_tokens must be skipped when None"
        );
        assert!(
            obj.get("cache_read_input_tokens").is_none(),
            "cache_read_input_tokens must be skipped when None"
        );
        // Exactly the two required fields — no extras.
        assert_eq!(
            obj.len(),
            2,
            "pre-#766 shape has exactly prompt_tokens + completion_tokens when all optionals are None"
        );
    }

    /// When cache fields ARE populated they surface on the wire under
    /// their snake_case names. Pinned here so the SSE `run_finished`
    /// data struct and subagent markers can rely on the shape.
    #[test]
    fn token_usage_serializes_cache_fields_when_some() {
        let usage = TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            cache_creation_input_tokens: Some(1500),
            cache_read_input_tokens: Some(8200),
            ..TokenUsage::default()
        };
        let value = serde_json::to_value(usage).unwrap();
        assert_eq!(
            value
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64()),
            Some(1500)
        );
        assert_eq!(
            value
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64()),
            Some(8200)
        );
    }

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
            from_agent: None,
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
