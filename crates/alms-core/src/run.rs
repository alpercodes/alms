// SPDX-License-Identifier: Apache-2.0

//! Run types and status for ALMS

use crate::lifecycle::MAX_LIFECYCLE_REVISION;
use crate::{
    AgentId, SessionId, config::ReasoningEffort, job::JobId, lifecycle::TransitionOutcome,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Snapshot of the **fully layered** configuration that a run committed to
/// at start-time (#837).
///
/// The gateway's two-layer precedence chain (per-agent > server default)
/// is implemented in
/// [`alms-gateway::runs::lifecycle`](../../../alms-gateway/src/runs/lifecycle.rs)
/// and the values resolved there are the ones the LLM adapter actually
/// uses on the wire. Persisting them here makes "I set model X but Y was
/// used" reports falsifiable from stored data alone — operators no longer
/// need to correlate the original PATCH request, the registry state at
/// run-start, and the server config at run-start across log files that may
/// have rotated out.
///
/// Per-run overrides were removed in the #941 pivot — agents are now the
/// single per-tenant config surface. Operators change agent config via
/// `PATCH /agents/{id}` (or server defaults via `PATCH /settings`) before
/// starting a run, never per-run.
///
/// All fields snapshot the **effective** value: the post-layering result,
/// not the raw per-run override. `provider` and `model` come from the
/// resolved [`crate::AgentConfig`]'s LLM client (`provider()` /
/// `default_model()`); the remaining fields come from the resolved
/// [`crate::AgentConfig`] itself.
///
/// Serialized with `skip_serializing_if = "Option::is_none"` on the
/// optional reasoning / thinking fields so a snapshot that didn't engage
/// (e.g. a Gemini-only field on an Anthropic run) stays absent on the
/// wire rather than emitting `null`.
///
/// **Per-knob shape asymmetry (intentional).** The three reasoning /
/// thinking-budget fields below carry deliberately different shapes:
/// - `thinking_budget_tokens` (Anthropic) — `u32`, never absent on the wire.
/// - `reasoning_effort` (OpenAI-compat) — `Option<ReasoningEffort>`,
///   skip-serialized when `None`.
/// - `gemini_thinking_budget` (Gemini) — `Option<u32>`, skip-serialized
///   when `None`.
///
/// Each field mirrors its underlying [`alms_runtime::AgentConfig`] shape
/// (`anthropic_thinking_budget: u32`, `openai_reasoning_effort:
/// Option<ReasoningEffort>`, `gemini_thinking_budget: Option<u32>`), so
/// the snapshot is a faithful projection of what the LLM adapter saw — not
/// a uniformized re-statement. A future reader who tries to "fix" the
/// asymmetry by promoting Anthropic to `Option<u32>` would lose information:
/// at the merged-`AgentConfig` layer Anthropic has no `None`-vs-`Some(0)`
/// semantic — `0` is the genuine "disabled" value — whereas Gemini
/// distinguishes `None` (inherit from server default) from `Some(0)`
/// (explicit per-layer disable). For the Anthropic case the snapshot
/// collapses two distinct origins (`server default of 0` vs `explicit
/// per-layer Some(0)`) into the same `0`. That's a known triage
/// limitation of the snapshot, not a bug to "uniformize" away — the
/// snapshot's job is to record the merged `AgentConfig` value the
/// adapter committed to (the input to its wire-shape decision), not the
/// literal wire bytes. (The Anthropic adapter, for example, omits the
/// `thinking` block entirely when the budget is `0` — so a literal-wire
/// reading of the snapshot would have no field to record. Recording the
/// merged value preserves the operator-visible "I asked for 0" answer.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedRunConfig {
    /// Effective LLM provider (`"openai"`, `"anthropic"`, `"openrouter"`,
    /// `"gemini"`, ...). Read from the resolved `LlmClient::provider()`.
    pub provider: String,
    /// Effective model slug. Read from the resolved
    /// `LlmClient::default_model()` after per-agent / server-default
    /// layering and any `apply_provider`-driven model rewrites have settled.
    /// May be empty when a per-agent provider switch with no per-agent
    /// model and no provider-entry model triggered the #860 leak guard —
    /// the empty string is preserved here so triage can confirm the fail-fast
    /// path actually engaged.
    pub model: String,
    /// Effective `max_tokens` cap — the value the adapter sends as the
    /// `max_tokens` field on the request body.
    pub max_tokens: u32,
    /// Effective execution posture: `"full_control"`, `"guarded"`, or
    /// `"autonomous"`. Stored as a string for wire stability — matches the
    /// shape of [`CreateRunRequest::posture`] and survives potential future
    /// posture additions without breaking older clients.
    pub posture: String,
    /// Effective `debug_mode` flag. Reflects the value the runtime actually
    /// uses — the per-agent record value merged in `resolve_agent_config`.
    /// Per-run overrides for `debug_mode` were removed in the #941 pivot,
    /// and the #546-era flip that forced `true` for system-triggered
    /// notification runs on user-facing sessions was removed too (it
    /// overrode the per-agent toggle) — the per-agent record is now the
    /// only operator-facing input into this field.
    pub debug_mode: bool,
    /// Effective Anthropic extended-thinking budget in tokens (#767).
    /// `0` means extended thinking is disabled for this run; non-zero is
    /// the budget the adapter sent on the wire. Persisted as a `u32`
    /// (matching [`crate::AgentConfig::anthropic_thinking_budget`]'s
    /// shape) rather than `Option<u32>` because `0` is a meaningful
    /// state, not absence.
    pub thinking_budget_tokens: u32,
    /// Effective OpenAI-compat reasoning effort (#768). `None` means the
    /// adapter did not send `reasoning_effort` on the wire (either the
    /// effective provider isn't OpenAI-compatible, the model isn't a
    /// reasoning model, or no value was configured at any layer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Effective Gemini extended-thinking budget in tokens (#794). `None`
    /// or `Some(0)` means extended thinking is disabled; non-zero is the
    /// budget the adapter sent on the wire as `thinkingConfig.thinkingBudget`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gemini_thinking_budget: Option<u32>,
}

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
    ///
    /// This is the LLM provider's id (`call_abc123`), NOT ALMS's own
    /// correlator — see [`Self::tool_invocation_id`], which is what the
    /// event stream and the frontend key on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_id: Option<String>,
    /// ALMS's own invocation correlator, as a string-formatted UUID (#5).
    ///
    /// This is the id carried by the `tool_start` / `tool_end` /
    /// `subagent_started` / `subagent_completed` SSE events, written into
    /// `session_messages` metadata as `tool_invocation_id`, and used as the
    /// identity key by every frontend lookup. [`Self::tool_id`] is the
    /// *provider's* id and is not known to any of them.
    ///
    /// Without this, a chat row the frontend reconstructs from
    /// `run_tool_calls` is structurally uncorrelatable with the live event
    /// stream: a `tool_end` cannot find the row by id and has to guess, a
    /// rehydrated subagent chip stores the provider id so nothing can
    /// terminate it, and the `subagent_started` session-id lookup (keyed by
    /// invocation id) misses, leaving a dead "View session" button.
    ///
    /// `None` for rows written before #5. Both persistence paths tolerate
    /// that; the frontend falls back to [`Self::tool_id`], which is the
    /// pre-#5 behaviour.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_invocation_id: Option<String>,
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

