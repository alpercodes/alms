//! Agent registry types — persistent named agents.
//!
//! An `AgentRecord` represents a named, persistent agent registered in the system.
//! Each agent has a name (unique case-insensitively, stored case-preserving —
//! see [`validate_agent_name`]), optional per-agent config overrides, and its
//! own workspace/sessions/jobs.

use crate::{AgentId, AlmsError, AlmsResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Per-agent worktree-isolation mode (#946 — workspace v2).
///
/// Controls how the agent's filesystem-sandbox root is resolved at run-start:
///
/// - [`WorktreeMode::Off`] (default): the agent's sandbox root is the
///   project root. Multiple agents with `Off` share the same project
///   directory and can step on each other's edits if they are running
///   concurrently. This matches the post-#945 default.
/// - [`WorktreeMode::Git`]: at agent-create time the gateway runs
///   `git worktree add <project>/.alms/worktrees/<name>/ -b alms/<name>`
///   and pins the agent's sandbox root at that worktree path instead.
///   Each agent gets its own copy of the working tree on a dedicated
///   branch — multiple agents on the same git project can work in
///   parallel without filesystem collisions. The parent project's
///   `.git/info/exclude` is appended with the worktree path so
///   `git status` on the parent does not show the worktree dir as
///   untracked.
///
/// **Precedence with `[security].allow_full_os_access` (#947):** the
/// security list always wins. An agent listed in
/// `allow_full_os_access` runs without any filesystem sandbox at all
/// (`AgentRuntime::with_unrestricted_filesystem`) regardless of its
/// `worktree_mode`. The worktree itself is still created at
/// agent-create time so the operator can flip the security knob off
/// later without re-running `git worktree add` — only the run-time
/// sandbox attachment is bypassed. A startup-time `WARN` documents
/// the precedence.
///
/// `Git` mode requires the project root to already be a git working
/// tree at agent-create time. Creating an agent with `mode = "git"`
/// on a non-git project returns `400 WORKTREE_REQUIRES_GIT` and the
/// agent record is NOT persisted. There is no silent fallback to
/// project-root mode — an operator who asked for `mode = "git"` gets
/// an explicit error.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorktreeMode {
    /// Default. The agent's sandbox root is the project root.
    #[default]
    Off,
    /// The agent's sandbox root is its own git worktree at
    /// `<project>/.alms/worktrees/<name>/` on branch `alms/<name>`.
    Git,
}

impl WorktreeMode {
    /// Wire string (`"off"` / `"git"`).
    pub fn as_wire_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Git => "git",
        }
    }

    /// Returns `true` when the mode is anything other than `Off`.
    ///
    /// Convenience helper for the gateway run-start path which only
    /// needs to know whether to compute a worktree path or use the
    /// default project-root pin.
    pub fn is_active(self) -> bool {
        !matches!(self, Self::Off)
    }
}

impl std::fmt::Display for WorktreeMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire_str())
    }
}

impl std::str::FromStr for WorktreeMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "git" => Ok(Self::Git),
            other => Err(format!(
                "invalid worktree_mode '{other}' — expected one of: off, git"
            )),
        }
    }
}

