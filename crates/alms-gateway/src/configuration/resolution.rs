// SPDX-License-Identifier: Apache-2.0

//! Provider/model and per-agent configuration resolution.

use alms_core::config::ProviderKind;
use tracing::{info, warn};

/// Does `model` belong to the wire-shape namespace of `kind`?
///
/// Used by the #942 cross-namespace drop in [`resolve_agent_config`] to
/// decide whether a per-agent `model` field carried over from before a
/// per-agent provider switch is safe to apply on the new provider's
/// wire. The check is intentionally asymmetric:
///
/// - **`OpenAiCompatible`** is permissive — the kind is a giant tent.
///   OpenAI accepts `gpt-*` / `o*`, OpenRouter accepts vendor-prefixed
///   names from every namespace (`anthropic/claude-*`, `google/gemini-*`,
///   `deepseek/deepseek-*`, …), DeepSeek accepts `deepseek-*`. We do
///   not have enough information at this layer to tell them apart, so
///   we accept every model name and let the wire 404 surface speak.
/// - **`Anthropic`** wires only accept `claude-*`. Stable prefix.
/// - **`Gemini`** wires only accept `gemini-*` (the bare form returned
///   by `models.list`) or `models/gemini-*` (the qualified form some
///   docs use).
///
/// Match is case-insensitive (defensive — provider model lists are all
/// lowercase today, but a user-typed config with mixed case shouldn't
/// silently drop). Mirrors the lowercase sugar-name handling in
/// `apply_provider`.
pub(crate) fn model_belongs_to_kind(model: &str, kind: ProviderKind) -> bool {
    let model = model.to_ascii_lowercase();
    match kind {
        ProviderKind::OpenAiCompatible => true,
        ProviderKind::Anthropic => model.starts_with("claude-"),
        ProviderKind::Gemini => model.starts_with("gemini-") || model.starts_with("models/gemini-"),
    }
}

/// Resolve the [`ProviderKind`] for `provider` from a `[llm.providers]`
/// snapshot, falling back to the same sugar-name mapping
/// `LlmClient::provider_kind` and `LlmClient::apply_provider` use.
///
/// Mirrors the lookup used inside `LlmClient::provider_kind` so that the
/// raw-string helpers below produce the same `ProviderKind` decision as
/// the runtime path.
pub(crate) fn provider_kind_for_name(
    provider: &str,
    providers: &std::collections::BTreeMap<String, alms_core::config::ProviderEntry>,
) -> ProviderKind {
    if let Some(entry) = providers.get(provider) {
        return entry.kind;
    }
    match provider {
        "anthropic" => ProviderKind::Anthropic,
        "gemini" => ProviderKind::Gemini,
        _ => ProviderKind::OpenAiCompatible,
    }
}

/// Failure modes from [`resolve_effective_provider_and_model`].
///
/// Mirrors [`ResolveAgentConfigError`] but on a raw-string surface
/// without an `AgentId` — callers that need the full error envelope wrap
/// this with their own `agent_id`. Exists as a distinct type so the
/// helper is callable from `settings::validate_patch_budget`, which
/// validates a fleet of records and does not have a single `AgentId` to
/// attach.
#[derive(Debug, Clone)]
pub(crate) enum ResolveEffectiveModelError {
    /// Per-agent provider override switched the effective provider AND
    /// no model was supplied at any layer for the new provider — same
    /// shape as [`ResolveAgentConfigError::MissingModelAfterProviderSwitch`].
    MissingModelAfterProviderSwitch {
        new_provider: String,
        prev_provider: String,
    },
}

