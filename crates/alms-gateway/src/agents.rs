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
pub(crate) fn get_store(
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
        "thinking_budget_tokens": agent.thinking_budget_tokens,
        "reasoning_effort": agent.reasoning_effort.map(|e| e.as_wire_str()),
        "gemini_thinking_budget": agent.gemini_thinking_budget,
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
    if agent.thinking_budget_tokens.is_none() {
        v.as_object_mut().unwrap().remove("thinking_budget_tokens");
    }
    if agent.reasoning_effort.is_none() {
        v.as_object_mut().unwrap().remove("reasoning_effort");
    }
    if agent.gemini_thinking_budget.is_none() {
        v.as_object_mut().unwrap().remove("gemini_thinking_budget");
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
        thinking_budget_tokens: req.thinking_budget_tokens,
        reasoning_effort: req.reasoning_effort,
        gemini_thinking_budget: req.gemini_thinking_budget,
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
        *state.default_agent_id.write() = agent.id;
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

/// Apply an `UpdateAgentRequest` to an existing `AgentRecord` in-place.
///
/// Returns `Ok(())` on success or a structured `UpdateAgentError` on
/// validation failure. Pure function — no I/O, no AppState. Split out
/// from the axum handler so the reasoning-knob clear-sentinel logic
/// (#809) is unit-testable without spinning up a fake HTTP stack.
///
/// Semantics:
/// - Empty-string sentinel for `model` / `posture` / `provider` /
///   `telegram_token` clears the override back to `None`.
/// - `clear_thinking_budget_tokens` / `clear_reasoning_effort` /
///   `clear_gemini_thinking_budget` booleans clear the corresponding
///   reasoning knob back to `None` (inherit server default). Sending
///   both a value and a clear flag for the same knob returns
///   `ClearAndValueConflict` — the handler surfaces that as
///   `400 BAD_REQUEST`.
pub(crate) fn apply_update_request(
    agent: &mut AgentRecord,
    req: UpdateAgentRequest,
) -> Result<(), UpdateAgentError> {
    // Apply non-None fields. Empty string = clear override.
    if let Some(desc) = req.description {
        agent.description = desc;
    }
    if let Some(model) = req.model {
        agent.model = if model.is_empty() { None } else { Some(model) };
    }
    if let Some(posture) = req.posture {
        validate_posture(&posture).map_err(UpdateAgentError::InvalidPosture)?;
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

    // Three reasoning knobs use `clear_*` boolean sentinels (#809) because
    // `Some(0)` on the value field is a legitimate override meaning
    // "disable extended thinking for this agent even when the server
    // default enables it". Sending both a value AND a clear flag for the
    // same knob is ambiguous (the caller is asking us to do two
    // contradictory things), so we reject with 400 rather than silently
    // picking one.

    // `thinking_budget_tokens`
    if req.thinking_budget_tokens.is_some() && req.clear_thinking_budget_tokens == Some(true) {
        return Err(UpdateAgentError::ClearAndValueConflict(
            "thinking_budget_tokens",
        ));
    }
    if req.clear_thinking_budget_tokens == Some(true) {
        agent.thinking_budget_tokens = None;
    } else if let Some(budget) = req.thinking_budget_tokens {
        agent.thinking_budget_tokens = Some(budget);
    }

    // `reasoning_effort` (#768)
    if req.reasoning_effort.is_some() && req.clear_reasoning_effort == Some(true) {
        return Err(UpdateAgentError::ClearAndValueConflict("reasoning_effort"));
    }
    if req.clear_reasoning_effort == Some(true) {
        agent.reasoning_effort = None;
    } else if let Some(effort) = req.reasoning_effort {
        agent.reasoning_effort = Some(effort);
    }

    // `gemini_thinking_budget` (#794)
    if req.gemini_thinking_budget.is_some() && req.clear_gemini_thinking_budget == Some(true) {
        return Err(UpdateAgentError::ClearAndValueConflict(
            "gemini_thinking_budget",
        ));
    }
    if req.clear_gemini_thinking_budget == Some(true) {
        agent.gemini_thinking_budget = None;
    } else if let Some(budget) = req.gemini_thinking_budget {
        agent.gemini_thinking_budget = Some(budget);
    }

    agent.last_active = Utc::now();
    Ok(())
}

/// Validation errors surfaced by [`apply_update_request`].
#[derive(Debug)]
pub(crate) enum UpdateAgentError {
    /// Posture string failed `Posture::from_str`.
    InvalidPosture(String),
    /// Request contained both a value and a `clear_*: true` for the same
    /// reasoning knob (#809).
    ClearAndValueConflict(&'static str),
}

impl UpdateAgentError {
    fn to_api_error(&self) -> (StatusCode, Json<serde_json::Value>) {
        match self {
            UpdateAgentError::InvalidPosture(msg) => {
                api_error(StatusCode::BAD_REQUEST, "INVALID_POSTURE", msg)
            }
            UpdateAgentError::ClearAndValueConflict(field) => api_error(
                StatusCode::BAD_REQUEST,
                "CLEAR_AND_VALUE_CONFLICT",
                format!(
                    "Cannot send both `{field}` value and `clear_{field}: true` in the same request"
                ),
            ),
        }
    }
}

/// PUT /agents/{id_or_name} — update agent config.
pub async fn update_agent(
    State(state): State<AppState>,
    Path(id_or_name): Path<String>,
    Json(req): Json<UpdateAgentRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let store = get_store(&state)?;
    let mut agent = resolve_agent(store, &id_or_name)?;

    apply_update_request(&mut agent, req).map_err(|e| e.to_api_error())?;

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
    *state.default_agent_id.write() = agent.id;

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
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
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

    #[test]
    fn test_agent_to_json_has_telegram_no_token() {
        let mut agent = new_agent("tg-bot");
        agent.telegram_token = Some("123456:ABC-DEF".to_string());

        let json = agent_to_json(&agent);
        // has_telegram should be true
        assert_eq!(json["has_telegram"], serde_json::json!(true));
        // telegram_token must NEVER appear in the output
        assert!(
            json.get("telegram_token").is_none(),
            "telegram_token must not be exposed in API responses"
        );
    }

    #[test]
    fn test_agent_to_json_no_telegram() {
        let agent = new_agent("no-tg");

        let json = agent_to_json(&agent);
        assert_eq!(json["has_telegram"], serde_json::json!(false));
        assert!(json.get("telegram_token").is_none());
    }

    // ==================================================================
    // UpdateAgentRequest clear-sentinel (#809)
    //
    // Covers Option A of the chosen approach: three boolean `clear_*`
    // flags that reset the corresponding reasoning knob back to `None`.
    // When a value and a clear flag are sent together we reject with
    // 400 BAD_REQUEST / `CLEAR_AND_VALUE_CONFLICT`.
    // ==================================================================

    #[test]
    fn update_request_defaults_all_clear_flags_to_none() {
        let req: UpdateAgentRequest = serde_json::from_str("{}").unwrap();
        assert!(req.clear_thinking_budget_tokens.is_none());
        assert!(req.clear_reasoning_effort.is_none());
        assert!(req.clear_gemini_thinking_budget.is_none());
    }

    #[test]
    fn update_request_parses_clear_thinking_budget_tokens() {
        let req: UpdateAgentRequest =
            serde_json::from_str(r#"{"clear_thinking_budget_tokens": true}"#).unwrap();
        assert_eq!(req.clear_thinking_budget_tokens, Some(true));
        assert!(req.thinking_budget_tokens.is_none());
    }

    #[test]
    fn apply_update_clears_thinking_budget_tokens() {
        let mut agent = new_agent("test");
        agent.thinking_budget_tokens = Some(8192);

        let req = UpdateAgentRequest {
            clear_thinking_budget_tokens: Some(true),
            ..Default::default()
        };
        apply_update_request(&mut agent, req).unwrap();
        assert!(
            agent.thinking_budget_tokens.is_none(),
            "clear flag must reset thinking_budget_tokens to None"
        );
    }

    #[test]
    fn apply_update_clears_reasoning_effort() {
        let mut agent = new_agent("test");
        agent.reasoning_effort = Some(alms_core::config::ReasoningEffort::High);

        let req = UpdateAgentRequest {
            clear_reasoning_effort: Some(true),
            ..Default::default()
        };
        apply_update_request(&mut agent, req).unwrap();
        assert!(agent.reasoning_effort.is_none());
    }

    #[test]
    fn apply_update_clears_gemini_thinking_budget() {
        let mut agent = new_agent("test");
        agent.gemini_thinking_budget = Some(4096);

        let req = UpdateAgentRequest {
            clear_gemini_thinking_budget: Some(true),
            ..Default::default()
        };
        apply_update_request(&mut agent, req).unwrap();
        assert!(agent.gemini_thinking_budget.is_none());
    }

    #[test]
    fn apply_update_clear_flag_false_is_no_op() {
        // `clear_*: false` is documented as equivalent to omitting the
        // field entirely — only `Some(true)` triggers the clear.
        let mut agent = new_agent("test");
        agent.thinking_budget_tokens = Some(8192);
        let req = UpdateAgentRequest {
            clear_thinking_budget_tokens: Some(false),
            ..Default::default()
        };
        apply_update_request(&mut agent, req).unwrap();
        assert_eq!(
            agent.thinking_budget_tokens,
            Some(8192),
            "clear_*: false must NOT clear the existing value"
        );
    }

    #[test]
    fn apply_update_rejects_clear_and_value_together_thinking_budget() {
        let mut agent = new_agent("test");
        let req = UpdateAgentRequest {
            thinking_budget_tokens: Some(4096),
            clear_thinking_budget_tokens: Some(true),
            ..Default::default()
        };
        let err = apply_update_request(&mut agent, req).unwrap_err();
        assert!(
            matches!(
                err,
                UpdateAgentError::ClearAndValueConflict("thinking_budget_tokens")
            ),
            "sending both a value and clear flag must return ClearAndValueConflict"
        );
        let (status, _) = err.to_api_error();
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn apply_update_rejects_clear_and_value_together_reasoning_effort() {
        let mut agent = new_agent("test");
        let req = UpdateAgentRequest {
            reasoning_effort: Some(alms_core::config::ReasoningEffort::Medium),
            clear_reasoning_effort: Some(true),
            ..Default::default()
        };
        let err = apply_update_request(&mut agent, req).unwrap_err();
        assert!(matches!(
            err,
            UpdateAgentError::ClearAndValueConflict("reasoning_effort")
        ));
    }

    #[test]
    fn apply_update_rejects_clear_and_value_together_gemini_thinking_budget() {
        let mut agent = new_agent("test");
        let req = UpdateAgentRequest {
            gemini_thinking_budget: Some(2048),
            clear_gemini_thinking_budget: Some(true),
            ..Default::default()
        };
        let err = apply_update_request(&mut agent, req).unwrap_err();
        assert!(matches!(
            err,
            UpdateAgentError::ClearAndValueConflict("gemini_thinking_budget")
        ));
    }

    #[test]
    fn apply_update_preserves_non_cleared_fields() {
        // Clearing one knob must not disturb the other two.
        let mut agent = new_agent("test");
        agent.thinking_budget_tokens = Some(8192);
        agent.reasoning_effort = Some(alms_core::config::ReasoningEffort::Medium);
        agent.gemini_thinking_budget = Some(4096);

        let req = UpdateAgentRequest {
            clear_thinking_budget_tokens: Some(true),
            ..Default::default()
        };
        apply_update_request(&mut agent, req).unwrap();
        assert!(agent.thinking_budget_tokens.is_none());
        assert_eq!(
            agent.reasoning_effort,
            Some(alms_core::config::ReasoningEffort::Medium),
            "clearing thinking_budget_tokens must not disturb reasoning_effort"
        );
        assert_eq!(
            agent.gemini_thinking_budget,
            Some(4096),
            "clearing thinking_budget_tokens must not disturb gemini_thinking_budget"
        );
    }

    /// End-to-end round trip through SQLite: insert an agent with all
    /// three reasoning knobs set, PATCH with all three `clear_*: true`,
    /// reload from the store, verify all three are `None`. This is what
    /// the `apply_overrides` precedence test relies on: once the SQLite
    /// record's knob is `None`, the agent-layer precedence falls through
    /// to the server default.
    #[test]
    fn clear_sentinels_round_trip_through_sqlite() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut agent = new_agent("round-trip");
        agent.thinking_budget_tokens = Some(16384);
        agent.reasoning_effort = Some(alms_core::config::ReasoningEffort::High);
        agent.gemini_thinking_budget = Some(8192);
        store.create_agent(&agent).unwrap();

        // Load, mutate via apply_update_request, persist.
        let mut loaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        let req = UpdateAgentRequest {
            clear_thinking_budget_tokens: Some(true),
            clear_reasoning_effort: Some(true),
            clear_gemini_thinking_budget: Some(true),
            ..Default::default()
        };
        apply_update_request(&mut loaded, req).unwrap();
        store.update_agent(&loaded).unwrap();

        // Reload and verify all three are None — this is what
        // `apply_overrides` will see on a subsequent POST /runs, and
        // that's what makes the precedence fall through to the server
        // default.
        let reloaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert!(reloaded.thinking_budget_tokens.is_none());
        assert!(reloaded.reasoning_effort.is_none());
        assert!(reloaded.gemini_thinking_budget.is_none());
    }

    // ==================================================================
    // `agent_to_json` wire-shape coverage for the new reasoning fields
    // (#809 follow-up — Tim review item 6).
    //
    // The UI work for #804 Slice B leans on the exact wire shape
    // produced by `agent_to_json`, including the "Some(N) emits the key,
    // None omits the key" contract. These tests pin all three new
    // reasoning knobs to that contract for `Some(N)`, `Some(0)`, and
    // `None` so a future refactor of `agent_to_json` cannot silently
    // drop a field.
    // ==================================================================

    #[test]
    fn agent_to_json_emits_reasoning_fields_when_some_nonzero() {
        let mut agent = new_agent("with-overrides");
        agent.thinking_budget_tokens = Some(8192);
        agent.reasoning_effort = Some(alms_core::config::ReasoningEffort::High);
        agent.gemini_thinking_budget = Some(4096);

        let json = agent_to_json(&agent);
        assert_eq!(json["thinking_budget_tokens"], serde_json::json!(8192));
        assert_eq!(json["reasoning_effort"], serde_json::json!("high"));
        assert_eq!(json["gemini_thinking_budget"], serde_json::json!(4096));
    }

    #[test]
    fn agent_to_json_emits_reasoning_fields_when_some_zero() {
        // `Some(0)` is a legitimate per-agent override meaning "disable
        // extended thinking even when the server default enables it".
        // It must NOT be dropped from the wire shape — that distinction
        // is exactly what the clear-sentinel design (#809) preserves.
        let mut agent = new_agent("with-zero");
        agent.thinking_budget_tokens = Some(0);
        agent.gemini_thinking_budget = Some(0);

        let json = agent_to_json(&agent);
        assert_eq!(json["thinking_budget_tokens"], serde_json::json!(0));
        assert_eq!(json["gemini_thinking_budget"], serde_json::json!(0));
        // reasoning_effort has no zero — only string variants — but
        // None still omits the key (covered in the next test).
    }

    #[test]
    fn agent_to_json_omits_reasoning_fields_when_none() {
        // Default `new_agent("...")` leaves all three fields `None` —
        // mirrors a freshly-created agent with no per-agent overrides.
        let agent = new_agent("no-overrides");
        let json = agent_to_json(&agent);
        let obj = json.as_object().unwrap();

        assert!(
            !obj.contains_key("thinking_budget_tokens"),
            "thinking_budget_tokens must be omitted when None: {json}"
        );
        assert!(
            !obj.contains_key("reasoning_effort"),
            "reasoning_effort must be omitted when None: {json}"
        );
        assert!(
            !obj.contains_key("gemini_thinking_budget"),
            "gemini_thinking_budget must be omitted when None: {json}"
        );
    }

    #[test]
    fn agent_to_json_emits_each_reasoning_effort_variant() {
        // Pin the wire-string mapping for every `ReasoningEffort` variant
        // so a future rename of `as_wire_str` is caught here, not in the
        // UI. This is the surface the dropdown in #804 will populate from.
        for (variant, expected_str) in [
            (alms_core::config::ReasoningEffort::Minimal, "minimal"),
            (alms_core::config::ReasoningEffort::Low, "low"),
            (alms_core::config::ReasoningEffort::Medium, "medium"),
            (alms_core::config::ReasoningEffort::High, "high"),
        ] {
            let mut agent = new_agent("variant-test");
            agent.reasoning_effort = Some(variant);
            let json = agent_to_json(&agent);
            assert_eq!(
                json["reasoning_effort"],
                serde_json::json!(expected_str),
                "variant {variant:?} must serialize as {expected_str:?}"
            );
        }
    }
}
