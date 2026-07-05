//! Configuration struct definitions and their [`Default`] implementations.
//!
//! Each struct corresponds to a TOML section in `alms.toml`.  Methods that
//! operate on a single config section (e.g. `ServerConfig::db_path`,
//! `ContextConfig::normalize_episodic`, `LoggingConfig::resolve_log_dir`)
//! live here alongside their struct.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tracing::warn;

/// Default per-agent output-token reservation (`agent.max_tokens`).
///
/// 32K matches prevailing agent-tool defaults — covers ~95-99% of agent turns
/// (most finish under 8K), reasoning models' hidden-thinking budgets, and
/// long code-gen flows. Operators can override per-agent for code-gen or
/// long-form writing workflows that need more headroom (#918).
///
/// Lifted out of `alms-runtime`'s `AgentConfig::default()` so the config
/// crate can reuse the same value during config-load-time token-budget
/// validation (#919) — the budget validator there cross-checks
/// `[context].max_input_tokens + agent.max_tokens` against the provider's
/// published context window. Importing the constant from
/// `alms-runtime` would invert the dependency graph (`alms-core` cannot
/// depend on `alms-runtime`), so the source of truth lives here.
pub const DEFAULT_AGENT_MAX_TOKENS: u32 = 32_000;

// ---------------------------------------------------------------------------
// RunSummaryMode enum
// ---------------------------------------------------------------------------

/// How run summaries are generated for episodic memory.
///
/// Serializes to/from lowercase strings (`"off"`, `"heuristic"`, `"llm"`)
/// for TOML and env-var compatibility.
///
/// Unknown/invalid values deserialize to [`RunSummaryMode::Unknown`] and are
/// normalized to [`RunSummaryMode::Llm`] with a warning during config loading.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunSummaryMode {
    /// No summaries generated, no episodic injection.
    Off,
    /// One-line summary from first ~120 chars of input (no LLM call).
    Heuristic,
    /// Rich summary via agent's model (or `summary_model` if configured). Default.
    #[default]
    Llm,
    /// Catch-all for unrecognized values — normalized to `Llm` during loading.
    #[serde(other, rename = "unknown")]
    Unknown,
}

impl std::fmt::Display for RunSummaryMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Off => write!(f, "off"),
            Self::Heuristic => write!(f, "heuristic"),
            Self::Llm => write!(f, "llm"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

impl std::str::FromStr for RunSummaryMode {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "heuristic" => Ok(Self::Heuristic),
            "llm" => Ok(Self::Llm),
            _ => Ok(Self::Unknown),
        }
    }
}

// ---------------------------------------------------------------------------
// ServerConfig
// ---------------------------------------------------------------------------

/// Server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub bind: String,
    /// Base directory for ALMS instance data (SQLite DB, workspace, secrets).
    /// Override with `ALMS_DATA_DIR` env var. Default: `./.alms`.
    ///
    /// Follows the CWD-as-workspace model (like `.git/` or `.cargo/`):
    /// `cd` into your project directory, run `alms gateway`, and all
    /// instance-specific state lives under `.alms/` in the project root.
    pub data_dir: String,
    /// Effective project root — the directory the agent's filesystem
    /// sandbox is rooted at (#945, the workspace v2 redesign).
    ///
    /// Empty string means "resolve at boot from env / current_dir". The CLI's
    /// `--project` flag writes the absolute path here directly so the
    /// downstream gateway sees a single source of truth. Populated by
    /// [`apply_env_overrides`](super::AlmsConfig::apply_env_overrides) from
    /// `ALMS_PROJECT_ROOT` when no CLI override is in play. Resolved to an
    /// absolute path via [`Self::resolved_project_root`] before flowing into
    /// `GatewayConfig` / `AppState`.
    ///
    /// Precedence at boot is:
    /// 1. CLI `--project <path>` flag (highest — written directly into this
    ///    field by the CLI before `serve_with_config`).
    /// 2. `ALMS_PROJECT_ROOT` env var (handled by `apply_env_overrides`).
    /// 3. `std::env::current_dir()` (the fallback used by
    ///    [`Self::resolved_project_root`] when this field is empty).
    ///
    /// Skipped from serde so the `Default::default()` (= "" → current_dir)
    /// behaviour is the canonical "no project root configured" state and a
    /// hand-edited `alms.toml` cannot accidentally pin the project root to a
    /// stale absolute path. Operators who want to override the cwd fallback
    /// should set `ALMS_PROJECT_ROOT` or pass `--project`.
    #[serde(skip)]
    pub project_root: String,
    /// Bearer token for API authentication — loaded from env only
    #[serde(skip)]
    pub auth_token: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8080".into(),
            data_dir: "./.alms".into(),
            project_root: String::new(),
            auth_token: None,
        }
    }
}

impl ServerConfig {
    /// Return the resolved path to the SQLite database file.
    ///
    /// Precedence: `ALMS_DB_PATH` env var > `{data_dir}/alms.db`.
    ///
    /// Known limitation: `to_string_lossy()` silently replaces non-UTF-8 path
    /// segments with U+FFFD, which could corrupt the path on Linux filesystems
    /// that allow arbitrary byte sequences in filenames. This is not an issue on
    /// Windows (paths are always UTF-16). The proper fix is changing the return
    /// type to `PathBuf`, but that requires updating callers in `alms-session`
    /// and `alms-gateway` which expect `String` for SQLite connection strings.
    pub fn db_path(&self) -> String {
        std::env::var("ALMS_DB_PATH").unwrap_or_else(|_| {
            Path::new(&self.data_dir)
                .join("alms.db")
                .to_string_lossy()
                .into_owned()
        })
    }

    /// Return the resolved path to the agents-metadata directory under
    /// the project root (#945).
    ///
    /// Resolution: `<project_root>/.alms/agents/`. This is the new layout
    /// — flat, one level under `.alms/`, sibling to `alms.db`. Each agent's
    /// `personality.md`/`goals.md`/`memories.md`/`user/` lives in
    /// `<agents_dir>/<name>/`.
    ///
    /// Precedence:
    /// 1. `ALMS_WORKSPACE_DIR` env var (legacy override — kept so operators
    ///    can pin agent metadata to a custom location).
    /// 2. `<project_root>/.alms/agents/` (the v2 default).
    pub fn agents_dir(&self) -> PathBuf {
        if let Ok(val) = std::env::var("ALMS_WORKSPACE_DIR") {
            return PathBuf::from(val);
        }
        self.resolved_project_root().join(".alms").join("agents")
    }

    /// Return the resolved project root — the directory the agent's
    /// filesystem sandbox is rooted at (#945).
    ///
    /// Precedence:
    /// 1. `self.project_root` if non-empty (CLI `--project` flag, written
    ///    directly into this field by the CLI before
    ///    `serve_with_config`).
    /// 2. `ALMS_PROJECT_ROOT` env var (also written into `self.project_root`
    ///    by `AlmsConfig::apply_env_overrides`; this branch handles the case
    ///    where the field was cleared by a fresh `Default::default()`).
    /// 3. `std::env::current_dir()` (the fallback every default install hits).
    /// 4. `PathBuf::from(".")` if the cwd lookup fails — same final fallback
    ///    as [`crate::resolve_to_absolute`].
    ///
    /// The returned path is NOT canonicalized here; callers that need a
    /// canonical path (the gateway's sandbox-root resolution at agent
    /// construction time) call `std::fs::canonicalize` themselves so they
    /// can fail-closed when the path doesn't exist (see
    /// `AgentRuntime::new`).
    pub fn resolved_project_root(&self) -> PathBuf {
        if !self.project_root.is_empty() {
            return PathBuf::from(&self.project_root);
        }
        if let Ok(val) = std::env::var("ALMS_PROJECT_ROOT")
            && !val.is_empty()
        {
            return PathBuf::from(val);
        }
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }
}

// ---------------------------------------------------------------------------
// LlmConfig
// ---------------------------------------------------------------------------

