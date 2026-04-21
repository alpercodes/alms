use alms_core::config::{
    ContextConfig, ShellClassificationMode, ShellPermissions, ShellSpillConfig,
};

/// Execution posture: controls whether tools require approval before running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Posture {
    /// Execute tools directly without approval.
    FullControl,
    /// Require explicit user approval before each tool execution (default).
    #[default]
    Guarded,
    /// Fully autonomous — tools execute without approval, no human-in-the-loop
    /// expected. Suitable for background agents, scheduled jobs, DM-triggered
    /// runs, and subagents that should run completely independently.
    Autonomous,
}

impl std::fmt::Display for Posture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Posture::FullControl => write!(f, "full_control"),
            Posture::Guarded => write!(f, "guarded"),
            Posture::Autonomous => write!(f, "autonomous"),
        }
    }
}

impl std::str::FromStr for Posture {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "full_control" => Ok(Posture::FullControl),
            "guarded" => Ok(Posture::Guarded),
            "autonomous" => Ok(Posture::Autonomous),
            other => Err(format!(
                "Invalid posture '{other}'. Must be one of: full_control, guarded, autonomous"
            )),
        }
    }
}

/// Staged system prompts for different phases of the agent loop.
///
/// Developer-controlled prompt files embedded at compile time from
/// `crates/alms-runtime/prompts/`. Not user-editable — workspace files
/// (personality, goals, memories, user) are prepended to both stages.
///
/// The initial prompt comes from `AgentConfig.system_prompt` (defaults to
/// `prompts/initial.md`, overridable per-agent). `tool_loop` is appended
/// to the initial prompt after tool results — it never replaces the
/// agent's identity.
#[derive(Debug, Clone)]
pub struct SystemPrompts {
    /// Appended to the system prompt for LLM calls after tool results.
    /// The agent's initial prompt (identity, instructions) is preserved;
    /// this adds continuation guidance on top.
    pub tool_loop: String,
}

impl Default for SystemPrompts {
    fn default() -> Self {
        Self {
            tool_loop: include_str!("../../prompts/tool_loop.md")
                .trim()
                .to_string(),
        }
    }
}

/// Agent runtime configuration
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// System prompt used for the initial LLM call. Defaults to
    /// `prompts/initial.md`. Per-agent overrides replace this value
    /// but leave `prompts.tool_loop` unchanged.
    pub system_prompt: String,
    /// Staged system prompts for the agent loop.
    pub prompts: SystemPrompts,
    /// Maximum iterations for tool loops
    pub max_iterations: u32,
    /// Maximum tokens per response
    pub max_tokens: u32,
    /// Context window management config
    pub context_config: ContextConfig,
    /// Execution posture (full_control, guarded, or autonomous)
    pub posture: Posture,
    /// Filesystem sandbox root (default "."). Empty string = unrestricted.
    pub sandbox_root: String,
    /// Shell execution policy: "sandboxed" or "unrestricted".
    pub shell_policy: String,
    /// Permission-based allow/deny list for shell commands.
    /// Regex patterns matched against commands before execution.
    pub shell_permissions: ShellPermissions,
    /// Built-in risk classification mode for shell commands.
    /// Layers with `shell_permissions` — both must pass before execution.
    pub shell_classification_mode: ShellClassificationMode,
    /// Large-output spill-to-disk policy for the shell tool (issue #756).
    ///
    /// Mirrors the plumbing of [`shell_permissions`]/[`shell_classification_mode`]:
    /// populated from `[tools.shell_spill]` in the gateway's config assembly
    /// and propagated to subagents in the coordinator so their shell tools
    /// inherit the same spill policy as the parent. Config-file-only — not
    /// mutable via `PATCH /settings`.
    pub shell_spill: ShellSpillConfig,
    /// Enabled builtin tools. Empty = all enabled (backward compatible).
    pub enabled_tools: Vec<String>,
    /// Enable the multi-stage fuzzy-match replacer cascade in `fs_edit`.
    /// Off by default — opt-in per agent via `tools.fs_edit.fuzzy_match`
    /// in `alms.toml`. See issue #755.
    pub fs_edit_fuzzy_match: bool,
    /// When true, the runtime emits a `ContextDebug` event after building
    /// the context window, allowing the web UI to display exactly what the
    /// LLM sees (system prompt, history, tools, token counts).
    pub debug_mode: bool,
    /// Extended-thinking budget for Anthropic Claude 4.x, in tokens.
    ///
    /// `0` disables extended thinking for this agent (no `thinking` field
    /// on outgoing requests, no reasoning deltas). Non-zero values enable
    /// it and govern how many tokens the model may spend on internal
    /// reasoning before emitting its final response.
    ///
    /// Populated by the gateway via the three-layer precedence chain
    /// (per-run > per-agent > server default from `[llm.anthropic]`).
    /// Silently ignored when the effective provider is not Anthropic.
    pub anthropic_thinking_budget: u32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            system_prompt: include_str!("../../prompts/initial.md").trim().to_string(),
            prompts: SystemPrompts::default(),
            max_iterations: 10,
            max_tokens: 100_000,
            context_config: ContextConfig::default(),
            posture: Posture::default(),
            sandbox_root: ".".into(),
            shell_policy: "sandboxed".into(),
            shell_permissions: ShellPermissions::default(),
            shell_classification_mode: ShellClassificationMode::default(),
            shell_spill: ShellSpillConfig::default(),
            enabled_tools: Vec::new(),
            fs_edit_fuzzy_match: false,
            debug_mode: false,
            anthropic_thinking_budget: 0,
        }
    }
}

/// Result of a single agent run, including the response text and accumulated token usage.
#[derive(Debug, Clone)]
pub struct RunOutput {
    pub response: String,
    pub usage: alms_core::TokenUsage,
    /// Tool call records collected during this run (calls + results).
    pub tool_calls: Vec<alms_core::ToolCallRecord>,
}
