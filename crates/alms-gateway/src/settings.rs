//! Settings endpoints — exposes server-side defaults for UI pre-population
//! and accepts partial config updates via PATCH.
//!
//! Server-level settings (context, session, tools) are persisted to
//! `{data_dir}/settings.json` so they survive restarts.

use crate::server::AppState;
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::info;

/// GET /settings — returns current server defaults for UI pre-population.
pub async fn get_settings(State(state): State<AppState>) -> impl IntoResponse {
    let llm = &state.llm_config;
    let agent = state.agent_config.read().clone();
    let posture_str = agent.posture.to_string();

    // Builtin tools: report only tools that are actually registered (intersection
    // of the enabled list with known builtins). Typos in enabled are excluded.
    let enabled = &agent.enabled_tools;
    let all_builtins: &[&str] = &[
        "echo",
        "fs_edit",
        "fs_grep",
        "fs_list",
        "fs_read",
        "fs_write",
        "http_get",
        "math",
        "shell",
        "shell_exec",
    ];
    let mut tools: Vec<String> = if enabled.is_empty() {
        all_builtins.iter().map(|s| String::from(*s)).collect()
    } else {
        enabled
            .iter()
            .filter(|e| all_builtins.contains(&e.as_str()))
            .cloned()
            .collect()
    };
    // Runtime-added tools (agent infrastructure, not subject to enabled filter)
    tools.extend(
        [
            "invoke_agent",
            "read_subagent_session",
            "read_session",
            "send_message",
            "list_agents",
            "read_messages",
            "ignore_message",
            "list_my_sessions",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    if state.workspace_dir.is_some() {
        tools.push("workspace_write".to_string());
    }

    let agent_id = state.default_agent_id.read().0.to_string();

    let agents_list = state
        .session_manager
        .store()
        .and_then(|s| match s.list_agents() {
            Ok(agents) => Some(agents),
            Err(e) => {
                tracing::warn!("Failed to list agents for /settings: {e}");
                None
            }
        })
        .unwrap_or_default()
        .into_iter()
        .map(|a| {
            let bootstrap = state
                .workspace_dir
                .as_ref()
                .map(|ws| alms_runtime::AgentWorkspace::new(ws, &a.name).needs_bootstrap())
                .unwrap_or(false);
            serde_json::json!({
                "name": a.name,
                "id": a.id.0.to_string(),
                "is_default": a.is_default,
                "model": a.model,
                "needs_bootstrap": bootstrap,
            })
        })
        .collect::<Vec<_>>();

    let workspace_dir = state
        .workspace_dir
        .as_ref()
        .map(|p| p.display().to_string());

    let ctx = &agent.context_config;
    let sess = state.session_config.read().clone();
    let log = &state.logging_config;
    let tools_cfg = state.tools_config.read().clone();

    Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "provider": llm.provider,
        "model": llm.default_model,
        "base_url": llm.base_url,
        "max_tokens": agent.max_tokens,
        "posture": posture_str,
        "context_strategy": ctx.strategy,
        "stream_chunk_timeout_secs": llm.stream_chunk_timeout_secs,
        "enabled_tools": tools,
        "agent_id": agent_id,
        "agents": agents_list,
        "workspace_dir": workspace_dir,
        // Context settings
        "context": {
            "strategy": ctx.strategy,
            "max_input_tokens": ctx.max_input_tokens,
            "recent_window": ctx.recent_window,
            "summary_interval": ctx.summary_interval,
            "summary_model": ctx.summary_model,
            "run_summary_mode": ctx.run_summary_mode.to_string(),
            "run_summary_budget": ctx.run_summary_budget,
        },
        // Session settings
        "session": {
            "max_messages": sess.max_messages,
            "max_context_tokens": sess.max_context_tokens,
            "idle_timeout_secs": sess.idle_timeout_secs,
            "auto_archive": sess.auto_archive,
            "archive_ttl_secs": sess.archive_ttl_secs,
        },
        // Logging settings
        "logging": {
            "file_enabled": log.file_enabled,
            "file_level": log.file_level,
            "rotation": log.rotation,
            "log_dir": log.log_dir,
        },
        // Tools settings
        "tools": {
            "sandbox_root": tools_cfg.sandbox_root,
            "shell_policy": tools_cfg.shell_policy,
            "timeout_secs": tools_cfg.timeout_secs,
            "max_output_bytes": tools_cfg.max_output_bytes,
            "enabled": tools,
        },
    }))
}

