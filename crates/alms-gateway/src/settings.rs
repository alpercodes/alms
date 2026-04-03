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

/// On-disk representation of the mutable server-level settings.
///
/// Written to `{data_dir}/settings.json` after every PATCH /settings and
/// loaded on startup to restore the previous configuration.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct PersistedSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<alms_core::config::ContextConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<alms_session::SessionConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<alms_core::config::ToolsConfig>,
}

/// Return the canonical path for the persisted settings file.
pub fn settings_path(data_dir: &Path) -> PathBuf {
    data_dir.join("settings.json")
}

/// Write the current mutable settings to `{data_dir}/settings.json`.
fn persist_settings(state: &AppState) {
    let persisted = PersistedSettings {
        context: Some(state.agent_config.read().context_config.clone()),
        session: Some(state.session_config.read().clone()),
        tools: Some(state.tools_config.read().clone()),
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
