use alms_core::config::ContextConfig;

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
    /// Enabled builtin tools. Empty = all enabled (backward compatible).
    pub enabled_tools: Vec<String>,
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
            enabled_tools: Vec::new(),
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