/// LLM provider configuration.
/// Note: api_key is only loaded from env vars, never serialized.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    /// Provider selector. Either a built-in sugar name (`openai`,
    /// `openrouter`, `anthropic`) or the key of an entry in
    /// `[llm.providers.<name>]`.
    pub provider: String,
    pub base_url: String,
    pub model: String,
    #[serde(skip)]
    pub api_key: Option<String>,
    /// Total per-request deadline (seconds): the whole LLM call — connect,
    /// headers, and the full response body — must complete within this
    /// window or the request is aborted.
    ///
    /// This is reqwest's total `.timeout()` and is the only bound on the
    /// post-connect header / time-to-first-byte wait — a healthy response that
    /// is slow to *start* is governed by this value alone. (TCP + TLS connect
    /// is bounded tighter by the client's fixed connect timeout, so a
    /// dead/unreachable host fails in ~30s rather than after this whole
    /// window.) A *stalled body* (one that started arriving and then went
    /// quiet) is caught sooner by the body-only inactivity guard
    /// ([`Self::stream_chunk_timeout_secs`]), so this value is the outer
    /// bound for an otherwise healthy-but-large or healthy-but-slow-to-start
    /// response.
    ///
    /// Default 600 (10 min). This is the per-*call* HTTP deadline (one
    /// request→response), not a run cap: heavy reasoning models (e.g.
    /// `minimax/minimax-m3` on openrouter) legitimately reason past the old
    /// 120s, while a genuine *stall* still fails fast via the body-only guard
    /// below (#1163).
    pub timeout_secs: u64,
    pub max_retries: u32,
    pub max_tokens_per_run: u32,
    pub mock: bool,
    /// Hard cap on the number of LLM-call iterations a single agent run may
    /// take before it is terminated with an error (#987 / B3).
    ///
    /// Each loop iteration is one LLM call plus the tool batch it requests.
    /// Per-step timeouts ([`Self::timeout_secs`] /
    /// [`Self::stream_chunk_timeout_secs`]) bound how long any one step can
    /// take, but nothing bounded the *count* of steps — so an agent that
    /// keeps calling tools without ever producing a deliverable reply ran
    /// forever. In a DM that left the peer stranded on "Chatting with…"
    /// indefinitely; the gateway's DM completion gate now converts the cap
    /// trip into an `Errored` conversation end so the peer is notified.
    ///
    /// Default 500 — generous enough that ordinary multi-tool turns are never
    /// clipped (most finish in single digits), sized for all run types
    /// including deep autonomous turns. `0` disables the cap (no limit).
    /// Inherited by subagents.
    pub max_iterations: u32,
    /// Absolute wall-clock backstop on a single agent run, in seconds
    /// (#987 / B3, repurposed in #1150). `0` disables it.
    ///
    /// Since #1150 the *primary* run-duration guard is **inactivity-based**
    /// (see [`Self::between_iterations_secs`] and
    /// [`Self::tool_phase_ceiling_secs`]): a run is terminated when it stops
    /// making progress for the relevant phase budget, not when total
    /// wall-clock elapses. A run that keeps producing tokens / starting tools
    /// indefinitely is therefore *not* killed by the inactivity guard — that
    /// only happens if the agent is genuinely wedged in a productive-looking
    /// loop (a bug). This value is the coarse backstop that catches exactly
    /// that case.
    ///
    /// Checked between iterations alongside the inactivity check; an
    /// in-flight LLM/tool step is bounded by its own per-step timeout, so the
    /// effective ceiling is this value plus at most one step. Default 86400
    /// (24 hours) — a deliberately generous ceiling that legitimate
    /// long-running scheduled jobs never reach; raised from 4h in #1150 now
    /// that inactivity (not wall-clock) is what stops a wedged run. Inherited
    /// by subagents.
    pub max_run_duration_secs: u64,
    /// Inactivity budget, in seconds, for the **between-iterations** phase of
    /// an agent run — the P1 budget of the phase-aware run timer (#1150).
    ///
    /// Evaluated at the top-of-loop checkpoint: if the run has produced no
    /// progress signal (a streamed token / reasoning delta, an LLM response,
    /// or a tool start) for at least this many seconds while resting between
    /// iterations, the run is terminated with a "stalled" error. Unlike the
    /// old wall-clock cap this resets on every progress signal, so a long but
    /// *productive* run is never clipped. `0` disables the P1 budget.
    ///
    /// Default 180 (3 minutes). Config-file-only — not mutable via
    /// `PATCH /settings`. Inherited by subagents.
    pub between_iterations_secs: u64,
    /// Coarse ceiling, in seconds, on the **tool-execution** phase of an
    /// agent run — the P3 budget of the phase-aware run timer (#1150).
    ///
    /// Reset at tool-batch start and evaluated at the next top-of-loop
    /// checkpoint, so it bounds how long a single tool batch may run before
    /// the run is terminated as stalled. Timed tools (e.g. `shell`, which is
    /// itself capped at `MAX_TIMEOUT_SECS`) finish first under their own
    /// timeout; this ceiling is the backstop for the currently-untimed
    /// `fs_*` tools (their per-tool timeouts are tracked separately in
    /// #1173). `0` disables the P3 ceiling.
    ///
    /// Default 900 (15 minutes) — deliberately set *above* the longest single
    /// tool timeout (the shell tool's 600s `MAX_TIMEOUT_SECS`), not equal to
    /// it: a 600s ceiling would false-stall a `shell` command that legitimately
    /// ran to its own 600s cap, because the batch then completes and the next
    /// checkpoint sees `idle == ceiling`, which trips. The 5-minute margin
    /// keeps this backstop clear of every per-tool timeout (`http_get` ≈ 30s,
    /// background `shell` ≈ 5s; `fs_*` is currently untimed).
    /// Config-file-only — not mutable via `PATCH /settings`. Inherited by
    /// subagents.
    pub tool_phase_ceiling_secs: u64,
    /// Per-chunk body-read inactivity timeout (seconds) — the window within
    /// which the response *body* must keep making progress, reset after every
    /// successful read.
    ///
    /// Applied identically on both paths (#1163), and **only to the body
    /// read** — never to the connect / header / time-to-first-byte wait,
    /// which stays bounded solely by the total [`Self::timeout_secs`]:
    /// - On the **streaming** path it is the application-level per-chunk
    ///   timeout in `stream_response`: if no SSE data arrives within this
    ///   window the stream is treated as stalled and terminated.
    /// - On the **non-streaming** (buffered) path the body is drained as a
    ///   chunk stream under the same per-chunk timeout
    ///   (`read_body_with_idle_timeout`), so a body that starts arriving and
    ///   then stalls mid-transfer faults within this window instead of hanging
    ///   until the total [`Self::timeout_secs`] deadline.
    ///
    /// Because the guard is body-only, a healthy response that is merely slow
    /// to send its *first* byte is governed by [`Self::timeout_secs`], not
    /// this value — raise `timeout_secs` for slow-to-start upstreams.
    ///
    /// Default: 180. The per-chunk *silence* guard for the body; a genuine
    /// stall still fails within this window. Note the #1150 P0
    /// awaiting-first-activity budget is *derived* as
    /// `stream_chunk_timeout_secs + 30`, so this default moves it 90s → 210s
    /// (intended — gives heavy reasoning models room before the first delta).
    pub stream_chunk_timeout_secs: u64,

    /// Generic per-provider config table. Keys here are referenced by
    /// [`LlmConfig::provider`].
    ///
    /// Entries are populated from `[llm.providers.<name>]` in `alms.toml`.
    /// Built-in sugar names (`openai`, `openrouter`, `anthropic`) are
    /// auto-populated at config-load time so the rest of the system sees a
    /// uniform view regardless of which config shape the user wrote.
    ///
    /// See [`ProviderEntry`] for the schema of an individual entry.
    pub providers: BTreeMap<String, ProviderEntry>,

    /// Anthropic-specific configuration surfaced as `[llm.anthropic]` in
    /// `alms.toml`. Only consulted when the effective provider maps to the
    /// Anthropic wire protocol; ignored otherwise.
    pub anthropic: AnthropicConfig,

    /// OpenAI-compatible reasoning-model configuration surfaced as
    /// `[llm.openai]` in `alms.toml`. Only consulted when the effective
    /// provider maps to the OpenAI-compatible wire protocol; ignored on
    /// Anthropic/Gemini paths.
    pub openai: OpenAiConfig,

    /// Gemini-specific configuration surfaced as `[llm.gemini]` in
    /// `alms.toml`. Only consulted when the effective provider maps to the
    /// Gemini wire protocol; ignored on OpenAI / Anthropic paths.
    pub gemini: GeminiConfig,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: "openrouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            model: "z-ai/glm-5.2".into(),
            api_key: None,
            // 10 min — per-call HTTP deadline, not a run cap. Heavy reasoning
            // models (minimax-m3 on openrouter) reason past the old 120s; a
            // genuine stall still fails fast via stream_chunk_timeout_secs.
            timeout_secs: 600,
            max_retries: 2,
            max_tokens_per_run: 0,
            // 500 iterations is a generous ceiling sized for all run types
            // (web chat, cron jobs, deep autonomous turns) — most agent turns
            // finish in single digits even with parallel tool batches. Bounds
            // the #987 "run forever" class without clipping legitimate long
            // multi-tool work.
            max_iterations: 500,
            // 24 hours — absolute backstop only (#1150). Inactivity, not
            // wall-clock, is what now stops a wedged run; this just catches a
            // run that pings activity forever (a bug). Raised from 4h so a
            // legitimate long-running scheduled job is never clipped.
            max_run_duration_secs: 86400,
            // Phase-aware inactivity budgets (#1150). P1 between-iterations
            // idle = 3 min; P3 tool-batch ceiling = 15 min — deliberately a
            // margin *above* the longest single-tool timeout (shell's 600s
            // MAX_TIMEOUT_SECS) so a tool run to its own cap completes and
            // reports back before this ceiling is evaluated; an equal 600s
            // would false-stall such a run at `idle == ceiling`. The P0
            // awaiting-first-activity budget is derived
            // (stream_chunk_timeout_secs + 30s slack), not a knob — so the 180s
            // default below makes it ~210s.
            between_iterations_secs: 180,
            tool_phase_ceiling_secs: 900,
            mock: false,
            // 3 min — per-chunk body-silence guard. Gives heavy reasoning
            // models room before the first delta; derives P0 = ~210s (#1150).
            stream_chunk_timeout_secs: 180,
            providers: BTreeMap::new(),
            anthropic: AnthropicConfig::default(),
            openai: OpenAiConfig::default(),
            gemini: GeminiConfig::default(),
        }
    }
}

