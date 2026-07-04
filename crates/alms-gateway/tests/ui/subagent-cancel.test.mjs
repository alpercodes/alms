// Node-side tests for the subagent cancel-confirm flow
// (`static/ui/state/subagent-cancel.js`) — the shared decision layer behind
// BOTH cancel surfaces added with the session-keyed subagent cancel feature:
//
//   1. the ✕ control on RUNNING chips in the Subagent status bar
//      (`components/chat/subagent-bar.js`), and
//   2. the "Cancel subagent" button in the drilled-down subagent session
//      view's breadcrumb header (`app.js`).
//
// Both surfaces call the exact same four functions with a subagent SESSION
// id, so exercising the module here covers the interaction contract for
// both buttons. Per the house testing pattern (#983 / card-activation): the
// unit-testable surface is the decision layer; preact rendering and DOM
// event bubbling (`stopPropagation` on the nested chip controls) are
// browser concerns, deliberately not pinned under Node.
//
// Pinned behaviors (Atlas's acceptance list):
//   1. Arming the confirm (✕ / Cancel click) shows the confirm UI
//      (`isCancelPending`) and makes NO API call.
//   2. Confirming calls `cancelSubagent` with the CORRECT session id,
//      exactly once — a double-click on Yes cannot fire twice.
//   3. Dismissing ("No") makes NO API call and restores the normal control.
//   4. The unknown-session-id guard: no control renders while the entry
//      has no sessionId (`showCancelControl`), arming with a nullish id is
//      refused, and a confirm can never fire with a nullish id.
//   5. Only RUNNING subagents offer cancel — terminal chips don't.
//   6. The armed confirm auto-reverts after CONFIRM_REVERT_MS (misclick on
//      the tiny chip target must not leave a live-fire Yes parked).
//   7. A stale Yes (auto-reverted, or re-armed for another session) makes
//      no call.
//   8. Chip-lifecycle clearing (Codex P2, PR #1192): the armed confirm is
//      dismissed when the armed subagent goes terminal, when its chip is
//      auto-removed, and on clearAllSubagents (session switch) — named
//      subagents reuse the same session id across re-invocations, so a
//      surviving confirm would pre-arm the NEXT invocation's chip. An
//      UNRELATED subagent's lifecycle never dismisses the armed confirm.
//      Driven through the REAL `state/subagents.js` wired to the same
//      cancel-module instance.
//
// Strategy: same loader pattern as subagent-status-bar.test.mjs — read the
// REAL `state/subagent-cancel.js`, rewrite its two top-level imports
// (signal from deps.js, cancelSubagent from api/sessions.js) with a stub
// block whose API stub RECORDS calls, write to a temp file, import.

import { test, beforeEach, mock } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import url from 'node:url';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const CANCEL_JS_PATH = path.resolve(
    __dirname,
    '../../static/ui/state/subagent-cancel.js'
);

// ---------------------------------------------------------------------------
// Load the REAL `state/subagent-cancel.js` under Node, rewriting its two
// top-level imports and appending test-only accessors for the recording
// API stub.
// ---------------------------------------------------------------------------
const SIGNAL_STUB = `
function signal(initial) {
    let v = initial;
    return {
        get value() { return v; },
        set value(next) { v = next; },
    };
}
`;

const STUB_PRELUDE = SIGNAL_STUB + `
// Recording stub for api/sessions.js's cancelSubagent. Resolves like the
// real helper (the module ignores the payload); set __rejectNext to make
// the next call reject (the 404 NO_LIVE_SUBAGENT shape).
const __cancelCalls = [];
let __rejectNext = false;
function cancelSubagent(sessionId) {
    __cancelCalls.push(sessionId);
    if (__rejectNext) {
        __rejectNext = false;
        return Promise.reject({ status: 404, error: { code: 'NO_LIVE_SUBAGENT' } });
    }
    return Promise.resolve({ session_id: sessionId, status: 'cancelling' });
}
export function __getCancelCalls() { return __cancelCalls; }
export function __resetCancelCalls() { __cancelCalls.length = 0; }
export function __rejectNextCancel() { __rejectNext = true; }
`;

