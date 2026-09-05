// SPDX-License-Identifier: Apache-2.0

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
use alms_core::worktree::{self, WorktreeError};
use alms_core::{
    AgentId, AgentRecord, AlmsResult, CreateAgentRequest, UpdateAgentRequest, WorktreeMode,
    validate_agent_name,
};
use alms_runtime::Posture;
use alms_session::SqliteStore;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::Utc;
use std::path::Path as StdPath;

/// Validate a posture string. Empty string is allowed (means "clear override").
fn validate_posture(posture: &str) -> Result<(), String> {
    if posture.is_empty() {
        Ok(())
    } else {
        posture.parse::<Posture>().map(|_| ())
    }
}

fn normalize_optional_override(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Validate the per-agent summary provider/model pair (#872).
///
/// Both fields must be set together or both must be unset. When a provider
/// is set, it must reference a configured `[llm.providers.<name>]` entry
/// and have a resolvable API key (entry-level `api_key_env` / `api_key`,
/// or a key in the live `SecretsStore`). Mirrors the symmetric pair-only
/// validation that runs at the server-level layer in `settings.rs`.
///
/// `provider` and `model` here are the post-update values (i.e. the state
/// the agent record will be in after the PATCH commits) — callers compute
/// these by merging the request against the live record before calling.
fn validate_summary_pair(
    state: &AppState,
    provider: Option<&str>,
    model: Option<&str>,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    crate::configuration::validate_summary_pair(
        provider,
        model,
        &state.llm_config.providers,
        &state.secrets.read(),
    )
    .map(|_| ())
    .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.code, error.message))
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
        "summary_provider": agent.summary_provider,
        "summary_model": agent.summary_model,
        "worktree_mode": agent.worktree_mode.as_wire_str(),
        "debug_mode": agent.debug_mode,
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
    if agent.summary_provider.is_none() {
        v.as_object_mut().unwrap().remove("summary_provider");
    }
    if agent.summary_model.is_none() {
        v.as_object_mut().unwrap().remove("summary_model");
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

    let model = normalize_optional_override(req.model.as_deref());
    let posture = normalize_optional_override(req.posture.as_deref());
    let provider = normalize_optional_override(req.provider.as_deref());

    // Validate posture if provided
    if let Some(ref p) = posture {
        validate_posture(p)
            .map_err(|msg| api_error(StatusCode::BAD_REQUEST, "INVALID_POSTURE", msg))?;
    }

    // Per-agent summary overrides (#872) — pair-only validation. Treat
    // the empty string the same as a missing field for back-compat with
    // CLIs / scripts that habitually pass `""` to mean "unset".
    let summary_provider_norm = req
        .summary_provider
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let summary_model_norm = req
        .summary_model
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    validate_summary_pair(
        &state,
        summary_provider_norm.as_deref(),
        summary_model_norm.as_deref(),
    )?;

    // Worktree-mode (#946). Default to `Off` when the request omits the
    // field — the wire shape stays back-compat with pre-#946 clients.
    // When `Git` is requested we provision the worktree BEFORE
    // persisting the agent record so a non-git project produces a
    // clean 4xx with NO half-created agent and NO half-created
    // worktree directory. The expensive bit (`git worktree add`)
    // happens on the createside; PATCH-time flips do the same dance.
    //
    // Name-uniqueness is enforced ahead of the worktree provisioning
    // step so a duplicate name doesn't even cost us a `git worktree
    // add`. The race between this lookup and the INSERT below is
    // handled by the SQLite UNIQUE-name constraint surfacing
    // `AlmsError::DuplicateName` and the worktree-op compensation
    // (#1022 — extended from #1019's PATCH path) cleaning up the
    // freshly-created worktree dir.
    let worktree_mode = req.worktree_mode.unwrap_or_default();
    if worktree_mode == WorktreeMode::Git
        && let Ok(Some(_)) = store.load_agent_by_name(&req.name)
    {
        return Err(api_error(
            StatusCode::CONFLICT,
            "DUPLICATE_NAME",
            format!("Agent name '{}' already exists", req.name),
        ));
    }

    let now = Utc::now();
    let mut agent = AgentRecord {
        id: AgentId::new(),
        name: req.name,
        description: req.description.unwrap_or_default(),
        model,
        posture,
        provider,
        telegram_token: req.telegram_token,
        thinking_budget_tokens: req.thinking_budget_tokens,
        reasoning_effort: req.reasoning_effort,
        gemini_thinking_budget: req.gemini_thinking_budget,
        summary_provider: summary_provider_norm,
        summary_model: summary_model_norm,
        worktree_mode,
        // Per-agent debug_mode (#1003). `None` from clients that predate
        // the field maps to `false` — matches the schema default and
        // preserves pre-#1003 behaviour (no context_debug SSE event).
        debug_mode: req.debug_mode.unwrap_or(false),
        // Always INSERT with is_default=false; set_default_agent atomically
        // clears old default + sets new one in a single transaction.
        is_default: false,
        created_at: now,
        last_active: now,
    };

    if worktree_mode == WorktreeMode::Git {
        // Route the worktree-create + SQLite-persist pair through the
        // shared `apply_worktree_op_and_persist_with_mapper` helper so
        // any post-side-effect persist failure (race-DuplicateName,
        // "database is locked", disk-full, fs-perm) gets the
        // just-created worktree cleaned up before the error surfaces
        // to the client. Pre-#1022 only DuplicateName triggered
        // rollback; every other error variant leaked the worktree
        // dir. See #1019 / Tim's round-3 review for the bug pattern
        // this fix generalizes.
        //
        // The mapper preserves the pre-#1022 wire shape `409
        // DUPLICATE_NAME` for the race-create case (where another
        // concurrent POST slipped its INSERT in between our
        // `load_agent_by_name` check above and our `create_agent`
        // below). Every other persist-error variant falls back to
        // the helper's default `500 INTERNAL` / `500
        // WORKTREE_COMPENSATION_FAILED` shapes.
        let agent_name = agent.name.clone();
        let project_root = state.project_root.clone();
        let store_ref = store.clone();
        let agent_ref = agent.clone();
        apply_worktree_op_and_persist_with_mapper(
            &project_root,
            &agent_name,
            WorktreeOp::Create,
            || store_ref.create_agent(&agent_ref),
            |e| match e {
                alms_core::AlmsError::DuplicateName(name) => Some(api_error(
                    StatusCode::CONFLICT,
                    "DUPLICATE_NAME",
                    format!("Agent name '{name}' already exists"),
                )),
                _ => None,
            },
        )?;

        // The startup-time WARN for `[security].allow_full_os_access`
        // already documents the precedence at boot. Surface a second
        // WARN here when an operator creates a worktree-mode agent
        // that is ALSO on the allow list — the worktree itself is
        // intentionally left in place (so flipping the security
        // knob off later restores the worktree sandbox without a
        // re-create), but the operator should know the worktree's
        // sandbox attachment will be skipped at every run.
        if state.security_config.is_full_os_access_agent(&agent.name) {
            tracing::warn!(
                target: "alms.security",
                agent_name = %agent.name,
                allow_full_os_access = true,
                worktree_mode = "git",
                "Agent '{}' has worktree_mode=git AND is on [security].allow_full_os_access. \
                 The worktree was created at <project>/.alms/worktrees/{}/, but at run time \
                 the security list takes precedence: the agent will run WITHOUT any \
                 filesystem sandbox (worktree pin is skipped).",
                agent.name,
                agent.name,
            );
        }
    } else {
        // worktree_mode = Off — no on-disk side-effect to compose
        // around, persist directly. Preserves pre-#1022 wire shape
        // for the (overwhelmingly common) non-worktree create path.
        store.create_agent(&agent).map_err(|e| match &e {
            alms_core::AlmsError::DuplicateName(name) => api_error(
                StatusCode::CONFLICT,
                "DUPLICATE_NAME",
                format!("Agent name '{name}' already exists"),
            ),
            _ => api_error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", e),
        })?;
    }

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

/// Map a [`WorktreeError`] onto an HTTP API error response.
///
/// Defined as a free function so both the create / update paths and
/// the delete path map to identical wire shapes.
fn worktree_error_to_api(e: &WorktreeError) -> (StatusCode, Json<serde_json::Value>) {
    match e {
        WorktreeError::NotAGitRepo => api_error(
            StatusCode::BAD_REQUEST,
            "WORKTREE_REQUIRES_GIT",
            "worktree_mode = \"git\" requires the project root to be a git working tree. \
             Either run `git init` in the project directory, or set `worktree_mode = \"off\"`.",
        ),
        WorktreeError::UncommittedChanges => api_error(
            StatusCode::CONFLICT,
            "WORKTREE_HAS_UNCOMMITTED_CHANGES",
            "the agent's worktree contains uncommitted changes. Pass \
             `force_worktree_remove: true` (PATCH) or `?force=true` (DELETE) \
             to override — this discards the worktree contents AND deletes \
             the `alms/<name>` branch.",
        ),
        WorktreeError::GitFailed(msg) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "WORKTREE_GIT_FAILED",
            format!("git worktree command failed: {msg}"),
        ),
        WorktreeError::Io(msg) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "WORKTREE_IO_FAILED",
            format!("worktree IO error: {msg}"),
        ),
    }
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

    // Per-agent summary provider/model (#872). Like the reasoning knobs
    // above, these use `clear_*` boolean sentinels rather than the
    // empty-string trick — an empty string is silently rejected here so
    // that callers can't smuggle a partial-set state past the pair-only
    // validator. A non-empty value writes through; `clear_*: true` resets
    // the field to `None`. The cross-field pair invariant is enforced in
    // the handler (`update_agent`) because it requires `AppState` to
    // validate the provider against the live `[llm.providers.<name>]` map
    // and the secrets store.
    if req.summary_provider.is_some() && req.clear_summary_provider == Some(true) {
        return Err(UpdateAgentError::ClearAndValueConflict("summary_provider"));
    }
    if req.clear_summary_provider == Some(true) {
        agent.summary_provider = None;
    } else if let Some(ref provider) = req.summary_provider {
        let trimmed = provider.trim();
        if trimmed.is_empty() {
            return Err(UpdateAgentError::EmptySummaryField("summary_provider"));
        }
        agent.summary_provider = Some(trimmed.to_string());
    }

    if req.summary_model.is_some() && req.clear_summary_model == Some(true) {
        return Err(UpdateAgentError::ClearAndValueConflict("summary_model"));
    }
    if req.clear_summary_model == Some(true) {
        agent.summary_model = None;
    } else if let Some(ref model) = req.summary_model {
        let trimmed = model.trim();
        if trimmed.is_empty() {
            return Err(UpdateAgentError::EmptySummaryField("summary_model"));
        }
        agent.summary_model = Some(trimmed.to_string());
    }

    // Worktree mode (#946). Pure-function update; the side-effecting
    // `git worktree add` / `remove` runs in the HTTP handler layer
    // (it needs `AppState` for the project root and the security
    // config) — see `update_agent` below.
    if let Some(mode) = req.worktree_mode {
        agent.worktree_mode = mode;
    }

    // Debug mode (#1003). Plain boolean — no `clear_*` sentinel needed
    // because `false` is itself the cleared / default state. Omitting
    // the field on PATCH leaves the existing value unchanged; sending
    // `Some(true)` / `Some(false)` writes through.
    if let Some(debug_mode) = req.debug_mode {
        agent.debug_mode = debug_mode;
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
    /// `summary_provider` / `summary_model` (#872) was sent as an empty
    /// string. Empty strings are not a valid clear-sentinel for this pair —
    /// callers must use `clear_summary_provider: true` /
    /// `clear_summary_model: true` so the pair-only invariant can be
    /// validated cleanly. Empty-string-as-clear was deliberately rejected
    /// for these fields to keep the wire shape unambiguous.
    EmptySummaryField(&'static str),
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
            UpdateAgentError::EmptySummaryField(field) => api_error(
                StatusCode::BAD_REQUEST,
                "SUMMARY_FIELD_EMPTY",
                format!(
                    "`{field}` cannot be an empty string — use `clear_{field}: true` to reset \
                     back to inheriting the server-level [context] settings"
                ),
            ),
        }
    }
}

/// Identifies which `/agents` handler invoked
/// `apply_worktree_op_and_persist` so the helper can run the right
/// forward side-effect and its inverse on compensation. PATCH is the
/// only direction-sensitive one (off↔git flip); POST and DELETE are
/// fixed-direction.
///
/// The shared compensation framework — phase-1 side-effect, phase-2
/// persist, phase-3 inverse op gated on `did_create` / `did_remove`,
/// dual-error wire shape on compensation failure — lives in
/// `apply_worktree_op_and_persist` regardless of the variant. See
/// #1022 for the POST + DELETE extension; the existing PATCH variant
/// preserves byte-for-byte semantics for the 10-cell matrix Tim
/// verified on #1019.
#[derive(Debug, Clone, Copy)]
enum WorktreeOp {
    /// POST /agents with `worktree_mode = git`. Forward op is
    /// `create_worktree`; inverse op on persist failure is
    /// `remove_worktree(force=true)` only when the forward call
    /// actually provisioned disk state (`Created`, not
    /// `AlreadyExisted`). An `AlreadyExisted` outcome can indicate
    /// either operator drift (a stale `.alms/worktrees/<name>/` dir
    /// the registry pre-call probe didn't see) OR a true race
    /// between concurrent POSTs where the loser's worktree-add
    /// arrives after the winner's; in both cases compensation
    /// correctly leaves the directory alone because we didn't
    /// create it.
    Create,
    /// DELETE /agents with `worktree_mode = git`. Forward op is
    /// `remove_worktree(force=query.force)`; inverse op on persist
    /// failure is `restore_worktree_at_sha` keyed to the SHA we
    /// snapshotted BEFORE the remove (or `create_worktree` as a
    /// fallback when the snapshot probe failed). Branch-restore
    /// triggers regardless of `did_remove` when we hold a snapshot,
    /// because `remove_worktree` calls `delete_branch` even on the
    /// `AlreadyAbsent` no-op path. See #1019 / Codex P1 round 3.
    ///
    /// Reversibility asymmetry: when `force = true`,
    /// `remove_worktree` discards uncommitted working-copy changes
    /// before deleting the worktree. Compensation restores the
    /// branch + worktree dir at `pre_remove_branch_sha`, but
    /// uncommitted operator state at the moment of the DELETE is
    /// unrecoverable. Same limitation as PATCH git→off with
    /// `force_remove = true` (see `Flip` variant below). The
    /// committed history is always reversible; the working-copy
    /// dirt is not. The operator-facing version of this limitation
    /// lives in `docs/security-model.md` under "Force-true
    /// reversibility asymmetry" in the worktree-mode section.
    Remove { force: bool },
    /// PATCH /agents. Forward op depends on the (`prev_mode`,
    /// `new_mode`) transition: off→git creates, git→off removes,
    /// same-mode is a no-op. Inverse op on persist failure mirrors
    /// the variant decision. `force_remove` only applies to git→off
    /// and carries the same uncommitted-state irreversibility as
    /// `Remove { force: true }` — committed history rolls back via
    /// `restore_worktree_at_sha`, but working-copy dirt at the
    /// moment of the flip is lost. The operator-facing version of
    /// this limitation lives in `docs/security-model.md` under
    /// "Force-true reversibility asymmetry" in the worktree-mode
    /// section. `is_full_os_access_agent` drives an extra security
    /// WARN when the off→git flip lands on an allow-listed agent.
    Flip {
        prev_mode: WorktreeMode,
        new_mode: WorktreeMode,
        force_remove: bool,
        is_full_os_access_agent: bool,
    },
}