// ── PATCH /settings — partial config update ────────────────────────────

/// Partial context config update.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct PatchContext {
    pub strategy: Option<String>,
    pub max_input_tokens: Option<usize>,
    pub recent_window: Option<usize>,
    pub summary_interval: Option<usize>,
    pub summary_model: Option<String>,
}

/// Partial session config update.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct PatchSession {
    pub max_messages: Option<usize>,
    pub max_context_tokens: Option<usize>,
    pub idle_timeout_secs: Option<u64>,
    pub auto_archive: Option<bool>,
    pub archive_ttl_secs: Option<u64>,
}

/// Partial tools config update.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct PatchTools {
    pub shell_policy: Option<String>,
    pub sandbox_root: Option<String>,
    pub timeout_secs: Option<u64>,
    pub max_output_bytes: Option<usize>,
}

/// Top-level PATCH /settings request body.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct PatchSettingsRequest {
    pub context: Option<PatchContext>,
    pub session: Option<PatchSession>,
    pub tools: Option<PatchTools>,
}

/// PATCH /settings — apply partial config updates to the running server.
///
/// Only context, session, and tools sections are mutable at runtime.
/// Logging requires a restart and is not accepted here.
/// Changes take effect on the next run (in-flight runs are unaffected).
pub async fn patch_settings(
    State(state): State<AppState>,
    Json(body): Json<PatchSettingsRequest>,
) -> impl IntoResponse {
    let mut errors: Vec<String> = Vec::new();

    // ── Context ────────────────────────────────────────────────────────
    if let Some(ctx_patch) = &body.context {
        let mut agent = state.agent_config.write();
        let ctx = &mut agent.context_config;

        if let Some(ref strategy) = ctx_patch.strategy {
            let valid = ["sliding-summary", "full", "truncate"];
            if valid.contains(&strategy.as_str()) {
                ctx.strategy = strategy.clone();
            } else {
                errors.push(format!(
                    "context.strategy must be one of {valid:?}, got '{strategy}'"
                ));
            }
        }
        if let Some(v) = ctx_patch.max_input_tokens {
            if v == 0 {
                errors.push("context.max_input_tokens must be > 0".into());
            } else {
                ctx.max_input_tokens = v;
            }
        }
        if let Some(v) = ctx_patch.recent_window {
            if v == 0 {
                errors.push("context.recent_window must be > 0".into());
            } else {
                ctx.recent_window = v;
            }
        }
        if let Some(v) = ctx_patch.summary_interval {
            ctx.summary_interval = v;
        }
        if let Some(ref v) = ctx_patch.summary_model {
            if v.is_empty() {
                ctx.summary_model = None;
            } else {
                ctx.summary_model = Some(v.clone());
            }
        }

        info!(
            strategy = %ctx.strategy,
            max_input_tokens = ctx.max_input_tokens,
            recent_window = ctx.recent_window,
            summary_interval = ctx.summary_interval,
            "Updated context config via PATCH /settings"
        );
    }

    // ── Session ────────────────────────────────────────────────────────
    if let Some(sess_patch) = &body.session {
        let mut sess = state.session_config.write();

        if let Some(v) = sess_patch.max_messages {
            sess.max_messages = v;
        }
        if let Some(v) = sess_patch.max_context_tokens {
            sess.max_context_tokens = v;
        }
        if let Some(v) = sess_patch.idle_timeout_secs {
            sess.idle_timeout_secs = v;
        }
        if let Some(v) = sess_patch.auto_archive {
            sess.auto_archive = v;
        }
        if let Some(v) = sess_patch.archive_ttl_secs {
            sess.archive_ttl_secs = v;
        }

        // Cross-section validation: session storage must hold at least one
        // full context window.
        let ctx_max = state.agent_config.read().context_config.max_input_tokens;
        if sess.max_context_tokens < ctx_max {
            errors.push(format!(
                "session.max_context_tokens ({}) must be >= context.max_input_tokens ({ctx_max})",
                sess.max_context_tokens,
            ));
            // Revert to safe value
            sess.max_context_tokens = ctx_max;
        }

        info!(
            max_messages = sess.max_messages,
            max_context_tokens = sess.max_context_tokens,
            idle_timeout_secs = sess.idle_timeout_secs,
            auto_archive = sess.auto_archive,
            "Updated session config via PATCH /settings"
        );
    }

    // ── Tools ──────────────────────────────────────────────────────────
    if let Some(tools_patch) = &body.tools {
        let mut tools = state.tools_config.write();

        if let Some(ref policy) = tools_patch.shell_policy {
            let valid = ["sandboxed", "unrestricted"];
            if valid.contains(&policy.as_str()) {
                tools.shell_policy = policy.clone();
                // Also update the agent_config copy so runs pick it up.
                state.agent_config.write().shell_policy = policy.clone();
            } else {
                errors.push(format!(
                    "tools.shell_policy must be one of {valid:?}, got '{policy}'"
                ));
            }
        }
        if let Some(ref root) = tools_patch.sandbox_root {
            tools.sandbox_root = root.clone();
            state.agent_config.write().sandbox_root = root.clone();
        }
        if let Some(v) = tools_patch.timeout_secs {
            if v == 0 {
                errors.push("tools.timeout_secs must be > 0".into());
            } else {
                tools.timeout_secs = v;
            }
        }
        if let Some(v) = tools_patch.max_output_bytes {
            tools.max_output_bytes = v;
        }

        info!(
            shell_policy = %tools.shell_policy,
            timeout_secs = tools.timeout_secs,
            max_output_bytes = tools.max_output_bytes,
            "Updated tools config via PATCH /settings"
        );
    }

    // Persist current settings to disk so they survive restarts.
    persist_settings(&state);

    if errors.is_empty() {
        (StatusCode::OK, Json(serde_json::json!({ "status": "ok" })))
    } else {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "status": "partial",
                "errors": errors,
            })),
        )
    }
}