function loadRealCancelModuleAsTempFile() {
    const src = fs.readFileSync(CANCEL_JS_PATH, 'utf8');

    const signalImportRe =
        /^import\s+\{\s*signal\s*\}\s+from\s+['"][^'"]+['"];?\s*$/m;
    if (!signalImportRe.test(src)) {
        throw new Error(
            'subagent-cancel.js: expected a top-level `import { signal } from ...` line — '
            + 'update subagent-cancel.test.mjs if the import shape changed.'
        );
    }
    const apiImportRe =
        /^import\s+\{\s*cancelSubagent\s*\}\s+from\s+['"][^'"]+['"];?\s*$/m;
    if (!apiImportRe.test(src)) {
        throw new Error(
            'subagent-cancel.js: expected a top-level `import { cancelSubagent } from ...` line — '
            + 'update subagent-cancel.test.mjs if the import shape changed.'
        );
    }

    const stubbed = src
        .replace(signalImportRe, STUB_PRELUDE)
        .replace(apiImportRe, '');

    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'alms-sa-cancel-'));
    const tmpFile = path.join(tmpDir, 'subagent-cancel.mjs');
    fs.writeFileSync(tmpFile, stubbed, 'utf8');
    return url.pathToFileURL(tmpFile).href;
}

// Keep the temp-module URL: the subagents.js integration loader below
// imports the SAME url, so both modules share one instance (Node module
// cache) — the lifecycle hooks in subagents.js must clear the exact
// pending state these tests observe.
const realCancelUrl = loadRealCancelModuleAsTempFile();

const {
    CONFIRM_REVERT_MS,
    pendingCancelSessionId,
    showCancelControl,
    isCancelPending,
    requestSubagentCancel,
    dismissSubagentCancel,
    confirmSubagentCancel,
    __getCancelCalls,
    __resetCancelCalls,
    __rejectNextCancel,
} = await import(realCancelUrl);

// ---------------------------------------------------------------------------
// Load the REAL `state/subagents.js` WIRED to the real cancel module above,
// for the chip-lifecycle integration tests (Codex P2, PR #1192). Same
// loader/rewrite strategy as subagent-status-bar.test.mjs, except the
// cancel-hook import is rewritten to the shared temp module instead of a
// no-op stub.
// ---------------------------------------------------------------------------
const SUBAGENTS_JS_PATH = path.resolve(
    __dirname,
    '../../static/ui/state/subagents.js'
);

function loadRealSubagentsWiredToCancel(cancelModuleUrl) {
    const src = fs.readFileSync(SUBAGENTS_JS_PATH, 'utf8');

    const signalImportRe =
        /^import\s+\{\s*signal\s*\}\s+from\s+['"][^'"]+['"];?\s*$/m;
    const sessionsImportRe =
        /^import\s+\{\s*activeSessionId\s*\}\s+from\s+['"][^'"]+['"];?\s*$/m;
    const cancelImportRe =
        /^import\s+\{\s*clearCancelConfirmForSession\s*,\s*dismissSubagentCancel\s*\}\s+from\s+['"][^'"]+['"];?\s*$/m;
    for (const [re, what] of [
        [signalImportRe, 'signal'],
        [sessionsImportRe, 'activeSessionId'],
        [cancelImportRe, 'clearCancelConfirmForSession, dismissSubagentCancel'],
    ]) {
        if (!re.test(src)) {
            throw new Error(
                `subagents.js: expected a top-level \`import { ${what} } from ...\` line — `
                + 'update subagent-cancel.test.mjs if the import shape changed.'
            );
        }
    }

    const stubbed = src
        .replace(signalImportRe, SIGNAL_STUB)
        .replace(sessionsImportRe,
            'const activeSessionId = { get value() { return null; }, set value(_) {} };')
        .replace(cancelImportRe,
            `import { clearCancelConfirmForSession, dismissSubagentCancel } from ${JSON.stringify(cancelModuleUrl)};`);

    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'alms-sa-cancel-int-'));
    const tmpFile = path.join(tmpDir, 'subagents.mjs');
    fs.writeFileSync(tmpFile, stubbed, 'utf8');
    return url.pathToFileURL(tmpFile).href;
}

