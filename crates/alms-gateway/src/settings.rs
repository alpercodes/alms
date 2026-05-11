//! Settings endpoints — exposes server-side defaults for UI pre-population
//! and accepts partial config updates via PATCH.
//!
//! Server-level settings (context, session, tools) are persisted to
//! `{data_dir}/settings.json` so they survive restarts.

use crate::server::AppState;
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

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
        "fs_edit",
        "fs_glob",
        "fs_grep",
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

    // #871 (Tim's nit on PR #871): expose the configured provider names so
    // the UI can populate the summary-provider dropdown from
    // `state.llm_config.providers` instead of a hardcoded four-entry list.
    // Names are sorted for deterministic UI ordering. Only the keys are
    // exposed — base URLs / API keys never leave the server.
    let mut llm_provider_names: Vec<String> = state.llm_config.providers.keys().cloned().collect();
    llm_provider_names.sort();

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
        // #871 (Tim's nit): provider names from `[llm.providers.<name>]`,
        // sorted alphabetically. UI uses this to populate the summary-provider
        // dropdown so user-defined providers in alms.toml show up automatically.
        "llm_providers": llm_provider_names,
        // Context settings
        "context": {
            "strategy": ctx.strategy,
            "max_input_tokens": ctx.max_input_tokens,
            // #869: threshold-based compaction knobs replace the
            // pre-#869 `recent_window` / `summary_interval` pair on
            // the wire. Old UI bundles that read `recent_window` will
            // see a missing key (defaulting to undefined / null on
            // their side); shipping this key removal atomically with
            // the bundled `settings-modal.js` update avoids in-flight
            // drift.
            "compact_trigger_pct": ctx.compact_trigger_pct,
            "compact_retain_pct": ctx.compact_retain_pct,
            "summary_model": ctx.summary_model,
            "summary_provider": ctx.summary_provider,
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
        // Security settings (#947) — informational / read-only.
        // PATCH /settings rejects any payload referencing this section
        // with `400 SECURITY_KNOB_NOT_PATCHABLE`; operators must edit
        // `[security]` in `alms.toml` and restart.
        "security": {
            "allow_full_os_access": state.security_config.allow_full_os_access,
        },
        // LLM provider-family settings (#809). Mirrors the shape of the
        // `[llm.anthropic]` / `[llm.openai]` / `[llm.gemini]` blocks in
        // `alms.toml`. Server-level defaults only; per-agent overrides
        // live on the agent registry. Per-run overrides were removed in
        // #941 — agents are the single per-tenant config surface.
        "llm": {
            "anthropic": {
                "thinking_budget_tokens": agent.anthropic_thinking_budget,
                "prompt_cache_enabled": agent.anthropic_prompt_cache_enabled,
            },
            "openai": {
                "reasoning_effort": agent.openai_reasoning_effort
                    .as_ref()
                    .map(|e| e.as_wire_str()),
            },
            "gemini": {
                "thinking_budget": agent.gemini_thinking_budget,
                "cache_enabled": agent.gemini_cache_enabled,
                "cache_ttl_seconds": agent.gemini_cache_ttl_seconds,
            },
        },
    }))
}

// ── PATCH /settings — partial config update ────────────────────────────

/// Partial context config update.
///
/// #869: `recent_window` and `summary_interval` were removed from the
/// PATCH surface and replaced with the threshold-based
/// [`Self::compact_trigger_pct`] / [`Self::compact_retain_pct`] knobs.
/// Old UI bundles or scripted clients that still send the legacy fields
/// receive a `400 CONTEXT_LEGACY_FIELD_DEPRECATED` so callers fail loud
/// rather than silently no-op'ing — see `patch_settings`.
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PatchContext {
    pub strategy: Option<String>,
    pub max_input_tokens: Option<usize>,
    /// #869: trigger compaction when assembled history exceeds this fraction
    /// of `max_input_tokens`. Range: `0.50..=0.95`.
    pub compact_trigger_pct: Option<f32>,
    /// #869: after compaction, retain at most this fraction of
    /// `max_input_tokens` worth of recent verbatim messages.
    /// Range: `0.20..=0.60`.
    pub compact_retain_pct: Option<f32>,
    pub summary_model: Option<String>,
    /// Separate provider for the summary task (#866). Empty string clears
    /// back to "inherit agent provider"; non-empty must reference a
    /// configured `[llm.providers.<name>]` block whose API key is resolvable.
    pub summary_provider: Option<String>,
}

/// Partial session config update.
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PatchSession {
    pub max_messages: Option<usize>,
    pub max_context_tokens: Option<usize>,
    pub idle_timeout_secs: Option<u64>,
    pub auto_archive: Option<bool>,
    pub archive_ttl_secs: Option<u64>,
}

/// Partial tools config update.
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PatchTools {
    pub shell_policy: Option<String>,
    pub sandbox_root: Option<String>,
    pub timeout_secs: Option<u64>,
    pub max_output_bytes: Option<usize>,
}

/// Partial Anthropic LLM config update (#809).
///
/// Mirrors `[llm.anthropic]` in `alms.toml`. Fields that are `None` are
/// untouched; fields that are `Some` overwrite the live server default.
/// The provider-neutral `AgentConfig` (which is what the agent loop reads
/// per run) is what the PATCH handler mutates — no restart required.
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PatchLlmAnthropic {
    pub thinking_budget_tokens: Option<u32>,
    pub prompt_cache_enabled: Option<bool>,
}

/// Partial OpenAI-compat LLM config update (#809).
///
/// Mirrors `[llm.openai]` in `alms.toml`. `reasoning_effort = None` in the
/// patch body means "leave unchanged"; to clear the server default back
/// to "don't send the field on the wire", use an empty-string value —
/// i.e. `{ "reasoning_effort": "" }` — which the handler treats as an
/// explicit clear sentinel. This matches the existing pattern used by
/// `PatchContext::summary_model`.
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PatchLlmOpenai {
    pub reasoning_effort: Option<String>,
}

/// Partial Gemini LLM config update (#809).
///
/// Mirrors `[llm.gemini]` in `alms.toml`. `thinking_budget = Some(0)` is
/// a legitimate value meaning "disable extended thinking server-wide";
/// it is not a clear sentinel. Three-layer precedence (per-run >
/// per-agent > server default) still applies downstream.
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PatchLlmGemini {
    pub thinking_budget: Option<u32>,
    pub cache_enabled: Option<bool>,
    pub cache_ttl_seconds: Option<u64>,
}

/// Partial LLM config update (#809).
///
/// Nested under `llm` in the top-level PATCH /settings body. Each
/// provider family has its own sub-object; absent sub-objects are
/// untouched.
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PatchLlm {
    pub anthropic: Option<PatchLlmAnthropic>,
    pub openai: Option<PatchLlmOpenai>,
    pub gemini: Option<PatchLlmGemini>,
}

/// Top-level PATCH /settings request body.
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PatchSettingsRequest {
    pub context: Option<PatchContext>,
    pub session: Option<PatchSession>,
    pub tools: Option<PatchTools>,
    pub llm: Option<PatchLlm>,
}