/// A persistent agent registered in the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRecord {
    pub id: AgentId,
    pub name: String,
    pub description: String,
    /// Per-agent model override (None = use server default).
    pub model: Option<String>,
    /// Per-agent posture override (None = use server default).
    pub posture: Option<String>,
    /// Per-agent LLM provider override (None = use server default).
    pub provider: Option<String>,
    /// Per-agent Telegram bot token (None = no dedicated Telegram bot).
    ///
    /// When set, the gateway spawns a dedicated polling loop for this agent's
    /// bot and routes all messages from that bot to this agent.
    /// Never serialized in list responses for security -- use `has_telegram`
    /// in the API response instead.
    #[serde(skip_serializing)]
    pub telegram_token: Option<String>,
    /// Per-agent Anthropic extended-thinking budget override.
    ///
    /// `None` = inherit the server default from `[llm.anthropic]`.
    /// `Some(0)` = explicitly disable extended thinking for this agent
    /// even when the server default enables it.
    /// `Some(n > 0)` = use exactly `n` tokens.
    ///
    /// Only applies when the resolved provider maps to the Anthropic wire
    /// protocol; silently ignored for other providers.
    pub thinking_budget_tokens: Option<u32>,
    /// Per-agent OpenAI-compat reasoning-effort override (#768).
    ///
    /// `None` = inherit the server default from `[llm.openai]`.
    /// `Some(effort)` = use exactly this effort level for this agent.
    ///
    /// Only applies when the resolved provider maps to the OpenAI-compatible
    /// wire protocol and the model is a reasoning model (o-series, GPT-5,
    /// xAI Grok reasoning variants). DeepSeek R1 accepts no request-side
    /// param — reasoning fires automatically on `deepseek-reasoner`. For
    /// non-reasoning models (gpt-4o, etc.) the value is silently stripped
    /// from the request body because those models return 400 on unknown
    /// params.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<crate::config::ReasoningEffort>,
    /// Per-agent Gemini extended-thinking budget override (#794).
    ///
    /// `None` = inherit the server default from `[llm.gemini].thinking_budget`.
    /// `Some(0)` = explicitly disable extended thinking for this agent
    /// even when the server default enables it.
    /// `Some(n > 0)` = use exactly `n` tokens.
    ///
    /// Only applies when the resolved provider maps to the Gemini wire
    /// protocol; silently ignored for other providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gemini_thinking_budget: Option<u32>,
    /// Per-agent summary-task provider override (#872, #875).
    ///
    /// Resolution: per-agent ?? server-level `[context].summary_provider`
    /// ?? fall back to the agent's primary LLM (lenient interpretation —
    /// when both per-agent and server-level are `None`, the summary task
    /// silently runs against the agent's own provider/model rather than
    /// being refused).
    /// `Some(provider)` = re-target the per-run summary task at this
    /// provider exclusively, regardless of the server-level setting.
    ///
    /// Must be set together with `summary_model` — asymmetric configs are
    /// rejected at PATCH time with `SUMMARY_PROVIDER_REQUIRES_MODEL` or
    /// `SUMMARY_MODEL_REQUIRES_PROVIDER`. When set, the provider must
    /// reference a configured `[llm.providers.<name>]` entry and have a
    /// resolvable API key, otherwise `SUMMARY_PROVIDER_UNKNOWN` or
    /// `SUMMARY_PROVIDER_MISSING_API_KEY` is returned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_provider: Option<String>,
    /// Per-agent summary-task model override (#872, #875).
    ///
    /// Resolution: per-agent ?? server-level `[context].summary_model` ??
    /// fall back to the agent's primary model (lenient interpretation —
    /// see #875).
    /// `Some(model)` = use this model on the summary provider's wire,
    /// regardless of the server-level setting.
    ///
    /// Must be set together with `summary_provider`. See
    /// [`AgentRecord::summary_provider`] for the validation rules and
    /// the full list of error codes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_model: Option<String>,
    /// Per-agent worktree-isolation mode (#946).
    ///
    /// `Off` (default) → sandbox root is the project root.
    /// `Git` → sandbox root is `<project>/.alms/worktrees/<name>/` on
    /// branch `alms/<name>`. The worktree is created at agent-create
    /// time and removed at agent-delete time. See
    /// [`WorktreeMode`] for the precedence with
    /// `[security].allow_full_os_access`.
    #[serde(default)]
    pub worktree_mode: WorktreeMode,
    /// Per-agent context-window inspection toggle (#1003).
    ///
    /// When `true`, the runtime emits a `ContextDebug` runtime event
    /// after building the context window for each turn. The gateway
    /// converts this to a `context_debug` SSE event so the web UI can
    /// render the assembled message array (system prompt + workspace +
    /// episodic + history + tool definitions, in the order the runtime
    /// sends them). The event is ephemeral — it is not persisted to
    /// the in-memory event log and is not replayed on SSE reconnect.
    ///
    /// PATCH-mutable via `PATCH /agents/{id}` — flipping the flag at
    /// run-time takes effect on the next run without restart. Subagents
    /// do NOT inherit the flag: the coordinator hardcodes
    /// `debug_mode: false` on the resolved config it constructs for each
    /// subagent, and as a belt-and-braces second layer it filters
    /// `RuntimeEvent::ContextDebug` out of the forwarded subagent
    /// event stream. The user-facing rationale is that the parent's
    /// debug panel is already collecting the parent's context window;
    /// surfacing subagent debug panels mid-stream would interleave two
    /// independent context windows in a single chat view with no clean
    /// way to disambiguate them.
    ///
    /// This is a developer / operator inspection knob, not a config
    /// override of any kind — it never affects what the LLM receives,
    /// only what is mirrored to the UI for triage. Default: `false`.
    #[serde(default)]
    pub debug_mode: bool,
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
    pub posture: Option<String>,
    pub provider: Option<String>,
    /// Per-agent Telegram bot token. Validated via `getMe` on gateway startup,
    /// not at persist time.
    pub telegram_token: Option<String>,
    /// Per-agent Anthropic extended-thinking budget override.
    /// See [`AgentRecord::thinking_budget_tokens`] for semantics.
    #[serde(default)]
    pub thinking_budget_tokens: Option<u32>,
    /// Per-agent OpenAI-compat reasoning-effort override (#768).
    /// See [`AgentRecord::reasoning_effort`] for semantics.
    #[serde(default)]
    pub reasoning_effort: Option<crate::config::ReasoningEffort>,
    /// Per-agent Gemini extended-thinking budget override (#794).
    /// See [`AgentRecord::gemini_thinking_budget`] for semantics.
    #[serde(default)]
    pub gemini_thinking_budget: Option<u32>,
    /// Per-agent summary-task provider override (#872).
    /// See [`AgentRecord::summary_provider`] for semantics; must be set
    /// together with `summary_model`.
    #[serde(default)]
    pub summary_provider: Option<String>,
    /// Per-agent summary-task model override (#872).
    /// See [`AgentRecord::summary_model`] for semantics; must be set
    /// together with `summary_provider`.
    #[serde(default)]
    pub summary_model: Option<String>,
    /// Per-agent worktree-isolation mode (#946). `None` is treated as
    /// `WorktreeMode::Off` for back-compat with clients that predate
    /// the field. Setting `Some(WorktreeMode::Git)` requires the
    /// project root to be a git working tree at the moment the
    /// request is processed; non-git projects return `400
    /// WORKTREE_REQUIRES_GIT` and the agent is NOT persisted.
    #[serde(default)]
    pub worktree_mode: Option<WorktreeMode>,
    /// Per-agent debug-mode toggle (#1003). `None` is treated as
    /// `false` for back-compat with clients that predate the field.
    /// See [`AgentRecord::debug_mode`] for semantics.
    #[serde(default)]
    pub debug_mode: Option<bool>,
    #[serde(default)]
    pub is_default: Option<bool>,
}