const {
    trackSubagentStart,
    trackSubagentEnd,
    setSubagentSessionId,
    clearAllSubagents,
} = await import(loadRealSubagentsWiredToCancel(realCancelUrl));

beforeEach(() => {
    // Reset armed state AND the auto-revert timer between tests (dismiss
    // clears both), then drop recorded calls, then clear chip state and its
    // removal timers. Also keeps the node:test process from idling on
    // dangling real timers at suite end.
    clearAllSubagents();
    dismissSubagentCancel();
    __resetCancelCalls();
});

// ---------------------------------------------------------------------
// showCancelControl — which chips render a cancel affordance at all
// ---------------------------------------------------------------------

test('showCancelControl: true only for RUNNING entries with a known sessionId', () => {
    assert.equal(
        showCancelControl({ status: 'running', sessionId: 'sess-1' }),
        true,
    );
});

test('showCancelControl: hidden while sessionId is unknown (lag window guard)', () => {
    // A subagent's session id can lag the chip (foreground subagents get it
    // via `subagent_started`); until then the chip click is a no-op and the
    // cancel control must not render — there is nothing safe to cancel.
    assert.equal(showCancelControl({ status: 'running', sessionId: null }), false);
    assert.equal(showCancelControl({ status: 'running' }), false);
});

test('showCancelControl: hidden for terminal chips', () => {
    for (const status of ['done', 'fail', 'cancelled']) {
        assert.equal(
            showCancelControl({ status, sessionId: 'sess-1' }),
            false,
            `terminal status '${status}' must not offer cancel`,
        );
    }
});

test('showCancelControl: defensive on nullish entries', () => {
    assert.equal(showCancelControl(null), false);
    assert.equal(showCancelControl(undefined), false);
});

// ---------------------------------------------------------------------
// Arm (✕ / Cancel click) → confirm UI, no API call
// ---------------------------------------------------------------------

test('requestSubagentCancel arms the confirm for that session and makes NO call', () => {
    assert.equal(requestSubagentCancel('sess-1'), true);
    assert.equal(isCancelPending('sess-1'), true);
    assert.equal(isCancelPending('sess-2'), false);
    assert.deepEqual(__getCancelCalls(), [], 'arming must not call the API');
    dismissSubagentCancel();
});

test('requestSubagentCancel refuses a nullish session id (unknown-id guard)', () => {
    assert.equal(requestSubagentCancel(null), false);
    assert.equal(requestSubagentCancel(undefined), false);
    assert.equal(pendingCancelSessionId.value, null, 'nothing must be armed');
    assert.deepEqual(__getCancelCalls(), []);
});

test('arming a second session replaces the first (one live-fire Yes at a time)', () => {
    requestSubagentCancel('sess-A');
    requestSubagentCancel('sess-B');
    assert.equal(isCancelPending('sess-A'), false, 'A\'s confirm must close');
    assert.equal(isCancelPending('sess-B'), true);
    // The Yes that belonged to A must now be a no-op.
    return confirmSubagentCancel('sess-A').then((fired) => {
        assert.equal(fired, false);
        assert.deepEqual(__getCancelCalls(), [], 'A\'s stale Yes must not call');
        dismissSubagentCancel();
    });
});

// ---------------------------------------------------------------------
// Confirm (Yes) → exactly one call with the right session id
// ---------------------------------------------------------------------

test('confirmSubagentCancel calls cancelSubagent once with the armed session id', async () => {
    requestSubagentCancel('sess-42');
    const fired = await confirmSubagentCancel('sess-42');
    assert.equal(fired, true);
    assert.deepEqual(__getCancelCalls(), ['sess-42']);
    assert.equal(isCancelPending('sess-42'), false, 'confirm consumes the armed state');
});

test('double-clicking Yes fires exactly one API call', async () => {
    requestSubagentCancel('sess-42');
    // Fire both before awaiting — models a fast double-click. The armed
    // state is cleared synchronously before the request, so the second
    // invocation must see nothing armed.
    const first = confirmSubagentCancel('sess-42');
    const second = confirmSubagentCancel('sess-42');
    assert.deepEqual(await Promise.all([first, second]), [true, false]);
    assert.deepEqual(__getCancelCalls(), ['sess-42'], 'exactly one call');
});