// ── Persistence helpers ───────────────────────────────────────────────

/// Persisted context overrides — only fields the user explicitly changed
/// via PATCH /settings are `Some`. Fields that are `None` fall through to
/// code defaults / TOML / env-var overrides.
///
/// Backward-compatible: old `settings.json` files that contain the full
/// `ContextConfig` will deserialize with all fields set to `Some`, which
/// is functionally equivalent to the old wholesale-replace behavior (but
/// new persists will only write the patchable subset).
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PersistedContextOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recent_window: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_interval: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_summary_mode: Option<alms_core::config::RunSummaryMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_summary_budget: Option<usize>,
}

impl PersistedContextOverrides {
    /// Merge these overrides into a `ContextConfig`, field by field.
    /// Only fields that are `Some` overwrite the target.
    pub fn apply_to(&self, ctx: &mut alms_core::config::ContextConfig) {
        if let Some(ref v) = self.strategy {
            ctx.strategy = v.clone();
        }
        if let Some(v) = self.max_input_tokens {
            ctx.max_input_tokens = v;
        }
        if let Some(v) = self.recent_window {
            ctx.recent_window = v;
        }
        if let Some(v) = self.summary_interval {
            ctx.summary_interval = v;
        }
        if self.summary_model.is_some() {
            ctx.summary_model = self.summary_model.clone();
        }
        if let Some(ref v) = self.run_summary_mode {
            ctx.run_summary_mode = v.clone();
        }
        if let Some(v) = self.run_summary_budget {
            ctx.run_summary_budget = v;
        }
    }
}

/// Persisted session overrides — only fields the user explicitly changed.
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PersistedSessionOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_messages: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_timeout_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_archive: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_ttl_secs: Option<u64>,
}

impl PersistedSessionOverrides {
    /// Merge these overrides into a `SessionConfig`, field by field.
    pub fn apply_to(&self, sess: &mut alms_session::SessionConfig) {
        if let Some(v) = self.max_messages {
            sess.max_messages = v;
        }
        if let Some(v) = self.max_context_tokens {
            sess.max_context_tokens = v;
        }
        if let Some(v) = self.idle_timeout_secs {
            sess.idle_timeout_secs = v;
        }
        if let Some(v) = self.auto_archive {
            sess.auto_archive = v;
        }
        if let Some(v) = self.archive_ttl_secs {
            sess.archive_ttl_secs = v;
        }
    }
}

