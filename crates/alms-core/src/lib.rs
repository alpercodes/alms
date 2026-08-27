pub mod audit;
pub mod channel;
pub mod config;
pub mod error;
pub mod job;
pub mod lifecycle;
pub mod registry;
pub mod run;
pub mod secrets;
pub mod source_label;
/// Interest-cache-safe `tracing` capture harness for this crate's
/// log-asserting unit tests — see the module docs and #1221.
#[cfg(test)]
pub(crate) mod test_log_capture;
pub mod worktree;

pub use channel::{Channel, ChannelConfig, IncomingMessage, OutgoingMessage};

pub use audit::{AuditDecision, AuditEvent};
pub use config::AlmsConfig;
pub use error::{AlmsError, AlmsResult, audit_error_string, sanitize_error_for_session};
pub use job::{
    CreateJobRequest, Job, JobId, JobSchedule, JobStatus, JobTerminalReason, JobTransition,
};
pub use lifecycle::{MAX_LIFECYCLE_REVISION, TransitionOutcome};
pub use registry::{
    AgentRecord, CreateAgentRequest, UpdateAgentRequest, WORKSPACE_FILENAMES, WorktreeMode,
    init_workspace_files, migrate_workspace_dirs, validate_agent_name,
};
pub use run::{
    CreateRunRequest, CreateRunResponse, ResolvedRunConfig, Run, RunId, RunInput, RunRegistrar,
    RunStatus, RunStatusResponse, RunTransition, TokenUsage, ToolCallRecord, ToolCallRole,
    deliverable_dm_reply, ran_ignore_message_successfully,
};
pub use source_label::{derive_source_label, truncate_to_char_boundary};

/// Classify a session's type from its `context_id`.
///
/// Returns a string suitable for the `session_type` field in the session
/// list API response and the `context_type` field in the `list_my_sessions`
/// tool output.
///
/// This is the **single source of truth** for context-ID classification --
/// all callers (gateway session list, tool output, etc.) should use this
/// function rather than maintaining their own prefix checks.
///
/// Mapping:
///
/// - `"dm:{a}:{b}"` -> `"dm"`
/// - `"notifications:{agent}"` -> `"notification"`
/// - `"job_{id}"` -> `"job"`
/// - `"subagent_{parent_agent_id}_{name|task_id}"` -> `"subagent"`
/// - `"episodic:{id}"` -> `"episodic"`
/// - `"telegram_{id}"` -> `"telegram"`
/// - anything else -> `"chat"`
pub fn classify_session_type(context_id: &str) -> &'static str {
    if context_id.starts_with("dm:") {
        "dm"
    } else if context_id.starts_with("notifications:") {
        "notification"
    } else if context_id.starts_with("job_") {
        "job"
    } else if context_id.starts_with("subagent_") {
        "subagent"
    } else if context_id.starts_with("episodic:") {
        "episodic"
    } else if context_id.starts_with("telegram_") {
        "telegram"
    } else {
        "chat"
    }
}

/// The owner encoded in a coordinator-reserved subagent `context_id`.
///
/// A subagent session is NOT stored under the subagent's registry id — a
/// named one is keyed on `AgentId::deterministic(parent_agent_id, name)`
/// (#1051) and an ephemeral one on a fresh `AgentId::new()`. Neither
/// resolves against the agent registry, so the `context_id` is the only
/// place the display identity survives. See [`parse_subagent_context`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentOwner<'a> {
    /// `subagent_{parent_agent_id}_{name}` — a named subagent. The name is
    /// `validate_agent_name`-valid: `invoke_agent` rejects the call before
    /// dispatch otherwise.
    Named(&'a str),
    /// `subagent_{parent_agent_id}_{task_id}` — an unnamed, one-shot
    /// subagent. There is no name to recover; the task id is deliberately
    /// NOT surfaced as one.
    Ephemeral,
}