/// Mirrors [`resolve_agent_config`]'s per-agent model resolution from
/// raw strings rather than an [`alms_runtime::LlmClient`].
///
/// Returns the effective `(provider, model)` pair the runtime would land
/// on for an agent record, applying the same precedence:
///
/// 1. **Effective provider.** `record.provider` if set, otherwise the
///    server-default `provider`.
/// 2. **Cross-namespace drop (#942).** When the per-agent provider
///    override changes the effective wire kind AND the per-agent model
///    belongs to the OLD provider's namespace (see
///    [`model_belongs_to_kind`]), drop the per-agent model so the run
///    does not 404 on the new provider's wire.
/// 3. **Effective model.** The per-agent model that survived the
///    namespace check, otherwise `[llm.providers.<effective>].model`,
///    otherwise the server-default model — but the server-default model
///    is reused only when the provider did NOT change. When the
///    provider changed AND no per-agent / provider-entry model is
///    available, return [`ResolveEffectiveModelError::MissingModelAfterProviderSwitch`]
///    rather than letting the OLD provider's default model leak onto
///    the new provider's wire (#860 / #863).
///
/// Used by [`resolve_agent_config`] (where it is the single source of
/// truth for the model decision; the LlmClient side-effects layer on
/// top) and by `settings::validate_patch_budget` (where the fleet check
/// validates each agent's effective `(provider, model)` against the
/// candidate `max_input_tokens` cap — the bare-record path skipped the
/// namespace drop pre-fix and silently green-lit PATCHes that the
/// runtime would later reject with `INVALID_TOKEN_BUDGET_FOR_PROVIDER`).
pub(crate) fn resolve_effective_provider_and_model(
    record_provider: Option<&str>,
    record_model: Option<&str>,
    server_provider: &str,
    server_default_model: &str,
    providers: &std::collections::BTreeMap<String, alms_core::config::ProviderEntry>,
) -> Result<(String, String), ResolveEffectiveModelError> {
    let prev_provider = server_provider.to_string();
    let effective_provider = record_provider
        .map(str::to_string)
        .unwrap_or_else(|| prev_provider.clone());
    let provider_changed = effective_provider != prev_provider;
    let effective_kind = provider_kind_for_name(&effective_provider, providers);

    // Mirror the per-agent model arm in `resolve_agent_config`: keep
    // the per-agent model when no provider switch fired, when the
    // namespace matches, or drop it (#942) when the wire kind would
    // reject it.
    let surviving_per_agent_model: Option<String> = match record_model {
        None => None,
        Some(m) if !provider_changed => Some(m.to_string()),
        Some(m) if model_belongs_to_kind(m, effective_kind) => Some(m.to_string()),
        Some(_) => None,
    };

    if let Some(model) = surviving_per_agent_model {
        return Ok((effective_provider, model));
    }

    // No surviving per-agent model: fall back to the new provider's
    // entry-level model override, then the server-default model.
    // The server-default model can ONLY be reused when the provider
    // did not change — reusing it across a provider switch is the #860
    // leak shape that `resolve_agent_config` rejects with
    // `MissingModelAfterProviderSwitch`.
    if let Some(entry_model) = providers
        .get(&effective_provider)
        .and_then(|e| e.model.clone())
    {
        return Ok((effective_provider, entry_model));
    }

    if provider_changed {
        return Err(
            ResolveEffectiveModelError::MissingModelAfterProviderSwitch {
                new_provider: effective_provider,
                prev_provider,
            },
        );
    }

    Ok((effective_provider, server_default_model.to_string()))
}

/// Result of resolving per-agent config from the agent registry.
pub(crate) struct ResolvedAgentConfig {
    pub(crate) agent_config: alms_runtime::AgentConfig,
    pub(crate) llm: alms_runtime::LlmClient,
    /// Agent name from registry (None if record not found).
    pub(crate) agent_name: Option<String>,
    /// Per-agent worktree-isolation mode (#946). `Off` for unnamed
    /// / unregistered agents and for agents that haven't opted
    /// into the worktree dance.
    pub(crate) worktree_mode: alms_core::WorktreeMode,
}

/// Failure modes from [`resolve_agent_config`].
///
/// Currently the only variant is [`Self::MissingModelAfterProviderSwitch`]
/// (#863) — the structured replacement for the old `with_model("")`
/// fail-fast that surfaced as an opaque downstream 4xx (e.g. Anthropic
/// 404 on `model: ""`) at the upstream provider. Catching this at the
/// gateway lets `POST /runs` reject the request with a clean
/// `400 MISSING_MODEL_AFTER_PROVIDER_SWITCH` before any LLM call.
#[derive(Debug, Clone)]
pub(crate) enum ResolveAgentConfigError {
    /// A per-agent `provider` override switched the effective provider
    /// AND no model was supplied at any layer for the new provider:
    /// per-agent `model` was either `None` or carried a name from the
    /// OLD provider's namespace (and was dropped by the #942 cross-
    /// namespace check), AND the new provider's
    /// `[llm.providers.<new>].model` entry has no `model` field. The
    /// agent loop would otherwise send the previous provider's default
    /// model on the new provider's wire (#860 leak) or send `model: ""`
    /// (the pre-#863 fail-fast). Returning this error from
    /// `resolve_agent_config` lets the gateway emit a structured 400
    /// before any LLM call.
    MissingModelAfterProviderSwitch {
        agent_id: alms_core::AgentId,
        new_provider: String,
        prev_provider: String,
    },
}