/// Anthropic-specific configuration (`[llm.anthropic]` in `alms.toml`).
///
/// Kept separate from [`ProviderEntry`] — which only models the generic
/// wire-level plumbing (base URL, auth scheme, quirks) — because extended
/// thinking is a provider-specific feature whose shape doesn't generalize
/// to OpenAI-compatible endpoints.
///
/// Fields here are the server-level defaults. Agents can opt in or out
/// individually via [`crate::registry::AgentRecord::thinking_budget_tokens`],
/// and an individual run can override either via the run-create API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnthropicConfig {
    /// Extended-thinking budget in tokens. `0` disables extended thinking
    /// (the default — Anthropic returns no `thinking` content blocks).
    ///
    /// When non-zero, every Anthropic request grows a
    /// `"thinking": {"type": "enabled", "budget_tokens": N}` field, and the
    /// provider streams `thinking_delta` content blocks before the final
    /// assistant text. The runtime surfaces those as
    /// `RuntimeEvent::ReasoningDelta` events; the UI renders them in a
    /// collapsible panel under the assistant turn.
    ///
    /// Follow-up turns with tool use do NOT need prior thinking replayed
    /// back (Anthropic's standard mode doesn't require it), so this value
    /// only affects what's emitted in the current response.
    pub thinking_budget_tokens: u32,
    /// Enable Anthropic prompt caching (#766).
    ///
    /// When `true` (the default), every Anthropic request attaches
    /// `cache_control: { type: "ephemeral" }` markers to the last tool
    /// definition and the trailing system content block. Anthropic
    /// caches the prefix up to each marker for 5 minutes; subsequent
    /// requests whose prefix matches byte-for-byte are served from
    /// cache at ~10% of standard input-token cost.
    ///
    /// Setting `false` strips all cache markers — use this to diagnose
    /// cache-related failures or if your upstream proxy does not honour
    /// Anthropic's cache-control shape.
    ///
    /// Server-level only — no per-agent or per-run override. Prompt
    /// caching is a pure optimization; toggling it mid-session only
    /// costs one cache miss on the next turn.
    pub prompt_cache_enabled: bool,
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        Self {
            // 2048 tokens of extended thinking enabled by default —
            // sits between Anthropic's minimum (~1024) and the
            // "noticeable latency" range (8192+). 2048 is enough
            // headroom for the model to plan a multi-step tool turn
            // without doubling response latency on short turns where
            // thinking falls below the wire-budget floor. Operators
            // who want thinking off can set `thinking_budget_tokens =
            // 0` in `[llm.anthropic]` (or via `PATCH /settings`'s
            // `llm.anthropic.thinking_budget_tokens` knob), and per-
            // agent `Some(0)` still wins per the two-layer precedence
            // chain from #767/#941.
            thinking_budget_tokens: 2048,
            // Caching defaults to on — it's free when the prefix is below
            // Anthropic's minimum cacheable size (they silently ignore
            // markers on short prefixes) and saves input tokens on the
            // common case of long system prompts + stable tool lists.
            prompt_cache_enabled: true,
        }
    }
}

/// Reasoning effort level for OpenAI-compatible reasoning models (#768).
///
/// Maps directly to the `reasoning_effort` request field accepted by
/// OpenAI o-series, GPT-5, and xAI Grok reasoning models. DeepSeek R1 does
/// NOT accept this param (reasoning is always on for `deepseek-reasoner`);
/// the adapter strips it from requests routed to DeepSeek-shaped endpoints.
///
/// Serialized lowercase for TOML / JSON / wire compatibility. Parsing is
/// strict — unknown values fail deserialization rather than silently
/// falling back, so misspellings in `alms.toml` surface at load time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    /// GPT-5 only — fastest, minimal chain-of-thought budget.
    Minimal,
    /// Low reasoning budget.
    Low,
    /// Balanced reasoning budget (provider default when param is omitted).
    Medium,
    /// High reasoning budget — slower, more thorough.
    High,
}

impl ReasoningEffort {
    /// Wire string (`"low"`, `"medium"`, `"high"`, `"minimal"`).
    pub fn as_wire_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

impl std::fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire_str())
    }
}

impl std::str::FromStr for ReasoningEffort {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "minimal" => Ok(Self::Minimal),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            other => Err(format!(
                "invalid reasoning_effort '{other}' — expected one of: low, medium, high, minimal"
            )),
        }
    }
}

/// OpenAI-compatible reasoning-model configuration (`[llm.openai]` in `alms.toml`).
///
/// Mirrors the shape of [`AnthropicConfig`] — a provider-family-specific
/// container that sits alongside the generic [`ProviderEntry`] table.
/// Applied to requests whose effective provider maps to the
/// OpenAI-compatible wire protocol (OpenAI, OpenRouter, xAI, Groq,
/// self-hosted vLLM, etc.). Silently ignored on Anthropic/Gemini paths.
///
/// See [`ReasoningEffort`] for semantics and [`crate::registry::AgentRecord::reasoning_effort`]
/// for the per-agent override. Three-layer precedence (per-run > per-agent >
/// server default) matches how `thinking_budget_tokens` behaves for Anthropic.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenAiConfig {
    /// Server default reasoning effort for OpenAI-compat reasoning models.
    ///
    /// `None` (the default — no value in TOML) means "don't send a
    /// `reasoning_effort` field on the wire", which preserves existing
    /// behaviour for non-reasoning models (gpt-4o, claude-sonnet, etc.)
    /// that 400 when they receive the param.
    ///
    /// When set, the adapter forwards the value only to requests targeting
    /// a reasoning-capable model (see `alms_runtime::llm_client::is_openai_reasoning_model`
    /// for the detection heuristic). DeepSeek R1 is specifically excluded —
    /// its `deepseek-reasoner` model reasons automatically and rejects the
    /// param. Non-reasoning models also skip the field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// Gemini-specific configuration (`[llm.gemini]` in `alms.toml`).
///
/// Applied to requests whose effective provider maps to the Gemini wire
/// protocol. Silently ignored on OpenAI / Anthropic paths.
///
/// Covers the two features added in issue #769:
/// - Explicit context caching via Gemini's `cachedContents` REST resource
///   ([`cache_enabled`][Self::cache_enabled], [`cache_ttl_seconds`][Self::cache_ttl_seconds]).
/// - Extended thinking via `generationConfig.thinkingConfig.thinkingBudget`
///   ([`thinking_budget`][Self::thinking_budget]).
///
/// Mirrors the shape of [`AnthropicConfig`] / [`OpenAiConfig`] — a
/// provider-family-specific container that sits alongside the generic
/// [`ProviderEntry`] table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeminiConfig {
    /// Enable Gemini explicit context caching (#769).
    ///
    /// When `true` (the default), the Gemini adapter creates a
    /// `cachedContents` resource on the first request of a session that
    /// crosses Gemini's minimum cacheable size (32,768 tokens) and
    /// references the returned cache name via `cachedContent:
    /// "cachedContents/<id>"` on subsequent requests. The cache is
    /// reused until the TTL expires or the stable prefix (system
    /// instruction + tool definitions) changes.
    ///
    /// Setting `false` disables cache creation entirely — useful for
    /// diagnosing cache-related failures, running on a Gemini project
    /// that doesn't have caching enabled, or for constraining cost when
    /// the stable prefix churns faster than the TTL.
    ///
    /// Server-level only — no per-agent or per-run override. Gemini
    /// caching is a pure optimization; toggling it only costs one cache
    /// miss on the next turn.
    pub cache_enabled: bool,

    /// Cache TTL in seconds for Gemini `cachedContents` (#769).
    ///
    /// Sent as the `ttl` field when creating a cache entry. Gemini
    /// enforces the TTL server-side; ALMS does not track the expiry
    /// client-side. When Gemini returns an error indicating the
    /// referenced cache is gone, the adapter invalidates its stored
    /// handle and creates a fresh cache on the next turn.
    ///
    /// Default: 300 seconds (5 minutes), matching Anthropic's ephemeral
    /// cache TTL and keeping idle cache storage cost low. Gemini's own
    /// default when the field is omitted is 1 hour.
    pub cache_ttl_seconds: u64,

    /// Extended-thinking budget for Gemini 2.5+, in tokens (#769).
    ///
    /// `None` or `Some(0)`: extended thinking is disabled (no
    /// `thinkingConfig` field on outgoing requests; `thought: true`
    /// parts are never requested).
    ///
    /// `Some(n)` with `n > 0`: the Gemini adapter injects
    /// `generationConfig.thinkingConfig: { thinkingBudget: n,
    /// includeThoughts: true }` into the outgoing request. The provider
    /// streams parts with `thought: true` alongside the visible text;
    /// the runtime routes those through the provider-neutral
    /// `RuntimeEvent::ReasoningDelta` channel so the UI renders them in
    /// the same collapsible panel as Anthropic extended thinking and
    /// OpenAI o-series reasoning.
    ///
    /// Three-layer precedence mirrors Anthropic `thinking_budget_tokens`
    /// and OpenAI `reasoning_effort`: per-run > per-agent > server
    /// default. `Some(0)` at any layer is an explicit opt-out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<u32>,
}

impl Default for GeminiConfig {
    fn default() -> Self {
        Self {
            cache_enabled: true,
            cache_ttl_seconds: 300,
            thinking_budget: None,
        }
    }
}

impl LlmConfig {
    /// Populate [`providers`][Self::providers] with built-in sugar entries
    /// for `openai`, `openrouter`, and `anthropic` if the user has not
    /// already defined them.
    ///
    /// This lets the rest of the system look up every provider uniformly
    /// in `providers`, regardless of whether the user wrote the classic
    /// flat form (`provider = "openrouter"` + `base_url = "..."`) or the
    /// new generic form (`[llm.providers.<name>]`).
    ///
    /// Called once during [`crate::AlmsConfig::load`]; safe to call
    /// multiple times (existing entries are preserved).
    pub fn ensure_builtin_providers(&mut self) {
        self.providers
            .entry("openrouter".to_string())
            .or_insert_with(|| ProviderEntry {
                kind: ProviderKind::OpenAiCompatible,
                base_url: "https://openrouter.ai/api/v1".into(),
                api_key_env: None,
                api_key: None,
                model: None,
                auth_scheme: AuthScheme::Bearer,
                quirks: ProviderQuirks::default(),
            });
        self.providers
            .entry("openai".to_string())
            .or_insert_with(|| ProviderEntry {
                kind: ProviderKind::OpenAiCompatible,
                base_url: "https://api.openai.com/v1".into(),
                api_key_env: None,
                api_key: None,
                model: None,
                auth_scheme: AuthScheme::Bearer,
                quirks: ProviderQuirks::default(),
            });
        self.providers
            .entry("anthropic".to_string())
            .or_insert_with(|| ProviderEntry {
                kind: ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: None,
                model: None,
                auth_scheme: AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: ProviderQuirks::default(),
            });
        // Google Gemini — native adapter (see `alms-runtime/src/gemini.rs`).
        // Authenticates via the `x-goog-api-key` header (preferred over the
        // `?key=` query parameter because it keeps the secret out of URL
        // logs).
        self.providers
            .entry("gemini".to_string())
            .or_insert_with(|| ProviderEntry {
                kind: ProviderKind::Gemini,
                base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
                api_key_env: None,
                api_key: None,
                model: None,
                auth_scheme: AuthScheme::Header {
                    name: "x-goog-api-key".into(),
                },
                quirks: ProviderQuirks::default(),
            });
    }