/// Recover the display owner of a subagent session from its `context_id`.
///
/// Both shapes `derive_subagent_identity` produces are structurally
/// identical — `subagent_{parent_agent_id}_{trailing}` — so the
/// named/ephemeral split is decided by what `trailing` IS, never by its
/// position. Taking the trailing segment unconditionally would render an
/// ephemeral subagent's task id as if it were an agent name (#1277).
///
/// The two trailing forms are disjoint by construction:
/// `validate_agent_name` rejects any name that parses as a UUID, precisely
/// so names can't collide with id-shaped lookups. The UUID test still runs
/// FIRST so the discrimination doesn't silently depend on that rule holding
/// — if agent names were ever relaxed to admit UUID shapes, an ephemeral
/// task id would still classify as ephemeral rather than leak as a name.
///
/// Returns `None` for anything that isn't one of the two current shapes —
/// a non-subagent context, the legacy pre-#1185 `subagent_{task_id}` form
/// (no parent segment), a non-UUID parent segment, or a trailing segment
/// that is neither a UUID nor a valid agent name. Callers must treat `None`
/// as "unknown owner" and display nothing: guessing is what turned a
/// resolution miss into a confident mislabel in the first place.
///
/// # The `validate_agent_name` gate is also an output constraint
///
/// The name returned by [`SubagentOwner::Named`] originates in an
/// LLM-supplied `invoke_agent` parameter and ends up rendered as an
/// identity label in the web UI. Gating the named arm on
/// `validate_agent_name` therefore does double duty: it is the
/// named/ephemeral discriminator, AND it constrains what can reach the DOM
/// to `[a-z0-9-]`, 1–64 chars. A "just take the trailing segment" parse
/// would have handed arbitrary model-controlled text to a label. Keep the
/// gate even if the discrimination is ever reworked.
pub fn parse_subagent_context(context_id: &str) -> Option<SubagentOwner<'_>> {
    let rest = context_id.strip_prefix("subagent_")?;

    // Neither a UUID nor an agent name contains '_', so the first '_' is
    // unambiguously the parent/trailing separator.
    let (parent, trailing) = rest.split_once('_')?;
    Uuid::parse_str(parent).ok()?;

    if Uuid::parse_str(trailing).is_ok() {
        Some(SubagentOwner::Ephemeral)
    } else if validate_agent_name(trailing).is_ok() {
        Some(SubagentOwner::Named(trailing))
    } else {
        None
    }
}

/// Error message returned to the LLM when both `send_message` and
/// `ignore_message` appear in the same tool-call batch (DM conflict).
///
/// Defined in `alms-core` so that both `alms-runtime` (conflict detection in
/// the agent loop) and `alms-gateway` (ignore-message detection in `execute_run`)
/// can reference the same sentinel string.
pub const DM_CONFLICT_MSG: &str = "send_message and ignore_message are mutually exclusive \
     — you can only use one per turn. Choose one.";

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for agents
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(pub Uuid);

/// ALMS namespace UUID for deterministic v5 derivation.
///
/// **STABILITY: Do not change this value.** All persisted subagent session IDs
/// and agent-to-agent DM session IDs are derived from this namespace via UUID v5.
/// Changing it would invalidate all existing deterministic IDs in the database.
const ALMS_NAMESPACE: Uuid = Uuid::from_bytes([
    0xa1, 0xb2, 0xc3, 0xd4, 0xe5, 0xf6, 0x47, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89,
]);

