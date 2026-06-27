use alms_core::config::{
    ContextConfig, DEFAULT_AGENT_MAX_TOKENS, ShellClassificationMode, ShellPermissions,
    ShellSpillConfig,
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
/// (personality, goals, memories, user) are appended after the base
/// prompt at both stages (see `docs/system-prompts.md` § Prompt Assembly
/// Order).
///
/// The initial prompt comes from `AgentConfig.system_prompt` (defaults to
/// `prompts/initial.md`, overridable per-agent). `tool_loop` is appended
/// after the workspace prefix on subsequent LLM calls — it never replaces
/// the agent's identity.
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
    /// Maximum tokens per response
    pub max_tokens: u32,
    /// Hard cap on the number of LLM-call iterations a single run may take
    /// before the agent loop terminates it with an error (#987 / B3).
    ///
    /// Each iteration is one LLM call plus the tool batch it requests.
    /// `0` disables the cap. Populated from
    /// [`alms_core::config::LlmConfig::max_iterations`] by the gateway's
    /// `AgentConfig` assembly and inherited by subagents.
    pub max_iterations: u32,
    /// Absolute wall-clock backstop on a single run, in seconds (#987 / B3,
    /// repurposed in #1150). `0` disables it. Checked between loop iterations
    /// alongside the phase-aware inactivity check.
    ///
    /// Since #1150 this is only the coarse backstop — the primary guard is
    /// inactivity ([`Self::between_iterations_secs`] /
    /// [`Self::tool_phase_ceiling_secs`]). A productive run that keeps making
    /// progress is never clipped by inactivity, so this catches only a run
    /// wedged in a forever-pinging loop. Populated from
    /// [`alms_core::config::LlmConfig::max_run_duration_secs`] by the
    /// gateway's `AgentConfig` assembly and inherited by subagents.
    pub max_run_duration_secs: u64,
    /// Inactivity budget (seconds) for the between-iterations phase — the P1
    /// budget of the phase-aware run timer (#1150). `0` disables it. Reset on
    /// every progress signal and evaluated at the top-of-loop checkpoint.
    ///
    /// Populated from
    /// [`alms_core::config::LlmConfig::between_iterations_secs`] by the
    /// gateway's `AgentConfig` assembly and inherited by subagents.
    pub between_iterations_secs: u64,
    /// Coarse ceiling (seconds) on the tool-execution phase — the P3 budget
    /// of the phase-aware run timer (#1150). `0` disables it. Reset at
    /// tool-batch start and evaluated at the next top-of-loop checkpoint.
    ///
    /// Populated from
    /// [`alms_core::config::LlmConfig::tool_phase_ceiling_secs`] by the
    /// gateway's `AgentConfig` assembly and inherited by subagents.
    pub tool_phase_ceiling_secs: u64,
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
    /// Shared in-loop tool-output truncation policy (issue #851).
    ///
    /// Mirrors `shell_spill`'s plumbing: populated from
    /// `[tools.tool_output_truncate]` in the gateway's config assembly and
    /// inherited by subagents through the coordinator so a parent and its
    /// subagents see identical caps. Config-file-only — not mutable via
    /// `PATCH /settings`.
    pub tool_output_truncate: alms_core::config::ToolOutputTruncateConfig,
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
    /// Populated by the gateway via the two-layer precedence chain
    /// (per-agent > server default from `[llm.anthropic]`).
    /// Silently ignored when the effective provider is not Anthropic.
    pub anthropic_thinking_budget: u32,
    /// Anthropic prompt caching toggle (#766).
    ///
    /// Server-level only — no per-agent / per-run override per issue #766.
    /// When `true`, the Anthropic adapter attaches `cache_control` markers
    /// to the last tool and trailing system block on every request.
    /// Defaults to `true` — prompt caching is free (Anthropic silently
    /// ignores markers on prefixes below the minimum cacheable size), so
    /// there's no downside to leaving it on.
    ///
    /// Populated from `[llm.anthropic].prompt_cache_enabled` in `alms.toml`
    /// via the gateway's `AgentConfig` assembly. Inherited by subagents.
    pub anthropic_prompt_cache_enabled: bool,
    /// OpenAI-compat reasoning effort (#768). Applies when the effective
    /// provider is OpenAI-compatible and the model is a reasoning model
    /// (OpenAI o-series, GPT-5, xAI Grok reasoning variants). The adapter
    /// in [`crate::llm_client`] strips the value for DeepSeek R1
    /// (reasoning is implicit there) and non-reasoning OpenAI models
    /// (gpt-4o etc. would 400 on the unknown param).
    ///
    /// Populated by the gateway via the two-layer precedence chain
    /// (per-agent > server default from `[llm.openai]`).
    pub openai_reasoning_effort: Option<alms_core::config::ReasoningEffort>,
    /// Extended-thinking budget for Gemini 2.5+, in tokens (#769).
    ///
    /// `None` or `Some(0)` disables thinking (no `thinkingConfig` on the
    /// wire). Non-zero values enable it and govern how many tokens the
    /// model may spend on internal reasoning.
    ///
    /// Populated by the gateway via the two-layer precedence chain
    /// (per-agent > server default from `[llm.gemini]`).
    /// Silently ignored when the effective provider is not Gemini.
    pub gemini_thinking_budget: Option<u32>,
    /// Gemini explicit context-caching toggle (#769).
    ///
    /// Server-level only — no per-agent / per-run override per issue #769.
    /// When `true`, the Gemini adapter creates a `cachedContents`
    /// resource for the stable prefix (system instruction + tool
    /// definitions) on the first turn of a session and references it on
    /// subsequent turns. Populated from `[llm.gemini].cache_enabled` in
    /// `alms.toml`; inherited verbatim by subagents.
    pub gemini_cache_enabled: bool,
    /// Gemini cache TTL in seconds (#769). Sent as `ttl` when creating a
    /// new cache entry. Defaults to 300; see [`alms_core::config::GeminiConfig::cache_ttl_seconds`].
    pub gemini_cache_ttl_seconds: u64,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            system_prompt: include_str!("../../prompts/initial.md").trim().to_string(),
            prompts: SystemPrompts::default(),
            // 32K matches prevailing agent-tool defaults (#918) — covers ~95-99%
            // of agent turns (most finish under 8K), reasoning models'
            // hidden-thinking budgets, and long code-gen flows.
            // Operators can override per-agent for code-gen or long-form
            // writing workflows that need more headroom. Sourced from
            // `alms-core` so the config crate's #919 token-budget
            // validator can reuse the same default at load time.
            max_tokens: DEFAULT_AGENT_MAX_TOKENS,
            // Mirror `LlmConfig::default()` (#987 / B3 / #1160 / #1150):
            // generous iteration ceiling, 24-hour absolute backstop, and the
            // phase-aware inactivity budgets (P1 = 3 min, P3 = 15 min). The
            // gateway overwrites these from `[llm]` during `AgentConfig`
            // assembly.
            max_iterations: 500,
            max_run_duration_secs: 86400,
            between_iterations_secs: 180,
            tool_phase_ceiling_secs: 900,
            context_config: ContextConfig::default(),
            posture: Posture::default(),
            sandbox_root: ".".into(),
            shell_policy: "sandboxed".into(),
            shell_permissions: ShellPermissions::default(),
            shell_classification_mode: ShellClassificationMode::default(),
            shell_spill: ShellSpillConfig::default(),
            tool_output_truncate: alms_core::config::ToolOutputTruncateConfig::default(),
            enabled_tools: Vec::new(),
            fs_edit_fuzzy_match: false,
            debug_mode: false,
            anthropic_thinking_budget: 0,
            // Caching is on by default — matches `AnthropicConfig::default()`
            // and is the right answer for every effective provider because
            // non-Anthropic adapters ignore the flag entirely.
            anthropic_prompt_cache_enabled: true,
            openai_reasoning_effort: None,
            // Gemini thinking is off by default, like Anthropic thinking.
            gemini_thinking_budget: None,
            // Gemini caching mirrors Anthropic caching — on by default;
            // adapter is a no-op when the effective provider isn't Gemini.
            gemini_cache_enabled: true,
            // 5 minutes — mirrors Anthropic ephemeral caching.
            gemini_cache_ttl_seconds: 300,
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
    /// Extended-thinking trace from the final LLM turn, surfaced verbatim
    /// from [`crate::agent::loop_impl::AgentLoopOutput::reasoning`].
    ///
    /// Carried out of the runtime so the gateway can:
    /// - Persist it as `reasoning_blocks` metadata on the assistant message
    ///   (already handled inside `finish_run`).
    /// - Strip it from any text passed to the episodic-summary generator
    ///   so the summarizer never ingests reasoning content (#1098).
    ///
    /// `Some(s)` when the model emitted any thinking deltas on the final
    /// turn; `None` otherwise. In the `[Thinking]`-only reasoning-promotion
    /// fallback (see `finalize_content_and_reasoning`), this field holds
    /// the same string as `response` — that's the signal the summarizer
    /// path uses to drop the response when computing the summary input.
    ///
    /// **Why `Option<String>` and not `Option<()>` / `bool`:** the equal-string
    /// reasoning-promotion case alone could be signalled with a flag, but
    /// `strip_reasoning_from_output` in `crates/alms-runtime/src/episodic.rs`
    /// also runs a `starts_with` prefix-match branch as defense-in-depth
    /// against a hypothetical future provider regression that concatenates
    /// reasoning + visible content on a single channel. That branch needs
    /// the actual trace text to subtract; a boolean would not be enough.
    /// Do not "simplify" this to a flag without removing the prefix-match
    /// branch first.
    pub reasoning: Option<String>,
}