/// Run the worktree side-effect for a `/agents` handler and then
/// persist the agent row, with compensating cleanup if the persist
/// step fails after the side-effect has already touched disk (#964,
/// extended to POST + DELETE in #1022).
///
/// The hazard is the same across all three callers: each handler
/// touches on-disk worktree state BEFORE the SQLite row is written
/// (so an `4xx` from the worktree layer never produces drift). If
/// the SQLite write itself fails — disk full, db locked, fs perm —
/// the worktree state has already mutated and the on-disk layout
/// silently diverges from the registry record. Without
/// compensation the operator is left to manually reconcile two
/// pieces of state that nobody told them are now inconsistent.
///
/// This helper wraps the side-effect + persist pair so that on a
/// post-side-effect persist failure we run the inverse operation
/// (delete what we created, restore what we removed) before
/// returning the original error to the caller. Compensation is
/// best-effort and force-true on the destructive side — for any
/// direction where the helper just created the worktree, deletion
/// is safe because we own it; for the removal direction we use
/// the pre-remove SHA snapshot to restore the agent branch at its
/// original tip rather than re-fork it off HEAD.
///
/// Every step (side-effect success, persist failure, compensation
/// success, compensation failure) emits a structured `tracing` event
/// on the `alms.worktree` target so operators can reconstruct the
/// drift recovery from the daemon logs.
///
/// `persist` is taken as a closure so tests can inject a synthetic
/// SQLite failure without a real SQLite layer behind the AppState.
///
/// CRITICAL: compensation must NOT gate on the variant alone — both
/// `create_worktree` and `remove_worktree` are idempotent, so a
/// `Create` op can land in an `AlreadyExisted` outcome (operator's
/// previous worktree was already on disk from a crash / prior-failed
/// PATCH / manual creation), and a `Remove` / git→off `Flip` can
/// land in `AlreadyAbsent` (operator nuked the worktree dir by hand).
/// Running the inverse op against either of those would invent or
/// destroy state this handler did not touch — silent data loss /
/// silent state fabrication, the exact bug class #964 and the #1019
/// Codex P1s exist to prevent. We track whether the side-effect call
/// ACTUALLY mutated state (`did_create` / `did_remove`) and gate
/// compensation on those flags, not on the variant.
fn apply_worktree_op_and_persist(
    project_root: &StdPath,
    agent_name: &str,
    op: WorktreeOp,
    persist: impl FnOnce() -> AlmsResult<()>,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    // Default behaviour: PATCH-style mapping where every persist error
    // round-trips through `persist_failure_to_api` and surfaces as
    // `500 INTERNAL`. Callers that need a different wire shape on a
    // specific persist-error variant (POST → 409 DUPLICATE_NAME for
    // the create-race) should call
    // `apply_worktree_op_and_persist_with_mapper` directly. See #1022.
    apply_worktree_op_and_persist_with_mapper(project_root, agent_name, op, persist, |_| None)
}

/// Variant of `apply_worktree_op_and_persist` that lets callers
/// customise the wire shape for a specific persist-error variant
/// while keeping the helper's compensation framework intact.
///
/// `persist_err_mapper` is invoked AFTER any compensation succeeds
/// (so disk + registry are already back in sync); a `Some(shape)`
/// return overrides the helper's default `500 INTERNAL` mapping.
/// The dual-failure path (persist failed AND compensation failed)
/// always surfaces `500 WORKTREE_COMPENSATION_FAILED` regardless of
/// the mapper — the operator MUST know about disk drift, and that
/// signal outranks any specific persist-error category.
fn apply_worktree_op_and_persist_with_mapper(
    project_root: &StdPath,
    agent_name: &str,
    op: WorktreeOp,
    persist: impl FnOnce() -> AlmsResult<()>,
    persist_err_mapper: impl FnOnce(
        &alms_core::AlmsError,
    ) -> Option<(StatusCode, Json<serde_json::Value>)>,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    // Resolve the variant into a tuple of phase-1 intentions: do we
    // create, do we remove, and which "direction" string do we use
    // for the structured logs. POST is always off→git, DELETE is
    // always git→off; PATCH derives the direction from the mode
    // transition (same-mode flips short-circuit to "no side-effect"
    // and skip the entire compensation framework below).
    let (will_create, will_remove, force_remove, is_full_os_access_agent, direction) = match op {
        WorktreeOp::Create => (true, false, false, false, "off->git"),
        WorktreeOp::Remove { force } => (false, true, force, false, "git->off"),
        WorktreeOp::Flip {
            prev_mode,
            new_mode,
            force_remove,
            is_full_os_access_agent,
        } => {
            let is_off_to_git = matches!(
                (prev_mode, new_mode),
                (WorktreeMode::Off, WorktreeMode::Git)
            );
            let is_git_to_off = matches!(
                (prev_mode, new_mode),
                (WorktreeMode::Git, WorktreeMode::Off)
            );
            let dir = if is_off_to_git {
                "off->git"
            } else {
                "git->off"
            };
            (
                is_off_to_git,
                is_git_to_off,
                force_remove,
                is_full_os_access_agent,
                dir,
            )
        }
    };

    // For the removal direction we snapshot the branch HEAD BEFORE
    // calling `remove_worktree` (which destroys both the worktree
    // dir AND the `alms/<name>` branch). The snapshot lets the
    // compensation path below restore the branch at its original
    // tip if persist fails — without it, compensation would
    // re-fork a fresh branch off `HEAD`, silently losing every
    // commit the operator had on the agent branch. See #1019 /
    // Codex P1 (first finding).
    //
    // `read_branch_head_sha` returns `Ok(None)` when the branch is
    // missing (treated as "nothing to restore" — and combined with
    // the `WorktreeRemove::AlreadyAbsent` outcome from the symmetric
    // P1 fix, this means "no branch and no worktree to begin with"
    // → no compensation runs at all).
    let pre_remove_branch_sha: Option<String> = if will_remove {
        match worktree::read_branch_head_sha(project_root, agent_name) {
            Ok(maybe_sha) => maybe_sha,
            Err(e) => {
                // Snapshot probe failure is non-fatal — we log and
                // continue with `None`. Worst case the operator
                // loses a branch on a persist failure, but that's
                // strictly no worse than pre-#1019 behavior, and
                // this only fires on a misbehaving git binary.
                tracing::warn!(
                    target: "alms.worktree",
                    agent_name = %agent_name,
                    error = %e,
                    "Failed to snapshot branch HEAD before worktree removal; \
                     compensation on persist failure will fall back to fresh branch off HEAD."
                );
                None
            }
        }
    } else {
        None
    };

    // `did_create` / `did_remove` are set to `true` only when the
    // helper actually mutated on-disk state. Idempotent no-op
    // outcomes (`AlreadyExisted` / `AlreadyAbsent`) leave both
    // flags `false`, which short-circuits compensation below — the
    // handler owes no compensation for state it did not touch.
    let mut did_create = false;
    let mut did_remove = false;

    if will_create {
        match worktree::create_worktree(project_root, agent_name) {
            Ok(outcome) => {
                did_create = outcome.was_created();
                if !did_create {
                    // The worktree was already on disk before this
                    // call — likely operator drift from a prior
                    // crash or a previously-failed handler that
                    // left disk state behind. We did NOT mutate
                    // disk; compensation must NOT delete this
                    // directory on persist failure. See #1019 /
                    // Codex P1 (off→git side, second finding).
                    tracing::warn!(
                        target: "alms.worktree",
                        agent_name = %agent_name,
                        direction = direction,
                        "worktree was already on disk before the side-effect ran \
                         (operator drift / prior failed handler); \
                         compensation will skip destructive cleanup on persist failure."
                    );
                }
            }
            Err(e) => return Err(worktree_error_to_api(&e)),
        }
        if is_full_os_access_agent {
            tracing::warn!(
                target: "alms.security",
                agent_name = %agent_name,
                allow_full_os_access = true,
                worktree_mode = "git",
                "Agent '{}' PATCH attempting flip to worktree_mode=git AND is on \
                 [security].allow_full_os_access — worktree pin will be skipped \
                 at run time (security list wins).",
                agent_name,
            );
        }
    } else if will_remove {
        match worktree::remove_worktree(project_root, agent_name, force_remove) {
            Ok(outcome) => {
                did_remove = outcome.was_removed();
                if !did_remove {
                    // The worktree directory was not on disk before
                    // this call — operator drift, manual nuking,
                    // or a partial state from a previous failed
                    // handler. We did NOT mutate the directory;
                    // compensation must NOT recreate a fresh
                    // worktree+branch on persist failure (that
                    // would invent state this handler did not
                    // touch). See #1019 / Codex P1 (symmetric
                    // git→off side).
                    tracing::warn!(
                        target: "alms.worktree",
                        agent_name = %agent_name,
                        direction = direction,
                        "worktree was already absent before the side-effect ran \
                         (operator drift / manual cleanup); \
                         compensation will skip recreate on persist failure."
                    );
                }
            }
            Err(e) => return Err(worktree_error_to_api(&e)),
        }
    }

    // Phase 2: persist. On success, return — happy path is unchanged
    // from pre-#964.
    let persist_err = match persist() {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };

    // Phase 3: compensation. The persist failed AFTER the side-effect
    // touched disk; without compensation the agent record (still in
    // its pre-call state) and the disk layout (now mutated) silently
    // diverge. Run the inverse op, force=true on destructive sides
    // (we own the state we're undoing — no operator changes possible
    // during the millisecond between side-effect success and persist
    // failure).
    //
    // Codex P1 round 3 (#1019): `WorktreeRemove::AlreadyAbsent` does
    // NOT mean "no state was mutated" — `remove_worktree` calls
    // `delete_branch` on the no-op path too, so the `alms/<name>`
    // branch may already be gone even when `did_remove == false`. The
    // pre-remove SHA snapshot is the load-bearing signal here: if we
    // captured one before the side-effect ran, we know there was
    // operator branch history to lose, so the branch-restore arm must
    // run regardless of `did_remove`. Worktree-create / worktree-delete
    // arms still gate on `did_create` / `did_remove` because those
    // surfaces really are no-ops on the idempotent path.
    let needs_branch_restore = will_remove && pre_remove_branch_sha.is_some();

    if !did_create && !did_remove && !needs_branch_restore {
        // No side-effect to compensate. Cases that land here:
        //   - Same-mode PATCH (Off↔Off, Git↔Git, or any PATCH that
        //     does not flip worktree_mode).
        //   - Create where `create_worktree` was a no-op
        //     (`AlreadyExisted`) — the worktree was already on disk.
        //   - Remove where the worktree dir was already absent
        //     AND no `alms/<name>` branch existed before the call —
        //     so `delete_branch` had nothing to delete either.
        // In all three the helper did not mutate disk, so persist
        // failure alone never causes drift — just surface the error
        // (caller-specific mapping takes precedence so e.g. POST can
        // surface a race-DuplicateName as 409, not 500).
        return Err(persist_err_mapper(&persist_err)
            .unwrap_or_else(|| persist_failure_to_api(agent_name, &persist_err, None)));
    }

    let compensation = if did_create {
        // Create / off→git flip where `create_worktree` actually
        // provisioned disk state (not the `AlreadyExisted` no-op
        // path) failed mid-flight — the worktree we just created
        // is now stranded. Delete it. Force=true: it was a fresh
        // checkout, nothing to lose.
        worktree::remove_worktree(project_root, agent_name, true).map(|_| ())
    } else if did_remove {
        // Remove / git→off flip where `remove_worktree` actually
        // deleted disk state (not the `AlreadyAbsent` no-op path)
        // failed mid-flight — we already removed the worktree AND
        // deleted the `alms/<name>` branch (`remove_worktree`
        // calls `git branch -D` after `git worktree remove`). If
        // we have a pre-removal SHA snapshot, restore the branch
        // at that tip so the operator's commits come back intact;
        // if we don't (probe failed before remove), fall back to a
        // fresh `create_worktree` off HEAD. The fallback matches
        // pre-#1019 behavior; the happy path preserves branch
        // history. See #1019 / Codex P1 (first finding).
        match pre_remove_branch_sha.as_deref() {
            Some(sha) => {
                worktree::restore_worktree_at_sha(project_root, agent_name, sha).map(|_| ())
            }
            None => worktree::create_worktree(project_root, agent_name).map(|_| ()),
        }
    } else {
        // Remove / git→off flip where the worktree dir was already
        // absent (`WorktreeRemove::AlreadyAbsent`) but the
        // `alms/<name>` branch existed before the call AND
        // `remove_worktree`'s best-effort `delete_branch` may have
        // nuked it. We have a pre-remove SHA snapshot (else we
        // wouldn't be in this arm — see `needs_branch_restore`),
        // so restore the branch at that tip.
        // `restore_worktree_at_sha` is happy to recreate the
        // worktree dir alongside the branch; that's a strict
        // superset of "just put the branch back" and matches what
        // the operator had pre-call (registry was at Git, so the
        // worktree dir on disk was the documented happy-path
        // shape). See #1019 / Codex P1 (third finding).
        let sha = pre_remove_branch_sha
            .as_deref()
            .expect("needs_branch_restore => pre_remove_branch_sha is Some");
        worktree::restore_worktree_at_sha(project_root, agent_name, sha).map(|_| ())
    };

    match compensation {
        Ok(()) => {
            tracing::warn!(
                target: "alms.worktree",
                agent_name = %agent_name,
                direction = direction,
                persist_error = %persist_err,
                "/agents persist failed after worktree side-effect; \
                 compensation succeeded — disk + record both reverted to \
                 the pre-call state."
            );
            // Caller-specific mapping takes precedence so e.g. POST
            // can surface a race-DuplicateName as 409 DUPLICATE_NAME
            // rather than the default 500 INTERNAL — the worktree
            // has been cleaned up, so the wire shape can honestly
            // be the caller's specific category.
            Err(persist_err_mapper(&persist_err)
                .unwrap_or_else(|| persist_failure_to_api(agent_name, &persist_err, None)))
        }
        Err(comp_err) => {
            // Both errors matter — the operator needs to know the
            // SQLite row never moved AND the on-disk worktree is
            // also broken. Log both, surface both in the API
            // response.
            tracing::error!(
                target: "alms.worktree",
                agent_name = %agent_name,
                direction = direction,
                persist_error = %persist_err,
                compensation_error = %comp_err,
                "/agents persist failed after worktree side-effect AND \
                 compensation also failed — disk layout is now diverged from \
                 the registry. Manual cleanup required."
            );
            Err(persist_failure_to_api(
                agent_name,
                &persist_err,
                Some(&comp_err),
            ))
        }
    }
}

/// PATCH-specific thin shim over `apply_worktree_op_and_persist`.
///
/// Kept as a separate entry point so the 10-cell test matrix Tim
/// verified on PR #1019 keeps calling the same signature byte-for-byte
/// — the refactor for #1022 reuses the underlying compensation
/// framework without churning the PATCH wire shape or its tests.
fn apply_worktree_flip_and_persist(
    project_root: &StdPath,
    agent_name: &str,
    prev_mode: WorktreeMode,
    new_mode: WorktreeMode,
    force_remove: bool,
    is_full_os_access_agent: bool,
    persist: impl FnOnce() -> AlmsResult<()>,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    apply_worktree_op_and_persist(
        project_root,
        agent_name,
        WorktreeOp::Flip {
            prev_mode,
            new_mode,
            force_remove,
            is_full_os_access_agent,
        },
        persist,
    )
}

/// Map a post-side-effect persist failure (and optionally a
/// compensation failure) onto the wire shape. Distinct from
/// `worktree_error_to_api` because the failure shape — SQLite write
/// error after disk has moved — is qualitatively different from a
/// failed `git worktree add`: the operator may have to manually
/// clean up disk state before retrying.
fn persist_failure_to_api(
    agent_name: &str,
    persist_err: &alms_core::AlmsError,
    compensation_err: Option<&WorktreeError>,
) -> (StatusCode, Json<serde_json::Value>) {
    match compensation_err {
        None => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            format!(
                "agent '{agent_name}' worktree side-effect succeeded but the \
                 registry write failed; compensation reverted the worktree to \
                 its pre-call state, so disk and record are consistent. \
                 Underlying error: {persist_err}"
            ),
        ),
        Some(comp) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "WORKTREE_COMPENSATION_FAILED",
            format!(
                "agent '{agent_name}' worktree side-effect succeeded, the \
                 registry write failed, AND the compensating cleanup also \
                 failed — disk layout is now diverged from the agent record. \
                 Manual cleanup required. Persist error: {persist_err}. \
                 Compensation error: {comp}"
            ),
        ),
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

    // Snapshot the pre-PATCH worktree mode and the force flag so the
    // side-effecting branch below can reason about transitions.
    // `apply_update_request` mutates `agent.worktree_mode` in place
    // for the new desired state.
    let prev_mode = agent.worktree_mode;
    let force_remove = req.force_worktree_remove.unwrap_or(false);
    let agent_name = agent.name.clone();

    apply_update_request(&mut agent, req).map_err(|e| e.to_api_error())?;

    // Cross-field pair invariant for the per-agent summary fields (#872).
    // Validated against the post-update record state so a single-field
    // PATCH that lands the row in an asymmetric shape is rejected even
    // when the live record has the other half set. Mirrors the
    // server-level `SUMMARY_PROVIDER_REQUIRES_MODEL` /
    // `SUMMARY_MODEL_REQUIRES_PROVIDER` check in `settings.rs`.
    validate_summary_pair(
        &state,
        agent.summary_provider.as_deref(),
        agent.summary_model.as_deref(),
    )?;

    // Worktree-mode flip side-effect + atomic persist (#946 / #964).
    // The helper handles compensation if the persist fails after the
    // side-effect has touched disk, so disk + registry stay
    // consistent on every error path.
    let new_mode = agent.worktree_mode;
    let is_full_os = state.security_config.is_full_os_access_agent(&agent_name);
    apply_worktree_flip_and_persist(
        &state.project_root,
        &agent_name,
        prev_mode,
        new_mode,
        force_remove,
        is_full_os,
        || store.update_agent(&agent),
    )?;

    Ok(Json(agent_to_json(&agent)))
}