impl AgentId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn nil() -> Self {
        Self(Uuid::nil())
    }

    pub fn is_nil(&self) -> bool {
        self.0 == Uuid::nil()
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

    /// Derive a deterministic SessionId from an arbitrary context string.
    ///
    /// Same input always produces the same SessionId (UUID v5). Use this
    /// for notification sessions, named sessions, or any case where a
    /// stable identity is needed from a string key.
    ///
    /// [`deterministic_dm`](Self::deterministic_dm) is a convenience wrapper
    /// that sorts agent names and delegates here with a `"dm:{a}:{b}"` key.
    pub fn deterministic(context: &str) -> Self {
        Self(Uuid::new_v5(&ALMS_NAMESPACE, context.as_bytes()))
    }

    /// Derive a deterministic SessionId for a DM conversation between two agents.
    ///
    /// Names are sorted alphabetically so both sides resolve to the same
    /// SessionId regardless of who initiated the conversation (UUID v5).
    ///
    /// This is a convenience wrapper around [`deterministic`](Self::deterministic)
    /// with the key `"dm:{sorted_a}:{sorted_b}"` -- see [`dm_context_id`].
    pub fn deterministic_dm(a: &str, b: &str) -> Self {
        Self::deterministic(&dm_context_id(a, b))
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Derive the DM context_id for a pair of agents.
///
/// Names are sorted alphabetically so both sides resolve to the same
/// context_id regardless of who initiated the conversation.
///
/// ```
/// # use alms_core::dm_context_id;
/// assert_eq!(dm_context_id("alice", "bob"), "dm:alice:bob");
/// assert_eq!(dm_context_id("bob", "alice"), "dm:alice:bob");
/// ```
pub fn dm_context_id(a: &str, b: &str) -> String {
    let (first, second) = if a < b { (a, b) } else { (b, a) };
    format!("dm:{first}:{second}")
}

/// Parse a DM `context_id` into its two participant names.
///
/// DM context IDs have the form `"dm:{name1}:{name2}"` (alphabetically sorted).
/// Returns `None` if the string does not match the expected format.
///
/// ```
/// # use alms_core::dm_participants;
/// assert_eq!(dm_participants("dm:alice:bob"), Some(("alice", "bob")));
/// assert_eq!(dm_participants("web-chat-123"), None);
/// assert_eq!(dm_participants("dm:alice"), None);
/// ```
pub fn dm_participants(context_id: &str) -> Option<(&str, &str)> {
    let rest = context_id.strip_prefix("dm:")?;
    rest.split_once(':')
}

/// Extract the peer agent name from a DM `context_id`.
///
/// DM context IDs have the form `"dm:{name1}:{name2}"` (alphabetically sorted).
/// The peer is whichever name is NOT `agent_name`.  Returns `None` if the
/// context ID does not match the expected format or neither name matches
/// `agent_name`.
///
/// ```
/// # use alms_core::dm_peer;
/// assert_eq!(dm_peer("dm:alice:bob", "alice"), Some("bob"));
/// assert_eq!(dm_peer("dm:alice:bob", "bob"), Some("alice"));
/// assert_eq!(dm_peer("dm:alice:bob", "charlie"), None);
/// assert_eq!(dm_peer("web-chat-123", "alice"), None);
/// ```
pub fn dm_peer<'a>(context_id: &'a str, agent_name: &str) -> Option<&'a str> {
    let (a, b) = dm_participants(context_id)?;
    if a == agent_name {
        Some(b)
    } else if b == agent_name {
        Some(a)
    } else {
        None
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

/// Build the default environment variables for shell_exec subprocesses.
///
/// This injects `ALMS_DATA_DIR` and `ALMS_WORKSPACE_DIR` so that CLI commands
/// invoked by agents (via `shell_exec`) find the correct database and workspace
/// regardless of the sandbox cwd.
///
/// Paths are resolved to absolute form so that subprocesses running in a
/// different working directory (e.g. an agent's workspace) don't interpret
/// them relative to their own cwd and create stray `data/` directories.
///
/// Used by the HTTP run path, the Telegram message path, and the coordinator's
/// subagent spawn path to avoid duplicating the same env-building logic.
pub fn build_shell_default_env(
    data_dir: Option<&std::path::Path>,
    workspace_dir: Option<&std::path::Path>,
) -> std::collections::HashMap<String, String> {
    let mut env = std::collections::HashMap::new();
    if let Some(dd) = data_dir {
        let abs = resolve_to_absolute(dd);
        env.insert("ALMS_DATA_DIR".to_string(), abs);
    }
    if let Some(ws) = workspace_dir {
        let abs = resolve_to_absolute(ws);
        env.insert("ALMS_WORKSPACE_DIR".to_string(), abs);
    }
    env
}

/// Resolve a path to an absolute string.
///
/// Tries `std::fs::canonicalize()` first (follows symlinks, adds UNC prefix on
/// Windows). Falls back to joining with `current_dir()` if the path doesn't
/// exist yet. Returns the path as-is as a last resort.
pub fn resolve_to_absolute(path: &std::path::Path) -> String {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        let s = canonical.to_string_lossy().into_owned();
        // On Windows, `canonicalize` returns extended-length paths (`\\?\C:\...`).
        // This prefix requires pure backslash separators and breaks code that
        // does string-based path construction with `/`.  Strip it when the
        // remaining path is a normal absolute path (e.g. `C:\...`).
        #[cfg(windows)]
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            // Only strip when what follows is a drive-letter path (e.g. "C:\…"),
            // so we don't accidentally mangle UNC paths like `\\?\UNC\…`.
            if stripped.len() >= 3 && stripped.as_bytes()[1] == b':' {
                return stripped.to_owned();
            }
        }
        return s;
    }
    // Path may not exist yet — make it absolute via current_dir + join.
    if !path.is_absolute()
        && let Ok(cwd) = std::env::current_dir()
    {
        return cwd.join(path).to_string_lossy().into_owned();
    }
    path.to_string_lossy().into_owned()
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

    #[test]
    fn test_dm_context_id_sorted() {
        assert_eq!(dm_context_id("alice", "bob"), "dm:alice:bob");
        assert_eq!(dm_context_id("bob", "alice"), "dm:alice:bob");
        assert_eq!(dm_context_id("zeta", "alpha"), "dm:alpha:zeta");
    }

    #[test]
    fn test_dm_context_id_symmetric() {
        let a = "developer";
        let b = "reviewer";
        assert_eq!(dm_context_id(a, b), dm_context_id(b, a));
    }

    // -- dm_participants -------------------------------------------------------

    #[test]
    fn test_dm_participants_valid() {
        assert_eq!(dm_participants("dm:alice:bob"), Some(("alice", "bob")));
    }

    #[test]
    fn test_dm_participants_non_dm() {
        assert_eq!(dm_participants("web-chat-123"), None);
    }

    #[test]
    fn test_dm_participants_malformed_no_second_colon() {
        assert_eq!(dm_participants("dm:alice"), None);
    }

    #[test]
    fn test_dm_participants_empty() {
        assert_eq!(dm_participants(""), None);
    }

    // -- dm_peer ---------------------------------------------------------------

    #[test]
    fn test_dm_peer_first_name() {
        assert_eq!(dm_peer("dm:alice:bob", "alice"), Some("bob"));
    }

    #[test]
    fn test_dm_peer_second_name() {
        assert_eq!(dm_peer("dm:alice:bob", "bob"), Some("alice"));
    }

    #[test]
    fn test_dm_peer_not_participant() {
        assert_eq!(dm_peer("dm:alice:bob", "charlie"), None);
    }

    #[test]
    fn test_dm_peer_non_dm_context() {
        assert_eq!(dm_peer("web-chat-123", "alice"), None);
    }

    #[test]
    fn test_dm_peer_malformed() {
        assert_eq!(dm_peer("dm:alice", "alice"), None);
    }

    #[test]
    fn test_dm_peer_empty() {
        assert_eq!(dm_peer("", "alice"), None);
    }

    #[test]
    fn test_session_id_deterministic_dm_stable() {
        let a = SessionId::deterministic_dm("alice", "bob");
        let b = SessionId::deterministic_dm("alice", "bob");
        assert_eq!(a, b, "same inputs must produce same SessionId");
    }

    #[test]
    fn test_session_id_deterministic_dm_symmetric() {
        let a = SessionId::deterministic_dm("alice", "bob");
        let b = SessionId::deterministic_dm("bob", "alice");
        assert_eq!(a, b, "reversed names must produce same SessionId");
    }

    #[test]
    fn test_session_id_deterministic_dm_differs_by_pair() {
        let ab = SessionId::deterministic_dm("alice", "bob");
        let ac = SessionId::deterministic_dm("alice", "charlie");
        assert_ne!(ab, ac, "different pairs must produce different SessionIds");
    }

    #[test]
    fn test_build_shell_default_env_both() {
        // Use a real existing directory so canonicalize works cross-platform.
        let tmp = std::env::temp_dir();
        let data = tmp.join("alms_test_data");
        let ws = tmp.join("alms_test_ws");
        let _ = std::fs::create_dir_all(&data);
        let _ = std::fs::create_dir_all(&ws);

        let env = build_shell_default_env(Some(&data), Some(&ws));
        let result_data = env.get("ALMS_DATA_DIR").unwrap();
        let result_ws = env.get("ALMS_WORKSPACE_DIR").unwrap();
        // Values must be absolute paths.
        assert!(
            std::path::Path::new(result_data).is_absolute(),
            "ALMS_DATA_DIR should be absolute, got: {result_data}"
        );
        assert!(
            std::path::Path::new(result_ws).is_absolute(),
            "ALMS_WORKSPACE_DIR should be absolute, got: {result_ws}"
        );

        let _ = std::fs::remove_dir(&data);
        let _ = std::fs::remove_dir(&ws);
    }

    #[test]
    fn test_build_shell_default_env_none() {
        let env = build_shell_default_env(None, None);
        assert!(env.is_empty());
    }

    #[test]
    fn test_build_shell_default_env_data_only() {
        let tmp = std::env::temp_dir();
        let data = tmp.join("alms_test_data_only");
        let _ = std::fs::create_dir_all(&data);

        let env = build_shell_default_env(Some(&data), None);
        assert_eq!(env.len(), 1);
        assert!(
            std::path::Path::new(env.get("ALMS_DATA_DIR").unwrap()).is_absolute(),
            "ALMS_DATA_DIR should be absolute"
        );
        assert!(!env.contains_key("ALMS_WORKSPACE_DIR"));

        let _ = std::fs::remove_dir(&data);
    }

    #[test]
    fn test_build_shell_default_env_relative_path_becomes_absolute() {
        // A relative path should be resolved to absolute even if it doesn't exist.
        let env = build_shell_default_env(Some(std::path::Path::new("relative/data/dir")), None);
        let result = env.get("ALMS_DATA_DIR").unwrap();
        assert!(
            std::path::Path::new(result).is_absolute(),
            "Relative data_dir should be resolved to absolute, got: {result}"
        );
    }

    #[test]
    fn test_resolve_to_absolute_existing_dir() {
        let tmp = std::env::temp_dir();
        let result = resolve_to_absolute(&tmp);
        assert!(
            std::path::Path::new(&result).is_absolute(),
            "Existing dir should resolve to absolute, got: {result}"
        );
    }

    /// On Windows, `canonicalize` returns `\\?\C:\...` paths which break
    /// string-based path construction.  Verify we strip the prefix.
    #[cfg(windows)]
    #[test]
    fn test_resolve_to_absolute_no_extended_prefix() {
        let tmp = std::env::temp_dir();
        let result = resolve_to_absolute(&tmp);
        assert!(
            !result.starts_with(r"\\?\"),
            "Extended-length prefix should be stripped, got: {result}"
        );
        assert!(
            std::path::Path::new(&result).is_absolute(),
            "Result should still be an absolute path, got: {result}"
        );
    }

    // -----------------------------------------------------------------------
    // classify_session_type tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_classify_session_type_dm() {
        assert_eq!(classify_session_type("dm:alice:bob"), "dm");
        assert_eq!(classify_session_type("dm:x:y"), "dm");
    }

    #[test]
    fn test_classify_session_type_notification() {
        assert_eq!(classify_session_type("notifications:alice"), "notification");
        assert_eq!(
            classify_session_type("notifications:my-agent"),
            "notification"
        );
    }

    #[test]
    fn test_classify_session_type_job() {
        assert_eq!(
            classify_session_type("job_550e8400-e29b-41d4-a716-446655440000"),
            "job"
        );
        assert_eq!(classify_session_type("job_abc"), "job");
    }

    #[test]
    fn test_classify_session_type_subagent() {
        assert_eq!(classify_session_type("subagent_research"), "subagent");
        assert_eq!(classify_session_type("subagent_task_1"), "subagent");
    }

    #[test]
    fn test_classify_session_type_episodic() {
        assert_eq!(classify_session_type("episodic:main"), "episodic");
    }

    #[test]
    fn test_classify_session_type_telegram() {
        assert_eq!(classify_session_type("telegram_mybot_12345"), "telegram");
        assert_eq!(classify_session_type("telegram_main_999"), "telegram");
    }

    #[test]
    fn test_classify_session_type_chat_default() {
        assert_eq!(classify_session_type("web"), "chat");
        assert_eq!(classify_session_type("default"), "chat");
        assert_eq!(classify_session_type("my-custom-context"), "chat");
    }

    // -----------------------------------------------------------------------
    // parse_subagent_context tests (#1277)
    // -----------------------------------------------------------------------

    /// Build the context ids exactly as `derive_subagent_identity` does, so
    /// these rows fail if that format ever moves.
    fn named_context(parent: AgentId, name: &str) -> String {
        format!("subagent_{}_{}", parent.0, name)
    }

    #[test]
    fn parse_subagent_context_recovers_the_name_of_a_named_subagent() {
        let parent = AgentId::new();
        assert_eq!(
            parse_subagent_context(&named_context(parent, "reviewer")),
            Some(SubagentOwner::Named("reviewer"))
        );
        // Hyphens are legal in agent names and must survive the split.
        assert_eq!(
            parse_subagent_context(&named_context(parent, "code-reviewer-2")),
            Some(SubagentOwner::Named("code-reviewer-2"))
        );
    }

    #[test]
    fn parse_subagent_context_never_reports_a_task_id_as_a_name() {
        let parent = AgentId::new();
        let task_id = Uuid::new_v4();
        let ctx = format!("subagent_{}_{}", parent.0, task_id);
        // The ephemeral shape is structurally identical to the named one, so
        // a "take the trailing segment" parse would hand a UUID to the UI as
        // if it were the subagent's name.
        assert_eq!(parse_subagent_context(&ctx), Some(SubagentOwner::Ephemeral));
    }

    #[test]
    fn parse_subagent_context_rejects_shapes_it_cannot_read() {
        // Not a subagent context at all.
        assert_eq!(parse_subagent_context("job_abc"), None);
        // Legacy pre-#1185 ephemeral shape: no parent segment to split on.
        assert_eq!(
            parse_subagent_context(&format!("subagent_{}", Uuid::new_v4())),
            None
        );
        // Parent segment isn't a UUID — not a coordinator-minted context.
        assert_eq!(parse_subagent_context("subagent_notauuid_reviewer"), None);
        // Trailing segment is neither a UUID nor a name the agent registry
        // would ever accept (uppercase is rejected by validate_agent_name).
        let parent = AgentId::new();
        assert_eq!(
            parse_subagent_context(&named_context(parent, "Reviewer")),
            None
        );
        assert_eq!(parse_subagent_context(&named_context(parent, "")), None);
    }

    #[test]
    fn parse_subagent_context_agrees_with_validate_agent_name() {
        // The named arm must not admit anything the registry would reject:
        // the parse is the only thing standing between a raw context segment
        // and a rendered agent label.
        let parent = AgentId::new();
        for name in ["reviewer", "a", "agent-1"] {
            assert!(validate_agent_name(name).is_ok(), "fixture {name} invalid");
            assert_eq!(
                parse_subagent_context(&named_context(parent, name)),
                Some(SubagentOwner::Named(name))
            );
        }
        for name in ["-lead", "lead-", "UPPER", "with space", "default"] {
            assert!(validate_agent_name(name).is_err(), "fixture {name} valid");
            assert_eq!(parse_subagent_context(&named_context(parent, name)), None);
        }
    }
}
