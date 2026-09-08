// Pinned behaviour for issue #162 half A: the onboarding key step decides
// whether to show itself from `GET /auth/keys`, and that endpoint answers a
// narrower question than the step is asking — in TWO independent ways.
//
// 1. `list_keys` (crates/alms-gateway/src/auth_keys.rs) reports ONLY keys held
//    in the secrets store — env-var keys are deliberately excluded, because an
//    agent with `shell_exec` can read the environment. So an operator running
//    with `OPENROUTER_API_KEY` exported gets `configured: false` for every
//    provider while being fully configured.
//
// 2. It iterates `VALID_PROVIDERS`, which is a list of SECRET SLOTS, not of
//    LLM providers: `telegram` is in there as a channel bot token. A stored
//    Telegram token authenticates nothing an agent can think with.
//
// That makes the predicate one-directional, and these tests exist to keep it
// that way: `true` is licence to SKIP the step, `false` is licence only to
// SHOW it. Nothing downstream may read `false` as "this operator has no key"
// and turn the step into a gate — the field is optional and the skip button
// is unconditional. See `static/ui/utils/onboarding-keys.js`.
//
// Component-level invariants (skip button rendered and enabled, probe failure
// resolving to step 1, agent creation not gated) are NOT covered here — this
// module is a pure predicate. They are pinned in
// `frontend/e2e/onboarding.spec.ts` against the real bundle.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import url from 'node:url';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const MODULE_PATH = path.resolve(
    __dirname,
    '../../static/ui/utils/onboarding-keys.js'
);
const PROVIDERS_MODULE_PATH = path.resolve(
    __dirname,
    '../../static/ui/utils/providers.js'
);
const SECRETS_RS_PATH = path.resolve(
    __dirname,
    '../../../alms-core/src/secrets.rs'
);

const { hasStoredKey, ONBOARDING_PROVIDER } = await import(
    url.pathToFileURL(MODULE_PATH).href
);
const { LLM_PROVIDERS } = await import(
    url.pathToFileURL(PROVIDERS_MODULE_PATH).href
);

/**
 * `VALID_PROVIDERS` as the backend actually declares it, parsed out of
 * `secrets.rs` rather than retyped.
 *
 * Retyping it is what let the telegram bug through: the fixture below used to
 * carry a comment claiming "every provider present" over a list of four, so a
 * suite of this size sailed past the fifth. Reading the Rust makes that
 * particular lie impossible — if the list grows, this file fails until someone
 * classifies the newcomer.
 */
function validProvidersFromRust() {
    const src = fs.readFileSync(SECRETS_RS_PATH, 'utf8');
    const match = src.match(/VALID_PROVIDERS:\s*&\[&str\]\s*=\s*&\[([^\]]*)\]/);
    assert.notEqual(match, null, `could not find VALID_PROVIDERS in ${SECRETS_RS_PATH}`);
    return [...match[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]);
}

const VALID_PROVIDERS = validProvidersFromRust();

/** Shape of one entry in the `GET /auth/keys` response. */
const entry = (provider, configured) => ({
    provider,
    configured,
    key: configured ? 'sk-...abcd' : null,
    source: configured ? 'secrets' : 'none',
});

/**
 * The fresh-install payload: every slot the handler emits — i.e. every entry
 * of `VALID_PROVIDERS`, telegram included — present and unconfigured.
 */
const FRESH_INSTALL = {
    keys: VALID_PROVIDERS.map((p) => entry(p, false)),
};

test('#162: hasStoredKey is exported as a function', () => {
    assert.equal(typeof hasStoredKey, 'function');
});

test('#162: the offered provider is openrouter', () => {
    // OpenRouter is the compiled default provider, and one key there covers
    // both compiled model defaults (`z-ai/glm-5.2` for chat,
    // `google/gemma-4-31b-it` for summaries), so it is the only single answer
    // that leaves nothing else to configure. It must also be a member of
    // `VALID_PROVIDERS` in alms-core/src/secrets.rs — `PUT /auth/keys`
    // rejects anything else with INVALID_PROVIDER.
    assert.equal(ONBOARDING_PROVIDER, 'openrouter');
});

test('#162: fresh install (nothing stored) does not skip the key step', () => {
    // The state a reader who just cloned the repo is in — the exact
    // reproduction in the issue. This is the whole reason the step exists.
    assert.equal(hasStoredKey(FRESH_INSTALL), false);
});

/** FRESH_INSTALL with exactly one slot flipped to configured. */
const withStored = (provider) => ({
    keys: FRESH_INSTALL.keys.map((k) =>
        k.provider === provider ? entry(provider, true) : k
    ),
});

test('#162: any stored LLM key skips the step, whichever provider holds it', () => {
    // The step offers OpenRouter, but an operator who already pasted an
    // Anthropic key in Settings is configured and must not be asked again.
    for (const provider of LLM_PROVIDERS) {
        assert.equal(hasStoredKey(withStored(provider)), true, `expected skip for ${provider}`);
    }
});