impl RunStatus {
    /// Returns `true` when the status is one of the absorbing terminal
    /// states (`Completed`, `Failed`, `Cancelled`).
    ///
    /// Used by [`Run::mark_completed`] / [`Run::mark_failed`] /
    /// [`Run::mark_cancelled`] to make terminal transitions idempotent —
    /// callers receive `false` and know to skip the terminal-arm
    /// bookkeeping (#1052: DM `dm_conversation_ended` emission, episodic
    /// summary spawn, `run_finished`/`run_cancelled`/`run_error`
    /// broadcasts).
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// The only supported mutations of a run's lifecycle state.
#[derive(Debug, Clone)]
pub enum RunTransition {
    Start {
        resolved_config: Option<ResolvedRunConfig>,
    },
    Complete {
        output: String,
        usage: TokenUsage,
    },
    Fail {
        error: String,
        terminal_reason: String,
    },
    Cancel {
        terminal_reason: String,
    },
}

impl RunTransition {
    fn target_status(&self) -> RunStatus {
        match self {
            Self::Start { .. } => RunStatus::Running,
            Self::Complete { .. } => RunStatus::Completed,
            Self::Fail { .. } => RunStatus::Failed,
            Self::Cancel { .. } => RunStatus::Cancelled,
        }
    }
}

/// A run represents a single agent execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub run_id: RunId,
    pub session_id: SessionId,
    pub agent_id: AgentId,
    status: RunStatus,
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
    /// Snapshot of the layered config (per-agent > server default) the run
    /// committed to at start-time (#837). `None` for runs created before
    /// the snapshot was wired up (old SQLite rows) or for runs that never
    /// advanced past the queued state. Populated once at the transition
    /// from `Queued` to `Running` and never mutated afterwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resolved_config: Option<ResolvedRunConfig>,
    /// Monotonically increases for every accepted lifecycle transition.
    #[serde(default)]
    lifecycle_revision: u64,
    /// Machine-readable reason for a terminal failure or cancellation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal_reason: Option<String>,
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
            resolved_config: None,
            lifecycle_revision: 0,
            terminal_reason: None,
        }
    }

    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn from_persisted(
        run_id: RunId,
        session_id: SessionId,
        agent_id: AgentId,
        status: RunStatus,
        input: String,
        output: Option<String>,
        error: Option<String>,
        usage: Option<TokenUsage>,
        created_at: DateTime<Utc>,
        started_at: Option<DateTime<Utc>>,
        ended_at: Option<DateTime<Utc>>,
        job_id: Option<JobId>,
        parent_run_id: Option<RunId>,
        resolved_config: Option<ResolvedRunConfig>,
        lifecycle_revision: u64,
        terminal_reason: Option<String>,
    ) -> Self {
        Self {
            run_id,
            session_id,
            agent_id,
            status,
            input,
            output,
            error,
            usage,
            created_at,
            started_at,
            ended_at,
            job_id,
            parent_run_id,
            resolved_config,
            lifecycle_revision,
            terminal_reason,
        }
    }

    pub fn status(&self) -> RunStatus {
        self.status
    }

    pub fn resolved_config(&self) -> Option<&ResolvedRunConfig> {
        self.resolved_config.as_ref()
    }

    pub fn lifecycle_revision(&self) -> u64 {
        self.lifecycle_revision
    }

    pub fn terminal_reason(&self) -> Option<&str> {
        self.terminal_reason.as_deref()
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

    /// Apply a lifecycle transition under the caller's synchronization
    /// boundary. This is the single authority for legal state changes.
    pub fn transition(&mut self, transition: RunTransition) -> TransitionOutcome<RunStatus> {
        let from = self.status;
        let to = transition.target_status();

        if from == to {
            return TransitionOutcome::NoOp {
                state: from,
                revision: self.lifecycle_revision,
            };
        }

        let legal = matches!(
            (&transition, from),
            (RunTransition::Start { .. }, RunStatus::Queued)
                | (RunTransition::Complete { .. }, RunStatus::Running)
                | (
                    RunTransition::Fail { .. } | RunTransition::Cancel { .. },
                    RunStatus::Queued | RunStatus::Running
                )
        );
        if !legal {
            return TransitionOutcome::Rejected {
                from,
                to,
                revision: self.lifecycle_revision,
            };
        }
        if self.lifecycle_revision >= MAX_LIFECYCLE_REVISION {
            return TransitionOutcome::Rejected {
                from,
                to,
                revision: self.lifecycle_revision,
            };
        }

        let now = Utc::now();
        match transition {
            RunTransition::Start { resolved_config } => {
                self.started_at = Some(now);
                self.resolved_config = resolved_config;
                self.terminal_reason = None;
            }
            RunTransition::Complete { output, usage } => {
                self.output = Some(output);
                self.usage = Some(usage);
                self.ended_at = Some(now);
                self.terminal_reason = None;
            }
            RunTransition::Fail {
                error,
                terminal_reason,
            } => {
                self.error = Some(error);
                self.ended_at = Some(now);
                self.terminal_reason = Some(terminal_reason);
            }
            RunTransition::Cancel { terminal_reason } => {
                self.ended_at = Some(now);
                self.terminal_reason = Some(terminal_reason);
            }
        }
        self.status = to;
        self.lifecycle_revision += 1;
        TransitionOutcome::Applied {
            from,
            to,
            revision: self.lifecycle_revision,
        }
    }

    pub fn mark_running(&mut self) -> bool {
        self.transition(RunTransition::Start {
            resolved_config: None,
        })
        .is_applied()
    }

    /// Transition the run to `Completed` and stamp `ended_at`.
    ///
    /// Returns `true` if the run was actually transitioned from `Running`
    /// to `Completed`, `false` if the run was already in a terminal state
    /// (`Cancelled`, `Completed`, or `Failed`) or was still `Queued`. The
    /// returned bool is used by `RunManager::mark_run_as_completed` to
    /// gate duplicate `run_finished` SSE broadcasts when an HTTP cancel
    /// races against natural completion (issue #1046 — symmetric hole in
    /// the original cancel-flip fix; refined in #1052 to also gate
    /// terminal-arm bookkeeping such as `dm_conversation_ended` and
    /// episodic-summary writes).
    ///
    /// `Queued → Completed` is rejected (returns `false`) because in
    /// practice `execute_run`'s `Ok(_)` arm only fires after
    /// `mark_run_as_running_with_config` has flipped the run to
    /// `Running`. Allowing `Queued → Completed` would silently mask a
    /// bug in the run lifecycle if that invariant ever broke.
    ///
    /// Idempotent for the run state: `ended_at` is only stamped on the
    /// first transition so it reflects when completion actually
    /// happened, not when a later cleanup pass re-marked the run.
    #[must_use = "callers that broadcast a terminal SSE event must gate the broadcast on this bool — discarding it regresses the #1046 duplicate-broadcast guard. If the discard is intentional (e.g. coordinator persistence, test setup), bind to `let _ =` to make it explicit."]
    pub fn mark_completed(&mut self, output: String, usage: TokenUsage) -> bool {
        self.transition(RunTransition::Complete { output, usage })
            .is_applied()
    }

    /// Transition the run to `Failed` and stamp `ended_at`.
    ///
    /// Returns `true` if the run was actually transitioned to `Failed`,
    /// `false` if it was already in a terminal state. Mirrors the
    /// idempotency contract of [`Self::mark_completed`] and
    /// [`Self::mark_cancelled`] so the gateway's three terminal-arm
    /// broadcasts can share a single first-writer-wins shape (issue
    /// #1046; gated terminal-arm bookkeeping added in #1052).
    ///
    /// Allows both `Running → Failed` AND `Queued → Failed`. The
    /// `Queued → Failed` transition is the legitimate setup-error path:
    /// `execute_run` runs `resolve_agent_config` and `AgentRuntime::new`
    /// BEFORE `mark_run_as_running_with_config`, and both can fail. In
    /// those early-failure branches the run never reaches `Running` —
    /// rejecting `Queued → Failed` would leave the run stuck in
    /// `Queued` after the early-error broadcast and the queue head
    /// advance, which is observably worse than the duplicate-event hole
    /// being closed here.
    ///
    /// Idempotent for the run state: `ended_at` is only stamped on the
    /// first transition.
    #[must_use = "callers that broadcast a terminal SSE event must gate the broadcast on this bool — discarding it regresses the #1046 duplicate-broadcast guard. If the discard is intentional (e.g. coordinator persistence, test setup), bind to `let _ =` to make it explicit."]
    pub fn mark_failed(&mut self, error: String) -> bool {
        self.transition(RunTransition::Fail {
            error,
            terminal_reason: "failed".to_string(),
        })
        .is_applied()
    }

    /// Transition the run to `Cancelled` and stamp `ended_at`.
    ///
    /// Returns `true` if the run was actually transitioned (i.e. it was
    /// `Queued` or `Running` and is now `Cancelled`), `false` if it was
    /// already in a terminal state and no transition occurred. The
    /// returned bool is used by `RunManager::mark_run_as_cancelled` to
    /// gate duplicate `run_cancelled` SSE broadcasts when the cancel
    /// arrives concurrently from both the HTTP handler and the
    /// `execute_run` terminal arm (issue #1046).
    ///
    /// Idempotent for the run state: `ended_at` is only stamped on the
    /// first transition so it reflects when cancellation actually
    /// happened, not when a later cleanup pass re-marked the run.
    #[must_use = "callers that broadcast a terminal SSE event must gate the broadcast on this bool — discarding it regresses the #1046 duplicate-broadcast guard. If the discard is intentional (e.g. coordinator persistence, test setup), bind to `let _ =` to make it explicit."]
    pub fn mark_cancelled(&mut self) -> bool {
        self.transition(RunTransition::Cancel {
            terminal_reason: "cancelled".to_string(),
        })
        .is_applied()
    }
}

