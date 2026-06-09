// JS-level tests for the per-session reasoning-dedupe suppress-set store in
// `static/ui/state/reasoning-dedupe.js` (#1135, follow-up to #1133 / PR #1134
// Layer 3).
//
// Background: the Layer-3 reasoning-dedupe suppress-set (`sealedReasoningRunIds`)
// used to live ONLY on the initial `openSessionStream` `opts`. On a mid-replay
// EventSource reconnect — which reopens with `{ lastEventId }` only — the set
// was lost, so already-sealed reasoning could re-duplicate as a spurious
// unsealed bubble until the next full reload. This module hoists the set to a
// per-session module-scoped store so the auto-backoff and manual reconnect
// paths recover the SAME set after the originating `opts` object is gone.
//
// The module under test is a pure leaf with no imports (no signals / deps.js),
// so it is loaded via dynamic import with a cache-busting query so each test
// gets fresh module-level state. This mirrors `reasoning-coverage.test.mjs`
// (direct import of a pure leaf) and `stream-health.test.mjs` (per-test fresh
// evaluation for module-scoped state).

import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import url from 'node:url';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const MODULE_PATH = path.resolve(
    __dirname,
    '../../static/ui/state/reasoning-dedupe.js',
);

/**
 * Import a FRESH evaluation of the module so module-scoped state (the
 * per-session Map) does not leak across tests.
 */
async function loadModule() {
    const fileUrl = url.pathToFileURL(MODULE_PATH).href
        + '?cb=' + Date.now() + '-' + Math.random();
    return import(fileUrl);
}

let mod;

beforeEach(async () => {
    mod = await loadModule();
});

// ---------------------------------------------------------------------
// Store / recover round-trip
// ---------------------------------------------------------------------

test('set then get round-trips the same Set instance', async () => {
    const set = new Set(['run-1', 'run-2']);
    mod.setSealedReasoningRunIds('sess-A', set);
    const got = mod.getSealedReasoningRunIds('sess-A');
    assert.strictEqual(got, set, 'returns the same Set reference');
    assert.equal(got.has('run-1'), true);
    assert.equal(got.has('run-2'), true);
});

test('get for an unknown session returns null (not undefined)', async () => {
    assert.equal(mod.getSealedReasoningRunIds('never-stored'), null);
});

// ---------------------------------------------------------------------
// Reconnect contract — the core #1135 fix
// ---------------------------------------------------------------------

test('the set survives a recover-before-clear, re-record cycle (reconnect)', async () => {
    // Models the openSessionStream reconnect flow: a load records the set,
    // then a same-session reconnect recovers it BEFORE the internal close
    // clears the entry, then re-records it. After the cycle the guard must
    // still see the suppressed run-id.
    const set = new Set(['terminal-run']);
    mod.setSealedReasoningRunIds('sess-A', set);

    // Reconnect: recover (opts carries no set on reconnect)...
    const carried = mod.getSealedReasoningRunIds('sess-A');
    assert.ok(carried instanceof Set, 'recovered set is available pre-close');

    // ...internal closeSessionStream drops the entry...
    mod.clearSealedReasoningRunIds('sess-A');
    assert.equal(mod.getSealedReasoningRunIds('sess-A'), null,
        'entry is gone immediately after the internal close');

    // ...openSessionStream re-records the carried set after the close.
    mod.setSealedReasoningRunIds('sess-A', carried);

    const afterReconnect = mod.getSealedReasoningRunIds('sess-A');
    assert.ok(afterReconnect instanceof Set, 'set is recovered after reconnect');
    assert.equal(afterReconnect.has('terminal-run'), true,
        'the sealed run-id still suppresses replayed deltas post-reconnect');
});

// ---------------------------------------------------------------------
// Per-session scoping — no cross-session leakage (acceptance criterion)
// ---------------------------------------------------------------------

test('sets are scoped per sessionId; one session cannot read another', async () => {
    mod.setSealedReasoningRunIds('sess-A', new Set(['run-a']));
    mod.setSealedReasoningRunIds('sess-B', new Set(['run-b']));

    assert.equal(mod.getSealedReasoningRunIds('sess-A').has('run-a'), true);
    assert.equal(mod.getSealedReasoningRunIds('sess-A').has('run-b'), false,
        'session A must not see session B run-ids');
    assert.equal(mod.getSealedReasoningRunIds('sess-B').has('run-b'), true);
    assert.equal(mod.getSealedReasoningRunIds('sess-B').has('run-a'), false,
        'session B must not see session A run-ids');
});

