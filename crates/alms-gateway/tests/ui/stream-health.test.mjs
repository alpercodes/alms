// JS-level regression tests for `static/ui/state/stream-health.js`.
//
// Pinned regression target:
//   - issue #907 — when both SSE streams (use-session-stream and
//     use-agent-events) exhaust their retry budgets, the only output
//     was a console.error. The fix introduces a global `streamDead`
//     signal, a click-to-reconnect plumbing path, and a
//     `window.addEventListener('online', ...)` re-arm. The pure-
//     function pub/sub surface is in `state/stream-health.js`; this
//     file pins its contract so a future refactor can't silently
//     regress the banner-up / banner-down semantics or break the
//     reconnect fan-out.
//
// The module under test depends on `@preact/signals` via `../deps.js`.
// We can't pull that into Node easily, so the test stubs the `signal`
// factory with a tiny in-memory implementation that mirrors the parts
// of the API the module actually uses (`signal(initial)` returning an
// object with a `.value` getter/setter that records writes).
//
// The stub is wired up by overriding the loader for `../deps.js` with
// a sibling test file that re-exports the stub. This is the same
// pattern as `composer-storage.test.mjs` — pure JS test, no Preact
// runtime needed.

import { test, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import url from 'node:url';
import fs from 'node:fs/promises';
import os from 'node:os';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const STREAM_HEALTH_PATH = path.resolve(
    __dirname,
    '../../static/ui/state/stream-health.js'
);

/**
 * Build a tiny in-memory signal stub. The module under test only
 * uses `.value` reads and writes, so we don't need the full
 * `@preact/signals` API surface — just an object that behaves like
 * a writable property store.
 */
function makeSignalStub() {
    const writes = [];
    function signal(initial) {
        let v = initial;
        return {
            get value() { return v; },
            set value(next) {
                v = next;
                writes.push(next);
            },
        };
    }
    return { signal, writes };
}

/**
 * Materialise a temp `deps.js` shim that exports our signal stub
 * plus no-op stubs for any other named exports the module under
 * test transitively touches. The stream-health module currently
 * only imports `signal`, but adding `html`, `computed`, etc. as
 * no-ops future-proofs the test against a future helper landing
 * in stream-health that pulls in another deps export.
 *
 * Returns the temp dir path so the caller can clean up.
 */
async function installDepsShim(stub) {
    const dir = await fs.mkdtemp(path.join(os.tmpdir(), 'alms-stream-health-test-'));
    // Mark the temp tree as ESM so Node doesn't emit a
    // MODULE_TYPELESS_PACKAGE_JSON warning on every dynamic import.
    await fs.writeFile(
        path.join(dir, 'package.json'),
        JSON.stringify({ type: 'module' }),
        'utf8',
    );
    const depsPath = path.join(dir, 'deps.js');
    // The shim writes the shared stub onto a global the module reads
    // at import time. Each loadStreamHealth() call installs a fresh
    // stub on globalThis before the dynamic import fires.
    const shim = `
const _stub = globalThis.__streamHealthSignalStub__;
export const signal = _stub.signal;
export const html = () => null;
export const h = () => null;
export const computed = () => ({ value: null });
export const effect = () => () => {};
export const batch = (fn) => fn();
export const useSignal = (v) => ({ value: v });
`;
    await fs.writeFile(depsPath, shim, 'utf8');

    // Copy the module-under-test next to the shim so its relative
    // import `../deps.js` resolves to our shim. The module lives at
    // `static/ui/state/stream-health.js`, importing `../deps.js`
    // (i.e. `static/ui/deps.js`). Mirror that layout in the temp
    // dir: temp/ui/state/stream-health.js + temp/ui/deps.js.
    const stateDir = path.join(dir, 'state');
    await fs.mkdir(stateDir, { recursive: true });
    const targetPath = path.join(stateDir, 'stream-health.js');
    const src = await fs.readFile(STREAM_HEALTH_PATH, 'utf8');
    await fs.writeFile(targetPath, src, 'utf8');

    // Move the shim up one level so `../deps.js` from
    // `state/stream-health.js` lands on it.
    await fs.rename(depsPath, path.join(dir, 'deps.js'));

    globalThis.__streamHealthSignalStub__ = stub;
    return { dir, modulePath: targetPath };
}

/**
 * Dynamically import a fresh copy of stream-health.js wired to a
 * fresh signal stub. Each test gets its own evaluation so module-
 * level state (deadSources set, registered callbacks) doesn't leak
 * across tests.
 */
async function loadStreamHealth() {
    const stub = makeSignalStub();
    const { dir, modulePath } = await installDepsShim(stub);
    const fileUrl = url.pathToFileURL(modulePath).href
        + '?cb=' + Date.now() + '-' + Math.random();
    const mod = await import(fileUrl);
    return { mod, stub, tempDir: dir };
}

let active;

beforeEach(async () => {
    active = null;
});

afterEach(async () => {
    if (active && active.tempDir) {
        await fs.rm(active.tempDir, { recursive: true, force: true });
    }
    delete globalThis.__streamHealthSignalStub__;
    delete globalThis.window;
});

// ---------------------------------------------------------------------
// streamDead signal transitions
// ---------------------------------------------------------------------

test('streamDead is false on first load', async () => {
    active = await loadStreamHealth();
    assert.equal(active.mod.streamDead.value, false);
    assert.deepEqual(active.mod._deadSourcesSnapshot(), []);
});

test('markStreamDead flips streamDead to true on first call', async () => {
    active = await loadStreamHealth();
    active.mod.markStreamDead('session');
    assert.equal(active.mod.streamDead.value, true);
    assert.deepEqual(active.mod._deadSourcesSnapshot(), ['session']);
});

test('markStreamDead is idempotent for the same source', async () => {
    // Hooks fire onerror multiple times in some browsers; the second
    // mark must not re-trigger a signal write (which would re-render
    // the banner pointlessly).
    active = await loadStreamHealth();
    active.mod.markStreamDead('session');
    const writesAfterFirst = active.stub.writes.length;
    active.mod.markStreamDead('session');
    assert.equal(active.stub.writes.length, writesAfterFirst,
        'second mark for the same source must not write the signal');
    assert.equal(active.mod.streamDead.value, true);
});

test('marking two distinct sources keeps streamDead true; clearing one keeps it true', async () => {
    // The "any stream dead → banner up" semantic from the issue body.
    // The banner must stay visible until both sources have reported
    // healthy, otherwise the user could see an empty banner state
    // while one stream is still dead.
    active = await loadStreamHealth();
    active.mod.markStreamDead('session');
    active.mod.markStreamDead('agent-events');
    assert.equal(active.mod.streamDead.value, true);
    assert.deepEqual(
        active.mod._deadSourcesSnapshot().sort(),
        ['agent-events', 'session'],
    );

    active.mod.clearStreamDead('session');
    assert.equal(active.mod.streamDead.value, true,
        'banner stays up while agent-events is still dead');
    assert.deepEqual(active.mod._deadSourcesSnapshot(), ['agent-events']);

    active.mod.clearStreamDead('agent-events');
    assert.equal(active.mod.streamDead.value, false,
        'banner comes down only when every source has cleared');
    assert.deepEqual(active.mod._deadSourcesSnapshot(), []);
});

test('clearStreamDead is idempotent and a no-op when source was never marked', async () => {
    // Hooks call clearStreamDead on every successful open. If the
    // source was never marked dead, the call must not write the
    // signal — that would burn a re-render on every reconnect.
    active = await loadStreamHealth();
    const writesBefore = active.stub.writes.length;
    active.mod.clearStreamDead('session');
    assert.equal(active.stub.writes.length, writesBefore,
        'clearing an undead source must not write the signal');
    assert.equal(active.mod.streamDead.value, false);

    // After mark + clear, a second clear is also a no-op.
    active.mod.markStreamDead('session');
    active.mod.clearStreamDead('session');
    const writesAfterCleanup = active.stub.writes.length;
    active.mod.clearStreamDead('session');
    assert.equal(active.stub.writes.length, writesAfterCleanup);
    assert.equal(active.mod.streamDead.value, false);
});

test('marking with falsy source is a no-op (defensive)', async () => {
    // Defense-in-depth: if a hook accidentally passed undefined / ''
    // we don't want it polluting the dead set with unmatchable keys.
    active = await loadStreamHealth();
    active.mod.markStreamDead(null);
    active.mod.markStreamDead(undefined);
    active.mod.markStreamDead('');
    assert.equal(active.mod.streamDead.value, false);
    assert.deepEqual(active.mod._deadSourcesSnapshot(), []);
});

// ---------------------------------------------------------------------
// Reconnect fan-out — issue #907 acceptance criterion
// ---------------------------------------------------------------------

test('reconnectAllStreams invokes both registered callbacks once', async () => {
    active = await loadStreamHealth();
    let sessionCalls = 0;
    let agentCalls = 0;
    active.mod.registerSessionReconnect(() => { sessionCalls += 1; });
    active.mod.registerAgentEventsReconnect(() => { agentCalls += 1; });
    active.mod.reconnectAllStreams();
    assert.equal(sessionCalls, 1);
    assert.equal(agentCalls, 1);
});

test('reconnectAllStreams with no callbacks registered is a no-op', async () => {
    // Test-side / early-boot path: no hook has registered yet. The
    // banner click must not throw.
    active = await loadStreamHealth();
    assert.doesNotThrow(() => active.mod.reconnectAllStreams());
});

test('register* replaces the previous callback (last-write-wins)', async () => {
    // Hooks register at module load. HMR / test re-imports must not
    // stack callbacks — only the most recent one fires.
    active = await loadStreamHealth();
    let firstCalls = 0;
    let secondCalls = 0;
    active.mod.registerSessionReconnect(() => { firstCalls += 1; });
    active.mod.registerSessionReconnect(() => { secondCalls += 1; });
    active.mod.reconnectAllStreams();
    assert.equal(firstCalls, 0, 'old callback must not fire after re-register');
    assert.equal(secondCalls, 1);
});

test('reconnectAllStreams swallows callback errors and still fires the other side', async () => {
    // If one hook's reconnect throws, the other must still be
    // re-armed — otherwise a single buggy callback breaks the whole
    // banner click path.
    active = await loadStreamHealth();
    let agentCalls = 0;
    active.mod.registerSessionReconnect(() => { throw new Error('boom'); });
    active.mod.registerAgentEventsReconnect(() => { agentCalls += 1; });
    const origError = console.error;
    let errorLogged = false;
    console.error = () => { errorLogged = true; };
    try {
        assert.doesNotThrow(() => active.mod.reconnectAllStreams());
    } finally {
        console.error = origError;
    }
    assert.equal(agentCalls, 1, 'agent-events reconnect must still fire');
    assert.equal(errorLogged, true, 'session error path must log to console');
});

test('register* with non-function clears the slot (defensive)', async () => {
    // Pin defensive handling so a future refactor can't pass a stale
    // null and have the next reconnectAllStreams() throw.
    active = await loadStreamHealth();
    active.mod.registerSessionReconnect(null);
    active.mod.registerAgentEventsReconnect(undefined);
    assert.doesNotThrow(() => active.mod.reconnectAllStreams());
});

test('triggerOnlineReconnect behaves identically to reconnectAllStreams', async () => {
    // The `online` event handler is exposed as a separate name so
    // future code could differentiate it, but today they fire the
    // same fan-out. Pin the equivalence so a refactor that moves
    // logic into one of them updates both.
    active = await loadStreamHealth();
    let sessionCalls = 0;
    let agentCalls = 0;
    active.mod.registerSessionReconnect(() => { sessionCalls += 1; });
    active.mod.registerAgentEventsReconnect(() => { agentCalls += 1; });
    active.mod.triggerOnlineReconnect();
    assert.equal(sessionCalls, 1);
    assert.equal(agentCalls, 1);
});

// ---------------------------------------------------------------------
// installOnlineReconnectListener — browser online-event re-arm
// ---------------------------------------------------------------------

test('installOnlineReconnectListener wires window.online to reconnect', async () => {
    // Stand up a stub `window` with addEventListener / removeEventListener.
    const listeners = new Map();
    globalThis.window = {
        addEventListener(type, fn) {
            const arr = listeners.get(type) || [];
            arr.push(fn);
            listeners.set(type, arr);
        },
        removeEventListener(type, fn) {
            const arr = listeners.get(type) || [];
            listeners.set(type, arr.filter(f => f !== fn));
        },
    };

    active = await loadStreamHealth();
    let sessionCalls = 0;
    let agentCalls = 0;
    active.mod.registerSessionReconnect(() => { sessionCalls += 1; });
    active.mod.registerAgentEventsReconnect(() => { agentCalls += 1; });

    const teardown = active.mod.installOnlineReconnectListener();
    assert.equal(typeof teardown, 'function');
    assert.equal(listeners.get('online').length, 1,
        'online listener must be registered');

    // Simulate the browser firing the event.
    listeners.get('online')[0]();
    assert.equal(sessionCalls, 1, 'session reconnect fires on online event');
    assert.equal(agentCalls, 1, 'agent-events reconnect fires on online event');

    // Teardown removes the listener.
    teardown();
    assert.equal(listeners.get('online').length, 0,
        'teardown must remove the online listener');
});

test('installOnlineReconnectListener is a no-op when window is undefined', async () => {
    // Server-side / test-side path with no DOM.
    delete globalThis.window;
    active = await loadStreamHealth();
    const teardown = active.mod.installOnlineReconnectListener();
    assert.equal(typeof teardown, 'function');
    assert.doesNotThrow(() => teardown());
});

// ---------------------------------------------------------------------
// End-to-end shape — banner up → click → banner down
// ---------------------------------------------------------------------

test('full reconnect cycle: mark dead, register reconnect, click clears banner', async () => {
    // Pin the integration of the three pieces (mark, register,
    // reconnect-callback-clearing) so a refactor can't break the
    // happy path the user actually experiences.
    active = await loadStreamHealth();
    active.mod.registerSessionReconnect(() => {
        // Simulating a successful open: the hook would call clearStreamDead.
        active.mod.clearStreamDead('session');
    });
    active.mod.registerAgentEventsReconnect(() => {
        active.mod.clearStreamDead('agent-events');
    });

    // Both streams die.
    active.mod.markStreamDead('session');
    active.mod.markStreamDead('agent-events');
    assert.equal(active.mod.streamDead.value, true);

    // User clicks the banner → reconnectAllStreams fires both
    // callbacks → both clearStreamDead calls fire → banner down.
    active.mod.reconnectAllStreams();
    assert.equal(active.mod.streamDead.value, false);
    assert.deepEqual(active.mod._deadSourcesSnapshot(), []);
});