    /// Look up the [`ProviderEntry`] matching [`LlmConfig::provider`],
    /// if any.
    pub fn resolved_provider(&self) -> Option<&ProviderEntry> {
        self.providers.get(&self.provider)
    }
}

impl ProviderEntry {
    /// Resolve the API key for this provider entry from its configured
    /// sources, in order of decreasing precedence:
    ///
    /// 1. `api_key_env` — read the named environment variable. Empty or
    ///    unset values fall through to the next source.
    /// 2. `api_key` — the literal key baked into the entry.
    ///
    /// Returns `None` if no source produced a value. The gateway is
    /// expected to consult its `SecretsStore` as a final fallback, since
    /// key material stored via `alms auth set` lives there.
    pub fn resolve_api_key(&self) -> Option<String> {
        if let Some(var) = &self.api_key_env
            && let Ok(val) = std::env::var(var)
            && !val.is_empty()
        {
            return Some(val);
        }
        if let Some(key) = &self.api_key
            && !key.is_empty()
        {
            return Some(key.clone());
        }
        None
    }
}

/// Wire-format for a single `[llm.providers.<name>]` entry in `alms.toml`.
///
/// This is the generic provider surface: an OpenAI-compatible endpoint is
/// described by its base URL, authentication scheme, and a small set of
/// per-provider quirks. Code paths that need to know the protocol family
/// branch on [`ProviderKind`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderEntry {
    /// Protocol family. Defaults to [`ProviderKind::OpenAiCompatible`] so
    /// that simply dropping in a `base_url` for a new provider "just works".
    pub kind: ProviderKind,
    /// Base URL (e.g. `https://api.x.ai/v1`). Path segments like
    /// `/chat/completions` are appended by the client.
    pub base_url: String,
    /// Name of an environment variable that holds the API key. If both
    /// `api_key_env` and `api_key` are set, `api_key_env` wins.
    ///
    /// The gateway resolves this value on demand via [`Self::resolve_api_key`]
    /// when building the runtime `LlmClient`. The env var is **not** written
    /// into [`crate::secrets::SecretsStore`] — if you want the key persisted
    /// in `.alms/secrets.json` (and thus redacted via the store's logging
    /// helpers) run `alms auth set <provider> <key>` separately.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Explicit API key. Discouraged — prefer `api_key_env` or
    /// `alms auth set <provider> <key>`.
    ///
    /// Parsed from inline TOML for local-dev convenience (`api_key = "sk-..."`),
    /// but never serialized back out — the field is only present on the
    /// deserialize side so that a handwritten dev config "just works". For
    /// any long-lived deployment, use `api_key_env` so the key lives in an
    /// environment variable or secrets vault, not in a checked-in file.
    #[serde(default, skip_serializing)]
    pub api_key: Option<String>,
    /// Optional override for the provider's default model. When set, this
    /// wins over [`LlmConfig::model`] when this provider is selected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// How the API key is sent on each request.
    pub auth_scheme: AuthScheme,
    /// Provider-specific behaviour tweaks. See [`ProviderQuirks`].
    pub quirks: ProviderQuirks,
}

/// Wire protocol family used to talk to a provider.
///
/// Native adapters are reserved for providers that cannot be reached
/// through the OpenAI chat-completions protocol. All others — including
/// xAI, DeepSeek, Groq, Mistral, Ollama, LM Studio, and self-hosted vLLM
/// — use [`ProviderKind::OpenAiCompatible`] and differ only in
/// `base_url` / `auth_scheme` / `quirks`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ProviderKind {
    /// OpenAI chat-completions wire format.
    #[default]
    #[serde(rename = "openai_compatible", alias = "openai-compatible")]
    OpenAiCompatible,
    /// Anthropic Messages API.
    #[serde(rename = "anthropic")]
    Anthropic,
    /// Google Gemini `generateContent` / `streamGenerateContent` API.
    ///
    /// Uses a distinct wire shape (`contents[]` with typed `parts[]`,
    /// top-level `systemInstruction`, `functionCall` / `functionResponse`
    /// parts for tool use). Reached only via the native adapter in
    /// `alms-runtime/src/gemini.rs`.
    #[serde(rename = "gemini")]
    Gemini,
}

/// How an API key is attached to each outgoing request.
///
/// Intentionally minimal — only schemes that map to a real-world provider
/// we ship or test against are listed. Additional variants (e.g. query
/// parameters, HMAC-signed requests) can be added the day a concrete
/// integration needs them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthScheme {
    /// `Authorization: Bearer <key>` — default for all OpenAI-compatible
    /// providers.
    #[default]
    Bearer,
    /// Custom header, e.g. `x-api-key` for Anthropic. The raw API key is
    /// placed in the header value without a `Bearer ` prefix.
    Header {
        /// HTTP header name.
        name: String,
    },
}

/// Per-provider request-build quirks.
///
/// These are small, deterministic transforms applied to the final
/// [`crate::config::LlmConfig`]-derived request body. They exist because
/// real-world OpenAI-compatible endpoints occasionally deviate from the
/// reference API in minor ways that are cheaper to paper over here than
/// to carry through as conditional logic in every caller.
///
/// Naming and semantics follow a conventional
/// provider-transform middleware set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderQuirks {
    /// Mistral-style: some providers reject back-to-back `tool` messages
    /// (tool-result followed immediately by another tool-result, which
    /// happens when the agent runs multiple tools in parallel). Set to
    /// true to inject an empty `user` turn between consecutive `tool`
    /// messages so the model sees an alternating role sequence.
    pub tool_gap_fill: bool,

    /// Drop messages whose `content` is empty-or-missing and which carry
    /// no `tool_calls`. Some OpenAI-compatible endpoints 400 on empty
    /// assistant turns; dropping them is safe because they convey no
    /// information.
    pub drop_empty_content: bool,
}

// ---------------------------------------------------------------------------
// SessionConfig
// ---------------------------------------------------------------------------

/// Session management configuration.
///
/// Controls how sessions are stored and retained. This is distinct from
/// [`ContextConfig`], which controls how much of a session's history is sent
/// to the LLM in a single request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionConfig {
    /// Idle timeout before archiving (seconds)
    pub idle_timeout_secs: u64,
    pub auto_archive: bool,
    /// Delete archived sessions after this many seconds
    pub archive_ttl_secs: u64,
    pub max_messages: usize,
    /// Maximum total tokens to retain in a session's history.
    ///
    /// This is a **storage** limit — it caps how much conversation history the
    /// session keeps on disk/in the database. It should be >= `context.max_input_tokens`
    /// because the session must store at least as much history as the LLM context
    /// window can consume per request.
    ///
    /// Not to be confused with [`ContextConfig::max_input_tokens`], which controls
    /// how many tokens are sent to the LLM in a single request.
    pub max_context_tokens: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            idle_timeout_secs: 24 * 60 * 60,
            auto_archive: true,
            archive_ttl_secs: 30 * 24 * 60 * 60,
            max_messages: 10000,
            max_context_tokens: 256_000,
        }
    }
}

// ---------------------------------------------------------------------------
// ContextConfig
// ---------------------------------------------------------------------------

