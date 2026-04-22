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
    /// Bearer token for API authentication — loaded from env only
    #[serde(skip)]
    pub auth_token: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8080".into(),
            data_dir: "./.alms".into(),
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

    /// Return the resolved path to the agent workspace directory.
    ///
    /// Precedence: `ALMS_WORKSPACE_DIR` env var > `{data_dir}/workspace`.
    pub fn workspace_dir(&self) -> PathBuf {
        std::env::var("ALMS_WORKSPACE_DIR")
            .map(Into::into)
            .unwrap_or_else(|_| Path::new(&self.data_dir).join("workspace"))
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
    pub timeout_secs: u64,
    pub max_retries: u32,
    pub max_tokens_per_run: u32,
    pub mock: bool,
    /// Per-chunk read timeout for SSE streaming (seconds).
    /// If no data arrives within this window the stream is treated as stalled.
    /// Default: 60. Increase for slow reasoning models or high-latency connections.
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
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: "openrouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            model: "moonshotai/kimi-k2.5".into(),
            api_key: None,
            timeout_secs: 120,
            max_retries: 2,
            max_tokens_per_run: 0,
            mock: false,
            stream_chunk_timeout_secs: 60,
            providers: BTreeMap::new(),
            anthropic: AnthropicConfig::default(),
            openai: OpenAiConfig::default(),
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
            thinking_budget_tokens: 0,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextConfig {
    /// Strategy: "sliding-summary", "full", "truncate"
    pub strategy: String,
    /// Maximum tokens to send to the LLM in a single request.
    ///
    /// This is the **per-request** token budget for the context window assembled
    /// by the ContextBuilder. It should match your LLM's context window size.
    ///
    /// Not to be confused with [`SessionConfig::max_context_tokens`], which is
    /// the total token storage limit for the session's history on disk.
    pub max_input_tokens: usize,
    /// Number of recent messages to always keep in full
    pub recent_window: usize,
    /// How often to trigger a new summary (in uncovered messages beyond recent_window)
    pub summary_interval: usize,
    /// Separate (cheaper) model for generating summaries.
    /// Falls back to the agent's default model when `None`.
    /// Defaults to `minimax/minimax-m2.7` to avoid wasting tokens on
    /// reasoning models that spend most of their budget on thinking.
    pub summary_model: Option<String>,
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
            recent_window: 20,
            summary_interval: 30,
            summary_model: Some("minimax/minimax-m2.7".into()),
            run_summary_mode: RunSummaryMode::Llm,
            run_summary_budget: 2000,
            summary_max_tokens: 1000,
        }
    }
}

impl ContextConfig {
    /// Normalize episodic memory settings: validate mode and enforce the 15%
    /// budget cap. Called during config loading, before hard validation.
    pub fn normalize_episodic(&mut self) {
        // Normalize Unknown variant (from unrecognized TOML/env values) to Llm
        if self.run_summary_mode == RunSummaryMode::Unknown {
            warn!("Unrecognized run_summary_mode, falling back to \"llm\"");
            self.run_summary_mode = RunSummaryMode::Llm;
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
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            enabled: Vec::new(),
            timeout_secs: 30,
            max_output_bytes: 65536,
            sandbox_root: ".".into(),
            shell_policy: "sandboxed".into(),
            shell_permissions: ShellPermissions::default(),
            shell_classification_mode: ShellClassificationMode::default(),
            fs_edit: FsEditConfig::default(),
            shell_spill: ShellSpillConfig::default(),
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
