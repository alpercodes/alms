//! HTTP handlers for API key management.
//!
//! GET    /auth/keys           — list providers with keys (masked)
//! PUT    /auth/keys           — set a key for a provider
//! DELETE /auth/keys/{provider} — remove a key

use crate::api_error;
use crate::server::AppState;
use alms_core::secrets::{SecretsStore, VALID_PROVIDERS};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;

/// GET /auth/keys — list which providers have keys (masked values only).
pub async fn list_keys(State(state): State<AppState>) -> impl IntoResponse {
    let secrets = state.secrets.read().unwrap();
    let keys: Vec<serde_json::Value> = VALID_PROVIDERS
        .iter()
        .map(|p| {
            let stored = secrets.get_masked(p);
            let env_key = alms_core::config::select_llm_api_key_from_env(p);
            let source = if stored.is_some() {
                "secrets"
            } else if env_key.is_some() {
                "env"
            } else {
                "none"
            };
            let masked = stored.or_else(|| env_key.map(|k| SecretsStore::masked_key(&k)));
            serde_json::json!({
                "provider": p,
                "configured": masked.is_some(),
                "key": masked,
                "source": source,
            })
        })
        .collect();
    Json(serde_json::json!({ "keys": keys }))
}

#[derive(Debug, Deserialize)]
pub struct SetKeyRequest {
    pub provider: String,
    pub key: String,
}

/// PUT /auth/keys — set an API key for a provider.
pub async fn set_key(
    State(state): State<AppState>,
    Json(req): Json<SetKeyRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if !VALID_PROVIDERS.contains(&req.provider.as_str()) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "INVALID_PROVIDER",
            format!(
                "Unknown provider '{}'. Must be one of: {}",
                req.provider,
                VALID_PROVIDERS.join(", ")
            ),
        ));
    }
    if req.key.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "EMPTY_KEY",
            "API key cannot be empty",
        ));
    }

    let masked = SecretsStore::masked_key(&req.key);
    state
        .secrets
        .write()
        .unwrap()
        .set_key(&req.provider, &req.key)
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", e))?;

    Ok(Json(serde_json::json!({
        "ok": true,
        "provider": req.provider,
        "key": masked,
    })))
}

/// DELETE /auth/keys/{provider} — remove a stored API key.
pub async fn remove_key(
    State(state): State<AppState>,
    Path(provider): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if !VALID_PROVIDERS.contains(&provider.as_str()) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "INVALID_PROVIDER",
            format!(
                "Unknown provider '{}'. Must be one of: {}",
                provider,
                VALID_PROVIDERS.join(", ")
            ),
        ));
    }
    let existed = state
        .secrets
        .write()
        .unwrap()
        .remove_key(&provider)
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", e))?;

    Ok(Json(serde_json::json!({
        "ok": true,
        "removed": existed,
        "provider": provider,
    })))
}