/// Context window management configuration.
///
/// Controls how the session's message history is assembled into a prompt for
/// each LLM request. This is distinct from [`SessionConfig`], which controls
/// how much history the session retains in storage.
///
/// # Strategy redesign (#869)
///
/// As of v0.2.4 the two compaction-relevant strategies are modelled on
/// **token thresholds**, not message counts. The pre-#869
/// `recent_window` + `summary_interval` shape was a hard message-count
/// cap that bypassed the token budget entirely. Strategies in scope:
///
/// - `truncate` — pure token-budget walk: keep the most recent messages
///   that fit the per-request budget; drop everything older.
/// - `compact` — Claude-Code / Codex style auto-compact: when
///   assembled history crosses [`compact_trigger_pct`] of the budget,
///   summarise the older prefix and retain at most [`compact_retain_pct`]
///   worth of recent verbatim messages. Renamed from `sliding-summary`.
/// - `full` — include all history; for small conversations / cheap models.
///
/// `"sliding-summary"` is accepted at load time as an alias for `"compact"`
/// (rewritten on deserialise). The legacy fields `recent_window` and
/// `summary_interval` are deprecated; if present in `alms.toml` they
/// produce a one-time WARN at boot and are ignored.
#[derive(Debug, Clone, Serialize)]
pub struct ContextConfig {
    /// Strategy: `"truncate"` | `"compact"` | `"full"`. The legacy alias
    /// `"sliding-summary"` is accepted at load time and rewritten to
    /// `"compact"` (see the `Deserialize` impl below).
    pub strategy: String,
    /// Maximum tokens to send to the LLM in a single request.
    ///
    /// This is the **per-request** token budget for the context window assembled
    /// by the ContextBuilder. It should match your LLM's context window size.
    ///
    /// Not to be confused with [`SessionConfig::max_context_tokens`], which is
    /// the total token storage limit for the session's history on disk.
    pub max_input_tokens: usize,
    /// Trigger compaction when the assembled history exceeds this
    /// fraction of the **effective history budget**
    /// (`max_input_tokens` minus the system prompt, current input,
    /// episodic block, and the 1000-token reserve `ContextBuilder`
    /// uses) (#869, refined PR #1012).
    ///
    /// Range: `0.50..=0.95`. Default: `0.80` (Claude Code parity).
    /// Out-of-range values are clamped at load time with a WARN.
    /// Only consulted when `strategy == "compact"`.
    pub compact_trigger_pct: f32,
    /// After compaction, retain at most this fraction of the
    /// **effective history budget** worth of recent verbatim messages;
    /// everything older folds into the summary block (#869, refined
    /// PR #1012).
    ///
    /// Range: `0.20..=0.60`. Default: `0.40`.
    /// Out-of-range values are clamped at load time with a WARN.
    /// `compact_retain_pct + 0.10 <= compact_trigger_pct` is enforced — if
    /// the gap is too small, retain is dropped to `trigger - 0.10` so
    /// compaction always measurably reduces context size.
    /// Only consulted when `strategy == "compact"`.
    pub compact_retain_pct: f32,
    /// Separate (cheaper) model for generating summaries.
    /// Falls back to the agent's default model when `None`.
    /// Defaults to `google/gemma-4-31b-it` (on the `openrouter` summary
    /// provider, see `summary_provider`) — a small non-reasoning model so
    /// summaries don't burn tokens on thinking (#1191).
    pub summary_model: Option<String>,
    /// Separate provider for generating summaries (#866).
    ///
    /// When `None` the summary task inherits the agent's resolved provider,
    /// matching the pre-#866 behaviour. When set the summary client is
    /// re-targeted at the named provider via `with_provider_and_secrets` so
    /// `summary_model` can be a slug for a different provider than the agent
    /// (e.g. agent on Anthropic, summary on OpenRouter). The provider must be
    /// configured under `[llm.providers.<name>]` and have a resolvable API key
    /// (either in the secrets store or via `api_key_env` / `api_key`).
    /// Defaults to `openrouter` (#1191), pairing with `summary_model`.
    pub summary_provider: Option<String>,
    /// How run summaries are generated for episodic memory.
    /// See [`RunSummaryMode`] for valid values.
    pub run_summary_mode: RunSummaryMode,
    /// Maximum token budget for episodic run summaries injected into context.
    /// Hard-capped at 15% of `max_input_tokens` — values exceeding the cap
    /// are clamped down with a warning at load time.
    pub run_summary_budget: usize,
    /// Maximum output tokens for the LLM summarizer call.
    ///
    /// Reasoning models may consume a large portion of the token budget on
    /// internal thinking before producing visible output.  The default (1000)
    /// provides enough headroom for models that spend 200-800 tokens on
    /// reasoning.  The actual summary text is typically 50-150 tokens.
    pub summary_max_tokens: u32,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            strategy: "truncate".into(),
            max_input_tokens: 128_000,
            // #869: threshold-based compaction defaults. 0.80 / 0.40 give
            // a comfortable gap (the 0.10 hard floor is satisfied) and
            // match Claude Code's auto-compact trigger. Only consulted
            // when `strategy == "compact"`.
            compact_trigger_pct: 0.80,
            compact_retain_pct: 0.40,
            // Ship an explicit SYMMETRIC summary pair by default (#1191,
            // adopting the alms-test-workspace2 settings). The pair-only
            // validator introduced by #872/#877 requires both fields set
            // together or both unset — the asymmetric (one-of-two) shape
            // was the misconfiguration behind the #866 `model: not found`
            // 404 and is still rejected at load time. A symmetric Some
            // pair is valid: `build_summary_client` re-targets the
            // summary client at `openrouter` via
            // `with_provider_and_secrets` and applies the explicit model
            // last, so the #871 leak guard never fires.
            //
            // Rationale for a dedicated pair (vs the #872-era both-None
            // "inherit the agent's model" default): summaries are
            // low-stakes, high-frequency calls — pinning them to a small
            // non-reasoning model keeps them cheap regardless of which
            // (possibly heavyweight) model the agent itself runs.
            // Operators on a non-OpenRouter agent provider need an
            // OpenRouter API key for the summary task, or should
            // reconfigure the pair. Reverting to inherit-the-agent-model
            // is still possible: clear both fields together (empty
            // strings via PATCH /settings, or `summary_model = ""` +
            // `summary_provider = ""` in alms.toml). A PATCH clear is
            // durable across restarts: `persist_settings` writes the
            // `""` sentinel into `settings.json` and the boot-time
            // overrides apply maps it back to `None` instead of letting
            // this compiled pair resurrect (PR #1194).
            summary_model: Some("google/gemma-4-31b-it".into()),
            summary_provider: Some("openrouter".into()),
            run_summary_mode: RunSummaryMode::Llm,
            run_summary_budget: 2000,
            summary_max_tokens: 1000,
        }
    }
}

// Custom Deserialize impl for ContextConfig (#869).
//
// Two pieces of migration logic land here so they fire wherever a
// `[context]` block is parsed (TOML, JSON via `PersistedContextOverrides`'s
// downstream apply, env-var-rebuilt configs that round-trip through
// deserialize):
//
// 1. The legacy fields `recent_window` and `summary_interval` are accepted
//    on the wire but dropped from the struct, with a one-time `WARN` at
//    `target = "alms.config"` so an operator who upgraded without
//    touching `alms.toml` sees the deprecation message exactly once per
//    process start.
// 2. `strategy = "sliding-summary"` is rewritten to `"compact"` so the
//    rest of the system sees a single canonical name. The runtime
//    dispatch in `context/mod.rs` keeps the alias arm as a belt-and-
//    braces fallback for any path that bypasses this deserialiser.
//
// The boilerplate below mirrors what `#[serde(default)] #[derive(Deserialize)]`
// would have generated, plus the two migration steps. Field order matches
// the struct definition.
impl<'de> Deserialize<'de> for ContextConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Raw {
            strategy: Option<String>,
            max_input_tokens: Option<usize>,
            compact_trigger_pct: Option<f32>,
            compact_retain_pct: Option<f32>,
            // Legacy fields — accepted on the wire so old `alms.toml`
            // / `settings.json` files load without a hard error, then
            // dropped with a one-time WARN.
            recent_window: Option<usize>,
            summary_interval: Option<usize>,
            summary_model: Option<String>,
            summary_provider: Option<String>,
            run_summary_mode: Option<RunSummaryMode>,
            run_summary_budget: Option<usize>,
            summary_max_tokens: Option<u32>,
        }

        let raw = Raw::deserialize(deserializer)?;

        // Deprecation WARN — fires at most once per process for each legacy
        // field, regardless of how many configs round-trip through
        // deserialize during boot (handful, normally).
        if raw.recent_window.is_some() {
            warn_recent_window_once();
        }
        if raw.summary_interval.is_some() {
            warn_summary_interval_once();
        }

        let defaults = ContextConfig::default();
        let strategy = match raw.strategy {
            Some(s) if s == "sliding-summary" => {
                warn_sliding_summary_alias_once();
                "compact".to_string()
            }
            Some(s) => s,
            None => defaults.strategy,
        };

        // Pair-aware default fill for the summary pair (#1191). The shipped
        // default is now a symmetric Some pair, so a naive per-field
        // `raw.x.or(defaults.x)` would silently backfill the missing half
        // of a hand-edited asymmetric TOML (`summary_model` set,
        // `summary_provider` absent, or vice versa) — exactly the
        // half-configured state the #877 pair-only validator exists to
        // reject at load time. Rules:
        //   - both fields absent → inherit the default pair;
        //   - anything else → taken as written, with `""` (empty string)
        //     normalised to `None` as an explicit clear, mirroring the
        //     PATCH /settings sentinel. `summary_model = ""` +
        //     `summary_provider = ""` therefore opts back into inheriting
        //     the agent's resolved (provider, model) for summaries, and an
        //     asymmetric survivor is rejected by `AlmsConfig::validate`.
        let (summary_model, summary_provider) =
            if raw.summary_model.is_none() && raw.summary_provider.is_none() {
                (defaults.summary_model, defaults.summary_provider)
            } else {
                (
                    raw.summary_model.filter(|s| !s.trim().is_empty()),
                    raw.summary_provider.filter(|s| !s.trim().is_empty()),
                )
            };

        Ok(ContextConfig {
            strategy,
            max_input_tokens: raw.max_input_tokens.unwrap_or(defaults.max_input_tokens),
            compact_trigger_pct: raw
                .compact_trigger_pct
                .unwrap_or(defaults.compact_trigger_pct),
            compact_retain_pct: raw
                .compact_retain_pct
                .unwrap_or(defaults.compact_retain_pct),
            summary_model,
            summary_provider,
            run_summary_mode: raw.run_summary_mode.unwrap_or(defaults.run_summary_mode),
            run_summary_budget: raw
                .run_summary_budget
                .unwrap_or(defaults.run_summary_budget),
            summary_max_tokens: raw
                .summary_max_tokens
                .unwrap_or(defaults.summary_max_tokens),
        })
    }
}

// One-time deprecation WARNs for #869. Each `OnceLock` ensures the
// message is emitted at most once per process — operators see the
// deprecation, log scanners don't drown in duplicates if multiple
// agents round-trip the same TOML.
use std::sync::OnceLock;