impl std::fmt::Display for ResolveAgentConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingModelAfterProviderSwitch {
                agent_id,
                new_provider,
                prev_provider,
            } => write!(
                f,
                "agent {agent_id} overrides provider to {new_provider} but no \
                 model was supplied; previous provider {prev_provider}'s default \
                 cannot be reused"
            ),
        }
    }
}

impl std::error::Error for ResolveAgentConfigError {}

/// Resolve per-agent config overrides from the agent registry.
///
/// Looks up the agent record by ID, layers per-agent overrides
/// (model/posture/provider/reasoning/thinking budgets/summary
/// provider+model) on top of the server-default `base_config`, and
/// returns the merged `AgentConfig`, an `LlmClient` retargeted at the
/// per-agent provider (with secrets re-resolved), and the agent's
/// registry name for workspace resolution.
///
/// **Two-layer precedence** (per-agent > server default). Per-run
/// overrides were removed in the #941 pivot — agents are the single
/// per-tenant config surface. Operators set values via `PATCH
/// /agents/{id}` before starting the run; `POST /runs` carries no
/// config knobs.
///
/// **#863 missing-model gateway-side 400.** When a per-agent `provider`
/// override switches the effective provider AND neither a per-agent
/// `model` nor a `[llm.providers.<name>].model` entry supplies a model
/// for the new provider, `apply_provider` leaves `default_model`
/// pointing at the OLD provider's default — which would 404 on the new
/// provider's wire. The pre-#863 behaviour was a defensive
/// `with_model("")` empty-clear that surfaced as an opaque downstream
/// 4xx (e.g. Anthropic 404 on `model: ""`). The post-#863 behaviour is
/// to return [`ResolveAgentConfigError::MissingModelAfterProviderSwitch`]
/// so callers can map it to a structured `400 MISSING_MODEL_AFTER_PROVIDER_SWITCH`
/// before any LLM call. This subsumes the original #860 leak guard —
/// the empty-clear path no longer fires at all.
///
/// **#942 cross-namespace per-agent model drop.** When a per-agent
/// provider override switches the effective provider AND the per-agent
/// `model` field carries a name from the OLD provider's namespace
/// (e.g. agent record has `provider: anthropic` and `model: gpt-4o`,
/// or `provider: gemini` and `model: claude-3.5-sonnet`), applying
/// that stale model produces an opaque downstream 404. The
/// cross-namespace check below drops the per-agent model on the floor
/// before it reaches `with_model`; the run then falls through to the
/// `[llm.providers.<new>].model` entry (if any) or to the #860
/// empty-clear fail-fast (if not). Namespace check is keyed on
/// [`ProviderKind`]: `Anthropic` requires a `claude-` prefix, `Gemini`
/// requires `gemini-` / `models/gemini-`, and `OpenAiCompatible` is
/// permissive (the kind is a giant tent — OpenAI / OpenRouter /
/// DeepSeek all share the wire format and accept different model
/// namespaces). When the cross-namespace path falls through to the
/// missing-model condition described above, the function returns
/// [`ResolveAgentConfigError::MissingModelAfterProviderSwitch`] (#863)
/// rather than emitting an empty-clear `default_model`.
pub(crate) fn resolve_agent_config(
    agent_id: alms_core::AgentId,
    session_manager: &alms_session::SessionManager,
    base_config: &alms_runtime::AgentConfig,
    llm: &alms_runtime::LlmClient,
    secrets: Option<&alms_core::secrets::SecretsStore>,
) -> Result<ResolvedAgentConfig, ResolveAgentConfigError> {
    let agent_record =
        session_manager
            .store()
            .and_then(|store| match store.load_agent_by_id(agent_id) {
                Ok(record) => record,
                Err(e) => {
                    tracing::warn!(
                        "Failed to load agent record for {}, using server defaults: {}",
                        agent_id,
                        e
                    );
                    None
                }
            });

    let agent_name = agent_record.as_ref().map(|r| r.name.clone());

    // Layer per-agent overrides onto the base config.
    let mut cfg = base_config.clone();
    let mut per_agent_model: Option<String> = None;
    if let Some(record) = agent_record.as_ref() {
        if let Some(ref m) = record.model {
            per_agent_model = Some(m.clone());
        }
        if let Some(ref p) = record.posture
            && let Ok(posture) = p.parse::<alms_runtime::Posture>()
        {
            cfg.posture = posture;
        }
        // Per-agent Anthropic thinking budget. `Some(0)` is a legitimate
        // per-agent override meaning "disable extended thinking for this
        // agent even when the server default enables it", so we honour
        // any `Some` value here.
        if let Some(budget) = record.thinking_budget_tokens {
            cfg.anthropic_thinking_budget = budget;
        }
        // Per-agent OpenAI-compat reasoning effort (#768). `Some(effort)`
        // wins over the server default; `None` falls through.
        if let Some(effort) = record.reasoning_effort {
            cfg.openai_reasoning_effort = Some(effort);
        }
        // Per-agent Gemini thinking budget (#794). `Some(n)` (including
        // `Some(0)`) is a legitimate per-agent override — same shape as
        // Anthropic `thinking_budget_tokens` above.
        if let Some(budget) = record.gemini_thinking_budget {
            cfg.gemini_thinking_budget = Some(budget);
        }
        // Per-agent summary provider/model overrides (#872). The
        // validator on `POST /agents` / `PATCH /agents/{id}` enforces
        // the pair-only invariant (both fields set together or both
        // unset) so by the time we get here the per-agent values are
        // guaranteed symmetric. `None` falls through to the
        // server-level setting already on `cfg.context_config`.
        if let Some(ref provider) = record.summary_provider {
            cfg.context_config.summary_provider = Some(provider.clone());
        }
        if let Some(ref model) = record.summary_model {
            cfg.context_config.summary_model = Some(model.clone());
        }
        // Per-agent debug-mode toggle (#1003). The agent record's
        // `debug_mode` is the single source of truth — it lands on
        // the resolved config so the runtime emits a `ContextDebug`
        // event on each turn. PATCH-mutable via
        // `PATCH /agents/{id}` so flipping the flag at run-time
        // takes effect on the next run without a restart. This
        // merge is the ONLY gate: the #546-era notification
        // debug-flip in `lifecycle::execute_run` that used to
        // force `debug_mode = true` for system-triggered runs on
        // user-facing sessions was removed — it overrode a toggle
        // the user had set to off (the "Context sent to LLM" row
        // appeared on subagent-completion turns with debug off).
        //
        // Merge is monotonic-on (`if true { cfg = true }`) rather
        // than symmetric (`cfg = record.debug_mode || cfg`) by
        // design: there is no server-level `debug_mode` knob today,
        // but if one is added in the future the per-agent default
        // (`false`) must NOT clobber a server-side `true` back to
        // false. The default-false per-agent value is therefore a
        // no-op rather than an explicit-disable, and pinning this
        // is what `test_per_agent_debug_mode_false_does_not_clobber_server_default`
        // guards against regression on. If `debug_mode` ever grows
        // a third state ("explicitly off"), revisit this merge —
        // `Option<bool>` with a clear-sentinel like #809's
        // reasoning knobs would be the natural shape.
        if record.debug_mode {
            cfg.debug_mode = true;
        }
    }

    // Resolve the effective `(provider, model)` for this record up-front
    // via the shared helper so the per-run path and the PATCH /settings
    // fleet-evaluation path (`settings::validate_patch_budget`) consult
    // the same source of truth. The helper encapsulates the #942
    // cross-namespace drop and the #863 missing-model decision; the
    // LlmClient side-effects (`with_provider_and_secrets` / `with_model`)
    // layer on top.
    let server_provider = llm.provider().to_string();
    let server_default_model = llm.default_model().to_string();
    let record_provider = agent_record.as_ref().and_then(|r| r.provider.as_deref());
    let (effective_provider, effective_model) = match resolve_effective_provider_and_model(
        record_provider,
        per_agent_model.as_deref(),
        &server_provider,
        &server_default_model,
        llm.providers_snapshot(),
    ) {
        Ok(pair) => pair,
        Err(ResolveEffectiveModelError::MissingModelAfterProviderSwitch {
            new_provider,
            prev_provider,
        }) => {
            // #863 structured rejection (subsumes the old #860 empty-clear
            // fail-fast). The helper returned the missing-model decision;
            // map it onto the public `ResolveAgentConfigError` so the
            // gateway emits a clean `400 MISSING_MODEL_AFTER_PROVIDER_SWITCH`
            // before any LLM call.
            warn!(
                agent_id = %agent_id,
                old_provider = %prev_provider,
                new_provider = %new_provider,
                stale_model = %server_default_model,
                "Per-agent provider switch with no per-agent model and no \
                 provider-entry model -- rejecting before LLM call so the \
                 user sees a clean 400 instead of an opaque downstream 4xx \
                 (#863)"
            );
            return Err(ResolveAgentConfigError::MissingModelAfterProviderSwitch {
                agent_id,
                new_provider,
                prev_provider,
            });
        }
    };

    // Log the cross-namespace drop here for runtime observability — the
    // helper itself is silent because it is also called from the PATCH
    // fleet-evaluation path (which has its own per-agent log line). The
    // drop fires when a per-agent provider override changed the
    // effective wire kind AND the per-agent model belongs to the OLD
    // provider's namespace.
    if let Some(stale) = per_agent_model.as_deref()
        && effective_provider != server_provider
        && !model_belongs_to_kind(
            stale,
            provider_kind_for_name(&effective_provider, llm.providers_snapshot()),
        )
    {
        warn!(
            agent_id = %agent_id,
            old_provider = %server_provider,
            new_provider = %effective_provider,
            stale_per_agent_model = %stale,
            "Per-agent provider switch with cross-namespace per-agent model -- \
             dropping the stale model so the run does not 404 on the new \
             provider's wire with a previous-namespace model name (#942)"
        );
    }

    // Apply per-agent provider override first (changes base_url +
    // api_key), then ALWAYS re-resolve the API key from secrets for the
    // effective provider. This ensures keys set at runtime (via UI or
    // CLI) are picked up even for the default agent which has no
    // per-agent provider field.
    let mut llm = llm.clone();
    if let Some(ref record) = agent_record
        && let Some(ref provider) = record.provider
    {
        info!(
            agent_id = %agent_id,
            provider = %provider,
            "Applying per-agent provider override with secrets resolution"
        );
        llm = if let Some(s) = secrets {
            llm.with_provider_and_secrets(provider, s)
        } else {
            warn!(
                agent_id = %agent_id,
                provider = %provider,
                "No secrets store available for per-agent provider override — API key may be missing"
            );
            llm.with_provider(provider)
        };
    } else if let Some(s) = secrets {
        // No per-agent provider override — re-resolve the key for the
        // server-default provider from the live secrets store.
        info!(
            agent_id = %agent_id,
            provider = %llm.provider(),
            "Re-resolving API key from secrets for default provider"
        );
        llm = llm.with_secrets(s);
    } else {
        warn!(
            agent_id = %agent_id,
            "No secrets store and no per-agent provider — using base LLM client key as-is"
        );
    }

    // Apply the effective model decided by the helper. The per-agent
    // model (when it survived the namespace check) or the
    // `[llm.providers.<effective>].model` fallback wins over the value
    // `apply_provider` left on `default_model`. When the helper picked
    // the server-default model (no per-agent override and no provider
    // switch), the `with_model` is a no-op assignment of the same
    // string, kept here for symmetry rather than a special case.
    llm = llm.with_model(effective_model);

    // Per-agent worktree-isolation mode (#946). Unnamed agents
    // and agents not in the registry get `Off` — there is no
    // worktree to attach to, so the run uses the project root
    // (or the security escape hatch) like every other unnamed
    // run.
    let worktree_mode = agent_record
        .as_ref()
        .map(|r| r.worktree_mode)
        .unwrap_or_default();

    Ok(ResolvedAgentConfig {
        agent_config: cfg,
        llm,
        agent_name,
        worktree_mode,
    })
}

