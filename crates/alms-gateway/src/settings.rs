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

    // Top-level `model` / `provider` are the *live* server-default
    // `(model, provider)` pair — since #1148 they are live for runs too,
    // not just for this display surface: `PATCH /settings` commits here
    // and rebuilds the shared `state.llm` client from the same pair, so
    // the next run sends on it with no daemon restart.
    let server_llm = state.server_llm_default.read().clone();
    // `base_url` must come from the LIVE client, not the boot-time
    // `state.llm_config` clone. A live provider switch re-derives the
    // base URL from `[llm.providers.<new>].base_url`; reporting the
    // boot value next to the patched provider name would describe a
    // wire nobody is talking to.
    let base_url = state.llm.read().base_url().to_string();
    Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "provider": server_llm.provider,
        "model": server_llm.model,
        "base_url": base_url,
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
///
/// `model` and `provider` are the server-default LLM model / provider — the
/// values agents inherit when they don't carry a per-agent override.
/// Since #1148 both are **live-mutable**: a successful PATCH commits the
/// pair to `state.server_llm_default`, rebuilds the shared
/// `state.llm` client from it, and persists it to `settings.json` for
/// restart survival. The next run picks the new pair up with no daemon
/// restart, matching the `context` / `session` / `tools` / `llm`
/// sections; in-flight runs are unaffected (they resolved their client
/// at run start).
///
/// Propagation is HTTP-path only, exactly like every other live-mutable
/// section: Telegram-triggered runs resolve against the `LlmClient` owned
/// by `Gateway` (a boot-time clone) and keep the boot pair until restart.
/// See `docs/api.md` § 10.2.
///
/// Per-agent model / provider overrides on the agent registry (`PATCH
/// /agents/{id}`) continue to win over the server default — this surface
/// only moves the value agents fall back to.
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PatchSettingsRequest {
    pub context: Option<PatchContext>,
    pub session: Option<PatchSession>,
    pub tools: Option<PatchTools>,
    pub llm: Option<PatchLlm>,
    /// Server-default LLM model. `Some("")` is rejected — there is no
    /// clear sentinel on this surface, since "no server-default model" is
    /// not a runnable state. Committed to `state.server_llm_default`,
    /// applied to the live `state.llm` client, and persisted into
    /// `PersistedSettings.model` for re-application on the next boot.
    pub model: Option<String>,
    /// Server-default LLM provider. Must be a key in `state.llm_config.providers`
    /// (the `[llm.providers]` map is config-file-only and never changes at
    /// runtime, so the boot snapshot is authoritative here). `Some("")` is
    /// rejected. Persisted into `PersistedSettings.provider`.
    pub provider: Option<String>,
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
/// until the daemon restarts. See the Telegram loop inside
/// `gateway.rs::Gateway::run_until_shutdown` for
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

    // Tim review on #1148: serialise the whole
    // validate/commit/rebuild/persist sequence.
    //
    // Every gate below reads the live `(provider, model)` pair, commits
    // several statements later, rebuilds the shared client after that, and
    // finally rewrites `settings.json` — with no lock held across the gap.
    // Two concurrent PATCHes can interleave those steps: `{"model":
    // "gpt-4o"}` validated against a live `openrouter` (accepted — the
    // kind is permissive) and `{"provider": "anthropic", "model":
    // "claude-sonnet-4-6"}` (accepted) can commit in an order that leaves
    // the live client on `(anthropic, gpt-4o)`. That is the incoherent
    // pair on the live wire the gates exist to prevent, reached through a
    // different door — and before #1148 the same race only corrupted
    // `settings.json`, so the blast radius grew with this change.
    //
    // Held for the rest of the handler, which also stops two writers from
    // racing on `settings.json`. Safe because `patch_settings` contains no
    // `.await` — and clippy's `await_holding_lock` turns that from a
    // convention into a compile-time tripwire if anyone adds one.
    let _patch_guard = state.settings_patch_lock.lock();

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
    // Codex follow-up on #1081 (P1): compute the would-be post-PATCH server
    // default `(provider, model)` and pass it into `validate_patch_budget`
    // so the budget check sees the *post-PATCH* pair rather than the
    // boot-time `state.llm_config.*` clone. Without this, a PATCH that
    // raises `context.max_input_tokens` while also switching to a stricter
    // provider/model could return 200 and persist an over-budget pair
    // that later rejects every run after restart.
    //
    // Codex follow-up on #1081 (P1 #3): the candidate-model resolution here
    // must mirror the commit-path resolution at the `provider_to_commit` /
    // `model_to_commit_with_provider` block below — namely
    // `body.model -> entry_model -> live_default.model`. Without the
    // `entry_model` step, a PATCH like
    // `{provider: "anthropic", max_input_tokens: 500_000}` (no `model` in
    // the body) is budget-validated against
    // `(anthropic, <stale_cross_namespace_live_default>)` — a pair the
    // budget table doesn't recognise, so the validator silently passes —
    // while the commit path later writes
    // `(anthropic, <entry_model>)` (e.g. `claude-haiku-4-5`, 200K cap) to
    // `server_llm_default`. The PATCH returns 200, persistence carries the
    // coherent pair across restart, and every subsequent run rejects with
    // `INVALID_TOKEN_BUDGET_FOR_PROVIDER` because 500K > 200K.
    let live_default = state.server_llm_default.read().clone();
    // Snapshot `live_default.provider` separately so it stays available
    // for the `entry_model_for_candidate` no-op guard below — the
    // `candidate_provider` builder moves the field out via `unwrap_or`.
    let live_default_provider = live_default.provider.clone();
    let candidate_provider = body
        .provider
        .as_deref()
        .filter(|p| !p.is_empty())
        .and_then(|p| {
            state
                .llm_config
                .providers
                .contains_key(p)
                .then(|| p.to_string())
        })
        .unwrap_or(live_default.provider);
    // Entry-level model fallback for the provider the PATCH would commit.
    // Only consulted when the body omits `model` AND the body actually
    // *switches* provider (i.e. `body.provider != live_default.provider`).
    // When the provider is unchanged we keep the live default model,
    // mirroring the commit-path guard at lines ~988-989 below
    // (`if body_model.is_none() && provider != current_provider`).
    //
    // Codex follow-up on #1081 (P1, Finding 5, refs issue #1086 Item 2):
    // the pre-fix overlay computed `entry_model_for_candidate` whenever
    // `body.provider` was present, even if it matched the live default.
    // The commit path, however, only adopts `entry_model` when the
    // provider *changes*, so an idempotent `{provider: "openrouter", ...}`
    // payload against a live `(openrouter, Y)` default would budget-check
    // against `(openrouter, X)` (the `[llm.providers.openrouter].model`
    // entry) while the commit path kept `Y`. False-positive rejections
    // followed whenever X had a tighter cap than Y. Gating the overlay on
    // the same `body.provider != live.provider` predicate that the commit
    // path uses restores symmetry.
    //
    // The compat check at lines ~879+ enforces wire compatibility
    // separately; here we only care about feeding the right (provider,
    // model) pair into the budget validator.
    let entry_model_for_candidate = body
        .provider
        .as_deref()
        .filter(|p| !p.is_empty())
        .filter(|p| *p != live_default_provider.as_str())
        .and_then(|p| state.llm_config.providers.get(p))
        .and_then(|e| e.model.clone());
    let candidate_model = body
        .model
        .as_deref()
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .or(entry_model_for_candidate)
        .unwrap_or(live_default.model);
    match validate_patch_budget(&state, &body, &candidate_provider, &candidate_model) {
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

        if ctx_patch.summary_model.is_some() || ctx_patch.summary_provider.is_some() {
            // Summary provider/model is one policy-owned pair. Compute the
            // complete post-PATCH pair, validate once, and only then commit.
            // commit either field so partial configuration cannot leak through.
            let next_summary_model = match ctx_patch.summary_model.as_ref() {
                Some(value) => Some(value.as_str()),
                None => ctx.summary_model.as_deref(),
            };
            let next_summary_provider = match ctx_patch.summary_provider.as_ref() {
                Some(value) => Some(value.as_str()),
                None => ctx.summary_provider.as_deref(),
            };

            match crate::configuration::validate_summary_pair(
                next_summary_provider,
                next_summary_model,
                &state.llm_config.providers,
                &state.secrets.read(),
            ) {
                Ok(pair) => {
                    if ctx_patch.summary_model.is_some() {
                        ctx.summary_model = pair.model;
                    }
                    if ctx_patch.summary_provider.is_some() {
                        ctx.summary_provider = pair.provider;
                    }
                }
                Err(error) => errors.push(error.to_string()),
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
    //
    // ── Shape: validate the whole section, then commit it (#1275) ───────
    //
    // This block used to commit every named field first and cross-validate
    // afterwards, and its "revert" wrote `ctx_max` — the context window —
    // rather than the pre-PATCH value. Two things followed. A rejected
    // PATCH left `max_context_tokens` at a number the operator never sent;
    // and because the check ran for *any* `body.session` rather than only
    // for bodies naming the field, `{"session": {"max_messages": 50}}`
    // could return `422` *and* move `max_context_tokens` once
    // `context.max_input_tokens` had been raised past it — an error about
    // one knob that silently changed another.
    //
    // Same treatment as the server-default pair block below (#1271): two
    // phases that do not overlap.
    //
    //   Phase 1 — validate. Reads live state; writes only
    //             `session_errors`. Mutates nothing shared.
    //   Phase 2 — commit. Runs only when `session_errors` is empty and
    //             contains no rejection path of its own, so no field can
    //             land ahead of the gate that should have stopped it and
    //             there is nothing to "revert".
    //
    // The section is all-or-nothing like the pair: a rejected `session`
    // body leaves every session field at its pre-PATCH value. An unrelated
    // section failing in the same body still leaves this one committed but
    // unpersisted, which is the documented `status: "partial"` contract
    // every section follows.
    if let Some(sess_patch) = &body.session {
        let mut session_errors: Vec<String> = Vec::new();

        // ── Phase 1: cross-section invariant ───────────────────────────
        //
        // Session storage must hold at least one full context window.
        // Judged on the would-be post-PATCH pair, never on committed
        // state: the session half is the body's value when it named one
        // and the live value otherwise, and `ctx_max` is the post-PATCH
        // context window (the context block above is itself
        // validate-then-commit, so what it committed is what runs will
        // use).
        //
        // Scoped to bodies that actually name one half of the invariant,
        // mirroring the `compact_trigger_pct` / `compact_retain_pct` gap
        // check above. A body naming neither half cannot move the two
        // values relative to each other, so judging it can only produce
        // an error about fields the operator did not send — which is how
        // the old shape came to reject a lone `max_messages` change.
        //
        // This does tolerate a live pair that already violates, and that
        // is deliberate: `session.max_context_tokens` has no runtime
        // consumer. It is written here, read by `GET /settings` and
        // `persist_settings`, and nowhere else — `SessionManager` takes a
        // by-value clone at boot and reads only `idle_timeout_secs` from
        // it. Nothing budgets against the pair, so the tolerated state is
        // inert, while a 422 naming two fields the body never sent is a
        // real rejection. The gap is real but belongs one level up: this
        // is a cross-section rule living inside one section's block, so a
        // context-only body escapes it entirely (tracked separately).
        let live_max_context_tokens = state.session_config.read().max_context_tokens;
        let next_max_context_tokens = sess_patch
            .max_context_tokens
            .unwrap_or(live_max_context_tokens);
        let names_either_half = sess_patch.max_context_tokens.is_some()
            || body
                .context
                .as_ref()
                .is_some_and(|ctx_patch| ctx_patch.max_input_tokens.is_some());
        if names_either_half {
            let ctx_max = state.agent_config.read().context_config.max_input_tokens;
            if next_max_context_tokens < ctx_max {
                session_errors.push(format!(
                    "session.max_context_tokens ({next_max_context_tokens}) must be >= \
                     context.max_input_tokens ({ctx_max})",
                ));
            }
        }

        // ── Phase 2: commit ────────────────────────────────────────────
        //
        // Reached only when the whole section validated. Nothing here can
        // reject. Each field is still written only when the body named it,
        // so no unnamed field can move under any outcome.
        let session_ok = session_errors.is_empty();
        errors.append(&mut session_errors);
        if session_ok {
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

            info!(
                max_messages = sess.max_messages,
                max_context_tokens = sess.max_context_tokens,
                idle_timeout_secs = sess.idle_timeout_secs,
                auto_archive = sess.auto_archive,
                "Updated session config via PATCH /settings"
            );
        }
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
    // `configuration::resolve_agent_config` and
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

    // ── Server-default LLM model / provider ──────────────────────────────
    //
    // These are the top-level `llm.model` / `llm.provider` knobs from
    // `alms.toml` — the values agents inherit when they have no
    // per-agent override. PATCH mutations land on the live
    // `server_llm_default` lock, rebuild the shared `state.llm` client
    // from it (#1148), and are persisted into `settings.json` for
    // restart survival. The rebuild is what makes the pair take effect
    // on the next run without a daemon restart; `state.llm_config`'s
    // by-value `provider` / `default_model` fields are left stale on
    // purpose and are no longer read by any live path.
    //
    // #1148: the response therefore no longer carries `restart_required`
    // for these fields. That flag is a promise about when a value becomes
    // real, and telling an operator "no restart needed" while the run
    // path still used the old value would be the worse of the two
    // failure modes — so it is dropped only because the rebuild below
    // genuinely lands.
    //
    // ── Shape: validate the whole pair, then commit it ──────────────────
    //
    // Tim review on #1148, third pass. This block used to interleave
    // gates and commits: the provider half committed, then the model half
    // was validated, then it committed. Four separate holes were found
    // and patched one at a time inside that shape, each one an input
    // whose rejection arrived after part of the pair had already gone
    // live. The last was `{"provider": <valid>, "model": ""}`, which
    // committed the provider *and* the target entry's
    // `[llm.providers.<name>].model` — a value the operator never named —
    // behind a `422`, with `persist_settings` skipped, so the daemon
    // moved and a restart silently moved it back.
    //
    // Four point-fixes on one invariant means the shape is wrong, not
    // that the inputs are unusual. So the block is now two phases that do
    // not overlap:
    //
    //   Phase 1 — validate. Reads state; writes only `pair_errors` and
    //             the two `*_to_commit` locals. Mutates nothing shared.
    //   Phase 2 — commit. Runs only when `pair_errors` is empty, and
    //             contains no rejection path of its own.
    //
    // The all-or-nothing property `docs/api.md` § 10.2 promises is now
    // structural rather than a conjunction of flags: there is exactly one
    // gate between the phases, so a rule added anywhere in Phase 1 cannot
    // leak a half-committed pair regardless of where it lands. Note this
    // is about the *pair*: an unrelated section failing in the same body
    // still leaves the committed pair live but unpersisted, which is the
    // documented `status: "partial"` contract every section follows.
    let mut llm_default_changed = false;
    let live_pair = state.server_llm_default.read().clone();
    let mut pair_errors: Vec<String> = Vec::new();

    // ── Phase 1a: each named half must be usable on its own ─────────────
    //
    // Both halves are checked before either is judged against the other,
    // so a body naming one bad half never commits the good one. Rejection
    // yields `None`, and the coherence gate below is skipped entirely
    // while `pair_errors` is non-empty — that is what carries "neither
    // half lands" through to Phase 2.
    //
    // Empty strings are rejected rather than read as "clear this field":
    // there is no runnable state with an empty provider or an empty
    // model, and since #1148 the pair reaches the live client
    // immediately, so a cleared field breaks every subsequent run instead
    // of merely persisting a bad row.
    let named_provider: Option<&str> = match body.provider.as_deref() {
        None => None,
        Some("") => {
            pair_errors.push(
                "provider: empty string not accepted — set a concrete provider name or omit the field"
                    .into(),
            );
            None
        }
        Some(provider) if !state.llm_config.providers.contains_key(provider) => {
            let mut known: Vec<&str> = state
                .llm_config
                .providers
                .keys()
                .map(|s| s.as_str())
                .collect();
            known.sort();
            pair_errors.push(format!(
                "provider '{provider}' is not configured — known providers: {known:?}. \
                 Add an `[llm.providers.{provider}]` entry to alms.toml first."
            ));
            None
        }
        Some(provider) => Some(provider),
    };
    let named_model: Option<&str> = match body.model.as_deref() {
        None => None,
        Some("") => {
            pair_errors.push(
                "model: empty string not accepted — set a concrete model id or omit the field"
                    .into(),
            );
            None
        }
        Some(model) => Some(model),
    };

    // ── Phase 1b: coherence of the would-be post-patch pair ─────────────
    //
    // Reached only when every named half survived 1a. Skipping it
    // otherwise is load-bearing in both directions:
    //
    //   * A body whose provider was rejected has no post-patch pair to
    //     judge, and judging its model against the provider that *stays*
    //     in force is what let `{"provider": "anthropic", "model":
    //     "openai/gpt-4o-mini"}` commit its model half against a live
    //     `openrouter` — whose `OpenAiCompatible` kind accepts every
    //     namespace — on the way out to a 422.
    //   * It keeps "one mistake, one error": the 1a rejection already
    //     explains why nothing moved. A body with two bad halves still
    //     gets two errors, both from 1a — two mistakes, two errors.
    let mut provider_to_commit: Option<String> = None;
    let mut model_to_commit: Option<String> = None;
    if pair_errors.is_empty() {
        match named_provider {
            // Provider named (a real switch, or an idempotent restatement
            // of the live one). The post-patch model is the body's, else
            // the new provider's `[llm.providers.<name>].model` entry,
            // else the live server default — mirroring the runtime's
            // `resolve_effective_provider_and_model` (#860 / #863 / #942)
            // — and it has to wire-match the new provider's kind.
            //
            // Codex follow-up on #1081 (P1): a provider-only PATCH used to
            // keep the old default model — e.g. `moonshotai/kimi-k2.6` on
            // OpenRouter — and persist a post-restart pair that fails on
            // the wire, since Anthropic 4xxs an OpenRouter model slug.
            // Rejecting an incompatible candidate here is one half of the
            // fix; committing the approved candidate below is the other.
            Some(provider) => {
                // Tim review on #1148: every candidate is empty-filtered,
                // not just the body's. `model_belongs_to_kind("",
                // OpenAiCompatible)` is `true`, so an unfiltered empty
                // candidate would sail through and commit `model = ""` —
                // and now that the pair is live, `with_model("")` clears
                // the client's model and every subsequent run fails with a
                // missing-model error. The reject arm below is where "no
                // candidate model at all" is meant to land; filtering here
                // is what routes an empty string to it. A daemon booted
                // with an empty `[llm] model` plus a provider-only PATCH
                // to an entry with no `model` is the reachable shape.
                //
                // Tim review on #1271: and it is consulted only when the
                // PATCH actually *switches* provider, mirroring the commit
                // guard below and the budget overlay's
                // `entry_model_for_candidate`. Resolving it
                // unconditionally judged a model that the same body would
                // then decline to commit: against a live
                // `(anthropic, claude-sonnet-4-6)` with a misconfigured
                // `[llm.providers.anthropic].model = "openai/gpt-4o-mini"`,
                // the idempotent body `{"provider": "anthropic"}` drew a
                // `422` naming a model the operator never sent and that
                // would never have gone live. Nothing leaked — the gate
                // rejected, so nothing moved — but a no-op PATCH failing
                // on a third value is not an error an operator can act on.
                // With the filter, `candidate_model` is exactly the
                // post-patch model in every arm, which is what this block
                // claims to validate.
                let entry_model = state
                    .llm_config
                    .providers
                    .get(provider)
                    .and_then(|e| e.model.clone())
                    .filter(|m| !m.is_empty())
                    .filter(|_| provider != live_pair.provider);
                let live_model = Some(live_pair.model.as_str()).filter(|m| !m.is_empty());
                let candidate_model: Option<&str> =
                    named_model.or(entry_model.as_deref()).or(live_model);
                let new_kind = crate::configuration::provider_kind_for_name(
                    provider,
                    &state.llm_config.providers,
                );
                match candidate_model {
                    Some(model) if crate::configuration::model_belongs_to_kind(model, new_kind) => {
                        // Codex follow-up on #1081 (P1 #B): when compat
                        // passes via the new provider's entry-level model
                        // rather than the body's, that model must be
                        // committed too. Pre-fix only `provider` landed,
                        // `server_llm_default.model` kept the old
                        // cross-namespace value, `persist_settings`
                        // serialised the bad pair, and the boot path
                        // reapplied both fields verbatim — so every run
                        // after the restart sent an OpenRouter slug to
                        // Anthropic's wire.
                        //
                        // Codex follow-up on #1081 (P2 #4): only when the
                        // PATCH actually *changes* the provider. An
                        // idempotent `{provider: "anthropic"}` against a
                        // live `(anthropic, X)` default is a no-op as far
                        // as the operator is concerned; silently replacing
                        // `model` with the entry's is an unexpected
                        // behaviour change for a payload that requested no
                        // model swap.
                        if named_model.is_none() && provider != live_pair.provider {
                            // Commit exactly the candidate this gate
                            // approved. `named_model` is `None` in this
                            // arm, so `candidate_model` *is* that
                            // candidate (entry model > live default) and
                            // it carries the empty-filter with it.
                            model_to_commit = candidate_model.map(str::to_string);
                        }
                        provider_to_commit = Some(provider.to_string());
                    }
                    // Either no candidate model at all (the entry has no
                    // `model` and the live default is empty) or one that
                    // the new provider's wire cannot speak. Both make the
                    // post-patch state unrunnable, so both reject.
                    _ => {
                        let candidate_display = candidate_model.unwrap_or("<unset>");
                        pair_errors.push(format!(
                            "INCOMPATIBLE_MODEL_FOR_PROVIDER: switching server-default provider to \
                             '{provider}' but the post-patch model '{candidate_display}' does not belong \
                             to that provider's wire kind ({new_kind:?}). Supply a compatible `model` \
                             in the same PATCH body, or set `[llm.providers.{provider}].model` in \
                             alms.toml before switching."
                        ));
                    }
                }
            }
            // Model-only arm: the provider that stays in force is the one
            // that has to speak the new model.
            None => {
                if let Some(model) = named_model
                    && let Some(rejection) =
                        reject_model_incompatible_with_provider(&state, &live_pair.provider, model)
                {
                    pair_errors.push(rejection);
                }
            }
        }
        // A body-supplied model always wins over the entry-model fallback
        // above, which only fires when the body omitted one.
        //
        // Deliberately NOT re-checking `pair_errors` here. Phase 1 stages;
        // Phase 2 decides. Re-testing the same condition at every staging
        // site is how the old shape drifted into a stack of flags, and it
        // also makes the Phase 2 gate unkillable by mutation: with that
        // guard present, `let pair_ok = true;` survives the entire suite,
        // because nothing is ever staged when the pair failed. Without it,
        // an incompatible body model *is* staged and only the gate stops
        // it — so `patch_model_incompatible_with_live_provider_is_rejected`
        // and `rejected_provider_switch_does_not_commit_the_model_half`
        // become real kills for the gate itself. A gate that cannot fail
        // is a gate nobody can trust.
        if let Some(model) = named_model {
            model_to_commit = Some(model.to_string());
        }
    }

    // ── Phase 2: commit ─────────────────────────────────────────────────
    //
    // Reached only when the whole pair validated. Nothing here can
    // reject, so no commit can precede its own gate — that is the
    // property the split buys, and the reason a fifth instance of this
    // bug would have to be introduced deliberately rather than by adding
    // one more rule in the wrong place.
    let pair_ok = pair_errors.is_empty();
    errors.append(&mut pair_errors);
    if pair_ok && (provider_to_commit.is_some() || model_to_commit.is_some()) {
        // One `write()` for both halves (Tim review on #1271). Two
        // acquisitions left a window between them in which a concurrent
        // `GET /settings` could read the new provider beside the old
        // model — a torn pair, and a cross-namespace one on exactly the
        // switches Phase 1b exists to vet. The lock is taken only when a
        // half is actually staged, so a body naming neither field still
        // never touches it.
        let mut snap = state.server_llm_default.write();
        // Codex follow-up on #1081 (P2 #4): only flip
        // `llm_default_changed` when the provider actually changes. The
        // rebuild below re-resolves the API key, so churning on an
        // idempotent PATCH would emit spurious key-resolution log lines
        // for a request that changed nothing — and would surface a
        // "server default updated" INFO for a genuine no-op.
        if let Some(provider) = provider_to_commit
            && snap.provider != provider
        {
            snap.provider = provider;
            llm_default_changed = true;
        }
        // Tim follow-up on #1081: the same no-op guard on the model half,
        // so an idempotent `{model: <same-as-current>}` PATCH neither
        // rebuilds the live client nor logs an update.
        if let Some(model) = model_to_commit
            && snap.model != model
        {
            snap.model = model;
            llm_default_changed = true;
        }
    }
    // #1148: make the committed pair live.
    //
    // Gated on `llm_default_changed` alone, NOT on `errors.is_empty()`.
    // Every value committed to `server_llm_default` above has already
    // passed the provider-exists and model/provider-coherence gates, so
    // the pair is safe to run on. Skipping the rebuild when some
    // *unrelated* field in the same body failed (a bad
    // `context.strategy`, say) would leave `server_llm_default` — what
    // `GET /settings` reports — describing a pair the run path is not
    // using, which is the exact divergence this issue removes. Applying
    // live mutations inline while gating only persistence on a clean
    // validation is the documented `status: "partial"` contract the
    // context / session / tools / llm sections already follow.
    //
    // The no-op guard still matters: rebuilding re-resolves the API key,
    // so churning on an idempotent PATCH would emit spurious key-
    // resolution log lines for a request that changed nothing.
    if llm_default_changed {
        let snap = state.server_llm_default.read().clone();
        state.refresh_llm_from_server_default();
        info!(
            model = %snap.model,
            provider = %snap.provider,
            "Updated server-default LLM model/provider via PATCH /settings — \
             effective on the next run; in-flight runs keep the client they \
             resolved at start"
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
        // #1148: no `restart_required` / `restart_reason` on this wire any
        // more. Every mutable section of `PATCH /settings` — including the
        // server-default `(model, provider)` pair — is now live for the
        // next run, so the response is the same plain `{"status": "ok"}`
        // the other sections have always returned.
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

/// Reject a **model-only** `PATCH /settings` whose new model cannot be
/// spoken on `provider` — the one that will still be in force after the
/// patch (#1148).
///
/// The provider-switch arm in `patch_settings` already validates the
/// post-patch model against the *new* provider's wire kind. Its mirror
/// image — patching the model while leaving the provider alone — had no
/// such check, because until #1148 the pair only became real on the next
/// restart and the coherence question was deferred to boot. Now that the
/// pair goes live immediately, an incoherent model-only PATCH would break
/// every subsequent run the instant it returned `200`, which is strictly
/// worse than the restart it replaces. So the same
/// `configuration::model_belongs_to_kind` gate the runtime uses for
/// per-agent provider switches (#942 / #863) runs here too.
///
/// **`provider` is an explicit argument on purpose.** An earlier shape
/// read it out of `state.server_llm_default` and relied on the caller
/// having already committed the provider half, so that the read returned
/// the post-patch value. That made a silent ordering constraint
/// load-bearing: hoisting the check above the commit made an ordinary
/// switch away from a strict kind (`{"provider": "openrouter", "model":
/// "z-ai/glm-5.2"}` against a live `anthropic`) start getting rejected
/// against the provider it was leaving. The pair block is now
/// validate-then-commit with nothing committed at this point, so the
/// provider to judge against is passed in — and this function is only
/// ever called on the model-only arm, where that provider is simply the
/// live one.
///
/// The body that must never reach this function is a **rejected** pair,
/// and the caller — not this function — is what keeps it away:
/// `{"provider": "anthropic", "model": "openai/gpt-4o-mini"}` against a
/// live `openrouter` default fails the provider arm, leaving `openrouter`
/// in force, whose `OpenAiCompatible` kind would happily accept the model
/// half on the way out to a 422 (Tim review on #1148). Phase 1b's
/// `pair_errors.is_empty()` guard is what enforces that.
///
/// `OpenAiCompatible` is permissive by construction (see
/// [`crate::configuration::model_belongs_to_kind`]), so the common
/// OpenAI / OpenRouter / DeepSeek deployments are unaffected — this only
/// fires for a strict kind (`Anthropic`, `Gemini`) receiving a model from
/// another namespace, e.g. `{"model": "gpt-4o"}` while the server default
/// provider is `anthropic`.
fn reject_model_incompatible_with_provider(
    state: &AppState,
    provider: &str,
    model: &str,
) -> Option<String> {
    let kind = crate::configuration::provider_kind_for_name(provider, &state.llm_config.providers);
    if crate::configuration::model_belongs_to_kind(model, kind) {
        return None;
    }
    Some(format!(
        "INCOMPATIBLE_MODEL_FOR_PROVIDER: model '{model}' does not belong to the \
         wire kind ({kind:?}) of the server-default provider '{provider}'. Since \
         this pair takes effect on the next run, committing it would fail every \
         subsequent default-agent run. Supply a matching `provider` in the same \
         PATCH body, or pick a model from the '{provider}' namespace."
    ))
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
    candidate_provider: &str,
    candidate_model: &str,
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
    //
    // Codex follow-up on #1081 (P1): `candidate_provider` / `candidate_model`
    // reflect the would-be POST-patch server default, not the boot-time
    // `state.llm_config.*` clone. Without this overlay, a PATCH that
    // simultaneously raised `context.max_input_tokens` and switched the
    // default to a stricter provider/model could return 200 and persist
    // an over-budget pair that later rejected every run after restart.
    let server_provider = candidate_provider;
    let server_model = candidate_model;
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
    // `configuration::resolve_agent_config` would resolve to. The shared
    // `configuration::resolve_effective_provider_and_model` helper is the
    // single source of truth for the resolution rules:
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
            match crate::configuration::resolve_effective_provider_and_model(
                record.provider.as_deref(),
                record.model.as_deref(),
                server_provider,
                server_model,
                &state.llm_config.providers,
            ) {
                Ok(pair) => pair,
                Err(crate::configuration::ResolveEffectiveModelError::MissingModelAfterProviderSwitch {
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
    /// Persisted summary model override.
    /// `None` (absent field) means "fall through to TOML / env / code
    /// default". `Some(non-empty)` applies as an override. `Some("")` is
    /// the **explicit-clear sentinel** (PR #1194): the operator cleared
    /// the summary pair via PATCH and the opt-out must survive restarts —
    /// without it, the compiled `Some(...)` default pair (#1191) would
    /// silently resurrect on the next boot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_model: Option<String>,
    /// Persisted summary provider override (#866).
    /// `None` means "fall through to TOML / env / code default".
    /// `Some(provider)` re-targets the summary task at that provider on
    /// every restart. `Some("")` is the explicit-clear sentinel — see
    /// [`Self::summary_model`].
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
        // Summary pair (#1191 / PR #1194): an empty (or whitespace-only)
        // string is the persisted EXPLICIT-CLEAR sentinel — the operator
        // cleared the pair via PATCH to restore inherit-agent-model
        // behaviour, and that opt-out must override the compiled
        // `Some(...)` default pair on reboot. Map the sentinel back to
        // `None`; apply `Some(non-empty)` as a normal override; leave a
        // genuinely absent field (`None`) untouched so the compiled /
        // TOML value still wins for non-overridden deployments. Mirrors
        // the TOML clear shape (`summary_model = ""` +
        // `summary_provider = ""`), whose deserializer trims the same way.
        if let Some(ref v) = self.summary_model {
            ctx.summary_model = (!v.trim().is_empty()).then(|| v.clone());
        }
        if let Some(ref v) = self.summary_provider {
            ctx.summary_provider = (!v.trim().is_empty()).then(|| v.clone());
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
    /// Server-default LLM model from a previous PATCH /settings.
    /// `Some(value)` overrides the boot-time `llm.model` from TOML / env
    /// on the next daemon start; `None` leaves it alone. Persisted
    /// top-level (not nested) because it shadows the top-level
    /// `llm.model` knob exposed by `alms.toml`, not any per-provider
    /// reasoning knob.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Server-default LLM provider from a previous PATCH /settings.
    /// Shadows `llm.provider` from TOML / env on the next daemon start.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
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

    // #1191 / PR #1194 (Codex P2, both rounds): the summary pair is
    // persisted only when it DIFFERS from the compiled default pair.
    //
    // - Pair == compiled default → persist BOTH fields as `None` (absent
    //   on the wire via `skip_serializing_if`). `persist_settings` runs
    //   after EVERY successful PATCH, so unconditionally snapshotting the
    //   pair would pin the compiled default into `settings.json` the
    //   first time an operator PATCHes an *unrelated* knob — and because
    //   `settings.json` overlays TOML on boot, that pin would clobber the
    //   documented `alms.toml` `summary_model = ""` /
    //   `summary_provider = ""` opt-out on every later restart.
    // - Pair != compiled default → the operator made a choice; persist it
    //   explicitly for BOTH fields: `Some(value)` for an override,
    //   `Some("")` for a cleared field (the explicit-clear sentinel — a
    //   `None` would serialize as absent, meaning "not overridden", and
    //   the compiled default pair would resurrect on the next boot). A
    //   cleared pair (`None`/`None`) never equals the compiled `Some`
    //   pair, so the durable-clear path always takes this branch.
    let (persist_summary_model, persist_summary_provider) =
        if alms_core::config::ContextConfig::is_compiled_default_summary_pair(
            ctx.summary_provider.as_deref(),
            ctx.summary_model.as_deref(),
        ) {
            (None, None)
        } else {
            (
                Some(ctx.summary_model.clone().unwrap_or_default()),
                Some(ctx.summary_provider.clone().unwrap_or_default()),
            )
        };

    let persisted = PersistedSettings {
        // Only persist the fields that PATCH /settings exposes for context.
        context: Some(PersistedContextOverrides {
            strategy: Some(ctx.strategy.clone()),
            max_input_tokens: Some(ctx.max_input_tokens),
            // #869: persist the threshold-based knobs.
            compact_trigger_pct: Some(ctx.compact_trigger_pct),
            compact_retain_pct: Some(ctx.compact_retain_pct),
            summary_model: persist_summary_model,
            summary_provider: persist_summary_provider,
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
        // Server-default `(model, provider)`. Snapshotted from the live
        // `server_llm_default` lock the PATCH handler mutates. Persisting
        // unconditionally on every successful PATCH ensures the operator's
        // last intent survives restart, mirroring the context / session /
        // tools / llm-family pattern. On restart, `AppState::new` reads
        // these back and re-applies them to `llm_config` + the `LlmClient`
        // snapshot before the AppState is published to handlers.
        model: Some(state.server_llm_default.read().model.clone()),
        provider: Some(state.server_llm_default.read().provider.clone()),
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
            ctx.summary_provider,
            Some("openrouter".into()),
            "default ContextConfig ships the explicit summary pair (#1191)"
        );

        // Setting it on the override applies it — use a provider that
        // differs from the compiled default so the mutation is observable.
        let overrides = PersistedContextOverrides {
            summary_provider: Some("anthropic".into()),
            ..Default::default()
        };
        overrides.apply_to(&mut ctx);
        assert_eq!(
            ctx.summary_provider,
            Some("anthropic".into()),
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
            Some("anthropic".into()),
            "PersistedContextOverrides with None summary_provider must not clear the live value"
        );
    }

    /// #1191 / PR #1194 (Codex P2): `Some("")` on the persisted summary
    /// pair is the explicit-clear sentinel — `apply_to` must map it to
    /// `None` so the operator's opt-out overrides the compiled `Some(...)`
    /// default pair on reboot instead of being resurrected by it.
    /// Whitespace-only values clear too, matching the TOML deserializer
    /// and the PATCH handler.
    #[test]
    fn context_overrides_empty_string_summary_pair_clears_compiled_default() {
        for sentinel in ["", "  \t"] {
            let mut ctx = alms_core::config::ContextConfig::default();
            assert!(
                ctx.summary_model.is_some() && ctx.summary_provider.is_some(),
                "precondition: the compiled default ships an explicit pair (#1191)"
            );
            let overrides = PersistedContextOverrides {
                summary_model: Some(sentinel.into()),
                summary_provider: Some(sentinel.into()),
                ..Default::default()
            };
            overrides.apply_to(&mut ctx);
            assert!(
                ctx.summary_model.is_none(),
                "sentinel {sentinel:?} must clear summary_model, got {:?}",
                ctx.summary_model
            );
            assert!(
                ctx.summary_provider.is_none(),
                "sentinel {sentinel:?} must clear summary_provider, got {:?}",
                ctx.summary_provider
            );
        }
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
            model: None,
            provider: None,
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
            model: None,
            provider: None,
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
            model: None,
            provider: None,
        };
        let json = serde_json::to_string_pretty(&persisted).unwrap();
        assert!(
            !json.contains("\"llm\""),
            "JSON should not contain llm key when it is None: {json}"
        );
        // Top-level model/provider follow the same `skip_serializing_if`
        // pattern — absent on the wire when `None`.
        assert!(
            !json.contains("\"model\""),
            "JSON should not contain model key when it is None: {json}"
        );
        assert!(
            !json.contains("\"provider\""),
            "JSON should not contain provider key when it is None: {json}"
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
            model: None,
            provider: None,
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
        let (trigger_tx, _trigger_rx) = tokio::sync::mpsc::channel(8);
        let (dm_event_tx, _dm_event_rx) = tokio::sync::mpsc::channel(8);
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
    // PATCH /settings — top-level `model` / `provider` (restoration of
    // the missing server-default-model UI surface, post-#941; made
    // live-mutable in #1148). Tests pin: the happy-path mutates
    // `server_llm_default` AND rebuilds the shared `state.llm` client the
    // run path reads, the empty-string / unknown-provider / incoherent-
    // pair rejections fire, and the wire no longer carries
    // `restart_required`.
    // ==================================================================

    #[tokio::test]
    async fn patch_top_level_model_provider_updates_server_llm_default() {
        let state = settings_test_app_state();
        let tmp = tempfile::tempdir().unwrap();
        let mut state = state;
        state.data_dir = tmp.path().to_path_buf();

        // `settings_test_app_state()` builds an `AppState` from
        // `GatewayConfig::default()` which uses `LlmConfig::default()` —
        // and that default constructs an EMPTY `providers` map. The
        // gateway's production boot path eventually fills the map via
        // `ensure_builtin_providers()` on the unified `AlmsConfig`, but
        // that step is not exercised by the bare test fixture, so the
        // PATCH `provider` validator (which checks
        // `state.llm_config.providers.contains_key(provider)`) rejects
        // `openrouter` as unknown. Seed the entry inline — same shape
        // `settings_test_app_state_with_openrouter()` uses below.
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
        // `AppState::new` seeds `server_llm_default` from
        // `LlmConfig::default()`, which post-#1191 carries
        // `(openrouter, z-ai/glm-5.2)` — i.e. the same pair this
        // test PATCHes. Force the baseline to a different pair so the
        // "PATCH actually mutated something" sanity check below is
        // meaningful instead of vacuously true.
        {
            let mut snap = state.server_llm_default.write();
            snap.model = "openai/gpt-4o-mini".into();
            snap.provider = "openrouter".into();
        }

        // Baseline visible to the rest of the test.
        let before = state.server_llm_default.read().clone();
        // PATCH both fields. With `openrouter` seeded above the provider
        // validator passes and both knobs land on `server_llm_default`.
        let body = PatchSettingsRequest {
            model: Some("z-ai/glm-5.2".into()),
            provider: Some("openrouter".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        let live = state.server_llm_default.read().clone();
        assert_eq!(live.model, "z-ai/glm-5.2");
        assert_eq!(live.provider, "openrouter");
        // The baseline should differ from the post-PATCH snapshot in at
        // least one knob — sanity check that the mutation actually
        // landed rather than the test echoing the seeded value.
        assert!(
            live.model != before.model || live.provider != before.provider,
            "PATCH should have mutated at least one of model/provider"
        );

        // settings.json on disk carries the new pair so a daemon restart
        // re-applies it on the next boot.
        let on_disk = std::fs::read_to_string(settings_path(&state.data_dir)).unwrap();
        let parsed: PersistedSettings = serde_json::from_str(&on_disk).unwrap();
        assert_eq!(parsed.model.as_deref(), Some("z-ai/glm-5.2"));
        assert_eq!(parsed.provider.as_deref(), Some("openrouter"));
    }

    /// #1148: the server-default pair is live for the next run, so the
    /// PATCH response must NOT claim a restart is needed.
    ///
    /// Pre-#1148 this same PATCH returned `restart_required: true` plus a
    /// `restart_reason`, and the UI turned that into a yellow banner —
    /// the exact message the issue reporter hit. Both keys must now be
    /// absent, and the live client must already carry the new model, so
    /// the "no restart needed" claim on the wire is backed by the run
    /// path rather than being an optimistic label.
    #[tokio::test]
    async fn patch_top_level_model_does_not_flag_restart_required() {
        use axum::body::to_bytes;

        let state = settings_test_app_state();
        let tmp = tempfile::tempdir().unwrap();
        let mut state = state;
        state.data_dir = tmp.path().to_path_buf();

        // `settings_test_app_state` boots `AppState::new`, which seeds
        // `server_llm_default.model = "z-ai/glm-5.2"` (the post-#1191
        // compiled default). PATCHing the same value back is a no-op and
        // would exercise nothing — pick a model that differs from the
        // live default. Both are OpenRouter-namespace slugs so the
        // model/provider coherence gate stays satisfied.
        let body = PatchSettingsRequest {
            model: Some("moonshotai/kimi-k2.5".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body_bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(json["status"], "ok");
        assert!(
            json.get("restart_required").is_none(),
            "#1148: the server-default pair is live for the next run — the \
             wire must not carry `restart_required` any more. Got: {json}"
        );
        assert!(
            json.get("restart_reason").is_none(),
            "#1148: `restart_reason` must be gone alongside `restart_required`. \
             Got: {json}"
        );
        // The claim on the wire has to be true: the shared client the run
        // path reads must already carry the patched model.
        assert_eq!(
            state.llm.read().default_model(),
            "moonshotai/kimi-k2.5",
            "dropping `restart_required` is only honest if the live client \
             was actually rebuilt from the committed pair"
        );
    }

    // ==================================================================
    // #1148 — the server-default `(model, provider)` pair is LIVE.
    //
    // The pair used to be persistence-only: PATCH rewrote settings.json,
    // returned `restart_required: true`, and the run path kept sending on
    // the boot-time client until the daemon was restarted. The wiring gap
    // was that `state.llm` was a by-value clone in `AppState` rather than
    // a shared handle, even though `resolve_agent_config` already rebuilds
    // the effective client from it on every single run.
    //
    // These tests pin the two halves of the new contract:
    //   1. a committed pair reaches `state.llm` — the base every run path
    //      layers per-agent overrides onto;
    //   2. nothing incoherent can reach it, because a pair that no run can
    //      speak is a worse outcome than the restart it replaced.
    // ==================================================================

    /// Build an `AppState` with both an `openrouter` and an `anthropic`
    /// provider entry so provider switches in either direction are
    /// validatable, plus keys for both so `apply_provider` does not clear
    /// credentials on the way through.
    fn settings_test_app_state_with_two_providers() -> crate::server::AppState {
        let mut state = settings_test_app_state();
        state.llm_config.providers.insert(
            "openrouter".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::OpenAiCompatible,
                base_url: "https://openrouter.ai/api/v1".into(),
                api_key_env: None,
                api_key: Some("sk-or-test".into()),
                model: None,
                auth_scheme: alms_core::config::AuthScheme::Bearer,
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        state.llm_config.providers.insert(
            "anthropic".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: Some("sk-ant-test".into()),
                // No entry-level model: the operator is expected to send a
                // `model` alongside a provider switch, which is what the
                // coherence gate enforces.
                model: None,
                auth_scheme: alms_core::config::AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        // The live client starts on the same providers map, so
        // `apply_provider` can find the entries above at PATCH time.
        {
            let mut llm = state.llm.write();
            *llm = alms_runtime::LlmClient::new(alms_runtime::LlmConfig {
                provider: "openrouter".into(),
                base_url: "https://openrouter.ai/api/v1".into(),
                api_key: "sk-or-test".into(),
                default_model: "z-ai/glm-5.2".into(),
                providers: state.llm_config.providers.clone(),
                ..alms_runtime::LlmConfig::default()
            })
            .unwrap();
        }
        {
            let mut snap = state.server_llm_default.write();
            snap.provider = "openrouter".into();
            snap.model = "z-ai/glm-5.2".into();
        }
        state
    }

    /// A full `(provider, model)` switch must retarget the live client's
    /// wire — provider name, wire kind, and base URL all re-derived from
    /// `[llm.providers.<new>]` — so the next run talks to the new endpoint
    /// with no daemon restart.
    #[tokio::test]
    async fn patch_provider_and_model_retargets_the_live_client_wire() {
        let mut state = settings_test_app_state_with_two_providers();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        // Baseline: whatever the run path would send on right now.
        assert_eq!(state.llm.read().provider(), "openrouter");
        assert_eq!(state.llm.read().base_url(), "https://openrouter.ai/api/v1");

        let body = PatchSettingsRequest {
            model: Some("claude-sonnet-4-6".into()),
            provider: Some("anthropic".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        let live = state.llm.read().clone();
        assert_eq!(
            live.provider(),
            "anthropic",
            "#1148: the client the run path reads must carry the patched provider"
        );
        assert_eq!(live.default_model(), "claude-sonnet-4-6");
        assert_eq!(
            live.provider_kind(),
            alms_core::config::ProviderKind::Anthropic,
            "the wire kind must be re-derived from the new provider entry, not \
             carried over — an OpenAI-shaped request body on Anthropic's wire \
             is the failure mode this guards"
        );
        assert_eq!(
            live.base_url(),
            "https://api.anthropic.com/v1",
            "base URL must come from `[llm.providers.anthropic]`"
        );

        // Restart survival is unchanged.
        let on_disk = std::fs::read_to_string(settings_path(&state.data_dir)).unwrap();
        let parsed: PersistedSettings = serde_json::from_str(&on_disk).unwrap();
        assert_eq!(parsed.provider.as_deref(), Some("anthropic"));
        assert_eq!(parsed.model.as_deref(), Some("claude-sonnet-4-6"));
    }

    /// `GET /settings` must report the base URL of the live client, not the
    /// boot-time `state.llm_config` clone. Reporting a stale endpoint next
    /// to a freshly patched provider name describes a wire nobody is
    /// talking to.
    #[tokio::test]
    async fn get_settings_reports_live_base_url_after_provider_switch() {
        use axum::body::to_bytes;

        let mut state = settings_test_app_state_with_two_providers();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        let body = PatchSettingsRequest {
            model: Some("claude-sonnet-4-6".into()),
            provider: Some("anthropic".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = get_settings(axum::extract::State(state.clone()))
            .await
            .into_response();
        let bytes = to_bytes(resp.into_body(), 512 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["provider"], "anthropic");
        assert_eq!(json["model"], "claude-sonnet-4-6");
        assert_eq!(
            json["base_url"], "https://api.anthropic.com/v1",
            "base_url must track the live provider — the boot-time \
             `llm_config.base_url` still says openrouter here"
        );
    }

    /// Switching away and back must land on exactly the client the first
    /// pair produced. `apply_provider` re-derives every provider-shaped
    /// field from the (immutable) `[llm.providers]` map rather than
    /// mutating incrementally, and `refresh_llm_from_server_default`
    /// relies on that: without it, repeated PATCHes would accumulate
    /// state (a cleared API key being the dangerous one) and the run path
    /// would drift away from the displayed pair.
    #[tokio::test]
    async fn repeated_provider_switches_are_idempotent_on_the_live_client() {
        let mut state = settings_test_app_state_with_two_providers();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        let baseline = state.llm.read().clone();

        for (model, provider) in [
            ("claude-sonnet-4-6", "anthropic"),
            ("z-ai/glm-5.2", "openrouter"),
        ] {
            let body = PatchSettingsRequest {
                model: Some(model.into()),
                provider: Some(provider.into()),
                ..Default::default()
            };
            let resp = patch_settings(
                axum::extract::State(state.clone()),
                Json(serde_json::to_value(&body).unwrap()),
            )
            .await
            .into_response();
            assert_eq!(resp.status(), StatusCode::OK, "PATCH to {provider} failed");
        }

        let round_tripped = state.llm.read().clone();
        assert_eq!(round_tripped.provider(), baseline.provider());
        assert_eq!(round_tripped.default_model(), baseline.default_model());
        assert_eq!(round_tripped.base_url(), baseline.base_url());
        assert_eq!(
            round_tripped.provider_kind(),
            baseline.provider_kind(),
            "switching away and back must reproduce the original client"
        );
        // `apply_provider` clears the outgoing provider's key on every
        // switch, so without re-resolution from `[llm.providers.<name>]`
        // a switch away and back would leave the client with empty
        // credentials and every subsequent run would fail with an opaque
        // 401. `LlmClient::api_key()` is `#[cfg(test)]`-gated to
        // `alms-runtime` so the exact value cannot be compared from here,
        // but `has_api_key()` answers the question that matters at this
        // call site — and unlike the runtime-side round-trip test it dies
        // if `refresh_llm_from_server_default` stops calling the resolving
        // setter.
        assert!(
            round_tripped.has_api_key(),
            "the round trip must leave the client able to authenticate"
        );
    }

    /// Coherence gate, model-only arm. The provider stays `anthropic`; the
    /// patched model belongs to the OpenAI namespace. Pre-#1148 this was
    /// merely a bad row in `settings.json` that bit on the next restart;
    /// now it would break every run the moment the PATCH returned, so it
    /// is rejected up front and NOTHING is committed — not the live
    /// client, not `server_llm_default`, not `settings.json`.
    #[tokio::test]
    async fn patch_model_incompatible_with_live_provider_is_rejected() {
        use axum::body::to_bytes;

        let mut state = settings_test_app_state_with_two_providers();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        // Move the live default onto a strict-kind provider first.
        let setup = PatchSettingsRequest {
            model: Some("claude-sonnet-4-6".into()),
            provider: Some("anthropic".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&setup).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let snapshot_before = std::fs::read_to_string(settings_path(&state.data_dir)).unwrap();

        // The typo: an OpenAI-namespace model with no accompanying provider.
        let body = PatchSettingsRequest {
            model: Some("gpt-4o".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let errors = json["errors"].as_array().expect("errors array");
        assert!(
            errors.iter().any(|e| e
                .as_str()
                .unwrap_or("")
                .contains("INCOMPATIBLE_MODEL_FOR_PROVIDER")),
            "expected INCOMPATIBLE_MODEL_FOR_PROVIDER, got {errors:?}"
        );

        assert_eq!(
            state.llm.read().default_model(),
            "claude-sonnet-4-6",
            "a rejected pair must never reach the client the run path reads"
        );
        assert_eq!(
            state.server_llm_default.read().model,
            "claude-sonnet-4-6",
            "the operator-facing default must not advertise a rejected model"
        );
        assert_eq!(
            std::fs::read_to_string(settings_path(&state.data_dir)).unwrap(),
            snapshot_before,
            "a rejected PATCH must be a no-op at the persistence layer"
        );
    }

    /// The permissive arm of the same gate: `OpenAiCompatible` providers
    /// accept every namespace (OpenRouter routes `anthropic/claude-*`,
    /// `google/gemini-*`, … on an OpenAI-shaped wire), so a model-only
    /// PATCH against one must still be accepted. Pins that the new
    /// rejection is narrow rather than a blanket prefix rule.
    #[tokio::test]
    async fn patch_model_only_accepted_on_openai_compatible_provider() {
        let mut state = settings_test_app_state_with_two_providers();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        let body = PatchSettingsRequest {
            model: Some("anthropic/claude-sonnet-4-6".into()),
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
            "OpenAiCompatible is a giant tent — vendor-prefixed slugs from \
             any namespace are legitimate on OpenRouter"
        );
        assert_eq!(
            state.llm.read().default_model(),
            "anthropic/claude-sonnet-4-6"
        );
    }

    /// A provider switch that leaves no speakable model is still rejected
    /// (`INCOMPATIBLE_MODEL_FOR_PROVIDER`), and — the part that is new in
    /// #1148 — the live client must be untouched by the failed attempt.
    /// This is the "config typo becomes a dead daemon" case: the whole
    /// point of live mutation is lost if a rejected switch can still
    /// poison the client every subsequent run resolves from.
    #[tokio::test]
    async fn rejected_provider_switch_leaves_the_live_client_untouched() {
        let mut state = settings_test_app_state_with_two_providers();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        // `anthropic` has no entry-level model and the live default model
        // is an OpenRouter slug, so the post-patch pair would be
        // (anthropic, z-ai/glm-5.2) — unspeakable on Anthropic's wire.
        let body = PatchSettingsRequest {
            provider: Some("anthropic".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let live = state.llm.read().clone();
        assert_eq!(live.provider(), "openrouter");
        assert_eq!(live.default_model(), "z-ai/glm-5.2");
        assert_eq!(
            live.base_url(),
            "https://openrouter.ai/api/v1",
            "a rejected provider switch must not retarget the live wire"
        );
    }

    /// First of three doors to the same defect: the body asks for a
    /// `(provider, model)` pair, the provider half is rejected — here an
    /// unknown name — and the model half must not commit behind it.
    ///
    /// Pre-fix shape: `{provider: "typo", model: "gpt-4o"}` against a live
    /// `anthropic` default returned 422 for the unknown provider while
    /// still committing `gpt-4o` onto the Anthropic client on the way out.
    /// This one is caught by the live-provider coherence check as well as
    /// by the pair rule; the two tests below are not.
    #[tokio::test]
    async fn unknown_provider_does_not_smuggle_an_incoherent_model_through() {
        let mut state = settings_test_app_state_with_two_providers();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        let setup = PatchSettingsRequest {
            model: Some("claude-sonnet-4-6".into()),
            provider: Some("anthropic".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&setup).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = PatchSettingsRequest {
            model: Some("gpt-4o".into()),
            provider: Some("provider-that-does-not-exist".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        assert_eq!(
            state.llm.read().provider(),
            "anthropic",
            "an unknown provider must not be committed"
        );
        assert_eq!(
            state.llm.read().default_model(),
            "claude-sonnet-4-6",
            "the cross-namespace model must not ride in behind a rejected \
             provider — nothing validated it against the live provider"
        );
    }

    /// Second door, and the one a gate keyed on provider *acceptance*
    /// cannot see: a **known** provider rejected on pair coherence.
    ///
    /// Live `(openrouter, z-ai/glm-5.2)`; body
    /// `{provider: "anthropic", model: "openai/gpt-4o-mini"}`. The
    /// provider arm rejects the pair — `openai/gpt-4o-mini` is not a
    /// `claude-*` slug. The surviving model half is then judged against
    /// the provider that STAYS in force, `openrouter`, whose
    /// `OpenAiCompatible` kind accepts every namespace. It is coherent
    /// with that wire, which is precisely why it slips a per-field check
    /// and only the pair-level rule stops it.
    ///
    /// Letting it through rebuilds the live client onto a model the
    /// operator was just told had been rejected, while the 422 suppresses
    /// `persist_settings` — so `settings.json` keeps the old model and a
    /// restart silently reverts. Neither half of a rejected pair commits.
    #[tokio::test]
    async fn rejected_provider_switch_does_not_commit_the_model_half() {
        use axum::body::to_bytes;

        let mut state = settings_test_app_state_with_two_providers();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        let body = PatchSettingsRequest {
            model: Some("openai/gpt-4o-mini".into()),
            provider: Some("anthropic".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let live = state.llm.read().clone();
        assert_eq!(
            live.provider(),
            "openrouter",
            "the rejected provider must not be committed"
        );
        assert_eq!(
            live.default_model(),
            "z-ai/glm-5.2",
            "nor may the model half ride in behind it — it is coherent with \
             the OpenAiCompatible provider that stays in force, so only the \
             pair rule stops it reaching the run path"
        );
        assert_eq!(
            state.server_llm_default.read().model,
            "z-ai/glm-5.2",
            "the displayed default must agree with the live client"
        );
        assert!(
            !settings_path(&state.data_dir).exists(),
            "a fully rejected PATCH persists nothing, so the live client and \
             settings.json must not diverge"
        );

        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json["errors"].as_array().map(Vec::len),
            Some(1),
            "one mistake, one error: the provider rejection already explains \
             why nothing moved. Got: {json}"
        );
    }

    /// The short-circuit has to hold in the other direction too: once the
    /// provider arm has *accepted* a switch, `body.model` must not be
    /// re-judged against the provider being switched **away** from.
    ///
    /// Live `(anthropic, claude-sonnet-4-6)`; body
    /// `{provider: "openrouter", model: "z-ai/glm-5.2"}`. The requested
    /// pair is coherent — an OpenRouter slug on an OpenAiCompatible wire —
    /// but `z-ai/glm-5.2` is not a `claude-*` slug, so a gate that also
    /// consulted the outgoing provider would reject a perfectly ordinary
    /// switch, commit the provider half alone, and hand the operator a 422
    /// naming a rule the request never broke. Every other test here
    /// switches *to* the strict kind, where the outgoing wire is
    /// permissive and the two readings agree.
    #[tokio::test]
    async fn an_accepted_switch_is_not_re_judged_against_the_outgoing_provider() {
        let mut state = settings_test_app_state_with_two_providers();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        // Move the live default onto the strict kind first.
        let setup = PatchSettingsRequest {
            model: Some("claude-sonnet-4-6".into()),
            provider: Some("anthropic".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&setup).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        // Now switch back out to OpenRouter with an OpenRouter slug.
        let body = PatchSettingsRequest {
            model: Some("z-ai/glm-5.2".into()),
            provider: Some("openrouter".into()),
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
            "the pair is coherent with the provider it names — the outgoing \
             Anthropic wire has no say in it"
        );

        let live = state.llm.read().clone();
        assert_eq!(live.provider(), "openrouter");
        assert_eq!(
            live.default_model(),
            "z-ai/glm-5.2",
            "both halves commit, or the operator is left with a provider \
             switch whose model never followed it"
        );
    }

    /// Third door: an **empty** `provider` string. It is rejected by its
    /// own guard before the coherence check ever runs, so nothing has
    /// looked at the pair — but the body still asked for one, and the
    /// model half must not commit behind the 422 either.
    #[tokio::test]
    async fn empty_provider_does_not_commit_the_model_half() {
        let mut state = settings_test_app_state_with_two_providers();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        let body = PatchSettingsRequest {
            model: Some("moonshotai/kimi-k2.5".into()),
            provider: Some(String::new()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        assert_eq!(
            state.llm.read().default_model(),
            "z-ai/glm-5.2",
            "an empty provider rejects the whole pair — the model half must \
             not reach the live client"
        );
        assert_eq!(
            state.server_llm_default.read().model,
            "z-ai/glm-5.2",
            "nor the displayed default"
        );
    }

    /// A provider-only switch whose only surviving candidate model is the
    /// **empty** live default must be rejected, not accepted.
    ///
    /// `model_belongs_to_kind("", OpenAiCompatible)` is `true`, so an
    /// unfiltered empty candidate passes the coherence check, commits
    /// `model = ""`, and — now that the pair is live — `with_model("")`
    /// clears the client every run resolves from. A 200 response would
    /// hand the operator a daemon where every subsequent run fails with a
    /// missing-model error. The `None => false` arm is where "no candidate
    /// model at all" belongs; the empty-filter is what routes it there.
    #[tokio::test]
    async fn provider_only_switch_rejects_an_empty_live_default_model() {
        use axum::body::to_bytes;

        let mut state = settings_test_app_state_with_two_providers();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        // A daemon booted with no `[llm] model` at all.
        {
            let mut snap = state.server_llm_default.write();
            snap.provider = "anthropic".into();
            snap.model = String::new();
        }

        // `openrouter` is OpenAiCompatible and its entry carries no
        // `model`, so the live default is the only candidate left.
        let body = PatchSettingsRequest {
            provider: Some("openrouter".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        assert_eq!(
            state.server_llm_default.read().provider,
            "anthropic",
            "an unrunnable post-patch state must commit nothing"
        );

        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            json["errors"][0]
                .as_str()
                .is_some_and(|e| e.contains("<unset>")),
            "the rejection must name the missing model rather than an empty \
             string that looks like a value. Got: {json}"
        );
    }

    /// The same empty-filter applies to the provider *entry* model.
    /// `[llm.providers.<n>].model = ""` in `alms.toml` is as reachable as
    /// an empty `[llm] model`, and it reaches the candidate chain one step
    /// earlier — so without the filter it wins over a perfectly good live
    /// default and clears the client the run path reads.
    #[tokio::test]
    async fn an_empty_provider_entry_model_falls_through_to_the_live_default() {
        let mut state = settings_test_app_state_with_two_providers();
        state.llm_config.providers.insert(
            "openrouter-blank".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::OpenAiCompatible,
                base_url: "https://openrouter.ai/api/v1".into(),
                api_key_env: None,
                api_key: Some("sk-or-blank".into()),
                model: Some(String::new()),
                auth_scheme: alms_core::config::AuthScheme::Bearer,
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        {
            let mut llm = state.llm.write();
            *llm = alms_runtime::LlmClient::new(alms_runtime::LlmConfig {
                provider: "openrouter".into(),
                base_url: "https://openrouter.ai/api/v1".into(),
                api_key: "sk-or-test".into(),
                default_model: "z-ai/glm-5.2".into(),
                providers: state.llm_config.providers.clone(),
                ..alms_runtime::LlmConfig::default()
            })
            .unwrap();
        }
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        let body = PatchSettingsRequest {
            provider: Some("openrouter-blank".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        assert_eq!(
            state.llm.read().provider(),
            "openrouter-blank",
            "the switch itself is fine — both sides are OpenAiCompatible"
        );
        assert_eq!(
            state.llm.read().default_model(),
            "z-ai/glm-5.2",
            "an empty entry model is not a model: it must fall through to \
             the live default rather than clearing the client"
        );
        assert_eq!(
            state.server_llm_default.read().model,
            "z-ai/glm-5.2",
            "and the displayed default must agree"
        );
    }

    /// `PATCH /settings` serialises end to end.
    ///
    /// The handler validates against the live `(provider, model)` pair,
    /// commits several statements later, rebuilds the shared client after
    /// that, and finally persists — so two concurrent requests could
    /// interleave into a pair neither of them asked for. `{"model":
    /// "gpt-4o"}` (validated against a live `openrouter`, accepted) and
    /// `{"provider": "anthropic", "model": "claude-sonnet-4-6"}` (also
    /// accepted) can land as `(anthropic, gpt-4o)` on the live wire —
    /// the incoherent pair the coherence gates exist to prevent, reached
    /// through a different door. Before #1148 the same race only corrupted
    /// `settings.json`.
    ///
    /// Pinned deterministically rather than by racing two requests: the
    /// test takes the lock the handler takes and asserts the handler makes
    /// no progress until it is released.
    ///
    /// **Not a timing flake, despite the timeout.** The polarity is
    /// inverted from the shape that flakes: `timeout(..).is_err()` fails
    /// only if the handler *completes* while the lock is held, so load or
    /// a slow runner makes it more likely to pass, not less. The window is
    /// deliberately small — the failure it catches (no lock at all)
    /// completes in microseconds, so a longer wait buys nothing and just
    /// costs every suite run. A panic in the task cannot pass it
    /// vacuously either: the `JoinHandle` would be ready and `is_err()`
    /// false.
    ///
    /// **`worker_threads = 2` is load-bearing.** One worker is parked on
    /// the blocking `parking_lot` guard for the duration (hence the scoped
    /// `await_holding_lock` allow); the test future itself runs on
    /// `block_on`'s thread. Trimming the worker count, or adding a second
    /// spawned task that also needs to make progress here, would deadlock
    /// rather than fail loudly.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::await_holding_lock)]
    async fn patch_settings_serialises_concurrent_requests() {
        let mut state = settings_test_app_state_with_two_providers();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        // Stand in for a PATCH that is already inside the handler.
        let lock = std::sync::Arc::clone(&state.settings_patch_lock);
        let guard = lock.lock();

        let bg_state = state.clone();
        let mut bg = tokio::spawn(async move {
            patch_settings(
                axum::extract::State(bg_state),
                Json(serde_json::json!({ "model": "moonshotai/kimi-k2.5" })),
            )
            .await
            .into_response()
            .status()
        });

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut bg)
                .await
                .is_err(),
            "a second PATCH must not run its validate/commit/rebuild while \
             another request holds the settings lock"
        );
        assert_eq!(
            state.llm.read().default_model(),
            "z-ai/glm-5.2",
            "and it must not have reached the live client either"
        );

        drop(guard);

        let status = tokio::time::timeout(std::time::Duration::from_secs(10), bg)
            .await
            .expect("the blocked PATCH must complete once the lock is released")
            .expect("the handler task must not panic");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            state.llm.read().default_model(),
            "moonshotai/kimi-k2.5",
            "and it must apply normally afterwards"
        );
    }

    /// Fixture whose third provider carries an entry-level `model`.
    ///
    /// That is the only shape that can tell the two apply orders inside
    /// `AppState::refresh_llm_from_server_default` apart, because
    /// `apply_provider` overwrites `default_model` from
    /// `[llm.providers.<name>].model` when the entry has one. It is a
    /// separate fixture rather than a fourth entry on the shared one
    /// because `rejected_provider_switch_leaves_the_live_client_untouched`
    /// depends on `anthropic` having no entry model.
    fn settings_test_app_state_with_entry_model_provider() -> crate::server::AppState {
        let mut state = settings_test_app_state_with_two_providers();
        state.llm_config.providers.insert(
            "anthropic-pinned".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: Some("sk-ant-pinned".into()),
                model: Some("claude-haiku-4-5".into()),
                auth_scheme: alms_core::config::AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        // Rebuild the live client so its own `providers` snapshot carries
        // the third entry — `apply_provider` reads the client's map, not
        // `state.llm_config`.
        {
            let mut llm = state.llm.write();
            *llm = alms_runtime::LlmClient::new(alms_runtime::LlmConfig {
                provider: "openrouter".into(),
                base_url: "https://openrouter.ai/api/v1".into(),
                api_key: "sk-or-test".into(),
                default_model: "z-ai/glm-5.2".into(),
                providers: state.llm_config.providers.clone(),
                ..alms_runtime::LlmConfig::default()
            })
            .unwrap();
        }
        state
    }

    /// The rebuild applies provider **then** model, mirroring boot — and
    /// the order is load-bearing.
    ///
    /// `apply_provider` overwrites `default_model` from
    /// `[llm.providers.<name>].model`, so a model-then-provider rebuild
    /// would silently replace the model the operator just patched with the
    /// provider entry's. Every other fixture in this file uses
    /// `model: None`, which makes both orders indistinguishable — this is
    /// the one that gives the ordering something to bite on.
    #[tokio::test]
    async fn patched_model_wins_over_the_target_provider_entry_model() {
        let mut state = settings_test_app_state_with_entry_model_provider();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        let body = PatchSettingsRequest {
            model: Some("claude-sonnet-4-6".into()),
            provider: Some("anthropic-pinned".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        let live = state.llm.read().clone();
        assert_eq!(live.provider(), "anthropic-pinned");
        assert_eq!(
            live.default_model(),
            "claude-sonnet-4-6",
            "the operator's model must survive the provider apply — \
             `[llm.providers.anthropic-pinned].model` is claude-haiku-4-5, \
             which is what a model-then-provider rebuild would leave here"
        );
        assert_eq!(
            state.server_llm_default.read().model,
            "claude-sonnet-4-6",
            "and the displayed default must agree with it"
        );
    }

    /// The fourth door on the pair invariant, and the mirror of
    /// `rejected_provider_switch_does_not_commit_the_model_half`.
    ///
    /// live = `(openrouter, z-ai/glm-5.2)`; body =
    /// `{"provider": "anthropic-pinned", "model": ""}`. The provider half
    /// is fine on its own, so the pair-rejection path that closes the
    /// other three doors never fires — but the model half is rejected,
    /// and in the pre-fix interleaved shape that rejection arrived
    /// *after* the pair had been committed and the live client rebuilt.
    ///
    /// Worse than a single leak: because an empty `model` is filtered to
    /// `None` before the compat check, the entry-model fallback also
    /// committed `[llm.providers.anthropic-pinned].model`
    /// (`claude-haiku-4-5`) — a model the body never named. The operator
    /// saw `422`, the daemon moved to a pair they had not asked for, and
    /// `settings.json` still held the old one, so a restart silently
    /// reverted it.
    ///
    /// This body is also what falsified the "all or nothing" sentence
    /// `docs/api.md` § 10.2 added in this PR, against row 2 of its own
    /// table (*"`model` / `provider` must be non-empty when present"*).
    /// Not reachable from the UI — `settings-modal.js` gates on a
    /// non-empty model — so it is an API / scripted-client shape, the
    /// same class as the `{"provider": ""}` door.
    #[tokio::test]
    async fn empty_model_does_not_commit_the_provider_half() {
        use axum::body::to_bytes;

        let mut state = settings_test_app_state_with_entry_model_provider();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::json!({ "provider": "anthropic-pinned", "model": "" })),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let errors = json["errors"].as_array().expect("errors array");
        assert_eq!(
            errors.len(),
            1,
            "one mistake, one error — the provider half was valid: {errors:?}"
        );
        assert!(
            errors[0]
                .as_str()
                .unwrap_or("")
                .contains("model: empty string not accepted"),
            "got {errors:?}"
        );

        // The object that matters is the client the run path reads.
        let live = state.llm.read().clone();
        assert_eq!(
            live.provider(),
            "openrouter",
            "the provider half must not commit behind a 422"
        );
        assert_eq!(
            live.default_model(),
            "z-ai/glm-5.2",
            "and the target entry's model — which the body never named — \
             must not be substituted for the operator's"
        );

        let displayed = state.server_llm_default.read().clone();
        assert_eq!(displayed.provider, "openrouter");
        assert_eq!(displayed.model, "z-ai/glm-5.2");

        assert!(
            !settings_path(&state.data_dir).exists(),
            "a rejected PATCH must be a no-op at the persistence layer — \
             which is exactly why a live commit here is not survivable: \
             nothing on disk records it, so the next restart reverts it"
        );
    }

    /// Two bad halves, two errors — and still nothing committed.
    ///
    /// Pins that hoisting the empty-model check above the provider arm
    /// did not collapse the pair into a single error, and that the
    /// "one mistake, one error" rule the coherence gate follows is about
    /// *derived* errors, not about suppressing a second independent
    /// mistake the operator actually made.
    #[tokio::test]
    async fn empty_model_with_unknown_provider_reports_both_mistakes() {
        use axum::body::to_bytes;

        let mut state = settings_test_app_state_with_entry_model_provider();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::json!({ "provider": "nope", "model": "" })),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let errors = json["errors"].as_array().expect("errors array");
        assert_eq!(errors.len(), 2, "two mistakes, two errors: {errors:?}");

        let live = state.llm.read().clone();
        assert_eq!(live.provider(), "openrouter");
        assert_eq!(live.default_model(), "z-ai/glm-5.2");
        assert!(!settings_path(&state.data_dir).exists());
    }

    /// Exhaustive sweep of the `(provider, model)` input space against the
    /// single property the pair block exists to guarantee: **a rejected
    /// PATCH moves nothing.**
    ///
    /// Four doors were found on this invariant one at a time, each by
    /// reasoning about one specific body, and each fix was followed by
    /// another body nobody had thought of. This asks the same question
    /// mechanically instead. For every combination of an absent / empty /
    /// unknown / same / different `provider` — strict targets both with
    /// and without an entry-level `model` — with an absent / empty /
    /// cross-namespace / in-namespace `model`, from both a permissive
    /// (`OpenAiCompatible`) and a strict (`Anthropic`) live provider, any
    /// non-`200` response must leave the live client, the displayed
    /// default, **and** `settings.json` exactly as they were.
    ///
    /// It deliberately does not assert *which* bodies get rejected — the
    /// named rows above own that, and duplicating it here would make this
    /// test fail for uninteresting reasons every time a rule is tuned.
    /// (Concretely: dropping the empty-model rule entirely leaves this
    /// test green and kills `empty_model_does_not_commit_the_provider_half`
    /// instead. The two are complements, not overlaps.) What this pins is
    /// that rejection and mutation never co-occur — precisely the property
    /// a fifth door would break — so a new rule added to Phase 1 in the
    /// wrong place fails here without anyone having to guess the body that
    /// exposes it.
    #[tokio::test]
    async fn no_rejected_pair_body_moves_the_live_client() {
        // Both live baselines: a permissive wire that accepts every
        // namespace, and a strict one that does not. The strict baseline
        // is what makes the cross-namespace cells reject at all.
        for baseline in ["openrouter", "anthropic-pinned"] {
            for provider in [
                None,
                Some(""),
                Some("nope"),
                Some("openrouter"),
                // Strict kind with NO entry-level `model` (Tim review on
                // #1271). `anthropic-pinned` alone leaves a Phase 1b
                // branch unswept: with an entry model present the
                // candidate resolves from `[llm.providers.<name>].model`,
                // while here it has to fall through to the live default —
                // which is how a provider-only switch onto a strict wire
                // reaches the reject arm carrying the *outgoing*
                // provider's model.
                Some("anthropic"),
                Some("anthropic-pinned"),
            ] {
                for model in [
                    None,
                    Some(""),
                    Some("gpt-4o"),
                    Some("claude-sonnet-4-6"),
                    Some("z-ai/glm-5.2"),
                ] {
                    let mut body = serde_json::Map::new();
                    if let Some(p) = provider {
                        body.insert("provider".into(), serde_json::json!(p));
                    }
                    if let Some(m) = model {
                        body.insert("model".into(), serde_json::json!(m));
                    }
                    if body.is_empty() {
                        continue;
                    }

                    let mut state = settings_test_app_state_with_entry_model_provider();
                    let tmp = tempfile::tempdir().unwrap();
                    state.data_dir = tmp.path().to_path_buf();

                    if baseline == "anthropic-pinned" {
                        let setup = patch_settings(
                            axum::extract::State(state.clone()),
                            Json(serde_json::json!({
                                "provider": "anthropic-pinned",
                                "model": "claude-sonnet-4-6",
                            })),
                        )
                        .await
                        .into_response();
                        assert_eq!(setup.status(), StatusCode::OK, "baseline setup failed");
                    }

                    let case = format!("baseline={baseline} body={body:?}");
                    let live_before = state.llm.read().clone();
                    let displayed_before = state.server_llm_default.read().clone();
                    let disk_before = std::fs::read_to_string(settings_path(&state.data_dir)).ok();

                    let resp = patch_settings(
                        axum::extract::State(state.clone()),
                        Json(serde_json::Value::Object(body)),
                    )
                    .await
                    .into_response();
                    if resp.status() == StatusCode::OK {
                        continue;
                    }

                    let live_after = state.llm.read().clone();
                    assert_eq!(
                        live_after.provider(),
                        live_before.provider(),
                        "rejected PATCH moved the live provider — {case}"
                    );
                    assert_eq!(
                        live_after.default_model(),
                        live_before.default_model(),
                        "rejected PATCH moved the live model — {case}"
                    );

                    let displayed_after = state.server_llm_default.read().clone();
                    assert_eq!(
                        displayed_after.provider, displayed_before.provider,
                        "rejected PATCH moved the displayed provider — {case}"
                    );
                    assert_eq!(
                        displayed_after.model, displayed_before.model,
                        "rejected PATCH moved the displayed model — {case}"
                    );

                    assert_eq!(
                        std::fs::read_to_string(settings_path(&state.data_dir)).ok(),
                        disk_before,
                        "rejected PATCH touched settings.json — {case}"
                    );
                }
            }
        }
    }

    /// Call-site row for `AppState::refresh_llm_from_server_default`'s
    /// choice of `with_provider_and_secrets` over `with_provider`.
    ///
    /// `apply_provider` clears the *outgoing* provider's API key whenever
    /// the switch's resolver returns `None`, so the one-token mutation
    /// `with_provider_and_secrets(..)` → `with_provider(..)` leaves the
    /// shared client with an empty key after every live provider switch —
    /// and every subsequent default-agent run 401s with nothing tying the
    /// failure back to the PATCH.
    ///
    /// That exact bug shipped once already, on the sibling boot path
    /// (#1081; see
    /// `boot_resolves_provider_entry_api_key_after_persisted_provider_switch`).
    /// The runtime-side `test_provider_round_trip_restores_entry_api_key`
    /// does **not** cover this one: it calls the callee directly, so it
    /// passes unchanged under a mutation at the call site. A callee test
    /// cannot kill a call-site mutation — hence
    /// [`alms_runtime::LlmClient::has_api_key`], which answers the yes/no
    /// question without exposing key material.
    #[tokio::test]
    async fn patch_provider_switch_re_resolves_the_api_key_on_the_live_client() {
        let mut state = settings_test_app_state_with_entry_model_provider();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        // `[llm.providers.anthropic-pinned].api_key` is the only key
        // source for the target provider — the SecretsStore holds none,
        // so the resolver has to consult the entry.
        assert!(
            state
                .secrets
                .read()
                .resolve_key("anthropic-pinned")
                .is_none(),
            "fixture must leave the provider entry as the only key source"
        );
        assert!(
            state.llm.read().has_api_key(),
            "baseline: the live client starts holding the openrouter key, \
             which is what `apply_provider` would clear on the switch"
        );

        let body = PatchSettingsRequest {
            model: Some("claude-sonnet-4-6".into()),
            provider: Some("anthropic-pinned".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        let live = state.llm.read().clone();
        assert_eq!(live.provider(), "anthropic-pinned");
        assert!(
            live.has_api_key(),
            "a live provider switch must re-resolve credentials for the new \
             provider — an empty key here means the rebuild skipped the \
             secrets resolver and every subsequent run would 401"
        );
    }

    /// An idempotent PATCH must not churn the live client. Rebuilding
    /// re-resolves the API key and logs provider-switch decisions, so a
    /// no-op body should leave the client byte-identical rather than
    /// round-tripping it through `apply_provider`.
    #[tokio::test]
    async fn no_op_patch_does_not_rebuild_the_live_client() {
        let mut state = settings_test_app_state_with_two_providers();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        // Deliberately desynchronise the client from `server_llm_default`
        // so a rebuild would be observable: only a rebuild would pull the
        // client's model back onto the (unchanged) default pair.
        {
            let mut llm = state.llm.write();
            *llm = llm.clone().with_model("sentinel-not-the-default");
        }

        let body = PatchSettingsRequest {
            // Same values the fixture already holds — a genuine no-op.
            model: Some("z-ai/glm-5.2".into()),
            provider: Some("openrouter".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        assert_eq!(
            state.llm.read().default_model(),
            "sentinel-not-the-default",
            "an idempotent PATCH must not rebuild the live client"
        );
    }

    /// Live mutation applies even when an unrelated field in the same body
    /// fails validation (422 `status: "partial"`). Gating the client
    /// rebuild on a globally clean request would leave
    /// `server_llm_default` — what `GET /settings` reports — describing a
    /// pair the run path is not using, re-creating the exact divergence
    /// #1148 removes. Only persistence is gated on a clean request.
    #[tokio::test]
    async fn partial_failure_still_applies_the_committed_pair_to_the_live_client() {
        let mut state = settings_test_app_state_with_two_providers();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        let raw = serde_json::json!({
            "model": "moonshotai/kimi-k2.5",
            "context": { "strategy": "definitely-not-a-strategy" },
        });
        let resp = patch_settings(axum::extract::State(state.clone()), Json(raw))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        assert_eq!(
            state.server_llm_default.read().model,
            "moonshotai/kimi-k2.5"
        );
        assert_eq!(
            state.llm.read().default_model(),
            "moonshotai/kimi-k2.5",
            "the displayed default and the run-path client must never \
             disagree — that divergence is the bug #1148 fixes"
        );
        assert!(
            !settings_path(&state.data_dir).exists(),
            "persistence stays gated on a clean request"
        );
    }

    #[tokio::test]
    async fn patch_top_level_model_rejects_empty_string() {
        use axum::body::to_bytes;

        let state = settings_test_app_state();
        let tmp = tempfile::tempdir().unwrap();
        let mut state = state;
        state.data_dir = tmp.path().to_path_buf();

        let body = PatchSettingsRequest {
            model: Some(String::new()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body_bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let errors = json["errors"].as_array().unwrap();
        assert!(
            errors
                .iter()
                .any(|e| e.as_str().unwrap_or("").contains("model: empty string")),
            "expected empty-model rejection, got: {errors:?}"
        );
    }

    #[tokio::test]
    async fn patch_top_level_provider_rejects_unknown() {
        use axum::body::to_bytes;

        let state = settings_test_app_state();
        let tmp = tempfile::tempdir().unwrap();
        let mut state = state;
        state.data_dir = tmp.path().to_path_buf();

        let body = PatchSettingsRequest {
            provider: Some("totally-not-a-provider".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body_bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let errors = json["errors"].as_array().unwrap();
        assert!(
            errors
                .iter()
                .any(|e| e.as_str().unwrap_or("").contains("totally-not-a-provider")),
            "expected unknown-provider rejection, got: {errors:?}"
        );
    }

    // ==================================================================
    // Codex follow-up on #1081 (P1 #1) — server-default provider switch
    // must enforce wire-compatibility against the post-patch model.
    //
    // Pre-fix: PATCHing only `{"provider": "anthropic"}` kept the previous
    // default model (e.g. `moonshotai/kimi-k2.6` on OpenRouter), persisted
    // the bad pair, and after restart every run sent an OpenRouter slug
    // to Anthropic's wire and failed authentication / model-id-shape.
    //
    // Post-fix: a provider-only PATCH is rejected with
    // `INCOMPATIBLE_MODEL_FOR_PROVIDER` unless the post-patch candidate
    // model (which is either the provider entry's `model` field or the
    // current server-default model) is wire-compatible with the new
    // provider's kind. Tests cover:
    //  - (a) provider-only PATCH where the existing default is from a
    //    different namespace → 422 with INCOMPATIBLE_MODEL_FOR_PROVIDER
    //  - (b) provider + model PATCH where the new model is compatible
    //    → 200, both knobs land
    //  - (c) model-only PATCH (no provider) → 200, model lands
    //  - (d) provider switch where the new provider has its own entry
    //    `model` field that matches the new kind → 200
    // ==================================================================

    /// (a) Provider-only PATCH switching from openrouter→anthropic while
    /// the live default model is `moonshotai/kimi-k2.6` is rejected. The
    /// post-patch pair would be `(anthropic, moonshotai/kimi-k2.6)` and
    /// `moonshotai/...` is not a `claude-*` prefix, so the runtime would
    /// fail on the wire after restart.
    #[tokio::test]
    async fn patch_provider_only_rejects_cross_namespace_default_model() {
        let mut state = settings_test_app_state();
        // Seed `[llm.providers.anthropic]` so the providers-map check
        // passes — the wire-compat check is what we want to exercise.
        state.llm_config.providers.insert(
            "anthropic".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: None,
                // Crucially: no entry-level model override. The candidate
                // model falls through to the live server default
                // (`moonshotai/kimi-k2.6`), which is NOT a `claude-*`.
                model: None,
                auth_scheme: alms_core::config::AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        // Force the baseline to a non-claude model so the compat check
        // sees something concrete to reject.
        {
            let mut snap = state.server_llm_default.write();
            snap.model = "moonshotai/kimi-k2.6".into();
            snap.provider = "openrouter".into();
        }

        let body = PatchSettingsRequest {
            provider: Some("anthropic".into()),
            ..Default::default()
        };
        let (status, errors) = patch_and_read_errors(state.clone(), body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("INCOMPATIBLE_MODEL_FOR_PROVIDER")),
            "expected INCOMPATIBLE_MODEL_FOR_PROVIDER, got: {errors:?}"
        );
        // The provider field MUST NOT have landed on `server_llm_default`
        // — the whole switch is rejected.
        let live = state.server_llm_default.read().clone();
        assert_eq!(live.provider, "openrouter");
        assert_eq!(live.model, "moonshotai/kimi-k2.6");
    }

    /// (b) Provider + model PATCH where the new model wire-matches the
    /// new provider's kind succeeds.
    #[tokio::test]
    async fn patch_provider_with_compatible_model_succeeds() {
        let mut state = settings_test_app_state();
        state.llm_config.providers.insert(
            "anthropic".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: None,
                model: None,
                auth_scheme: alms_core::config::AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        {
            let mut snap = state.server_llm_default.write();
            snap.model = "moonshotai/kimi-k2.6".into();
            snap.provider = "openrouter".into();
        }

        let body = PatchSettingsRequest {
            provider: Some("anthropic".into()),
            model: Some("claude-sonnet-4-5".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let live = state.server_llm_default.read().clone();
        assert_eq!(live.provider, "anthropic");
        assert_eq!(live.model, "claude-sonnet-4-5");
    }

    /// (c) Model-only PATCH (no provider) succeeds — the provider stays
    /// on the live default and the model lands. The compat check is
    /// guarded behind a provider switch so this path is untouched.
    #[tokio::test]
    async fn patch_model_only_succeeds() {
        let mut state = settings_test_app_state();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        {
            let mut snap = state.server_llm_default.write();
            snap.model = "moonshotai/kimi-k2.6".into();
            snap.provider = "openrouter".into();
        }

        let body = PatchSettingsRequest {
            model: Some("openai/gpt-4o-mini".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let live = state.server_llm_default.read().clone();
        assert_eq!(live.provider, "openrouter");
        assert_eq!(live.model, "openai/gpt-4o-mini");
    }

    /// (d) Provider-only PATCH where the new provider's entry carries
    /// its own compatible `model` succeeds — the entry model is the
    /// candidate that satisfies the compat check, exactly like the
    /// runtime's `resolve_effective_provider_and_model` decision.
    ///
    /// Codex follow-up on #1081 (P1 #B): the entry model is ALSO
    /// committed to `server_llm_default.model` so the persisted pair is
    /// coherent. Pre-fix the live default model kept the old
    /// cross-namespace value (`moonshotai/kimi-k2.6`), `persist_settings`
    /// serialised the bad pair, and the boot path reapplied both fields
    /// verbatim — the LlmClient landed on `(anthropic,
    /// moonshotai/kimi-k2.6)` and every run 4xxed on the model-id shape.
    #[tokio::test]
    async fn patch_provider_only_succeeds_when_provider_entry_has_compatible_model() {
        let mut state = settings_test_app_state();
        state.llm_config.providers.insert(
            "anthropic".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: None,
                // Entry-level model override — satisfies the compat check
                // even when the live default model is cross-namespace.
                model: Some("claude-haiku-4-5".into()),
                auth_scheme: alms_core::config::AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        {
            let mut snap = state.server_llm_default.write();
            snap.model = "moonshotai/kimi-k2.6".into();
            snap.provider = "openrouter".into();
        }

        let body = PatchSettingsRequest {
            provider: Some("anthropic".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let live = state.server_llm_default.read().clone();
        assert_eq!(live.provider, "anthropic");
        // The entry-level model is the candidate that satisfied the
        // compat check, so it lands on `server_llm_default.model` too —
        // this is what keeps the persisted pair coherent across a
        // restart (see the round-trip test below for the boot half).
        assert_eq!(live.model, "claude-haiku-4-5");
    }

    /// Codex follow-up on #1081 (P1 #B): end-to-end round-trip
    /// confirming the boot path is clean.
    ///
    /// Pre-fix: provider-only PATCH committed `provider = "anthropic"`
    /// but left `server_llm_default.model = "moonshotai/kimi-k2.6"`.
    /// `persist_settings` serialised both fields, so `settings.json`
    /// carried `{provider: "anthropic", model: "moonshotai/kimi-k2.6"}`.
    /// `AppState::new` then reapplies them verbatim and the LlmClient
    /// boots on `(anthropic, moonshotai/kimi-k2.6)` — every run 4xxs.
    ///
    /// Post-fix: PATCH commits the entry-level candidate that passed
    /// the compat check, persistence reflects the coherent pair, and
    /// the boot path lands on `(anthropic, claude-haiku-4-5)`.
    #[tokio::test]
    async fn patch_provider_only_persists_coherent_pair_across_restart() {
        // ── 1. PATCH ────────────────────────────────────────────────────
        // First half: drive the PATCH handler with the attack body and
        // confirm `persist_settings` writes the COHERENT pair to disk.
        let mut state = settings_test_app_state();
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
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        {
            let mut snap = state.server_llm_default.write();
            snap.model = "moonshotai/kimi-k2.6".into();
            snap.provider = "openrouter".into();
        }

        let body = PatchSettingsRequest {
            provider: Some("anthropic".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        // ── 2. PERSIST ─────────────────────────────────────────────────
        // Read `settings.json` directly off disk — `persist_settings` is
        // called inline by `patch_settings` on the success path. Confirm
        // the on-disk pair matches the post-PATCH live default.
        let path = settings_path(&state.data_dir);
        let raw = std::fs::read_to_string(&path).unwrap();
        let persisted: PersistedSettings = serde_json::from_str(&raw).unwrap();
        assert_eq!(persisted.provider.as_deref(), Some("anthropic"));
        assert_eq!(
            persisted.model.as_deref(),
            Some("claude-haiku-4-5"),
            "persisted model must be the entry-level candidate that satisfied the \
             compat check, NOT the stale cross-namespace default that would otherwise \
             override the provider on the next boot"
        );

        // ── 3. (sanity) Live default mirrors the persisted pair ───────
        // No separate restart simulation needed: `AppState::new` reads
        // `persisted.model` / `persisted.provider` and calls the same
        // `with_provider_and_secrets` + `with_model` setters that we
        // already exercise from the live default. The persisted-pair
        // assertion above is the boot-path-equivalent check.
        let live = state.server_llm_default.read().clone();
        assert_eq!(live.provider, "anthropic");
        assert_eq!(live.model, "claude-haiku-4-5");
    }

    // ==================================================================
    // Codex follow-up on #1081 (P1 #3) — boot-path API key resolution
    // for a persisted server-default provider switch.
    //
    // Pre-fix: `AppState::new` applied a persisted `provider` via
    // `llm.with_provider(provider)`, which never resolves keys from
    // `[llm.providers.<name>].api_key_env / api_key` or `SecretsStore`.
    // `apply_provider` then cleared the previous provider's key, leaving
    // an empty `api_key` on the LlmClient. A later `resolve_agent_config`
    // call for the default agent only refreshes from `SecretsStore`
    // (`with_secrets`), never the provider entry. Deployments configuring
    // keys exclusively via `[llm.providers.<name>].api_key_env / api_key`
    // (no `alms auth set`) would silently boot with an empty key every
    // restart after a persisted provider switch.
    //
    // Post-fix: `AppState::new` calls `with_provider_and_secrets` so
    // both the SecretsStore AND the provider entry are consulted.
    // ==================================================================

    /// Persisted provider switch + provider-entry `api_key` (no
    /// SecretsStore key for the new provider) must result in a non-empty
    /// `LlmClient::api_key()` on boot.
    #[tokio::test]
    async fn boot_resolves_provider_entry_api_key_after_persisted_provider_switch() {
        // 1. Build a `GatewayConfig` whose providers map carries an
        //    `anthropic` entry with an inline `api_key`. The SecretsStore
        //    is left empty so the entry is the only source of truth.
        let mut gateway_config = crate::gateway::GatewayConfig::default();
        gateway_config.llm_config.providers.insert(
            "anthropic".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: Some("sk-ant-from-provider-entry".into()),
                model: Some("claude-haiku-4-5".into()),
                auth_scheme: alms_core::config::AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        // The boot path's persisted-settings load lives under
        // `data_dir`. Point at a tempdir so the persisted shape we plant
        // below is what `AppState::new` actually reads.
        let tmp = tempfile::tempdir().unwrap();
        gateway_config.data_dir = Some(tmp.path().to_path_buf());

        // 2. Plant a `settings.json` recording a previous PATCH that
        //    flipped the server-default provider to `anthropic`.
        let persisted = PersistedSettings {
            context: None,
            session: None,
            tools: None,
            llm: None,
            model: None,
            provider: Some("anthropic".into()),
        };
        std::fs::write(
            settings_path(tmp.path()),
            serde_json::to_string_pretty(&persisted).unwrap(),
        )
        .unwrap();

        // 3. Construct the AppState — this exercises the boot path that
        //    Finding #3 targets.
        let gateway = crate::gateway::Gateway::new(gateway_config).unwrap();
        let scheduler = std::sync::Arc::new(alms_runtime::Scheduler::new());
        let shutdown_token = tokio_util::sync::CancellationToken::new();
        let (completion_tx, _cr) = tokio::sync::mpsc::unbounded_channel();
        let (trigger_tx, _tr) = tokio::sync::mpsc::channel(8);
        let (dm_event_tx, _dr) = tokio::sync::mpsc::channel(8);
        let state = crate::server::AppState::new(
            gateway,
            scheduler,
            shutdown_token,
            completion_tx,
            trigger_tx,
            dm_event_tx,
        )
        .unwrap();

        // 4. Assertions. The LlmClient must have switched to Anthropic
        //    and the persisted-settings load must have flowed through to
        //    `server_llm_default`.
        //
        // The exact key value can't be compared from outside the runtime
        // crate (`LlmClient::api_key()` is `#[cfg(test)]`-gated), but
        // `has_api_key()` pins the property this boot path exists for:
        // the persisted switch must leave the client able to
        // authenticate. Without that assertion this test passed under
        // `with_provider_and_secrets` → `with_provider`, which is the
        // exact shape of the #1081 bug it was written to prevent — a
        // callee test cannot kill a call-site mutation.
        assert_eq!(state.llm.read().provider(), "anthropic");
        assert!(
            state.llm.read().has_api_key(),
            "boot must resolve `[llm.providers.anthropic].api_key` for the \
             persisted provider — `apply_provider` clears the outgoing key, \
             so an empty key here is the #1081 regression"
        );
        let default = state.server_llm_default.read().clone();
        assert_eq!(default.provider, "anthropic");
    }

    // ==================================================================
    // Codex follow-up on #1081 (P1, Finding 6) — boot path must revalidate
    // the persisted provider against the live `[llm.providers]` map before
    // applying it. An operator who PATCH-switched the server default to
    // `anthropic` and later removed `[llm.providers.anthropic]` from
    // `alms.toml` (or renamed it) would otherwise still force the stale
    // provider on every restart, and `with_provider_and_secrets` would
    // clear the previously active provider's API key without populating a
    // new one — leaving default runs failing with opaque 401s the operator
    // can't correlate back to the persisted PATCH.
    //
    // Post-fix: the boot path calls `providers.contains_key(&persisted)`
    // before mutating `llm_config.provider` / the LlmClient. On miss it
    // emits a structured WARN under `alms.config` and skips the override
    // — the boot path falls through to the `alms.toml` default provider.
    // ==================================================================

    /// Persisted server-default provider that is no longer present in
    /// `alms.toml` must be silently ignored on the next restart. The boot
    /// path keeps the in-file default, and the LlmClient retains the
    /// `alms.toml`-configured credentials instead of being cleared by
    /// `apply_provider`.
    #[tokio::test]
    async fn boot_skips_persisted_provider_when_no_longer_configured() {
        // 1. Build a `GatewayConfig` whose providers map intentionally
        //    OMITS the persisted provider name. The in-file default is
        //    `openrouter` (LlmConfig::default), and we add an openrouter
        //    entry with a usable inline key so the post-skip state has
        //    something to fall through to.
        let mut gateway_config = crate::gateway::GatewayConfig::default();
        // Confirm the default-provider invariant we rely on for the
        // assertion below — this test pins the regression even if the
        // default changes, but make the dependency explicit so a future
        // default flip surfaces here.
        assert_eq!(
            gateway_config.llm_config.provider, "openrouter",
            "test relies on LlmConfig::default().provider being openrouter; \
             update the assertion below if that changes"
        );
        gateway_config.llm_config.providers.insert(
            "openrouter".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::OpenAiCompatible,
                base_url: "https://openrouter.ai/api/v1".into(),
                api_key_env: None,
                // Inline `api_key` so the boot path's
                // `with_provider_and_secrets` (when it runs) would
                // populate non-empty credentials. We assert below that
                // the openrouter key survives because the persisted
                // override was skipped — if the guard regressed and
                // `apply_provider("anthropic-removed")` ran, this key
                // would be cleared.
                api_key: Some("sk-or-from-provider-entry".into()),
                model: None,
                auth_scheme: alms_core::config::AuthScheme::Bearer,
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let tmp = tempfile::tempdir().unwrap();
        gateway_config.data_dir = Some(tmp.path().to_path_buf());

        // 2. Plant a `settings.json` recording a previous PATCH that
        //    flipped the server-default provider to `anthropic` — which
        //    is no longer in the providers map (simulates operator
        //    removing the `[llm.providers.anthropic]` entry between
        //    boots).
        let persisted = PersistedSettings {
            context: None,
            session: None,
            tools: None,
            llm: None,
            model: None,
            provider: Some("anthropic-removed".into()),
        };
        std::fs::write(
            settings_path(tmp.path()),
            serde_json::to_string_pretty(&persisted).unwrap(),
        )
        .unwrap();

        // 3. Construct AppState — exercises the boot path with the
        //    stale persisted provider.
        let gateway = crate::gateway::Gateway::new(gateway_config).unwrap();
        let scheduler = std::sync::Arc::new(alms_runtime::Scheduler::new());
        let shutdown_token = tokio_util::sync::CancellationToken::new();
        let (completion_tx, _cr) = tokio::sync::mpsc::unbounded_channel();
        let (trigger_tx, _tr) = tokio::sync::mpsc::channel(8);
        let (dm_event_tx, _dr) = tokio::sync::mpsc::channel(8);
        let state = crate::server::AppState::new(
            gateway,
            scheduler,
            shutdown_token,
            completion_tx,
            trigger_tx,
            dm_event_tx,
        )
        .unwrap();

        // 4. Assertions. The boot path must have ignored the persisted
        //    `anthropic-removed` value and kept the in-file default
        //    (`openrouter`).
        assert_eq!(
            state.llm.read().provider(),
            "openrouter",
            "boot must skip persisted provider when no longer configured \
             in `[llm.providers]` and keep the in-file default"
        );
        let default = state.server_llm_default.read().clone();
        assert_eq!(
            default.provider, "openrouter",
            "server_llm_default must mirror the in-file provider, not \
             the skipped persisted value"
        );

        // 5. Provider-kind cross-check: the LlmClient's wire-kind
        //    must still match the in-file openrouter entry
        //    (`OpenAiCompatible`). If the pre-fix path had invoked
        //    `with_provider_and_secrets("anthropic-removed", ...)`,
        //    `apply_provider` would have fallen through to the
        //    `_ => OpenAiCompatible` arm of `provider_kind_for_name`
        //    (which would still be `OpenAiCompatible`, so this is not
        //    a sharp discriminator on its own). The combined assertions
        //    above — `provider() == "openrouter"` and
        //    `server_llm_default.provider == "openrouter"` — are the
        //    load-bearing signal: any pre-fix regression flips both.
        assert_eq!(
            state.llm.read().provider_kind(),
            alms_core::config::ProviderKind::OpenAiCompatible,
            "openrouter entry was provisioned with OpenAiCompatible kind \
             — the skipped persisted-provider path must leave that wire \
             shape intact"
        );
    }

    // ==================================================================
    // Codex follow-up on #1081 (#1088) — gate the persisted MODEL
    // override on the same provider-validity check.
    //
    // Pre-fix: `AppState::new` correctly skipped `persisted.provider`
    // when the provider name was no longer in `llm_config.providers`,
    // but unconditionally applied `persisted.model` below that guard.
    // For namespace-specific models (e.g. `claude-haiku-4-5`) that's a
    // stale slug on the fallback provider (e.g. `openrouter`), surfacing
    // as opaque 4xx on the next default-agent run.
    //
    // Post-fix: when the persisted provider is invalid, both `provider`
    // and `model` are skipped together. A single WARN under `alms.config`
    // names both dropped values; the in-file default `(provider, model)`
    // pair is preserved end-to-end.
    // ==================================================================

    /// When the persisted provider has been removed from `alms.toml`,
    /// the persisted *model* override must be skipped too — the model is
    /// namespace-specific and would force the fallback provider to
    /// receive an unresolvable slug.
    #[tokio::test]
    async fn boot_skips_persisted_model_when_persisted_provider_invalid() {
        // 1. Mirror the upstream regression test: in-file default provider
        //    is `openrouter` with a usable inline key; the persisted
        //    settings record a stale `(anthropic-removed, claude-haiku-4-5)`
        //    pair from a previous PATCH that targeted a provider that has
        //    since been deleted from `alms.toml`.
        let mut gateway_config = crate::gateway::GatewayConfig::default();
        assert_eq!(
            gateway_config.llm_config.provider, "openrouter",
            "test relies on LlmConfig::default().provider being openrouter; \
             update the assertion below if that changes"
        );
        assert_eq!(
            gateway_config.llm_config.default_model, "z-ai/glm-5.2",
            "test relies on LlmConfig::default().default_model being \
             z-ai/glm-5.2; update the assertion below if that changes"
        );
        gateway_config.llm_config.providers.insert(
            "openrouter".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::OpenAiCompatible,
                base_url: "https://openrouter.ai/api/v1".into(),
                api_key_env: None,
                api_key: Some("sk-or-from-provider-entry".into()),
                model: None,
                auth_scheme: alms_core::config::AuthScheme::Bearer,
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let tmp = tempfile::tempdir().unwrap();
        gateway_config.data_dir = Some(tmp.path().to_path_buf());

        // 2. Persisted: provider points at a name absent from
        //    `llm_config.providers`, AND a namespace-specific model that
        //    only makes sense on the dropped provider.
        let persisted = PersistedSettings {
            context: None,
            session: None,
            tools: None,
            llm: None,
            model: Some("claude-haiku-4-5".into()),
            provider: Some("anthropic-removed".into()),
        };
        std::fs::write(
            settings_path(tmp.path()),
            serde_json::to_string_pretty(&persisted).unwrap(),
        )
        .unwrap();

        // 3. Construct AppState — exercises the boot path.
        let gateway = crate::gateway::Gateway::new(gateway_config).unwrap();
        let scheduler = std::sync::Arc::new(alms_runtime::Scheduler::new());
        let shutdown_token = tokio_util::sync::CancellationToken::new();
        let (completion_tx, _cr) = tokio::sync::mpsc::unbounded_channel();
        let (trigger_tx, _tr) = tokio::sync::mpsc::channel(8);
        let (dm_event_tx, _dr) = tokio::sync::mpsc::channel(8);
        let state = crate::server::AppState::new(
            gateway,
            scheduler,
            shutdown_token,
            completion_tx,
            trigger_tx,
            dm_event_tx,
        )
        .unwrap();

        // 4. The boot path must have dropped BOTH the stale provider AND
        //    the stale model. The LlmClient and the `server_llm_default`
        //    surface must reflect the in-file default pair end-to-end.
        assert_eq!(
            state.llm.read().provider(),
            "openrouter",
            "boot must skip persisted provider when no longer configured"
        );
        assert_eq!(
            state.llm.read().default_model(),
            "z-ai/glm-5.2",
            "boot must skip persisted model when its persisted provider is \
             invalid — the model is namespace-specific and would be an \
             unresolvable slug on the fallback provider"
        );
        let default = state.server_llm_default.read().clone();
        assert_eq!(
            default.provider, "openrouter",
            "server_llm_default.provider must mirror the in-file default"
        );
        assert_eq!(
            default.model, "z-ai/glm-5.2",
            "server_llm_default.model must mirror the in-file default — \
             not the dropped persisted model"
        );
        // Cross-check `state.llm_config` (the by-value snapshot used by
        // GET /settings until the next restart) — both fields must reflect
        // the in-file default pair, not the dropped persisted pair.
        assert_eq!(state.llm_config.provider, "openrouter");
        assert_eq!(state.llm_config.default_model, "z-ai/glm-5.2");
    }

    /// When the persisted provider is absent (operator pinned only the
    /// model via PATCH /settings, against the active in-file provider),
    /// the persisted model override must still apply. This pins the
    /// behavior that #1088's fix does NOT regress operators who only
    /// PATCHed `model`.
    #[tokio::test]
    async fn boot_applies_persisted_model_when_persisted_provider_absent() {
        let mut gateway_config = crate::gateway::GatewayConfig::default();
        gateway_config.llm_config.providers.insert(
            "openrouter".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::OpenAiCompatible,
                base_url: "https://openrouter.ai/api/v1".into(),
                api_key_env: None,
                api_key: Some("sk-or-from-provider-entry".into()),
                model: None,
                auth_scheme: alms_core::config::AuthScheme::Bearer,
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let tmp = tempfile::tempdir().unwrap();
        gateway_config.data_dir = Some(tmp.path().to_path_buf());

        let persisted = PersistedSettings {
            context: None,
            session: None,
            tools: None,
            llm: None,
            // Operator pinned a different OpenRouter-namespace model via
            // PATCH; no provider override (active provider stays
            // openrouter from `alms.toml`).
            model: Some("openai/gpt-4o-mini".into()),
            provider: None,
        };
        std::fs::write(
            settings_path(tmp.path()),
            serde_json::to_string_pretty(&persisted).unwrap(),
        )
        .unwrap();

        let gateway = crate::gateway::Gateway::new(gateway_config).unwrap();
        let scheduler = std::sync::Arc::new(alms_runtime::Scheduler::new());
        let shutdown_token = tokio_util::sync::CancellationToken::new();
        let (completion_tx, _cr) = tokio::sync::mpsc::unbounded_channel();
        let (trigger_tx, _tr) = tokio::sync::mpsc::channel(8);
        let (dm_event_tx, _dr) = tokio::sync::mpsc::channel(8);
        let state = crate::server::AppState::new(
            gateway,
            scheduler,
            shutdown_token,
            completion_tx,
            trigger_tx,
            dm_event_tx,
        )
        .unwrap();

        assert_eq!(state.llm.read().provider(), "openrouter");
        assert_eq!(
            state.llm.read().default_model(),
            "openai/gpt-4o-mini",
            "persisted model must apply when persisted provider is absent — \
             #1088 fix must not regress the model-only PATCH path"
        );
        let default = state.server_llm_default.read().clone();
        assert_eq!(default.provider, "openrouter");
        assert_eq!(default.model, "openai/gpt-4o-mini");
    }

    // ==================================================================
    // Codex follow-up on #1081 (P1 #2) — budget revalidation against
    // the post-patch default LLM pair.
    //
    // Pre-fix: `validate_patch_budget` consulted
    // `state.llm_config.provider` / `state.llm_config.default_model`
    // (the boot-time clone) regardless of any `model` / `provider` in
    // the same PATCH body. A PATCH that simultaneously raised
    // `context.max_input_tokens` AND switched to a stricter pair would
    // pass the boot-time check and persist an over-budget pair that
    // later rejected runs on restart.
    //
    // Post-fix: the caller computes the candidate `(provider, model)`
    // (body value with live `server_llm_default` as fallback) and
    // passes it into `validate_patch_budget`, which uses it as
    // `server_provider` / `server_model` for the layer-1 budget check.
    // ==================================================================

    /// PATCH that raises `max_input_tokens` to a value that fits the
    /// CURRENT default but overshoots a stricter provider/model in the
    /// SAME PATCH body is rejected with `INVALID_TOKEN_BUDGET_FOR_PROVIDER`
    /// against the POST-patch pair.
    #[tokio::test]
    async fn patch_revalidates_budget_against_post_patch_default_pair() {
        let mut state = settings_test_app_state();
        // Seed Anthropic provider (kind matters for the compat check
        // and for budget-validator wire-cap lookup).
        state.llm_config.providers.insert(
            "anthropic".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: None,
                model: None,
                auth_scheme: alms_core::config::AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        // Force strict mode so the validator returns a 400 envelope (it
        // also respects ALMS_TOKEN_BUDGET_VALIDATION env, but we want a
        // deterministic test).
        // SAFETY: tests in this module are not run concurrently across
        // env writes in practice; this matches the existing budget-test
        // pattern in this file.
        unsafe { std::env::set_var("ALMS_TOKEN_BUDGET_VALIDATION", "strict") };
        // Seed live default to a permissive (provider, model) pair that
        // would pass any reasonable budget at 1_000_000 input tokens —
        // OpenRouter caps are sourced from the slug's underlying model;
        // we use a small explicit `max_tokens` instead so the math is
        // tight enough to differentiate.
        {
            let mut snap = state.server_llm_default.write();
            snap.model = "moonshotai/kimi-k2.6".into();
            snap.provider = "openrouter".into();
        }

        // Use a candidate input token count that overshoots Anthropic
        // haiku's 200K cap but fits OpenRouter (the budget validator
        // skips unknown (provider, model) silently, so the pre-fix path
        // would have green-lit this). `claude-haiku-4-5` is in the
        // budget table at 200_000; pick a value strictly above it.
        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                max_input_tokens: Some(500_000),
                ..Default::default()
            }),
            model: Some("claude-haiku-4-5".into()),
            provider: Some("anthropic".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        // Layer-1 budget validator returns 400 (not 422) for
        // INVALID_TOKEN_BUDGET_FOR_PROVIDER. The pre-fix behaviour
        // would have returned 200 because the seeded live-default pair
        // (openrouter, kimi-k2.6) is not in the budget table and the
        // validator silently skips unknown pairs.
        unsafe { std::env::remove_var("ALMS_TOKEN_BUDGET_VALIDATION") };
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "PATCH must be rejected by the budget validator against the \
             post-patch (anthropic, claude-haiku-4-5) pair, not the \
             seeded (openrouter, kimi-k2.6) live-default pair"
        );
        let body_bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(json["error_code"], "INVALID_TOKEN_BUDGET_FOR_PROVIDER");
        // The validator should have looked at the POST-patch pair.
        assert_eq!(json["provider"], "anthropic");
        assert_eq!(json["model"], "claude-haiku-4-5");
    }

    /// Codex follow-up on #1081 (P1 #3): the budget validator overlay
    /// must mirror the commit-path resolution. When a PATCH carries
    /// `provider` (but not `model`) plus `context.max_input_tokens`, the
    /// candidate-model resolution at lines ~455-490 must consult the
    /// provider entry's `model` BEFORE falling back to the live default —
    /// otherwise the validator runs against
    /// `(new_provider, stale_cross_namespace_default)` (an unknown pair
    /// the budget table silently passes) while the commit path later
    /// writes `(new_provider, entry_model)` to `server_llm_default`.
    /// The PATCH returns 200, persistence carries the coherent pair
    /// across restart, and every subsequent run rejects with
    /// `INVALID_TOKEN_BUDGET_FOR_PROVIDER` because the entry model's
    /// real cap was below the budget all along.
    #[tokio::test]
    async fn patch_revalidates_budget_against_provider_entry_fallback_model() {
        let mut state = settings_test_app_state();
        // Seed Anthropic provider WITH an entry-level `model`. This is
        // the same setup the Finding #B commit-path test uses — the
        // entry-model fallback is what makes the budget validator see
        // the post-patch (anthropic, claude-haiku-4-5) pair instead of
        // the stale openrouter slug.
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
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        // Force strict mode so the validator returns a 400 envelope.
        // SAFETY: tests in this module are not run concurrently across
        // env writes in practice; this matches the existing budget-test
        // pattern in this file (see
        // `patch_revalidates_budget_against_post_patch_default_pair`).
        unsafe { std::env::set_var("ALMS_TOKEN_BUDGET_VALIDATION", "strict") };
        // Seed live default to a cross-namespace pair the budget table
        // does not recognise (so the pre-fix validator silently passes).
        {
            let mut snap = state.server_llm_default.write();
            snap.model = "moonshotai/kimi-k2.6".into();
            snap.provider = "openrouter".into();
        }

        // Provider-only switch plus an over-budget `max_input_tokens` —
        // 500K overshoots Anthropic haiku's 200K cap. The PATCH body
        // intentionally omits `model` so we can verify the overlay
        // consults the entry-level fallback. Without the fix the
        // validator runs against `(anthropic, moonshotai/kimi-k2.6)`,
        // doesn't find that pair in the budget table, and silently
        // green-lights the request.
        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                max_input_tokens: Some(500_000),
                ..Default::default()
            }),
            provider: Some("anthropic".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        unsafe { std::env::remove_var("ALMS_TOKEN_BUDGET_VALIDATION") };
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "PATCH must be rejected by the budget validator against the \
             post-patch (anthropic, claude-haiku-4-5) pair the commit path \
             would later persist via the provider-entry fallback, NOT the \
             seeded (openrouter, kimi-k2.6) live default"
        );
        let body_bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(json["error_code"], "INVALID_TOKEN_BUDGET_FOR_PROVIDER");
        // The validator should have looked at the POST-patch pair
        // resolved via the entry fallback.
        assert_eq!(json["provider"], "anthropic");
        assert_eq!(json["model"], "claude-haiku-4-5");
    }

    /// Codex follow-up on #1081 (P1, Finding 5, refs issue #1086 Item 2):
    /// the budget-overlay entry-model fallback must mirror the commit
    /// path's same-provider no-op guard. When `body.provider` matches the
    /// live default provider, the overlay must NOT consult
    /// `[llm.providers.<body.provider>].model` — because the commit path
    /// won't either (it keeps the live default model). Validating the
    /// budget against a tighter entry-model that won't be persisted yields
    /// a false-positive rejection.
    ///
    /// Setup: live default `(anthropic, claude-sonnet-4-6)` (a permissive
    /// 1M-token cap pair); `[llm.providers.anthropic].model =
    /// claude-haiku-4-5` (200K cap — what operators typically pin in
    /// `alms.toml` as a per-provider default). PATCH
    /// `{provider: "anthropic", max_input_tokens: 500_000}` carries a
    /// budget that fits the live default but overshoots the entry model.
    ///
    /// Pre-fix: overlay resolves candidate model to `claude-haiku-4-5`
    /// (entry fallback fires unconditionally on `body.provider`). Budget
    /// validator rejects 500K > 200K with
    /// `INVALID_TOKEN_BUDGET_FOR_PROVIDER`. The PATCH returns 400 even
    /// though the commit path would have kept `claude-sonnet-4-6`
    /// (fitting 500K fine) — a false-positive rejection.
    ///
    /// Post-fix: overlay's entry-model fallback is gated on
    /// `body.provider != live.provider`. Same-provider PATCH keeps the
    /// live default model in the overlay; validator sees
    /// `(anthropic, claude-sonnet-4-6)` (the same pair the commit path
    /// would persist) and passes the request through to the structural
    /// validation layer. PATCH returns 200.
    #[tokio::test]
    async fn patch_same_provider_does_not_use_entry_model_for_budget_overlay() {
        let mut state = settings_test_app_state();
        // Seed an anthropic entry whose `model` field is INTENTIONALLY
        // tighter (haiku — 200K cap) than the live default model (sonnet
        // — 1M cap). This is the asymmetry trap the symmetry fix closes:
        // the entry-model fallback would budget-check against haiku even
        // though the commit path will keep sonnet (same-provider no-op).
        state.llm_config.providers.insert(
            "anthropic".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: None,
                // Entry-level model: haiku 200K. The live default below
                // is sonnet 1M. 500K fits sonnet but overshoots haiku.
                model: Some("claude-haiku-4-5".into()),
                auth_scheme: alms_core::config::AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        // Force strict mode so a budget overshoot would surface as 400
        // (pre-fix observable failure mode).
        // SAFETY: tests in this module are not run concurrently across
        // env writes in practice; matches the existing budget-test
        // pattern (see other `patch_revalidates_budget_*` tests).
        unsafe { std::env::set_var("ALMS_TOKEN_BUDGET_VALIDATION", "strict") };
        // Live default `(anthropic, claude-sonnet-4-6)` — same provider
        // as the PATCH body, but a different (permissive) model. The
        // commit path will keep this model because the provider is
        // unchanged.
        {
            let mut snap = state.server_llm_default.write();
            snap.model = "claude-sonnet-4-6".into();
            snap.provider = "anthropic".into();
        }

        // Same-provider idempotent PATCH with a budget that:
        //   - fits claude-sonnet-4-6 (1M — the live default, what the
        //     commit path keeps)
        //   - overshoots claude-haiku-4-5 (200K — the entry-model trap)
        // 500K is the canonical "fits live, overshoots entry" probe.
        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                max_input_tokens: Some(500_000),
                ..Default::default()
            }),
            provider: Some("anthropic".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        unsafe { std::env::remove_var("ALMS_TOKEN_BUDGET_VALIDATION") };

        // Pre-fix this would have been 400 with
        // `INVALID_TOKEN_BUDGET_FOR_PROVIDER` against
        // `(anthropic, claude-haiku-4-5)`. Post-fix the overlay matches
        // the commit-path resolution and the PATCH passes.
        let status = resp.status();
        let body_bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert_eq!(
            status,
            StatusCode::OK,
            "same-provider PATCH must not use entry-model fallback for \
             budget overlay — the commit path keeps the live default \
             model, so the overlay must too. Mismatched overlay yields a \
             false-positive rejection. Got status {status} with body {:?}",
            String::from_utf8_lossy(&body_bytes),
        );

        // Confirm the live default `model` was NOT rewritten to the
        // entry-model — the same-provider commit-path no-op guard
        // (`patch_provider_only_noop_does_not_mutate_default_or_rebuild_the_client`
        // pins this separately, but we double-check here so a regression
        // that breaks both guards is caught by either test).
        let live = state.server_llm_default.read().clone();
        assert_eq!(live.provider, "anthropic");
        assert_eq!(
            live.model, "claude-sonnet-4-6",
            "same-provider PATCH must leave the live default model \
             untouched"
        );

        // Confirm the context.max_input_tokens did commit (200 status
        // means the partial-update contract proceeded; the budget knob
        // is the only mutation in this body).
        let agent = state.agent_config.read();
        assert_eq!(agent.context_config.max_input_tokens, 500_000);
    }

    /// The compat gate's half of the same symmetry (Tim review on #1271).
    ///
    /// `patch_same_provider_does_not_use_entry_model_for_budget_overlay`
    /// above pins the budget overlay; this pins Phase 1b. Both sites
    /// resolve an entry-level model candidate, and the commit guard
    /// adopts one only when the provider actually *changes* — so any site
    /// that resolves it unconditionally is judging a value the same body
    /// will then decline to commit.
    ///
    /// The reachable shape is a misconfigured `alms.toml`: an
    /// `[llm.providers.anthropic]` whose `model` is from another
    /// namespace. Nothing rejects that at boot, and it never reaches the
    /// wire, because a provider switch onto that entry is exactly what
    /// the compat gate stops. But an *idempotent* `{"provider":
    /// "anthropic"}` — the body the settings UI sends when the operator
    /// re-saves without touching the model — used to draw a `422` naming
    /// `openai/gpt-4o-mini`: a model the operator never sent, that is not
    /// the live default, and that the commit path would have discarded.
    /// A rejection an operator cannot act on.
    ///
    /// This is a spurious *rejection*, not a leak — the gate did stop,
    /// and `no_rejected_pair_body_moves_the_live_client` still holds. So
    /// the assertion that matters is the status, and the mutation it
    /// kills is deleting `.filter(|_| provider != live_pair.provider)`
    /// from `entry_model`.
    #[tokio::test]
    async fn idempotent_provider_patch_is_not_judged_against_the_entry_model() {
        let mut state = settings_test_app_state();
        state.llm_config.providers.insert(
            "anthropic".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: None,
                // The misconfiguration: a strict (`Anthropic`) entry
                // carrying a model from the OpenAI namespace. This value
                // is only ever a *candidate* — the commit guard never
                // adopts it on a same-provider PATCH — so an
                // unconditional compat check is the only way it can
                // affect a response.
                model: Some("openai/gpt-4o-mini".into()),
                auth_scheme: alms_core::config::AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        // Live default `(anthropic, claude-sonnet-4-6)`: coherent, and
        // the pair the PATCH below leaves in place.
        {
            let mut snap = state.server_llm_default.write();
            snap.provider = "anthropic".into();
            snap.model = "claude-sonnet-4-6".into();
        }
        // Desynchronise the live client so a rebuild would be observable.
        {
            let mut llm = state.llm.write();
            *llm = llm.clone().with_model("sentinel-not-the-default");
        }

        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::json!({ "provider": "anthropic" })),
        )
        .await
        .into_response();

        let status = resp.status();
        let body_bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body_text = String::from_utf8_lossy(&body_bytes);
        assert_eq!(
            status,
            StatusCode::OK,
            "an idempotent provider PATCH must be judged against the pair              it would actually leave live — `(anthropic,              claude-sonnet-4-6)` — not against              `[llm.providers.anthropic].model`, which the commit path              discards on a same-provider body. Got {status} with {body_text:?}"
        );
        assert!(
            !body_text.contains("openai/gpt-4o-mini"),
            "no response to this body may name the entry model: the              operator never sent it and it never goes live. Got {body_text:?}"
        );

        // And the no-op stays a no-op end to end.
        let live = state.server_llm_default.read().clone();
        assert_eq!(live.provider, "anthropic");
        assert_eq!(
            live.model, "claude-sonnet-4-6",
            "the entry model must not be adopted on a same-provider PATCH"
        );
        assert_eq!(
            state.llm.read().default_model(),
            "sentinel-not-the-default",
            "and nothing changed, so the live client must not be rebuilt"
        );
    }

    /// Codex follow-up on #1081 (P2 #4): a no-op provider PATCH
    /// (`{provider: <same-as-current>}`) must NOT mutate the live
    /// default `(provider, model)` pair. Pre-fix, the commit path
    /// unconditionally fell back to `entry_model` when `body.model` was
    /// absent, so an idempotent `{provider: "anthropic"}` against a live
    /// `(anthropic, claude-sonnet-4-6)` default silently rewrote the model
    /// to the entry's `claude-haiku-4-5`.
    ///
    /// #1148 repointed the second half of this test. It used to assert
    /// that the response carried no `restart_required: true`; that field
    /// no longer exists on the wire, so the assertion could never fail.
    /// The live client is what the guard actually protects now — a lost
    /// no-op guard rebuilds it, and the rebuild is observable.
    #[tokio::test]
    async fn patch_provider_only_noop_does_not_mutate_default_or_rebuild_the_client() {
        let mut state = settings_test_app_state();
        state.llm_config.providers.insert(
            "anthropic".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: None,
                // Entry-level default model is intentionally DIFFERENT
                // from the live `server_llm_default.model` so the pre-fix
                // fallback would observably overwrite it — making this
                // test fail loudly without the guard.
                model: Some("claude-haiku-4-5".into()),
                auth_scheme: alms_core::config::AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        // Seed live default to `(anthropic, claude-sonnet-4-6)` — same
        // provider as the PATCH body, but a different model than the
        // entry-level fallback.
        {
            let mut snap = state.server_llm_default.write();
            snap.model = "claude-sonnet-4-6".into();
            snap.provider = "anthropic".into();
        }
        // Desynchronise the live client from that pair so a rebuild would
        // be observable: only a rebuild pulls the client back onto it.
        {
            let mut llm = state.llm.write();
            *llm = llm.clone().with_model("sentinel-not-the-default");
        }

        let body = PatchSettingsRequest {
            provider: Some("anthropic".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        // Core assertion: the live default `(provider, model)` pair is
        // byte-identical to its pre-PATCH state.
        let live = state.server_llm_default.read().clone();
        assert_eq!(
            live.provider, "anthropic",
            "no-op provider PATCH must leave `provider` unchanged"
        );
        assert_eq!(
            live.model, "claude-sonnet-4-6",
            "no-op provider PATCH must NOT overwrite `model` with the \
             provider-entry fallback — that fallback is reserved for \
             actual provider switches (Finding #B)"
        );

        // And the live client is untouched. The sentinel above is only
        // reachable by NOT rebuilding: any rebuild re-applies the provider
        // and then `server_llm_default.model`, landing on
        // `claude-sonnet-4-6`. Pre-fix this branch set
        // `llm_default_changed = true` on every provider write.
        assert_eq!(
            state.llm.read().default_model(),
            "sentinel-not-the-default",
            "a no-op provider PATCH must not rebuild the live client — the \
             rebuild re-resolves the API key and churns the run path's \
             client for a request that changed nothing"
        );
    }

    /// Tim follow-up on #1081: the reverse symmetry of the provider-only
    /// no-op guard. A `{model: <same-as-current>}` PATCH must not be
    /// treated as a change. Pre-fix, the standalone `body.model` branch
    /// wrote the value unconditionally and set `llm_default_changed = true`
    /// on every model PATCH — including the no-op case where the operator
    /// re-submitted the live default value.
    ///
    /// #1148 repointed the wire assertion the same way as its
    /// provider-only twin: `restart_required` is gone from the response,
    /// so asserting its absence proved nothing. The live client does.
    #[tokio::test]
    async fn patch_model_only_noop_does_not_rebuild_the_live_client() {
        let mut state = settings_test_app_state();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        // Seed live default to a known model and capture the pre-PATCH
        // pair for the byte-identical assertion below.
        {
            let mut snap = state.server_llm_default.write();
            snap.model = "moonshotai/kimi-k2.6".into();
            snap.provider = "openrouter".into();
        }
        let before = state.server_llm_default.read().clone();
        // Desynchronise the live client so a rebuild would be observable.
        {
            let mut llm = state.llm.write();
            *llm = llm.clone().with_model("sentinel-not-the-default");
        }

        // PATCH with the same model value already live.
        let body = PatchSettingsRequest {
            model: Some("moonshotai/kimi-k2.6".into()),
            ..Default::default()
        };
        let resp = patch_settings(
            axum::extract::State(state.clone()),
            Json(serde_json::to_value(&body).unwrap()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        // Core assertion: the live default `(provider, model)` pair is
        // byte-identical to its pre-PATCH state.
        let after = state.server_llm_default.read().clone();
        assert_eq!(
            after.model, before.model,
            "no-op model PATCH must not mutate `model`"
        );
        assert_eq!(
            after.provider, before.provider,
            "no-op model PATCH must not touch `provider`"
        );

        // Live-client assertion: pre-fix the model branch unconditionally
        // set `llm_default_changed = true`, which rebuilds the client every
        // run resolves from and re-resolves its API key.
        assert_eq!(
            state.llm.read().default_model(),
            "sentinel-not-the-default",
            "a no-op model PATCH must not rebuild the live client"
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

    /// PR #1194 (Tim's nit): the PATCH clear sentinel is trim-aware, like
    /// the TOML path — a whitespace-only value clears the pair instead of
    /// being treated as a provider name / model slug (pre-fix, `"  "` fell
    /// through to the provider lookup and 422'd with
    /// `SUMMARY_PROVIDER_UNKNOWN`).
    #[tokio::test]
    async fn patch_treats_whitespace_only_summary_pair_as_clear() {
        let state = settings_test_app_state_with_openrouter();
        {
            let mut agent = state.agent_config.write();
            agent.context_config.summary_provider = Some("openrouter".into());
            agent.context_config.summary_model = Some("google/gemma-4-31b-it".into());
        }

        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                summary_provider: Some("  ".into()),
                summary_model: Some(" \t ".into()),
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

    /// #1191 / PR #1194 (Codex P2) end-to-end round-trip: PATCH clears the
    /// summary pair → `persist_settings` writes the `""` sentinel for BOTH
    /// fields (not absent fields) → a simulated reboot applies the persisted
    /// overrides onto a fresh compiled-default `ContextConfig` → the pair
    /// stays cleared instead of resurrecting the compiled default.
    ///
    /// Pre-fix: the clear was persisted as omitted fields, `apply_to` only
    /// reapplied `Some` values, and the next boot fell back to
    /// `Some("google/gemma-4-31b-it")` / `Some("openrouter")` — breaking
    /// non-OpenRouter deployments that cleared the pair precisely because
    /// they have no OpenRouter key.
    #[tokio::test]
    async fn patch_clear_of_summary_pair_survives_restart() {
        // ── 1. PATCH ────────────────────────────────────────────────────
        let mut state = settings_test_app_state_with_openrouter();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        {
            let mut agent = state.agent_config.write();
            agent.context_config.summary_provider = Some("openrouter".into());
            agent.context_config.summary_model = Some("google/gemma-4-31b-it".into());
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

        // ── 2. PERSIST ─────────────────────────────────────────────────
        // The on-disk overrides must carry the EXPLICIT `""` sentinel for
        // both fields — an absent field would mean "not overridden" on
        // reload and resurrect the compiled default pair.
        let raw = std::fs::read_to_string(settings_path(&state.data_dir)).unwrap();
        let persisted: PersistedSettings = serde_json::from_str(&raw).unwrap();
        let ctx_overrides = persisted.context.expect("context block must be persisted");
        assert_eq!(
            ctx_overrides.summary_model.as_deref(),
            Some(""),
            "cleared summary_model must persist as the \"\" sentinel, not an absent field"
        );
        assert_eq!(
            ctx_overrides.summary_provider.as_deref(),
            Some(""),
            "cleared summary_provider must persist as the \"\" sentinel, not an absent field"
        );

        // ── 3. REBOOT ──────────────────────────────────────────────────
        // `AppState::new` starts from the resolved config (compiled
        // defaults + TOML + env) and applies the persisted context
        // overrides on top — replicate exactly that merge here.
        let mut boot_ctx = alms_core::config::ContextConfig::default();
        ctx_overrides.apply_to(&mut boot_ctx);
        assert!(
            boot_ctx.summary_model.is_none(),
            "the clear must survive restart — compiled default summary_model resurrected"
        );
        assert!(
            boot_ctx.summary_provider.is_none(),
            "the clear must survive restart — compiled default summary_provider resurrected"
        );
    }

    /// PR #1194 (Codex P2, round 2): `persist_settings` must NOT pin the
    /// compiled default summary pair into `settings.json`. It runs after
    /// every successful PATCH, so a PATCH to an *unrelated* knob on a
    /// deployment that never touched the pair would otherwise persist
    /// `Some(compiled default)` as an explicit override — which overlays
    /// TOML on the next boot and clobbers the documented `alms.toml`
    /// `summary_model = ""` / `summary_provider = ""` opt-out.
    #[tokio::test]
    async fn unrelated_patch_does_not_pin_compiled_default_summary_pair() {
        let mut state = settings_test_app_state();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        // Live pair sits at the compiled default (never touched).
        {
            let defaults = alms_core::config::ContextConfig::default();
            let mut agent = state.agent_config.write();
            agent.context_config.summary_provider = defaults.summary_provider.clone();
            agent.context_config.summary_model = defaults.summary_model.clone();
        }

        // PATCH an unrelated knob.
        let body = PatchSettingsRequest {
            session: Some(PatchSession {
                max_messages: Some(4321),
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

        // The persisted context block must leave BOTH summary fields
        // absent — not pinned to the compiled default.
        let raw = std::fs::read_to_string(settings_path(&state.data_dir)).unwrap();
        assert!(
            !raw.contains("summary_model") && !raw.contains("summary_provider"),
            "unrelated PATCH must not persist the compiled default summary pair — got: {raw}"
        );
        let persisted: PersistedSettings = serde_json::from_str(&raw).unwrap();
        let ctx_overrides = persisted.context.expect("context block must be persisted");
        assert_eq!(ctx_overrides.summary_model, None);
        assert_eq!(ctx_overrides.summary_provider, None);

        // Reboot half: applying these overrides onto a TOML-cleared config
        // must NOT resurrect the compiled default pair — the exact clobber
        // this fix prevents.
        let mut boot_ctx = alms_core::config::ContextConfig {
            // Simulates the `alms.toml` `""`/`""` opt-out, which the
            // deserializer normalizes to a cleared pair.
            summary_model: None,
            summary_provider: None,
            ..Default::default()
        };
        ctx_overrides.apply_to(&mut boot_ctx);
        assert!(
            boot_ctx.summary_model.is_none(),
            "persisted overrides must not clobber the alms.toml summary_model opt-out"
        );
        assert!(
            boot_ctx.summary_provider.is_none(),
            "persisted overrides must not clobber the alms.toml summary_provider opt-out"
        );
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
            model: Some("test-model".to_string()),
            provider: Some("openrouter".to_string()),
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
        // Codex follow-up on #1081 (P1 #2): the PATCH-time budget
        // validator now consults `server_llm_default` (overlaid with any
        // body-supplied `provider` / `model`) rather than the boot-time
        // `state.llm_config` clone, so that a same-PATCH default-pair
        // switch revalidates against the post-PATCH wire. Mirror the
        // baseline pair onto `server_llm_default` here so the existing
        // budget tests continue to exercise the (anthropic, haiku) cap.
        {
            let mut snap = state.server_llm_default.write();
            snap.provider = "anthropic".into();
            snap.model = "claude-haiku-4-5".into();
        }
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
        // model=z-ai/glm-5.2 — both unknown to the budget table.
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
        let (trigger_tx, _tr) = tokio::sync::mpsc::channel(8);
        let (dm_event_tx, _dr) = tokio::sync::mpsc::channel(8);
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

    // ==================================================================
    // PATCH /settings § session — validate-then-commit (#1275)
    //
    // The section used to commit every named field first and cross-
    // validate afterwards, with a "revert" that wrote `ctx_max` rather
    // than the pre-PATCH value. These tests pin the properties the
    // two-phase restructure buys; the first three are the ones that die
    // when the Phase 2 gate (`session_ok`) is forced open, and the fourth
    // is what dies if Phase 2 stops committing at all.
    // ==================================================================

    /// Coherent baseline — context window 128_000, session storage
    /// 256_000 — plus recognisable non-default values on every other
    /// session knob, so a commit is always distinguishable from the
    /// defaults.
    fn session_invariant_test_state() -> crate::server::AppState {
        let state = settings_test_app_state();
        state.agent_config.write().context_config.max_input_tokens = 128_000;
        {
            let mut sess = state.session_config.write();
            sess.max_messages = 200;
            sess.max_context_tokens = 256_000;
            sess.idle_timeout_secs = 3600;
            sess.auto_archive = true;
            sess.archive_ttl_secs = 7200;
        }
        state
    }

    /// A rejected `session` PATCH leaves *every* session field at its
    /// pre-PATCH value — `max_context_tokens` included. Pre-fix the
    /// handler committed all five fields before the cross-section check
    /// ran and then "reverted" `max_context_tokens` to the context
    /// window, so a 422 landed four of the operator's values plus a fifth
    /// number nobody had sent.
    #[tokio::test]
    async fn rejected_session_patch_leaves_every_field_at_its_pre_patch_value() {
        use axum::body::to_bytes;
        use axum::response::IntoResponse;

        let mut state = session_invariant_test_state();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        // Every knob named, with `max_context_tokens` below the live
        // context window — so the section is rejected as a whole.
        let body = PatchSettingsRequest {
            session: Some(PatchSession {
                max_messages: Some(50),
                max_context_tokens: Some(64_000),
                idle_timeout_secs: Some(99),
                auto_archive: Some(false),
                archive_ttl_secs: Some(111),
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
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            json["errors"][0]
                .as_str()
                .is_some_and(|e| e.contains("session.max_context_tokens")),
            "the 422 must name the field at fault; got: {json}"
        );

        let sess = state.session_config.read();
        assert_eq!(
            sess.max_context_tokens, 256_000,
            "must hold the pre-PATCH value — not 64_000 (the rejected value) \
             and not 128_000 (the old 'revert to ctx_max', a number the \
             operator never sent)"
        );
        assert_eq!(
            sess.max_messages, 200,
            "no session field may commit behind the section's own rejection"
        );
        assert_eq!(sess.idle_timeout_secs, 3600);
        assert!(sess.auto_archive);
        assert_eq!(sess.archive_ttl_secs, 7200);
    }

    /// A `session` body that does not name `max_context_tokens` cannot
    /// change it under any outcome.
    ///
    /// The old check fired for *any* `body.session`, not just bodies
    /// touching the invariant. Once `context.max_input_tokens` had been
    /// raised past the live session storage — reachable through a plain
    /// `{"context": ...}` PATCH, which never runs this check —
    /// `{"session": {"max_messages": 50}}` returned 422 *and* rewrote
    /// `max_context_tokens`. The operator asked to change a message cap,
    /// got an error, and a different setting moved.
    #[tokio::test]
    async fn session_patch_that_does_not_name_max_context_tokens_cannot_change_it() {
        use axum::response::IntoResponse;

        let mut state = session_invariant_test_state();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();
        // Live state already violates the invariant, as an earlier
        // context-only PATCH could leave it.
        state.agent_config.write().context_config.max_input_tokens = 300_000;

        let body = PatchSettingsRequest {
            session: Some(PatchSession {
                max_messages: Some(50),
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
            "a body naming neither half of the invariant cannot make the \
             relationship worse, so it must not be judged against it"
        );
        let sess = state.session_config.read();
        assert_eq!(
            sess.max_context_tokens, 256_000,
            "a field the body never named must not move under any outcome"
        );
        assert_eq!(sess.max_messages, 50, "the named field must commit");
    }

    /// The invariant's other half. A body that raises
    /// `context.max_input_tokens` above the live session storage *does*
    /// touch the invariant, so it is still rejected — and the session
    /// section commits nothing, including the unrelated field it named.
    #[tokio::test]
    async fn session_patch_rejected_via_the_context_half_commits_no_session_field() {
        use axum::response::IntoResponse;

        let _env = crate::test_env_locks::BudgetValidationEnvGuard::unset();

        let mut state = session_invariant_test_state();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        let body = PatchSettingsRequest {
            context: Some(PatchContext {
                max_input_tokens: Some(300_000),
                ..Default::default()
            }),
            session: Some(PatchSession {
                max_messages: Some(50),
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
        let sess = state.session_config.read();
        assert_eq!(
            sess.max_messages, 200,
            "the whole session section is gated on its own validation"
        );
        assert_eq!(sess.max_context_tokens, 256_000);
        // The context section validates and commits on its own terms; its
        // value landing beside a 422 from another section is the
        // documented `status: "partial"` cross-section contract.
        assert_eq!(
            state.agent_config.read().context_config.max_input_tokens,
            300_000
        );
    }

    /// The invariant's accepting boundary, and the only path that reaches
    /// it expecting a 200.
    ///
    /// Structural gap this closes: every other test that evaluates the
    /// rule expects a rejection, so the `unwrap_or(live_max_context_tokens)`
    /// fallback was only ever exercised on a body already heading for a
    /// 422. This body names no session half, so the *live* value is what
    /// the rule must judge — `unwrap_or(0)` turns this 200 into a 422.
    /// Equality then pins the comparison itself: session storage equal to
    /// the context window satisfies "at least one full context window",
    /// so `<` → `<=` fails here too. One test, both mutants.
    #[tokio::test]
    async fn context_half_raised_to_the_live_session_ceiling_is_accepted() {
        use axum::response::IntoResponse;

        let _env = crate::test_env_locks::BudgetValidationEnvGuard::unset();

        let mut state = session_invariant_test_state();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        let body = PatchSettingsRequest {
            // Raised to exactly the live session storage (256_000) —
            // equal, not above, so the invariant still holds.
            context: Some(PatchContext {
                max_input_tokens: Some(256_000),
                ..Default::default()
            }),
            // A session block is required to reach the rule at all, but
            // it names neither half of it.
            session: Some(PatchSession {
                max_messages: Some(50),
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
            "session storage equal to the context window satisfies \
             'must be >=' — the section must commit, not reject"
        );
        let sess = state.session_config.read();
        assert_eq!(sess.max_messages, 50, "the named session field commits");
        assert_eq!(
            sess.max_context_tokens, 256_000,
            "the body named no session half, so the live value is what the \
             rule judged — and it must not move"
        );
        assert_eq!(
            state.agent_config.read().context_config.max_input_tokens,
            256_000,
            "the context half commits on its own terms"
        );
    }

    /// Phase 2 commits. Without this the three gate tests above would all
    /// pass against a handler that never writes anything at all.
    #[tokio::test]
    async fn accepted_session_patch_commits_every_named_field() {
        use axum::response::IntoResponse;

        let mut state = session_invariant_test_state();
        let tmp = tempfile::tempdir().unwrap();
        state.data_dir = tmp.path().to_path_buf();

        let body = PatchSettingsRequest {
            session: Some(PatchSession {
                max_messages: Some(50),
                // >= the live 128_000 context window.
                max_context_tokens: Some(512_000),
                idle_timeout_secs: Some(99),
                auto_archive: Some(false),
                archive_ttl_secs: Some(111),
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
        let sess = state.session_config.read();
        assert_eq!(sess.max_messages, 50);
        assert_eq!(sess.max_context_tokens, 512_000);
        assert_eq!(sess.idle_timeout_secs, 99);
        assert!(!sess.auto_archive);
        assert_eq!(sess.archive_ttl_secs, 111);
    }
}