static WARN_RECENT_WINDOW_ONCE: OnceLock<()> = OnceLock::new();
static WARN_SUMMARY_INTERVAL_ONCE: OnceLock<()> = OnceLock::new();
static WARN_SLIDING_SUMMARY_ALIAS_ONCE: OnceLock<()> = OnceLock::new();

fn warn_recent_window_once() {
    WARN_RECENT_WINDOW_ONCE.get_or_init(|| {
        warn!(
            target: "alms.config",
            "context.recent_window is deprecated and ignored as of v0.2.4 — \
             the threshold-based \"compact\" strategy uses compact_trigger_pct \
             / compact_retain_pct. Remove the field from alms.toml. See #869."
        );
    });
}

fn warn_summary_interval_once() {
    WARN_SUMMARY_INTERVAL_ONCE.get_or_init(|| {
        warn!(
            target: "alms.config",
            "context.summary_interval is deprecated and ignored as of v0.2.4 — \
             the threshold-based \"compact\" strategy uses compact_trigger_pct \
             / compact_retain_pct. Remove the field from alms.toml. See #869."
        );
    });
}

fn warn_sliding_summary_alias_once() {
    WARN_SLIDING_SUMMARY_ALIAS_ONCE.get_or_init(|| {
        warn!(
            target: "alms.config",
            "context.strategy = \"sliding-summary\" is deprecated as of v0.2.4 — \
             rename to \"compact\". The alias is still accepted but will be \
             removed in v0.3.0. See #869."
        );
    });
}

impl ContextConfig {
    /// True when the given summary `(provider, model)` pair equals the
    /// compiled-in default pair (#1191).
    ///
    /// Since #1191 the shipped default is an explicit `Some` pair, so a
    /// resolved `Some` summary provider no longer implies the operator
    /// opted in. Callers that want to distinguish "operator configured a
    /// dedicated summary provider" (worth an `info!`) from "running on the
    /// stock default" (routine, `debug!`) use this — shared here so the
    /// gateway run path and the coordinator subagent path can't drift
    /// (PR #1194, Tim's nit on #1191).
    pub fn is_compiled_default_summary_pair(provider: Option<&str>, model: Option<&str>) -> bool {
        let defaults = Self::default();
        provider == defaults.summary_provider.as_deref()
            && model == defaults.summary_model.as_deref()
    }

    /// Normalize episodic memory settings: validate mode and enforce the 15%
    /// budget cap. Called during config loading, before hard validation.
    ///
    /// As of #869 also handles the new `compact_*` knobs:
    /// - Out-of-range trigger / retain are clamped to their valid ranges.
    /// - The `retain + 0.10 <= trigger` floor is enforced — if violated,
    ///   retain is dropped to `trigger - 0.10` so compaction always
    ///   measurably reduces context size.
    /// - `"sliding-summary"` strategy at this stage is rewritten to
    ///   `"compact"` as a belt-and-braces fallback for any path that
    ///   bypassed the `Deserialize` impl above (e.g. env var override,
    ///   a hand-built `ContextConfig` literal that the test fixture
    ///   sweep missed).
    pub fn normalize_episodic(&mut self) {
        // Normalize Unknown variant (from unrecognized TOML/env values) to Llm
        if self.run_summary_mode == RunSummaryMode::Unknown {
            warn!("Unrecognized run_summary_mode, falling back to \"llm\"");
            self.run_summary_mode = RunSummaryMode::Llm;
        }

        // #869: rewrite the "sliding-summary" alias here too, in case the
        // value arrived via `ALMS_CONTEXT_STRATEGY` (which goes through
        // `apply_env_overrides` directly without round-tripping through
        // `Deserialize`).
        if self.strategy == "sliding-summary" {
            warn_sliding_summary_alias_once();
            self.strategy = "compact".into();
        }

        // #869: clamp `compact_trigger_pct` to [0.50, 0.95].
        const TRIGGER_MIN: f32 = 0.50;
        const TRIGGER_MAX: f32 = 0.95;
        if !(self.compact_trigger_pct.is_finite()
            && (TRIGGER_MIN..=TRIGGER_MAX).contains(&self.compact_trigger_pct))
        {
            let clamped = if !self.compact_trigger_pct.is_finite() {
                0.80
            } else {
                self.compact_trigger_pct.clamp(TRIGGER_MIN, TRIGGER_MAX)
            };
            warn!(
                target: "alms.config",
                configured = self.compact_trigger_pct,
                clamped = clamped,
                "context.compact_trigger_pct out of range [0.50, 0.95]; clamped"
            );
            self.compact_trigger_pct = clamped;
        }

        // #869: clamp `compact_retain_pct` to [0.20, 0.60].
        const RETAIN_MIN: f32 = 0.20;
        const RETAIN_MAX: f32 = 0.60;
        if !(self.compact_retain_pct.is_finite()
            && (RETAIN_MIN..=RETAIN_MAX).contains(&self.compact_retain_pct))
        {
            let clamped = if !self.compact_retain_pct.is_finite() {
                0.40
            } else {
                self.compact_retain_pct.clamp(RETAIN_MIN, RETAIN_MAX)
            };
            warn!(
                target: "alms.config",
                configured = self.compact_retain_pct,
                clamped = clamped,
                "context.compact_retain_pct out of range [0.20, 0.60]; clamped"
            );
            self.compact_retain_pct = clamped;
        }

        // #869: enforce `retain + 0.10 <= trigger` so compaction always
        // measurably reduces context size. If retain is too close to (or
        // above) trigger, drop retain to `trigger - 0.10`.
        const MIN_GAP: f32 = 0.10;
        if self.compact_retain_pct + MIN_GAP > self.compact_trigger_pct {
            let new_retain = (self.compact_trigger_pct - MIN_GAP).max(RETAIN_MIN);
            warn!(
                target: "alms.config",
                trigger = self.compact_trigger_pct,
                retain_was = self.compact_retain_pct,
                retain_now = new_retain,
                "context.compact_retain_pct must be at least 0.10 below compact_trigger_pct; \
                 lowered retain to maintain the gap"
            );
            self.compact_retain_pct = new_retain;
        }

        // Enforce 15% hard cap on run_summary_budget
        let cap = self.max_input_tokens * 15 / 100;
        if self.run_summary_budget > cap {
            warn!(
                configured = self.run_summary_budget,
                cap = cap,
                max_input_tokens = self.max_input_tokens,
                "run_summary_budget exceeds 15% of max_input_tokens, clamping to {}",
                cap,
            );
            self.run_summary_budget = cap;
        }

        // Zero summary_max_tokens would cause the LLM API to reject the
        // request, and since summary generation is fire-and-forget the error
        // gets silently swallowed.  Reset to the default (1000).
        if self.summary_max_tokens == 0 {
            warn!("summary_max_tokens is 0, resetting to default (1000)");
            self.summary_max_tokens = 1000;
        }
    }
}

// ---------------------------------------------------------------------------
// ToolsConfig
// ---------------------------------------------------------------------------

