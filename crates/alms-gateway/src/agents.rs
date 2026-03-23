//! Agent registry HTTP API
//!
//! CRUD endpoints for managing persistent named agents.
//!
//! ```text
//! GET    /agents                      — list all agents
//! POST   /agents                      — create agent
//! GET    /agents/{id_or_name}         — get agent details
//! PUT    /agents/{id_or_name}         — update agent config
//! DELETE /agents/{id_or_name}         — delete agent
//! POST   /agents/{id_or_name}/default — set as default
//! ```

use crate::api_error;
use crate::server::AppState;
use alms_core::{
    AgentId, AgentRecord, CreateAgentRequest, UpdateAgentRequest, validate_agent_name,
};
use alms_runtime::Posture;
use alms_session::SqliteStore;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::Utc;

/// Validate a posture string. Empty string is allowed (means "clear override").
fn validate_posture(posture: &str) -> Result<(), String> {
    if posture.is_empty() {
        Ok(())
    } else {
        posture.parse::<Posture>().map(|_| ())
    }
}

/// Helper: get the SqliteStore from app state, or return 503.
fn get_store(
    state: &AppState,
) -> Result<&std::sync::Arc<SqliteStore>, (StatusCode, Json<serde_json::Value>)> {
    state.session_manager.store().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "NOT_AVAILABLE",
            "Agent registry not available (no database configured)",
        )
    })
}

/// Helper: resolve an agent by UUID or name slug.
pub(crate) fn resolve_agent(
    store: &SqliteStore,
    id_or_name: &str,
) -> Result<AgentRecord, (StatusCode, Json<serde_json::Value>)> {
    let not_found = || {
        api_error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            format!("Agent not found: {id_or_name}"),
        )
    };
    let internal =
        |e: alms_core::AlmsError| api_error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", e);

    // Try UUID first
    if let Ok(uuid) = uuid::Uuid::parse_str(id_or_name) {
        let agent_id = AgentId(uuid);
        return match store.load_agent_by_id(agent_id) {
            Ok(Some(agent)) => Ok(agent),
            Ok(None) => Err(not_found()),
            Err(e) => Err(internal(e)),
        };
    }

    // Fall back to name lookup
    match store.load_agent_by_name(id_or_name) {
        Ok(Some(agent)) => Ok(agent),
        Ok(None) => Err(not_found()),
        Err(e) => Err(internal(e)),
    }
}

/// Build a safe JSON representation of an agent for API responses.
///
/// The `telegram_token` is never exposed -- instead a `has_telegram` boolean
/// flag is included so the UI can show connection status.
fn agent_to_json(agent: &AgentRecord) -> serde_json::Value {
    let mut v = serde_json::json!({
        "id": agent.id.0.to_string(),
        "name": agent.name,
        "description": agent.description,
        "model": agent.model,
        "posture": agent.posture,
        "provider": agent.provider,
        "has_telegram": agent.telegram_token.is_some(),
        "is_default": agent.is_default,
        "created_at": agent.created_at.to_rfc3339(),
        "last_active": agent.last_active.to_rfc3339(),
    });
    // Strip null fields for cleaner output (match existing serde behavior)
    if agent.model.is_none() {
        v.as_object_mut().unwrap().remove("model");
    }
    if agent.posture.is_none() {
        v.as_object_mut().unwrap().remove("posture");
    }
    if agent.provider.is_none() {
        v.as_object_mut().unwrap().remove("provider");
    }
    v
}

/// GET /agents — list all agents.
pub async fn list_agents(State(state): State<AppState>) -> impl IntoResponse {
    let store = get_store(&state)?;
    let agents = store
        .list_agents()
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", e))?;
    let agents_json: Vec<_> = agents.iter().map(agent_to_json).collect();
    Ok::<_, (StatusCode, Json<serde_json::Value>)>(Json(
        serde_json::json!({ "agents": agents_json }),
    ))
}

/// POST /agents — create a new agent.
pub async fn create_agent(
    State(state): State<AppState>,
    Json(req): Json<CreateAgentRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let store = get_store(&state)?;

    // Validate name
    validate_agent_name(&req.name)
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, "INVALID_NAME", e))?;

    let wants_default = req.is_default.unwrap_or(false);

    // Validate posture if provided
    if let Some(ref p) = req.posture {
        validate_posture(p)
            .map_err(|msg| api_error(StatusCode::BAD_REQUEST, "INVALID_POSTURE", msg))?;
    }

    let now = Utc::now();
    let mut agent = AgentRecord {
        id: AgentId::new(),
        name: req.name,
        description: req.description.unwrap_or_default(),
        model: req.model,
        posture: req.posture,
        provider: req.provider,
        telegram_token: req.telegram_token,
        // Always INSERT with is_default=false; set_default_agent atomically
        // clears old default + sets new one in a single transaction.
        is_default: false,
        created_at: now,
        last_active: now,
    };

    store.create_agent(&agent).map_err(|e| match &e {
        alms_core::AlmsError::DuplicateName(name) => api_error(
            StatusCode::CONFLICT,
            "DUPLICATE_NAME",
            format!("Agent name '{name}' already exists"),
        ),
        _ => api_error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", e),
    })?;

    if wants_default {
        store
            .set_default_agent(agent.id)
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", e))?;
        *state.default_agent_id.write().unwrap() = agent.id;
        agent.is_default = true;
    }

    // Create workspace directory and initial files
    if let Some(ref workspace_dir) = state.workspace_dir {
        let agent_ws_dir = workspace_dir.join(&agent.name);
        if let Err(e) = alms_core::init_workspace_files(&agent_ws_dir) {
            tracing::warn!(
                "Could not create workspace files in {}: {}",
                agent_ws_dir.display(),
                e
            );
        }
    }

    Ok((StatusCode::CREATED, Json(agent_to_json(&agent))))
}