test('#163: a stored telegram token does NOT skip the step', () => {
    // THE bug this filter exists for (Tim's review of PR #163). `telegram` is
    // in `VALID_PROVIDERS` because it is a secret slot — `alms auth set
    // telegram` writes it and the gateway reads it at startup to spawn the
    // polling loop — but it is a channel bot token, not an LLM credential.
    //
    // Before the filter, an operator who wired up Telegram before first
    // opening the dashboard had step 1 skipped, created an agent, and hit
    // exactly the failed first run #162 exists to prevent. The one case where
    // skipping is wrong was the one case that fired.
    assert.equal(hasStoredKey(withStored('telegram')), false);

    // ...and it must not mask a genuinely missing LLM key when combined with
    // other unconfigured rows, nor suppress a real one when both are present.
    assert.equal(
        hasStoredKey({ keys: [entry('telegram', true)] }),
        false
    );
    assert.equal(
        hasStoredKey({ keys: [entry('telegram', true), entry('openrouter', true)] }),
        true
    );
});

test('#163: unknown provider slots are ignored, not trusted', () => {
    // A future secret slot (or a hand-rolled `secrets.json`) must not be read
    // as an LLM credential by default. The safe direction is to show the step.
    assert.equal(hasStoredKey({ keys: [entry('smtp', true)] }), false);
    assert.equal(hasStoredKey({ keys: [entry('', true)] }), false);
    assert.equal(hasStoredKey({ keys: [{ configured: true }] }), false);
});

test('#163: LLM_PROVIDERS is VALID_PROVIDERS minus the non-LLM slots', () => {
    // The drift guard. `GET /auth/keys` iterates `VALID_PROVIDERS`, so every
    // entry of it reaches `hasStoredKey`, and each one has to be classified:
    // either it is an LLM credential (skipping the step is correct) or it is
    // not (skipping is the #163 bug).
    //
    // Adding a slot to secrets.rs without deciding fails HERE, loudly, rather
    // than silently widening the skip. If the newcomer is an LLM provider, add
    // it to `static/ui/utils/providers.js`; if it is another channel or
    // service token, add it to the list below.
    const NON_LLM_SLOTS = ['telegram'];

    assert.deepEqual(
        [...VALID_PROVIDERS].sort(),
        [...LLM_PROVIDERS, ...NON_LLM_SLOTS].sort(),
        'VALID_PROVIDERS (secrets.rs) has an entry that is in neither '
        + 'LLM_PROVIDERS nor NON_LLM_SLOTS — classify it before shipping'
    );

    // Sanity on the parse itself: if the regex ever silently matched nothing
    // the assertion above could pass vacuously against an empty JS list.
    assert.ok(VALID_PROVIDERS.length >= 5, 'parsed too few providers from secrets.rs');
    assert.ok(VALID_PROVIDERS.includes('telegram'));
    assert.ok(VALID_PROVIDERS.includes(ONBOARDING_PROVIDER));
});

test('#162: several stored keys still read as "stored"', () => {
    const payload = {
        keys: [entry('openrouter', true), entry('anthropic', true)],
    };
    assert.equal(hasStoredKey(payload), true);
});

test('#162: a masked key without `configured` does not count as stored', () => {
    // `configured` is the field the handler documents as "a key is directly
    // stored under this provider". Inferring from the masked value instead
    // would be a second, drifting source of truth.
    assert.equal(
        hasStoredKey({ keys: [{ provider: 'openrouter', key: 'sk-...abcd', source: 'none' }] }),
        false
    );
});

test('#162: only a real boolean true counts as stored', () => {
    // The contract bridge (frontend/contracts.ts, `GET /auth/keys`) validates
    // `configured` as a boolean, so anything else is already off-contract.
    // Requiring `=== true` keeps the failure mode on the safe side: an
    // unexpected value shows the step rather than hiding it.
    for (const bogus of ['true', 1, {}, [], 'yes']) {
        assert.equal(
            hasStoredKey({ keys: [{ provider: 'openrouter', configured: bogus }] }),
            false,
            `expected ${JSON.stringify(bogus)} not to count as stored`
        );
    }
});

test('#162: every non-answer means "show the step", never "hide it"', () => {
    // The probe's failure paths all funnel here: a rejected fetch handed
    // back as null/undefined, an empty body, a `keys` field of the wrong
    // type, or entries that aren't objects. Showing an unnecessary step
    // costs one click; hiding a necessary one costs the first run.
    assert.equal(hasStoredKey(null), false);
    assert.equal(hasStoredKey(undefined), false);
    assert.equal(hasStoredKey({}), false);
    assert.equal(hasStoredKey({ keys: [] }), false);
    assert.equal(hasStoredKey({ keys: null }), false);
    assert.equal(hasStoredKey({ keys: 'openrouter' }), false);
    assert.equal(hasStoredKey({ keys: { openrouter: true } }), false);
    assert.equal(hasStoredKey({ keys: [null, undefined] }), false);
    assert.equal(hasStoredKey('nope'), false);
    assert.equal(hasStoredKey(42), false);
});

test('#162: env-var keys are invisible here — false is not "has no key"', () => {
    // The load-bearing caveat, pinned as a test so it cannot be forgotten
    // during a refactor. An operator with `OPENROUTER_API_KEY` exported and
    // an empty secrets store produces EXACTLY the fresh-install payload:
    // there is no field that distinguishes them, which is the deliberate
    // design of `list_keys`, not an oversight to be worked around here.
    //
    // Consequence for the caller: `false` may only be used to SHOW step 1.
    // It may never be used to require a key, disable the skip button, or
    // block agent creation — that operator would be locked out of a setup
    // that already works.
    assert.equal(hasStoredKey(FRESH_INSTALL), false);
    assert.equal(hasStoredKey({ keys: FRESH_INSTALL.keys.map((k) => ({ ...k })) }), false);
});