/// Request to create a run.
///
/// Per-run config overrides were removed in the #941 pivot. The agent
/// is now the single per-tenant config surface — operators set
/// model/provider/posture/budgets via `PATCH /agents/{id}` before starting
/// the run. Stale per-run override fields sent by older clients are
/// silently ignored on the wire (no `#[serde(deny_unknown_fields)]`) so
/// in-flight UI bundles on stale builds keep working with no migration.
#[derive(Debug, Serialize, Deserialize)]
pub struct CreateRunRequest {
    pub session_id: SessionId,
    /// Optional agent identity asserted by the caller.
    ///
    /// Normal sessions are owned by exactly one agent and the gateway rejects
    /// mismatches. Shared sessions (DMs) are stored under a nil-agent sentinel,
    /// so callers must provide the active agent id to resolve per-agent config.
    #[serde(default)]
    pub agent_id: Option<AgentId>,
    pub input: RunInput,
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
    pub lifecycle_revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
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
    /// 1-indexed queue position when the run is `Queued` (#831).
    ///
    /// `Some(N)` means there are still N runs ahead of this one in the
    /// per-agent queue (matching the same semantic as
    /// `run_created.queued_behind` and `run_queue_position.position` SSE
    /// events). `None` means the run is not currently queued (running,
    /// completed, failed, cancelled, or queue lookup not available — for
    /// example the bare `From<Run>` conversion which does not know about
    /// the live queue depth).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_position: Option<usize>,
    /// Snapshot of the fully layered config the run committed to at
    /// start-time (#837). `None` when the run is still queued or was
    /// created before the snapshot was wired up (old SQLite rows).
    /// Surfaced here so `GET /runs/{id}` is a one-shot triage source for
    /// "I set model X but Y was used"-class reports without log
    /// correlation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_config: Option<ResolvedRunConfig>,
}