/// Query params for `DELETE /agents/{id_or_name}` (#946).
///
/// `force=true` is consumed by the worktree-removal step — when set
/// it overrides the uncommitted-changes guard inside
/// `worktree::remove_worktree`, discarding the worktree contents
/// AND deleting the `alms/<name>` branch. Defaults to `false`.
#[derive(Debug, Default, serde::Deserialize)]
pub struct DeleteAgentQuery {
    #[serde(default)]
    pub force: bool,
}

/// DELETE /agents/{id_or_name} — delete an agent.
pub async fn delete_agent(
    State(state): State<AppState>,
    Path(id_or_name): Path<String>,
    Query(query): Query<DeleteAgentQuery>,
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

    // Worktree teardown (#946). Runs BEFORE the SQLite delete so an
    // uncommitted-changes refusal surfaces a 4xx without orphaning
    // the agent record. `force` from the query string overrides the
    // uncommitted-changes guard inside `remove_worktree`.
    //
    // Routed through `apply_worktree_op_and_persist` (#1022 — extended
    // from #1019's PATCH compensation framework) so any
    // post-side-effect SQLite-delete failure (db locked, disk full,
    // fs perm) gets the just-removed worktree restored at its
    // pre-call SHA before the error surfaces to the client. Pre-#1022
    // a `delete_agent` SQLite failure left the agent record in place
    // but the worktree gone — operator hit a silent half-deleted
    // state. The helper's branch-SHA snapshot preserves agent commit
    // history across the round trip.
    if agent.worktree_mode == WorktreeMode::Git {
        let agent_name = agent.name.clone();
        let agent_id = agent.id;
        let project_root = state.project_root.clone();
        let store_ref = store.clone();
        apply_worktree_op_and_persist(
            &project_root,
            &agent_name,
            WorktreeOp::Remove { force: query.force },
            || store_ref.delete_agent(agent_id).map(|_| ()),
        )?;
    } else {
        // worktree_mode = Off — no on-disk side-effect to compose
        // around, delete directly. Preserves pre-#1022 wire shape
        // for the (overwhelmingly common) non-worktree delete path.
        store
            .delete_agent(agent.id)
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", e))?;
    }

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
    use crate::test_support::TestAppState;
    use alms_session::SqliteStore;

    #[test]
    fn optional_overrides_trim_values_and_reject_empty_sentinels() {
        assert_eq!(normalize_optional_override(None), None);
        assert_eq!(normalize_optional_override(Some("")), None);
        assert_eq!(normalize_optional_override(Some("   ")), None);
        assert_eq!(
            normalize_optional_override(Some("  openai  ")),
            Some("openai".to_string())
        );
    }

    fn new_agent(name: &str) -> AgentRecord {
        AgentRecord::for_test(name)
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
    /// the `resolve_agent_config` precedence test relies on: once the
    /// SQLite record's knob is `None`, the agent-layer precedence falls
    /// through to the server default.
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
        // `resolve_agent_config` will see on a subsequent POST /runs,
        // and that's what makes the precedence fall through to the
        // server default.
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

    // ==================================================================
    // `debug_mode` (#1003) — agent_to_json + apply_update_request +
    // CRUD round-trip
    //
    // Unlike the reasoning knobs above, `debug_mode` is a plain
    // `bool` (not `Option<bool>`) on `AgentRecord`, so the wire shape
    // always includes the field regardless of value. There is no
    // `clear_*` sentinel — `false` is itself the cleared / default
    // state. PATCH semantics: omitting the field on PATCH leaves the
    // existing value unchanged; sending `Some(true)` / `Some(false)`
    // writes through.
    // ==================================================================

    #[test]
    fn agent_to_json_always_emits_debug_mode() {
        // `debug_mode = false` (the default) must still appear on the
        // wire so the UI can populate the toggle without a separate
        // GET round-trip when the agent has never been touched.
        let agent_off = new_agent("debug-off");
        let json_off = agent_to_json(&agent_off);
        assert_eq!(
            json_off["debug_mode"],
            serde_json::json!(false),
            "debug_mode = false must appear on the wire"
        );

        let mut agent_on = new_agent("debug-on");
        agent_on.debug_mode = true;
        let json_on = agent_to_json(&agent_on);
        assert_eq!(
            json_on["debug_mode"],
            serde_json::json!(true),
            "debug_mode = true must appear on the wire"
        );
    }

    #[test]
    fn apply_update_request_flips_debug_mode_to_true() {
        let mut agent = new_agent("debugger");
        assert!(!agent.debug_mode);

        let req = UpdateAgentRequest {
            debug_mode: Some(true),
            ..Default::default()
        };
        apply_update_request(&mut agent, req).unwrap();
        assert!(
            agent.debug_mode,
            "apply_update_request must flip debug_mode to true"
        );
    }

    #[test]
    fn apply_update_request_flips_debug_mode_to_false() {
        let mut agent = new_agent("debugger");
        agent.debug_mode = true;

        let req = UpdateAgentRequest {
            debug_mode: Some(false),
            ..Default::default()
        };
        apply_update_request(&mut agent, req).unwrap();
        assert!(
            !agent.debug_mode,
            "apply_update_request must flip debug_mode to false"
        );
    }

    #[test]
    fn apply_update_request_leaves_debug_mode_unchanged_when_omitted() {
        // Omitting `debug_mode` on PATCH (`None` on the wire) leaves
        // the stored value alone — same shape as the rest of the
        // mutable knobs. Run the assertion in both directions to make
        // sure the behaviour is symmetric.
        let mut agent_on = new_agent("on-stays-on");
        agent_on.debug_mode = true;
        apply_update_request(&mut agent_on, UpdateAgentRequest::default()).unwrap();
        assert!(
            agent_on.debug_mode,
            "omitted debug_mode must leave true unchanged"
        );

        let mut agent_off = new_agent("off-stays-off");
        agent_off.debug_mode = false;
        apply_update_request(&mut agent_off, UpdateAgentRequest::default()).unwrap();
        assert!(
            !agent_off.debug_mode,
            "omitted debug_mode must leave false unchanged"
        );
    }

    #[test]
    fn create_agent_request_debug_mode_default_is_false() {
        // `CreateAgentRequest::debug_mode` is `Option<bool>` and
        // defaults to `None` on the wire (clients can omit the field
        // entirely). The create handler maps `None` -> `false` so
        // pre-#1003 client bundles continue to create non-debug
        // agents with no operator action.
        let req: CreateAgentRequest = serde_json::from_value(serde_json::json!({
            "name": "from-old-client",
        }))
        .unwrap();
        assert!(
            req.debug_mode.is_none(),
            "Pre-#1003 client wire shape must deserialize with debug_mode = None"
        );

        // The handler's `unwrap_or(false)` lands the persisted record
        // at debug_mode = false. (Exercised end-to-end by the route
        // integration tests below; the unit-level proof is the
        // `unwrap_or(false)` site in `create_agent`.)
    }

    #[test]
    fn create_agent_request_explicit_debug_mode_round_trips() {
        // Clients that DO set debug_mode on create must see the value
        // survive the `Option<bool> -> bool` mapping.
        let req: CreateAgentRequest = serde_json::from_value(serde_json::json!({
            "name": "with-debug",
            "debug_mode": true,
        }))
        .unwrap();
        assert_eq!(
            req.debug_mode,
            Some(true),
            "Explicit debug_mode = true must deserialize as Some(true)"
        );
    }

    #[test]
    fn debug_mode_round_trips_through_sqlite_and_patch_chain() {
        // End-to-end: insert an agent, simulate `PATCH /agents/{id}
        // { "debug_mode": true }` by deserialising the request body
        // through `UpdateAgentRequest` (the same path the axum handler
        // uses), apply via `apply_update_request`, persist, reload —
        // assert the value reaches disk and comes back. This is what
        // `resolve_agent_config` will see on the next `POST /runs`,
        // and it's the only path that lands `cfg.debug_mode = true`
        // for runtime emission of the `context_debug` SSE event.
        let store = SqliteStore::open_in_memory().unwrap();

        // Fresh agent — debug_mode defaults to false.
        let agent = new_agent("debug-roundtrip");
        store.create_agent(&agent).unwrap();
        let loaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert!(
            !loaded.debug_mode,
            "fresh agents must default to debug_mode = false"
        );

        // PATCH `{"debug_mode": true}` via the same JSON shape the
        // axum handler accepts.
        let req: UpdateAgentRequest = serde_json::from_value(serde_json::json!({
            "debug_mode": true,
        }))
        .unwrap();
        assert_eq!(
            req.debug_mode,
            Some(true),
            "PATCH JSON `{{\"debug_mode\": true}}` must deserialize as Some(true)"
        );
        let mut updated = loaded;
        apply_update_request(&mut updated, req).unwrap();
        store.update_agent(&updated).unwrap();

        let reloaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert!(
            reloaded.debug_mode,
            "PATCH /agents/{{id}} {{debug_mode: true}} must round-trip through SQLite"
        );

        // PATCH back to false — same path.
        let req: UpdateAgentRequest = serde_json::from_value(serde_json::json!({
            "debug_mode": false,
        }))
        .unwrap();
        assert_eq!(req.debug_mode, Some(false));
        let mut updated = reloaded;
        apply_update_request(&mut updated, req).unwrap();
        store.update_agent(&updated).unwrap();

        let reloaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert!(
            !reloaded.debug_mode,
            "PATCH /agents/{{id}} {{debug_mode: false}} must round-trip back to false"
        );

        // Omitting the field on a subsequent PATCH must leave the
        // value alone — pin the "PATCH-without-debug_mode is a no-op
        // on this field" contract that the UI's diff-on-Apply
        // logic depends on.
        store
            .update_agent(&{
                let mut a = reloaded;
                a.debug_mode = true; // pre-flip the stored value
                a
            })
            .unwrap();
        let req: UpdateAgentRequest = serde_json::from_value(serde_json::json!({
            "description": "unrelated change",
        }))
        .unwrap();
        assert!(
            req.debug_mode.is_none(),
            "Omitted debug_mode in PATCH body must deserialize as None"
        );
        let mut loaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert!(loaded.debug_mode);
        apply_update_request(&mut loaded, req).unwrap();
        store.update_agent(&loaded).unwrap();
        let reloaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert!(
            reloaded.debug_mode,
            "PATCH that omits debug_mode must leave the stored value unchanged"
        );
        assert_eq!(reloaded.description, "unrelated change");
    }

    // ==================================================================
    // Per-agent summary provider/model (#872)
    //
    // Pair-only validation runs in two layers:
    //   - `apply_update_request` (this layer) handles the per-field
    //     `clear_*` sentinel + empty-string rejection. Cross-field pair
    //     invariant is delegated to `validate_summary_pair` in the HTTP
    //     handler (because that needs `AppState` to verify provider
    //     existence and key resolvability).
    // ==================================================================

    #[test]
    fn apply_update_sets_summary_provider_and_model_together() {
        let mut agent = new_agent("summary-set");
        let req = UpdateAgentRequest {
            summary_provider: Some("openrouter".into()),
            summary_model: Some("minimax/minimax-m2.7".into()),
            ..Default::default()
        };
        apply_update_request(&mut agent, req).unwrap();
        assert_eq!(agent.summary_provider.as_deref(), Some("openrouter"));
        assert_eq!(agent.summary_model.as_deref(), Some("minimax/minimax-m2.7"));
    }

    #[test]
    fn apply_update_clears_summary_provider_and_model() {
        let mut agent = new_agent("summary-clear");
        agent.summary_provider = Some("openrouter".into());
        agent.summary_model = Some("minimax/minimax-m2.7".into());
        let req = UpdateAgentRequest {
            clear_summary_provider: Some(true),
            clear_summary_model: Some(true),
            ..Default::default()
        };
        apply_update_request(&mut agent, req).unwrap();
        assert!(agent.summary_provider.is_none());
        assert!(agent.summary_model.is_none());
    }

    #[test]
    fn apply_update_rejects_clear_and_value_together_summary_provider() {
        let mut agent = new_agent("test");
        let req = UpdateAgentRequest {
            summary_provider: Some("openrouter".into()),
            clear_summary_provider: Some(true),
            ..Default::default()
        };
        let err = apply_update_request(&mut agent, req).unwrap_err();
        assert!(matches!(
            err,
            UpdateAgentError::ClearAndValueConflict("summary_provider")
        ));
    }

    #[test]
    fn apply_update_rejects_clear_and_value_together_summary_model() {
        let mut agent = new_agent("test");
        let req = UpdateAgentRequest {
            summary_model: Some("anthropic/claude-haiku-4".into()),
            clear_summary_model: Some(true),
            ..Default::default()
        };
        let err = apply_update_request(&mut agent, req).unwrap_err();
        assert!(matches!(
            err,
            UpdateAgentError::ClearAndValueConflict("summary_model")
        ));
    }

    #[test]
    fn apply_update_rejects_empty_summary_provider() {
        // `""` for `summary_provider` is rejected — operators must use
        // `clear_summary_provider: true` so the pair-only invariant can
        // be validated cleanly. This is intentionally stricter than the
        // empty-string-as-clear convention used for `model`/`provider`/
        // `posture` (those fields have no pair-only invariant).
        let mut agent = new_agent("test");
        let req = UpdateAgentRequest {
            summary_provider: Some(String::new()),
            ..Default::default()
        };
        let err = apply_update_request(&mut agent, req).unwrap_err();
        assert!(matches!(
            err,
            UpdateAgentError::EmptySummaryField("summary_provider")
        ));
        let (status, _) = err.to_api_error();
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn apply_update_rejects_empty_summary_model() {
        let mut agent = new_agent("test");
        let req = UpdateAgentRequest {
            summary_model: Some(String::new()),
            ..Default::default()
        };
        let err = apply_update_request(&mut agent, req).unwrap_err();
        assert!(matches!(
            err,
            UpdateAgentError::EmptySummaryField("summary_model")
        ));
    }

    #[test]
    fn apply_update_summary_fields_are_trimmed() {
        // Non-empty values are trimmed so accidental leading/trailing
        // whitespace doesn't slip onto the wire as a malformed model
        // slug. Empty after trim is treated the same as empty from the
        // start — rejected with EmptySummaryField, not silently mapped
        // to None.
        let mut agent = new_agent("test");
        let req = UpdateAgentRequest {
            summary_provider: Some("  openrouter  ".into()),
            summary_model: Some("  minimax/minimax-m2.7  ".into()),
            ..Default::default()
        };
        apply_update_request(&mut agent, req).unwrap();
        assert_eq!(agent.summary_provider.as_deref(), Some("openrouter"));
        assert_eq!(agent.summary_model.as_deref(), Some("minimax/minimax-m2.7"));
    }

    #[test]
    fn agent_to_json_emits_summary_fields_when_some() {
        let mut agent = new_agent("with-summary");
        agent.summary_provider = Some("openrouter".into());
        agent.summary_model = Some("minimax/minimax-m2.7".into());
        let json = agent_to_json(&agent);
        assert_eq!(json["summary_provider"], serde_json::json!("openrouter"));
        assert_eq!(
            json["summary_model"],
            serde_json::json!("minimax/minimax-m2.7")
        );
    }

    #[test]
    fn agent_to_json_omits_summary_fields_when_none() {
        // Default `new_agent("...")` leaves both fields None so the
        // JSON shape must omit the keys entirely — matches the existing
        // pattern for `provider`/`posture`/`reasoning_effort`.
        let agent = new_agent("no-summary");
        let json = agent_to_json(&agent);
        let obj = json.as_object().unwrap();
        assert!(
            !obj.contains_key("summary_provider"),
            "summary_provider must be omitted when None: {json}"
        );
        assert!(
            !obj.contains_key("summary_model"),
            "summary_model must be omitted when None: {json}"
        );
    }

    #[test]
    fn summary_fields_round_trip_through_sqlite_with_apply_update() {
        // End-to-end through the gateway-side wrapper: insert an agent,
        // set both fields via PATCH, reload from the store, verify both
        // round-trip; then clear both via the dedicated sentinels,
        // reload, verify both are None. Mirrors
        // `clear_sentinels_round_trip_through_sqlite` but for the new
        // #872 fields.
        let store = SqliteStore::open_in_memory().unwrap();
        let agent = new_agent("round-trip-summary");
        store.create_agent(&agent).unwrap();

        // Set both fields together.
        let mut loaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        let req = UpdateAgentRequest {
            summary_provider: Some("openrouter".into()),
            summary_model: Some("minimax/minimax-m2.7".into()),
            ..Default::default()
        };
        apply_update_request(&mut loaded, req).unwrap();
        store.update_agent(&loaded).unwrap();
        let reloaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert_eq!(reloaded.summary_provider.as_deref(), Some("openrouter"));
        assert_eq!(
            reloaded.summary_model.as_deref(),
            Some("minimax/minimax-m2.7")
        );

        // Clear both fields together.
        let mut loaded = reloaded;
        let req = UpdateAgentRequest {
            clear_summary_provider: Some(true),
            clear_summary_model: Some(true),
            ..Default::default()
        };
        apply_update_request(&mut loaded, req).unwrap();
        store.update_agent(&loaded).unwrap();
        let reloaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert!(reloaded.summary_provider.is_none());
        assert!(reloaded.summary_model.is_none());
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

    // ==================================================================
    // HTTP-level coverage for per-agent SUMMARY_PROVIDER_UNKNOWN /
    // SUMMARY_PROVIDER_MISSING_API_KEY (#878)
    //
    // The PATCH /settings layer in `settings.rs` already has axum
    // oneshot-style coverage for `SUMMARY_PROVIDER_UNKNOWN` and
    // `SUMMARY_PROVIDER_MISSING_API_KEY`. The per-agent CRUD layer wires
    // the same `validate_summary_pair` against `state.llm_config` /
    // `state.secrets`, but until #878 only the mutual-exclusion /
    // `clear_*` sentinel paths had explicit unit tests — the
    // provider-existence and key-resolvability paths were verified by
    // code inspection only. These tests close that gap so a regression
    // in `validate_summary_pair` cannot land silently.
    //
    // Test matrix:
    //   1. POST /agents with provider="nonexistent"
    //      -> 400 SUMMARY_PROVIDER_UNKNOWN
    //   2. POST /agents with provider that's configured but has no key
    //      -> 400 SUMMARY_PROVIDER_MISSING_API_KEY
    //   3. PUT /agents/{id} (the same two cases via update path)
    //   4. POST /agents with happy path -> 201 Created (both configured
    //      AND a key resolvable from the secrets store)
    //   5. POST /agents with an asymmetric pair (#877 mirror at the
    //      per-agent layer) -> 400 SUMMARY_PROVIDER_REQUIRES_MODEL /
    //      SUMMARY_MODEL_REQUIRES_PROVIDER
    // ==================================================================

    /// Build an `AppState` with a SQLite-backed agent registry AND a
    /// custom `[llm.providers]` map so we can drive the
    /// `validate_summary_pair` provider-existence / key-resolvability
    /// branches deterministically. Mirrors
    /// `runs::integration_tests::test_app_state_with_sqlite` but with
    /// the plumbing for the four channels collapsed into `_` since the
    /// per-agent CRUD path doesn't drive any of them.
    fn agents_test_app_state_with_sqlite() -> crate::server::AppState {
        TestAppState::new().in_memory_sqlite().build()
    }

    /// Inject an `[llm.providers.openrouter]` entry into the test state
    /// AND seed an API key for it in the secrets store. The summary
    /// provider validator passes both the `SUMMARY_PROVIDER_UNKNOWN`
    /// (entry exists) and `SUMMARY_PROVIDER_MISSING_API_KEY` (key
    /// resolves) checks, so the cross-field pair invariant or other
    /// validators are what we exercise on top.
    fn inject_openrouter_provider_with_key(state: &mut crate::server::AppState) {
        state.llm_config.providers.insert(
            "openrouter".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::OpenAiCompatible,
                base_url: "https://openrouter.ai/api/v1".into(),
                api_key_env: None,
                api_key: None,
                model: None,
                auth_scheme: alms_core::config::AuthScheme::Bearer,
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        state
            .secrets
            .write()
            .set_key("openrouter", "sk-or-test")
            .unwrap();
    }

    /// Inject a provider entry that exists but has NO resolvable key —
    /// neither in the entry's own `api_key_env`/`api_key` fields nor in
    /// the secrets store. Used to drive `SUMMARY_PROVIDER_MISSING_API_KEY`.
    fn inject_anthropic_provider_without_key(state: &mut crate::server::AppState) {
        state.llm_config.providers.insert(
            "anthropic".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::OpenAiCompatible,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: None,
                model: None,
                auth_scheme: alms_core::config::AuthScheme::Bearer,
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        // Make sure the secrets store does NOT have a key for this
        // provider — `agents_test_app_state_with_sqlite` returns a
        // freshly-constructed state, but be defensive in case future
        // refactors thread a non-empty store through.
        let _ = state.secrets.write().remove_key("anthropic");
    }

    /// Helper: invoke `create_agent` and unwrap the JSON error payload.
    /// The handler returns a `Result<(StatusCode, Json), (StatusCode,
    /// Json)>` — both arms carry a JSON body, but only the error arm
    /// wraps the well-known `error.code` shape we want to assert on.
    async fn create_agent_err(
        state: crate::server::AppState,
        req: alms_core::CreateAgentRequest,
    ) -> (axum::http::StatusCode, serde_json::Value) {
        let result = create_agent(axum::extract::State(state), Json(req)).await;
        let (status, body) = result.expect_err("expected create_agent to return Err");
        (status, body.0)
    }

    async fn update_agent_err(
        state: crate::server::AppState,
        id_or_name: String,
        req: alms_core::UpdateAgentRequest,
    ) -> (axum::http::StatusCode, serde_json::Value) {
        let result = update_agent(
            axum::extract::State(state),
            axum::extract::Path(id_or_name),
            Json(req),
        )
        .await;
        let body = result.expect_err("expected update_agent to return Err");
        (body.0, body.1.0)
    }

    /// Helper: extract the well-known `error.code` field from an
    /// `api_error`-shaped JSON payload, matching the shape produced by
    /// `crate::api_error`. Panics if the structure is unexpected so
    /// failed tests print a clear message.
    fn err_code(json: &serde_json::Value) -> &str {
        json.get("error")
            .and_then(|e| e.get("code"))
            .and_then(|c| c.as_str())
            .expect("error response missing error.code field")
    }

    /// Helper: a minimal `CreateAgentRequest` carrying only a name.
    fn create_req(name: &str) -> alms_core::CreateAgentRequest {
        alms_core::CreateAgentRequest {
            name: name.into(),
            description: None,
            model: None,
            posture: None,
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
            worktree_mode: None,
            debug_mode: None,
            is_default: None,
        }
    }

    /// #2: `POST /agents` accepts uppercase and stores the operator's
    /// casing verbatim.
    #[tokio::test]
    async fn post_agents_accepts_uppercase_and_preserves_the_operators_casing() {
        let state = agents_test_app_state_with_sqlite();

        let (status, body) = create_agent(
            axum::extract::State(state.clone()),
            Json(create_req("Atlas")),
        )
        .await
        .expect("uppercase names are valid since #2");
        assert_eq!(status, axum::http::StatusCode::CREATED);
        assert_eq!(body.0["name"], serde_json::json!("Atlas"));

        let store = state.session_manager.store().expect("sqlite store");
        assert_eq!(
            store.load_agent_by_name("Atlas").unwrap().unwrap().name,
            "Atlas"
        );
    }

    /// #2: `Atlas` and `atlas` cannot coexist.
    ///
    /// The workspace directory is `{workspace_dir}/{name}/`, so two such
    /// records would share one directory on Windows/macOS and split into two
    /// on Linux. This is the collision that would otherwise ship as a silent,
    /// platform-dependent bug, so pin the wire shape the operator sees:
    /// `409 DUPLICATE_NAME`, same as an exact-name clash.
    #[tokio::test]
    async fn post_agents_rejects_a_name_that_differs_only_in_case() {
        let state = agents_test_app_state_with_sqlite();
        let _ = create_agent(
            axum::extract::State(state.clone()),
            Json(create_req("Atlas")),
        )
        .await
        .expect("first create must succeed");

        for collision in ["atlas", "ATLAS", "aTlAs"] {
            let (status, body) = create_agent_err(state.clone(), create_req(collision)).await;
            assert_eq!(
                status,
                axum::http::StatusCode::CONFLICT,
                "{collision} must conflict with 'Atlas'"
            );
            assert_eq!(err_code(&body), "DUPLICATE_NAME");
        }

        // Exactly one record, still spelled the operator's way.
        let store = state.session_manager.store().expect("sqlite store");
        let agents = store.list_agents().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "Atlas");
    }

    /// #2: `DM` / `Default` / `Workspace` are reserved in any casing.
    ///
    /// The reserved list exists because these names collide with API
    /// sub-route segments and internal `context_id` prefixes. An exact-match
    /// guard would have let `DM` through the moment uppercase became legal —
    /// the other silent-bug candidate in this change.
    #[tokio::test]
    async fn post_agents_rejects_reserved_names_in_any_casing() {
        let state = agents_test_app_state_with_sqlite();
        for name in ["DM", "Dm", "Default", "DEFAULT", "Workspace", "WORKSPACE"] {
            let (status, body) = create_agent_err(state.clone(), create_req(name)).await;
            assert_eq!(
                status,
                axum::http::StatusCode::BAD_REQUEST,
                "{name} must be refused as reserved"
            );
            assert_eq!(err_code(&body), "INVALID_NAME");
        }
        let store = state.session_manager.store().expect("sqlite store");
        assert!(store.list_agents().unwrap().is_empty());
    }

    /// #2: `GET /agents/{name}` resolves case-insensitively and answers with
    /// the stored casing.
    ///
    /// Chosen semantics: case-insensitive lookup, case-preserving storage.
    /// The alternative (exact-match lookup) would make an agent that the
    /// registry says is unique reachable under only one of its spellings,
    /// while `send_message` and `invoke_agent` — both of which funnel through
    /// the same `load_agent_by_name` — happily resolved either.
    #[tokio::test]
    async fn get_agent_by_name_resolves_case_insensitively() {
        let state = agents_test_app_state_with_sqlite();
        let _ = create_agent(
            axum::extract::State(state.clone()),
            Json(create_req("Atlas")),
        )
        .await
        .expect("create must succeed");

        for spelling in ["Atlas", "atlas", "ATLAS"] {
            let body = get_agent(
                axum::extract::State(state.clone()),
                axum::extract::Path(spelling.to_string()),
            )
            .await
            .unwrap_or_else(|e| panic!("GET /agents/{spelling} must resolve: {e:?}"));
            assert_eq!(body.0["name"], serde_json::json!("Atlas"));
        }

        // Not a prefix match — resolution is still whole-name.
        assert!(
            get_agent(
                axum::extract::State(state.clone()),
                axum::extract::Path("atl".to_string()),
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn post_agents_rejects_unknown_summary_provider() {
        // No `[llm.providers.nonexistent]` entry exists, so the validator
        // must reject before any DB write.
        let state = agents_test_app_state_with_sqlite();
        let req = alms_core::CreateAgentRequest {
            name: "bad-summary-prov".into(),
            description: None,
            model: None,
            posture: None,
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: Some("nonexistent".into()),
            summary_model: Some("some-model".into()),
            worktree_mode: None,
            debug_mode: None,
            is_default: None,
        };
        let (status, body) = create_agent_err(state.clone(), req).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err_code(&body), "SUMMARY_PROVIDER_UNKNOWN");

        // Defense in depth: nothing was written to the registry.
        let store = state.session_manager.store().expect("sqlite store");
        assert!(
            store
                .load_agent_by_name("bad-summary-prov")
                .unwrap()
                .is_none(),
            "rejected POST must not commit the agent record"
        );
    }

    #[tokio::test]
    async fn post_agents_rejects_summary_provider_without_api_key() {
        // Provider is configured under `[llm.providers.<name>]` but no
        // key resolves from either the entry or the secrets store. This
        // is the exact run-time failure mode #878 asks us to surface
        // cleanly at create time so the operator hears about it before
        // a summary task ever fires.
        let mut state = agents_test_app_state_with_sqlite();
        inject_anthropic_provider_without_key(&mut state);
        let req = alms_core::CreateAgentRequest {
            name: "no-key-agent".into(),
            description: None,
            model: None,
            posture: None,
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: Some("anthropic".into()),
            summary_model: Some("claude-haiku-4".into()),
            worktree_mode: None,
            debug_mode: None,
            is_default: None,
        };
        let (status, body) = create_agent_err(state.clone(), req).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err_code(&body), "SUMMARY_PROVIDER_MISSING_API_KEY");
    }

    #[tokio::test]
    async fn post_agents_rejects_summary_provider_without_model() {
        // Asymmetric pair (provider set, model unset) — same shape #877
        // closes at the server-level config-load layer, mirrored here at
        // the per-agent layer.
        let mut state = agents_test_app_state_with_sqlite();
        inject_openrouter_provider_with_key(&mut state);
        let req = alms_core::CreateAgentRequest {
            name: "asymmetric-a".into(),
            description: None,
            model: None,
            posture: None,
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: Some("openrouter".into()),
            summary_model: None,
            worktree_mode: None,
            debug_mode: None,
            is_default: None,
        };
        let (status, body) = create_agent_err(state, req).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err_code(&body), "SUMMARY_PROVIDER_REQUIRES_MODEL");
    }

    #[tokio::test]
    async fn post_agents_rejects_summary_model_without_provider() {
        let mut state = agents_test_app_state_with_sqlite();
        inject_openrouter_provider_with_key(&mut state);
        let req = alms_core::CreateAgentRequest {
            name: "asymmetric-b".into(),
            description: None,
            model: None,
            posture: None,
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: Some("minimax/minimax-m2.7".into()),
            worktree_mode: None,
            debug_mode: None,
            is_default: None,
        };
        let (status, body) = create_agent_err(state, req).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err_code(&body), "SUMMARY_MODEL_REQUIRES_PROVIDER");
    }

    #[tokio::test]
    async fn post_agents_accepts_summary_pair_with_resolvable_key() {
        // Happy path: provider entry exists AND has a resolvable key.
        // Both fields land in the persisted record verbatim.
        let mut state = agents_test_app_state_with_sqlite();
        inject_openrouter_provider_with_key(&mut state);
        let req = alms_core::CreateAgentRequest {
            name: "happy-summary".into(),
            description: None,
            model: None,
            posture: None,
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: Some("openrouter".into()),
            summary_model: Some("minimax/minimax-m2.7".into()),
            worktree_mode: None,
            debug_mode: None,
            is_default: None,
        };
        let (status, body) = create_agent(axum::extract::State(state.clone()), Json(req))
            .await
            .expect("happy path must succeed");
        assert_eq!(status, axum::http::StatusCode::CREATED);
        assert_eq!(body.0["summary_provider"], "openrouter");
        assert_eq!(body.0["summary_model"], "minimax/minimax-m2.7");

        // Round-trip: the registry-stored record carries both fields.
        let store = state.session_manager.store().expect("sqlite store");
        let stored = store.load_agent_by_name("happy-summary").unwrap().unwrap();
        assert_eq!(stored.summary_provider.as_deref(), Some("openrouter"));
        assert_eq!(
            stored.summary_model.as_deref(),
            Some("minimax/minimax-m2.7")
        );
    }

    #[tokio::test]
    async fn put_agents_rejects_unknown_summary_provider() {
        // PATCH path mirror: an agent is already in the registry with
        // both summary fields unset; an UPDATE that sets `summary_provider
        // = "nonexistent"` must reject with `SUMMARY_PROVIDER_UNKNOWN`.
        let state = agents_test_app_state_with_sqlite();
        let store = state.session_manager.store().expect("sqlite store");
        let agent = new_agent("update-target");
        store.create_agent(&agent).unwrap();

        let req = alms_core::UpdateAgentRequest {
            summary_provider: Some("nonexistent".into()),
            summary_model: Some("some-model".into()),
            ..Default::default()
        };
        let (status, body) = update_agent_err(state, agent.id.to_string(), req).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err_code(&body), "SUMMARY_PROVIDER_UNKNOWN");
    }

    #[tokio::test]
    async fn put_agents_rejects_summary_provider_without_api_key() {
        // PATCH path mirror of the missing-key check. Even when the
        // entry-exists check passes, the resolvable-key check must fire
        // before the agent record is updated.
        let mut state = agents_test_app_state_with_sqlite();
        inject_anthropic_provider_without_key(&mut state);
        let store = state.session_manager.store().expect("sqlite store");
        let agent = new_agent("missing-key-target");
        store.create_agent(&agent).unwrap();

        let req = alms_core::UpdateAgentRequest {
            summary_provider: Some("anthropic".into()),
            summary_model: Some("claude-haiku-4".into()),
            ..Default::default()
        };
        let (status, body) = update_agent_err(state.clone(), agent.id.to_string(), req).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err_code(&body), "SUMMARY_PROVIDER_MISSING_API_KEY");

        // Defense in depth: the agent record must be untouched.
        let stored = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert!(
            stored.summary_provider.is_none(),
            "rejected PATCH must not commit summary_provider"
        );
        assert!(
            stored.summary_model.is_none(),
            "rejected PATCH must not commit summary_model"
        );
    }

    #[tokio::test]
    async fn put_agents_runtime_path_surfaces_missing_key_when_key_was_removed() {
        // The runtime case Atlas described in #878: agent was created
        // when the API key was set; the operator removes the key from
        // the secrets store; subsequent PATCH attempts touching the
        // summary fields must surface `MISSING_API_KEY` cleanly. This
        // is the second layer the load-time validator (#877) cannot
        // cover.
        let mut state = agents_test_app_state_with_sqlite();
        inject_openrouter_provider_with_key(&mut state);
        let store = state.session_manager.store().expect("sqlite store");

        // Create an agent with the summary pair set while the key is
        // still resolvable.
        let req = alms_core::CreateAgentRequest {
            name: "key-removed".into(),
            description: None,
            model: None,
            posture: None,
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: Some("openrouter".into()),
            summary_model: Some("minimax/minimax-m2.7".into()),
            worktree_mode: None,
            debug_mode: None,
            is_default: None,
        };
        let (_status, _body) = create_agent(axum::extract::State(state.clone()), Json(req))
            .await
            .expect("create must succeed while key is present");

        // Operator removes the key — the entry remains configured but
        // there's no resolvable secret. Any PATCH that touches the
        // summary fields must reject.
        state.secrets.write().remove_key("openrouter").unwrap();

        let agent = store.load_agent_by_name("key-removed").unwrap().unwrap();
        let patch = alms_core::UpdateAgentRequest {
            summary_provider: Some("openrouter".into()),
            summary_model: Some("minimax/minimax-m2.7".into()),
            ..Default::default()
        };
        let (status, body) = update_agent_err(state, agent.id.to_string(), patch).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err_code(&body), "SUMMARY_PROVIDER_MISSING_API_KEY");
    }

    // ── #946: worktree-mode flip on apply_update_request ──────────

    #[test]
    fn apply_update_flips_worktree_mode_off_to_git() {
        let mut agent = new_agent("test");
        assert_eq!(agent.worktree_mode, WorktreeMode::Off);

        let req = UpdateAgentRequest {
            worktree_mode: Some(WorktreeMode::Git),
            ..Default::default()
        };
        apply_update_request(&mut agent, req).unwrap();
        assert_eq!(agent.worktree_mode, WorktreeMode::Git);
    }

    #[test]
    fn apply_update_flips_worktree_mode_git_to_off() {
        let mut agent = new_agent("test");
        agent.worktree_mode = WorktreeMode::Git;

        let req = UpdateAgentRequest {
            worktree_mode: Some(WorktreeMode::Off),
            ..Default::default()
        };
        apply_update_request(&mut agent, req).unwrap();
        assert_eq!(agent.worktree_mode, WorktreeMode::Off);
    }

    #[test]
    fn apply_update_omitted_worktree_mode_is_no_op() {
        // Omitting `worktree_mode` from the PATCH must leave the
        // stored value alone — same shape as every other field.
        let mut agent = new_agent("test");
        agent.worktree_mode = WorktreeMode::Git;

        let req = UpdateAgentRequest {
            description: Some("touched".into()),
            ..Default::default()
        };
        apply_update_request(&mut agent, req).unwrap();
        assert_eq!(
            agent.worktree_mode,
            WorktreeMode::Git,
            "omitted worktree_mode must NOT clobber the stored value"
        );
        assert_eq!(agent.description, "touched");
    }

    #[test]
    fn update_request_parses_worktree_mode_off() {
        let req: UpdateAgentRequest = serde_json::from_str(r#"{"worktree_mode": "off"}"#).unwrap();
        assert_eq!(req.worktree_mode, Some(WorktreeMode::Off));
    }

    #[test]
    fn update_request_parses_worktree_mode_git() {
        let req: UpdateAgentRequest = serde_json::from_str(r#"{"worktree_mode": "git"}"#).unwrap();
        assert_eq!(req.worktree_mode, Some(WorktreeMode::Git));
    }

    #[test]
    fn update_request_parses_force_worktree_remove() {
        let req: UpdateAgentRequest =
            serde_json::from_str(r#"{"force_worktree_remove": true}"#).unwrap();
        assert_eq!(req.force_worktree_remove, Some(true));
    }

    // ── #946: HTTP-level worktree-mode integration tests ──────────

    /// Override the test app state's `project_root` AND wire a tempdir
    /// git repo so worktree-mode HTTP tests have a real working tree
    /// to fork from.
    fn agents_test_state_with_git_project(tmp_dir: &std::path::Path) -> crate::server::AppState {
        init_git_repo(tmp_dir);

        let mut state = agents_test_app_state_with_sqlite();
        state.project_root = tmp_dir.to_path_buf();
        state
    }

    /// Issue acceptance: `POST /agents` with `worktree_mode = "git"`
    /// on a git project provisions the worktree at the canonical
    /// path AND persists the agent record with `worktree_mode =
    /// Git`.
    #[tokio::test]
    async fn post_agents_with_worktree_mode_git_provisions_worktree() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = agents_test_state_with_git_project(tmp.path());

        let req = alms_core::CreateAgentRequest {
            name: "atlas".into(),
            description: None,
            model: None,
            posture: None,
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
            worktree_mode: Some(WorktreeMode::Git),
            debug_mode: None,
            is_default: None,
        };

        let (status, body) = create_agent(axum::extract::State(state.clone()), Json(req))
            .await
            .expect("create must succeed on git project");
        assert_eq!(status, axum::http::StatusCode::CREATED);
        assert_eq!(body.0["worktree_mode"], serde_json::json!("git"));

        // Worktree directory exists at the canonical path.
        let worktree_dir = tmp.path().join(".alms").join("worktrees").join("atlas");
        assert!(
            worktree_dir.is_dir(),
            "expected worktree at {}",
            worktree_dir.display()
        );

        // The agent record persisted with worktree_mode = Git.
        let store = state.session_manager.store().expect("sqlite store");
        let stored = store.load_agent_by_name("atlas").unwrap().unwrap();
        assert_eq!(stored.worktree_mode, WorktreeMode::Git);
    }

    /// Issue acceptance: `POST /agents` with `worktree_mode = "git"`
    /// on a non-git project returns `400 WORKTREE_REQUIRES_GIT` and
    /// the agent record is NOT persisted.
    #[tokio::test]
    async fn post_agents_with_worktree_mode_git_on_non_git_project_returns_400() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Note: NO git init — bare directory.
        let mut state = agents_test_app_state_with_sqlite();
        state.project_root = tmp.path().to_path_buf();

        let req = alms_core::CreateAgentRequest {
            name: "atlas".into(),
            description: None,
            model: None,
            posture: None,
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
            worktree_mode: Some(WorktreeMode::Git),
            debug_mode: None,
            is_default: None,
        };

        let (status, body) = create_agent_err(state.clone(), req).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err_code(&body), "WORKTREE_REQUIRES_GIT");

        // Defensive: no agent record.
        let store = state.session_manager.store().expect("sqlite store");
        assert!(store.load_agent_by_name("atlas").unwrap().is_none());
        // Defensive: no stranded worktree dir.
        assert!(
            !tmp.path()
                .join(".alms")
                .join("worktrees")
                .join("atlas")
                .exists(),
            "non-git project must not leave a half-created worktree dir"
        );
    }

    /// `PATCH /agents/{id}` flipping `Off → Git` provisions the
    /// worktree on the fly.
    #[tokio::test]
    async fn patch_agents_off_to_git_provisions_worktree() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = agents_test_state_with_git_project(tmp.path());

        // Create the agent in `Off` mode first.
        let store = state.session_manager.store().expect("sqlite store");
        let mut agent = new_agent("atlas");
        agent.worktree_mode = WorktreeMode::Off;
        store.create_agent(&agent).unwrap();

        let req = UpdateAgentRequest {
            worktree_mode: Some(WorktreeMode::Git),
            ..Default::default()
        };
        let result = update_agent(
            axum::extract::State(state.clone()),
            axum::extract::Path(agent.id.0.to_string()),
            Json(req),
        )
        .await
        .expect("PATCH must succeed");

        assert_eq!(result.0["worktree_mode"], serde_json::json!("git"));
        assert!(
            tmp.path()
                .join(".alms")
                .join("worktrees")
                .join("atlas")
                .is_dir(),
            "Off → Git flip must provision the worktree"
        );
    }

    /// `PATCH /agents/{id}` flipping `Git → Off` removes the
    /// worktree and refuses if uncommitted changes are present
    /// (without `force_worktree_remove`).
    #[tokio::test]
    async fn patch_agents_git_to_off_refuses_uncommitted_without_force() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = agents_test_state_with_git_project(tmp.path());

        // Create with Git mode so the worktree is on disk.
        let create_req = alms_core::CreateAgentRequest {
            name: "atlas".into(),
            description: None,
            model: None,
            posture: None,
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
            worktree_mode: Some(WorktreeMode::Git),
            debug_mode: None,
            is_default: None,
        };
        let (_status, _body) = create_agent(axum::extract::State(state.clone()), Json(create_req))
            .await
            .expect("create must succeed");

        // Dirty up the worktree.
        let worktree_dir = tmp.path().join(".alms").join("worktrees").join("atlas");
        std::fs::write(worktree_dir.join("dirty.txt"), "wip").unwrap();

        // Flip Git → Off without force — must refuse.
        let req = UpdateAgentRequest {
            worktree_mode: Some(WorktreeMode::Off),
            ..Default::default()
        };
        let (status, body) = update_agent_err(state.clone(), "atlas".into(), req).await;
        assert_eq!(status, axum::http::StatusCode::CONFLICT);
        assert_eq!(err_code(&body), "WORKTREE_HAS_UNCOMMITTED_CHANGES");
        assert!(
            worktree_dir.is_dir(),
            "refused PATCH must leave the worktree on disk"
        );

        // Retry with force — should succeed.
        let req_force = UpdateAgentRequest {
            worktree_mode: Some(WorktreeMode::Off),
            force_worktree_remove: Some(true),
            ..Default::default()
        };
        let result = update_agent(
            axum::extract::State(state.clone()),
            axum::extract::Path("atlas".into()),
            Json(req_force),
        )
        .await
        .expect("force PATCH must succeed");
        assert_eq!(result.0["worktree_mode"], serde_json::json!("off"));
        assert!(
            !worktree_dir.exists(),
            "force PATCH must remove the worktree"
        );
    }

    /// `DELETE /agents/{id}` with `worktree_mode = git` removes the
    /// worktree. Refuses on uncommitted changes unless `?force=true`.
    #[tokio::test]
    async fn delete_agents_with_worktree_refuses_uncommitted_without_force() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = agents_test_state_with_git_project(tmp.path());

        let create_req = alms_core::CreateAgentRequest {
            name: "atlas".into(),
            description: None,
            model: None,
            posture: None,
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
            worktree_mode: Some(WorktreeMode::Git),
            debug_mode: None,
            is_default: None,
        };
        let _ = create_agent(axum::extract::State(state.clone()), Json(create_req))
            .await
            .expect("create");

        let worktree_dir = tmp.path().join(".alms").join("worktrees").join("atlas");
        std::fs::write(worktree_dir.join("dirty.txt"), "wip").unwrap();

        // DELETE without force — refuses.
        let result = delete_agent(
            axum::extract::State(state.clone()),
            axum::extract::Path("atlas".into()),
            axum::extract::Query(DeleteAgentQuery { force: false }),
        )
        .await;
        let (status, body) = result.expect_err("delete must refuse on uncommitted");
        assert_eq!(status, axum::http::StatusCode::CONFLICT);
        assert_eq!(err_code(&body.0), "WORKTREE_HAS_UNCOMMITTED_CHANGES");

        // The agent record is still there.
        let store = state.session_manager.store().expect("sqlite store");
        assert!(store.load_agent_by_name("atlas").unwrap().is_some());

        // DELETE with force — succeeds.
        let result = delete_agent(
            axum::extract::State(state.clone()),
            axum::extract::Path("atlas".into()),
            axum::extract::Query(DeleteAgentQuery { force: true }),
        )
        .await
        .expect("force delete must succeed");
        assert_eq!(result.0["ok"], serde_json::json!(true));
        assert!(
            !worktree_dir.exists(),
            "force delete must remove the worktree"
        );
        assert!(store.load_agent_by_name("atlas").unwrap().is_none());
    }

    // ── #964: PATCH /agents worktree side-effect must compensate on
    //          SQLite persist failure ────────────────────────────────

    /// Captured-log harness, shared with the #947 boot-WARN tests in
    /// `gateway.rs`. The compensation tests below assert on the
    /// structured `alms.worktree` events the helper emits.
    ///
    /// Do **not** reach for `tracing::subscriber::with_default` here:
    /// it is thread-scoped while `tracing`'s callsite-`Interest` cache
    /// is process-global, which is the #1221 flake — a callsite first
    /// touched by another test on a subscriber-less thread caches
    /// `Interest::never()` and this test's capture comes back empty.
    use alms_test_support::{capture_logs, init_git_repo};

    /// Provision a worktree for `agent_name` under `project_root`,
    /// write `file_contents` to `agent-state.txt`, configure the
    /// worktree's local git identity, and commit. Returns the
    /// worktree path so callers can do further assertions on it.
    ///
    /// Used by the PATCH git→off and DELETE compensation tests that
    /// need real agent-only branch history (an `alms/<agent>` tip
    /// diverged from parent `main`) so the SHA-snapshot restore
    /// path has something to do. Factored out per Tim's #1029 nit.
    fn provision_worktree_with_commit(
        project_root: &std::path::Path,
        agent_name: &str,
        file_contents: &str,
        commit_message: &str,
    ) -> std::path::PathBuf {
        let worktree_path = worktree::create_worktree(project_root, agent_name)
            .unwrap()
            .into_path();
        std::fs::write(worktree_path.join("agent-state.txt"), file_contents).unwrap();
        let run = |dir: &std::path::Path, args: &[&str]| {
            let s = std::process::Command::new("git")
                .current_dir(dir)
                .env("GIT_TERMINAL_PROMPT", "0")
                .args(args)
                .status()
                .expect("git command");
            assert!(s.success(), "git {args:?} failed in {}", dir.display());
        };
        run(
            &worktree_path,
            &["config", "user.email", "test@example.com"],
        );
        run(&worktree_path, &["config", "user.name", "Test"]);
        run(&worktree_path, &["add", "agent-state.txt"]);
        run(&worktree_path, &["commit", "-m", commit_message]);
        worktree_path
    }

    /// Issue #964 acceptance: on the `Off → Git` flip, a persist
    /// failure after the worktree has been created must roll back
    /// the on-disk state — the worktree directory must NOT exist
    /// after the helper returns, so disk and (still-on-Off)
    /// registry stay consistent.
    #[test]
    fn off_to_git_persist_failure_compensates_by_deleting_worktree() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let worktree_dir = tmp.path().join(".alms").join("worktrees").join("atlas");

        let captured = capture_logs(tracing::Level::WARN, || {
            let result = apply_worktree_flip_and_persist(
                tmp.path(),
                "atlas",
                WorktreeMode::Off,
                WorktreeMode::Git,
                false,
                false,
                || {
                    // Synthetic SQLite failure — mimics a "database is locked"
                    // / disk-full / fs-perm error the way `update_agent`
                    // would surface it.
                    Err(alms_core::AlmsError::Runtime(
                        "SQLite update_agent: database is locked".into(),
                    ))
                },
            );
            let (status, body) = result.expect_err("persist failure must surface as Err");
            assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(err_code(&body.0), "INTERNAL");
        });

        // Disk: worktree must be gone — compensation deleted what we
        // just created.
        assert!(
            !worktree_dir.exists(),
            "compensation must delete the just-created worktree on persist failure; \
             still found at {}",
            worktree_dir.display(),
        );

        // Audit log: structured WARN at alms.worktree target with
        // direction + persist_error + the explicit "compensation
        // succeeded" verb.
        assert!(
            captured.contains("alms.worktree"),
            "compensation event must use the alms.worktree tracing target: {captured}"
        );
        assert!(
            captured.contains("agent_name=\"atlas\"") || captured.contains("agent_name=atlas"),
            "compensation event must carry structured agent_name: {captured}"
        );
        assert!(
            captured.contains("direction=\"off->git\"") || captured.contains("direction=off->git"),
            "compensation event must carry structured direction=off->git: {captured}"
        );
        assert!(
            captured.contains("compensation succeeded"),
            "compensation event message must say 'compensation succeeded': {captured}"
        );
    }

    /// Issue #964 acceptance: on the `Git → Off` flip, a persist
    /// failure after the worktree has been removed must recreate
    /// the on-disk state — the worktree directory MUST exist again
    /// after the helper returns, so disk and (still-on-Git)
    /// registry stay consistent.
    #[test]
    fn git_to_off_persist_failure_compensates_by_recreating_worktree() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_git_repo(tmp.path());

        // Pre-provision the worktree so the Git→Off flip has
        // something to remove.
        worktree::create_worktree(tmp.path(), "atlas").unwrap();
        let worktree_dir = tmp.path().join(".alms").join("worktrees").join("atlas");
        assert!(worktree_dir.is_dir());

        let captured = capture_logs(tracing::Level::WARN, || {
            let result = apply_worktree_flip_and_persist(
                tmp.path(),
                "atlas",
                WorktreeMode::Git,
                WorktreeMode::Off,
                true, // force_remove — the existing PATCH path passes this through
                false,
                || {
                    Err(alms_core::AlmsError::Runtime(
                        "SQLite update_agent: disk I/O error".into(),
                    ))
                },
            );
            let (status, body) = result.expect_err("persist failure must surface as Err");
            assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(err_code(&body.0), "INTERNAL");
        });

        // Disk: worktree must be back — compensation recreated it.
        assert!(
            worktree_dir.is_dir(),
            "compensation must recreate the just-removed worktree on persist failure; \
             still missing at {}",
            worktree_dir.display(),
        );

        // Audit log: structured WARN at alms.worktree with the
        // git->off direction.
        assert!(
            captured.contains("alms.worktree"),
            "compensation event must use the alms.worktree tracing target: {captured}"
        );
        assert!(
            captured.contains("direction=\"git->off\"") || captured.contains("direction=git->off"),
            "compensation event must carry structured direction=git->off: {captured}"
        );
        assert!(
            captured.contains("compensation succeeded"),
            "compensation event must say 'compensation succeeded': {captured}"
        );
    }

    /// Codex P1 (#1019 follow-up): regression guard for the
    /// silent-data-loss case in the `git→off` compensation path.
    ///
    /// Before the fix, compensation called `create_worktree`,
    /// which always forks a new branch from `HEAD`. So when an
    /// operator had committed work on `alms/<name>` and triggered
    /// a `git→off` flip that hit a persist failure, the helper
    /// would happily report "compensation succeeded" while the
    /// branch silently came back pointing at the parent project's
    /// HEAD (typically `main`) — every commit on the agent branch
    /// was orphaned in the reflog at best, lost at worst.
    ///
    /// This test makes a real commit on `alms/atlas`, snapshots
    /// its tip, drives a `git→off` flip with a forced persist
    /// failure, and asserts the post-compensation branch points
    /// at the SAME tip and the committed file is back in the
    /// worktree. Pre-fix this test fails: branch points at the
    /// initial empty commit on `main`, file is missing.
    #[test]
    fn git_to_off_persist_failure_preserves_branch_history() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_git_repo(tmp.path());

        // Provision the worktree and commit real work on the
        // agent branch so HEAD (parent's `main`) and `alms/atlas`
        // diverge.
        let worktree_path = worktree::create_worktree(tmp.path(), "atlas")
            .unwrap()
            .into_path();
        std::fs::write(
            worktree_path.join("agent-state.txt"),
            "important agent work",
        )
        .unwrap();
        let run = |dir: &std::path::Path, args: &[&str]| {
            let s = std::process::Command::new("git")
                .current_dir(dir)
                .env("GIT_TERMINAL_PROMPT", "0")
                .args(args)
                .status()
                .expect("git command");
            assert!(s.success(), "git {args:?} failed in {}", dir.display());
        };
        run(
            &worktree_path,
            &["config", "user.email", "test@example.com"],
        );
        run(&worktree_path, &["config", "user.name", "Test"]);
        run(&worktree_path, &["add", "agent-state.txt"]);
        run(
            &worktree_path,
            &["commit", "-m", "agent commit — must survive"],
        );

        // Snapshot the agent branch tip BEFORE the flip.
        let pre_flip_sha = worktree::read_branch_head_sha(tmp.path(), "atlas")
            .unwrap()
            .expect("agent branch must exist after commit");

        // Snapshot parent HEAD too — pre-fix, compensation would
        // (incorrectly) restore the branch to this SHA, so we use
        // it as the "wrong answer" baseline.
        let parent_head_output = std::process::Command::new("git")
            .current_dir(tmp.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let parent_head_sha = String::from_utf8_lossy(&parent_head_output.stdout)
            .trim()
            .to_string();
        assert_ne!(
            pre_flip_sha, parent_head_sha,
            "test invariant: agent commit must diverge from parent HEAD",
        );

        // Drive the git→off flip with a synthetic persist failure
        // — same shape as the SQLite "database is locked" error
        // the production handler would surface.
        let result = apply_worktree_flip_and_persist(
            tmp.path(),
            "atlas",
            WorktreeMode::Git,
            WorktreeMode::Off,
            true, // force — required to remove the worktree+branch
            false,
            || {
                Err(alms_core::AlmsError::Runtime(
                    "SQLite update_agent: database is locked".into(),
                ))
            },
        );
        let (status, _) = result.expect_err("persist failure must surface as Err");
        assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);

        // The contract: registry stayed at Git, so disk must be
        // back at Git too — worktree dir present, branch present
        // AND POINTING AT THE PRE-FLIP SHA.
        let worktree_dir = tmp.path().join(".alms").join("worktrees").join("atlas");
        assert!(
            worktree_dir.is_dir(),
            "compensation must recreate the worktree directory"
        );

        let post_flip_sha = worktree::read_branch_head_sha(tmp.path(), "atlas")
            .unwrap()
            .expect("compensation must restore the alms/atlas branch");
        assert_eq!(
            post_flip_sha, pre_flip_sha,
            "compensation must restore alms/atlas to its pre-flip tip ({pre_flip_sha}), \
             NOT fork a new branch from parent HEAD ({parent_head_sha}). \
             Got: {post_flip_sha}. This is the silent-data-loss bug Codex P1 \
             caught on PR #1019 — pre-fix the branch came back pointing at parent HEAD."
        );
        assert_ne!(
            post_flip_sha, parent_head_sha,
            "compensation MUST NOT have re-forked the branch from parent HEAD"
        );

        // Belt-and-braces: the committed file must be present
        // inside the restored worktree. Pre-fix this assertion
        // fails because the worktree was checked out at parent
        // HEAD, which never had the file.
        assert!(
            worktree_dir.join("agent-state.txt").exists(),
            "restored worktree must carry the committed file — direct evidence \
             that history survived the round trip; missing file means the branch \
             was re-forked from HEAD and the operator's commit is lost"
        );
    }

    /// Issue #964 acceptance, third arm: when the COMPENSATION
    /// itself also fails (worktree-delete fails after SQLite-write
    /// failed), the helper must surface a distinct error code,
    /// preserve both error strings, and emit an ERROR-level
    /// structured log so the operator can see disk diverged.
    #[test]
    fn off_to_git_persist_and_compensation_both_fail_surfaces_both_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let worktree_dir = tmp.path().join(".alms").join("worktrees").join("atlas");

        let captured = capture_logs(tracing::Level::ERROR, || {
            let result = apply_worktree_flip_and_persist(
                tmp.path(),
                "atlas",
                WorktreeMode::Off,
                WorktreeMode::Git,
                false,
                false,
                || {
                    // Inside the persist closure, after the worktree
                    // create has already touched disk, simulate a
                    // catastrophic environment shift: the parent
                    // git repo is nuked. Compensation will then run
                    // `git -C <project> worktree remove ...`, which
                    // fails because `.git` no longer exists. This
                    // exercises the dual-failure path — the persist
                    // result we return here is the "first" error,
                    // and the inverse worktree op is the "second"
                    // error.
                    std::fs::remove_dir_all(tmp.path().join(".git"))
                        .expect("nuke .git to break compensation");
                    Err(alms_core::AlmsError::Runtime(
                        "SQLite update_agent: disk full".into(),
                    ))
                },
            );
            let (status, body) = result.expect_err("dual failure must surface as Err");
            assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(err_code(&body.0), "WORKTREE_COMPENSATION_FAILED");

            // Both error strings round-trip through the wire payload
            // so the operator's audit log carries the full diagnosis.
            let msg = body.0["error"]["message"].as_str().unwrap_or_default();
            assert!(
                msg.contains("disk full"),
                "wire body must include the persist error: {msg}"
            );
            assert!(
                msg.contains("Compensation error"),
                "wire body must explicitly mention compensation failure: {msg}"
            );
        });

        // Disk: worktree dir is still on disk (compensation tried
        // to delete it but git failed). This is the divergent state
        // — the test asserts we surfaced the divergence rather than
        // hiding it.
        assert!(
            worktree_dir.exists(),
            "compensation failed, so the orphan worktree dir must still be on disk \
             — this is the divergence the operator needs to clean up manually"
        );

        // Audit log: ERROR-level event with BOTH persist_error and
        // compensation_error fields, and the explicit "compensation
        // also failed" verb.
        assert!(
            captured.contains("alms.worktree"),
            "dual-failure event must use the alms.worktree tracing target: {captured}"
        );
        assert!(
            captured.contains("ERROR"),
            "dual failure must log at ERROR level (not WARN): {captured}"
        );
        assert!(
            captured.contains("persist_error"),
            "dual-failure event must carry structured persist_error: {captured}"
        );
        assert!(
            captured.contains("compensation_error"),
            "dual-failure event must carry structured compensation_error: {captured}"
        );
        assert!(
            captured.contains("compensation also failed"),
            "dual-failure event message must say 'compensation also failed': {captured}"
        );
    }

    /// Regression guard: a same-mode PATCH (e.g. Git → Git) with a
    /// persist failure must not run any compensating worktree op —
    /// there's no side-effect to undo. The helper still surfaces
    /// the persist error, but the wire shape stays `INTERNAL` (not
    /// the dual-failure code) and disk is untouched.
    #[test]
    fn no_flip_persist_failure_skips_compensation() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_git_repo(tmp.path());
        worktree::create_worktree(tmp.path(), "atlas").unwrap();
        let worktree_dir = tmp.path().join(".alms").join("worktrees").join("atlas");

        let result = apply_worktree_flip_and_persist(
            tmp.path(),
            "atlas",
            WorktreeMode::Git,
            WorktreeMode::Git,
            false,
            false,
            || {
                Err(alms_core::AlmsError::Runtime(
                    "SQLite update_agent: db locked".into(),
                ))
            },
        );

        let (status, body) = result.expect_err("persist failure must surface");
        assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            err_code(&body.0),
            "INTERNAL",
            "no-flip persist failure must NOT use the dual-failure code"
        );
        // Worktree on disk is unchanged — no compensation ran.
        assert!(
            worktree_dir.is_dir(),
            "same-mode PATCH must not touch the existing worktree on persist failure"
        );
    }

    /// Codex P2 follow-up (#1019 round 5): when the `git→off`
    /// compensation path runs and finds the agent branch already
    /// at the snapshot SHA — the AlreadyAbsent flavor where
    /// `remove_worktree` silently failed to delete the branch
    /// (`git branch -D` refused due to stale worktree metadata,
    /// locked ref, etc.) — the response must be the regular
    /// `INTERNAL` persist-failure error, NOT
    /// `WORKTREE_COMPENSATION_FAILED`. No drift was introduced —
    /// the branch is still at the snapshot SHA, so compensation
    /// is effectively a no-op.
    ///
    /// We model the "branch survived remove_worktree" state by
    /// having the persist closure re-create the branch at the
    /// snapshot SHA before returning Err. Pre-fix
    /// `restore_worktree_at_sha` would then fail with `fatal: A
    /// branch named ... already exists` and the operator would see
    /// the scary dual-failure code.
    #[test]
    fn git_to_off_persist_failure_idempotent_when_branch_already_at_snapshot() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_git_repo(tmp.path());

        // Provision + real commit so the branch carries history.
        let worktree_path = worktree::create_worktree(tmp.path(), "atlas")
            .unwrap()
            .into_path();
        std::fs::write(worktree_path.join("agent-state.txt"), "important").unwrap();
        let run = |dir: &std::path::Path, args: &[&str]| {
            let s = std::process::Command::new("git")
                .current_dir(dir)
                .env("GIT_TERMINAL_PROMPT", "0")
                .args(args)
                .status()
                .expect("git command");
            assert!(s.success(), "git {args:?} failed in {}", dir.display());
        };
        run(
            &worktree_path,
            &["config", "user.email", "test@example.com"],
        );
        run(&worktree_path, &["config", "user.name", "Test"]);
        run(&worktree_path, &["add", "agent-state.txt"]);
        run(&worktree_path, &["commit", "-m", "agent commit"]);

        let snapshot_sha = worktree::read_branch_head_sha(tmp.path(), "atlas")
            .unwrap()
            .expect("agent branch must exist after commit");

        let project_root = tmp.path().to_path_buf();
        let snapshot_for_closure = snapshot_sha.clone();

        let captured = capture_logs(tracing::Level::INFO, || {
            let result = apply_worktree_flip_and_persist(
                tmp.path(),
                "atlas",
                WorktreeMode::Git,
                WorktreeMode::Off,
                true, // force — required to remove the worktree+branch
                false,
                move || {
                    // Phase 1's `remove_worktree` deleted the branch.
                    // Re-create it at the same SHA before returning
                    // the persist error — this is the post-state we
                    // get when the AlreadyAbsent arm of
                    // `remove_worktree` silently failed to delete
                    // the branch.
                    let s = std::process::Command::new("git")
                        .current_dir(&project_root)
                        .env("GIT_TERMINAL_PROMPT", "0")
                        .args(["branch", "alms/atlas", &snapshot_for_closure])
                        .status()
                        .expect("git branch");
                    assert!(s.success(), "test setup: re-creating branch failed");

                    Err(alms_core::AlmsError::Runtime(
                        "SQLite update_agent: database is locked".into(),
                    ))
                },
            );
            let (status, body) = result.expect_err("persist failure must surface");
            assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(
                err_code(&body.0),
                "INTERNAL",
                "branch already at snapshot SHA must surface the regular INTERNAL \
                 persist error, NOT WORKTREE_COMPENSATION_FAILED — pre-fix the \
                 idempotent restore would error with `branch already exists` and \
                 the operator would see the scary dual-failure code"
            );
        });

        // Branch must still be at the snapshot SHA — we never asked
        // to touch it.
        let post_sha = worktree::read_branch_head_sha(tmp.path(), "atlas")
            .unwrap()
            .expect("branch must still exist after compensation");
        assert_eq!(
            post_sha, snapshot_sha,
            "branch SHA must be unchanged across the idempotent compensation path"
        );

        // Worktree dir must be back on disk — the `git worktree
        // add` step still ran inside the idempotent restore.
        let worktree_dir = tmp.path().join(".alms").join("worktrees").join("atlas");
        assert!(
            worktree_dir.is_dir(),
            "worktree dir must be restored after compensation"
        );

        // Audit log: the alms.worktree info-level trace must
        // mention the idempotency skip — proves the new code path
        // actually fired (rather than the test accidentally taking
        // the case-(a) branch-missing path).
        assert!(
            captured.contains("alms.worktree"),
            "compensation must emit alms.worktree event: {captured}"
        );
        assert!(
            captured.contains("Branch already at snapshot SHA"),
            "compensation must log the idempotency-skip message: {captured}"
        );
    }

    /// Happy path through the helper: persist succeeds, no
    /// compensation runs, no audit-log event fires. Tests the
    /// pre-#964 wire shape stays unchanged when nothing goes wrong.
    #[test]
    fn off_to_git_happy_path_no_compensation_log() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let worktree_dir = tmp.path().join(".alms").join("worktrees").join("atlas");

        let captured = capture_logs(tracing::Level::WARN, || {
            apply_worktree_flip_and_persist(
                tmp.path(),
                "atlas",
                WorktreeMode::Off,
                WorktreeMode::Git,
                false,
                false,
                || Ok(()),
            )
            .expect("happy path must return Ok");
        });

        assert!(worktree_dir.is_dir(), "worktree must exist on happy path");
        assert!(
            !captured.contains("compensation"),
            "happy path must not emit a compensation log line: {captured}"
        );
    }

    /// Codex P1 (#1019, second finding): regression guard for the
    /// silent-data-loss case in the `off→git` compensation path.
    ///
    /// Before the fix, compensation gated on the mode-transition
    /// alone — `(Off, Git)` ⇒ "the helper created a worktree". But
    /// `create_worktree` is idempotent: when an operator's worktree
    /// was already drifted-present (prior crash, manual `git
    /// worktree add`, earlier failed PATCH that left state behind),
    /// `create_worktree` returns `Ok(WorktreeCreate::AlreadyExisted)`
    /// without running `git worktree add`. The pre-fix compensation
    /// then ran `remove_worktree(..., force=true)` on this
    /// pre-existing worktree, deleting an operator-owned directory
    /// AND branch this PATCH never created.
    ///
    /// This test pre-creates the worktree on disk with a real
    /// commit, drives an off→git PATCH with a forced persist
    /// failure, and asserts the worktree directory AND its branch
    /// AND the committed file all survive. Pre-fix the worktree
    /// was deleted and the operator's commit was lost (only
    /// recoverable via reflog).
    #[test]
    fn off_to_git_persist_failure_preserves_pre_existing_worktree() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_git_repo(tmp.path());

        // Pre-create the worktree to simulate operator drift —
        // the disk has a worktree even though the registry record
        // (still on Off) does not yet point at it.
        let pre_existing = worktree::create_worktree(tmp.path(), "atlas")
            .unwrap()
            .into_path();
        assert!(pre_existing.is_dir(), "test setup: worktree must exist");

        // Make a real commit on the agent branch so we can prove
        // the compensation path does NOT destroy operator history.
        std::fs::write(pre_existing.join("operator-state.txt"), "drifted state").unwrap();
        let run = |dir: &std::path::Path, args: &[&str]| {
            let s = std::process::Command::new("git")
                .current_dir(dir)
                .env("GIT_TERMINAL_PROMPT", "0")
                .args(args)
                .status()
                .expect("git command");
            assert!(s.success(), "git {args:?} failed in {}", dir.display());
        };
        run(&pre_existing, &["config", "user.email", "test@example.com"]);
        run(&pre_existing, &["config", "user.name", "Test"]);
        run(&pre_existing, &["add", "operator-state.txt"]);
        run(&pre_existing, &["commit", "-m", "drifted operator commit"]);

        let pre_flip_sha = worktree::read_branch_head_sha(tmp.path(), "atlas")
            .unwrap()
            .expect("agent branch must exist after commit");

        // Drive an off→git flip with a synthetic persist failure.
        // Pre-fix the helper would gate compensation on the
        // (Off, Git) transition and run
        // `remove_worktree(..., force=true)` even though the
        // side-effect call was a no-op (`AlreadyExisted`),
        // deleting the worktree dir + branch + commit above.
        let captured = capture_logs(tracing::Level::WARN, || {
            let result = apply_worktree_flip_and_persist(
                tmp.path(),
                "atlas",
                WorktreeMode::Off,
                WorktreeMode::Git,
                false,
                false,
                || {
                    Err(alms_core::AlmsError::Runtime(
                        "SQLite update_agent: database is locked".into(),
                    ))
                },
            );
            let (status, body) = result.expect_err("persist failure must surface");
            assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
            // No dual-failure code — we did not even run a
            // destructive compensation. The wire shape must match
            // the same-mode persist-failure path.
            assert_eq!(
                err_code(&body.0),
                "INTERNAL",
                "no-op side-effect persist failure must NOT use the \
                 dual-failure code — there was nothing to compensate"
            );
        });

        // The contract: the pre-existing worktree, branch, AND
        // commit must all survive untouched. Pre-fix every one of
        // these assertions fails.
        assert!(
            pre_existing.is_dir(),
            "compensation must NOT delete a pre-existing worktree directory \
             when create_worktree returned AlreadyExisted; this is the silent-data-loss \
             bug Codex P1 caught on PR #1019 (off→git side, second finding)"
        );
        let post_flip_sha = worktree::read_branch_head_sha(tmp.path(), "atlas")
            .unwrap()
            .expect("compensation must NOT have deleted the alms/atlas branch");
        assert_eq!(
            post_flip_sha, pre_flip_sha,
            "alms/atlas branch SHA must be unchanged: this PATCH created no \
             on-disk state and therefore must not undo any. Pre-fix the branch \
             was deleted (or rewritten via -D / fresh -b)."
        );
        assert!(
            pre_existing.join("operator-state.txt").exists(),
            "operator's drifted commit must survive — direct evidence the \
             pre-existing worktree was NOT torn down by spurious compensation"
        );

        // Audit log: the helper must emit a structured WARN
        // explicitly noting that the worktree was already on disk.
        // Operators rely on this line to recognize drift cases in
        // post-mortem.
        assert!(
            captured.contains("alms.worktree"),
            "no-op-create event must use the alms.worktree tracing target: {captured}"
        );
        assert!(
            captured.contains("already on disk") || captured.contains("AlreadyExisted"),
            "no-op-create event must explicitly call out the pre-existing \
             state so operators can spot drift in audit logs: {captured}"
        );
        assert!(
            !captured.contains("compensation succeeded")
                && !captured.contains("compensation also failed"),
            "no destructive compensation must run, so neither 'compensation \
             succeeded' nor 'compensation also failed' must appear: {captured}"
        );
    }

    /// Symmetric Codex P1 (#1019, second finding) — the git→off
    /// arm of the same idempotent-helper hazard.
    ///
    /// Hazard: `remove_worktree` is idempotent — when the worktree
    /// directory is not on disk, it returns
    /// `Ok(WorktreeRemove::AlreadyAbsent)` after only a best-effort
    /// branch cleanup. Pre-fix, the gateway compensation path gated
    /// on the `(Git, Off)` transition alone and ran a fresh
    /// `create_worktree` (or `restore_worktree_at_sha` if a snapshot
    /// existed) on persist failure — fabricating a worktree + branch
    /// the operator never had, simply because the registry row had
    /// been at `Git`.
    ///
    /// This test simulates operator drift by setting up a registry
    /// row at `Git` while the on-disk worktree has been manually
    /// nuked, drives a git→off PATCH with a forced persist failure,
    /// and asserts no spurious worktree / branch is created on
    /// compensation. Pre-fix the test fails: a fresh worktree dir
    /// appears at `<project>/.alms/worktrees/atlas/` and a fresh
    /// `alms/atlas` branch is forked off `HEAD`.
    #[test]
    fn git_to_off_persist_failure_skips_recreate_when_worktree_already_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_git_repo(tmp.path());

        // Drift simulation: registry says Git, disk says nothing.
        // No `create_worktree` is called — we go straight from
        // "fresh repo" to the helper, mimicking the case where an
        // operator manually `rm -rf` their worktree dir but the
        // registry never noticed.
        let worktree_dir = tmp.path().join(".alms").join("worktrees").join("atlas");
        assert!(
            !worktree_dir.exists(),
            "test setup: worktree must NOT exist before the flip"
        );
        let branch_sha_before = worktree::read_branch_head_sha(tmp.path(), "atlas").unwrap();
        assert!(
            branch_sha_before.is_none(),
            "test setup: alms/atlas branch must NOT exist before the flip"
        );

        // Drive a git→off flip with a synthetic persist failure.
        // Pre-fix the helper would (a) snapshot the branch (returns
        // None — no branch), (b) call `remove_worktree` (no-op,
        // AlreadyAbsent), (c) on persist failure gate compensation
        // on the (Git, Off) transition and run `create_worktree`
        // (because pre_remove_branch_sha was None), fabricating a
        // worktree + branch this PATCH never owned.
        let captured = capture_logs(tracing::Level::WARN, || {
            let result = apply_worktree_flip_and_persist(
                tmp.path(),
                "atlas",
                WorktreeMode::Git,
                WorktreeMode::Off,
                true,
                false,
                || {
                    Err(alms_core::AlmsError::Runtime(
                        "SQLite update_agent: database is locked".into(),
                    ))
                },
            );
            let (status, body) = result.expect_err("persist failure must surface");
            assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(
                err_code(&body.0),
                "INTERNAL",
                "no-op side-effect persist failure must NOT use the \
                 dual-failure code — there was nothing to compensate"
            );
        });

        // The contract: no fabricated state. Worktree dir absent,
        // branch absent. Pre-fix both of these assertions fail.
        assert!(
            !worktree_dir.exists(),
            "compensation must NOT fabricate a worktree directory when \
             remove_worktree returned AlreadyAbsent; this is the silent-state- \
             fabrication bug — symmetric of the off→git Codex P1 on PR #1019"
        );
        let branch_sha_after = worktree::read_branch_head_sha(tmp.path(), "atlas").unwrap();
        assert!(
            branch_sha_after.is_none(),
            "compensation must NOT fabricate the alms/atlas branch — this PATCH \
             never touched any branch, so the post-failure state must equal \
             the pre-PATCH state. Got branch at {branch_sha_after:?}."
        );

        // Audit log: the helper must emit a structured WARN
        // explicitly noting that the worktree was already absent.
        assert!(
            captured.contains("alms.worktree"),
            "no-op-remove event must use the alms.worktree tracing target: {captured}"
        );
        assert!(
            captured.contains("already absent") || captured.contains("AlreadyAbsent"),
            "no-op-remove event must explicitly call out the pre-absent \
             state so operators can spot drift in audit logs: {captured}"
        );
        assert!(
            !captured.contains("compensation succeeded")
                && !captured.contains("compensation also failed"),
            "no compensation must run, so neither 'compensation succeeded' \
             nor 'compensation also failed' must appear: {captured}"
        );
    }

    /// Codex P1 round 3 (#1019): when the worktree dir is already
    /// absent on disk but the `alms/<name>` branch exists with
    /// real commits, `remove_worktree` returns `AlreadyAbsent`
    /// (the dir no-op) but ALSO calls `delete_branch(...)` as
    /// best-effort cleanup --- silently nuking the operator branch
    /// history. Pre-fix the helper gated compensation on
    /// `did_remove == false`, so on persist failure no
    /// compensation ran and the branch (with all its commits) was
    /// gone for good.
    ///
    /// Post-fix contract: when we hold a pre-remove SHA snapshot,
    /// compensation MUST restore the branch at that tip even when
    /// `did_remove == false`. The committed file must be
    /// reachable through the restored branch --- direct evidence
    /// that no agent-only commits were lost in the failure path
    /// this helper exists to make reversible.
    #[test]
    fn git_to_off_persist_failure_restores_branch_when_worktree_already_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_git_repo(tmp.path());

        // Provision the worktree, commit real agent work on the
        // branch, then detach the worktree by hand (simulating
        // operator drift / a previously-failed PATCH that left
        // the branch but not the dir). The branch with commits
        // must survive on its own.
        let worktree_path = worktree::create_worktree(tmp.path(), "atlas")
            .unwrap()
            .into_path();
        std::fs::write(
            worktree_path.join("agent-state.txt"),
            "important agent work that must survive",
        )
        .unwrap();
        let run = |dir: &std::path::Path, args: &[&str]| {
            let s = std::process::Command::new("git")
                .current_dir(dir)
                .env("GIT_TERMINAL_PROMPT", "0")
                .args(args)
                .status()
                .expect("git command");
            assert!(s.success(), "git {args:?} failed in {}", dir.display());
        };
        run(
            &worktree_path,
            &["config", "user.email", "test@example.com"],
        );
        run(&worktree_path, &["config", "user.name", "Test"]);
        run(&worktree_path, &["add", "agent-state.txt"]);
        run(
            &worktree_path,
            &["commit", "-m", "agent-only commit (must survive)"],
        );

        let pre_flip_sha = worktree::read_branch_head_sha(tmp.path(), "atlas")
            .unwrap()
            .expect("branch must exist after commit");

        // Detach the worktree (so git stops tracking it as a
        // live worktree and removes the directory) but leave the
        // branch behind. The helper will see
        // `target.exists() == false` and return
        // `WorktreeRemove::AlreadyAbsent` while `delete_branch`
        // fires as best-effort cleanup --- the exact code path
        // Codex P1 round 3 calls out.
        let detach = std::process::Command::new("git")
            .current_dir(tmp.path())
            .env("GIT_TERMINAL_PROMPT", "0")
            .args([
                "worktree",
                "remove",
                "--force",
                worktree_path.to_str().unwrap(),
            ])
            .output()
            .expect("git worktree remove");
        assert!(
            detach.status.success(),
            "test setup: detach worktree failed: {}",
            String::from_utf8_lossy(&detach.stderr)
        );
        assert!(
            !worktree_path.exists(),
            "test setup: worktree dir must be gone after `git worktree remove`"
        );
        let surviving_sha = worktree::read_branch_head_sha(tmp.path(), "atlas")
            .unwrap()
            .expect("branch alms/atlas must survive `git worktree remove`");
        assert_eq!(
            surviving_sha, pre_flip_sha,
            "test setup invariant: branch tip must still be at pre_flip_sha after \
             worktree dir removal --- only the dir is gone, not the branch."
        );

        // Drive the git->off flip with a synthetic persist failure.
        let result = apply_worktree_flip_and_persist(
            tmp.path(),
            "atlas",
            WorktreeMode::Git,
            WorktreeMode::Off,
            true, // force
            false,
            || {
                Err(alms_core::AlmsError::Runtime(
                    "SQLite update_agent: database is locked".into(),
                ))
            },
        );
        let (status, body) = result.expect_err("persist failure must surface as Err");
        assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            err_code(&body.0),
            "INTERNAL",
            "compensation succeeded (branch restored), so the wire shape \
             stays INTERNAL --- not WORKTREE_COMPENSATION_FAILED"
        );

        // Contract: post-failure branch must still point at
        // pre_flip_sha, with the committed content reachable.
        let post_flip_sha = worktree::read_branch_head_sha(tmp.path(), "atlas")
            .unwrap()
            .expect(
                "compensation MUST restore the alms/atlas branch even when the \
                 worktree dir was already absent --- the branch was deleted by \
                 `delete_branch` on the AlreadyAbsent path",
            );
        assert_eq!(
            post_flip_sha, pre_flip_sha,
            "compensation must restore the branch at the snapshotted SHA \
             ({pre_flip_sha}), not lose the agent-only commits. Got {post_flip_sha}. \
             This is the silent-data-loss bug Codex P1 round 3 caught on PR #1019."
        );

        // Belt-and-braces: the committed file must be reachable
        // through the restored branch tip. Use `git show` so we
        // are not depending on the worktree dir layout.
        let show = std::process::Command::new("git")
            .current_dir(tmp.path())
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["show", "alms/atlas:agent-state.txt"])
            .output()
            .expect("git show");
        assert!(
            show.status.success(),
            "agent-state.txt must be reachable through the restored branch: stderr={}",
            String::from_utf8_lossy(&show.stderr)
        );
        let content = String::from_utf8_lossy(&show.stdout);
        assert!(
            content.contains("important agent work that must survive"),
            "restored branch must carry the agent commit content; got: {content}"
        );
    }

    /// Tim Tier 2 (PR #1019): symmetric counterpart of
    /// `off_to_git_persist_and_compensation_both_fail_surfaces_both_errors`.
    /// When BOTH the SQLite write AND the compensating
    /// `restore_worktree_at_sha` fail, the helper must surface a
    /// `WORKTREE_COMPENSATION_FAILED` error code carrying both
    /// error strings, and emit an ERROR-level structured log on
    /// the `alms.worktree` target tagged with the `git->off`
    /// direction. On-disk state stays diverged --- the test exists
    /// to assert that the divergence is SURFACED, not hidden.
    #[test]
    fn git_to_off_persist_and_compensation_both_fail_surfaces_both_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_git_repo(tmp.path());

        // Set up the same shape as
        // `git_to_off_persist_failure_preserves_branch_history`:
        // a real worktree with a real agent-only commit so the
        // restore step has something to do.
        let worktree_path = worktree::create_worktree(tmp.path(), "atlas")
            .unwrap()
            .into_path();
        std::fs::write(worktree_path.join("agent-state.txt"), "agent work").unwrap();
        let run = |dir: &std::path::Path, args: &[&str]| {
            let s = std::process::Command::new("git")
                .current_dir(dir)
                .env("GIT_TERMINAL_PROMPT", "0")
                .args(args)
                .status()
                .expect("git command");
            assert!(s.success(), "git {args:?} failed in {}", dir.display());
        };
        run(
            &worktree_path,
            &["config", "user.email", "test@example.com"],
        );
        run(&worktree_path, &["config", "user.name", "Test"]);
        run(&worktree_path, &["add", "agent-state.txt"]);
        run(&worktree_path, &["commit", "-m", "agent commit"]);

        let captured = capture_logs(tracing::Level::ERROR, || {
            let result = apply_worktree_flip_and_persist(
                tmp.path(),
                "atlas",
                WorktreeMode::Git,
                WorktreeMode::Off,
                true, // force
                false,
                || {
                    // Inside the persist closure --- after the
                    // worktree has already been removed AND the
                    // branch deleted --- nuke `.git` so the
                    // compensation step `restore_worktree_at_sha`
                    // fails on `is_git_repo` (returns
                    // `NotAGitRepo`). Same trick the off->git
                    // dual-failure test uses, applied to the
                    // symmetric direction.
                    std::fs::remove_dir_all(tmp.path().join(".git"))
                        .expect("nuke .git to break compensation");
                    Err(alms_core::AlmsError::Runtime(
                        "SQLite update_agent: disk full".into(),
                    ))
                },
            );
            let (status, body) = result.expect_err("dual failure must surface as Err");
            assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(err_code(&body.0), "WORKTREE_COMPENSATION_FAILED");

            // Wire body must carry both error strings so the
            // operator audit trail is complete.
            let msg = body.0["error"]["message"].as_str().unwrap_or_default();
            assert!(
                msg.contains("disk full"),
                "wire body must include the persist error: {msg}"
            );
            assert!(
                msg.contains("Compensation error"),
                "wire body must explicitly mention compensation failure: {msg}"
            );
        });

        // Audit log: ERROR-level event with BOTH persist_error and
        // compensation_error fields, the explicit "compensation
        // also failed" verb, and direction tagged "git->off".
        assert!(
            captured.contains("alms.worktree"),
            "dual-failure event must use the alms.worktree tracing target: {captured}"
        );
        assert!(
            captured.contains("ERROR"),
            "dual failure must log at ERROR level (not WARN): {captured}"
        );
        assert!(
            captured.contains("persist_error"),
            "dual-failure event must carry structured persist_error: {captured}"
        );
        assert!(
            captured.contains("compensation_error"),
            "dual-failure event must carry structured compensation_error: {captured}"
        );
        assert!(
            captured.contains("compensation also failed"),
            "dual-failure event message must say compensation also failed: {captured}"
        );
        assert!(
            captured.contains("git->off"),
            "dual-failure event must tag direction = git->off: {captured}"
        );
    }

    // ── #1022: POST /agents worktree side-effect must compensate on
    //          SQLite persist failure (non-DuplicateName variants) ──

    /// Issue #1022 acceptance (POST happy compensation path): when
    /// `POST /agents` with `worktree_mode = git` runs the worktree
    /// create + SQLite insert and the insert fails with a non-
    /// `DuplicateName` error (the bug Tim called out on PR #1019 —
    /// pre-#1022 only `DuplicateName` triggered rollback, every
    /// other variant leaked the just-created worktree dir), the
    /// helper must clean up the worktree before the error surfaces.
    /// On-disk state stays consistent with the (still-absent)
    /// registry row.
    #[test]
    fn post_create_persist_failure_compensates_by_deleting_worktree() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let worktree_dir = tmp.path().join(".alms").join("worktrees").join("atlas");

        let captured = capture_logs(tracing::Level::WARN, || {
            let result =
                apply_worktree_op_and_persist(tmp.path(), "atlas", WorktreeOp::Create, || {
                    // Synthetic SQLite failure — same shape "database
                    // is locked" / disk-full / fs-perm would surface
                    // from `store.create_agent(...)` in the production
                    // handler. Pre-#1022 this path only handled
                    // `DuplicateName`; every other variant leaked the
                    // worktree.
                    Err(alms_core::AlmsError::Runtime(
                        "SQLite create_agent: database is locked".into(),
                    ))
                });
            let (status, body) = result.expect_err("persist failure must surface as Err");
            assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(err_code(&body.0), "INTERNAL");
        });

        // Disk: worktree must be gone — compensation deleted what we
        // just created. This is the bug fix vs pre-#1022.
        assert!(
            !worktree_dir.exists(),
            "compensation must delete the just-created worktree on POST persist failure; \
             still found at {} — pre-#1022 the worktree was orphaned on any non-DuplicateName error",
            worktree_dir.display(),
        );

        // Audit log: structured WARN at alms.worktree with direction
        // off->git (Create op uses the same direction as the PATCH
        // off→git flip because they share the same forward op).
        assert!(
            captured.contains("alms.worktree"),
            "compensation event must use the alms.worktree tracing target: {captured}"
        );
        assert!(
            captured.contains("direction=\"off->git\"") || captured.contains("direction=off->git"),
            "Create-op compensation event must carry structured direction=off->git: {captured}"
        );
        assert!(
            captured.contains("compensation succeeded"),
            "compensation event message must say 'compensation succeeded': {captured}"
        );
    }

    /// Issue #1022 acceptance (POST dual-failure path): when BOTH
    /// the SQLite insert AND the compensating `remove_worktree`
    /// fail, the helper must surface `WORKTREE_COMPENSATION_FAILED`
    /// carrying both error strings, and emit an ERROR-level
    /// structured log on the `alms.worktree` target tagged with
    /// the `off->git` direction. Mirrors the PATCH off→git
    /// dual-failure test that landed in #1019.
    #[test]
    fn post_create_persist_and_compensation_both_fail_surfaces_both_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let worktree_dir = tmp.path().join(".alms").join("worktrees").join("atlas");

        let captured = capture_logs(tracing::Level::ERROR, || {
            let result =
                apply_worktree_op_and_persist(tmp.path(), "atlas", WorktreeOp::Create, || {
                    // Inside the persist closure, after the worktree
                    // create has touched disk, nuke `.git` so the
                    // compensation step `remove_worktree` fails on
                    // its underlying `git worktree remove` call.
                    // Same dual-failure recipe the PATCH off→git
                    // test uses.
                    std::fs::remove_dir_all(tmp.path().join(".git"))
                        .expect("nuke .git to break compensation");
                    Err(alms_core::AlmsError::Runtime(
                        "SQLite create_agent: disk full".into(),
                    ))
                });
            let (status, body) = result.expect_err("dual failure must surface as Err");
            assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(err_code(&body.0), "WORKTREE_COMPENSATION_FAILED");

            let msg = body.0["error"]["message"].as_str().unwrap_or_default();
            assert!(
                msg.contains("disk full"),
                "wire body must include the persist error: {msg}"
            );
            assert!(
                msg.contains("Compensation error"),
                "wire body must explicitly mention compensation failure: {msg}"
            );
        });

        // Disk: worktree dir still on disk (compensation tried to
        // delete it but git failed). The test asserts we SURFACED
        // the divergence rather than hiding it — same contract as
        // the PATCH dual-failure path.
        assert!(
            worktree_dir.exists(),
            "compensation failed, so the orphan worktree dir must still be on disk \
             — this is the divergence the operator needs to clean up manually"
        );

        assert!(
            captured.contains("alms.worktree"),
            "dual-failure event must use the alms.worktree tracing target: {captured}"
        );
        assert!(
            captured.contains("ERROR"),
            "dual failure must log at ERROR level (not WARN): {captured}"
        );
        assert!(
            captured.contains("compensation also failed"),
            "dual-failure event message must say 'compensation also failed': {captured}"
        );
        assert!(
            captured.contains("off->git"),
            "POST dual-failure event must tag direction = off->git: {captured}"
        );
    }

    /// Issue #1022 acceptance (POST race-DuplicateName + cleanup):
    /// the persist_err_mapper hook lets POST preserve its pre-#1022
    /// `409 DUPLICATE_NAME` wire shape for the race where another
    /// concurrent POST committed the same name between our
    /// load_agent_by_name check and our INSERT, while STILL running
    /// the compensation that cleans up our just-created worktree.
    /// The two contracts compose: client sees the correct
    /// category-409, and the orphan worktree dir is gone.
    #[test]
    fn post_create_race_duplicate_name_maps_to_409_and_cleans_up_worktree() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let worktree_dir = tmp.path().join(".alms").join("worktrees").join("atlas");

        let result = apply_worktree_op_and_persist_with_mapper(
            tmp.path(),
            "atlas",
            WorktreeOp::Create,
            || {
                // Synthetic race-DuplicateName — the production
                // handler would surface this when a concurrent
                // POST committed the same name between our
                // pre-call uniqueness probe and our INSERT.
                Err(alms_core::AlmsError::DuplicateName("atlas".into()))
            },
            |e| match e {
                alms_core::AlmsError::DuplicateName(name) => Some(api_error(
                    axum::http::StatusCode::CONFLICT,
                    "DUPLICATE_NAME",
                    format!("Agent name '{name}' already exists"),
                )),
                _ => None,
            },
        );

        let (status, body) = result.expect_err("DuplicateName must surface as Err");
        // Wire shape: mapper-specified 409 DUPLICATE_NAME, NOT the
        // default 500 INTERNAL. This preserves the pre-#1022
        // wire-shape contract for the race case.
        assert_eq!(status, axum::http::StatusCode::CONFLICT);
        assert_eq!(err_code(&body.0), "DUPLICATE_NAME");

        // Disk: worktree must STILL be gone — the mapper only
        // rewrites the wire shape; compensation still ran and
        // cleaned up the orphan dir. This is the strict superset
        // of pre-#1022 behaviour: same client-visible error code,
        // PLUS the cleanup that was previously only attempted via
        // the inline `remove_worktree` call in the create_agent
        // handler (which only fired for `DuplicateName`, never for
        // other variants — the gap #1022 closes).
        assert!(
            !worktree_dir.exists(),
            "DuplicateName must STILL clean up the worktree on the mapped wire shape; \
             still found at {}",
            worktree_dir.display(),
        );
    }

    // ── #1022: DELETE /agents worktree side-effect must compensate
    //          on SQLite persist failure ──────────────────────────

    /// Issue #1022 acceptance (DELETE happy compensation path):
    /// when `DELETE /agents/<id>` with `worktree_mode = git` runs
    /// the worktree remove + SQLite delete and the delete fails
    /// (db locked, disk full, fs perm), the helper must restore
    /// the worktree at its pre-call SHA before the error surfaces.
    /// On-disk state stays consistent with the (still-present)
    /// registry row. Pre-#1022 the SQLite failure left the agent
    /// record in place but the worktree gone — silent half-deleted
    /// state.
    #[test]
    fn delete_persist_failure_compensates_by_restoring_worktree() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_git_repo(tmp.path());

        // Pre-provision the worktree with a real commit so the
        // restore step has actual branch history to preserve. This
        // mirrors the PATCH `git_to_off_persist_failure_preserves_branch_history`
        // shape — DELETE compensation is structurally identical to
        // PATCH git→off compensation (same SHA snapshot + restore
        // dance under the hood).
        let _worktree_path = provision_worktree_with_commit(
            tmp.path(),
            "atlas",
            "important agent work that must survive DELETE compensation",
            "agent commit — must survive DELETE rollback",
        );

        let pre_call_sha = worktree::read_branch_head_sha(tmp.path(), "atlas")
            .unwrap()
            .expect("agent branch must exist after commit");

        let captured = capture_logs(tracing::Level::WARN, || {
            let result = apply_worktree_op_and_persist(
                tmp.path(),
                "atlas",
                WorktreeOp::Remove { force: true },
                || {
                    Err(alms_core::AlmsError::Runtime(
                        "SQLite delete_agent: database is locked".into(),
                    ))
                },
            );
            let (status, body) = result.expect_err("persist failure must surface as Err");
            assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(err_code(&body.0), "INTERNAL");
        });

        // Disk: worktree dir is back AND the branch points at the
        // pre-call SHA (not parent HEAD — the SHA-snapshot machinery
        // shared with PATCH git→off preserves agent history).
        let worktree_dir = tmp.path().join(".alms").join("worktrees").join("atlas");
        assert!(
            worktree_dir.is_dir(),
            "compensation must recreate the worktree directory after DELETE persist failure"
        );

        let post_call_sha = worktree::read_branch_head_sha(tmp.path(), "atlas")
            .unwrap()
            .expect("compensation must restore the alms/atlas branch");
        assert_eq!(
            post_call_sha, pre_call_sha,
            "DELETE compensation must restore alms/atlas to its pre-call tip ({pre_call_sha}). \
             Got: {post_call_sha}. The branch-SHA snapshot machinery from #1019 PATCH \
             compensation is shared with DELETE in #1022 — pre-fix the branch came back \
             pointing at parent HEAD and the agent's commits were orphaned in the reflog."
        );

        // Belt-and-braces: the committed file must be reachable
        // through the restored worktree. Direct evidence that
        // branch history survived the DELETE round trip.
        assert!(
            worktree_dir.join("agent-state.txt").exists(),
            "restored worktree must carry the committed file — DELETE compensation \
             must preserve agent branch history, not re-fork from HEAD"
        );

        // Audit log: structured WARN at alms.worktree with direction
        // git->off (Remove op uses the same direction as the PATCH
        // git→off flip — they share the same forward op shape).
        assert!(
            captured.contains("alms.worktree"),
            "compensation event must use the alms.worktree tracing target: {captured}"
        );
        assert!(
            captured.contains("direction=\"git->off\"") || captured.contains("direction=git->off"),
            "Remove-op compensation event must carry structured direction=git->off: {captured}"
        );
        assert!(
            captured.contains("compensation succeeded"),
            "compensation event message must say 'compensation succeeded': {captured}"
        );
    }

    /// Issue #1022 acceptance (DELETE dual-failure path): when BOTH
    /// the SQLite delete AND the compensating
    /// `restore_worktree_at_sha` fail, the helper must surface
    /// `WORKTREE_COMPENSATION_FAILED` carrying both error strings,
    /// and emit an ERROR-level structured log on the
    /// `alms.worktree` target tagged with the `git->off`
    /// direction. Mirrors the PATCH git→off dual-failure test
    /// shape from #1019.
    #[test]
    fn delete_persist_and_compensation_both_fail_surfaces_both_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_git_repo(tmp.path());

        // Same fixture shape as `delete_persist_failure_compensates_by_restoring_worktree`
        // — provision + real commit so the restore step has
        // something to do, then nuke `.git` inside the persist
        // closure so compensation fails on `is_git_repo`.
        let _worktree_path =
            provision_worktree_with_commit(tmp.path(), "atlas", "agent work", "agent commit");

        let captured = capture_logs(tracing::Level::ERROR, || {
            let result = apply_worktree_op_and_persist(
                tmp.path(),
                "atlas",
                WorktreeOp::Remove { force: true },
                || {
                    std::fs::remove_dir_all(tmp.path().join(".git"))
                        .expect("nuke .git to break compensation");
                    Err(alms_core::AlmsError::Runtime(
                        "SQLite delete_agent: disk full".into(),
                    ))
                },
            );
            let (status, body) = result.expect_err("dual failure must surface as Err");
            assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(err_code(&body.0), "WORKTREE_COMPENSATION_FAILED");

            let msg = body.0["error"]["message"].as_str().unwrap_or_default();
            assert!(
                msg.contains("disk full"),
                "wire body must include the persist error: {msg}"
            );
            assert!(
                msg.contains("Compensation error"),
                "wire body must explicitly mention compensation failure: {msg}"
            );
        });

        assert!(
            captured.contains("alms.worktree"),
            "dual-failure event must use the alms.worktree tracing target: {captured}"
        );
        assert!(
            captured.contains("ERROR"),
            "dual failure must log at ERROR level (not WARN): {captured}"
        );
        assert!(
            captured.contains("compensation also failed"),
            "dual-failure event message must say 'compensation also failed': {captured}"
        );
        assert!(
            captured.contains("git->off"),
            "DELETE dual-failure event must tag direction = git->off: {captured}"
        );
    }

    /// Issue #1022 (handler-level end-to-end): drive the real
    /// `delete_agent` HTTP handler with a worktree-mode-git agent
    /// and a force-true query. Happy path — verifies the
    /// handler-to-helper wiring is intact AND the pre-#1022
    /// observable contract (worktree gone, agent record gone) is
    /// preserved. Complements the synthetic-failure helper-level
    /// tests above by exercising the actual `AppState` /
    /// `SqliteStore` plumbing end-to-end.
    #[tokio::test]
    async fn delete_handler_routes_through_worktree_op_helper_on_happy_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = agents_test_state_with_git_project(tmp.path());

        // Create the agent with worktree_mode = git so the DELETE
        // path enters the `apply_worktree_op_and_persist` branch.
        let create_req = alms_core::CreateAgentRequest {
            name: "atlas".into(),
            description: None,
            model: None,
            posture: None,
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
            worktree_mode: Some(WorktreeMode::Git),
            debug_mode: None,
            is_default: None,
        };
        let _ = create_agent(axum::extract::State(state.clone()), Json(create_req))
            .await
            .expect("create must succeed");

        let worktree_dir = tmp.path().join(".alms").join("worktrees").join("atlas");
        assert!(worktree_dir.is_dir(), "setup: worktree must exist");

        // Happy DELETE — both the worktree and the registry row go
        // away cleanly. Wire shape stays `{"ok": true, "deleted": ...}`.
        let result = delete_agent(
            axum::extract::State(state.clone()),
            axum::extract::Path("atlas".into()),
            axum::extract::Query(DeleteAgentQuery { force: true }),
        )
        .await
        .expect("happy DELETE must succeed");
        assert_eq!(result.0["ok"], serde_json::json!(true));

        assert!(
            !worktree_dir.exists(),
            "happy DELETE must remove the worktree (handler-to-helper wiring)"
        );
        let store = state.session_manager.store().expect("sqlite store");
        assert!(
            store.load_agent_by_name("atlas").unwrap().is_none(),
            "happy DELETE must remove the registry row"
        );
    }

    /// Issue #1022 (handler-level end-to-end): drive the real
    /// `create_agent` HTTP handler with worktree_mode = git on a
    /// real git project and verify the happy-path wire shape is
    /// unchanged from pre-#1022. The compensation paths are
    /// already covered at the helper level above; this test pins
    /// the handler-to-helper wiring so a future refactor cannot
    /// silently route POST around the helper.
    #[tokio::test]
    async fn create_handler_routes_through_worktree_op_helper_on_happy_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = agents_test_state_with_git_project(tmp.path());

        let req = alms_core::CreateAgentRequest {
            name: "atlas".into(),
            description: None,
            model: None,
            posture: None,
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
            worktree_mode: Some(WorktreeMode::Git),
            debug_mode: None,
            is_default: None,
        };

        let (status, body) = create_agent(axum::extract::State(state.clone()), Json(req))
            .await
            .expect("create must succeed on git project");
        assert_eq!(status, axum::http::StatusCode::CREATED);
        assert_eq!(body.0["worktree_mode"], serde_json::json!("git"));

        let worktree_dir = tmp.path().join(".alms").join("worktrees").join("atlas");
        assert!(
            worktree_dir.is_dir(),
            "happy POST must provision the worktree (handler-to-helper wiring)"
        );
        let store = state.session_manager.store().expect("sqlite store");
        let stored = store.load_agent_by_name("atlas").unwrap().unwrap();
        assert_eq!(stored.worktree_mode, WorktreeMode::Git);
    }
}