test('confirm without an armed confirm makes NO call', async () => {
    assert.equal(await confirmSubagentCancel('sess-1'), false);
    assert.deepEqual(__getCancelCalls(), []);
});

test('confirm with a nullish session id makes NO call even if state is corrupt', async () => {
    // Belt-and-suspenders: even if the pending signal somehow held a
    // nullish value, a nullish confirm must not fire.
    pendingCancelSessionId.value = null;
    assert.equal(await confirmSubagentCancel(null), false);
    assert.equal(await confirmSubagentCancel(undefined), false);
    assert.deepEqual(__getCancelCalls(), []);
});

test('a rejected cancel (already-finished race → 404) resolves false, no throw', async () => {
    requestSubagentCancel('sess-9');
    __rejectNextCancel();
    const fired = await confirmSubagentCancel('sess-9');
    assert.equal(fired, false);
    assert.deepEqual(__getCancelCalls(), ['sess-9'], 'the call was made');
    assert.equal(isCancelPending('sess-9'), false);
});

// ---------------------------------------------------------------------
// Dismiss (No) → no call, control restored
// ---------------------------------------------------------------------

test('dismissSubagentCancel makes NO call and restores the normal control', async () => {
    requestSubagentCancel('sess-7');
    dismissSubagentCancel();
    assert.equal(isCancelPending('sess-7'), false);
    assert.deepEqual(__getCancelCalls(), []);
    // A Yes clicked after the dismissal (stale confirm) must be a no-op.
    assert.equal(await confirmSubagentCancel('sess-7'), false);
    assert.deepEqual(__getCancelCalls(), []);
});

// ---------------------------------------------------------------------
// Auto-revert — the armed confirm self-dismisses
// ---------------------------------------------------------------------

test('the armed confirm auto-reverts after CONFIRM_REVERT_MS with NO call', async () => {
    mock.timers.enable({ apis: ['setTimeout'] });
    try {
        requestSubagentCancel('sess-5');
        assert.equal(isCancelPending('sess-5'), true);

        mock.timers.tick(CONFIRM_REVERT_MS - 1);
        assert.equal(isCancelPending('sess-5'), true, 'still armed just before the deadline');

        mock.timers.tick(1);
        assert.equal(isCancelPending('sess-5'), false, 'auto-reverted at the deadline');
        assert.deepEqual(__getCancelCalls(), [], 'auto-revert must not call the API');

        // A Yes that lands after the revert (slow operator) must be a no-op.
        assert.equal(await confirmSubagentCancel('sess-5'), false);
        assert.deepEqual(__getCancelCalls(), []);
    } finally {
        mock.timers.reset();
    }
});

test('re-arming re-arms the auto-revert timer (no stale-timer early dismiss)', () => {
    mock.timers.enable({ apis: ['setTimeout'] });
    try {
        requestSubagentCancel('sess-A');
        mock.timers.tick(CONFIRM_REVERT_MS - 1);
        // Re-arm (another ✕ click, possibly a different chip) 1ms before
        // the first timer would fire: the OLD timer must not dismiss the
        // NEW confirm.
        requestSubagentCancel('sess-B');
        mock.timers.tick(1);
        assert.equal(
            isCancelPending('sess-B'), true,
            'the first arm\'s timer must have been cleared by the re-arm',
        );
        mock.timers.tick(CONFIRM_REVERT_MS - 1);
        assert.equal(isCancelPending('sess-B'), false, 'the re-armed timer fires on its own schedule');
    } finally {
        mock.timers.reset();
    }
});

// ---------------------------------------------------------------------
// Chip-lifecycle integration (Codex P2, PR #1192): named subagents reuse
// the SAME session id across re-invocations, so an armed confirm must be
// cleared when the armed subagent goes terminal / its chip is removed /
// the session view switches — otherwise the NEXT invocation's chip
// renders pre-armed and its Yes live-fires with no confirming first
// click. Driven through the REAL `state/subagents.js` wired to the REAL
// cancel module (shared instance).
// ---------------------------------------------------------------------