impl From<Run> for RunStatusResponse {
    fn from(run: Run) -> Self {
        Self {
            run_id: run.run_id,
            session_id: run.session_id,
            agent_id: run.agent_id,
            status: run.status,
            lifecycle_revision: run.lifecycle_revision,
            terminal_reason: run.terminal_reason.clone(),
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
            queue_position: None,
            resolved_config: run.resolved_config,
        }
    }
}

/// Trait for registering and updating runs from outside the gateway.
///
/// The Coordinator uses this to make subagent runs visible in the RunManager
/// without depending on `alms-gateway`. The gateway implements this trait
/// on its `RunManager`.
#[async_trait::async_trait]
pub trait RunRegistrar: Send + Sync + std::fmt::Debug {
    /// Register a new run (insert into the run store and persist to SQLite).
    async fn register_run(&self, run: Run) -> crate::AlmsResult<()>;
    /// Update an existing run (e.g. mark as completed/failed).
    fn update_run(&self, run: Run) -> crate::AlmsResult<()>;
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

/// Classify a run's final assistant text as a deliverable DM reply (#1154).
///
/// Under implicit DM replies, the agent's final assistant text **is** the
/// message delivered to its DM peer — no `send_message` call required.
/// This helper decides whether the text is actually deliverable:
///
/// - Empty or whitespace-only text is **not** deliverable (returns `None`).
/// - Text that is byte-identical to the run's reasoning trace is **not**
///   deliverable: that is the `[Thinking]`-only promotion fallback from
///   `finalize_content_and_reasoning` (#1098 detection pattern,
///   `response == reasoning`), where the model emitted no visible content
///   and the runtime promoted the thinking trace as a stand-in answer.
///   Reasoning must never be delivered to a peer as a reply.
/// - Otherwise the trimmed text is the reply.
///
/// Shared between `alms-runtime` (the in-loop empty-reply nudge) and
/// `alms-gateway` (the DM completion gate's delivered/errored decision) so
/// both layers agree on what counts as a reply.
pub fn deliverable_dm_reply<'a>(response: &'a str, reasoning: Option<&str>) -> Option<&'a str> {
    let trimmed = response.trim();
    if trimmed.is_empty() {
        return None;
    }
    if reasoning.is_some_and(|r| r == response) {
        return None;
    }
    Some(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn cancellation_before_start_cannot_be_resurrected() {
        let mut run = Run::new(SessionId::new(), AgentId::new(), "test".into());

        assert!(run.mark_cancelled());
        assert_eq!(run.status, RunStatus::Cancelled);
        assert_eq!(run.lifecycle_revision, 1);

        assert!(!run.mark_running());
        assert_eq!(run.status, RunStatus::Cancelled);
        assert_eq!(run.lifecycle_revision, 1);
    }

    #[test]
    fn duplicate_terminal_transition_is_a_revision_preserving_noop() {
        let mut run = Run::new(SessionId::new(), AgentId::new(), "test".into());
        assert!(run.mark_running());
        assert!(run.mark_failed("boom".into()));
        let ended_at = run.ended_at;
        let revision = run.lifecycle_revision;

        assert!(!run.mark_failed("later".into()));
        assert_eq!(run.status, RunStatus::Failed);
        assert_eq!(run.error.as_deref(), Some("boom"));
        assert_eq!(run.ended_at, ended_at);
        assert_eq!(run.lifecycle_revision, revision);
    }

    #[test]
    fn run_revision_exhaustion_is_rejected_without_mutation() {
        let mut run = Run::new(SessionId::new(), AgentId::new(), "test".into());
        run.lifecycle_revision = MAX_LIFECYCLE_REVISION;

        assert!(matches!(
            run.transition(RunTransition::Start {
                resolved_config: None,
            }),
            TransitionOutcome::Rejected { .. }
        ));
        assert_eq!(run.status, RunStatus::Queued);
        assert!(run.started_at.is_none());
        assert_eq!(run.lifecycle_revision, MAX_LIFECYCLE_REVISION);
    }

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
            tool_invocation_id: None,
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

    /// `Run` with no `resolved_config` (the default for newly-constructed
    /// runs and for old SQLite rows hydrated by callers that have not
    /// migrated yet) must serialize **without** the `resolved_config`
    /// field — not as `"resolved_config": null`. Pinned here so the
    /// `RunStatusResponse` and SSE wire shapes stay backward-compatible
    /// for clients that don't know about #837 yet.
    #[test]
    fn test_run_serializes_without_resolved_config_when_none() {
        let run = Run::new(SessionId::new(), AgentId::new(), "hello".into());
        assert!(run.resolved_config.is_none());
        let value = serde_json::to_value(&run).unwrap();
        let obj = value.as_object().unwrap();
        assert!(
            obj.get("resolved_config").is_none(),
            "resolved_config must be skipped when None — got {value}"
        );
    }

    /// Same wire-compat invariant for `RunStatusResponse`: a queued run
    /// (or any run that never advanced past `Queued`) must surface as
    /// `resolved_config` absent, not `null`. Triage clients learn the
    /// run is missing a snapshot from the field's absence.
    #[test]
    fn test_run_status_response_skips_resolved_config_when_none() {
        let run = Run::new(SessionId::new(), AgentId::new(), "hello".into());
        let resp: RunStatusResponse = run.into();
        assert!(resp.resolved_config.is_none());
        let value = serde_json::to_value(&resp).unwrap();
        let obj = value.as_object().unwrap();
        assert!(
            obj.get("resolved_config").is_none(),
            "resolved_config must be skipped when None"
        );
    }

    /// Symmetric to the `None` case above: a `Run` carrying a populated
    /// `resolved_config` must propagate the snapshot through the
    /// `From<Run> for RunStatusResponse` conversion **and** surface it
    /// on the wire under the field names the issue specifies. Pinned so
    /// the trivial direct field assignment in `From<Run>` doesn't
    /// silently break the `GET /runs/{id}` triage surface — the whole
    /// point of #837 is that operators can read the snapshot off this
    /// endpoint without log correlation.
    #[test]
    fn test_run_status_response_propagates_resolved_config_when_some() {
        let mut run = Run::new(SessionId::new(), AgentId::new(), "hello".into());
        let snapshot = ResolvedRunConfig {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-20250514".into(),
            max_tokens: 4096,
            posture: "guarded".into(),
            debug_mode: false,
            thinking_budget_tokens: 2048,
            reasoning_effort: None,
            gemini_thinking_budget: None,
        };
        assert!(
            run.transition(RunTransition::Start {
                resolved_config: Some(snapshot.clone()),
            })
            .is_applied()
        );

        let resp: RunStatusResponse = run.into();
        assert_eq!(
            resp.resolved_config.as_ref(),
            Some(&snapshot),
            "From<Run> must propagate the populated snapshot verbatim"
        );

        // Wire shape: nested object under `resolved_config` with the
        // issue-specified field names.
        let value = serde_json::to_value(&resp).unwrap();
        let cfg = value
            .get("resolved_config")
            .and_then(|v| v.as_object())
            .expect("resolved_config must be present as an object on the wire");
        assert_eq!(
            cfg.get("provider").and_then(|v| v.as_str()),
            Some("anthropic")
        );
        assert_eq!(
            cfg.get("model").and_then(|v| v.as_str()),
            Some("claude-sonnet-4-20250514")
        );
        assert_eq!(cfg.get("max_tokens").and_then(|v| v.as_u64()), Some(4096));
        assert_eq!(cfg.get("posture").and_then(|v| v.as_str()), Some("guarded"));
        assert_eq!(cfg.get("debug_mode").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(
            cfg.get("thinking_budget_tokens").and_then(|v| v.as_u64()),
            Some(2048)
        );
        // Optional reasoning fields stay absent on the wire when None,
        // matching the per-field `skip_serializing_if` contract pinned
        // by `test_resolved_run_config_serde_roundtrip_minimal`.
        assert!(cfg.get("reasoning_effort").is_none());
        assert!(cfg.get("gemini_thinking_budget").is_none());
    }

    /// A populated `ResolvedRunConfig` round-trips through serde with
    /// the field names the issue specifies, and optional fields stay
    /// absent when `None`.
    #[test]
    fn test_resolved_run_config_serde_roundtrip_minimal() {
        let cfg = ResolvedRunConfig {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-20250514".into(),
            max_tokens: 4096,
            posture: "guarded".into(),
            debug_mode: false,
            thinking_budget_tokens: 0,
            reasoning_effort: None,
            gemini_thinking_budget: None,
        };
        let value = serde_json::to_value(&cfg).unwrap();
        let obj = value.as_object().unwrap();
        // Required fields present with the issue's wire names.
        assert_eq!(
            obj.get("provider").and_then(|v| v.as_str()),
            Some("anthropic")
        );
        assert_eq!(
            obj.get("model").and_then(|v| v.as_str()),
            Some("claude-sonnet-4-20250514")
        );
        assert_eq!(obj.get("max_tokens").and_then(|v| v.as_u64()), Some(4096));
        assert_eq!(obj.get("posture").and_then(|v| v.as_str()), Some("guarded"));
        assert_eq!(obj.get("debug_mode").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(
            obj.get("thinking_budget_tokens").and_then(|v| v.as_u64()),
            Some(0)
        );
        // Optional fields skip-serialize when None — pinned so the wire
        // doesn't gain stray nulls for runs that didn't engage Gemini /
        // OpenAI reasoning.
        assert!(obj.get("reasoning_effort").is_none());
        assert!(obj.get("gemini_thinking_budget").is_none());

        let parsed: ResolvedRunConfig = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, cfg);
    }

    /// Populated optional fields surface on the wire. Pinned so the
    /// triage UI / log scrapers can rely on the field names.
    #[test]
    fn test_resolved_run_config_serde_with_optionals() {
        let cfg = ResolvedRunConfig {
            provider: "openai".into(),
            model: "gpt-5".into(),
            max_tokens: 16_000,
            posture: "autonomous".into(),
            debug_mode: true,
            thinking_budget_tokens: 0,
            reasoning_effort: Some(crate::config::ReasoningEffort::High),
            gemini_thinking_budget: Some(8192),
        };
        let value = serde_json::to_value(&cfg).unwrap();
        assert_eq!(
            value.get("reasoning_effort").and_then(|v| v.as_str()),
            Some("high")
        );
        assert_eq!(
            value.get("gemini_thinking_budget").and_then(|v| v.as_u64()),
            Some(8192)
        );
        let parsed: ResolvedRunConfig = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, cfg);
    }

    /// #1046 regression — `Run::mark_completed` returns `false` and does
    /// NOT mutate the run when the run is already in a terminal state
    /// (Cancelled / Completed / Failed) or is still Queued. Mirrors the
    /// `mark_cancelled` idempotency contract at the gateway level so the
    /// `execute_run` Ok-arm broadcast gate can rely on first-writer-wins
    /// semantics: a near-complete run whose state was already flipped to
    /// `Cancelled` by the HTTP cancel handler must not regress to
    /// `Completed` here and emit a duplicate terminal SSE event.
    #[test]
    fn test_run_mark_completed_is_idempotent_and_returns_transition_bool() {
        let session_id = SessionId::new();
        let agent_id = AgentId::new();

        // Queued → Completed is REJECTED: in production `execute_run`'s
        // Ok arm only fires after `mark_run_as_running_with_config`, so
        // hitting this branch would indicate a lifecycle bug. The
        // contract makes that bug detectable rather than silently
        // committing.
        let mut queued_run = Run::new(session_id, agent_id, "test".into());
        assert!(matches!(queued_run.status, RunStatus::Queued));
        assert!(
            !queued_run.mark_completed("out".into(), TokenUsage::default()),
            "Queued → Completed must report false (lifecycle invariant: \
             mark_completed requires Running)"
        );
        assert!(
            matches!(queued_run.status, RunStatus::Queued),
            "Queued run must remain Queued when mark_completed is rejected"
        );

        // Happy path: Running → Completed transitions and returns true.
        let mut run = Run::new(session_id, agent_id, "test".into());
        run.mark_running();
        assert!(matches!(run.status, RunStatus::Running));
        assert!(
            run.mark_completed("out".into(), TokenUsage::default()),
            "Running → Completed must report transition=true"
        );
        assert!(matches!(run.status, RunStatus::Completed));
        let first_ended_at = run.ended_at.expect("ended_at must be stamped");
        let first_output = run.output.clone();

        // Second call on an already-Completed run reports false and does
        // NOT update ended_at, output, or usage.
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(
            !run.mark_completed("second-write".into(), TokenUsage::default()),
            "second mark_completed on already-Completed run must report false"
        );
        assert_eq!(
            run.ended_at,
            Some(first_ended_at),
            "ended_at must NOT be updated by a no-op second mark_completed"
        );
        assert_eq!(
            run.output, first_output,
            "output must NOT be overwritten by a no-op second mark_completed"
        );

        // Cancelled → mark_completed must NOT overwrite the terminal
        // status. This is the #1046 symmetric-hole regression: a run
        // whose state has been flipped to Cancelled by the HTTP cancel
        // handler must not regress to Completed when execute_run's Ok
        // arm reaches its terminal flip.
        let mut cancelled_run = Run::new(session_id, agent_id, "test".into());
        cancelled_run.mark_running();
        assert!(cancelled_run.mark_cancelled());
        assert!(matches!(cancelled_run.status, RunStatus::Cancelled));
        assert!(
            !cancelled_run.mark_completed("ok".into(), TokenUsage::default()),
            "mark_completed on a Cancelled run must report false (no transition)"
        );
        assert!(
            matches!(cancelled_run.status, RunStatus::Cancelled),
            "mark_completed on a Cancelled run must NOT overwrite the terminal status"
        );

        // Failed → mark_completed must NOT overwrite the terminal status.
        let mut failed_run = Run::new(session_id, agent_id, "test".into());
        failed_run.mark_running();
        let _ = failed_run.mark_failed("boom".into());
        assert!(matches!(failed_run.status, RunStatus::Failed));
        assert!(
            !failed_run.mark_completed("ok".into(), TokenUsage::default()),
            "mark_completed on a Failed run must report false"
        );
        assert!(matches!(failed_run.status, RunStatus::Failed));
    }

    /// #1046 regression — `Run::mark_failed` returns `false` and does
    /// NOT mutate when the run is already in a terminal state, but does
    /// allow `Queued → Failed` for the legitimate setup-error paths
    /// (`resolve_agent_config` / `AgentRuntime::new` failure before
    /// `mark_run_as_running_with_config`). Mirrors `mark_completed`'s
    /// shape so the gateway's three terminal-arm broadcasts share a
    /// single first-writer-wins gate.
    #[test]
    fn test_run_mark_failed_is_idempotent_and_returns_transition_bool() {
        let session_id = SessionId::new();
        let agent_id = AgentId::new();

        // Queued → Failed is ALLOWED — covers the setup-error path
        // (resolve_agent_config / runtime init fails before
        // mark_run_as_running_with_config). Different from
        // `mark_completed`, which rejects Queued because the Ok arm of
        // execute_run is unreachable for a never-started run.
        let mut queued_run = Run::new(session_id, agent_id, "test".into());
        assert!(matches!(queued_run.status, RunStatus::Queued));
        assert!(
            queued_run.mark_failed("setup error".into()),
            "Queued → Failed must report transition=true (setup-error path)"
        );
        assert!(matches!(queued_run.status, RunStatus::Failed));
        assert_eq!(queued_run.error.as_deref(), Some("setup error"));
        assert!(queued_run.ended_at.is_some());

        // Happy path: Running → Failed transitions and returns true.
        let mut run = Run::new(session_id, agent_id, "test".into());
        run.mark_running();
        assert!(
            run.mark_failed("boom".into()),
            "Running → Failed must report transition=true"
        );
        assert!(matches!(run.status, RunStatus::Failed));
        let first_ended_at = run.ended_at.expect("ended_at must be stamped");
        let first_error = run.error.clone();

        // Second call on an already-Failed run reports false and does
        // NOT update ended_at or error.
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(
            !run.mark_failed("second-write".into()),
            "second mark_failed on already-Failed run must report false"
        );
        assert_eq!(
            run.ended_at,
            Some(first_ended_at),
            "ended_at must NOT be updated by a no-op second mark_failed"
        );
        assert_eq!(
            run.error, first_error,
            "error must NOT be overwritten by a no-op second mark_failed"
        );

        // Cancelled → mark_failed must NOT overwrite the terminal status.
        // This is the #1046 symmetric-hole regression on the failure
        // side: a run whose state has been flipped to Cancelled by the
        // HTTP cancel handler must not regress to Failed when
        // execute_run's FailedWithToolCalls / generic Err arm reaches
        // its terminal flip.
        let mut cancelled_run = Run::new(session_id, agent_id, "test".into());
        cancelled_run.mark_running();
        assert!(cancelled_run.mark_cancelled());
        assert!(matches!(cancelled_run.status, RunStatus::Cancelled));
        assert!(
            !cancelled_run.mark_failed("boom".into()),
            "mark_failed on a Cancelled run must report false (no transition)"
        );
        assert!(
            matches!(cancelled_run.status, RunStatus::Cancelled),
            "mark_failed on a Cancelled run must NOT overwrite the terminal status"
        );

        // Completed → mark_failed must NOT overwrite the terminal
        // status either (defense-in-depth; not a path the gateway
        // exercises today, but the invariant should hold).
        let mut completed_run = Run::new(session_id, agent_id, "test".into());
        completed_run.mark_running();
        let _ = completed_run.mark_completed("ok".into(), TokenUsage::default());
        assert!(matches!(completed_run.status, RunStatus::Completed));
        assert!(
            !completed_run.mark_failed("boom".into()),
            "mark_failed on a Completed run must report false"
        );
        assert!(matches!(completed_run.status, RunStatus::Completed));
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

    // ---- deliverable_dm_reply tests (#1154 implicit DM replies) ----

    #[test]
    fn test_deliverable_dm_reply_normal_text() {
        assert_eq!(deliverable_dm_reply("Hello Bob!", None), Some("Hello Bob!"));
    }

    #[test]
    fn test_deliverable_dm_reply_trims_whitespace() {
        assert_eq!(
            deliverable_dm_reply("  Hello Bob!\n", None),
            Some("Hello Bob!")
        );
    }

    #[test]
    fn test_deliverable_dm_reply_empty_is_none() {
        assert_eq!(deliverable_dm_reply("", None), None);
    }

    #[test]
    fn test_deliverable_dm_reply_whitespace_only_is_none() {
        assert_eq!(deliverable_dm_reply("   \n\t ", None), None);
    }

    /// `[Thinking]`-only promotion fallback: when the runtime promoted the
    /// reasoning trace into `response` (`response == reasoning`, the #1098
    /// detection pattern), the text is NOT a deliverable reply.
    #[test]
    fn test_deliverable_dm_reply_promoted_reasoning_is_none() {
        let trace = "I should think about whether to reply...";
        assert_eq!(deliverable_dm_reply(trace, Some(trace)), None);
    }

    /// Distinct reasoning alongside real visible text does not block delivery.
    #[test]
    fn test_deliverable_dm_reply_distinct_reasoning_delivers() {
        assert_eq!(
            deliverable_dm_reply("Hello Bob!", Some("thinking about a greeting")),
            Some("Hello Bob!")
        );
    }
}
