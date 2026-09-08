// Pinned behaviour for issue #162 half A: the onboarding key step decides
// whether to show itself from `GET /auth/keys`, and that endpoint answers a
// narrower question than the step is asking.
//
// `list_keys` (crates/alms-gateway/src/auth_keys.rs) reports ONLY keys held
// in the secrets store — env-var keys are deliberately excluded, because an
// agent with `shell_exec` can read the environment. So an operator running
// with `OPENROUTER_API_KEY` exported gets `configured: false` for every
// provider while being fully configured.
//
// That makes the predicate one-directional, and these tests exist to keep it
// that way: `true` is licence to SKIP the step, `false` is licence only to
// SHOW it. Nothing downstream may read `false` as "this operator has no key"
// and turn the step into a gate — the field is optional and the skip button
// is unconditional. See `static/ui/utils/onboarding-keys.js`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import url from 'node:url';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const MODULE_PATH = path.resolve(
    __dirname,
    '../../static/ui/utils/onboarding-keys.js'
);

const { hasStoredKey, ONBOARDING_PROVIDER } = await import(
    url.pathToFileURL(MODULE_PATH).href
);

/** Shape of one entry in the `GET /auth/keys` response. */
const entry = (provider, configured) => ({
    provider,
    configured,
    key: configured ? 'sk-...abcd' : null,
    source: configured ? 'secrets' : 'none',
});

/** The full fresh-install payload: every provider present, none configured. */
const FRESH_INSTALL = {
    keys: [
        entry('openai', false),
        entry('anthropic', false),
        entry('openrouter', false),
        entry('gemini', false),
    ],
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

test('#162: any stored key skips the step, whichever provider holds it', () => {
    // The step offers OpenRouter, but an operator who already pasted an
    // Anthropic key in Settings is configured and must not be asked again.
    for (const provider of ['openai', 'anthropic', 'openrouter', 'gemini']) {
        const payload = {
            keys: FRESH_INSTALL.keys.map((k) =>
                k.provider === provider ? entry(provider, true) : k
            ),
        };
        assert.equal(hasStoredKey(payload), true, `expected skip for ${provider}`);
    }
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
