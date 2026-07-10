// Pinned regression for the timer-cancel-on-manual-reconnect contract in
// `static/ui/hooks/use-agent-events.js` (#907 follow-up — Tim's
// Suggestion 1 on PR #1001).
//
// The bug shape:
//   1. EventSource hits `onerror`, retryCount increments, a backoff
//      `setTimeout(reopen, delay)` is scheduled.
//   2. Before it fires, the user (or the `online` event) triggers a
//      manual reconnect — `openAgentEventsStream(...)` is called with
//      a fresh EventSource.
//   3. The pending setTimeout still fires later and calls
//      `openAgentEventsStream` *again*, redundantly closing and
//      re-opening the freshly-healthy stream.
//
// The fix stores the timer id at module scope and clears it at the top
// of `openAgentEventsStream` and `closeAgentEventsStream`. This test
// pins the cancel contract end-to-end against the real hook code.
//
// We focus the regression test on `use-agent-events.js` because it has
// the smallest dependency surface (only `state/queue.js` and
// `state/stream-health.js`). The same fix shape is applied to
// `use-session-stream.js` but that hook pulls in 50+ collaborator
// modules and is not worth standing up a parallel harness for — the
// 15-case `stream-health.test.mjs` already pins the pub/sub contract
// the session-stream hook depends on, and the timer-cancel behaviour
// is byte-for-byte the same shape (`backoffTimer = setTimeout(...)` /
// `clearTimeout(backoffTimer)` at the same call sites).

import { test, beforeEach, afterEach, mock } from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import url from 'node:url';
import fs from 'node:fs/promises';
import os from 'node:os';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const UI_ROOT = path.resolve(__dirname, '../../static/ui');

/**
 * Build a tiny in-memory signal stub mirroring the @preact/signals
 * API surface that the modules under test actually use (`.value`
 * read/write). Same shape as `stream-health.test.mjs`.
 */
function makeSignalStub() {
    function signal(initial) {
        let v = initial;
        return {
            get value() { return v; },
            set value(next) { v = next; },
        };
    }
    return { signal };
}

/**
 * Materialise a temp directory mirroring the `static/ui/` layout the
 * hook imports from:
 *
 *   <tmp>/deps.js                             (signal stub)
 *   <tmp>/state/queue.js                      (real)
 *   <tmp>/state/stream-health.js              (real)
 *   <tmp>/hooks/use-agent-events.js           (real)
 *
 * Returns the path to the hook so the test can dynamic-import it.
 */
async function installAgentEventsHarness(stub) {
    const dir = await fs.mkdtemp(path.join(os.tmpdir(), 'alms-agent-events-timer-'));
    await fs.writeFile(
        path.join(dir, 'package.json'),
        JSON.stringify({ type: 'module' }),
        'utf8',
    );

    // deps.js shim — only the stream-health module reads `signal`,
    // queue.js does too, but neither hook touches the other named
    // exports. Stub the rest as no-ops so a future helper landing in
    // either module doesn't break the harness.
    const depsShim = `
const _stub = globalThis.__agentEventsTimerSignalStub__;
export const signal = _stub.signal;
export const html = () => null;
export const h = () => null;
export const computed = () => ({ value: null });
export const effect = () => () => {};
export const batch = (fn) => fn();
export const useSignal = (v) => ({ value: v });
`;
    await fs.writeFile(path.join(dir, 'deps.js'), depsShim, 'utf8');

    // Copy real source files preserving relative-import structure.
    const stateDir = path.join(dir, 'state');
    const hooksDir = path.join(dir, 'hooks');
    const apiDir = path.join(dir, 'api');
    await fs.mkdir(stateDir, { recursive: true });
    await fs.mkdir(hooksDir, { recursive: true });
    await fs.mkdir(apiDir, { recursive: true });
    await fs.writeFile(
        path.join(apiDir, 'sessions.js'),
        'export const listSessions = async () => ({ sessions: [] });\n',
        'utf8',
    );
    await fs.copyFile(
        path.join(UI_ROOT, 'state', 'queue.js'),
        path.join(stateDir, 'queue.js'),
    );
    await fs.copyFile(
        path.join(UI_ROOT, 'state', 'stream-health.js'),
        path.join(stateDir, 'stream-health.js'),
    );
    await fs.copyFile(
        path.join(UI_ROOT, 'hooks', 'use-agent-events.js'),
        path.join(hooksDir, 'use-agent-events.js'),
    );

    globalThis.__agentEventsTimerSignalStub__ = stub;
    return { dir, hookPath: path.join(hooksDir, 'use-agent-events.js') };
}

/**
 * Stub EventSource — captures the most recently constructed instance so
 * the test can simulate the `onerror` -> CLOSED transition without a
 * real network. Each construction increments `constructed` so the test
 * can pin "no second EventSource was constructed" to detect a redundant
 * reopen.
 */
