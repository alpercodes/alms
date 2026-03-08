//! Settings endpoint — exposes server-side LLM defaults for UI pre-population.

use crate::server::AppState;
use axum::{Json, extract::State, response::IntoResponse};

/// GET /settings — returns current server LLM defaults
pub async fn get_settings(State(state): State<AppState>) -> impl IntoResponse {
    let gateway = state.gateway.lock().await;
    let llm = gateway.llm_config();
    Json(serde_json::json!({
        "model": llm.default_model,
        "base_url": llm.base_url,
    }))
}
