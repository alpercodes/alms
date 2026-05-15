//! Token-budget cross-validation against per-provider context windows (#919).
//!
//! Every LLM provider charges **input + output** against a single context-window
//! cap. ALMS lets operators set `[context].max_input_tokens` (the per-request
//! input budget) and `agent.max_tokens` (output reservation) independently —
//! and pre-#919 there was no validation that the sum stays under the
//! provider's actual cap. The symptom was a runtime HTTP 400 from the
//! provider partway through a turn (per #851's repro), instead of failing
//! fast at config-load or returning a clean 400 from the gateway.
//!
//! This module supplies:
//! - [`provider_context_window`]: a hardcoded `(provider, model_id) → cap`
//!   table keyed off provider tag + raw model id, with bare-id fallbacks
//!   for models that ALMS reaches through both their direct API and via
//!   OpenRouter.
//! - [`validate_token_budget`]: pure cross-validator that returns
//!   [`TokenBudgetError`] when `max_input_tokens + max_tokens` exceeds the
//!   table-published cap.
//! - [`ValidationMode`] + [`ValidationMode::from_env`]: parse the
//!   `ALMS_LLM_BUDGET_VALIDATION` env-var opt-out.
//!
//! Both call sites (config-load in [`super::AlmsConfig::validate`] and the
//! per-run model-resolve path in `alms-gateway/src/runs/lifecycle.rs`)
//! consult these helpers; the per-run site wraps the result in a structured
//! `400 INVALID_TOKEN_BUDGET_FOR_PROVIDER` response instead of failing the
//! daemon at boot.
//!
//! ## Verification policy
//!
//! Every model row in [`lookup`] is annotated with a `Source: <URL>` plus a
//! `Verified <DATE> by <Atlas|Heph>` line. The verification round on
//! 2026-05-09 narrowed the table to current models only — anything older
//! than ~2 months or in deprecation has been dropped, and Mistral / Groq
//! were dropped pending a future verification round (no current published
//! caps fetched this round). Add new rows by fetching the provider's docs
//! page at the URL listed and recording the date here.

use std::env;

use serde::Serialize;
use tracing::warn;

// ---------------------------------------------------------------------------
// Validation mode (env-var opt-out)
// ---------------------------------------------------------------------------

/// How a token-budget violation is surfaced.
///
/// Strict (the default) is the right shape for production: a misconfigured
/// budget surfaces as a fast, actionable error instead of an opaque
/// downstream 4xx mid-run. The `warn` opt-out exists for testing edge cases
/// or experimental providers whose published caps differ from the table —
/// the operator can toggle it without recompiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ValidationMode {
    /// Refuse the offending config / per-run resolve. Default.
    #[default]
    Strict,
    /// Log a structured `WARN` at `target = "alms.config"` and continue.
    Warn,
}

impl ValidationMode {
    /// Resolve the validation mode from the `ALMS_LLM_BUDGET_VALIDATION`
    /// environment variable.
    ///
    /// Recognised values (case-insensitive):
    /// - `"strict"` (or unset) → [`ValidationMode::Strict`]
    /// - `"warn"` → [`ValidationMode::Warn`]
    /// - anything else → [`ValidationMode::Strict`] with a one-time WARN.
    ///   Falling back to strict-on-typo means a misspelled opt-out can't
    ///   silently leave production without enforcement.
    pub fn from_env() -> Self {
        match env::var("ALMS_LLM_BUDGET_VALIDATION") {
            Ok(val) => match val.trim().to_ascii_lowercase().as_str() {
                "" | "strict" => Self::Strict,
                "warn" => Self::Warn,
                other => {
                    warn!(
                        target: "alms.config",
                        value = %other,
                        "Unknown ALMS_LLM_BUDGET_VALIDATION value; defaulting to strict. \
                         Expected one of: strict, warn."
                    );
                    Self::Strict
                }
            },
            Err(_) => Self::Strict,
        }
    }
}

// ---------------------------------------------------------------------------
// Provider context-window table
// ---------------------------------------------------------------------------