/// Persisted tools overrides — only fields the user explicitly changed.
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PersistedToolsOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_bytes: Option<usize>,
}

impl PersistedToolsOverrides {
    /// Merge these overrides into a `ToolsConfig`, field by field.
    pub fn apply_to(&self, tools: &mut alms_core::config::ToolsConfig) {
        if let Some(ref v) = self.shell_policy {
            tools.shell_policy = v.clone();
        }
        if let Some(ref v) = self.sandbox_root {
            tools.sandbox_root = v.clone();
        }
        if let Some(v) = self.timeout_secs {
            tools.timeout_secs = v;
        }
        if let Some(v) = self.max_output_bytes {
            tools.max_output_bytes = v;
        }
    }
}

/// On-disk representation of the mutable server-level settings.
///
/// Written to `{data_dir}/settings.json` after every PATCH /settings and
/// loaded on startup to restore the previous configuration.
///
/// Only fields explicitly set by the user via PATCH /settings are persisted.
/// On load, these overrides are merged field-by-field into the resolved
/// config (code defaults + TOML + env vars), ensuring that:
/// - Code default changes are picked up for non-overridden fields
/// - Env var overrides (e.g. `ALMS_RUN_SUMMARY_MODE`) still take precedence
///   when they exist
///
/// Backward-compatible: old `settings.json` files with the full
/// `ContextConfig` / `SessionConfig` / `ToolsConfig` structs will
/// deserialize correctly because `serde(default)` fills in `None` for
/// missing fields, and extra fields in the JSON are silently ignored.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct PersistedSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<PersistedContextOverrides>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<PersistedSessionOverrides>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<PersistedToolsOverrides>,
}

/// Return the canonical path for the persisted settings file.
pub fn settings_path(data_dir: &Path) -> PathBuf {
    data_dir.join("settings.json")
}

/// Write the current user-overridden settings to `{data_dir}/settings.json`.
///
/// Only persists the fields that PATCH /settings can modify — never writes
/// fields like `run_summary_mode` or `run_summary_budget` that are not
/// exposed in the PATCH API, so that code-default and env-var changes to
/// those fields are always respected on restart.
fn persist_settings(state: &AppState) {
    let agent = state.agent_config.read();
    let ctx = &agent.context_config;
    let sess = state.session_config.read();
    let tools = state.tools_config.read();

    let persisted = PersistedSettings {
        // Only persist the fields that PATCH /settings exposes for context.
        context: Some(PersistedContextOverrides {
            strategy: Some(ctx.strategy.clone()),
            max_input_tokens: Some(ctx.max_input_tokens),
            recent_window: Some(ctx.recent_window),
            summary_interval: Some(ctx.summary_interval),
            summary_model: ctx.summary_model.clone(),
            // run_summary_mode and run_summary_budget are NOT exposed in
            // PATCH /settings, so we never persist them. This ensures that
            // code-default changes and env-var overrides are always respected.
            run_summary_mode: None,
            run_summary_budget: None,
        }),
        session: Some(PersistedSessionOverrides {
            max_messages: Some(sess.max_messages),
            max_context_tokens: Some(sess.max_context_tokens),
            idle_timeout_secs: Some(sess.idle_timeout_secs),
            auto_archive: Some(sess.auto_archive),
            archive_ttl_secs: Some(sess.archive_ttl_secs),
        }),
        tools: Some(PersistedToolsOverrides {
            shell_policy: Some(tools.shell_policy.clone()),
            sandbox_root: Some(tools.sandbox_root.clone()),
            timeout_secs: Some(tools.timeout_secs),
            max_output_bytes: Some(tools.max_output_bytes),
        }),
    };

    let path = settings_path(&state.data_dir);
    match serde_json::to_string_pretty(&persisted) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Failed to persist settings to disk"
                );
            } else {
                tracing::debug!(path = %path.display(), "Persisted settings to disk");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to serialize settings for persistence");
        }
    }
}