test('clearing one session leaves the other intact', async () => {
    mod.setSealedReasoningRunIds('sess-A', new Set(['run-a']));
    mod.setSealedReasoningRunIds('sess-B', new Set(['run-b']));

    mod.clearSealedReasoningRunIds('sess-A');

    assert.equal(mod.getSealedReasoningRunIds('sess-A'), null,
        'cleared session has no set');
    assert.ok(mod.getSealedReasoningRunIds('sess-B') instanceof Set,
        'other session is untouched');
    assert.deepEqual(mod._trackedSessionsSnapshot(), ['sess-B']);
});

// ---------------------------------------------------------------------
// Cleanup — no unbounded growth (acceptance criterion)
// ---------------------------------------------------------------------

test('switching across many sessions does not accumulate entries when each is cleared', async () => {
    // Mirrors the openSessionStream-switch path: each session-switch tears
    // down the previous stream (clearSealedReasoningRunIds on the old
    // sessionId) before recording the new one. The store size must stay
    // bounded at 1, not grow unbounded across switches.
    for (let i = 0; i < 50; i++) {
        const prev = `sess-${i - 1}`;
        const cur = `sess-${i}`;
        mod.clearSealedReasoningRunIds(prev); // teardown of the prior stream
        mod.setSealedReasoningRunIds(cur, new Set([`run-${i}`]));
        assert.equal(mod._trackedSessionsSnapshot().length, 1,
            `only the current session is tracked at step ${i}`);
    }
    assert.deepEqual(mod._trackedSessionsSnapshot(), ['sess-49']);
});

test('clearSealedReasoningRunIds() with no argument clears every entry', async () => {
    mod.setSealedReasoningRunIds('sess-A', new Set(['a']));
    mod.setSealedReasoningRunIds('sess-B', new Set(['b']));
    mod.setSealedReasoningRunIds('sess-C', new Set(['c']));

    mod.clearSealedReasoningRunIds();

    assert.deepEqual(mod._trackedSessionsSnapshot(), [],
        'no-arg clear drops every tracked session');
});

// ---------------------------------------------------------------------
// No-op safety — the 4 openSessionStream callers that pass no set
// ---------------------------------------------------------------------

test('set with a falsy sessionId is a no-op', async () => {
    mod.setSealedReasoningRunIds(null, new Set(['x']));
    mod.setSealedReasoningRunIds(undefined, new Set(['x']));
    mod.setSealedReasoningRunIds('', new Set(['x']));
    assert.deepEqual(mod._trackedSessionsSnapshot(), [],
        'falsy sessionId never stores anything');
});

test('set with a non-Set value is a no-op (no-set callers stay safe)', async () => {
    // The new-session / boot / session-list / reconnect-before-first-load
    // callers pass no `sealedReasoningRunIds`; the guard reads back `null`
    // and is therefore inert.
    mod.setSealedReasoningRunIds('sess-A', undefined);
    mod.setSealedReasoningRunIds('sess-A', null);
    mod.setSealedReasoningRunIds('sess-A', ['not', 'a', 'set']);
    mod.setSealedReasoningRunIds('sess-A', { has: () => true });
    assert.equal(mod.getSealedReasoningRunIds('sess-A'), null,
        'non-Set values never store; the guard stays a no-op');
});

test('get with a falsy sessionId returns null', async () => {
    assert.equal(mod.getSealedReasoningRunIds(null), null);
    assert.equal(mod.getSealedReasoningRunIds(undefined), null);
    assert.equal(mod.getSealedReasoningRunIds(''), null);
});

// ---------------------------------------------------------------------
// Last-write-wins — a fresh loadSession supersedes the old set
// ---------------------------------------------------------------------

test('re-recording for the same session replaces the previous set (last-write-wins)', async () => {
    mod.setSealedReasoningRunIds('sess-A', new Set(['old-run']));
    const fresh = new Set(['new-run']);
    mod.setSealedReasoningRunIds('sess-A', fresh);

    const got = mod.getSealedReasoningRunIds('sess-A');
    assert.strictEqual(got, fresh, 'newest set wins');
    assert.equal(got.has('old-run'), false, 'old run-id no longer suppressed');
    assert.equal(got.has('new-run'), true);
});
