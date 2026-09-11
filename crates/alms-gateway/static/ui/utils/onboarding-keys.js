// Key-probe helpers for the first-run onboarding flow (issue #162 half A).
//
// Onboarding offers to store a provider key before it creates the first
// agent, but skips that step when the daemon already holds one. The decision
// is made from `GET /auth/keys`, and the shape of that endpoint is the reason
// these helpers exist rather than an inline `.some(...)` at the call site.
//
// TWO ways that endpoint answers a narrower question than the step is asking:
//
// 1. `list_keys` in `crates/alms-gateway/src/auth_keys.rs` reports ONLY the
//    secrets store — what `alms auth set` and the Settings modal write. It is
//    not the only place a working key can live. A key declared in `alms.toml`
//    as `[llm.providers.<name>].api_key_env` (read from the named variable) or
//    as an inline `api_key` is resolved at gateway startup
//    (`gateway.rs`, `entry.resolve_api_key()`) and SURVIVES per-run
//    re-resolution, because `LlmClient::with_secrets` keeps the existing key
//    when the store has none. `AlmsConfig::validate` even suppresses its
//    missing-key warning for that setup. Such an operator sees
//    `configured: false` on every row — a payload byte-identical to a fresh
//    install's — while being able to run perfectly well.
//
//    NOT a key source, and the trap to avoid re-deriving: a bare
//    `OPENROUTER_API_KEY` (or `OPENAI_API_KEY`, ...) export. Startup detects
//    it and logs "API key env var detected but IGNORED for security"
//    (`config/mod.rs::warn_deprecated_secret_env_vars`), `resolve_key` has no
//    env fallback (pinned by `secrets.rs::test_resolve_key_no_env_fallback`),
//    and `docs/config.md` states it as policy. `select_llm_api_key` reads like
//    production precedence logic and does implement an env chain — it lives in
//    `config/tests.rs` and is labelled "Used only in tests". Naming a bare
//    export as a working option in this UI would send the reader straight into
//    the failed first run this flow exists to close.
//
// 2. It iterates `VALID_PROVIDERS` (`alms-core/src/secrets.rs`), which is not
//    a list of LLM providers: `telegram` is in there too, as a channel bot
//    token. A `configured: true` on that row says nothing about whether any
//    model can be reached. Hence the `LLM_PROVIDERS` filter below — without
//    it, an operator who set up Telegram first got the step skipped and the
//    failed first run that #162 exists to prevent.
//
// So the predicate below is one-directional and must stay that way:
//   true  -> an LLM key IS stored, the step has nothing to offer, skip it
//   false -> nothing is KNOWN to be stored; show the step, but never require
//            anything from it. It is an offer, not a gate.
// Reading `false` as "this user has no key" would strand exactly the
// operators who configured themselves correctly the other way.

import { LLM_PROVIDERS } from './providers.js';

/**
 * The provider onboarding offers a key for.
 *
 * OpenRouter is the compiled default provider, and one key there covers both
 * compiled model defaults (chat and summary), so it is the single answer that
 * leaves nothing else to configure. Everything else is a Settings trip.
 */
export const ONBOARDING_PROVIDER = 'openrouter';

/**
 * True when `GET /auth/keys` reports at least one **LLM** provider with a key
 * in the secrets store.
 *
 * The `LLM_PROVIDERS` filter is load-bearing, not tidiness: the payload also
 * carries a `telegram` row, and a bot token cannot answer a chat run.
 *
 * Defensive against every non-answer the probe can produce (network failure
 * handed back as `null`, an empty body, a `keys` array of the wrong shape):
 * they all mean "not known to be stored", which shows the step rather than
 * hiding it. Showing an unnecessary step costs one click; hiding a necessary
 * one costs the first run.
 *
 * @param {{ keys?: Array<{ provider?: string, configured?: boolean }> } | null | undefined} payload
 * @returns {boolean}
 */
export function hasStoredKey(payload) {
    const keys = payload && payload.keys;
    if (!Array.isArray(keys)) return false;
    return keys.some(
        (k) => k && k.configured === true && LLM_PROVIDERS.includes(k.provider),
    );
}