test('Codex P2: terminal transition clears the armed confirm — a same-session re-invocation is not pre-armed', async () => {
    // Named subagent: entry keyed by name; the session id is deterministic
    // and therefore REUSED by the next invocation.
    trackSubagentStart('reviewer', 'first run', 'inv-1');
    setSubagentSessionId('reviewer', 'sess-named', 'inv-1');
    assert.equal(requestSubagentCancel('sess-named'), true);
    assert.equal(isCancelPending('sess-named'), true, 'confirm armed on the live chip');

    // The subagent finishes while the confirm is still armed (inside the
    // auto-revert window).
    trackSubagentEnd('reviewer', 'done', 'inv-1', 'sess-named');
    assert.equal(
        isCancelPending('sess-named'), false,
        'the terminal transition must clear the armed confirm',
    );
    assert.deepEqual(__getCancelCalls(), [], 'clearing is a dismissal — no API call');

    // Re-invocation reuses the SAME session id. The fresh chip must not
    // render pre-armed…
    trackSubagentStart('reviewer', 'second run', 'inv-2');
    setSubagentSessionId('reviewer', 'sess-named', 'inv-2');
    assert.equal(
        isCancelPending('sess-named'), false,
        'the new invocation\'s chip must NOT be pre-armed',
    );
    // …and a Yes that lands now (stale from the previous chip's DOM) must
    // make NO call.
    assert.equal(await confirmSubagentCancel('sess-named'), false);
    assert.deepEqual(__getCancelCalls(), []);
    clearAllSubagents();
});

test('Codex P2: chip auto-removal clears an armed confirm (removal path)', () => {
    mock.timers.enable({ apis: ['setTimeout'] });
    try {
        trackSubagentStart('reviewer', 'run', 'inv-9');
        setSubagentSessionId('reviewer', 'sess-rm', 'inv-9');
        // Terminal at t=0 — schedules the chip's auto-removal at t=15000
        // (REMOVE_DELAY_MS in state/subagents.js; not exported — if that
        // constant changes this tick choreography needs updating). The
        // confirm is NOT armed yet, so the terminal-transition clear is a
        // no-op here; this test isolates the REMOVAL clear.
        trackSubagentEnd('reviewer', 'done', 'inv-9', 'sess-rm');

        // Arm at t=12000 (e.g. the session view's Cancel button racing the
        // chip's removal window): the auto-revert would fire at t=16000,
        // AFTER the removal at t=15000 — so a cleared confirm at exactly
        // t=15000 can only have been cleared by the removal path.
        mock.timers.tick(12000);
        requestSubagentCancel('sess-rm');
        assert.equal(isCancelPending('sess-rm'), true);

        mock.timers.tick(3000); // t=15000: removal fires; revert (t=16000) has not
        assert.equal(
            isCancelPending('sess-rm'), false,
            'chip removal must clear the armed confirm for its session',
        );
        assert.deepEqual(__getCancelCalls(), []);
    } finally {
        mock.timers.reset();
    }
});

test('Codex P2: clearAllSubagents (session switch) dismisses any armed confirm', () => {
    requestSubagentCancel('sess-x');
    assert.equal(isCancelPending('sess-x'), true);
    clearAllSubagents();
    assert.equal(isCancelPending('sess-x'), false, 'session switch must dismiss');
    assert.deepEqual(__getCancelCalls(), []);
});

test('an unrelated subagent completing does NOT dismiss a different armed confirm', () => {
    trackSubagentStart('reviewer', 'run', 'inv-a');
    setSubagentSessionId('reviewer', 'sess-a', 'inv-a');
    trackSubagentStart('researcher', 'run', 'inv-b');
    setSubagentSessionId('researcher', 'sess-b', 'inv-b');

    requestSubagentCancel('sess-a');
    trackSubagentEnd('researcher', 'done', 'inv-b', 'sess-b');
    assert.equal(
        isCancelPending('sess-a'), true,
        'B\'s terminal transition must not clear A\'s armed confirm',
    );
    dismissSubagentCancel();
    clearAllSubagents();
});