/// Tool configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolsConfig {
    pub enabled: Vec<String>,
    pub timeout_secs: u64,
    pub max_output_bytes: usize,
    /// Filesystem sandbox root. Relative paths are resolved from cwd.
    /// Default: "." (current directory — safe by default).
    /// Set to "" for unrestricted filesystem access.
    pub sandbox_root: String,
    /// Shell execution policy: "sandboxed" (default) or "unrestricted".
    /// In sandboxed mode, the shell's persistent cwd is restricted to `sandbox_root`.
    /// On Linux 5.13+, Landlock filesystem restrictions are also applied so the
    /// child process can only access files within `sandbox_root` (plus read-only
    /// system paths like /usr, /bin, /lib). On Windows/macOS, sandboxed mode
    /// restricts cwd only -- the command can still access files outside the sandbox.
    pub shell_policy: String,
    /// Absolute path to the shell interpreter used by the `shell` tool
    /// (#1121).
    ///
    /// **Config-file-only** (`[tools].shell_path` in `alms.toml`) — like
    /// [`ShellPermissions::classifier_overrides`], this knob is never
    /// mutable via `PATCH /settings`. When set, it is checked first and
    /// wins over all built-in discovery, subject to two hard validations
    /// at spawn time: the path must be an existing *file*, and it must
    /// not live under Windows' `System32`/`Sysnative` (that `bash.exe`
    /// is the WSL launcher, not a shell).
    ///
    /// The target must be a **bash-compatible** interpreter: the pwd
    /// marker wrapper, the destructive-command classifier, and operator
    /// [`ShellPermissions`] regexes all assume POSIX/bash semantics —
    /// pointing this at pwsh/zsh silently degrades the classifier to
    /// coincidental matching.
    ///
    /// Default: unset. On Unix the tool runs `bash` from `PATH`; on
    /// Windows, Git Bash is discovered from well-known install locations
    /// and from the location of `git.exe` on `PATH`. If discovery fails
    /// on Windows, the shell tool fails with an actionable error instead
    /// of silently spawning the WSL launcher.
    #[serde(default)]
    pub shell_path: Option<PathBuf>,
    /// Shell execution engine for the `shell` tool (#1143, Phase 2 of
    /// #1121).
    ///
    /// * `"system-bash"` (default) — spawn an external bash resolved via
    ///   [`shell_path`](Self::shell_path) / built-in discovery. This is
    ///   byte-for-byte the pre-#1143 behavior.
    /// * `"builtin"` — re-exec the ALMS binary itself as a hidden
    ///   `alms shell-host` subcommand, which evaluates the command string
    ///   with the `brush_core` Rust bash interpreter. No `PATH` or
    ///   install-location resolution happens at all, so the silent-WSL
    ///   hazard from #1121 is impossible by construction. The command
    ///   still runs in a **child process**, so Landlock, timeouts,
    ///   `kill_on_drop`, env scrubbing, the pwd marker, the
    ///   destructive-command classifier, and [`ShellPermissions`] all
    ///   apply unchanged.
    ///
    /// `builtin` is opt-in while brush compatibility is validated
    /// (notably on Windows). Note that brush interprets bash *syntax*
    /// but does not provide coreutils (`grep`, `sed`, `tail`, ...) —
    /// external commands are still spawned from `PATH` by the child.
    ///
    /// **Config-file-only** (`[tools].shell_engine` in `alms.toml`) —
    /// like [`shell_path`](Self::shell_path), never mutable via
    /// `PATCH /settings`.
    #[serde(default)]
    pub shell_engine: ShellEngine,
    /// Permission-based allow/deny list for shell commands.
    ///
    /// Regex patterns matched against command strings before execution.
    /// Can be configured globally here and/or per-agent (per-agent rules
    /// merge with global rules, with per-agent deny taking precedence).
    #[serde(default)]
    pub shell_permissions: ShellPermissions,

    /// Built-in risk classification mode for shell commands.
    ///
    /// Layers on top of [`shell_permissions`]: permissions = user policy,
    /// classification = built-in risk detection. Both must pass before a
    /// command is executed. See
    /// [`alms_sandbox::shell::classification`] for the full set of
    /// heuristics.
    ///
    /// Default: `block_destructive` (safe default that blocks `rm -rf /`,
    /// `sudo`, `mkfs`, reverse shells, etc. but allows normal dev workflows).
    #[serde(default)]
    pub shell_classification_mode: ShellClassificationMode,

    /// Per-tool settings for `fs_edit`.
    ///
    /// See [`FsEditConfig`] for the full set of knobs. Mirrors the
    /// `shell_permissions` shape: config-file-only, compiled once at
    /// process / agent startup, never mutable via `PATCH /settings`.
    #[serde(default)]
    pub fs_edit: FsEditConfig,

    /// Full-output spill-to-disk policy for the shell tool.
    ///
    /// When a shell command's stdout or stderr exceeds the head+tail
    /// truncation threshold, the full captured bytes are written to
    /// `{data_dir}/shell_output/{run_id}/shell_{tool_call_id}.log` and a
    /// `[full output spilled to: ...]` marker is appended to the tool result.
    /// The agent can then `fs_read` the spill path with offset/limit to
    /// inspect the middle of the output that truncation dropped.
    ///
    /// Like [`shell_permissions`], this is **config-file-only** and not
    /// mutable via `PATCH /settings` — the spill directory and retention
    /// window affect disk usage and are considered operator-level policy.
    #[serde(default)]
    pub shell_spill: ShellSpillConfig,

    /// Shared in-loop tool-output truncation policy (issue #851).
    ///
    /// The agent loop routes every tool's result through a single
    /// truncation service before pushing it into the live messages vec
    /// or persisting it to the session DB. Outputs larger than the byte
    /// or line cap are truncated to a head+tail preview, with the full
    /// pre-truncation bytes spilled to
    /// `{data_dir}/tool-output/{run_id}/tool_<tool_call_id>.txt` for
    /// recovery via `fs_read` / `fs_grep`. Mirrors `shell_spill` in
    /// shape and retention semantics, but applies to *every* tool —
    /// closing the multi-tool context-blowup hole reported in #851.
    ///
    /// Like [`shell_spill`], **config-file-only**.
    #[serde(default)]
    pub tool_output_truncate: ToolOutputTruncateConfig,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            enabled: Vec::new(),
            timeout_secs: 30,
            max_output_bytes: 65536,
            sandbox_root: ".".into(),
            shell_policy: "sandboxed".into(),
            shell_path: None,
            shell_engine: ShellEngine::default(),
            shell_permissions: ShellPermissions::default(),
            shell_classification_mode: ShellClassificationMode::default(),
            fs_edit: FsEditConfig::default(),
            shell_spill: ShellSpillConfig::default(),
            tool_output_truncate: ToolOutputTruncateConfig::default(),
        }
    }
}

/// Configuration for the `fs_edit` tool.
///
/// **Startup-only**: Like [`ShellPermissions`], `FsEditConfig` is compiled
/// into each `FsEditTool` instance at agent-construction time and is never
/// mutable via runtime APIs (`PATCH /settings`). To change the policy for
/// an agent, restart the process with a new `alms.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FsEditConfig {
    /// Enable the multi-stage fuzzy-match replacer cascade.
    ///
    /// When `false` (the default), `fs_edit` performs only its existing
    /// exact-match + uniqueness guard plus the curly-quote / CRLF
    /// normalization fallback. This preserves the "exact-string-only"
    /// contract that existing agents rely on.
    ///
    /// When `true`, two additional cascade stages run before the
    /// curly-quote / CRLF fallback (cheapest-first), catching common LLM
    /// foot-guns: trailing-whitespace drift on each line and
    /// leading-indent drift (e.g. the model emits 2-space indent while
    /// the file uses 4). The uniqueness guard still fires — if more
    /// than one candidate matches after any stage, `fs_edit` returns
    /// the same "ambiguous match" error as the exact path.
    ///
    /// Opt-in per agent; never silently on. See issue #755.
    pub fuzzy_match: bool,
}

/// Shell execution engine for the `shell` tool (#1143, Phase 2 of #1121).
///
/// Selects *how* `alms-sandbox` spawns the child process that evaluates a
/// shell command. Both engines run bash syntax in a sandboxed child; only
/// the interpreter binary differs. See
/// [`ToolsConfig::shell_engine`] for the full operator-facing contract.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShellEngine {
    /// Spawn an external `bash -c` (Phase 1 resolver from #1140).
    /// Default — byte-for-byte the pre-#1143 behavior.
    #[default]
    SystemBash,
    /// Re-exec `current_exe()` as `alms shell-host`, evaluating the
    /// command via the embedded `brush_core` bash interpreter.
    Builtin,
}

/// Built-in risk classification policy for the shell tool.
///
/// Mirrors `alms_sandbox::shell::classification::ClassificationMode`. Kept
/// here so that `alms-core` (which has no sandbox dependency) can own the
/// serde wire format; `alms-sandbox` converts this into its internal enum.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellClassificationMode {
    /// Classifier is disabled entirely.
    Off,
    /// Classifier runs and logs findings but never blocks.
    Warn,
    /// Block destructive commands, log moderate. Default.
    #[default]
    BlockDestructive,
    /// Block both moderate and destructive commands.
    Strict,
}

/// Permission-based allow/deny list for shell commands.
///
/// Patterns are regex strings matched against the full command string.
/// Evaluation order:
/// 1. If any `denied_commands` pattern matches, the command is blocked.
/// 2. If `allowed_commands` is non-empty, only commands matching at least
///    one pattern are permitted (allowlist mode).
/// 3. If `allowed_commands` is empty, all non-denied commands are allowed
///    (denylist-only mode).
///
/// **Startup-only**: Shell permissions are compiled from config at process
/// startup (or agent creation) and are not dynamically changeable via
/// `PATCH /settings`. This is intentional -- regex patterns are compiled
/// once into [`alms_sandbox::shell::permissions::CompiledPermissions`] and
/// baked into the `ShellTool` instance. Changing them at runtime would
/// require reconstructing the shell tool and re-registering it in the tool
/// registry, which is not currently supported.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellPermissions {
    /// Regex patterns for commands that are always allowed.
    ///
    /// When non-empty, acts as an allowlist: only commands matching at least
    /// one pattern are permitted. Empty means "allow everything not denied".
    pub allowed_commands: Vec<String>,

    /// Regex patterns for commands that are always denied.
    ///
    /// Deny takes precedence over allow. A command matching any deny pattern
    /// is blocked regardless of whether it also matches an allow pattern.
    pub denied_commands: Vec<String>,

    /// Regex patterns for commands that bypass the built-in risk classifier.
    ///
    /// The classifier is otherwise a non-bypassable floor: destructive findings
    /// (`rm -rf /`, `mkfs`, `sudo`, `curl | sh`, etc.) are blocked even when
    /// the allowlist accepts the command. Operators who have a legitimate need
    /// to run a flagged command (e.g. a specific `sudo apt-get update` line on
    /// a trusted host) may enumerate it here.
    ///
    /// Overrides bypass **only** the classifier. The `denied_commands` list
    /// still applies, and so does the OS-level sandbox (Landlock/cwd
    /// restrictions on Linux). An override pattern is explicit operator
    /// intent; it is never model-visible and cannot be added via any runtime
    /// API (including `PATCH /settings`).
    pub classifier_overrides: Vec<String>,
}

impl ShellPermissions {
    /// Returns true if no patterns are configured (no-op mode).
    pub fn is_empty(&self) -> bool {
        self.allowed_commands.is_empty()
            && self.denied_commands.is_empty()
            && self.classifier_overrides.is_empty()
    }

