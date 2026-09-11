// The provider slots that hold an LLM credential.
//
// `VALID_PROVIDERS` in `crates/alms-core/src/secrets.rs` is the single source
// of truth for what `PUT /auth/keys` accepts and what `GET /auth/keys` reports,
// and it holds FIVE entries — the four below plus `telegram`. That fifth one is
// a channel bot token (read at gateway startup to spawn the Telegram polling
// loop), not something any LLM call can authenticate with.
//
// So the two lists are not interchangeable, and code that asks "is this
// operator able to talk to a model?" must filter to this one. Reading
// `telegram` as an LLM credential is what shipped as a bug in PR #163: an
// operator who wired up Telegram first had the onboarding key step skipped and
// landed on exactly the failed first run that issue #162 exists to prevent.
//
// Kept as its own module so the Settings API-key rows and the onboarding probe
// share one list rather than two that drift. `onboarding-keys.test.mjs` pins it
// against `secrets.rs`, so adding a sixth secret slot there fails that test
// until someone decides which kind it is.
export const LLM_PROVIDERS = ['openai', 'anthropic', 'openrouter', 'gemini'];