/// Snapshot the layered run config for triage persistence (#837).
///
/// Called from `lifecycle::execute_run` after `resolve_agent_config` and
/// `resolve_posture_for_run` have settled — i.e. the `agent_config` and
/// `llm` passed in here carry the values the LLM adapter is about to send
/// on the wire. (The #546-era notification debug-flip that used to settle
/// before this call was removed — the per-agent `debug_mode` toggle is
/// now the sole gate for notification runs too.)
///
/// Reads `provider()` and `default_model()` from the resolved
/// [`alms_runtime::LlmClient`] (which threaded per-agent model / provider
/// overrides through `with_provider_and_secrets` and `with_model`); the
/// remaining fields come from the merged [`alms_runtime::AgentConfig`].
///
/// Pure function — kept here next to `resolve_agent_config` so the
/// layering primitives all live in one place and can be unit-tested
/// without spinning up an `AppState`.
pub(crate) fn build_resolved_config(
    agent_config: &alms_runtime::AgentConfig,
    llm: &alms_runtime::LlmClient,
) -> alms_core::ResolvedRunConfig {
    alms_core::ResolvedRunConfig {
        provider: llm.provider().to_string(),
        model: llm.default_model().to_string(),
        max_tokens: agent_config.max_tokens,
        posture: agent_config.posture.to_string(),
        debug_mode: agent_config.debug_mode,
        thinking_budget_tokens: agent_config.anthropic_thinking_budget,
        reasoning_effort: agent_config.openai_reasoning_effort,
        gemini_thinking_budget: agent_config.gemini_thinking_budget,
    }
}
