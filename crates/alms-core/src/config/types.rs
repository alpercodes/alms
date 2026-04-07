//! Configuration struct definitions and their [`Default`] implementations.
//!
//! Each struct corresponds to a TOML section in `alms.toml`.  Methods that
//! operate on a single config section (e.g. `ServerConfig::db_path`,
//! `ContextConfig::normalize_episodic`, `LoggingConfig::resolve_log_dir`)
//! live here alongside their struct.

use serde::{Deserialize, Serialize};
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
    /// Base directory for ALMS data files (SQLite DB, workspace, secrets).
    /// Override with `ALMS_DATA_DIR` env var. Default: `./data`.
    pub data_dir: String,
    /// Bearer token for API authentication — loaded from env only
    #[serde(skip)]
    pub auth_token: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8080".into(),
            data_dir: "./data".into(),
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
    /// Provider: "openrouter" (default), "openai", or "anthropic"
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
        }
    }
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
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            enabled: Vec::new(),
            timeout_secs: 30,
            max_output_bytes: 65536,
            sandbox_root: ".".into(),
            shell_policy: "sandboxed".into(),
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
