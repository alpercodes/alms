// JS-level tests for `historyCoversSeal` in
// `static/ui/utils/reasoning-coverage.js` — the load-time coverage gate for
// the `reasoning_delta` suppress-set (#1133, Codex finding #3 / sub-race B).
// See that module's docstring for the full rationale; these tests pin the
// sub-race split and the conservative fallbacks.
//
// The module under test is a pure leaf with no imports, so it is loaded
// directly via static import — no signal/api stubbing required.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { historyCoversSeal } from '../../static/ui/utils/reasoning-coverage.js';

test('sub-race A: history HWM at/above the seal anchor covers the seal -> suppress', () => {
    // The messages GET resolved AFTER the run sealed + broadcast its terminal
    // event, so the sealed reasoning is in the loaded history and the replayed
    // deltas would double-render. Coverage is proven -> add to the set.
    assert.equal(historyCoversSeal(100, 100), true, 'HWM == seal is covered');
    assert.equal(historyCoversSeal(101, 100), true, 'HWM > seal is covered');
    assert.equal(historyCoversSeal(5000, 42), true, 'HWM well above seal is covered');
});

test('sub-race B: history HWM below the seal anchor does NOT cover the seal -> do not suppress', () => {
    // The messages GET resolved BEFORE the run sealed its assistant message
    // (its HWM was sampled at most at the last reasoning delta, strictly below
    // the terminal-event id), so the sealed reasoning is NOT in history. The
    // replayed deltas are the only source of that reasoning and must render —
    // coverage is NOT proven, so the run must NOT be added to the suppress-set.
    assert.equal(historyCoversSeal(99, 100), false, 'HWM one below seal is uncovered');
    assert.equal(historyCoversSeal(0, 100), false, 'HWM far below seal is uncovered');
    assert.equal(historyCoversSeal(41, 42), false, 'HWM just below seal is uncovered');
});

test('missing / null seal anchor is conservative -> do not suppress', () => {
    // A terminal run with no terminal SSE event logged yet returns
    // seal_event_id: null. Without an anchor, coverage cannot be proven, so the
    // predicate returns false (render once rather than risk zero renders).
    assert.equal(historyCoversSeal(100, null), false, 'null seal anchor -> false');
    assert.equal(historyCoversSeal(100, undefined), false, 'undefined seal anchor -> false');
    assert.equal(historyCoversSeal(100, 'not-a-number'), false, 'non-numeric seal anchor -> false');
    assert.equal(historyCoversSeal(100, NaN), false, 'NaN seal anchor -> false');
});

test('missing / null history HWM is conservative -> do not suppress', () => {
    // If the messages GET produced no high-water mark (null lastEventId), there
    // is no basis to claim coverage; do not suppress.
    assert.equal(historyCoversSeal(null, 100), false, 'null HWM -> false');
    assert.equal(historyCoversSeal(undefined, 100), false, 'undefined HWM -> false');
});

test('string-numeric history HWM compares numerically, not lexically', () => {
    // `lastEventId` is sometimes a string (it originates from `e.lastEventId`
    // on the SSE wire). The predicate coerces with Number() so the comparison
    // is numeric — a lexical compare would mis-rank e.g. "9" vs 10.
    assert.equal(historyCoversSeal('100', 100), true, 'string "100" >= 100');
    assert.equal(historyCoversSeal('10', 9), true, 'string "10" >= 9 numerically');
    assert.equal(historyCoversSeal('9', 10), false, 'string "9" < 10 numerically');
});