function installEventSourceStub() {
    const created = [];
    class StubEventSource {
        constructor(url) {
            this.url = url;
            this.readyState = 0; // CONNECTING
            this._listeners = {};
            this.onerror = null;
            created.push(this);
        }
        addEventListener(type, fn) {
            (this._listeners[type] = this._listeners[type] || []).push(fn);
        }
        removeEventListener(type, fn) {
            const arr = this._listeners[type] || [];
            this._listeners[type] = arr.filter(f => f !== fn);
        }
        close() {
            this.readyState = 2; // CLOSED
        }
        _fireError() {
            // Real EventSource sets readyState to CLOSED before
            // firing onerror when the server abandons the stream.
            this.readyState = 2;
            if (this.onerror) this.onerror({});
        }
    }
    StubEventSource.CLOSED = 2;
    StubEventSource.CONNECTING = 0;
    StubEventSource.OPEN = 1;
    globalThis.EventSource = StubEventSource;
    return created;
}

/**
 * Local-storage stub — the hook reads `alms_auth_token`. Returns null
 * by default; tests that care about the URL can preseed the map.
 */
function installLocalStorageStub() {
    const map = new Map();
    globalThis.localStorage = {
        getItem(key) { return map.has(key) ? map.get(key) : null; },
        setItem(key, val) { map.set(key, String(val)); },
        removeItem(key) { map.delete(key); },
        clear() { map.clear(); },
    };
    return map;
}

let active;

beforeEach(() => {
    active = null;
});

afterEach(async () => {
    if (active && active.tempDir) {
        await fs.rm(active.tempDir, { recursive: true, force: true });
    }
    delete globalThis.__agentEventsTimerSignalStub__;
    delete globalThis.EventSource;
    delete globalThis.localStorage;
    if (mock.timers && mock.timers.reset) {
        mock.timers.reset();
    }
});

async function loadHook() {
    const stub = makeSignalStub();
    const created = installEventSourceStub();
    installLocalStorageStub();
    const { dir, hookPath } = await installAgentEventsHarness(stub);
    const fileUrl = url.pathToFileURL(hookPath).href
        + '?cb=' + Date.now() + '-' + Math.random();
    const mod = await import(fileUrl);
    return { mod, created, tempDir: dir };
}

// ---------------------------------------------------------------------
// Timer-cancel contract — Suggestion 1 on PR #1001
// ---------------------------------------------------------------------

test('manual reconnect mid-backoff cancels the pending reopen timer', async () => {
    // Reproduces the wifi-blip race Tim called out: stream errors,
    // backoff timer fires `reopen` 5 s later — but a fresh
    // openAgentEventsStream() call (banner click / online event) hits
    // first. The pending timer must be cancelled so it does not
    // double-open the now-healthy stream.
    mock.timers.enable({ apis: ['setTimeout'] });
    active = await loadHook();
    const { openAgentEventsStream } = active.mod;

    // Initial open + simulate server abandoning the stream.
    openAgentEventsStream('agent-1');
    assert.equal(active.created.length, 1, 'first EventSource constructed');
    active.created[0]._fireError();
    // The hook scheduled a backoff setTimeout. Without intervention,
    // ticking forward >2 s would call openAgentEventsStream again.
    // First, manually reconnect by calling open again — this should
    // cancel the pending timer.
    openAgentEventsStream('agent-1');
    assert.equal(active.created.length, 2,
        'manual reconnect constructs a fresh EventSource');

    // Fast-forward past the original backoff window. If the cancel
    // worked, no third EventSource is constructed. If it didn't, the
    // pending setTimeout fires and we'd see a third construction.
    mock.timers.tick(60000);
    assert.equal(active.created.length, 2,
        'pending backoff timer must be cancelled on manual reconnect '
        + '— a third EventSource construction here means the cancel '
        + 'did not run');
});

test('closeAgentEventsStream cancels the pending backoff timer', async () => {
    // The teardown path must also cancel — otherwise switching agents
    // mid-backoff would queue a reopen against the stale agentId.
    // The stale-agent guard inside the timer callback already prevents
    // a wrong-agent reopen, but the timer should not fire at all
    // after explicit teardown.
    mock.timers.enable({ apis: ['setTimeout'] });
    active = await loadHook();
    const { openAgentEventsStream, closeAgentEventsStream } = active.mod;

    openAgentEventsStream('agent-1');
    assert.equal(active.created.length, 1);
    active.created[0]._fireError();

    closeAgentEventsStream();

    // Tick the clock — if the cancel worked, no reopen happens.
    // (We can't directly observe whether the setTimeout callback ran,
    // because the activeAgentId guard inside it would short-circuit
    // anyway. But we can pin the public side-effect: no new
    // EventSource is constructed.)
    mock.timers.tick(60000);
    assert.equal(active.created.length, 1,
        'no new EventSource constructed after explicit close + tick — '
        + 'pending backoff timer must be cancelled');
});

test('a timer that fires before any reconnect does still trigger a reopen', async () => {
    // Negative control — proves the cancel test above is meaningful by
    // showing the timer *does* normally fire and reopen when nothing
    // cancels it. Without this, a broken cancel implementation that
    // null'd the timer slot but never scheduled it could pass the
    // first test by accident.
    mock.timers.enable({ apis: ['setTimeout'] });
    active = await loadHook();
    const { openAgentEventsStream } = active.mod;

    openAgentEventsStream('agent-1');
    assert.equal(active.created.length, 1);
    active.created[0]._fireError();

    // First retry uses delay = 2000 * 2^0 = 2000 ms. Tick just past it.
    mock.timers.tick(2500);
    assert.equal(active.created.length, 2,
        'backoff timer must fire and reopen when nothing cancelled it');
});
