// JS-level regression tests for `static/ui/utils/debug-mode-patch.js`.
//
// Pinned regression target:
//   - issue #1003 — restore the per-agent Debug-mode toggle that was
//     swept out in #941 alongside the per-run config-override path.
//     The frontend portion of #1003 lives on two surfaces — the
//     AgentEditModal and the Settings modal Debug section — both of
//     which compute the PATCH /agents/{id} delta the same way:
//     "send `debug_mode: <new>` only when the form value differs from
//     the stored value, otherwise omit the field entirely so opening
//     and closing the modal without touching anything never PATCHes".
//
//     The diff has subtle "undefined vs false" semantics. Pre-#1003
//     agent records carry `debug_mode === undefined` because the
//     field was absent from the GET /agents response; both UI surfaces
//     must treat that as `false` when comparing against the form
//     state, otherwise opening the modal on a legacy record and
//     pressing Apply would always emit a redundant PATCH that flips
//     the record from "no field" to "explicit false" on every save.
//
//     Pinning the helper here means a future refactor that touches
//     either UI surface can't silently regress the wire shape — both
//     surfaces import the same helper, and this file pins all four
//     transition cells (off→off, off→on, on→on, on→off) plus the
//     `undefined` fallback.
//
// The module under test is pure — no `localStorage`, no DOM, no
// preact — so the test loads it directly without a stub harness.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import url from 'node:url';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const HELPER_PATH = path.resolve(
    __dirname,
    '../../static/ui/utils/debug-mode-patch.js'
);

async function loadHelper() {
    const fileUrl = url.pathToFileURL(HELPER_PATH).href
        + '?cb=' + Date.now() + '-' + Math.random();
    return await import(fileUrl);
}

// ---------------------------------------------------------------------
// All four transition cells — stored × draft.
// ---------------------------------------------------------------------

test('debugModePatchDelta: stored=false, draft=false => empty body (no PATCH)', async () => {
    const { debugModePatchDelta } = await loadHelper();
    const delta = debugModePatchDelta(false, false);
    assert.deepEqual(delta, {});
});

test('debugModePatchDelta: stored=true, draft=true => empty body (no PATCH)', async () => {
    const { debugModePatchDelta } = await loadHelper();
    const delta = debugModePatchDelta(true, true);
    assert.deepEqual(delta, {});
});

test('debugModePatchDelta: stored=false, draft=true => { debug_mode: true }', async () => {
    const { debugModePatchDelta } = await loadHelper();
    const delta = debugModePatchDelta(false, true);
    assert.deepEqual(delta, { debug_mode: true });
});

test('debugModePatchDelta: stored=true, draft=false => { debug_mode: false }', async () => {
    const { debugModePatchDelta } = await loadHelper();
    const delta = debugModePatchDelta(true, false);
    assert.deepEqual(delta, { debug_mode: false });
});

// ---------------------------------------------------------------------
// `undefined` / `null` fallback — pre-#1003 agent records.
// ---------------------------------------------------------------------

test('debugModePatchDelta: stored=undefined, draft=false => empty body', async () => {
    // Pre-#1003 GET /agents responses don't carry `debug_mode` at all,
    // so the field arrives at the form as `undefined`. Treating it as
    // `false` means "open the modal, change nothing, press Apply" never
    // emits a redundant PATCH.
    const { debugModePatchDelta } = await loadHelper();
    const delta = debugModePatchDelta(undefined, false);
    assert.deepEqual(delta, {});
});

test('debugModePatchDelta: stored=null, draft=false => empty body', async () => {
    // Belt-and-braces: same coercion for `null`. Wire shape never
    // sends `null` for this field today, but the helper should be
    // robust against future server-side serde changes that might
    // (e.g. `Some(false)` round-tripping through a permissive
    // serde derive).
    const { debugModePatchDelta } = await loadHelper();
    const delta = debugModePatchDelta(null, false);
    assert.deepEqual(delta, {});
});

test('debugModePatchDelta: stored=undefined, draft=true => { debug_mode: true }', async () => {
    // First-time enable on a legacy record: stored is `undefined`
    // (no field on the wire), draft flipped to `true` — emit
    // `debug_mode: true` so the PATCH actually creates the per-agent
    // override.
    const { debugModePatchDelta } = await loadHelper();
    const delta = debugModePatchDelta(undefined, true);
    assert.deepEqual(delta, { debug_mode: true });
});

// ---------------------------------------------------------------------
// Truthy / falsy non-boolean inputs — defensive coercion.
// ---------------------------------------------------------------------

test('debugModePatchDelta coerces truthy/falsy non-booleans through `!!`', async () => {
    // The helper coerces both arguments through `!!` so "stored=1"
    // and "draft=true" compare equal (no PATCH), and "stored=0" and
    // "draft=false" likewise. This is defence-in-depth — the form
    // state is always a boolean signal — but pinning the contract
    // here means a future caller passing through e.g. `agent.debug_mode`
    // straight from JSON.parse without coercion can't accidentally
    // produce a spurious PATCH.
    const { debugModePatchDelta } = await loadHelper();
    assert.deepEqual(debugModePatchDelta(1, true), {});
    assert.deepEqual(debugModePatchDelta(0, false), {});
    assert.deepEqual(debugModePatchDelta('', false), {});
    assert.deepEqual(debugModePatchDelta('yes', true), {});
    // A truthy stored vs a falsy draft must still emit the PATCH.
    assert.deepEqual(debugModePatchDelta(1, false), { debug_mode: false });
});