/// PATCH /settings — apply partial config updates to the running server.
///
/// The context, session, tools, and llm (#809) sections are mutable at
/// runtime. Logging requires a restart and is not accepted here.
/// Changes take effect on the next run (in-flight runs are unaffected).
///
/// **Security knobs are rejected up front (#947).** Any payload referencing
/// `security` (currently `security.allow_full_os_access`) is rejected
/// with **400 `SECURITY_KNOB_NOT_PATCHABLE`** before any other field is
/// processed. The `[security]` section is config-file-only — see
/// [`alms_core::config::SecurityConfig`] for the threat model. Other
/// fields in the same payload are ignored (the request is rejected as a
/// whole, not partially applied), so an attacker cannot ride a security
/// flip in alongside a benign-looking field. Mirrors the rejection
/// shape of `classifier_overrides` from #745 — config-file-only knobs
/// are not exposed via PATCH on principle.
///
/// Live-mutation propagation is **HTTP-path only**: mutations write through
/// to the shared `Arc<RwLock<AgentConfig>>` referenced by the HTTP `POST
/// /runs` and Coordinator paths. Telegram-triggered runs read from a
/// boot-time clone held inside `Gateway` and continue to use the snapshot
/// until the daemon restarts. See `gateway.rs::Gateway::run_telegram` for
/// the inheritance site. This is pre-existing behaviour for the
/// `context` / `session` / `tools` sections and is documented in
/// `docs/api.md` § 10.2.
pub async fn patch_settings(
    State(state): State<AppState>,
    Json(raw_body): Json<serde_json::Value>,
) -> impl IntoResponse {
    // ── Security-knob rejection (#947) ─────────────────────────────────
    //
    // Inspect the raw payload BEFORE deserialising into
    // `PatchSettingsRequest` so a request that names `security.*` is
    // rejected even when its `security` sub-object is otherwise empty
    // (e.g. `{ "security": {} }`). This is the same shape Tim asked for
    // on the #745 review of the `classifier_overrides` PATCH guard:
    // config-file-only knobs surface a single, deterministic error code
    // and never partially apply.
    if let Some(rejection) = reject_security_knob(&raw_body) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "error",
                "code": "SECURITY_KNOB_NOT_PATCHABLE",
                "errors": [rejection],
            })),
        );
    }

    // #869: reject deprecated context fields BEFORE structural deserialise
    // so callers that still send the pre-#869 shape fail loud rather than
    // silently no-op'ing once we drop the fields. The TOML / `alms.toml`
    // path stays soft (one-time WARN at boot) because operators editing
    // config files don't read 4xx responses, but the HTTP API exposes
    // ALMS to scripted clients that should be updated atomically with
    // the rest of the surface.
    if let Some(rejection) = reject_legacy_context_fields(&raw_body) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "error",
                "code": "CONTEXT_LEGACY_FIELD_DEPRECATED",
                "errors": [rejection],
            })),
        );
    }

    let body: PatchSettingsRequest = match serde_json::from_value(raw_body) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "status": "error",
                    "errors": [format!("Invalid PATCH /settings body: {e}")],
                })),
            );
        }
    };

    // ── Token-budget pre-validation (#919, PR #1020 Tim review item 3) ──
    //
    // If the patch carries a positive `context.max_input_tokens`, run the
    // same budget validator the per-run path uses (`pre_flight_token_budget`
    // in `runs/lifecycle.rs`) BEFORE any commit lands. Computes a candidate
    // next-state from the patched value plus the live `max_tokens` and
    // boot-time `(provider, model)`. On strict-mode overshoot the whole
    // PATCH is rejected with a structured `400 INVALID_TOKEN_BUDGET_FOR_PROVIDER`
    // — no partial commits in any other section. On warn-mode the same
    // structured WARN log fires and the PATCH proceeds.
    //
    // Without this guard an operator could PATCH `[context].max_input_tokens`
    // upward, get a 200, and only discover the overshoot on the next
    // `POST /runs` rejection — the run-side validator would catch it but
    // the PATCH-time UX would already have led the operator to believe
    // the value was accepted. Mirroring the per-run shape closes that
    // trap.
    // Codex P2 #1020 follow-up: `validate_patch_budget` now returns a
    // `Result` so the fleet layer can fail CLOSED on `list_agents()`
    // errors instead of silently bypassing the per-agent check. `Err`
    // carries an explicit `(StatusCode, envelope)` pair (503 with
    // `AGENT_STORE_UNAVAILABLE`) which we forward verbatim, before any
    // mutation lands or `settings.json` is rewritten.
    match validate_patch_budget(&state, &body) {
        Ok(Some(rejection)) => return (StatusCode::BAD_REQUEST, Json(rejection)),
        Ok(None) => {}
        Err((status, envelope)) => return (status, Json(envelope)),
    }

    let mut errors: Vec<String> = Vec::new();

    // ── Context ────────────────────────────────────────────────────────
    if let Some(ctx_patch) = &body.context {
        let mut agent = state.agent_config.write();
        let ctx = &mut agent.context_config;

        if let Some(ref strategy) = ctx_patch.strategy {
            // #869: `compact` is the canonical name; `sliding-summary` is
            // accepted as a back-compat alias and rewritten on commit
            // (matching the deserialise-time rewrite in
            // `ContextConfig::Deserialize`). The UI bundle ships
            // `compact` only.
            let valid = ["compact", "sliding-summary", "full", "truncate"];
            if valid.contains(&strategy.as_str()) {
                ctx.strategy = if strategy == "sliding-summary" {
                    "compact".to_string()
                } else {
                    strategy.clone()
                };
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
        // #869 / Codex review (PR #1012): `compact_trigger_pct` and
        // `compact_retain_pct` form a cross-field invariant
        // (`retain + 0.10 <= trigger`). Pre-commit individual writes
        // followed by a post-commit gap check left a partial-mutation
        // hole — a same-PATCH pair like `{trigger: 0.55, retain: 0.50}`
        // passed both per-knob range checks, committed both writes,
        // then the gap check returned the would-be pair as invalid.
        // The daemon then served runs with the invalid pair until the
        // next successful PATCH or restart.
        //
        // Mirror the `summary_model` / `summary_provider` pattern below:
        // compute would-be candidate values, validate range AND gap on
        // the candidates, commit only when every check passes. Any
        // per-knob range failure preserves the live value for the
        // candidate the gap check sees, matching the documented
        // status: partial wire contract for unrelated fields.
        let mut next_compact_trigger_pct = ctx.compact_trigger_pct;
        let mut trigger_range_ok = true;
        if let Some(v) = ctx_patch.compact_trigger_pct {
            if !(0.50..=0.95).contains(&v) || !v.is_finite() {
                errors.push(format!(
                    "context.compact_trigger_pct must be in [0.50, 0.95], got {v}"
                ));
                trigger_range_ok = false;
            } else {
                next_compact_trigger_pct = v;
            }
        }
        let mut next_compact_retain_pct = ctx.compact_retain_pct;
        let mut retain_range_ok = true;
        if let Some(v) = ctx_patch.compact_retain_pct {
            if !(0.20..=0.60).contains(&v) || !v.is_finite() {
                errors.push(format!(
                    "context.compact_retain_pct must be in [0.20, 0.60], got {v}"
                ));
                retain_range_ok = false;
            } else {
                next_compact_retain_pct = v;
            }
        }
        // Cross-field invariant: `retain + 0.10 <= trigger`. Evaluated
        // on the candidate pair (not on `ctx`) so a same-PATCH pair
        // that fails the gap leaves the live config untouched.
        let gap_ok = next_compact_retain_pct + 0.10 <= next_compact_trigger_pct;
        if !gap_ok
            && (ctx_patch.compact_trigger_pct.is_some() || ctx_patch.compact_retain_pct.is_some())
        {
            errors.push(format!(
                "context.compact_retain_pct ({next_compact_retain_pct}) must be at least 0.10 below \
                 compact_trigger_pct ({next_compact_trigger_pct}) — compaction must measurably reduce context size",
            ));
        }
        // Commit candidate values only when range AND gap pass for the
        // touched knobs. A patch that touches just one knob still
        // commits that one knob iff its own range check passed AND the
        // resulting pair satisfies the gap.
        if gap_ok {
            if trigger_range_ok && ctx_patch.compact_trigger_pct.is_some() {
                ctx.compact_trigger_pct = next_compact_trigger_pct;
            }
            if retain_range_ok && ctx_patch.compact_retain_pct.is_some() {
                ctx.compact_retain_pct = next_compact_retain_pct;
            }
        }

        // #866 / #871: `summary_model` and `summary_provider` are validated
        // together because their post-PATCH combined state matters: when a
        // dedicated summary provider is configured, the agent's resolved
        // model name belongs to a different provider and must NOT be used
        // as a fallback for the summary task's wire model. The runtime has
        // a defense-in-depth leak guard in `runs/lifecycle.rs` that clears
        // the inherited model in this shape, but we reject the combination
        // up front so misconfigurations surface at PATCH time exactly like
        // `SUMMARY_PROVIDER_UNKNOWN` / `SUMMARY_PROVIDER_MISSING_API_KEY`
        // do (Option 1 philosophy from #866).
        //
        // Compute the would-be post-PATCH values without committing them,
        // validate the combined invariant, then commit.
        let next_summary_model: Option<String> = match ctx_patch.summary_model.as_ref() {
            Some(v) if v.is_empty() => None,
            Some(v) => Some(v.clone()),
            None => ctx.summary_model.clone(),
        };
        // Validate `summary_provider` if the patch touches it. `Ok(Some(p))`
        // = set to provider name `p`; `Ok(None)` = clear; per-field error
        // pushed onto `errors` and the live value preserved for the cross-
        // field check.
        let next_summary_provider_resolved: Option<String> =
            match ctx_patch.summary_provider.as_ref() {
                None => ctx.summary_provider.clone(),
                Some(v) if v.is_empty() => None,
                Some(v) => {
                    let entry = state.llm_config.providers.get(v);
                    let entry_has_key = entry.is_some_and(|e| e.resolve_api_key().is_some());
                    let secrets_has_key = state.secrets.read().resolve_key(v).is_some();
                    if entry.is_none() {
                        errors.push(format!(
                            "SUMMARY_PROVIDER_UNKNOWN: context.summary_provider '{v}' \
                         is not configured under [llm.providers.<name>] in alms.toml"
                        ));
                        ctx.summary_provider.clone()
                    } else if !entry_has_key && !secrets_has_key {
                        errors.push(format!(
                            "SUMMARY_PROVIDER_MISSING_API_KEY: context.summary_provider '{v}' \
                         has no resolvable API key — set one with `alms auth set {v}` or \
                         configure `[llm.providers.{v}].api_key_env` / `api_key`"
                        ));
                        ctx.summary_provider.clone()
                    } else {
                        Some(v.clone())
                    }
                }
            };

        // #871 / #872 cross-field invariant: the summary provider/model
        // pair must be symmetric — both set or both unset. The pre-#872
        // shape only validated provider-set + model-missing, but the
        // model-set + provider-missing direction is just as broken: the
        // resolver used to silently pair the user's `summary_model` with
        // the agent's primary provider (the exact misconfiguration that
        // produced the `model: not found` 404 in #866), and the v0.2.2
        // default config shipped in that asymmetric shape. Reject both
        // directions at PATCH time, regardless of which field the patch
        // touched.
        let provider_set = next_summary_provider_resolved.is_some();
        let model_set = next_summary_model.is_some();
        if provider_set && !model_set {
            errors.push(
                "SUMMARY_PROVIDER_REQUIRES_MODEL: context.summary_provider is set but \
                 context.summary_model is empty. The agent's resolved model belongs to \
                 the AGENT's provider namespace and is not a safe fallback for the \
                 summary provider's wire — set summary_model to a slug valid for the \
                 summary provider, or clear summary_provider (use empty string) to \
                 inherit the agent's provider/model."
                    .to_string(),
            );
        } else if !provider_set && model_set {
            errors.push(
                "SUMMARY_MODEL_REQUIRES_PROVIDER: context.summary_model is set but \
                 context.summary_provider is empty. The user-supplied summary_model \
                 belongs to a specific provider's namespace; pairing it with the \
                 agent's primary provider would 404 on the wire. Set \
                 summary_provider to the matching provider name, or clear summary_model \
                 (use empty string) to inherit the agent's provider/model."
                    .to_string(),
            );
        } else {
            // Cross-field invariant holds — commit the would-be values for
            // any field the patch touched. Per-field validation errors above
            // already preserved the live value, so this is a no-op for those.
            if ctx_patch.summary_model.is_some() {
                ctx.summary_model = next_summary_model;
            }
            if ctx_patch.summary_provider.is_some() {
                ctx.summary_provider = next_summary_provider_resolved;
            }
        }

        info!(
            strategy = %ctx.strategy,
            max_input_tokens = ctx.max_input_tokens,
            compact_trigger_pct = ctx.compact_trigger_pct,
            compact_retain_pct = ctx.compact_retain_pct,
            summary_provider = ?ctx.summary_provider,
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

    // ── LLM (provider-family defaults, #809) ──────────────────────────
    //
    // All six knobs live on the server-level `AgentConfig` which is the
    // single source of truth for new runs (see
    // `runs/mod.rs::resolve_agent_config` and
    // `runs/lifecycle.rs::execute_run`). Mutating it under the same
    // write lock as the other PATCH branches ensures the next `POST /runs`
    // picks up the new value without a daemon restart.
    if let Some(llm_patch) = &body.llm {
        let mut agent = state.agent_config.write();

        if let Some(ant) = &llm_patch.anthropic {
            if let Some(v) = ant.thinking_budget_tokens {
                agent.anthropic_thinking_budget = v;
            }
            if let Some(v) = ant.prompt_cache_enabled {
                agent.anthropic_prompt_cache_enabled = v;
            }
        }

        if let Some(oai) = &llm_patch.openai
            && let Some(ref v) = oai.reasoning_effort
        {
            if v.is_empty() {
                // Empty string = clear server default back to "don't
                // send reasoning_effort on the wire".
                agent.openai_reasoning_effort = None;
            } else {
                match v.parse::<alms_core::config::ReasoningEffort>() {
                    Ok(effort) => {
                        agent.openai_reasoning_effort = Some(effort);
                    }
                    Err(msg) => {
                        errors.push(format!("llm.openai.reasoning_effort: {msg}"));
                    }
                }
            }
        }

        if let Some(gem) = &llm_patch.gemini {
            if let Some(v) = gem.thinking_budget {
                // `Some(0)` is a legitimate server-level disable, not a
                // clear sentinel — preserved verbatim.
                agent.gemini_thinking_budget = Some(v);
            }
            if let Some(v) = gem.cache_enabled {
                agent.gemini_cache_enabled = v;
            }
            if let Some(v) = gem.cache_ttl_seconds {
                if v == 0 {
                    errors.push("llm.gemini.cache_ttl_seconds must be > 0".into());
                } else {
                    agent.gemini_cache_ttl_seconds = v;
                }
            }
        }

        // Tag the log with the rejected-fields count so a partial-failure
        // PATCH (e.g. one valid sub-field plus one rejected sub-field) is
        // not misread as a clean success — the message still says "Updated"
        // because some fields *did* land, but `errors_count > 0` is the
        // signal that the response was 422 not 200.
        info!(
            anthropic_thinking_budget = agent.anthropic_thinking_budget,
            anthropic_prompt_cache_enabled = agent.anthropic_prompt_cache_enabled,
            openai_reasoning_effort = ?agent.openai_reasoning_effort,
            gemini_thinking_budget = ?agent.gemini_thinking_budget,
            gemini_cache_enabled = agent.gemini_cache_enabled,
            gemini_cache_ttl_seconds = agent.gemini_cache_ttl_seconds,
            errors_count = errors.len(),
            "Updated LLM config via PATCH /settings"
        );
    }

    // Persist current settings to disk so they survive restarts — but only
    // when the request validates cleanly. A rejected PATCH (handler returns
    // 422) must be side-effect-free at the persistence layer: otherwise an
    // invalid request like `{ "llm": { "openai": { "reasoning_effort":
    // "turbo" } } }` would still rewrite `settings.json` from the current
    // live snapshot, and any field that *did* land before the validation
    // error (or earlier PATCH-applied values still in memory) would be
    // baked into the persisted snapshot — silently changing post-restart
    // behaviour for a request the operator was told failed.
    //
    // Note: this only closes the persistence half. Live `AgentConfig`
    // mutations are still applied inline above and a partial-failure 422
    // can leave the in-memory config half-mutated until the next
    // successful PATCH or a restart. That is the documented
    // "status: partial" wire contract — fixing it would require buffering
    // every mutation and applying atomically only after validation passes.
    // See PR #810 for context.
    if errors.is_empty() {
        persist_settings(&state);
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

// ── Security-knob rejection (#947) ────────────────────────────────────

/// Inspect a raw `PATCH /settings` body for any reference to the
/// `[security]` section.
///
/// Returns `Some(message)` when the payload contains a `security` key at
/// the top level — including `{ "security": {} }`, `{ "security": null }`,
/// and `{ "security": { "allow_full_os_access": [...] } }`. The caller
/// translates that into a `400 SECURITY_KNOB_NOT_PATCHABLE` response.
///
/// Empty / `null` payloads are explicitly rejected because permitting
/// them would create a back door: a future PATCH evolution that adds
/// `security` as a real serde field would silently start accepting the
/// shape that previously round-tripped to a no-op. Failing closed at the
/// raw-JSON layer is the same defensive posture Tim asked for on the
/// `classifier_overrides` rejection in #745.
///
/// Returns `None` when the payload does NOT reference `security` at the
/// top level — the normal happy path. Nested objects whose name happens
/// to be `security` (e.g. `{ "tools": { "security": ... } }`) are
/// ignored on purpose: this guard is about the top-level
/// [`PatchSettingsRequest::security`] surface, not arbitrary key
/// matches.
/// #869: reject deprecated `context.recent_window` / `context.summary_interval`
/// fields on the PATCH wire. The TOML loader is soft (one-time WARN) because
/// operators editing config files don't read 4xx responses; the HTTP
/// surface is loud because scripted clients (cached UI bundles, automation)
/// should fail fast and update.
///
/// Returns `Some(message)` when the payload's `context` object names
/// either legacy field — even with a `null` value, mirroring the
/// fail-closed posture of `reject_security_knob`. Returns `None` for
/// payloads that don't carry a `context` block at all, or carry one that
/// only references new fields.
fn reject_legacy_context_fields(body: &serde_json::Value) -> Option<String> {
    let ctx = body.as_object()?.get("context")?.as_object()?;
    let mut offenders: Vec<&str> = Vec::new();
    if ctx.contains_key("recent_window") {
        offenders.push("recent_window");
    }
    if ctx.contains_key("summary_interval") {
        offenders.push("summary_interval");
    }
    if offenders.is_empty() {
        return None;
    }
    Some(format!(
        "context.{} is deprecated as of v0.2.4 — the threshold-based \
         \"compact\" strategy uses compact_trigger_pct (0.50–0.95) and \
         compact_retain_pct (0.20–0.60). Update your client to send the \
         new fields. See #869.",
        offenders.join(" / context.")
    ))
}

/// Pre-validate a `PATCH /settings` body's candidate `context.max_input_tokens`
/// against the boot-time server-default `(provider, model)` AND every
/// registered agent's per-agent `(provider, model)` override (#919,
/// PR #1020 Tim review item 3 + Codex P2 #2 follow-up).
///
/// Mirrors `pre_flight_token_budget` in `runs/lifecycle.rs` — same validator
/// (`alms_core::config::validate_token_budget`), same `ValidationMode::from_env`
/// gating, same response envelope shape (`error_code` /
/// `INVALID_TOKEN_BUDGET_FOR_PROVIDER` / provider / model / both knobs /
/// effective total / cap).
///
/// The PATCH guard does TWO things:
///
/// 1. Server-default check. The candidate `max_input_tokens` is validated
///    against `state.llm_config.provider` / `state.llm_config.default_model`
///    (which already reflect any `[llm.providers.<provider>].model` override
///    applied at gateway boot — see `LlmConfig::From<alms_core::config::LlmConfig>`).
///    Same shape as the boot-time `validate_at_config_load`.
///
/// 2. Fleet evaluation (Codex P2 #2). For every registered agent, resolve
///    the effective `(provider, model)` using the same precedence
///    `resolve_agent_config` applies (`record.provider ?? server-default`,
///    `record.model ?? [llm.providers.<effective_provider>].model ?? server-default-model`)
///    and re-run the validator. Without this, a PATCH could land on the
///    server-default and immediately make some existing agents unrunnable —
///    the operator would only see the per-run 400 on the next `POST /runs`.
///    The fleet check refuses the entire PATCH if any agent overshoots and
///    names every offender so the operator sees the full picture in one
///    response.
///
/// Mock mode (`state.llm_config.mock = true`) bypasses both layers, same
/// as `validate_at_config_load` and the per-run pre-flight.
///
/// ## Return shape
///
/// - `Ok(None)` — patch does NOT touch `max_input_tokens` (or touches it
///   with a `0` sentinel rejected later by the structural handler), mock
///   mode is active, warn-mode is active, or the budget fits every cap.
///   The PATCH proceeds.
/// - `Ok(Some(envelope))` — strict-mode caught a known overshoot at
///   either layer. Caller emits the envelope as a `400 Bad Request`.
/// - `Err((status, envelope))` — fleet evaluation could not run because
///   `store.list_agents()` failed (Codex P2 #1020 follow-up). The PATCH
///   guard fails CLOSED rather than silently bypassing the per-agent
///   layer: an in-budget-against-server-default PATCH could otherwise
///   commit a higher `context.max_input_tokens` while SQLite is
///   temporarily failing, leaving every agent with a tighter resolved
///   cap unrunnable until the next `POST /runs`. Caller emits the
///   envelope verbatim with the supplied 503 status.
fn validate_patch_budget(
    state: &AppState,
    body: &PatchSettingsRequest,
) -> Result<Option<serde_json::Value>, (StatusCode, serde_json::Value)> {
    // Only fire if the patch actually carries a `max_input_tokens`. The
    // `0` sentinel is rejected by the structural handler with a 422 below;
    // we skip it here so the PATCH path's per-knob validation still owns
    // the "must be > 0" message.
    let Some(candidate_input) = body
        .context
        .as_ref()
        .and_then(|c| c.max_input_tokens)
        .filter(|v| *v > 0)
    else {
        return Ok(None);
    };

    // Mock mode: never enforce the cap. Mirrors `validate_at_config_load`.
    if state.llm_config.mock {
        return Ok(None);
    }

    // Live `max_tokens` is read under the AgentConfig RwLock — drop the
    // guard before the validator runs so the strict-mode early-return
    // doesn't risk lock contention with another handler.
    let max_tokens = state.agent_config.read().max_tokens;
    let mode = alms_core::config::ValidationMode::from_env();

    // ── Layer 1: server-default check ─────────────────────────────────
    let server_provider = state.llm_config.provider.as_str();
    let server_model = state.llm_config.default_model.as_str();
    if let Err(err) = alms_core::config::validate_token_budget(
        server_provider,
        server_model,
        candidate_input,
        max_tokens,
    ) {
        match mode {
            alms_core::config::ValidationMode::Strict => {
                warn!(
                    target: "alms.config",
                    provider = %err.provider,
                    model = %err.model,
                    max_input_tokens = err.max_input_tokens,
                    max_tokens = err.max_tokens,
                    effective_total = err.effective_total,
                    provider_cap = err.provider_cap,
                    "Rejecting PATCH /settings with INVALID_TOKEN_BUDGET_FOR_PROVIDER (#919)"
                );
                return Ok(Some(serde_json::json!({
                    "error_code": "INVALID_TOKEN_BUDGET_FOR_PROVIDER",
                    "message": err.message(),
                    "provider": err.provider,
                    "model": err.model,
                    "max_input_tokens": err.max_input_tokens,
                    "max_tokens": err.max_tokens,
                    "effective_total": err.effective_total,
                    "provider_cap": err.provider_cap,
                })));
            }
            alms_core::config::ValidationMode::Warn => {
                warn!(
                    target: "alms.config",
                    provider = %err.provider,
                    model = %err.model,
                    max_input_tokens = err.max_input_tokens,
                    max_tokens = err.max_tokens,
                    effective_total = err.effective_total,
                    provider_cap = err.provider_cap,
                    "{}",
                    err.message()
                );
            }
        }
    }

    // ── Layer 2: fleet evaluation (Codex P2 #2) ────────────────────────
    //
    // Iterate over every registered agent and re-run the validator
    // against the same effective `(provider, model)` that the runtime's
    // `runs::resolve_agent_config` would resolve to. The shared helper
    // `runs::resolve_effective_provider_and_model` is the single source
    // of truth for the resolution rules:
    //
    // 1. `record.provider` ?? server-default provider.
    // 2. Cross-namespace drop (#942): when the per-agent provider override
    //    changed the effective wire kind AND the per-agent model belongs
    //    to the OLD provider's namespace, drop the per-agent model so the
    //    fleet check validates against the SAME model the runtime will
    //    actually pick. Pre-fix this layer trusted `record.model`
    //    verbatim and silently green-lit PATCHes whose post-fallback
    //    `(provider, model)` would immediately fail
    //    `INVALID_TOKEN_BUDGET_FOR_PROVIDER` on the next `POST /runs`.
    // 3. `record.model` (after the namespace drop) ??
    //    `[llm.providers.<effective_provider>].model` ?? server-default model.
    //
    // When the helper returns `MissingModelAfterProviderSwitch`, the
    // runtime would reject the run with `400 MISSING_MODEL_AFTER_PROVIDER_SWITCH`
    // before any LLM call, so there is no effective model to validate
    // against. That row is silently skipped — the budget guard's
    // contract is "fail fast on KNOWN-overshoot configs", and a
    // missing-model row is unrunnable for orthogonal reasons.
    let Some(store) = state.session_manager.store() else {
        // No SQLite-backed store → no per-agent overrides to evaluate.
        // The server-default check above is the whole guard.
        return Ok(None);
    };
    // Codex P2 #1020 follow-up — fail CLOSED on `list_agents()` errors.
    //
    // The earlier shape soft-skipped here (warn + `return None`) so a
    // PATCH could return 200 and commit a higher
    // `context.max_input_tokens` whenever SQLite was temporarily failing.
    // In that state, agents whose resolved `(provider, model)` had a
    // tighter cap would become silently unrunnable and only fail later
    // at `POST /runs` with `INVALID_TOKEN_BUDGET_FOR_PROVIDER`, which
    // defeats the PATCH-time safety guarantee the fleet layer is here
    // to provide. Surface a 503 with a structured `AGENT_STORE_UNAVAILABLE`
    // code so the operator sees the failure synchronously and the
    // candidate-then-commit gate prevents any partial mutation: live
    // config stays untouched and `settings.json` is not rewritten.
    let agents = match store.list_agents() {
        Ok(a) => a,
        Err(e) => {
            warn!(
                target: "alms.config",
                error = %e,
                "PATCH /settings fleet budget evaluation could not load \
                 agents from SQLite; failing closed with 503 \
                 AGENT_STORE_UNAVAILABLE rather than skipping the \
                 per-agent layer (Codex P2 #1020 follow-up)"
            );
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                serde_json::json!({
                    "error_code": "AGENT_STORE_UNAVAILABLE",
                    "message": format!(
                        "could not validate PATCH /settings against per-agent \
                         token budgets: failed to list agents from the \
                         registry ({e}). The PATCH was REJECTED to avoid \
                         silently accepting a budget that some agents would \
                         overshoot — retry once the agent store is reachable."
                    ),
                }),
            ));
        }
    };

    let mut offenders: Vec<serde_json::Value> = Vec::new();
    let mut last_message: Option<String> = None;
    for record in agents {
        let (effective_provider, effective_model) =
            match crate::runs::resolve_effective_provider_and_model(
                record.provider.as_deref(),
                record.model.as_deref(),
                server_provider,
                server_model,
                &state.llm_config.providers,
            ) {
                Ok(pair) => pair,
                Err(crate::runs::ResolveEffectiveModelError::MissingModelAfterProviderSwitch {
                    new_provider,
                    prev_provider,
                }) => {
                    // Runtime would reject this agent's next run with
                    // `400 MISSING_MODEL_AFTER_PROVIDER_SWITCH` regardless
                    // of the candidate budget; skip the budget check so
                    // we don't surface a misleading error code from the
                    // fleet layer. The runtime's per-run pre-flight is
                    // the right place for this diagnostic.
                    warn!(
                        target: "alms.config",
                        agent = %record.name,
                        old_provider = %prev_provider,
                        new_provider = %new_provider,
                        "Skipping fleet budget check for agent — runtime would \
                         reject with MISSING_MODEL_AFTER_PROVIDER_SWITCH on next run"
                    );
                    continue;
                }
            };
        if let Err(err) = alms_core::config::validate_token_budget(
            &effective_provider,
            &effective_model,
            candidate_input,
            max_tokens,
        ) {
            warn!(
                target: "alms.config",
                agent = %record.name,
                provider = %err.provider,
                model = %err.model,
                max_input_tokens = err.max_input_tokens,
                max_tokens = err.max_tokens,
                effective_total = err.effective_total,
                provider_cap = err.provider_cap,
                "Per-agent budget overshoot under candidate PATCH /settings \
                 (Codex P2 #2 — agent would become unrunnable)"
            );
            last_message = Some(err.message());
            offenders.push(serde_json::json!({
                "name": record.name,
                "provider": err.provider,
                "model": err.model,
                "agent_cap": err.provider_cap,
                "would_be_total": err.effective_total,
            }));
        }
    }

    if offenders.is_empty() {
        return Ok(None);
    }

    match mode {
        alms_core::config::ValidationMode::Strict => {
            // Surface a single structured envelope listing every offending
            // agent — the operator sees the full fleet impact in one
            // response rather than discovering it agent-by-agent on the
            // next `POST /runs`. The top-level `provider` / `model` /
            // `provider_cap` / `effective_total` fields point at the FIRST
            // offender so existing clients that only read those fields
            // still get a usable error surface; the new `agents` array
            // carries the complete list.
            let first = &offenders[0];
            let message = format!(
                "configured token budget would exceed the provider cap for \
                 {n} registered agent(s) under this PATCH; lower \
                 context.max_input_tokens or update the affected agents \
                 (offenders: {names})",
                n = offenders.len(),
                names = offenders
                    .iter()
                    .filter_map(|o| o["name"].as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            // Prefer the per-agent `TokenBudgetError::message()` when only
            // one agent is affected — it carries the full diagnostic
            // (both knobs, total, cap, and the env-var bypass hint).
            let message = if offenders.len() == 1 {
                last_message.unwrap_or(message)
            } else {
                message
            };
            Ok(Some(serde_json::json!({
                "error_code": "INVALID_TOKEN_BUDGET_FOR_PROVIDER",
                "message": message,
                "provider": first["provider"].clone(),
                "model": first["model"].clone(),
                "max_input_tokens": candidate_input,
                "max_tokens": max_tokens,
                "effective_total": first["would_be_total"].clone(),
                "provider_cap": first["agent_cap"].clone(),
                "agents": offenders,
            })))
        }
        alms_core::config::ValidationMode::Warn => {
            // Warn mode: per-agent overshoots already logged in the loop
            // above. Let the PATCH proceed — same opt-out semantics as
            // the server-default warn branch and the per-run pre-flight.
            Ok(None)
        }
    }
}

fn reject_security_knob(body: &serde_json::Value) -> Option<String> {
    let obj = body.as_object()?;
    if !obj.contains_key("security") {
        return None;
    }
    Some(
        "SECURITY_KNOB_NOT_PATCHABLE: settings.security is config-file-only \
         and cannot be modified via PATCH /settings. Edit `[security]` in \
         alms.toml (e.g. `[security]\\nallow_full_os_access = [\"agent-name\"]`) \
         and restart the gateway. See issue #947 for the threat model — \
         PATCH mutability would let a compromised auth token silently \
         widen the agent sandbox."
            .to_string(),
    )
}

// ── Persistence helpers ───────────────────────────────────────────────

/// Persisted context overrides — only fields the user explicitly changed
/// via PATCH /settings are `Some`. Fields that are `None` fall through to
/// code defaults / TOML / env-var overrides.
///
/// Backward-compatible: old `settings.json` files predating #869 contain
/// `recent_window` / `summary_interval` keys. Serde silently ignores
/// unknown keys here, so those values deserialize away cleanly. The next
/// PATCH writes the new shape (`compact_trigger_pct` / `compact_retain_pct`).
/// `strategy = "sliding-summary"` is rewritten to `"compact"` on apply
/// to mirror the deserialise-time rewrite in `ContextConfig`.
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PersistedContextOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<usize>,
    /// #869: trigger compaction when assembled history exceeds this fraction
    /// of `max_input_tokens`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compact_trigger_pct: Option<f32>,
    /// #869: after compaction, retain at most this fraction of
    /// `max_input_tokens` worth of recent verbatim messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compact_retain_pct: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_model: Option<String>,
    /// Persisted summary provider override (#866).
    /// `None` means "fall through to TOML / env / code default" — typically
    /// "inherit agent provider". `Some(provider)` re-targets the summary
    /// task at that provider on every restart.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_summary_mode: Option<alms_core::config::RunSummaryMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_summary_budget: Option<usize>,
}

impl PersistedContextOverrides {
    /// Merge these overrides into a `ContextConfig`, field by field.
    /// Only fields that are `Some` overwrite the target.
    pub fn apply_to(&self, ctx: &mut alms_core::config::ContextConfig) {
        if let Some(ref v) = self.strategy {
            // #869: rewrite the legacy alias on apply so a `settings.json`
            // written before #869 lands resolves to the new canonical name
            // without round-tripping through `ContextConfig::Deserialize`.
            ctx.strategy = if v == "sliding-summary" {
                "compact".to_string()
            } else {
                v.clone()
            };
        }
        if let Some(v) = self.max_input_tokens {
            ctx.max_input_tokens = v;
        }
        if let Some(v) = self.compact_trigger_pct {
            ctx.compact_trigger_pct = v;
        }
        if let Some(v) = self.compact_retain_pct {
            ctx.compact_retain_pct = v;
        }
        if self.summary_model.is_some() {
            ctx.summary_model = self.summary_model.clone();
        }
        if self.summary_provider.is_some() {
            ctx.summary_provider = self.summary_provider.clone();
        }
        if let Some(ref v) = self.run_summary_mode {
            ctx.run_summary_mode = v.clone();
        }
        if let Some(v) = self.run_summary_budget {
            ctx.run_summary_budget = v;
        }
    }
}

/// Persisted session overrides — only fields the user explicitly changed.
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PersistedSessionOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_messages: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_timeout_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_archive: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_ttl_secs: Option<u64>,
}

impl PersistedSessionOverrides {
    /// Merge these overrides into a `SessionConfig`, field by field.
    pub fn apply_to(&self, sess: &mut alms_session::SessionConfig) {
        if let Some(v) = self.max_messages {
            sess.max_messages = v;
        }
        if let Some(v) = self.max_context_tokens {
            sess.max_context_tokens = v;
        }
        if let Some(v) = self.idle_timeout_secs {
            sess.idle_timeout_secs = v;
        }
        if let Some(v) = self.auto_archive {
            sess.auto_archive = v;
        }
        if let Some(v) = self.archive_ttl_secs {
            sess.archive_ttl_secs = v;
        }
    }
}

/// Persisted tools overrides — only fields the user explicitly changed.
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PersistedToolsOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_bytes: Option<usize>,
}

impl PersistedToolsOverrides {
    /// Merge these overrides into a `ToolsConfig`, field by field.
    pub fn apply_to(&self, tools: &mut alms_core::config::ToolsConfig) {
        if let Some(ref v) = self.shell_policy {
            tools.shell_policy = v.clone();
        }
        if let Some(ref v) = self.sandbox_root {
            tools.sandbox_root = v.clone();
        }
        if let Some(v) = self.timeout_secs {
            tools.timeout_secs = v;
        }
        if let Some(v) = self.max_output_bytes {
            tools.max_output_bytes = v;
        }
    }
}

/// Persisted Anthropic LLM overrides (#809).
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PersistedLlmAnthropicOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_enabled: Option<bool>,
}

/// Persisted OpenAI-compat LLM overrides (#809).
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PersistedLlmOpenaiOverrides {
    /// Wire-string representation (`"low"` / `"medium"` / `"high"` /
    /// `"minimal"`). We store the string rather than the enum so the
    /// JSON on disk round-trips through JSON-anything without requiring
    /// the reader to know about `ReasoningEffort`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

/// Persisted Gemini LLM overrides (#809).
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PersistedLlmGeminiOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_ttl_seconds: Option<u64>,
}

/// Persisted LLM overrides umbrella (#809).
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PersistedLlmOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic: Option<PersistedLlmAnthropicOverrides>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openai: Option<PersistedLlmOpenaiOverrides>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gemini: Option<PersistedLlmGeminiOverrides>,
}

