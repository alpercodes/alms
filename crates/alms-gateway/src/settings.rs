//! Settings endpoint — exposes server-side LLM defaults for UI pre-population.

use crate::server::AppState;
use alms_runtime::Posture;
use axum::{Json, extract::State, response::IntoResponse};

/// GET /settings — returns current server defaults for UI pre-population.
pub async fn get_settings(State(state): State<AppState>) -> impl IntoResponse {
    let gateway = state.gateway.lock().await;
    let llm = gateway.llm_config();
    let agent = gateway.agent_config();
    let posture_str = match agent.posture {
        Posture::FullControl => "full_control",
        Posture::Guarded => "guarded",
    };

    // List tools by name directly — avoids constructing a full ToolRegistry per request.
    let mut tools: Vec<&str> = vec![
        "echo",
        "fs_list",
        "fs_read",
        "fs_write",
        "get_task_result",
        "http_get",
        "invoke_agent",
        "math",
        "shell_exec",
    ];
    if state.workspace_dir.is_some() {
        tools.push("workspace_write");
    }

    let agent_id = gateway.agent_id().0.to_string();

    let agents_list = state
        .session_manager
        .store()
        .and_then(|s| s.list_agents().ok())
        .unwrap_or_default()
        .into_iter()
        .map(|a| {
            serde_json::json!({
                "name": a.name,
                "id": a.id.0.to_string(),
                "is_default": a.is_default,
                "model": a.model,
            })
        })
        .collect::<Vec<_>>();

    let workspace_dir = state
        .workspace_dir
        .as_ref()
        .map(|p| p.display().to_string());

    Json(serde_json::json!({
        "model": llm.default_model,
        "base_url": llm.base_url,
        "temperature": agent.temperature,
        "max_tokens": agent.max_tokens,
        "posture": posture_str,
        "context_strategy": agent.context_config.strategy,
        "enabled_tools": tools,
        "agent_id": agent_id,
        "agents": agents_list,
        "workspace_dir": workspace_dir,
    }))
}