// TODO: keep in sync with provider docs as new models ship. When in doubt,
// pick the **practical** cap (the one the provider actually serves under a
// default tier) rather than the marketing cap.
//
// Lookup keys are `(provider_tag, model_id)`. The "provider_tag" matches the
// resolver tag (`anthropic`, `openai`, `gemini`, `xai`, `deepseek`,
// `ollama`, `openrouter`). For OpenRouter we also do a best-effort prefix
// strip (`anthropic/claude-...` → `claude-...`) to route through the
// underlying-provider's table — see [`provider_context_window`] for the
// dispatch.

/// Look up the published context-window cap (in tokens) for a
/// `(provider, model)` pair.
///
/// Returns `None` for combinations the table doesn't know about — the
/// validator skips unknown rows rather than refusing the boot, since a
/// missing entry means "we can't enforce this without false positives".
/// Operators can still trip over a real cap at runtime; the env-var
/// opt-out is for the rare case where the table is wrong.
///
/// OpenRouter routes to whichever underlying model the operator selected.
/// The provider tag is `"openrouter"` but the actual context window
/// depends on the model id string. We strip the OpenRouter vendor prefix
/// (`anthropic/claude-opus-4-7` → `claude-opus-4-7`) and look up the
/// stripped id under the inferred underlying-provider tag. Unrecognised
/// vendor prefixes fall through to `None` (treated as warn-only by the
/// validator regardless of mode).
pub fn provider_context_window(provider: &str, model: &str) -> Option<usize> {
    // Normalise the case so a hand-edited TOML with `Provider = "Anthropic"`
    // still hits the table.
    let provider = provider.to_ascii_lowercase();
    let model = model.trim();

    // OpenRouter dispatch: split on the FIRST `/` so the vendor prefix is
    // peeled off cleanly even when the underlying-provider id is
    // multi-segment (e.g. `google/gemini-2.5-pro/preview` →
    // `vendor="google"`, `rest="gemini-2.5-pro/preview"`). We then call
    // `lookup` exactly once under the inferred underlying-provider tag —
    // there is no recursion. The `starts_with` arms inside `lookup`
    // tolerate any trailing `/segment` on the rest string, so a
    // multi-segment `rest` resolves to the same cap as the bare
    // underlying id.
    if provider == "openrouter"
        && let Some((vendor, rest)) = model.split_once('/')
    {
        let underlying = match vendor.to_ascii_lowercase().as_str() {
            "anthropic" => Some("anthropic"),
            "openai" => Some("openai"),
            "google" => Some("gemini"),
            "x-ai" | "xai" => Some("xai"),
            "deepseek" => Some("deepseek"),
            // Unknown OpenRouter vendor prefix (`moonshotai/`, `minimax/`,
            // `z-ai/`, `mistralai/`, `meta-llama/`, etc.) — the table has no
            // published caps for these (Mistral / Groq dropped pending a
            // future verification round, others never enumerated), so we
            // return None and the validator treats it as "unenforceable"
            // (warn-only).
            _ => None,
        };
        if let Some(under) = underlying {
            return lookup(under, rest);
        }
        return None;
    }

    lookup(&provider, model)
}