impl PersistedLlmOverrides {
    /// Merge these overrides into the live `AgentConfig` on startup so
    /// the PATCH /settings mutations survive a gateway restart.
    ///
    /// Semantics mirror the other `PersistedXOverrides::apply_to` impls:
    /// fields that are `Some` overwrite the target; fields that are
    /// `None` leave the target untouched. The one subtlety is
    /// `openai.reasoning_effort`: because the target field is itself an
    /// `Option<ReasoningEffort>`, we need a way to represent "cleared"
    /// on disk. We use the empty string (`""`) for that — same trick
    /// the rest of the API uses to clear a nullable value, and matching
    /// the PATCH-wire shape of `PatchLlmOpenai::reasoning_effort`.
    pub fn apply_to(&self, cfg: &mut alms_runtime::AgentConfig) {
        if let Some(ant) = &self.anthropic {
            if let Some(v) = ant.thinking_budget_tokens {
                cfg.anthropic_thinking_budget = v;
            }
            if let Some(v) = ant.prompt_cache_enabled {
                cfg.anthropic_prompt_cache_enabled = v;
            }
        }
        if let Some(oai) = &self.openai
            && let Some(ref v) = oai.reasoning_effort
        {
            if v.is_empty() {
                cfg.openai_reasoning_effort = None;
            } else if let Ok(effort) = v.parse::<alms_core::config::ReasoningEffort>() {
                cfg.openai_reasoning_effort = Some(effort);
            } else {
                tracing::warn!(
                    value = %v,
                    "Ignoring unknown persisted openai.reasoning_effort value"
                );
            }
        }
        if let Some(gem) = &self.gemini {
            if let Some(v) = gem.thinking_budget {
                cfg.gemini_thinking_budget = Some(v);
            }
            if let Some(v) = gem.cache_enabled {
                cfg.gemini_cache_enabled = v;
            }
            if let Some(v) = gem.cache_ttl_seconds {
                cfg.gemini_cache_ttl_seconds = v;
            }
        }
    }
}

/// On-disk representation of the mutable server-level settings.
///
/// Written to `{data_dir}/settings.json` after every PATCH /settings and
/// loaded on startup to restore the previous configuration.
///
/// Only fields explicitly set by the user via PATCH /settings are persisted.
/// On load, these overrides are merged field-by-field into the resolved
/// config (code defaults + TOML + env vars), ensuring that:
/// - Code default changes are picked up for non-overridden fields
/// - Env var overrides (e.g. `ALMS_RUN_SUMMARY_MODE`) still take precedence
///   when they exist
///
/// Backward-compatible: old `settings.json` files with the full
/// `ContextConfig` / `SessionConfig` / `ToolsConfig` structs will
/// deserialize correctly because `serde(default)` fills in `None` for
/// missing fields, and extra fields in the JSON are silently ignored.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct PersistedSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<PersistedContextOverrides>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<PersistedSessionOverrides>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<PersistedToolsOverrides>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm: Option<PersistedLlmOverrides>,
}