/// Load persisted settings from disk. Returns `None` if the file does not
/// exist or cannot be parsed (a warning is logged in the latter case).
pub fn load_persisted_settings(data_dir: &Path) -> Option<PersistedSettings> {
    let path = settings_path(data_dir);
    match std::fs::read_to_string(&path) {
        Ok(json) => match serde_json::from_str::<PersistedSettings>(&json) {
            Ok(s) => {
                info!(path = %path.display(), "Loaded persisted settings from disk");
                Some(s)
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Failed to parse persisted settings — using defaults"
                );
                None
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to read persisted settings — using defaults"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulate the old settings.json format (full ContextConfig with all fields
    /// present). Verify backward compatibility: all fields deserialize as `Some`.
    #[test]
    fn backward_compat_old_settings_json_with_full_context_config() {
        let old_json = r#"{
            "context": {
                "strategy": "truncate",
                "max_input_tokens": 128000,
                "run_summary_mode": "off",
                "run_summary_budget": 2000,
                "recent_window": 20,
                "summary_interval": 30,
                "summary_model": null
            }
        }"#;
        let persisted: PersistedSettings = serde_json::from_str(old_json).unwrap();
        let ctx = persisted.context.unwrap();
        assert_eq!(ctx.strategy, Some("truncate".into()));
        assert_eq!(ctx.max_input_tokens, Some(128_000));
        assert_eq!(
            ctx.run_summary_mode,
            Some(alms_core::config::RunSummaryMode::Off)
        );
        assert_eq!(ctx.run_summary_budget, Some(2000));
    }

    /// New settings.json format omits run_summary_mode and run_summary_budget.
    /// Verify they deserialize as `None`.
    #[test]
    fn new_format_omits_non_patchable_fields() {
        let new_json = r#"{
            "context": {
                "strategy": "truncate",
                "max_input_tokens": 128000,
                "recent_window": 20,
                "summary_interval": 30
            }
        }"#;
        let persisted: PersistedSettings = serde_json::from_str(new_json).unwrap();
        let ctx = persisted.context.unwrap();
        assert_eq!(ctx.strategy, Some("truncate".into()));
        assert_eq!(ctx.max_input_tokens, Some(128_000));
        // Non-patchable fields should be None
        assert_eq!(ctx.run_summary_mode, None);
        assert_eq!(ctx.run_summary_budget, None);
    }

    /// Context overrides with `None` fields should not overwrite the target.
    #[test]
    fn context_overrides_apply_only_some_fields() {
        let mut ctx = alms_core::config::ContextConfig::default();
        // Default run_summary_mode is Llm, run_summary_budget is 2000
        assert_eq!(ctx.run_summary_mode, alms_core::config::RunSummaryMode::Llm);
        assert_eq!(ctx.run_summary_budget, 2000);

        let overrides = PersistedContextOverrides {
            strategy: Some("sliding-summary".into()),
            max_input_tokens: Some(64_000),
            // Leave everything else as None — should not overwrite
            ..Default::default()
        };
        overrides.apply_to(&mut ctx);

        assert_eq!(ctx.strategy, "sliding-summary");
        assert_eq!(ctx.max_input_tokens, 64_000);
        // These should be UNCHANGED from defaults
        assert_eq!(
            ctx.run_summary_mode,
            alms_core::config::RunSummaryMode::Llm,
            "run_summary_mode should not be overwritten when override is None"
        );
        assert_eq!(
            ctx.run_summary_budget, 2000,
            "run_summary_budget should not be overwritten when override is None"
        );
        assert_eq!(ctx.recent_window, 20);
        assert_eq!(ctx.summary_interval, 30);
    }

    /// Session overrides with partial fields should only overwrite `Some` fields.
    #[test]
    fn session_overrides_apply_only_some_fields() {
        let mut sess = alms_session::SessionConfig::default();
        let original_idle = sess.idle_timeout_secs;

        let overrides = PersistedSessionOverrides {
            max_messages: Some(500),
            // Leave everything else as None
            ..Default::default()
        };
        overrides.apply_to(&mut sess);

        assert_eq!(sess.max_messages, 500);
        assert_eq!(
            sess.idle_timeout_secs, original_idle,
            "idle_timeout should be unchanged"
        );
    }

    /// Tools overrides with partial fields should only overwrite `Some` fields.
    #[test]
    fn tools_overrides_apply_only_some_fields() {
        let mut tools = alms_core::config::ToolsConfig::default();
        let original_timeout = tools.timeout_secs;

        let overrides = PersistedToolsOverrides {
            shell_policy: Some("unrestricted".into()),
            ..Default::default()
        };
        overrides.apply_to(&mut tools);

        assert_eq!(tools.shell_policy, "unrestricted");
        assert_eq!(
            tools.timeout_secs, original_timeout,
            "timeout should be unchanged"
        );
    }

    /// Serialization of new format should NOT include run_summary_mode or
    /// run_summary_budget when they are None.
    #[test]
    fn serialized_new_format_excludes_non_patchable_fields() {
        let persisted = PersistedSettings {
            context: Some(PersistedContextOverrides {
                strategy: Some("truncate".into()),
                max_input_tokens: Some(128_000),
                recent_window: Some(20),
                summary_interval: Some(30),
                summary_model: None,
                run_summary_mode: None,
                run_summary_budget: None,
            }),
            session: None,
            tools: None,
        };
        let json = serde_json::to_string_pretty(&persisted).unwrap();
        assert!(
            !json.contains("run_summary_mode"),
            "Serialized JSON should not contain run_summary_mode"
        );
        assert!(
            !json.contains("run_summary_budget"),
            "Serialized JSON should not contain run_summary_budget"
        );
    }

    /// Round-trip: serialize then deserialize preserves values.
    #[test]
    fn round_trip_serialization() {
        let original = PersistedSettings {
            context: Some(PersistedContextOverrides {
                strategy: Some("sliding-summary".into()),
                max_input_tokens: Some(64_000),
                recent_window: Some(10),
                summary_interval: Some(15),
                summary_model: Some("gpt-4o-mini".into()),
                run_summary_mode: None,
                run_summary_budget: None,
            }),
            session: Some(PersistedSessionOverrides {
                max_messages: Some(5000),
                max_context_tokens: Some(200_000),
                idle_timeout_secs: Some(3600),
                auto_archive: Some(false),
                archive_ttl_secs: Some(86400),
            }),
            tools: Some(PersistedToolsOverrides {
                shell_policy: Some("unrestricted".into()),
                sandbox_root: Some("/tmp".into()),
                timeout_secs: Some(60),
                max_output_bytes: Some(1_000_000),
            }),
        };
        let json = serde_json::to_string_pretty(&original).unwrap();
        let deserialized: PersistedSettings = serde_json::from_str(&json).unwrap();

        let ctx = deserialized.context.unwrap();
        assert_eq!(ctx.strategy, Some("sliding-summary".into()));
        assert_eq!(ctx.max_input_tokens, Some(64_000));
        assert_eq!(ctx.summary_model, Some("gpt-4o-mini".into()));
        assert_eq!(ctx.run_summary_mode, None);
        assert_eq!(ctx.run_summary_budget, None);

        let sess = deserialized.session.unwrap();
        assert_eq!(sess.max_messages, Some(5000));

        let tools = deserialized.tools.unwrap();
        assert_eq!(tools.shell_policy, Some("unrestricted".into()));
    }

    /// Empty JSON should deserialize to default PersistedSettings (all None).
    #[test]
    fn empty_json_deserializes_to_defaults() {
        let persisted: PersistedSettings = serde_json::from_str("{}").unwrap();
        assert!(persisted.context.is_none());
        assert!(persisted.session.is_none());
        assert!(persisted.tools.is_none());
    }

    /// Old format with full SessionConfig should still deserialize.
    #[test]
    fn backward_compat_old_session_config() {
        let old_json = r#"{
            "session": {
                "idle_timeout_secs": 86400,
                "auto_archive": true,
                "archive_ttl_secs": 2592000,
                "max_messages": 10000,
                "max_context_tokens": 128000
            }
        }"#;
        let persisted: PersistedSettings = serde_json::from_str(old_json).unwrap();
        let sess = persisted.session.unwrap();
        assert_eq!(sess.max_messages, Some(10_000));
        assert_eq!(sess.idle_timeout_secs, Some(86_400));
        assert_eq!(sess.auto_archive, Some(true));
    }
}
