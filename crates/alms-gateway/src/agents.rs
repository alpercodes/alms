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

use crate::server::AppState;
use alms_core::{
    AgentId, AgentRecord, CreateAgentRequest, UpdateAgentRequest, validate_agent_name,
};
use alms_session::SqliteStore;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::Utc;

/// Valid posture values.
const VALID_POSTURES: &[&str] = &["full_control", "guarded"];

/// Validate a posture string. Empty string is allowed (means "clear override").
fn validate_posture(posture: &str) -> Result<(), String> {
    if posture.is_empty() || VALID_POSTURES.contains(&posture) {
        Ok(())
    } else {
        Err(format!(
            "Invalid posture '{}'. Must be one of: {}",
            posture,
            VALID_POSTURES.join(", ")
        ))
    }
}

/// Helper: get the SqliteStore from app state, or return 503.
fn get_store(
    state: &AppState,
) -> Result<&std::sync::Arc<SqliteStore>, (StatusCode, Json<serde_json::Value>)> {
    state.session_manager.store().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": { "code": "NOT_AVAILABLE", "message": "Agent registry not available (no database configured)" }
            })),
        )
    })
}

/// Helper: resolve an agent by UUID or name slug.
fn resolve_agent(
    store: &SqliteStore,
    id_or_name: &str,
) -> Result<AgentRecord, (StatusCode, Json<serde_json::Value>)> {
    let map_err = |e: alms_core::AlmsError| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": { "code": "INTERNAL", "message": e.to_string() }
            })),
        )
    };

    // Try UUID first
    if let Ok(uuid) = uuid::Uuid::parse_str(id_or_name) {
        let agent_id = AgentId(uuid);
        return match store.load_agent_by_id(agent_id) {
            Ok(Some(agent)) => Ok(agent),
            Ok(None) => Err((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": { "code": "NOT_FOUND", "message": format!("Agent not found: {id_or_name}") }
                })),
            )),
            Err(e) => Err(map_err(e)),
        };
    }

    // Fall back to name lookup
    match store.load_agent_by_name(id_or_name) {
        Ok(Some(agent)) => Ok(agent),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": { "code": "NOT_FOUND", "message": format!("Agent not found: {id_or_name}") }
            })),
        )),
        Err(e) => Err(map_err(e)),
    }
}

/// GET /agents — list all agents.
pub async fn list_agents(State(state): State<AppState>) -> impl IntoResponse {
    let store = get_store(&state)?;
    let agents = store.list_agents().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": { "code": "INTERNAL", "message": e.to_string() }
            })),
        )
    })?;
    Ok::<_, (StatusCode, Json<serde_json::Value>)>(Json(serde_json::json!({ "agents": agents })))
}

/// POST /agents — create a new agent.
pub async fn create_agent(
    State(state): State<AppState>,
    Json(req): Json<CreateAgentRequest>,
) -> Result<(StatusCode, Json<AgentRecord>), (StatusCode, Json<serde_json::Value>)> {
    let store = get_store(&state)?;

    // Validate name
    validate_agent_name(&req.name).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": { "code": "INVALID_NAME", "message": e.to_string() }
            })),
        )
    })?;

    let wants_default = req.is_default.unwrap_or(false);

    // Validate posture if provided
    if let Some(ref p) = req.posture {
        validate_posture(p).map_err(|msg| {
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": { "code": "INVALID_POSTURE", "message": msg }
                })),
            )
        })?;
    }

    let now = Utc::now();
    let mut agent = AgentRecord {
        id: AgentId::new(),
        name: req.name,
        description: req.description.unwrap_or_default(),
        model: req.model,
        system_prompt: req.system_prompt,
        posture: req.posture,
        // Always INSERT with is_default=false; set_default_agent atomically
        // clears old default + sets new one in a single transaction.
        is_default: false,
        created_at: now,
        last_active: now,
    };

    store.create_agent(&agent).map_err(|e| match &e {
        alms_core::AlmsError::DuplicateName(name) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": { "code": "DUPLICATE_NAME", "message": format!("Agent name '{name}' already exists") }
            })),
        ),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": { "code": "INTERNAL", "message": e.to_string() }
            })),
        ),
    })?;

    if wants_default {
        store.set_default_agent(agent.id).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": { "code": "INTERNAL", "message": e.to_string() }
                })),
            )
        })?;
        agent.is_default = true;
    }

    Ok((StatusCode::CREATED, Json(agent)))
}

/// GET /agents/{id_or_name} — get agent details.
pub async fn get_agent(
    State(state): State<AppState>,
    Path(id_or_name): Path<String>,
) -> Result<Json<AgentRecord>, (StatusCode, Json<serde_json::Value>)> {
    let store = get_store(&state)?;
    let agent = resolve_agent(store, &id_or_name)?;
    Ok(Json(agent))
}

/// PUT /agents/{id_or_name} — update agent config.
pub async fn update_agent(
    State(state): State<AppState>,
    Path(id_or_name): Path<String>,
    Json(req): Json<UpdateAgentRequest>,
) -> Result<Json<AgentRecord>, (StatusCode, Json<serde_json::Value>)> {
    let store = get_store(&state)?;
    let mut agent = resolve_agent(store, &id_or_name)?;

    // Apply non-None fields. Empty string = clear override.
    if let Some(desc) = req.description {
        agent.description = desc;
    }
    if let Some(model) = req.model {
        agent.model = if model.is_empty() { None } else { Some(model) };
    }
    if let Some(sp) = req.system_prompt {
        agent.system_prompt = if sp.is_empty() { None } else { Some(sp) };
    }
    if let Some(posture) = req.posture {
        validate_posture(&posture).map_err(|msg| {
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": { "code": "INVALID_POSTURE", "message": msg }
                })),
            )
        })?;
        agent.posture = if posture.is_empty() {
            None
        } else {
            Some(posture)
        };
    }

    agent.last_active = Utc::now();

    store.update_agent(&agent).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": { "code": "INTERNAL", "message": e.to_string() }
            })),
        )
    })?;

    Ok(Json(agent))
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
        return Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": {
                    "code": "CANNOT_DELETE_DEFAULT",
                    "message": "Cannot delete the default agent. Set another agent as default first."
                }
            })),
        ));
    }

    store.delete_agent(agent.id).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": { "code": "INTERNAL", "message": e.to_string() }
            })),
        )
    })?;

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

    store.set_default_agent(agent.id).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": { "code": "INTERNAL", "message": e.to_string() }
            })),
        )
    })?;

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
            system_prompt: None,
            posture: None,
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