/// Direct table lookup by `(provider_tag, model_id)`. Bare model ids only —
/// callers strip vendor prefixes (OpenRouter) before reaching here.
fn lookup(provider: &str, model: &str) -> Option<usize> {
    match (provider, model) {
        // -------------------------------------------------------------
        // Anthropic — Claude 4.x current generation.
        //
        // Source: https://platform.claude.com/docs/en/about-claude/models/overview
        // Verified 2026-05-09 by Atlas via WebFetch (Heph rewrite — see PR
        // #1020 verification round).
        //
        // Opus 4.7 and Sonnet 4.6 are 1M context natively — no beta header
        // required. Haiku 4.5 stays on 200K. Older Claude 3.x and any
        // `claude-sonnet-4-5-1m` legacy id are dropped (>2 months / model
        // id was fabricated in the prior round).
        // -------------------------------------------------------------
        ("anthropic", m) if m.starts_with("claude-opus-4-7") => Some(1_000_000),
        ("anthropic", m) if m.starts_with("claude-sonnet-4-6") => Some(1_000_000),
        ("anthropic", m) if m.starts_with("claude-haiku-4-5") => Some(200_000),

        // -------------------------------------------------------------
        // OpenAI — GPT-5.5 / GPT-5.4 current generation.
        //
        // Source: https://developers.openai.com/api/docs/models
        // Verified 2026-05-09 by Atlas via WebFetch (Heph rewrite — see PR
        // #1020 verification round).
        //
        // GPT-5.5 (incl. `gpt-5.5-2026-04-23` snapshot) and GPT-5.4 ship
        // with a 1.05M context window; gpt-5.4-mini stays on 400K. Older
        // GPT-5 (original), GPT-4.1, GPT-4o, o1, o3, o4-mini families are
        // dropped (>2 months).
        // -------------------------------------------------------------
        ("openai", m) if m.starts_with("gpt-5.5") => Some(1_050_000),
        ("openai", "gpt-5.4") => Some(1_050_000),
        ("openai", "gpt-5.4-mini") => Some(400_000),

        // -------------------------------------------------------------
        // xAI Grok — current Grok 4.x family.
        //
        // Source: https://docs.x.ai/developers/models
        // Verified 2026-05-09 by Atlas via WebFetch (Heph rewrite — see PR
        // #1020 verification round).
        //
        // grok-4.3 ships at 1M; the grok-4.20 family (0309 reasoning /
        // non-reasoning / multi-agent) and grok-4-1-fast-{reasoning,
        // non-reasoning} ship at 2M. Bare `grok-4`, `grok-4-fast`, and
        // `grok-3` are retiring 2026-05-15 and dropped from the table.
        // -------------------------------------------------------------
        ("xai", "grok-4.3") => Some(1_000_000),
        ("xai", m) if m.starts_with("grok-4.20") => Some(2_000_000),
        ("xai", "grok-4-1-fast-reasoning") => Some(2_000_000),
        ("xai", "grok-4-1-fast-non-reasoning") => Some(2_000_000),

        // -------------------------------------------------------------
        // DeepSeek — V4 Flash + legacy aliases.
        //
        // Source: https://api-docs.deepseek.com/quick_start/pricing
        // Verified 2026-05-09 by Atlas via WebFetch (Heph rewrite — see PR
        // #1020 verification round).
        //
        // `deepseek-v4-flash` is current; `deepseek-chat` and
        // `deepseek-reasoner` are documented compatibility aliases that
        // route to v4-flash (non-thinking and thinking respectively). All
        // three share the 1M cap. The prior round's 64K / 128K values were
        // both wrong.
        // -------------------------------------------------------------
        ("deepseek", "deepseek-v4-flash") => Some(1_000_000),
        ("deepseek", "deepseek-chat") => Some(1_000_000),
        ("deepseek", "deepseek-reasoner") => Some(1_000_000),

        // -------------------------------------------------------------
        // Google Gemini — 2.5 Pro / Flash current generation.
        //
        // Source: https://docs.cloud.google.com/vertex-ai/generative-ai/docs/models/gemini/2-5-pro
        //         https://docs.cloud.google.com/vertex-ai/generative-ai/docs/models/gemini/2-5-flash
        // Verified 2026-05-09 by Atlas via WebFetch (Heph rewrite — see PR
        // #1020 verification round).
        //
        // 1,048,576 input tokens (the published value — keep the literal
        // rather than rounding to 1M so an operator who pins a budget at
        // exactly 1,048,576 doesn't trip a rounding-induced false
        // positive). 1.5 / 1.5-flash / 2.0 dropped (>2 months).
        //
        // The `models/` prefix is the qualified form Gemini's own
        // `models.list` REST endpoint returns and that `runs::model_belongs_to_kind`
        // accepts as Gemini-namespaced. Strip it before pattern-matching
        // so `models/gemini-2.5-pro` resolves to the same cap as the bare
        // form, otherwise the validator silently skips a known overshoot
        // (Codex P2 #3 follow-up on PR #1020). The strip is Gemini-only —
        // Anthropic and OpenAI ids never carry a `models/` prefix on the
        // wire.
        // -------------------------------------------------------------
        ("gemini", m)
            if m.strip_prefix("models/")
                .unwrap_or(m)
                .starts_with("gemini-2.5-pro") =>
        {
            Some(1_048_576)
        }
        ("gemini", m)
            if m.strip_prefix("models/")
                .unwrap_or(m)
                .starts_with("gemini-2.5-flash") =>
        {
            Some(1_048_576)
        }

        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Validator
// ---------------------------------------------------------------------------

/// Structured failure shape returned by [`validate_token_budget`].
///
/// Carries every datum a caller needs to render either an
/// `AlmsError::InvalidConfig` (config-load) or a structured 400 JSON body
/// (per-run gateway path). All fields are public so callers can serialise
/// the response without reaching into private state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TokenBudgetError {
    pub provider: String,
    pub model: String,
    pub max_input_tokens: usize,
    pub max_tokens: u32,
    pub effective_total: u64,
    pub provider_cap: usize,
}

impl TokenBudgetError {
    /// Render the error as a one-line human-readable message — the form
    /// `validate_at_config_load` uses for `AlmsError::InvalidConfig` and the
    /// gateway uses for the JSON `message` field.
    pub fn message(&self) -> String {
        format!(
            "configured token budget exceeds provider cap: \
             context.max_input_tokens ({input}) + agent.max_tokens ({output}) = {total} \
             > {provider} {model} context window ({cap}). \
             Lower one or both knobs, or set ALMS_LLM_BUDGET_VALIDATION=warn to bypass.",
            input = self.max_input_tokens,
            output = self.max_tokens,
            total = self.effective_total,
            provider = self.provider,
            model = self.model,
            cap = self.provider_cap,
        )
    }
}

/// Cross-validate `max_input_tokens + max_tokens` against the published
/// context window for `(provider, model)`.
///
/// Returns:
/// - `Ok(())` when the sum fits the cap, OR when the table has no entry
///   for the pair (the validator can't enforce what it doesn't know about
///   — the operator can still trip over the real cap at runtime, which is
///   exactly the symptom this validator is trying to prevent for
///   *known* pairs).
/// - `Err(TokenBudgetError)` when the sum exceeds the cap.
///
/// The caller decides what to do with the error based on
/// [`ValidationMode`]: strict surfaces it as a fatal config / 400; warn
/// logs and continues.
pub fn validate_token_budget(
    provider: &str,
    model: &str,
    max_input_tokens: usize,
    max_tokens: u32,
) -> Result<(), TokenBudgetError> {
    let Some(cap) = provider_context_window(provider, model) else {
        // Unknown (provider, model) — skip silently. The
        // validator's contract is "fail fast on KNOWN-overshoot
        // configs"; an unknown row is exactly the case we don't want to
        // false-positive on.
        return Ok(());
    };
    let effective_total = (max_input_tokens as u64).saturating_add(u64::from(max_tokens));
    if effective_total > cap as u64 {
        return Err(TokenBudgetError {
            provider: provider.to_string(),
            model: model.to_string(),
            max_input_tokens,
            max_tokens,
            effective_total,
            provider_cap: cap,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // Anthropic
    // -----------------------------------------------------------------

    #[test]
    fn anthropic_claude_opus_4_7_returns_1m() {
        // Regression guard: opus-4-7 is 1M (NOT the prior round's
        // fabricated 200K). Pinned as a dedicated test so a future
        // accidental rollback to 200K trips this assertion immediately.
        assert_eq!(
            provider_context_window("anthropic", "claude-opus-4-7"),
            Some(1_000_000)
        );
    }

    #[test]
    fn anthropic_claude_sonnet_4_6_returns_1m() {
        assert_eq!(
            provider_context_window("anthropic", "claude-sonnet-4-6"),
            Some(1_000_000)
        );
    }

    #[test]
    fn anthropic_claude_haiku_4_5_returns_200k() {
        assert_eq!(
            provider_context_window("anthropic", "claude-haiku-4-5"),
            Some(200_000)
        );
        // Dated alias — same 200K cap.
        assert_eq!(
            provider_context_window("anthropic", "claude-haiku-4-5-20251001"),
            Some(200_000)
        );
    }

    // -----------------------------------------------------------------
    // OpenAI
    // -----------------------------------------------------------------

    #[test]
    fn openai_gpt_5_5_returns_1_050k() {
        assert_eq!(
            provider_context_window("openai", "gpt-5.5"),
            Some(1_050_000)
        );
        assert_eq!(
            provider_context_window("openai", "gpt-5.5-2026-04-23"),
            Some(1_050_000)
        );
    }

    #[test]
    fn openai_gpt_5_4_returns_1_050k() {
        assert_eq!(
            provider_context_window("openai", "gpt-5.4"),
            Some(1_050_000)
        );
    }

    #[test]
    fn openai_gpt_5_4_mini_returns_400k() {
        assert_eq!(
            provider_context_window("openai", "gpt-5.4-mini"),
            Some(400_000)
        );
    }

    // -----------------------------------------------------------------
    // xAI Grok
    // -----------------------------------------------------------------

    #[test]
    fn xai_grok_4_3_returns_1m() {
        assert_eq!(provider_context_window("xai", "grok-4.3"), Some(1_000_000));
    }

    #[test]
    fn xai_grok_4_20_family_returns_2m() {
        assert_eq!(
            provider_context_window("xai", "grok-4.20-0309-reasoning"),
            Some(2_000_000)
        );
        assert_eq!(
            provider_context_window("xai", "grok-4.20-0309-non-reasoning"),
            Some(2_000_000)
        );
        assert_eq!(
            provider_context_window("xai", "grok-4.20-multi-agent-0309"),
            Some(2_000_000)
        );
    }

    #[test]
    fn xai_grok_4_1_fast_returns_2m() {
        assert_eq!(
            provider_context_window("xai", "grok-4-1-fast-reasoning"),
            Some(2_000_000)
        );
        assert_eq!(
            provider_context_window("xai", "grok-4-1-fast-non-reasoning"),
            Some(2_000_000)
        );
    }

    #[test]
    fn xai_dropped_legacy_ids_return_none() {
        // grok-3 / bare grok-4 / bare grok-4-fast are retiring 2026-05-15
        // and were dropped from the table; the validator skips them.
        assert_eq!(provider_context_window("xai", "grok-3"), None);
        assert_eq!(provider_context_window("xai", "grok-4"), None);
        assert_eq!(provider_context_window("xai", "grok-4-fast"), None);
    }

    // -----------------------------------------------------------------
    // DeepSeek
    // -----------------------------------------------------------------

    #[test]
    fn deepseek_v4_flash_and_aliases_return_1m() {
        assert_eq!(
            provider_context_window("deepseek", "deepseek-v4-flash"),
            Some(1_000_000)
        );
        // `deepseek-chat` and `deepseek-reasoner` are documented
        // compatibility aliases routing to v4-flash; same 1M cap.
        assert_eq!(
            provider_context_window("deepseek", "deepseek-chat"),
            Some(1_000_000)
        );
        assert_eq!(
            provider_context_window("deepseek", "deepseek-reasoner"),
            Some(1_000_000)
        );
    }

    // -----------------------------------------------------------------
    // Gemini
    // -----------------------------------------------------------------

    #[test]
    fn gemini_2_5_pro_and_flash_return_published_cap() {
        // 1,048,576 — keep the published literal (not rounded to 1M).
        assert_eq!(
            provider_context_window("gemini", "gemini-2.5-pro"),
            Some(1_048_576)
        );
        assert_eq!(
            provider_context_window("gemini", "gemini-2.5-flash"),
            Some(1_048_576)
        );
    }

    #[test]
    fn gemini_dropped_legacy_returns_none() {
        // 1.5 / 2.0 are >2 months old; dropped from the table.
        assert_eq!(provider_context_window("gemini", "gemini-1.5-pro"), None);
        assert_eq!(provider_context_window("gemini", "gemini-2.0-flash"), None);
    }

    #[test]
    fn gemini_models_prefix_resolves_to_same_cap() {
        // Gemini's `models.list` REST endpoint returns ids in the
        // qualified `models/<id>` form, and `runs::model_belongs_to_kind`
        // explicitly accepts both bare and qualified forms as
        // Gemini-namespaced. The budget table must match that contract —
        // pre-fix the qualified form fell through to None and silently
        // bypassed the validator (Codex P2 #3 on PR #1020).
        assert_eq!(
            provider_context_window("gemini", "models/gemini-2.5-pro"),
            Some(1_048_576),
            "qualified `models/gemini-2.5-pro` form must resolve to the same cap as the bare id"
        );
        assert_eq!(
            provider_context_window("gemini", "models/gemini-2.5-flash"),
            Some(1_048_576),
            "qualified `models/gemini-2.5-flash` form must resolve to the same cap as the bare id"
        );
        // Dated suffix on the qualified form (Gemini's REST shape sometimes
        // returns `models/gemini-2.5-pro-latest` etc.) — same prefix-match
        // semantics as the bare form.
        assert_eq!(
            provider_context_window("gemini", "models/gemini-2.5-pro-latest"),
            Some(1_048_576),
        );
    }

    #[test]
    fn openrouter_google_models_prefix_resolves_to_same_cap() {
        // OpenRouter dispatches `google/...` to the underlying Gemini
        // table; an operator who copies the qualified form into a
        // `[llm.providers.openrouter].model = "google/models/gemini-2.5-pro"`
        // entry must hit the same cap as the bare form. The OpenRouter
        // dispatcher splits on the FIRST `/`, leaving rest =
        // `models/gemini-2.5-pro`, which the Gemini-arm strip then
        // normalises to the bare form.
        assert_eq!(
            provider_context_window("openrouter", "google/models/gemini-2.5-pro"),
            Some(1_048_576),
        );
        assert_eq!(
            provider_context_window("openrouter", "google/models/gemini-2.5-flash"),
            Some(1_048_576),
        );
    }

    // -----------------------------------------------------------------
    // Unknown / fall-through
    // -----------------------------------------------------------------

    #[test]
    fn unknown_provider_returns_none() {
        assert_eq!(provider_context_window("ollama", "llama3.3:70b"), None);
        assert_eq!(provider_context_window("custom", "frankenmodel-7b"), None);
    }

    #[test]
    fn unknown_anthropic_model_returns_none() {
        // Old / unknown Anthropic ids that the table doesn't pattern-match
        // on should fall through to None and be skipped by the validator.
        assert_eq!(
            provider_context_window("anthropic", "claude-2.1"),
            None,
            "claude-2.1 predates the table; should fall through to None"
        );
        // The prior round's fabricated 1M extended-context id is now gone.
        assert_eq!(
            provider_context_window("anthropic", "claude-sonnet-4-5-1m"),
            None,
            "claude-sonnet-4-5-1m was a fabricated id; should fall through to None"
        );
    }

    #[test]
    fn dropped_provider_tags_return_none() {
        // Mistral and Groq were dropped pending a future verification
        // round (operator scope decision: "older than 2 months = no
        // need", no current data fetched). Direct-tag lookups must miss.
        assert_eq!(
            provider_context_window("mistral", "mistral-large-latest"),
            None
        );
        assert_eq!(
            provider_context_window("groq", "llama-3.3-70b-versatile"),
            None
        );
    }

    #[test]
    fn provider_case_insensitive() {
        assert_eq!(
            provider_context_window("ANTHROPIC", "claude-opus-4-7"),
            Some(1_000_000)
        );
        assert_eq!(
            provider_context_window("Anthropic", "claude-opus-4-7"),
            Some(1_000_000)
        );
    }

    // -----------------------------------------------------------------
    // OpenRouter dispatch
    // -----------------------------------------------------------------

    #[test]
    fn openrouter_strips_anthropic_prefix() {
        assert_eq!(
            provider_context_window("openrouter", "anthropic/claude-opus-4-7"),
            Some(1_000_000)
        );
    }

    #[test]
    fn openrouter_strips_openai_prefix() {
        assert_eq!(
            provider_context_window("openrouter", "openai/gpt-5.4"),
            Some(1_050_000)
        );
        assert_eq!(
            provider_context_window("openrouter", "openai/gpt-5.4-mini"),
            Some(400_000)
        );
    }

    #[test]
    fn openrouter_strips_google_prefix_to_gemini() {
        assert_eq!(
            provider_context_window("openrouter", "google/gemini-2.5-pro"),
            Some(1_048_576)
        );
    }

    #[test]
    fn openrouter_unknown_vendor_returns_none() {
        // OpenRouter exposes a long tail of vendors the table doesn't
        // enumerate (moonshotai, z-ai). They must fall through to None so
        // the validator doesn't false-positive on a default-shaped boot.
        assert_eq!(
            provider_context_window("openrouter", "moonshotai/kimi-k2.6"),
            None
        );
        assert_eq!(provider_context_window("openrouter", "z-ai/glm-5.1"), None);
        // mistralai/, mistral/, meta-llama/, groq/ were enumerated in the
        // prior round but dropped this round (operator scope decision —
        // see module-level "Verification policy"). They must now also
        // fall through to None.
        assert_eq!(
            provider_context_window("openrouter", "mistralai/mistral-large"),
            None
        );
        assert_eq!(
            provider_context_window("openrouter", "meta-llama/llama-3.3-70b"),
            None
        );
        assert_eq!(
            provider_context_window("openrouter", "groq/llama-3.3-70b-versatile"),
            None
        );
    }

    #[test]
    fn openrouter_xai_alias() {
        // OpenRouter exposes xAI under both `xai/` and `x-ai/`.
        assert_eq!(
            provider_context_window("openrouter", "x-ai/grok-4.3"),
            Some(1_000_000)
        );
        assert_eq!(
            provider_context_window("openrouter", "xai/grok-4.3"),
            Some(1_000_000)
        );
    }

    #[test]
    fn openrouter_deepseek_prefix() {
        assert_eq!(
            provider_context_window("openrouter", "deepseek/deepseek-v4-flash"),
            Some(1_000_000)
        );
        assert_eq!(
            provider_context_window("openrouter", "deepseek/deepseek-chat"),
            Some(1_000_000)
        );
    }

    // -----------------------------------------------------------------
    // Validator
    // -----------------------------------------------------------------

    #[test]
    fn fits_under_cap_passes() {
        // Anthropic Claude Opus 4.7 (1M). 128K input + 32K output = 160K
        // fits comfortably.
        let r = validate_token_budget("anthropic", "claude-opus-4-7", 128_000, 32_000);
        assert!(r.is_ok());
    }

    #[test]
    fn equal_to_cap_passes() {
        // Haiku 4.5 (200K). 168K input + 32K output = 200K — exactly at
        // cap, allowed.
        let r = validate_token_budget("anthropic", "claude-haiku-4-5", 168_000, 32_000);
        assert!(r.is_ok());
    }

    #[test]
    fn exceeds_cap_fails() {
        // Haiku 4.5 stays on the 200K tier even as Opus / Sonnet move to
        // 1M, so it's the natural overshoot canary post-rewrite.
        // 250K input + 0 output = 250K > 200K cap.
        let err = validate_token_budget("anthropic", "claude-haiku-4-5", 250_000, 0).unwrap_err();
        assert_eq!(err.provider, "anthropic");
        assert_eq!(err.model, "claude-haiku-4-5");
        assert_eq!(err.max_input_tokens, 250_000);
        assert_eq!(err.max_tokens, 0);
        assert_eq!(err.effective_total, 250_000);
        assert_eq!(err.provider_cap, 200_000);
    }

    #[test]
    fn unknown_pair_skips() {
        // Unknown model — validator skips silently regardless of size.
        let r = validate_token_budget("anthropic", "claude-2.1", 10_000_000, 1_000_000);
        assert!(
            r.is_ok(),
            "unknown (provider, model) pair must skip the budget check"
        );
    }

    #[test]
    fn gemini_1m_cap_catches_overshoot() {
        // gemini-2.5-pro publishes a 1,048,576-token cap. 1.5M input +
        // 64K output = 1,564,000 > 1,048,576 → fires.
        //
        // Replaces the prior `deepseek_v3_64k_cap_catches_overshoot` test
        // — DeepSeek's old 64K / 128K caps are gone (v4-flash is 1M), so
        // we needed a different model with a tight enough cap to demo an
        // overshoot.
        let err = validate_token_budget("gemini", "gemini-2.5-pro", 1_500_000, 64_000).unwrap_err();
        assert_eq!(err.provider_cap, 1_048_576);
        assert_eq!(err.effective_total, 1_564_000);
    }

    #[test]
    fn token_budget_error_message_mentions_both_knobs_and_provider() {
        let err = TokenBudgetError {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            max_input_tokens: 250_000,
            max_tokens: 0,
            effective_total: 250_000,
            provider_cap: 200_000,
        };
        let msg = err.message();
        assert!(msg.contains("max_input_tokens"));
        assert!(msg.contains("max_tokens"));
        assert!(msg.contains("250000")); // input + total
        assert!(msg.contains("anthropic"));
        assert!(msg.contains("claude-haiku-4-5"));
        assert!(msg.contains("200000")); // cap
        assert!(msg.contains("ALMS_LLM_BUDGET_VALIDATION"));
    }

    #[test]
    fn validation_mode_default_is_strict() {
        assert_eq!(ValidationMode::default(), ValidationMode::Strict);
    }
}