/// Request body for updating an existing agent.
///
/// All fields are optional — only non-`None` fields are applied.
/// To clear an override on `model`/`posture`/`provider`/`telegram_token`,
/// pass an empty string (handler treats `""` as `None`).
///
/// The three reasoning knobs (`thinking_budget_tokens`, `reasoning_effort`,
/// `gemini_thinking_budget`) have a dedicated `clear_*` boolean sentinel
/// (#809) because they cannot use the empty-string trick — `Some(0)` is a
/// legitimate per-agent override meaning "disable extended thinking for
/// this agent even when the server default enables it". Setting the
/// corresponding `clear_*` flag to `true` resets the stored value back to
/// `None` (inherit server default). It is an error (400 BAD_REQUEST) to
/// send both a `clear_*` flag and a value for the same knob in one
/// request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateAgentRequest {
    pub description: Option<String>,
    pub model: Option<String>,
    pub posture: Option<String>,
    pub provider: Option<String>,
    /// Per-agent Telegram bot token. Empty string = remove token.
    pub telegram_token: Option<String>,
    /// Per-agent Anthropic extended-thinking budget override. A value of
    /// `0` explicitly disables extended thinking for this agent even when
    /// the server default enables it. Omitting the field leaves the
    /// existing value unchanged. Use `clear_thinking_budget_tokens: true`
    /// to reset back to "inherit server default".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget_tokens: Option<u32>,
    /// Per-agent OpenAI-compat reasoning-effort override (#768). Omitting
    /// the field leaves the existing value unchanged. Use
    /// `clear_reasoning_effort: true` to reset back to "inherit server
    /// default".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<crate::config::ReasoningEffort>,
    /// Per-agent Gemini extended-thinking budget override (#794). A value
    /// of `0` explicitly disables extended thinking for this agent even
    /// when the server default enables it. Omitting the field leaves the
    /// existing value unchanged. Use `clear_gemini_thinking_budget: true`
    /// to reset back to "inherit server default".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gemini_thinking_budget: Option<u32>,
    /// When `true`, clear `thinking_budget_tokens` back to `None` (inherit
    /// server default). Mutually exclusive with a non-`None` value for
    /// `thinking_budget_tokens` in the same request — sending both is a
    /// `400 BAD_REQUEST`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clear_thinking_budget_tokens: Option<bool>,
    /// When `true`, clear `reasoning_effort` back to `None` (inherit
    /// server default). Mutually exclusive with a non-`None` value for
    /// `reasoning_effort` in the same request — sending both is a `400
    /// BAD_REQUEST`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clear_reasoning_effort: Option<bool>,
    /// When `true`, clear `gemini_thinking_budget` back to `None` (inherit
    /// server default). Mutually exclusive with a non-`None` value for
    /// `gemini_thinking_budget` in the same request — sending both is a
    /// `400 BAD_REQUEST`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clear_gemini_thinking_budget: Option<bool>,
    /// Per-agent summary-task provider override (#872). Empty string
    /// `""` is rejected — use `clear_summary_provider: true` to clear,
    /// or send a non-empty provider name. Must be set together with
    /// `summary_model` (the symmetric pair-only validator runs at
    /// post-PATCH state).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_provider: Option<String>,
    /// Per-agent summary-task model override (#872). Empty string `""`
    /// is rejected — use `clear_summary_model: true` to clear. Must be
    /// set together with `summary_provider`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_model: Option<String>,
    /// When `true`, clear `summary_provider` back to `None` (fall
    /// through to the server-level `[context].summary_provider`).
    /// Mutually exclusive with a non-`None` value for
    /// `summary_provider` in the same request — sending both is a
    /// `400 CLEAR_AND_VALUE_CONFLICT`. Must be sent together with
    /// `clear_summary_model: true` (or with both summary_* fields
    /// staying None) so the post-PATCH state respects the pair-only
    /// invariant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clear_summary_provider: Option<bool>,
    /// When `true`, clear `summary_model` back to `None` (fall through
    /// to the server-level `[context].summary_model`). Mutually
    /// exclusive with a non-`None` value for `summary_model` in the
    /// same request — sending both is a `400 CLEAR_AND_VALUE_CONFLICT`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clear_summary_model: Option<bool>,
    /// Per-agent worktree-isolation mode (#946). `Some(mode)` flips the
    /// stored value; omit the field to leave the existing setting
    /// unchanged. Flipping `Off → Git` triggers a `git worktree add`
    /// at PATCH time; `Git → Off` triggers `git worktree remove`. The
    /// remove path refuses if the worktree has uncommitted changes —
    /// pass `force_worktree_remove: true` to override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_mode: Option<WorktreeMode>,
    /// When `true`, force the `Git → Off` flip even if the worktree
    /// has uncommitted changes. Has no effect when `worktree_mode` is
    /// not `Some(WorktreeMode::Off)` or when the agent's stored mode
    /// is already `Off`. The forced removal discards the worktree
    /// contents AND deletes the `alms/<name>` branch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub force_worktree_remove: Option<bool>,
    /// Per-agent debug-mode toggle (#1003). `Some(true)` enables the
    /// context-window inspection panel for this agent on the next run;
    /// `Some(false)` disables it. Omitting the field leaves the
    /// existing value unchanged. See [`AgentRecord::debug_mode`] for
    /// semantics. Empty-string sentinel and `clear_*` boolean are
    /// intentionally not offered — `false` is itself the cleared /
    /// default state, so the wire shape is the value field alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug_mode: Option<bool>,
}