/// Return the canonical path for the persisted settings file.
pub fn settings_path(data_dir: &Path) -> PathBuf {
    data_dir.join("settings.json")
}

/// Write the current user-overridden settings to `{data_dir}/settings.json`.
///
/// Only persists the fields that PATCH /settings can modify — never writes
/// fields like `run_summary_mode` or `run_summary_budget` that are not
/// exposed in the PATCH API, so that code-default and env-var changes to
/// those fields are always respected on restart.
fn persist_settings(state: &AppState) {
    let agent = state.agent_config.read();
    let ctx = &agent.context_config;
    let sess = state.session_config.read();
    let tools = state.tools_config.read();

    let persisted = PersistedSettings {
        // Only persist the fields that PATCH /settings exposes for context.
        context: Some(PersistedContextOverrides {
            strategy: Some(ctx.strategy.clone()),
            max_input_tokens: Some(ctx.max_input_tokens),
            // #869: persist the threshold-based knobs.
            compact_trigger_pct: Some(ctx.compact_trigger_pct),
            compact_retain_pct: Some(ctx.compact_retain_pct),
            summary_model: ctx.summary_model.clone(),
            summary_provider: ctx.summary_provider.clone(),
            // run_summary_mode and run_summary_budget are NOT exposed in
            // PATCH /settings, so we never persist them. This ensures that
            // code-default changes and env-var overrides are always respected.
            run_summary_mode: None,
            run_summary_budget: None,
        }),
        session: Some(PersistedSessionOverrides {
            max_messages: Some(sess.max_messages),
            max_context_tokens: Some(sess.max_context_tokens),
            idle_timeout_secs: Some(sess.idle_timeout_secs),
            auto_archive: Some(sess.auto_archive),
            archive_ttl_secs: Some(sess.archive_ttl_secs),
        }),
        tools: Some(PersistedToolsOverrides {
            shell_policy: Some(tools.shell_policy.clone()),
            sandbox_root: Some(tools.sandbox_root.clone()),
            timeout_secs: Some(tools.timeout_secs),
            max_output_bytes: Some(tools.max_output_bytes),
        }),
        // LLM provider-family defaults (#809). We persist the full
        // snapshot of each knob — they all live on `AgentConfig` as
        // plain values with no "unset" sentinel at the struct level, so
        // there's nothing to distinguish "user-set via PATCH" from
        // "loaded from TOML / env on boot" anyway. On restart the
        // persisted value wins over TOML / env (same precedence as the
        // context / session / tools blocks above).
        llm: Some(PersistedLlmOverrides {
            anthropic: Some(PersistedLlmAnthropicOverrides {
                thinking_budget_tokens: Some(agent.anthropic_thinking_budget),
                prompt_cache_enabled: Some(agent.anthropic_prompt_cache_enabled),
            }),
            openai: Some(PersistedLlmOpenaiOverrides {
                // Persist the wire string, or `""` to represent
                // "explicitly cleared". `None` at this layer would mean
                // "leave the TOML / env value alone on reload" which is
                // NOT what we want after a PATCH clear — we want the
                // clear to win on every subsequent boot.
                reasoning_effort: Some(
                    agent
                        .openai_reasoning_effort
                        .as_ref()
                        .map(|e| e.as_wire_str().to_string())
                        .unwrap_or_default(),
                ),
            }),
            gemini: Some(PersistedLlmGeminiOverrides {
                thinking_budget: agent.gemini_thinking_budget,
                cache_enabled: Some(agent.gemini_cache_enabled),
                cache_ttl_seconds: Some(agent.gemini_cache_ttl_seconds),
            }),
        }),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulate the pre-#869 settings.json format (full ContextConfig with
    /// `recent_window` / `summary_interval`). Verify backward compatibility:
    /// the legacy fields are dropped silently from the struct (serde ignores
    /// unknown keys at this layer), and the new fields fall back to `None`.
    #[test]
    fn backward_compat_old_settings_json_with_full_context_config() {
        let old_json = r#"{
            "context": {
                "strategy": "truncate",
                "max_input_tokens": 128000,
                "run_summary_mode": "off",
                "run_summary_budget": 2000,
                "recent_window": 20,
                "summary_interval": 30,
                "summary_model": null
            }
        }"#;
        let persisted: PersistedSettings = serde_json::from_str(old_json).unwrap();
        let ctx = persisted.context.unwrap();
        assert_eq!(ctx.strategy, Some("truncate".into()));
        assert_eq!(ctx.max_input_tokens, Some(128_000));
        assert_eq!(
            ctx.run_summary_mode,
            Some(alms_core::config::RunSummaryMode::Off)
        );
        assert_eq!(ctx.run_summary_budget, Some(2000));
        // #869: legacy keys are silently dropped; the new keys default to None.
        assert_eq!(ctx.compact_trigger_pct, None);
        assert_eq!(ctx.compact_retain_pct, None);
    }

    /// New settings.json format carries the threshold-based knobs and
    /// omits run_summary_mode / run_summary_budget.
    #[test]
    fn new_format_omits_non_patchable_fields() {
        let new_json = r#"{
            "context": {
                "strategy": "truncate",
                "max_input_tokens": 128000,
                "compact_trigger_pct": 0.85,
                "compact_retain_pct": 0.35
            }
        }"#;
        let persisted: PersistedSettings = serde_json::from_str(new_json).unwrap();
        let ctx = persisted.context.unwrap();
        assert_eq!(ctx.strategy, Some("truncate".into()));
        assert_eq!(ctx.max_input_tokens, Some(128_000));
        assert_eq!(ctx.compact_trigger_pct, Some(0.85));
        assert_eq!(ctx.compact_retain_pct, Some(0.35));
        // Non-patchable fields should be None
        assert_eq!(ctx.run_summary_mode, None);
        assert_eq!(ctx.run_summary_budget, None);
    }

    /// Context overrides with `None` fields should not overwrite the target.
    /// #869: also pin the `"sliding-summary"` → `"compact"` rewrite on apply.
    #[test]
    fn context_overrides_apply_only_some_fields() {
        let mut ctx = alms_core::config::ContextConfig::default();
        // Default run_summary_mode is Llm, run_summary_budget is 2000
        assert_eq!(ctx.run_summary_mode, alms_core::config::RunSummaryMode::Llm);
        assert_eq!(ctx.run_summary_budget, 2000);

        let overrides = PersistedContextOverrides {
            strategy: Some("sliding-summary".into()),
            max_input_tokens: Some(64_000),
            // Leave everything else as None — should not overwrite
            ..Default::default()
        };
        overrides.apply_to(&mut ctx);

        // #869: alias rewritten to canonical name on apply.
        assert_eq!(ctx.strategy, "compact");
        assert_eq!(ctx.max_input_tokens, 64_000);
        // These should be UNCHANGED from defaults
        assert_eq!(
            ctx.run_summary_mode,
            alms_core::config::RunSummaryMode::Llm,
            "run_summary_mode should not be overwritten when override is None"
        );
        assert_eq!(
            ctx.run_summary_budget, 2000,
            "run_summary_budget should not be overwritten when override is None"
        );
        // #869: defaults preserved when overrides leave the new knobs None.
        assert_eq!(ctx.compact_trigger_pct, 0.80);
        assert_eq!(ctx.compact_retain_pct, 0.40);
    }

    /// #866: `summary_provider` applies to ContextConfig when set in
    /// overrides and is preserved when overrides leave it None.
    #[test]
    fn context_overrides_summary_provider_round_trip() {
        let mut ctx = alms_core::config::ContextConfig::default();
        assert_eq!(
            ctx.summary_provider, None,
            "default ContextConfig has summary_provider = None"
        );

        // Setting it on the override applies it.
        let overrides = PersistedContextOverrides {
            summary_provider: Some("openrouter".into()),
            ..Default::default()
        };
        overrides.apply_to(&mut ctx);
        assert_eq!(
            ctx.summary_provider,
            Some("openrouter".into()),
            "summary_provider override should land on ContextConfig"
        );

        // Subsequent override with None on summary_provider must not clear
        // the value (the persisted-overrides semantics: only Some fields
        // overwrite; mirrors `summary_model`).
        let no_op = PersistedContextOverrides {
            strategy: Some("truncate".into()),
            ..Default::default()
        };
        no_op.apply_to(&mut ctx);
        assert_eq!(
            ctx.summary_provider,
            Some("openrouter".into()),
            "PersistedContextOverrides with None summary_provider must not clear the live value"
        );
    }

    /// Session overrides with partial fields should only overwrite `Some` fields.
    #[test]
    fn session_overrides_apply_only_some_fields() {
        let mut sess = alms_session::SessionConfig::default();
        let original_idle = sess.idle_timeout_secs;

        let overrides = PersistedSessionOverrides {
            max_messages: Some(500),
            // Leave everything else as None
            ..Default::default()
        };
        overrides.apply_to(&mut sess);

        assert_eq!(sess.max_messages, 500);
        assert_eq!(
            sess.idle_timeout_secs, original_idle,
            "idle_timeout should be unchanged"
        );
    }

    /// Tools overrides with partial fields should only overwrite `Some` fields.
    #[test]
    fn tools_overrides_apply_only_some_fields() {
        let mut tools = alms_core::config::ToolsConfig::default();
        let original_timeout = tools.timeout_secs;

        let overrides = PersistedToolsOverrides {
            shell_policy: Some("unrestricted".into()),
            ..Default::default()
        };
        overrides.apply_to(&mut tools);

        assert_eq!(tools.shell_policy, "unrestricted");
        assert_eq!(
            tools.timeout_secs, original_timeout,
            "timeout should be unchanged"
        );
    }

    /// Serialization of new format should NOT include run_summary_mode or
    /// run_summary_budget when they are None.
    #[test]
    fn serialized_new_format_excludes_non_patchable_fields() {
        let persisted = PersistedSettings {
            context: Some(PersistedContextOverrides {
                strategy: Some("truncate".into()),
                max_input_tokens: Some(128_000),
                // #869: threshold knobs replace recent_window / summary_interval.
                compact_trigger_pct: Some(0.80),
                compact_retain_pct: Some(0.40),
                summary_model: None,
                summary_provider: None,
                run_summary_mode: None,
                run_summary_budget: None,
            }),
            session: None,
            tools: None,
            llm: None,
        };
        let json = serde_json::to_string_pretty(&persisted).unwrap();
        assert!(
            !json.contains("run_summary_mode"),
            "Serialized JSON should not contain run_summary_mode"
        );
        assert!(
            !json.contains("run_summary_budget"),
            "Serialized JSON should not contain run_summary_budget"
        );
        // #869: pin that the legacy keys never appear on the persistence
        // wire — the persist function dropped them.
        assert!(!json.contains("recent_window"), "JSON: {json}");
        assert!(!json.contains("summary_interval"), "JSON: {json}");
    }

    /// Round-trip: serialize then deserialize preserves values.
    #[test]
    fn round_trip_serialization() {
        let original = PersistedSettings {
            context: Some(PersistedContextOverrides {
                // #869: use "compact" + threshold knobs for the round-trip
                // shape. The legacy "sliding-summary" + recent_window /
                // summary_interval combo is covered by the
                // `backward_compat_*` test above.
                strategy: Some("compact".into()),
                max_input_tokens: Some(64_000),
                compact_trigger_pct: Some(0.85),
                compact_retain_pct: Some(0.35),
                summary_model: Some("gpt-4o-mini".into()),
                summary_provider: None,
                run_summary_mode: None,
                run_summary_budget: None,
            }),
            session: Some(PersistedSessionOverrides {
                max_messages: Some(5000),
                max_context_tokens: Some(200_000),
                idle_timeout_secs: Some(3600),
                auto_archive: Some(false),
                archive_ttl_secs: Some(86400),
            }),
            tools: Some(PersistedToolsOverrides {
                shell_policy: Some("unrestricted".into()),
                sandbox_root: Some("/tmp".into()),
                timeout_secs: Some(60),
                max_output_bytes: Some(1_000_000),
            }),
            llm: None,
        };
        let json = serde_json::to_string_pretty(&original).unwrap();
        let deserialized: PersistedSettings = serde_json::from_str(&json).unwrap();

        let ctx = deserialized.context.unwrap();
        assert_eq!(ctx.strategy, Some("compact".into()));
        assert_eq!(ctx.max_input_tokens, Some(64_000));
        assert_eq!(ctx.compact_trigger_pct, Some(0.85));
        assert_eq!(ctx.compact_retain_pct, Some(0.35));
        assert_eq!(ctx.summary_model, Some("gpt-4o-mini".into()));
        assert_eq!(ctx.run_summary_mode, None);
        assert_eq!(ctx.run_summary_budget, None);

        let sess = deserialized.session.unwrap();
        assert_eq!(sess.max_messages, Some(5000));

        let tools = deserialized.tools.unwrap();
        assert_eq!(tools.shell_policy, Some("unrestricted".into()));
    }

    /// Empty JSON should deserialize to default PersistedSettings (all None).
    #[test]
    fn empty_json_deserializes_to_defaults() {
        let persisted: PersistedSettings = serde_json::from_str("{}").unwrap();
        assert!(persisted.context.is_none());
        assert!(persisted.session.is_none());
        assert!(persisted.tools.is_none());
        assert!(persisted.llm.is_none());
    }

    /// Old format with full SessionConfig should still deserialize.
    #[test]
    fn backward_compat_old_session_config() {
        let old_json = r#"{
            "session": {
                "idle_timeout_secs": 86400,
                "auto_archive": true,
                "archive_ttl_secs": 2592000,
                "max_messages": 10000,
                "max_context_tokens": 128000
            }
        }"#;
        let persisted: PersistedSettings = serde_json::from_str(old_json).unwrap();
        let sess = persisted.session.unwrap();
        assert_eq!(sess.max_messages, Some(10_000));
        assert_eq!(sess.idle_timeout_secs, Some(86_400));
        assert_eq!(sess.auto_archive, Some(true));
    }

    // ==================================================================
    // LLM provider-family surface (#809)
    // ==================================================================

    /// Old settings.json files predating #809 must still deserialize
    /// cleanly — the new `llm` field must be optional.
    #[test]
    fn backward_compat_settings_json_without_llm() {
        let old_json = r#"{
            "context": { "strategy": "truncate", "max_input_tokens": 128000 },
            "tools": { "shell_policy": "sandboxed" }
        }"#;
        let persisted: PersistedSettings = serde_json::from_str(old_json).unwrap();
        assert!(persisted.llm.is_none(), "pre-#809 JSON should lack `llm`");
        // And the other sections should still deserialize.
        assert_eq!(persisted.context.unwrap().strategy, Some("truncate".into()));
    }

    /// A `PersistedSettings` with `llm: None` must NOT emit the key in
    /// the serialized JSON — same `skip_serializing_if` pattern as the
    /// other top-level sections.
    #[test]
    fn serialized_omits_llm_when_none() {
        let persisted = PersistedSettings {
            context: None,
            session: None,
            tools: None,
            llm: None,
        };
        let json = serde_json::to_string_pretty(&persisted).unwrap();
        assert!(
            !json.contains("\"llm\""),
            "JSON should not contain llm key when it is None: {json}"
        );
    }

    /// `PersistedLlmOverrides::apply_to` must overwrite each field the
    /// overrides populate and leave everything else untouched. Mirrors
    /// the shape of the context/session/tools `apply_to` tests above.
    #[test]
    fn llm_overrides_apply_to_mutates_agent_config() {
        let mut cfg = alms_runtime::AgentConfig::default();
        // Baseline: caching on, no reasoning effort, no Gemini thinking.
        assert!(cfg.anthropic_prompt_cache_enabled);
        assert_eq!(cfg.anthropic_thinking_budget, 0);
        assert!(cfg.openai_reasoning_effort.is_none());
        assert!(cfg.gemini_thinking_budget.is_none());
        assert!(cfg.gemini_cache_enabled);
        assert_eq!(cfg.gemini_cache_ttl_seconds, 300);

        let overrides = PersistedLlmOverrides {
            anthropic: Some(PersistedLlmAnthropicOverrides {
                thinking_budget_tokens: Some(8192),
                prompt_cache_enabled: Some(false),
            }),
            openai: Some(PersistedLlmOpenaiOverrides {
                reasoning_effort: Some("high".into()),
            }),
            gemini: Some(PersistedLlmGeminiOverrides {
                thinking_budget: Some(4096),
                cache_enabled: Some(false),
                cache_ttl_seconds: Some(600),
            }),
        };
        overrides.apply_to(&mut cfg);

        assert_eq!(cfg.anthropic_thinking_budget, 8192);
        assert!(!cfg.anthropic_prompt_cache_enabled);
        assert_eq!(
            cfg.openai_reasoning_effort,
            Some(alms_core::config::ReasoningEffort::High)
        );
        assert_eq!(cfg.gemini_thinking_budget, Some(4096));
        assert!(!cfg.gemini_cache_enabled);
        assert_eq!(cfg.gemini_cache_ttl_seconds, 600);
    }

    /// Empty-string `reasoning_effort` on the persisted layer must clear
    /// the live `AgentConfig.openai_reasoning_effort` back to `None`.
    /// This is the only "clear sentinel" the /settings surface needs
    /// for LLM knobs — the other two tri-state fields
    /// (`gemini_thinking_budget`, `anthropic_thinking_budget` is plain
    /// u32) don't need one because `Some(0)` is a legitimate disable
    /// value for those.
    #[test]
    fn llm_overrides_empty_string_reasoning_effort_clears() {
        let mut cfg = alms_runtime::AgentConfig {
            openai_reasoning_effort: Some(alms_core::config::ReasoningEffort::Medium),
            ..Default::default()
        };
        let overrides = PersistedLlmOverrides {
            anthropic: None,
            openai: Some(PersistedLlmOpenaiOverrides {
                reasoning_effort: Some(String::new()),
            }),
            gemini: None,
        };
        overrides.apply_to(&mut cfg);
        assert!(
            cfg.openai_reasoning_effort.is_none(),
            "empty-string reasoning_effort must clear to None"
        );
    }

    /// Unknown `reasoning_effort` string (typo in persisted JSON) must
    /// be ignored with a warning — never panic, never crash the apply.
    #[test]
    fn llm_overrides_unknown_reasoning_effort_ignored() {
        let mut cfg = alms_runtime::AgentConfig {
            openai_reasoning_effort: Some(alms_core::config::ReasoningEffort::Medium),
            ..Default::default()
        };
        let overrides = PersistedLlmOverrides {
            anthropic: None,
            openai: Some(PersistedLlmOpenaiOverrides {
                reasoning_effort: Some("turbo".into()),
            }),
            gemini: None,
        };
        overrides.apply_to(&mut cfg);
        // Target is untouched on unknown input.
        assert_eq!(
            cfg.openai_reasoning_effort,
            Some(alms_core::config::ReasoningEffort::Medium)
        );
    }

    /// The `PatchLlm*` wire structs must all default cleanly — used by
    /// axum's JSON body extractor to accept callers that omit the new
    /// surface entirely.
    #[test]
    fn patch_llm_body_all_none_by_default() {
        let body: PatchSettingsRequest = serde_json::from_str("{}").unwrap();
        assert!(body.llm.is_none());
    }

    /// A PATCH body with `llm: { anthropic: { thinking_budget_tokens: 16384 } }`
    /// must deserialize into the expected nested shape.
    #[test]
    fn patch_llm_body_parses_nested_shape() {
        let json = r#"{
            "llm": {
                "anthropic": { "thinking_budget_tokens": 16384, "prompt_cache_enabled": false },
                "openai": { "reasoning_effort": "high" },
                "gemini": { "thinking_budget": 4096, "cache_enabled": false, "cache_ttl_seconds": 600 }
            }
        }"#;
        let body: PatchSettingsRequest = serde_json::from_str(json).unwrap();
        let llm = body.llm.expect("llm should be present");
        let ant = llm.anthropic.expect("anthropic should be present");
        assert_eq!(ant.thinking_budget_tokens, Some(16384));
        assert_eq!(ant.prompt_cache_enabled, Some(false));
        let oai = llm.openai.expect("openai should be present");
        assert_eq!(oai.reasoning_effort.as_deref(), Some("high"));
        let gem = llm.gemini.expect("gemini should be present");
        assert_eq!(gem.thinking_budget, Some(4096));
        assert_eq!(gem.cache_enabled, Some(false));
        assert_eq!(gem.cache_ttl_seconds, Some(600));
    }

    /// Round-trip a full LLM-populated PersistedSettings through JSON
    /// and back, verifying every field survives.
    #[test]
    fn llm_overrides_round_trip() {
        let original = PersistedSettings {
            context: None,
            session: None,
            tools: None,
            llm: Some(PersistedLlmOverrides {
                anthropic: Some(PersistedLlmAnthropicOverrides {
                    thinking_budget_tokens: Some(8192),
                    prompt_cache_enabled: Some(true),
                }),
                openai: Some(PersistedLlmOpenaiOverrides {
                    reasoning_effort: Some("low".into()),
                }),
                gemini: Some(PersistedLlmGeminiOverrides {
                    thinking_budget: Some(0),
                    cache_enabled: Some(false),
                    cache_ttl_seconds: Some(1800),
                }),
            }),
        };
        let json = serde_json::to_string_pretty(&original).unwrap();
        let round_tripped: PersistedSettings = serde_json::from_str(&json).unwrap();
        let llm = round_tripped.llm.unwrap();
        let ant = llm.anthropic.unwrap();
        assert_eq!(ant.thinking_budget_tokens, Some(8192));
        assert_eq!(ant.prompt_cache_enabled, Some(true));
        let oai = llm.openai.unwrap();
        assert_eq!(oai.reasoning_effort, Some("low".into()));
        let gem = llm.gemini.unwrap();
        assert_eq!(gem.thinking_budget, Some(0));
        assert_eq!(gem.cache_enabled, Some(false));
        assert_eq!(gem.cache_ttl_seconds, Some(1800));
    }

    // ==================================================================
    // PATCH /settings -> live `AgentConfig` mutation (#809 follow-up,
    // Tim review item 1).
    //
    // The persistence-load and per-run-merge tests above prove the
    // serde / business-logic layers, but the actual claim of #809 is
    // that `patch_settings()` writes through to the live
    // `Arc<RwLock<AgentConfig>>` shared with the HTTP run path. These
    // end-to-end tests construct a real `AppState`, call
    // `patch_settings()` as the axum handler, and read
    // `state.agent_config.read()` afterwards to assert the mutation
    // landed. They are the only tests that would catch a future
    // refactor introducing a stale-clone in the handler.
    // ==================================================================

    /// Construct a minimal `AppState` with no SQLite, no LLM, fresh
    /// channels — same shape as `runs::integration_tests::test_app_state`,
    /// inlined here because that helper is private to its module.
    fn settings_test_app_state() -> crate::server::AppState {
        let gateway_config = crate::gateway::GatewayConfig::default();
        let gateway = crate::gateway::Gateway::new(gateway_config).unwrap();
        let scheduler = std::sync::Arc::new(alms_runtime::Scheduler::new());
        let shutdown_token = tokio_util::sync::CancellationToken::new();
        let (completion_tx, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
        let (trigger_tx, _trigger_rx) = tokio::sync::mpsc::unbounded_channel();
        let (dm_event_tx, _dm_event_rx) = tokio::sync::mpsc::unbounded_channel();
        crate::server::AppState::new(
            gateway,
            scheduler,
            shutdown_token,
            completion_tx,
            trigger_tx,
            dm_event_tx,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn patch_llm_writes_through_to_live_agent_config() {
        let state = settings_test_app_state();

        // Seed a baseline different from the values we will PATCH so the
        // assertions below distinguish the mutation from the default.
        {
            let mut cfg = state.agent_config.write();
            cfg.anthropic_thinking_budget = 0;
            cfg.anthropic_prompt_cache_enabled = true;
            cfg.openai_reasoning_effort = None;
            cfg.gemini_thinking_budget = None;
            cfg.gemini_cache_enabled = true;
            cfg.gemini_cache_ttl_seconds = 300;
        }

        // PATCH every LLM knob to a non-default value.
        let body = PatchSettingsRequest {
            llm: Some(PatchLlm {
                anthropic: Some(PatchLlmAnthropic {
                    thinking_budget_tokens: Some(8192),
                    prompt_cache_enabled: Some(false),
                }),
                openai: Some(PatchLlmOpenai {
                    reasoning_effort: Some("high".into()),
                }),
                gemini: Some(PatchLlmGemini {
                    thinking_budget: Some(4096),
                    cache_enabled: Some(false),
                    cache_ttl_seconds: Some(1800),
                }),
            }),
            ..Default::default()
        };
        let _resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await;

        // Read the live config back through the shared lock.
        let cfg = state.agent_config.read();
        assert_eq!(
            cfg.anthropic_thinking_budget, 8192,
            "PATCH must write through to live anthropic_thinking_budget"
        );
        assert!(
            !cfg.anthropic_prompt_cache_enabled,
            "PATCH must write through to live anthropic_prompt_cache_enabled"
        );
        assert_eq!(
            cfg.openai_reasoning_effort,
            Some(alms_core::config::ReasoningEffort::High),
            "PATCH must write through to live openai_reasoning_effort"
        );
        assert_eq!(
            cfg.gemini_thinking_budget,
            Some(4096),
            "PATCH must write through to live gemini_thinking_budget"
        );
        assert!(
            !cfg.gemini_cache_enabled,
            "PATCH must write through to live gemini_cache_enabled"
        );
        assert_eq!(
            cfg.gemini_cache_ttl_seconds, 1800,
            "PATCH must write through to live gemini_cache_ttl_seconds"
        );
    }

    /// Empty-string `reasoning_effort` on the PATCH wire must clear the
    /// live `openai_reasoning_effort` back to `None` — same clear-sentinel
    /// behaviour as the persistence-load path.
    #[tokio::test]
    async fn patch_openai_empty_string_clears_live_reasoning_effort() {
        let state = settings_test_app_state();
        // Seed: high effort.
        state.agent_config.write().openai_reasoning_effort =
            Some(alms_core::config::ReasoningEffort::High);

        let body = PatchSettingsRequest {
            llm: Some(PatchLlm {
                openai: Some(PatchLlmOpenai {
                    reasoning_effort: Some(String::new()),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let _resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await;

        assert!(
            state.agent_config.read().openai_reasoning_effort.is_none(),
            "empty-string reasoning_effort must clear the live config to None"
        );
    }

    /// `gemini.thinking_budget = Some(0)` on the PATCH wire must land as
    /// `Some(0)` on the live config — it is "disable extended thinking",
    /// not a clear sentinel. This is the asymmetry from the OpenAI knob
    /// that the api.md table calls out.
    #[tokio::test]
    async fn patch_gemini_thinking_budget_zero_is_disable_not_clear() {
        let state = settings_test_app_state();
        state.agent_config.write().gemini_thinking_budget = Some(4096);

        let body = PatchSettingsRequest {
            llm: Some(PatchLlm {
                gemini: Some(PatchLlmGemini {
                    thinking_budget: Some(0),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let _resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await;

        assert_eq!(
            state.agent_config.read().gemini_thinking_budget,
            Some(0),
            "gemini.thinking_budget = Some(0) is disable, not clear — \
             must round-trip through the handler verbatim"
        );
    }

    // ==================================================================
    // Rejected PATCH must be side-effect-free at the persistence layer
    // (#810 follow-up).
    //
    // Before the fix, `patch_settings` called `persist_settings(&state)`
    // unconditionally — even when validation errors were collected and
    // the handler was about to return 422. That meant any rejected
    // request still rewrote `settings.json` from the current live
    // snapshot, baking in pre-existing PATCH-applied values (and any
    // sub-fields that landed before the rejected one) such that they
    // would override `alms.toml` / env on the next daemon boot. This is
    // the same shape as #814 (rejected fs_write leaving directories
    // behind): the operationally damaging case is a *failed* request
    // changing post-restart behaviour.
    // ==================================================================

    /// Rejected PATCH must NOT write `settings.json`. The handler returns
    /// 422 and the persistence file does not exist on disk afterwards.
    /// This is the load-bearing assertion for the #810 follow-up.
    #[tokio::test]
    async fn rejected_llm_patch_does_not_persist_settings_json() {
        use axum::response::IntoResponse;

        let mut state = settings_test_app_state();
        // Redirect persistence into a fresh tempdir so the assertion is
        // self-contained and does not race with the cwd-relative
        // `./.alms/settings.json` other tests in this module write to.
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        let path = settings_path(&state.data_dir);
        assert!(
            !path.exists(),
            "precondition: settings.json must not exist before the rejected PATCH"
        );

        // Seed the live config with a known baseline so we can also
        // verify the per-knob ordering claim below.
        let baseline_effort = Some(alms_core::config::ReasoningEffort::High);
        state.agent_config.write().openai_reasoning_effort = baseline_effort;

        // Send an invalid `reasoning_effort` — `"turbo"` is not one of
        // minimal/low/medium/high so `ReasoningEffort::from_str` rejects
        // it and `errors` ends up non-empty.
        let body = PatchSettingsRequest {
            llm: Some(PatchLlm {
                openai: Some(PatchLlmOpenai {
                    reasoning_effort: Some("turbo".into()),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();

        assert_eq!(
            resp.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid reasoning_effort must produce 422"
        );

        // Core assertion: the rejected request did not write the
        // persistence file. On the next daemon boot, `alms.toml` /
        // env-var values for the LLM block will be honoured, not
        // shadowed by a bogus snapshot the operator was told failed.
        assert!(
            !path.exists(),
            "rejected PATCH must not persist settings.json — found at {}",
            path.display(),
        );

        // Bonus: for this specific knob the validation happens *before*
        // the live mutation (the parse() error path doesn't write to
        // `agent.openai_reasoning_effort`), so the live config is also
        // unchanged. This is the per-knob ordering #810 already had
        // right; the persistence skip closes the broader bug surface.
        assert_eq!(
            state.agent_config.read().openai_reasoning_effort,
            baseline_effort,
            "rejected reasoning_effort must not mutate the live config either"
        );
    }

    /// Same persistence-skip guarantee for the `context` sub-path. The
    /// `errors` vec is shared across all PATCH branches so the single
    /// `persist_settings` skip-on-error closes the same bug surface for
    /// every section uniformly — this test pins that behaviour for
    /// `context` so a future refactor that splits the error vec per
    /// section can't quietly regress it.
    #[tokio::test]
    async fn rejected_context_patch_does_not_persist_settings_json() {
        use axum::response::IntoResponse;

        let mut state = settings_test_app_state();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        let path = settings_path(&state.data_dir);

        // Invalid strategy — must be one of sliding-summary/full/truncate.
        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                strategy: Some("nonsense".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            !path.exists(),
            "rejected context PATCH must not persist settings.json"
        );
    }

    /// Rejected PATCH on a fresh state must also not overwrite a
    /// *pre-existing* `settings.json` from an earlier successful PATCH.
    /// This is the operationally damaging case: prior overrides stay
    /// intact, the rejected request is a true no-op on disk.
    #[tokio::test]
    async fn rejected_llm_patch_preserves_prior_settings_json() {
        use axum::response::IntoResponse;

        let mut state = settings_test_app_state();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        let path = settings_path(&state.data_dir);

        // Step 1: a valid PATCH lands and writes settings.json with
        // known values.
        {
            let body = PatchSettingsRequest {
                llm: Some(PatchLlm {
                    anthropic: Some(PatchLlmAnthropic {
                        thinking_budget_tokens: Some(8192),
                        prompt_cache_enabled: Some(false),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let resp = patch_settings(
                axum::extract::State(state.clone()),
                Json(serde_json::to_value(&body).unwrap()),
            )
            .await
            .into_response();
            assert_eq!(resp.status(), StatusCode::OK);
        }
        let prior_snapshot =
            std::fs::read_to_string(&path).expect("first PATCH should have written settings.json");
        assert!(
            prior_snapshot.contains("8192"),
            "sanity: prior snapshot must contain the previously-PATCHed thinking_budget"
        );

        // Step 2: a follow-up PATCH carries one valid mutation
        // (anthropic.thinking_budget_tokens = 16384) AND one invalid
        // mutation (openai.reasoning_effort = "turbo"). Pre-fix, this
        // would still rewrite settings.json from the live snapshot
        // (which now has 16384 baked in) — silently committing a
        // mutation the operator was told was a partial-failure 422.
        {
            let body = PatchSettingsRequest {
                llm: Some(PatchLlm {
                    anthropic: Some(PatchLlmAnthropic {
                        thinking_budget_tokens: Some(16384),
                        prompt_cache_enabled: None,
                    }),
                    openai: Some(PatchLlmOpenai {
                        reasoning_effort: Some("turbo".into()),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let resp = patch_settings(
                axum::extract::State(state.clone()),
                Json(serde_json::to_value(&body).unwrap()),
            )
            .await
            .into_response();
            assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        }

        // Disk must still hold the prior snapshot byte-for-byte. The
        // rejected request is a no-op at the persistence layer.
        let after_snapshot = std::fs::read_to_string(&path)
            .expect("settings.json must still exist from the prior successful PATCH");
        assert_eq!(
            after_snapshot, prior_snapshot,
            "rejected PATCH must not rewrite settings.json — \
             persisted snapshot drifted across a 422 response"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // #871 cross-field validator: SUMMARY_PROVIDER_REQUIRES_MODEL
    //
    // Mirror of #861's leak-guard tests but at the PATCH layer. The
    // validator rejects any combination that would leave the live config
    // with `summary_provider = Some(...)` AND `summary_model = None`,
    // regardless of which field the patch actually touched. The runtime
    // leak guard in `lifecycle.rs::build_summary_client` is the second
    // layer of defense for hand-edited TOML and any other path that
    // bypasses PATCH validation.
    // ─────────────────────────────────────────────────────────────────────

    /// Build an `AppState` with `[llm.providers.openrouter]` configured
    /// AND a usable API key in the secrets store, so the `summary_provider`
    /// validator passes the `SUMMARY_PROVIDER_UNKNOWN` /
    /// `SUMMARY_PROVIDER_MISSING_API_KEY` per-field checks. The
    /// cross-field invariant is what we want to exercise.
    fn settings_test_app_state_with_openrouter() -> crate::server::AppState {
        let mut state = settings_test_app_state();
        // Inject an `[llm.providers.openrouter]` entry so SUMMARY_PROVIDER_UNKNOWN
        // does not fire. The base_url / kind don't matter for validation.
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
        state
    }

    /// Helper: read the `errors` array from a PATCH /settings 422 response.
    async fn patch_and_read_errors(
        state: crate::server::AppState,
        body: PatchSettingsRequest,
    ) -> (StatusCode, Vec<String>) {
        use axum::body::to_bytes;
        use axum::response::IntoResponse;
        let resp = patch_settings(
            axum::extract::State(state),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let errors = json
            .get("errors")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        (status, errors)
    }

    /// Attempting to set `summary_provider` while `summary_model` is empty
    /// (cleared in the SAME patch) must produce 422 and leave both fields
    /// untouched in the live config.
    #[tokio::test]
    async fn patch_rejects_summary_provider_with_empty_summary_model() {
        let state = settings_test_app_state_with_openrouter();
        // Seed: both fields cleared so the pre-state is the default.
        {
            let mut agent = state.agent_config.write();
            agent.context_config.summary_provider = None;
            agent.context_config.summary_model = None;
        }

        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                summary_provider: Some("openrouter".into()),
                summary_model: Some(String::new()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let (status, errors) = patch_and_read_errors(state.clone(), body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("SUMMARY_PROVIDER_REQUIRES_MODEL")),
            "expected SUMMARY_PROVIDER_REQUIRES_MODEL error, got: {errors:?}"
        );

        let cfg = state.agent_config.read();
        assert!(
            cfg.context_config.summary_provider.is_none(),
            "rejected PATCH must not commit summary_provider"
        );
        assert!(
            cfg.context_config.summary_model.is_none(),
            "rejected PATCH must not commit summary_model"
        );
    }

    /// Setting `summary_provider` alone when `summary_model` is already
    /// `None` in the live config must produce 422 — the cross-field check
    /// considers the would-be combined post-state, not just the patch
    /// fields.
    #[tokio::test]
    async fn patch_rejects_summary_provider_when_live_model_is_none() {
        let state = settings_test_app_state_with_openrouter();
        {
            let mut agent = state.agent_config.write();
            agent.context_config.summary_provider = None;
            agent.context_config.summary_model = None;
        }

        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                summary_provider: Some("openrouter".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let (status, errors) = patch_and_read_errors(state.clone(), body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("SUMMARY_PROVIDER_REQUIRES_MODEL")),
            "expected SUMMARY_PROVIDER_REQUIRES_MODEL error, got: {errors:?}"
        );
        assert!(
            state
                .agent_config
                .read()
                .context_config
                .summary_provider
                .is_none(),
        );
    }

    /// Clearing `summary_model` (`""`) when the live `summary_provider`
    /// is `Some(...)` must produce 422 — the operator is told to clear
    /// both together, mirroring the validator's recommended remediation.
    #[tokio::test]
    async fn patch_rejects_clearing_model_while_provider_is_set() {
        let state = settings_test_app_state_with_openrouter();
        // Seed: both set together (the valid state).
        {
            let mut agent = state.agent_config.write();
            agent.context_config.summary_provider = Some("openrouter".into());
            agent.context_config.summary_model = Some("minimax/minimax-m2.7".into());
        }

        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                summary_model: Some(String::new()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let (status, errors) = patch_and_read_errors(state.clone(), body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("SUMMARY_PROVIDER_REQUIRES_MODEL")),
            "expected SUMMARY_PROVIDER_REQUIRES_MODEL error, got: {errors:?}"
        );

        let cfg = state.agent_config.read();
        assert_eq!(
            cfg.context_config.summary_provider,
            Some("openrouter".into()),
            "rejected PATCH must leave live summary_provider intact"
        );
        assert_eq!(
            cfg.context_config.summary_model,
            Some("minimax/minimax-m2.7".into()),
            "rejected PATCH must leave live summary_model intact"
        );
    }

    // ------------------------------------------------------------------
    // Symmetric pair-only validation (#872)
    //
    // The pre-#872 validator rejected only the (provider-set,
    // model-missing) direction. The (model-set, provider-missing)
    // direction was the original v0.2.2 default and silently paired
    // the user's `summary_model` with the agent's primary provider —
    // the exact misconfiguration that produced the 404 in #866. These
    // tests lock down the symmetric reject path so the regression
    // can't sneak back in.
    // ------------------------------------------------------------------

    /// Setting `summary_model` alone (provider untouched / unset) must
    /// fire the new symmetric `SUMMARY_MODEL_REQUIRES_PROVIDER` error.
    #[tokio::test]
    async fn patch_rejects_summary_model_when_provider_is_unset() {
        let state = settings_test_app_state_with_openrouter();
        {
            let mut agent = state.agent_config.write();
            agent.context_config.summary_provider = None;
            agent.context_config.summary_model = None;
        }

        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                summary_model: Some("minimax/minimax-m2.7".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let (status, errors) = patch_and_read_errors(state.clone(), body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("SUMMARY_MODEL_REQUIRES_PROVIDER")),
            "expected SUMMARY_MODEL_REQUIRES_PROVIDER error, got: {errors:?}"
        );
        let cfg = state.agent_config.read();
        assert!(
            cfg.context_config.summary_provider.is_none(),
            "rejected PATCH must not change summary_provider"
        );
        assert!(
            cfg.context_config.summary_model.is_none(),
            "rejected PATCH must not commit summary_model"
        );
    }

    /// Clearing `summary_provider` (`""`) when the live `summary_model`
    /// is `Some(...)` must produce 422 — symmetric to
    /// `patch_rejects_clearing_model_while_provider_is_set`. Pre-#872
    /// this combination was silently accepted, leaving the model
    /// stranded with no matching provider.
    #[tokio::test]
    async fn patch_rejects_clearing_provider_while_model_is_set() {
        let state = settings_test_app_state_with_openrouter();
        // Seed: both set together (the valid state).
        {
            let mut agent = state.agent_config.write();
            agent.context_config.summary_provider = Some("openrouter".into());
            agent.context_config.summary_model = Some("minimax/minimax-m2.7".into());
        }

        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                summary_provider: Some(String::new()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let (status, errors) = patch_and_read_errors(state.clone(), body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("SUMMARY_MODEL_REQUIRES_PROVIDER")),
            "expected SUMMARY_MODEL_REQUIRES_PROVIDER error, got: {errors:?}"
        );

        let cfg = state.agent_config.read();
        assert_eq!(
            cfg.context_config.summary_provider,
            Some("openrouter".into()),
            "rejected PATCH must leave live summary_provider intact"
        );
        assert_eq!(
            cfg.context_config.summary_model,
            Some("minimax/minimax-m2.7".into()),
            "rejected PATCH must leave live summary_model intact"
        );
    }

    /// Happy path: setting both fields together in one PATCH is accepted.
    #[tokio::test]
    async fn patch_accepts_summary_provider_and_model_together() {
        let state = settings_test_app_state_with_openrouter();
        {
            let mut agent = state.agent_config.write();
            agent.context_config.summary_provider = None;
            agent.context_config.summary_model = None;
        }

        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                summary_provider: Some("openrouter".into()),
                summary_model: Some("minimax/minimax-m2.7".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        use axum::response::IntoResponse;
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        let cfg = state.agent_config.read();
        assert_eq!(
            cfg.context_config.summary_provider,
            Some("openrouter".into())
        );
        assert_eq!(
            cfg.context_config.summary_model,
            Some("minimax/minimax-m2.7".into())
        );
    }

    /// Clearing both fields together is accepted — the cross-field
    /// invariant ("provider set without model") is `false` so PATCH lands.
    #[tokio::test]
    async fn patch_accepts_clearing_both_summary_fields_together() {
        let state = settings_test_app_state_with_openrouter();
        {
            let mut agent = state.agent_config.write();
            agent.context_config.summary_provider = Some("openrouter".into());
            agent.context_config.summary_model = Some("minimax/minimax-m2.7".into());
        }

        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                summary_provider: Some(String::new()),
                summary_model: Some(String::new()),
                ..Default::default()
            }),
            ..Default::default()
        };
        use axum::response::IntoResponse;
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        let cfg = state.agent_config.read();
        assert!(cfg.context_config.summary_provider.is_none());
        assert!(cfg.context_config.summary_model.is_none());
    }

    // ── #947: PATCH /settings rejects security knobs ──────────────────

    /// Read the JSON body of an axum response into a `serde_json::Value`.
    /// Used by the `[security]` rejection tests below.
    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let body = resp.into_body();
        let bytes = axum::body::to_bytes(body, usize::MAX)
            .await
            .expect("read body bytes");
        serde_json::from_slice(&bytes).expect("body is valid JSON")
    }

    /// `PATCH /settings` with a populated `security.allow_full_os_access`
    /// payload returns `400 SECURITY_KNOB_NOT_PATCHABLE` and does NOT
    /// mutate the live `state.security_config`.
    #[tokio::test]
    async fn patch_security_allow_full_os_access_returns_400() {
        use axum::response::IntoResponse;

        let mut state = settings_test_app_state();
        // Populate a non-empty baseline so we can assert PATCH didn't
        // mutate it (the snapshot is taken at gateway boot — patching is
        // forbidden, not just a no-op).
        state.security_config = alms_core::config::SecurityConfig {
            allow_full_os_access: vec!["seeded-agent".into()],
        };

        let payload = serde_json::json!({
            "security": {
                "allow_full_os_access": ["new-agent"],
            }
        });

        let resp = patch_settings(axum::extract::State(state.clone()), Json(payload))
            .await
            .into_response();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "PATCH on a security knob must return 400, not 422 — the \
             knob is config-file-only, not a validation failure (#947)"
        );
        let body = body_json(resp).await;
        assert_eq!(
            body.get("code").and_then(|v| v.as_str()),
            Some("SECURITY_KNOB_NOT_PATCHABLE")
        );
        // The structured error code is the load-bearing wire shape;
        // make sure the human-readable error message names the
        // offending field too so operators can debug from logs alone.
        let errors_str = body
            .get("errors")
            .map(|v| v.to_string())
            .unwrap_or_default();
        assert!(
            errors_str.contains("SECURITY_KNOB_NOT_PATCHABLE"),
            "errors array must surface the structured code: {errors_str}"
        );
        assert!(
            errors_str.contains("security"),
            "errors array must name the rejected section: {errors_str}"
        );

        // The snapshot on `AppState` must still hold the boot-time list.
        assert_eq!(
            state.security_config.allow_full_os_access,
            vec!["seeded-agent".to_string()],
            "rejected PATCH must not mutate the security_config snapshot"
        );
    }

    /// An empty `security: {}` object also fails — the rejection key is
    /// "the section is named at all", not "the section has values". A
    /// future PATCH evolution that adds `security` as a real serde field
    /// would otherwise silently start accepting the empty-object shape.
    #[tokio::test]
    async fn patch_security_empty_object_still_returns_400() {
        use axum::response::IntoResponse;

        let state = settings_test_app_state();

        let payload = serde_json::json!({ "security": {} });
        let resp = patch_settings(axum::extract::State(state.clone()), Json(payload))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_json(resp).await;
        assert_eq!(
            body.get("code").and_then(|v| v.as_str()),
            Some("SECURITY_KNOB_NOT_PATCHABLE")
        );
    }

    /// A request that mixes `security` with otherwise-valid fields is
    /// rejected as a whole — no partial application. A request the
    /// operator was told failed must NOT have written through to the
    /// live config (the same fail-closed posture #810 applies for the
    /// LLM path).
    #[tokio::test]
    async fn patch_security_combined_with_valid_fields_rejects_whole_request() {
        use axum::response::IntoResponse;

        let state = settings_test_app_state();
        // Seed a known baseline that the LLM block in the request would
        // otherwise overwrite if the rejection didn't fire first.
        let baseline_budget = state.agent_config.read().anthropic_thinking_budget;

        let payload = serde_json::json!({
            "security": { "allow_full_os_access": ["whatever"] },
            "llm": { "anthropic": { "thinking_budget_tokens": 9999 } },
        });

        let resp = patch_settings(axum::extract::State(state.clone()), Json(payload))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // The LLM block must NOT have landed — rejection is whole-payload,
        // not per-section. This is the same fail-closed posture other
        // PATCH guards use; here it doubles as a defence against an
        // attacker riding a security flip in alongside benign fields.
        assert_eq!(
            state.agent_config.read().anthropic_thinking_budget,
            baseline_budget,
            "rejected payload must not partially apply other fields"
        );
    }

    /// A payload that does NOT name `security` at the top level still
    /// goes through the normal handler — the rejection guard is
    /// surgical, not a blanket reject-everything-with-the-word.
    #[tokio::test]
    async fn patch_without_security_key_runs_normal_path() {
        use axum::response::IntoResponse;

        let state = settings_test_app_state();
        let payload = serde_json::json!({
            "llm": { "anthropic": { "thinking_budget_tokens": 4096 } },
        });

        let resp = patch_settings(axum::extract::State(state.clone()), Json(payload))
            .await
            .into_response();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "a payload without `security` at the top level must take the \
             normal happy path, not the security rejection"
        );
        assert_eq!(
            state.agent_config.read().anthropic_thinking_budget,
            4096,
            "the LLM mutation must have landed when no security key is named"
        );
    }

    // ─────────────────────────────────────────────────────────────────
    // #869 — context strategy redesign + legacy-field rejection
    // ─────────────────────────────────────────────────────────────────

    /// `PATCH /settings` with the new `compact_trigger_pct` knob persists
    /// the value into the live `AgentConfig` and round-trips through
    /// `GET /settings`. Pins the new wire surface end-to-end.
    #[tokio::test]
    async fn test_patch_context_compact_trigger_pct_persists() {
        use axum::response::IntoResponse;

        // Re-seed to the canonical defaults — `settings_test_app_state`
        // can pick up a stray `settings.json` from the cwd's `.alms/`
        // directory, so we explicitly write the baseline before
        // exercising the PATCH path.
        let mut state = settings_test_app_state();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        {
            let mut cfg = state.agent_config.write();
            cfg.context_config.compact_trigger_pct = 0.80;
            cfg.context_config.compact_retain_pct = 0.40;
        }

        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                compact_trigger_pct: Some(0.85),
                compact_retain_pct: Some(0.30),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        let cfg = state.agent_config.read();
        assert_eq!(cfg.context_config.compact_trigger_pct, 0.85);
        assert_eq!(cfg.context_config.compact_retain_pct, 0.30);
    }

    /// `PATCH /settings` with `context.recent_window` is rejected with
    /// `400 CONTEXT_LEGACY_FIELD_DEPRECATED` so cached UI bundles or
    /// scripted clients fail loud rather than silently no-op'ing.
    #[tokio::test]
    async fn test_patch_context_recent_window_rejected_with_400() {
        use axum::body::to_bytes;
        use axum::response::IntoResponse;

        let mut state = settings_test_app_state();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        // Seed a known baseline so the post-rejection assertion has a
        // stable reference even if the cwd's settings.json carried
        // something else into AppState construction.
        {
            let mut cfg = state.agent_config.write();
            cfg.context_config.compact_trigger_pct = 0.80;
        }

        let payload = serde_json::json!({
            "context": { "recent_window": 5 }
        });
        let resp = patch_settings(axum::extract::State(state.clone()), Json(payload))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            body.get("code").and_then(|v| v.as_str()),
            Some("CONTEXT_LEGACY_FIELD_DEPRECATED"),
            "expected the dedicated rejection code, got body: {body}"
        );
        // The live config must NOT have been mutated by a rejected request.
        assert_eq!(
            state.agent_config.read().context_config.compact_trigger_pct,
            0.80,
            "rejected legacy-field PATCH must not mutate the live config"
        );
    }

    /// `summary_interval` triggers the same rejection as `recent_window`.
    #[tokio::test]
    async fn test_patch_context_summary_interval_rejected_with_400() {
        use axum::response::IntoResponse;

        let mut state = settings_test_app_state();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        let payload = serde_json::json!({
            "context": { "summary_interval": 30 }
        });
        let resp = patch_settings(axum::extract::State(state.clone()), Json(payload))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// `PATCH /settings` with `strategy = "sliding-summary"` is accepted
    /// as a back-compat alias and rewritten to `"compact"` on commit. The
    /// PATCH does NOT 4xx — internal automation that still sends the
    /// legacy strategy name continues to work for one major version.
    #[tokio::test]
    async fn test_patch_context_sliding_summary_alias_accepted() {
        use axum::response::IntoResponse;

        let mut state = settings_test_app_state();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                strategy: Some("sliding-summary".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(state.agent_config.read().context_config.strategy, "compact");
    }

    /// Out-of-range trigger / retain values fail with a 422 and don't
    /// mutate the live config. Pins the per-knob clamps from
    /// `normalize_episodic` at the PATCH layer.
    #[tokio::test]
    async fn test_patch_context_compact_trigger_out_of_range_rejected() {
        use axum::response::IntoResponse;

        let mut state = settings_test_app_state();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        {
            let mut cfg = state.agent_config.write();
            cfg.context_config.compact_trigger_pct = 0.80;
        }

        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                compact_trigger_pct: Some(0.99),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        // Live config baseline preserved.
        assert_eq!(
            state.agent_config.read().context_config.compact_trigger_pct,
            0.80
        );
    }

    /// PR #1012 / Codex review: `PATCH /settings` that fails the
    /// cross-field `retain + 0.10 <= trigger` gap check must not commit
    /// either `compact_trigger_pct` or `compact_retain_pct` to live
    /// config. Pre-fix, both writes landed before the gap check ran,
    /// leaving the daemon serving runs with the invalid pair until a
    /// later PATCH or a restart.
    ///
    /// Baseline: `(0.80, 0.40)` (gap = 0.40, ok).
    /// Patch:    `(0.55, 0.50)` — gap = 0.05, fails the gap floor.
    /// Expected: 422 with the gap error message AND live config still
    /// at baseline (0.80, 0.40).
    #[tokio::test]
    async fn test_patch_context_gap_floor_failure_does_not_commit() {
        use axum::body::to_bytes;
        use axum::response::IntoResponse;

        let mut state = settings_test_app_state();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        {
            let mut cfg = state.agent_config.write();
            cfg.context_config.compact_trigger_pct = 0.80;
            cfg.context_config.compact_retain_pct = 0.40;
        }

        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                // Both per-knob ranges pass (trigger in [0.50, 0.95],
                // retain in [0.20, 0.60]); only the gap fails.
                compact_trigger_pct: Some(0.55),
                compact_retain_pct: Some(0.50),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        // Body carries the gap-floor error message.
        let body_bytes = to_bytes(resp.into_body(), 65536).await.unwrap();
        let body_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let errors = body_json["errors"].as_array().expect("errors array");
        assert!(
            errors
                .iter()
                .any(|e| e.as_str().unwrap_or("").contains("at least 0.10 below")),
            "expected gap-floor error, got: {errors:?}"
        );

        // Live config UNCHANGED at baseline — neither knob committed.
        let cfg = state.agent_config.read();
        assert_eq!(
            cfg.context_config.compact_trigger_pct, 0.80,
            "compact_trigger_pct must not commit when gap check fails — partial mutation regression"
        );
        assert_eq!(
            cfg.context_config.compact_retain_pct, 0.40,
            "compact_retain_pct must not commit when gap check fails — partial mutation regression"
        );
    }

    /// `PersistedSettings` does NOT carry `security`. Operators who
    /// edit `[security]` in `alms.toml` and restart see the new value;
    /// PATCH-style persistence must never round-trip the section. This
    /// test pins the absence so a future refactor that adds `security`
    /// to `PersistedSettings` will fail loudly.
    #[test]
    fn persisted_settings_struct_does_not_serialize_security() {
        // Build a fully-populated `PersistedSettings` and serialize it.
        // The wire shape must not contain a `security` key.
        let persisted = PersistedSettings {
            context: Some(PersistedContextOverrides::default()),
            session: Some(PersistedSessionOverrides::default()),
            tools: Some(PersistedToolsOverrides::default()),
            llm: Some(PersistedLlmOverrides::default()),
        };
        let json = serde_json::to_string(&persisted).expect("serialize PersistedSettings");
        assert!(
            !json.contains("security"),
            "PersistedSettings on-disk shape must not contain a `security` \
             key — the section is config-file-only and PATCH-persistence \
             would re-introduce a back door (#947). Serialized: {json}"
        );
        assert!(
            !json.contains("allow_full_os_access"),
            "Persisted JSON must not contain `allow_full_os_access` either: {json}"
        );
    }

    // ── #919 / PR #1020 Tim review item 3: PATCH /settings budget validation ──
    //
    // Mirror the per-run `pre_flight_token_budget` guard at PATCH time so
    // an operator can't bump `[context].max_input_tokens` past the
    // boot-time `(provider, model)` cap, get a 200, and then hit a 400
    // on the next `POST /runs`. Strict-mode overshoot is a structured
    // 400 rejection (whole PATCH never lands); warn-mode logs and lets
    // the patch proceed.

    /// Pin a known overshoot config (anthropic + claude-haiku-4-5, 200K cap)
    /// so the same fixture works for both strict and warn tests below.
    fn settings_test_app_state_with_anthropic_haiku() -> crate::server::AppState {
        let mut state = settings_test_app_state();
        // Boot-time provider/model — the PATCH validator reads these and
        // they cannot be mutated via PATCH, only via TOML / `alms auth`.
        state.llm_config.provider = "anthropic".into();
        state.llm_config.default_model = "claude-haiku-4-5".into();
        state.llm_config.mock = false;
        // Live `max_tokens` — the second budget knob; combined with the
        // `max_input_tokens` candidate to compute the effective total.
        // Default is `DEFAULT_AGENT_MAX_TOKENS = 32_000`; pin it explicitly
        // here so the assertions below carry self-documenting numbers.
        {
            let mut cfg = state.agent_config.write();
            cfg.max_tokens = alms_core::config::DEFAULT_AGENT_MAX_TOKENS;
        }
        state
    }

    /// Strict mode (default): a PATCH that pushes `max_input_tokens`
    /// past the boot-time provider cap is rejected with a structured
    /// `400 INVALID_TOKEN_BUDGET_FOR_PROVIDER`. Live config must be
    /// untouched, persistence file must not be written, the response
    /// body must carry every datum the operator needs to fix the config
    /// (provider, model, both knobs, effective_total, provider_cap).
    #[tokio::test]
    async fn test_patch_settings_rejects_budget_overshoot_in_strict_mode() {
        use axum::body::to_bytes;
        use axum::response::IntoResponse;

        // Pin strict mode for this test; concurrent warn-mode tests share
        // the same env-var via the shared `BUDGET_VALIDATION_ENV_LOCK`.
        let _env = crate::test_env_locks::BudgetValidationEnvGuard::unset();

        let mut state = settings_test_app_state_with_anthropic_haiku();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        // Seed a baseline well below the cap so we can assert the live
        // config did NOT mutate to the overshooting candidate.
        {
            let mut cfg = state.agent_config.write();
            cfg.context_config.max_input_tokens = 128_000;
        }

        // Candidate: 250_000 + 32_000 = 282_000 > 200_000 (Haiku 4.5 cap).
        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                max_input_tokens: Some(250_000),
                ..Default::default()
            }),
            ..Default::default()
        };

        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();

        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "PATCH overshoot must be a structured 400, not 422 — same envelope as the per-run guard (#919)"
        );

        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let body_json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            body_json["error_code"], "INVALID_TOKEN_BUDGET_FOR_PROVIDER",
            "body must carry the structured error_code so clients can branch on it"
        );
        assert_eq!(body_json["provider"], "anthropic");
        assert_eq!(body_json["model"], "claude-haiku-4-5");
        assert_eq!(body_json["max_input_tokens"], 250_000);
        assert_eq!(body_json["max_tokens"], 32_000);
        assert_eq!(body_json["effective_total"], 282_000);
        assert_eq!(body_json["provider_cap"], 200_000);
        let message = body_json["message"]
            .as_str()
            .expect("message must be a string");
        assert!(
            message.contains("max_input_tokens") && message.contains("max_tokens"),
            "message must name both budget knobs: {message}"
        );

        // Live config UNCHANGED — strict-mode PATCH-budget rejection is
        // whole-payload, not per-section. The 250K candidate did not
        // commit to `state.agent_config`.
        assert_eq!(
            state.agent_config.read().context_config.max_input_tokens,
            128_000,
            "rejected PATCH must not mutate live max_input_tokens"
        );

        // Persistence file must NOT have been written — a rejected PATCH
        // is side-effect-free at the persistence layer (mirrors the
        // existing 422 -> no-persist contract from #810).
        let settings_path = tmp.path().join("settings.json");
        assert!(
            !settings_path.exists(),
            "rejected PATCH must not write settings.json — found {}",
            settings_path.display()
        );
    }

    /// Warn mode: the same overshoot config is accepted (the env-var
    /// downgrades the strict reject to a structured WARN log). Live
    /// config commits the new `max_input_tokens` value.
    #[tokio::test]
    async fn test_patch_settings_warn_mode_accepts_budget_overshoot() {
        use axum::response::IntoResponse;

        // Pin warn mode for this test so a concurrent strict-mode test
        // can't make us silently reject the overbudget config.
        let _env = crate::test_env_locks::BudgetValidationEnvGuard::set("warn");

        let mut state = settings_test_app_state_with_anthropic_haiku();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        {
            let mut cfg = state.agent_config.write();
            cfg.context_config.max_input_tokens = 128_000;
        }

        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                max_input_tokens: Some(250_000),
                ..Default::default()
            }),
            ..Default::default()
        };

        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();

        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "warn-mode PATCH must accept the same overshoot config that strict-mode rejects"
        );
        assert_eq!(
            state.agent_config.read().context_config.max_input_tokens,
            250_000,
            "warn-mode PATCH must commit the candidate value to live config"
        );
    }

    /// Unknown `(provider, model)` pair: validator skips the cap check
    /// regardless of budget size, just like the per-run path. PATCH
    /// proceeds with normal 200 / commit semantics.
    #[tokio::test]
    async fn test_patch_settings_skips_validation_for_unknown_model() {
        use axum::response::IntoResponse;

        let _env = crate::test_env_locks::BudgetValidationEnvGuard::unset();

        let mut state = settings_test_app_state();
        // Default test state already uses provider=openrouter,
        // model=moonshotai/kimi-k2.5 — both unknown to the budget table.
        // Bump the candidate to a wildly large value to prove "unknown"
        // really does mean "skip" (not "fail open accidentally because
        // the test fixture is small").
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        {
            let mut sess = state.session_config.write();
            sess.max_context_tokens = 10_000_000;
        }

        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                max_input_tokens: Some(10_000_000),
                ..Default::default()
            }),
            ..Default::default()
        };

        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();

        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "unknown (provider, model) must skip the budget check, mirroring the per-run validator's contract"
        );
        assert_eq!(
            state.agent_config.read().context_config.max_input_tokens,
            10_000_000,
        );
    }

    /// Mock mode: the validator is bypassed entirely (mirrors
    /// `validate_at_config_load`). A PATCH that would overshoot a real
    /// provider's cap lands cleanly when the daemon is in mock mode.
    #[tokio::test]
    async fn test_patch_settings_skips_validation_in_mock_mode() {
        use axum::response::IntoResponse;

        let _env = crate::test_env_locks::BudgetValidationEnvGuard::unset();

        let mut state = settings_test_app_state_with_anthropic_haiku();
        // Flip mock on — the validator must skip even though the
        // candidate budget overshoots Haiku 4.5's cap.
        state.llm_config.mock = true;
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                max_input_tokens: Some(250_000),
                ..Default::default()
            }),
            ..Default::default()
        };

        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();

        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "mock mode must bypass token-budget validation entirely (matches load-time behaviour)"
        );
    }

    // ── #919 / Codex P2 #2: PATCH /settings fleet evaluation ──────────
    //
    // The PATCH guard's first iteration only validated the candidate
    // `max_input_tokens` against the boot-time server-default
    // `(provider, model)`. Per-agent overrides bypass it: an agent
    // whose `record.provider`/`record.model` resolve to a smaller cap
    // could become unrunnable under the new server-default while the
    // PATCH still returns 200, deferring the failure to the next
    // `POST /runs` 400. The Codex follow-up extends `validate_patch_budget`
    // to iterate every registered agent and reject the entire PATCH if
    // any agent overshoots, naming each offender in the response body.

    /// SQLite-backed app state — needed for the fleet evaluation tests
    /// because `state.session_manager.store()` returns `None` on the
    /// in-memory default helper above. Mirrors
    /// `runs::integration_tests::test_app_state_with_sqlite`.
    fn settings_test_app_state_with_sqlite() -> crate::server::AppState {
        let gateway_config = crate::gateway::GatewayConfig {
            db_path: Some(":memory:".to_string()),
            ..crate::gateway::GatewayConfig::default()
        };
        let gateway = crate::gateway::Gateway::new(gateway_config).unwrap();
        let scheduler = std::sync::Arc::new(alms_runtime::Scheduler::new());
        let shutdown_token = tokio_util::sync::CancellationToken::new();
        let (completion_tx, _cr) = tokio::sync::mpsc::unbounded_channel();
        let (trigger_tx, _tr) = tokio::sync::mpsc::unbounded_channel();
        let (dm_event_tx, _dr) = tokio::sync::mpsc::unbounded_channel();
        crate::server::AppState::new(
            gateway,
            scheduler,
            shutdown_token,
            completion_tx,
            trigger_tx,
            dm_event_tx,
        )
        .unwrap()
    }

    /// Seed an `AgentRecord` with the given per-agent provider/model pair.
    /// All other fields default — the fleet check only looks at provider
    /// and model.
    fn seed_agent(
        state: &crate::server::AppState,
        name: &str,
        provider: Option<&str>,
        model: Option<&str>,
    ) {
        use alms_core::registry::AgentRecord;
        use chrono::Utc;
        let now = Utc::now();
        let record = AgentRecord {
            id: alms_core::AgentId::new(),
            name: name.to_string(),
            description: String::new(),
            model: model.map(|s| s.to_string()),
            posture: None,
            provider: provider.map(|s| s.to_string()),
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
            worktree_mode: alms_core::WorktreeMode::Off,
            debug_mode: false,
            is_default: false,
            created_at: now,
            last_active: now,
        };
        state
            .session_manager
            .store()
            .expect("SQLite-backed state must have a store")
            .create_agent(&record)
            .expect("agent seed should succeed");
    }

    /// Two agents on the same server-default provider, but one pinned
    /// to a tight-cap model (Haiku 4.5, 200K) and one pinned to a loose
    /// model (Opus 4.7, 1M). The PATCH bumps `max_input_tokens` to a
    /// value that fits Opus's 1M but overshoots Haiku's 200K. The fleet
    /// guard must reject the PATCH and name the tight-cap agent.
    #[tokio::test]
    async fn test_patch_settings_fleet_rejects_when_only_one_agent_overshoots() {
        use axum::body::to_bytes;
        use axum::response::IntoResponse;

        let _env = crate::test_env_locks::BudgetValidationEnvGuard::unset();

        let mut state = settings_test_app_state_with_sqlite();
        // Server default lands on Sonnet 4.6 (1M cap) so the layer-1
        // server-default check passes; the fleet layer is the one that
        // must catch the tight-cap agent.
        state.llm_config.provider = "anthropic".into();
        state.llm_config.default_model = "claude-sonnet-4-6".into();
        state.llm_config.mock = false;
        {
            let mut cfg = state.agent_config.write();
            cfg.max_tokens = 32_000;
            cfg.context_config.max_input_tokens = 128_000;
        }
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        // Agent A: tight cap (Haiku 4.5, 200K). Will overshoot.
        seed_agent(&state, "tight", Some("anthropic"), Some("claude-haiku-4-5"));
        // Agent B: loose cap (Opus 4.7, 1M). Will fit.
        seed_agent(&state, "loose", Some("anthropic"), Some("claude-opus-4-7"));

        // Candidate: 250K input + 32K output = 282K.
        // - Sonnet 4.6 (1M)        → fits, server-default layer passes
        // - Haiku 4.5 (200K)       → overshoots, fleet layer fires
        // - Opus 4.7 (1M)          → fits
        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                max_input_tokens: Some(250_000),
                ..Default::default()
            }),
            ..Default::default()
        };

        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();

        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "fleet evaluation must reject the PATCH when ANY agent overshoots"
        );

        let body_bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(body["error_code"], "INVALID_TOKEN_BUDGET_FOR_PROVIDER");

        // The `agents` array must list the offenders, naming each one
        // so the operator sees the per-agent impact at a glance.
        let agents = body["agents"]
            .as_array()
            .expect("response body must carry an `agents` array of offenders");
        assert_eq!(
            agents.len(),
            1,
            "exactly one agent should overshoot in this fixture; got: {body}",
        );
        assert_eq!(
            agents[0]["name"], "tight",
            "offender array must name the tight-cap agent"
        );
        assert_eq!(agents[0]["provider"], "anthropic");
        assert_eq!(agents[0]["model"], "claude-haiku-4-5");
        assert_eq!(agents[0]["agent_cap"], 200_000);
        assert_eq!(agents[0]["would_be_total"], 282_000);

        // Live config must not have mutated — the PATCH never landed.
        assert_eq!(
            state.agent_config.read().context_config.max_input_tokens,
            128_000,
            "rejected PATCH must not commit any mutation"
        );
    }

    /// Two agents both fit the new cap (one on the loose server default,
    /// one on a per-agent override that happens to also fit). The PATCH
    /// commits and lands on live config.
    #[tokio::test]
    async fn test_patch_settings_fleet_commits_when_all_agents_fit() {
        use axum::response::IntoResponse;

        let _env = crate::test_env_locks::BudgetValidationEnvGuard::unset();

        let mut state = settings_test_app_state_with_sqlite();
        state.llm_config.provider = "anthropic".into();
        state.llm_config.default_model = "claude-sonnet-4-6".into();
        state.llm_config.mock = false;
        {
            let mut cfg = state.agent_config.write();
            cfg.max_tokens = 32_000;
            cfg.context_config.max_input_tokens = 128_000;
        }
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        // Both agents land on a 1M cap.
        seed_agent(&state, "alpha", Some("anthropic"), Some("claude-opus-4-7"));
        seed_agent(&state, "beta", Some("anthropic"), Some("claude-sonnet-4-6"));

        // Candidate: 250K + 32K = 282K — fits 1M comfortably for every
        // agent. Fleet layer must accept and the PATCH must commit.
        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                max_input_tokens: Some(250_000),
                ..Default::default()
            }),
            ..Default::default()
        };

        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();

        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "fleet evaluation must accept when every registered agent fits the new cap"
        );
        assert_eq!(
            state.agent_config.read().context_config.max_input_tokens,
            250_000,
            "accepted PATCH must commit through to live agent_config"
        );
    }

    /// Regression for the Codex P2 follow-up on PR #1020 — fleet evaluation
    /// must mirror the runtime's #942 cross-namespace drop before validating
    /// the budget. Pre-fix, an agent with a stale cross-namespace `model`
    /// (e.g. `record.provider = "anthropic"`, `record.model = "gpt-5.5"`)
    /// validated against the stale `(anthropic, gpt-5.5)` pair, which the
    /// budget table does not know about — `validate_token_budget` returned
    /// `Ok(())` (silent skip), and the PATCH committed even though the
    /// runtime would resolve to a model with a tighter cap and immediately
    /// fail `INVALID_TOKEN_BUDGET_FOR_PROVIDER` on the next `POST /runs`.
    ///
    /// Fixture: server default is `(openai, gpt-5.5)` (1.05M cap). The
    /// `[llm.providers.anthropic]` entry pins its own `model` to
    /// `claude-haiku-4-5` (200K cap) so the runtime fallback chain has
    /// somewhere to land after the stale per-agent model is dropped. Agent
    /// A carries the cross-namespace pair `(anthropic, gpt-5.5)`. The
    /// candidate `max_input_tokens` is 200K — server-default check passes
    /// (232K < 1.05M) but the post-fallback fleet pair `(anthropic,
    /// claude-haiku-4-5)` overshoots the 200K Haiku cap (232K > 200K).
    /// Pre-fix this returns 200; post-fix it must return 400.
    #[tokio::test]
    async fn test_patch_settings_fleet_drops_cross_namespace_stale_model_before_validating() {
        use axum::body::to_bytes;
        use axum::response::IntoResponse;

        let _env = crate::test_env_locks::BudgetValidationEnvGuard::unset();

        let mut state = settings_test_app_state_with_sqlite();
        state.llm_config.provider = "openai".into();
        state.llm_config.default_model = "gpt-5.5".into();
        state.llm_config.mock = false;
        // Anthropic provider entry pins its own model to a tight-cap value
        // — this is the model the runtime falls back to after the
        // cross-namespace drop fires on the per-agent stale `gpt-5.5`.
        state.llm_config.providers.insert(
            "anthropic".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: None,
                model: Some("claude-haiku-4-5".into()),
                auth_scheme: alms_core::config::AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        state.llm_config.providers.insert(
            "openai".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::OpenAiCompatible,
                base_url: "https://api.openai.com/v1".into(),
                api_key_env: None,
                api_key: None,
                model: None,
                auth_scheme: alms_core::config::AuthScheme::Bearer,
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        {
            let mut cfg = state.agent_config.write();
            cfg.max_tokens = 32_000;
            cfg.context_config.max_input_tokens = 128_000;
        }
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        // Cross-namespace stale: agent's per-agent provider override
        // switched to anthropic but the per-agent model is still the
        // openai-namespace `gpt-5.5`. Runtime drops the model and falls
        // back to the anthropic entry's `claude-haiku-4-5`.
        seed_agent(&state, "stale", Some("anthropic"), Some("gpt-5.5"));

        // Candidate 200K + 32K = 232K. Caps:
        // - openai gpt-5.5      → 1_050_000 (server-default check: 232K fits)
        // - anthropic gpt-5.5   → unknown   (pre-fix path: skipped → green)
        // - anthropic haiku-4-5 → 200_000   (post-fix path: 232K overshoots)
        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                max_input_tokens: Some(200_000),
                ..Default::default()
            }),
            ..Default::default()
        };

        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();

        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "fleet evaluation must drop the cross-namespace stale model and \
             validate against the runtime's effective `(provider, model)` \
             — Codex P2 follow-up on PR #1020"
        );

        let body_bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(body["error_code"], "INVALID_TOKEN_BUDGET_FOR_PROVIDER");

        let agents = body["agents"]
            .as_array()
            .expect("response body must carry an `agents` array of offenders");
        assert_eq!(
            agents.len(),
            1,
            "exactly one agent should overshoot in this fixture; got: {body}",
        );
        assert_eq!(agents[0]["name"], "stale");
        assert_eq!(
            agents[0]["provider"], "anthropic",
            "fleet check must report the runtime's effective provider"
        );
        assert_eq!(
            agents[0]["model"], "claude-haiku-4-5",
            "fleet check must report the runtime's effective model after the \
             #942 cross-namespace drop, not the stale per-agent `gpt-5.5`"
        );
        assert_eq!(agents[0]["agent_cap"], 200_000);
        assert_eq!(agents[0]["would_be_total"], 232_000);

        // Live config must not have mutated — the PATCH never landed.
        assert_eq!(
            state.agent_config.read().context_config.max_input_tokens,
            128_000,
            "rejected PATCH must not commit any mutation"
        );
    }

    /// Codex P2 follow-up on PR #1020 — fail PATCH budget validation
    /// CLOSED when the agent registry cannot be listed.
    ///
    /// Pre-fix shape: `validate_patch_budget` swallowed `list_agents()`
    /// errors with `warn! + return None`, so a PATCH that fit the
    /// boot-time server default but would have made some per-agent
    /// resolved cap unrunnable could return 200 and commit while
    /// SQLite was temporarily failing — defeating the entire fleet
    /// guarantee. The runtime would then 400 every subsequent
    /// `POST /runs` for the affected agents until the operator
    /// rolled the PATCH back.
    ///
    /// Post-fix shape: the validator returns `Err((503, envelope))`
    /// with `error_code = AGENT_STORE_UNAVAILABLE`. The candidate-then-
    /// commit gate guarantees the live `AgentConfig` and the on-disk
    /// `settings.json` are both untouched — same persistence contract
    /// as the existing per-agent overshoot rejection.
    ///
    /// Fixture: drop the underlying `agents` table after
    /// `settings_test_app_state_with_sqlite()` builds the store, so
    /// `list_agents()` fails at the `prepare()` step with
    /// `no such table: agents`. The PATCH carries a candidate that
    /// fits the server-default `(anthropic, claude-sonnet-4-6)` 1M
    /// cap, so layer 1 passes — only the fleet layer can fail this
    /// request.
    #[tokio::test]
    async fn test_patch_settings_fleet_fails_closed_when_agent_store_unavailable() {
        use axum::body::to_bytes;
        use axum::response::IntoResponse;

        let _env = crate::test_env_locks::BudgetValidationEnvGuard::unset();

        let mut state = settings_test_app_state_with_sqlite();
        state.llm_config.provider = "anthropic".into();
        state.llm_config.default_model = "claude-sonnet-4-6".into();
        state.llm_config.mock = false;
        {
            let mut cfg = state.agent_config.write();
            cfg.max_tokens = 32_000;
            cfg.context_config.max_input_tokens = 128_000;
        }
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        let path = settings_path(&state.data_dir);
        assert!(
            !path.exists(),
            "precondition: settings.json must not exist before the rejected PATCH"
        );

        // Simulate "SQLite temporarily failing" by dropping the agents
        // table. `list_agents()` then fails at `prepare()` with
        // `no such table: agents`. The doc-hidden `_for_test` method
        // is the cross-crate test affordance — `#[cfg(test)]` would
        // be scoped to alms-session's own test target only.
        state
            .session_manager
            .store()
            .expect("SQLite-backed state must have a store")
            .drop_agents_table_for_test()
            .expect("dropping agents table should succeed");

        // Candidate: 250K + 32K = 282K. Sonnet 4.6 has a 1M cap, so
        // the layer-1 server-default check passes cleanly. The fleet
        // layer is the only thing that could refuse this request —
        // and it can't run because list_agents() now fails.
        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                max_input_tokens: Some(250_000),
                ..Default::default()
            }),
            ..Default::default()
        };

        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();

        // Core assertion: 503 (not 200, not 400). Pre-fix this returned
        // 200 with a silently-skipped fleet check.
        assert_eq!(
            resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "PATCH must fail closed with 503 when list_agents() errors — \
             silently skipping the fleet check would defeat the per-agent \
             budget guarantee"
        );

        let body_bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let resp_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(
            resp_body["error_code"], "AGENT_STORE_UNAVAILABLE",
            "wire body must carry the structured failure code so \
             clients can distinguish this from a routing 503; got: {resp_body}",
        );
        assert!(
            resp_body["message"]
                .as_str()
                .is_some_and(|m| m.contains("REJECTED")),
            "message must explicitly state the PATCH was rejected (not a \
             warning) so operators know to retry; got: {resp_body}",
        );

        // Live config must NOT have mutated — the candidate-then-commit
        // gate ran before any inline write. Pre-fix, layer 1 would have
        // returned `None`, the early-return at the handler would not
        // fire, and `context.max_input_tokens` would have committed to
        // 250_000 even though the fleet layer could not vouch for it.
        assert_eq!(
            state.agent_config.read().context_config.max_input_tokens,
            128_000,
            "rejected PATCH must not commit any mutation to live config",
        );

        // settings.json must NOT have been written — `persist_settings`
        // sits behind both the budget early-return and the
        // `errors.is_empty()` gate, so a 503 here is side-effect-free
        // at the persistence layer (same contract as the rejected_llm
        // and rejected_context tests above).
        assert!(
            !path.exists(),
            "rejected 503 PATCH must not persist settings.json — found at {}",
            path.display(),
        );
    }
}
