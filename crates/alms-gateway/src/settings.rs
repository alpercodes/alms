//! Settings endpoints — exposes server-side defaults for UI pre-population
//! and accepts partial config updates via PATCH.
//!
//! Server-level settings (context, session, tools) are persisted to
//! `{data_dir}/settings.json` so they survive restarts.

use crate::server::AppState;
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::info;

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
            "recent_window": ctx.recent_window,
            "summary_interval": ctx.summary_interval,
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
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct PatchContext {
    pub strategy: Option<String>,
    pub max_input_tokens: Option<usize>,
    pub recent_window: Option<usize>,
    pub summary_interval: Option<usize>,
    pub summary_model: Option<String>,
    /// Separate provider for the summary task (#866). Empty string clears
    /// back to "inherit agent provider"; non-empty must reference a
    /// configured `[llm.providers.<name>]` block whose API key is resolvable.
    pub summary_provider: Option<String>,
}

/// Partial session config update.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct PatchSession {
    pub max_messages: Option<usize>,
    pub max_context_tokens: Option<usize>,
    pub idle_timeout_secs: Option<u64>,
    pub auto_archive: Option<bool>,
    pub archive_ttl_secs: Option<u64>,
}

/// Partial tools config update.
#[derive(Debug, Deserialize, Default)]
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
#[derive(Debug, Deserialize, Default)]
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
#[derive(Debug, Deserialize, Default)]
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
#[derive(Debug, Deserialize, Default)]
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
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct PatchLlm {
    pub anthropic: Option<PatchLlmAnthropic>,
    pub openai: Option<PatchLlmOpenai>,
    pub gemini: Option<PatchLlmGemini>,
}

/// Top-level PATCH /settings request body.
#[derive(Debug, Deserialize, Default)]
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
    Json(body): Json<PatchSettingsRequest>,
) -> impl IntoResponse {
    let mut errors: Vec<String> = Vec::new();

    // ── Context ────────────────────────────────────────────────────────
    if let Some(ctx_patch) = &body.context {
        let mut agent = state.agent_config.write();
        let ctx = &mut agent.context_config;

        if let Some(ref strategy) = ctx_patch.strategy {
            let valid = ["sliding-summary", "full", "truncate"];
            if valid.contains(&strategy.as_str()) {
                ctx.strategy = strategy.clone();
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
        if let Some(v) = ctx_patch.recent_window {
            if v == 0 {
                errors.push("context.recent_window must be > 0".into());
            } else {
                ctx.recent_window = v;
            }
        }
        if let Some(v) = ctx_patch.summary_interval {
            ctx.summary_interval = v;
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
            recent_window = ctx.recent_window,
            summary_interval = ctx.summary_interval,
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

// ── Persistence helpers ───────────────────────────────────────────────

/// Persisted context overrides — only fields the user explicitly changed
/// via PATCH /settings are `Some`. Fields that are `None` fall through to
/// code defaults / TOML / env-var overrides.
///
/// Backward-compatible: old `settings.json` files that contain the full
/// `ContextConfig` will deserialize with all fields set to `Some`, which
/// is functionally equivalent to the old wholesale-replace behavior (but
/// new persists will only write the patchable subset).
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PersistedContextOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recent_window: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_interval: Option<usize>,
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
            ctx.strategy = v.clone();
        }
        if let Some(v) = self.max_input_tokens {
            ctx.max_input_tokens = v;
        }
        if let Some(v) = self.recent_window {
            ctx.recent_window = v;
        }
        if let Some(v) = self.summary_interval {
            ctx.summary_interval = v;
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
            recent_window: Some(ctx.recent_window),
            summary_interval: Some(ctx.summary_interval),
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

    /// Simulate the old settings.json format (full ContextConfig with all fields
    /// present). Verify backward compatibility: all fields deserialize as `Some`.
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
    }

    /// New settings.json format omits run_summary_mode and run_summary_budget.
    /// Verify they deserialize as `None`.
    #[test]
    fn new_format_omits_non_patchable_fields() {
        let new_json = r#"{
            "context": {
                "strategy": "truncate",
                "max_input_tokens": 128000,
                "recent_window": 20,
                "summary_interval": 30
            }
        }"#;
        let persisted: PersistedSettings = serde_json::from_str(new_json).unwrap();
        let ctx = persisted.context.unwrap();
        assert_eq!(ctx.strategy, Some("truncate".into()));
        assert_eq!(ctx.max_input_tokens, Some(128_000));
        // Non-patchable fields should be None
        assert_eq!(ctx.run_summary_mode, None);
        assert_eq!(ctx.run_summary_budget, None);
    }

    /// Context overrides with `None` fields should not overwrite the target.
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

        assert_eq!(ctx.strategy, "sliding-summary");
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
        assert_eq!(ctx.recent_window, 20);
        assert_eq!(ctx.summary_interval, 30);
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
                recent_window: Some(20),
                summary_interval: Some(30),
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
    }

    /// Round-trip: serialize then deserialize preserves values.
    #[test]
    fn round_trip_serialization() {
        let original = PersistedSettings {
            context: Some(PersistedContextOverrides {
                strategy: Some("sliding-summary".into()),
                max_input_tokens: Some(64_000),
                recent_window: Some(10),
                summary_interval: Some(15),
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
        assert_eq!(ctx.strategy, Some("sliding-summary".into()));
        assert_eq!(ctx.max_input_tokens, Some(64_000));
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
        let _resp = patch_settings(axum::extract::State(state.clone()), Json(body)).await;

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
        let _resp = patch_settings(axum::extract::State(state.clone()), Json(body)).await;

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
        let _resp = patch_settings(axum::extract::State(state.clone()), Json(body)).await;

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
        let resp = patch_settings(axum::extract::State(state.clone()), Json(body))
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
        let resp = patch_settings(axum::extract::State(state.clone()), Json(body))
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
            let resp = patch_settings(axum::extract::State(state.clone()), Json(body))
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
            let resp = patch_settings(axum::extract::State(state.clone()), Json(body))
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
        let resp = patch_settings(axum::extract::State(state), Json(body))
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
        let resp = patch_settings(axum::extract::State(state.clone()), Json(body))
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
        let resp = patch_settings(axum::extract::State(state.clone()), Json(body))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        let cfg = state.agent_config.read();
        assert!(cfg.context_config.summary_provider.is_none());
        assert!(cfg.context_config.summary_model.is_none());
    }
}