/// Agent names that collide with API sub-route segments or internal
/// `context_id` prefixes, and are therefore refused by
/// [`validate_agent_name`]. Matched case-insensitively.
///
/// Mirrored client-side by `RESERVED_NAMES` in
/// `crates/alms-gateway/static/ui/utils/agent-name.js` — keep the two lists
/// in lock-step.
pub const RESERVED_AGENT_NAMES: &[&str] = &["default", "dm", "workspace"];

/// Validate an agent name.
///
/// Rules: 1–64 chars, ASCII alphanumeric + hyphens, no leading/trailing
/// hyphens. Must not be a reserved name, and must not be a valid UUID (would
/// collide with UUID-first `resolve_agent` lookup).
///
/// # Case
///
/// Both cases are admissible (`Atlas` is as valid as `atlas`), and the
/// operator's casing is preserved verbatim for display and storage. What is
/// *not* admissible is two agents whose names differ only in case: agent
/// workspaces are directories at `{workspace_dir}/{name}/`, and Windows /
/// macOS filesystems are case-insensitive while Linux is not, so `Atlas` and
/// `atlas` would share one directory on two platforms and split across two on
/// the third. Uniqueness is therefore enforced **case-insensitively**, by a
/// `UNIQUE INDEX ... (name COLLATE NOCASE)` in the agents schema, and every
/// name lookup (`load_agent_by_name`, and so the HTTP routes, the CLI,
/// `invoke_agent` and `send_message` that funnel through it) matches
/// case-insensitively too. This function is per-name only — it cannot see the
/// registry — so it does not and cannot enforce that part.
///
/// # This is also an output constraint
///
/// The character class is deliberately narrow because this validator gates
/// LLM-supplied names on their way to the DOM — see the "output constraint"
/// section on [`crate::parse_subagent_context`]. `[A-Za-z0-9-]` still admits
/// no `<`, `&`, quotes, whitespace, or path separators. Widen the class only
/// with that property in mind.
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

    // Only ASCII alphanumeric + hyphens. Uppercase is admissible; anything
    // outside the class is not (see the output-constraint note above).
    for ch in name.chars() {
        if !ch.is_ascii_alphanumeric() && ch != '-' {
            return Err(AlmsError::InvalidConfig(format!(
                "agent name must contain only ASCII letters, digits, and hyphens, got '{ch}' in '{name}'"
            )));
        }
    }

    // Reserved names that collide with API sub-route segments or internal
    // prefixes. Matched case-insensitively: those route segments and prefixes
    // are matched case-insensitively downstream, so an exact-match guard here
    // would let `DM` walk straight through it.
    if RESERVED_AGENT_NAMES
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(name))
    {
        return Err(AlmsError::InvalidConfig(format!(
            "agent name '{name}' is reserved"
        )));
    }

    // Reject names that parse as UUIDs — resolve_agent uses UUID-first lookup,
    // so a UUID-shaped name would be unreachable by name.
    if uuid::Uuid::parse_str(name).is_ok() {
        return Err(AlmsError::InvalidConfig(format!(
            "agent name '{name}' looks like a UUID (conflicts with ID-based lookup)"
        )));
    }

    Ok(())
}

