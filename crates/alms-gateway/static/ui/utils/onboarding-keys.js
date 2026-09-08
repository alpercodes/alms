// Key-probe helpers for the first-run onboarding flow (issue #162 half A).
//
// Onboarding offers to store a provider key before it creates the first
// agent, but skips that step when the daemon already holds one. The decision
// is made from `GET /auth/keys`, and the shape of that endpoint is the reason
// these helpers exist rather than an inline `.some(...)` at the call site.
//
// `list_keys` in `crates/alms-gateway/src/auth_keys.rs` DELIBERATELY ignores
// keys supplied through the environment — agents can read env vars via
// `shell_exec`, so only the secrets store is reported. An operator running
// with `OPENROUTER_API_KEY` exported therefore sees `configured: false` for
// every provider while having a perfectly working setup.
//
// So the predicate below is one-directional and must stay that way:
//   true  -> a key IS stored, the step has nothing to offer, skip it
//   false -> nothing is KNOWN to be stored; show the step, but never require
//            anything from it. It is an offer, not a gate.
// Reading `false` as "this user has no key" would strand exactly the
// operators who configured themselves correctly the other way.

/**
 * The provider onboarding offers a key for.
 *
 * OpenRouter is the compiled default provider, and one key there covers both
 * compiled model defaults (chat and summary), so it is the single answer that
 * leaves nothing else to configure. Everything else is a Settings trip.
 */
export const ONBOARDING_PROVIDER = 'openrouter';

/**
 * True when `GET /auth/keys` reports at least one provider with a key in the
 * secrets store.
 *
 * Defensive against every non-answer the probe can produce (network failure
 * handed back as `null`, an empty body, a `keys` array of the wrong shape):
 * they all mean "not known to be stored", which shows the step rather than
 * hiding it. Showing an unnecessary step costs one click; hiding a necessary
 * one costs the first run.
 *
 * @param {{ keys?: Array<{ configured?: boolean }> } | null | undefined} payload
 * @returns {boolean}
 */
export function hasStoredKey(payload) {
    const keys = payload && payload.keys;
    if (!Array.isArray(keys)) return false;
    return keys.some((k) => k && k.configured === true);
}