    /// Merge two permission sets. The `other` set (per-agent) extends the
    /// base set (global): denied patterns from both are combined, and
    /// per-agent allow patterns replace global allow patterns if non-empty.
    ///
    /// # SECURITY NOTE
    ///
    /// Deny patterns use union semantics (agents can only add restrictions),
    /// but allow patterns use **replace** semantics: a per-agent allowlist
    /// replaces the global allowlist entirely rather than intersecting with
    /// it. This means a per-agent config can widen access beyond the global
    /// policy (e.g. replacing `["^git\\b"]` with `[".*"]`).
    ///
    /// This is intentional for the current design where only operators set
    /// per-agent config via TOML. When per-agent permissions are exposed to
    /// less-trusted sources (e.g. an API), consider switching to intersection
    /// semantics for the allowlist to prevent privilege escalation.
    pub fn merge_with(&self, other: &ShellPermissions) -> ShellPermissions {
        // Denied: union of both sets (per-agent deny extends global deny)
        let mut denied = self.denied_commands.clone();
        denied.extend(other.denied_commands.iter().cloned());

        // Allowed: per-agent replaces global if non-empty, otherwise inherit global.
        // See SECURITY NOTE above regarding the replace semantics.
        let allowed = if other.allowed_commands.is_empty() {
            self.allowed_commands.clone()
        } else {
            other.allowed_commands.clone()
        };

        // Classifier overrides: union semantics (operator-only surface, same
        // trust level as denied_commands). If per-agent config is ever exposed
        // to a less-trusted source, flip to "global-only" to prevent
        // sub-agents from weakening the classifier floor.
        let mut classifier_overrides = self.classifier_overrides.clone();
        classifier_overrides.extend(other.classifier_overrides.iter().cloned());

        ShellPermissions {
            allowed_commands: allowed,
            denied_commands: denied,
            classifier_overrides,
        }
    }
}

// ---------------------------------------------------------------------------
// ShellSpillConfig
// ---------------------------------------------------------------------------

/// Configuration for the shell tool's large-output spill-to-disk behaviour.
///
/// When a shell command's output would exceed the head+tail truncation
/// threshold, the full captured bytes are written to a per-run directory
/// under `{data_dir}/shell_output/{run_id}/` so the agent can recover the
/// middle of the output via `fs_read`. The retention sweep runs once at
/// gateway startup (filesystem-mtime check; no SQLite tracking) and deletes
/// spill files older than [`retention_days`][Self::retention_days].
///
/// **Config-file-only**: mirrors the posture of
/// [`ShellPermissions`] — not exposed via `PATCH /settings`. Operators who
/// need to tune spill behaviour edit `alms.toml` and restart the gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellSpillConfig {
    /// Whether spill-to-disk is enabled. Default: `true`.
    ///
    /// When disabled, the shell tool behaves exactly as before this feature
    /// landed: output is head+tail truncated in place, and the middle of the
    /// stream is gone. Disable only if disk-usage concerns outweigh the
    /// debuggability benefit.
    pub enabled: bool,

    /// Number of days a spilled file is retained before the startup sweep
    /// deletes it. Default: `7`. A value of `0` disables retention (files
    /// are deleted at the next startup sweep regardless of age).
    pub retention_days: u32,
}

impl Default for ShellSpillConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            retention_days: 7,
        }
    }
}

// ---------------------------------------------------------------------------
// ToolOutputTruncateConfig
// ---------------------------------------------------------------------------

/// Configuration for the shared in-loop tool-output truncation service
/// (issue #851).
///
/// Every tool's result is routed through the truncation service before it
/// lands in the agent loop's live message list or the session DB. When the
/// result exceeds either [`max_bytes`][Self::max_bytes] or
/// [`max_lines`][Self::max_lines], a head+tail preview is returned to the
/// LLM and the full pre-truncation bytes are spilled to
/// `{data_dir}/tool-output/{run_id}/tool_<tool_call_id>.txt`. The retention
/// sweep runs once at gateway startup (filesystem-mtime check) and deletes
/// spill files older than [`retention_days`][Self::retention_days].
///
/// **Config-file-only**: like [`ShellSpillConfig`] / [`ShellPermissions`],
/// not exposed via `PATCH /settings`. Operators who need to tune truncation
/// edit `alms.toml` and restart the gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolOutputTruncateConfig {
    /// Whether the truncation service is enabled. Default: `true`.
    ///
    /// When disabled, the agent loop emits raw tool results into the
    /// LLM's context with no per-result cap. Disable only if you have
    /// per-tool caps tuned tightly enough to bound the worst case (and
    /// understand that `http_get` + `read_session` had no caps at all
    /// pre-#851).
    pub enabled: bool,

    /// Hard byte cap on the preview returned to the LLM. Outputs larger
    /// than this trigger truncation + spill. Default: `32_768` (32 KB).
    pub max_bytes: usize,

    /// Hard line cap on the preview returned to the LLM. Outputs with more
    /// than this many lines trigger truncation + spill, even if they fit
    /// inside `max_bytes`. Default: `2000`.
    pub max_lines: usize,

    /// Number of days a spilled file is retained before the startup sweep
    /// deletes it. Default: `7`. A value of `0` deletes every spill at the
    /// next startup sweep regardless of age.
    pub retention_days: u32,
}

impl Default for ToolOutputTruncateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_bytes: 32 * 1024,
            max_lines: 2000,
            retention_days: 7,
        }
    }
}

// ---------------------------------------------------------------------------
// SecurityConfig
// ---------------------------------------------------------------------------

/// Security policy knobs that operators set in `alms.toml` and that the
/// gateway must NOT accept via `PATCH /settings`.
///
/// **Threat model.** Every field on this struct widens the blast radius of
/// the daemon — a compromised auth token (or a misbehaving operator script)
/// must not be able to silently flip them. They are config-file-only,
/// loaded at startup, and never mirrored into `PersistedSettings`. The
/// gateway's `/settings` PATCH handler rejects any payload referencing a
/// field on this section with `400 SECURITY_KNOB_NOT_PATCHABLE`. Operators
/// who need to change one of these values edit `alms.toml` and restart the
/// daemon.
///
/// `GET /settings` MAY surface the values as read-only / informational; a
/// `SecurityConfig` field is never accepted on the inbound side.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    /// Operator-only escape hatch from the project-root sandbox boundary
    /// introduced in #945 (workspace v2).
    ///
    /// Names listed here name agents (by registry name) that should run
    /// **without** the project-root filesystem sandbox: `fs_*` tools have
    /// no path prefix to enforce, and the `shell` tool's persistent cwd is
    /// not pinned to the project root. These agents are subject only to
    /// the OS-level user permissions of the daemon process — `fs_read
    /// /etc/passwd` works (modulo file mode), `shell ls /` returns the
    /// real root.
    ///
    /// The two **independent** operator policies in
    /// [`ToolsConfig::shell_permissions`] (#717) and
    /// [`ToolsConfig::shell_classification_mode`] (#745) **still apply**
    /// to listed agents. They are defense-in-depth controls layered on
    /// top of the sandbox, not part of it.
    ///
    /// Worktree-mode `git` (when wired up — see #938) on a listed agent
    /// is silently ignored at runtime; this list wins. The gateway logs a
    /// startup-time `WARN` for every listed agent so operators see the
    /// precedence on boot. A second `WARN` fires at every `run_started`
    /// for the listed agent so log scanners can correlate runs against
    /// the loosened sandbox.
    ///
    /// **Config-file-only** — see the type-level docs on
    /// [`SecurityConfig`] for why this field is not `PATCH`-mutable.
    pub allow_full_os_access: Vec<String>,
}

impl SecurityConfig {
    /// Returns `true` when the named agent is on the
    /// [`Self::allow_full_os_access`] list.
    ///
    /// Comparison is exact-match on the registry name (the same string an
    /// operator types for `alms agent create --name <name>`). Empty input
    /// (an unnamed/ephemeral agent) never matches because empty entries
    /// in the list itself would be a configuration error caught by
    /// [`Self::validate`]. The check is O(n) — the list is short by
    /// design (a handful of operator-blessed agents), so a `HashSet` is
    /// not worth the allocation cost.
    pub fn is_full_os_access_agent(&self, agent_name: &str) -> bool {
        if agent_name.is_empty() {
            return false;
        }
        self.allow_full_os_access.iter().any(|n| n == agent_name)
    }
}

// ---------------------------------------------------------------------------
// ChannelsConfig
// ---------------------------------------------------------------------------

/// Channel configuration (Telegram, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelsConfig {
    /// Telegram bot token — loaded from secrets store at runtime.
    // TODO: wrap in a `Secret<String>` newtype that redacts Display/Debug output,
    // similar to `alms_channel::telegram::Secret`. Currently raw `String` here,
    // in `GatewayConfig`, and throughout the config layer — see PR #259 discussion.
    #[serde(skip)]
    pub telegram_token: Option<String>,
    pub telegram_poll_interval_secs: u64,
}

impl Default for ChannelsConfig {
    fn default() -> Self {
        Self {
            telegram_token: None,
            telegram_poll_interval_secs: 5,
        }
    }
}

// ---------------------------------------------------------------------------
// LoggingConfig
// ---------------------------------------------------------------------------

/// Logging configuration.
///
/// Controls file-based logging with daily rotation. Stderr output is always
/// active (level controlled by `RUST_LOG`). File output provides a persistent
/// debug-level log for post-hoc investigation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    /// Whether file-based logging is enabled. Default: true.
    /// Set to false (or env `ALMS_LOG_FILE_ENABLED=false`) to disable entirely.
    pub file_enabled: bool,
    /// Directory for log files. Defaults to `{data_dir}/logs/`.
    /// Set to override the default location, or `None` to use the default.
    pub log_dir: Option<String>,
    /// Log level for file output. Default: "debug".
    /// Accepts standard tracing levels: trace, debug, info, warn, error.
    pub file_level: String,
    /// Log rotation policy: "daily" (default), "hourly", or "never".
    pub rotation: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            file_enabled: true,
            log_dir: None,
            file_level: "debug".into(),
            rotation: "daily".into(),
        }
    }
}

impl LoggingConfig {
    /// Resolve the log directory path.
    ///
    /// Precedence: `logging.log_dir` config > `{data_dir}/logs/`.
    pub fn resolve_log_dir(&self, data_dir: &str) -> PathBuf {
        match &self.log_dir {
            Some(dir) => PathBuf::from(dir),
            None => Path::new(data_dir).join("logs"),
        }
    }
}