/// Workspace file names created for every new agent.
pub const WORKSPACE_FILENAMES: &[&str] = &["personality.md", "goals.md", "memories.md", "user.md"];

/// Migrate workspace directories from UUID-based to name-based paths.
///
/// For each `(uuid, name)` pair, renames `{workspace_dir}/{uuid}/` to
/// `{workspace_dir}/{name}/` if the UUID directory exists and the name
/// directory does not. Idempotent and non-destructive — skips if both
/// exist or neither exists.
pub fn migrate_workspace_dirs(
    workspace_dir: &Path,
    agents: &[(uuid::Uuid, String)],
) -> std::io::Result<usize> {
    let mut migrated = 0;
    for (uuid, name) in agents {
        let uuid_dir = workspace_dir.join(uuid.to_string());
        let name_dir = workspace_dir.join(name);
        if uuid_dir.is_dir() && !name_dir.exists() {
            std::fs::rename(&uuid_dir, &name_dir)?;
            migrated += 1;
            tracing::debug!(
                agent_name = %name,
                from = %uuid_dir.display(),
                to = %name_dir.display(),
                "Migrated workspace directory from UUID to name-based path"
            );
        } else if uuid_dir.is_dir() && name_dir.exists() {
            tracing::warn!(
                agent_name = %name,
                uuid_dir = %uuid_dir.display(),
                name_dir = %name_dir.display(),
                "Both UUID and name workspace directories exist — skipping migration; \
                 orphaned UUID directory may need manual cleanup"
            );
        }
    }
    Ok(migrated)
}