/// GET /agents/{id_or_name} — get agent details.
pub async fn get_agent(
    State(state): State<AppState>,
    Path(id_or_name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let store = get_store(&state)?;
    let agent = resolve_agent(store, &id_or_name)?;
    Ok(Json(agent_to_json(&agent)))
}

/// PUT /agents/{id_or_name} — update agent config.
pub async fn update_agent(
    State(state): State<AppState>,
    Path(id_or_name): Path<String>,
    Json(req): Json<UpdateAgentRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let store = get_store(&state)?;
    let mut agent = resolve_agent(store, &id_or_name)?;

    // Apply non-None fields. Empty string = clear override.
    if let Some(desc) = req.description {
        agent.description = desc;
    }
    if let Some(model) = req.model {
        agent.model = if model.is_empty() { None } else { Some(model) };
    }
    if let Some(posture) = req.posture {
        validate_posture(&posture)
            .map_err(|msg| api_error(StatusCode::BAD_REQUEST, "INVALID_POSTURE", msg))?;
        agent.posture = if posture.is_empty() {
            None
        } else {
            Some(posture)
        };
    }

    if let Some(provider) = req.provider {
        agent.provider = if provider.is_empty() {
            None
        } else {
            Some(provider)
        };
    }

    if let Some(telegram_token) = req.telegram_token {
        agent.telegram_token = if telegram_token.is_empty() {
            None
        } else {
            Some(telegram_token)
        };
    }

    agent.last_active = Utc::now();

    store
        .update_agent(&agent)
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", e))?;

    Ok(Json(agent_to_json(&agent)))
}

/// DELETE /agents/{id_or_name} — delete an agent.
pub async fn delete_agent(
    State(state): State<AppState>,
    Path(id_or_name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let store = get_store(&state)?;
    let agent = resolve_agent(store, &id_or_name)?;

    // Guard: cannot delete the default agent
    if agent.is_default {
        return Err(api_error(
            StatusCode::CONFLICT,
            "CANNOT_DELETE_DEFAULT",
            "Cannot delete the default agent. Set another agent as default first.",
        ));
    }

    store
        .delete_agent(agent.id)
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", e))?;

    Ok(Json(
        serde_json::json!({ "ok": true, "deleted": agent.id.to_string() }),
    ))
}

/// POST /agents/{id_or_name}/default — set agent as default.
pub async fn set_default(
    State(state): State<AppState>,
    Path(id_or_name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let store = get_store(&state)?;
    let agent = resolve_agent(store, &id_or_name)?;

    store
        .set_default_agent(agent.id)
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", e))?;

    // Update the live default agent ID so the running gateway uses it immediately.
    *state.default_agent_id.write().unwrap() = agent.id;

    Ok(Json(
        serde_json::json!({ "ok": true, "default_agent": agent.name }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alms_session::SqliteStore;

    fn new_agent(name: &str) -> AgentRecord {
        let now = Utc::now();
        AgentRecord {
            id: AgentId::new(),
            name: name.to_string(),
            description: String::new(),
            model: None,
            posture: None,
            provider: None,
            telegram_token: None,
            is_default: false,
            created_at: now,
            last_active: now,
        }
    }

    #[test]
    fn test_resolve_by_uuid() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent = new_agent("atlas");
        store.create_agent(&agent).unwrap();

        let resolved = resolve_agent(&store, &agent.id.to_string()).unwrap();
        assert_eq!(resolved.id, agent.id);
        assert_eq!(resolved.name, "atlas");
    }

    #[test]
    fn test_resolve_by_name() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent = new_agent("atlas");
        store.create_agent(&agent).unwrap();

        let resolved = resolve_agent(&store, "atlas").unwrap();
        assert_eq!(resolved.id, agent.id);
    }

    #[test]
    fn test_resolve_not_found_uuid() {
        let store = SqliteStore::open_in_memory().unwrap();
        let fake_id = AgentId::new();
        let result = resolve_agent(&store, &fake_id.to_string());
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_resolve_not_found_name() {
        let store = SqliteStore::open_in_memory().unwrap();
        let result = resolve_agent(&store, "nonexistent");
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_resolve_prefers_uuid_over_name() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent = new_agent("test-agent");
        store.create_agent(&agent).unwrap();

        // UUID lookup should find it even if there's also a name match
        let by_id = resolve_agent(&store, &agent.id.to_string()).unwrap();
        let by_name = resolve_agent(&store, "test-agent").unwrap();
        assert_eq!(by_id.id, by_name.id);
    }

    #[test]
    fn test_validate_name_rejected() {
        // Verify that invalid names would be caught
        assert!(validate_agent_name("").is_err());
        assert!(validate_agent_name("My Agent").is_err());
        assert!(validate_agent_name("-leading").is_err());
    }
}
