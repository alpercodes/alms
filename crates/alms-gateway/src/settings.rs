//! Settings endpoint — exposes server-side LLM defaults for UI pre-population.

use crate::server::AppState;
use axum::{Json, extract::State, response::IntoResponse};

/// GET /settings — returns current server defaults for UI pre-population.
pub async fn get_settings(State(state): State<AppState>) -> impl IntoResponse {
    let llm = &state.llm_config;
    let agent = &state.agent_config;
    let posture_str = agent.posture.to_string();

    // Builtin tools: report only tools that are actually registered (intersection
    // of the enabled list with known builtins). Typos in enabled are excluded.
    let enabled = &state.agent_config.enabled_tools;
    let all_builtins: &[&str] = &[
        "echo",
        "fs_list",
        "fs_read",
        "fs_write",
        "http_get",
        "math",
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
    let sess = &state.session_config;
    let log = &state.logging_config;
    let tools_cfg = &state.tools_config;

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