/// Create empty workspace files in a directory.
///
/// Skips files that already exist so the function is idempotent.
/// The directory is created if it doesn't exist.
pub fn init_workspace_files(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for filename in WORKSPACE_FILENAMES {
        let path = dir.join(filename);
        if !path.exists() {
            std::fs::write(&path, "")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_names() {
        assert!(validate_agent_name("atlas").is_ok());
        assert!(validate_agent_name("my-agent-1").is_ok());
        assert!(validate_agent_name("a").is_ok());
        assert!(validate_agent_name("agent-v2").is_ok());
        assert!(validate_agent_name("a1b2c3").is_ok());
    }

    /// Uppercase is admissible (#2). The operator's casing is preserved
    /// verbatim — this validator never rewrites the name it is handed.
    #[test]
    fn uppercase_names_are_accepted() {
        for name in ["Atlas", "ATLAS", "ResearchBot", "Agent-V2", "A"] {
            assert!(
                validate_agent_name(name).is_ok(),
                "expected {name} to be accepted"
            );
        }
    }

    #[test]
    fn test_reserved_names() {
        let err = validate_agent_name("default").unwrap_err();
        assert!(err.to_string().contains("reserved"));
        let err = validate_agent_name("dm").unwrap_err();
        assert!(err.to_string().contains("reserved"));
        let err = validate_agent_name("workspace").unwrap_err();
        assert!(err.to_string().contains("reserved"));
    }

    /// #2: once uppercase is admissible, an exact-match reserved-name guard
    /// is a bypass — `DM` would sail through it and land on the very
    /// `dm:`-prefixed context space the guard exists to protect.
    #[test]
    fn reserved_names_are_matched_case_insensitively() {
        for name in [
            "DM",
            "Dm",
            "dM",
            "Default",
            "DEFAULT",
            "Workspace",
            "WorkSpace",
            "WORKSPACE",
        ] {
            let err = validate_agent_name(name).unwrap_err().to_string();
            assert!(
                err.contains("reserved"),
                "expected {name} to be rejected as reserved, got: {err}"
            );
        }
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

    /// The class widened to `[A-Za-z0-9-]` (#2), not to "anything". Non-ASCII
    /// letters stay out: this validator is also the gate on LLM-supplied
    /// `invoke_agent` names before they are rendered as UI labels, and on the
    /// name used as a workspace directory component.
    #[test]
    fn test_invalid_non_ascii_letters() {
        assert!(validate_agent_name("café").is_err());
        assert!(validate_agent_name("Ünicode").is_err());
        assert!(validate_agent_name("αβγ").is_err());
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

    #[test]
    fn test_uuid_shaped_name_rejected() {
        let err = validate_agent_name("a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap_err();
        assert!(err.to_string().contains("UUID"));
        // Also test a v4-style UUID
        let err = validate_agent_name("550e8400-e29b-41d4-a716-446655440000").unwrap_err();
        assert!(err.to_string().contains("UUID"));
        // `Uuid::parse_str` also accepts the simple 32-char hex form —
        // make sure the validator covers that shape too (Tim's PR #999
        // re-review item; the client-side mirror missed this case).
        let err = validate_agent_name("a1b2c3d4e5f67890abcdef1234567890").unwrap_err();
        assert!(err.to_string().contains("UUID"));
    }

    /// #2: `Uuid::parse_str` is itself case-insensitive over the hex digits,
    /// so admitting uppercase letters does not open a UUID-shaped hole in the
    /// name space. Pins that the widened class did not shift the boundary the
    /// named/ephemeral subagent discriminator leans on.
    #[test]
    fn uppercase_uuid_shaped_names_are_still_rejected() {
        for name in [
            "A1B2C3D4-E5F6-7890-ABCD-EF1234567890",
            "550E8400-E29B-41D4-A716-446655440000",
            "A1B2C3D4E5F67890ABCDEF1234567890",
        ] {
            let err = validate_agent_name(name).unwrap_err().to_string();
            assert!(err.contains("UUID"), "expected {name} rejected, got: {err}");
        }
    }

    #[test]
    fn test_migrate_workspace_dirs_renames_uuid_to_name() {
        let tmp = std::env::temp_dir().join(format!("alms-migrate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();

        let uuid = uuid::Uuid::new_v4();
        let uuid_dir = tmp.join(uuid.to_string());
        init_workspace_files(&uuid_dir).unwrap();
        std::fs::write(uuid_dir.join("personality.md"), "hello").unwrap();

        let migrated = migrate_workspace_dirs(&tmp, &[(uuid, "atlas".to_string())]).unwrap();
        assert_eq!(migrated, 1);
        assert!(!uuid_dir.exists(), "UUID dir should be gone");
        assert!(
            tmp.join("atlas").join("personality.md").exists(),
            "name dir should have files"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.join("atlas").join("personality.md")).unwrap(),
            "hello"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_migrate_workspace_dirs_skips_when_name_exists() {
        let tmp = std::env::temp_dir().join(format!("alms-migrate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();

        let uuid = uuid::Uuid::new_v4();
        let uuid_dir = tmp.join(uuid.to_string());
        let name_dir = tmp.join("atlas");
        init_workspace_files(&uuid_dir).unwrap();
        init_workspace_files(&name_dir).unwrap();

        // Both exist — migration should skip (no overwrite)
        let migrated = migrate_workspace_dirs(&tmp, &[(uuid, "atlas".to_string())]).unwrap();
        assert_eq!(migrated, 0);
        assert!(uuid_dir.exists(), "UUID dir should still exist");
        assert!(name_dir.exists(), "name dir should still exist");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_migrate_workspace_dirs_idempotent() {
        let tmp = std::env::temp_dir().join(format!("alms-migrate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();

        let uuid = uuid::Uuid::new_v4();
        let uuid_dir = tmp.join(uuid.to_string());
        init_workspace_files(&uuid_dir).unwrap();

        let agents = vec![(uuid, "atlas".to_string())];
        let m1 = migrate_workspace_dirs(&tmp, &agents).unwrap();
        assert_eq!(m1, 1);
        // Second call — UUID dir is gone, name dir exists, should return 0
        let m2 = migrate_workspace_dirs(&tmp, &agents).unwrap();
        assert_eq!(m2, 0);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── #946: WorktreeMode enum tests ──────────────────────────────

    #[test]
    fn test_worktree_mode_default_is_off() {
        assert_eq!(WorktreeMode::default(), WorktreeMode::Off);
    }

    #[test]
    fn test_worktree_mode_wire_strings() {
        assert_eq!(WorktreeMode::Off.as_wire_str(), "off");
        assert_eq!(WorktreeMode::Git.as_wire_str(), "git");
    }

    #[test]
    fn test_worktree_mode_is_active() {
        assert!(!WorktreeMode::Off.is_active());
        assert!(WorktreeMode::Git.is_active());
    }

    #[test]
    fn test_worktree_mode_from_str() {
        use std::str::FromStr;
        assert_eq!(WorktreeMode::from_str("off").unwrap(), WorktreeMode::Off);
        assert_eq!(WorktreeMode::from_str("git").unwrap(), WorktreeMode::Git);
        assert_eq!(WorktreeMode::from_str("OFF").unwrap(), WorktreeMode::Off);
        assert_eq!(WorktreeMode::from_str("GIT").unwrap(), WorktreeMode::Git);
        assert!(WorktreeMode::from_str("xdg").is_err());
        assert!(WorktreeMode::from_str("").is_err());
    }

    #[test]
    fn test_worktree_mode_serde_lowercase() {
        // Round-trip JSON serialization. Wire format is lowercase
        // strings to match what the CLI accepts via `--worktree-mode`.
        let json = serde_json::to_string(&WorktreeMode::Git).unwrap();
        assert_eq!(json, r#""git""#);
        let parsed: WorktreeMode = serde_json::from_str(r#""off""#).unwrap();
        assert_eq!(parsed, WorktreeMode::Off);
    }

    #[test]
    fn test_init_workspace_files_creates_all() {
        let tmp = std::env::temp_dir().join(format!("alms-test-{}", uuid::Uuid::new_v4()));
        let dir = tmp.join("my-agent");
        init_workspace_files(&dir).unwrap();
        for filename in WORKSPACE_FILENAMES {
            assert!(dir.join(filename).exists(), "{filename} should exist");
        }
        // Idempotent — writing content then calling again should not overwrite
        std::fs::write(dir.join("goals.md"), "Keep this").unwrap();
        init_workspace_files(&dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("goals.md")).unwrap(),
            "Keep this"
        );
        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
