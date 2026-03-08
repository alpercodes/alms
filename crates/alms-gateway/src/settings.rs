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

    // Base builtin tools always available; workspace_write added when workspace is configured.
    let mut tools = alms_runtime::ToolRegistry::with_builtins().list();
    if state.workspace_dir.is_some() {
        tools.push("workspace_write".to_string());
    }
    tools.sort();

    Json(serde_json::json!({
        "model": llm.default_model,
        "base_url": llm.base_url,
        "temperature": agent.temperature,
        "max_tokens": agent.max_tokens,
        "posture": posture_str,
        "context_strategy": agent.context_config.strategy,
        "enabled_tools": tools,
    }))
}
