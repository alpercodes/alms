// Regression for #1211: the sidebar's cross-session active-run dot must be
// driven by the GLOBAL cross-agent session-activity feed, not a per-agent
// feed. This pins the observable frontend contract of
// `static/ui/hooks/use-agent-events.js` end-to-end against the real hook +
// real `state/queue.js`:
//
//   1. `openAgentEventsStream(...)` connects to `/events/session-activity`
//      (NOT `/agents/{id}/events`). A per-agent URL is exactly the #1211
//      root cause: a run on another agent's session never reaches the
//      active agent's feed, so the dot only lit on the selected session.
//   2. A `session_activity_started` event on that feed — for a session
//      owned by ANY agent — writes `bgRuns[sessionId]`, which is what
//      `hasActiveRun()` in `session-list.js` reads to render `.has-run`.
//   3. `session_activity_ended` clears it.
//
// The Preact re-render of the row itself (hasActiveRun -> `.has-run`
// class) is browser-only and out of scope here; #1216 moved the signal
// reads into the row body for that. This test pins the delivery + state
// half that #1211 was actually about.

import { test, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import url from 'node:url';
import fs from 'node:fs/promises';
import os from 'node:os';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const UI_ROOT = path.resolve(__dirname, '../../static/ui');

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
 * Materialise a temp dir mirroring the `static/ui/` layout the hook
 * imports from, copying the REAL `queue.js` + `stream-health.js` +
 * `use-agent-events.js` so the test exercises production code.
 */
async function installHarness(stub) {
    const dir = await fs.mkdtemp(path.join(os.tmpdir(), 'alms-agent-events-global-'));
    await fs.writeFile(
        path.join(dir, 'package.json'),
        JSON.stringify({ type: 'module' }),
        'utf8',
    );
    const depsShim = `
const _stub = globalThis.__agentEventsGlobalSignalStub__;
export const signal = _stub.signal;
export const html = () => null;
export const h = () => null;
export const computed = () => ({ value: null });
export const effect = () => () => {};
export const batch = (fn) => fn();
export const useSignal = (v) => ({ value: v });
`;
    await fs.writeFile(path.join(dir, 'deps.js'), depsShim, 'utf8');

    const stateDir = path.join(dir, 'state');
    const hooksDir = path.join(dir, 'hooks');
    const apiDir = path.join(dir, 'api');
    await fs.mkdir(stateDir, { recursive: true });
    await fs.mkdir(hooksDir, { recursive: true });
    await fs.mkdir(apiDir, { recursive: true });
    await fs.writeFile(
        path.join(apiDir, 'sessions.js'),
        'export const listSessions = (...args) => globalThis.__agentEventsGlobalApi__.listSessions(...args);\n',
        'utf8',
    );
    await fs.copyFile(path.join(UI_ROOT, 'state', 'queue.js'), path.join(stateDir, 'queue.js'));
    await fs.copyFile(
        path.join(UI_ROOT, 'state', 'stream-health.js'),
        path.join(stateDir, 'stream-health.js'),
    );
    await fs.copyFile(
        path.join(UI_ROOT, 'hooks', 'use-agent-events.js'),
        path.join(hooksDir, 'use-agent-events.js'),
    );

    globalThis.__agentEventsGlobalSignalStub__ = stub;
    return {
        dir,
        hookPath: path.join(hooksDir, 'use-agent-events.js'),
        queuePath: path.join(stateDir, 'queue.js'),
    };
}

/**
 * Stub EventSource that records constructed instances (with their URL) and
 * lets the test dispatch named events into the hook's listeners.
 */
function installEventSourceStub() {
    const created = [];
    class StubEventSource {
        constructor(url) {
            this.url = url;
            this.readyState = 0;
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
        close() { this.readyState = 2; }
        _emitOpen() {
            this.readyState = 1;
            for (const fn of this._listeners.open || []) fn({});
        }
        _emit(type, dataObj, id) {
            const evt = { data: JSON.stringify(dataObj), lastEventId: id != null ? String(id) : '' };
            for (const fn of this._listeners[type] || []) fn(evt);
        }
    }
    StubEventSource.CLOSED = 2;
    StubEventSource.CONNECTING = 0;
    StubEventSource.OPEN = 1;
    globalThis.EventSource = StubEventSource;
    return created;
}

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

beforeEach(() => { active = null; });

afterEach(async () => {
    if (active && active.tempDir) {
        await fs.rm(active.tempDir, { recursive: true, force: true });
    }
    delete globalThis.__agentEventsGlobalSignalStub__;
    delete globalThis.__agentEventsGlobalApi__;
    delete globalThis.EventSource;
    delete globalThis.localStorage;
});

async function loadHook(api = { listSessions: async () => ({ sessions: [] }) }) {
    const stub = makeSignalStub();
    const created = installEventSourceStub();
    installLocalStorageStub();
    globalThis.__agentEventsGlobalApi__ = api;
    const { dir, hookPath, queuePath } = await installHarness(stub);
    // Import the hook (cache-busted so each test gets fresh module state)
    // and the SAME queue.js instance the hook imports (NOT cache-busted,
    // so it dedups to the hook's copy and reflects its bgRuns writes).
    const hookUrl = url.pathToFileURL(hookPath).href + '?cb=' + Date.now() + '-' + Math.random();
    const mod = await import(hookUrl);
    const queue = await import(url.pathToFileURL(queuePath).href);
    return { mod, queue, created, tempDir: dir };
}

test('opens the GLOBAL /events/session-activity feed, not a per-agent feed (#1211)', async () => {
    active = await loadHook();
    const { openAgentEventsStream } = active.mod;

    openAgentEventsStream('agent-1');

    assert.equal(active.created.length, 1, 'one EventSource constructed');
    const u = active.created[0].url;
    assert.ok(
        u.startsWith('/events/session-activity'),
        `expected the global activity feed URL, got: ${u}`,
    );
    assert.ok(
        !u.includes('/agents/'),
        `must NOT open a per-agent feed (that is the #1211 root cause), got: ${u}`,
    );
});

test('session_activity_started for ANY agent writes bgRuns; _ended clears it (#1211)', async () => {
    active = await loadHook();
    const { openAgentEventsStream } = active.mod;
    const { bgRuns } = active.queue;

    openAgentEventsStream('agent-1');
    const es = active.created[0];

    // A run starts on a session owned by a DIFFERENT agent than the active
    // one (e.g. a job in the cross-agent Jobs group). Pre-#1211 this event
    // never reached the sidebar's per-agent feed.
    es._emit('session_activity_started', {
        session_id: 'cross-agent-sess',
        run_id: 'r1',
        has_active_run: true,
    }, 1);
    assert.deepEqual(
        bgRuns.value['cross-agent-sess'],
        { runId: 'r1', finished: false },
        'session_activity_started must seed bgRuns for the (cross-agent) session',
    );

    es._emit('session_activity_ended', {
        session_id: 'cross-agent-sess',
        run_id: 'r1',
        has_active_run: false,
    }, 2);
    assert.equal(
        bgRuns.value['cross-agent-sess'],
        undefined,
        'session_activity_ended must clear the bgRuns entry',
    );
});

test('overlapping runs keep the dot active until the final run ends', async () => {
    active = await loadHook();
    const { openAgentEventsStream } = active.mod;
    const { bgRuns } = active.queue;
    const sessionId = 'shared-dm-session';

    // Boot-time snapshots intentionally have no per-run cardinality.
    bgRuns.value = {
        [sessionId]: { runId: null, finished: false },
    };

    openAgentEventsStream('agent-1');
    const es = active.created[0];
    es._emit('session_activity_started', {
        session_id: sessionId,
        run_id: 'r1',
        has_active_run: true,
    }, 10);
    es._emit('session_activity_started', {
        session_id: sessionId,
        run_id: 'r2',
        has_active_run: true,
    }, 11);
    es._emit('session_activity_ended', {
        session_id: sessionId,
        run_id: 'r1',
        has_active_run: true,
    }, 12);

    assert.deepEqual(
        bgRuns.value[sessionId],
        { runId: null, finished: false },
        'ending r1 must keep the session active while r2 is running',
    );

    es._emit('session_activity_ended', {
        session_id: sessionId,
        run_id: 'r2',
        has_active_run: false,
    }, 13);
    assert.equal(
        bgRuns.value[sessionId],
        undefined,
        'ending the final run must clear the session activity',
    );
});

test('reconnect older than the retained floor reconciles from /sessions and buffers the tail', async () => {
    let resolveSnapshot;
    const snapshot = new Promise(resolve => { resolveSnapshot = resolve; });
    const calls = [];
    active = await loadHook({
        listSessions: (agentId, opts) => {
            calls.push({ agentId, opts });
            return snapshot;
        },
    });
    const { openAgentEventsStream } = active.mod;
    const { bgRuns } = active.queue;

    // Cursor 1 represents a client that fell behind a retained log whose
    // remaining tail now starts much later.
    openAgentEventsStream('agent-1', {
        lastEventId: 1,
        streamEpoch: 'epoch-before-gap',
    });
    const es = active.created[0];
    assert.ok(es.url.includes('last_event_id=1'));
    assert.ok(es.url.includes('stream_epoch=epoch-before-gap'));
    es._emitOpen();
    assert.equal(calls.length, 0, 'ordinary initial open does not duplicate the boot snapshot');
    es._emit('stream_state', {
        stream_epoch: 'epoch-before-gap',
        retained_from: 1000,
        newest: 1001,
        replay_gap: true,
        epoch_mismatch: false,
        requires_reconciliation: true,
    });
    assert.deepEqual(calls, [{
        agentId: null,
        opts: { includeDms: true },
    }]);

    // A retained-tail event arrives while the authoritative request is in
    // flight. It must apply after the snapshot, not be overwritten by it.
    es._emit('session_activity_ended', {
        session_id: 'replayed-before-snapshot',
        run_id: 'r1001',
        has_active_run: false,
    }, 1001);
    es._emit('session_activity_ended', {
        session_id: 'ended-in-tail',
        run_id: 'r1002',
        has_active_run: false,
    }, 1002);
    resolveSnapshot({
        sessions: [
            { id: 'missed-start-before-floor', has_active_run: true },
            { id: 'replayed-before-snapshot', has_active_run: true },
            { id: 'ended-in-tail', has_active_run: true },
        ],
    });
    await new Promise(resolve => setImmediate(resolve));

    assert.ok(
        bgRuns.value['missed-start-before-floor'],
        'the snapshot restores activity whose start fell before the retained floor',
    );
    assert.ok(
        bgRuns.value['replayed-before-snapshot'],
        'replayed events at or below stream_state.newest must not regress the newer snapshot',
    );
    assert.equal(
        bgRuns.value['ended-in-tail'],
        undefined,
        'the buffered retained-tail event must win over the older snapshot',
    );
});

test('a second reconnect queues a separate snapshot instead of widening the first ceiling', async () => {
    const resolvers = [];
    let snapshotCalls = 0;
    active = await loadHook({
        listSessions: () => {
            snapshotCalls++;
            return new Promise(resolve => { resolvers.push(resolve); });
        },
    });
    const { openAgentEventsStream } = active.mod;
    const { bgRuns } = active.queue;

    openAgentEventsStream('agent-1');
    const es = active.created[0];
    es._emit('stream_state', {
        stream_epoch: 'epoch-a',
        newest: 10,
        replay_gap: true,
        epoch_mismatch: false,
        requires_reconciliation: true,
    });
    assert.equal(snapshotCalls, 1);

    // A newer reconnect happens before snapshot 1 resolves. Event 15 is newer
    // than snapshot 1's replay ceiling, so snapshot 1 must apply it. It is
    // older than snapshot 2's ceiling, but that ceiling belongs exclusively
    // to the second authoritative request.
    es._emit('stream_state', {
        stream_epoch: 'epoch-a',
        newest: 20,
        replay_gap: true,
        epoch_mismatch: false,
        requires_reconciliation: true,
    });
    es._emit('session_activity_ended', {
        session_id: 'changed-between-reconnects',
        run_id: 'r15',
        has_active_run: false,
    }, 15);

    resolvers[0]({
        sessions: [{ id: 'changed-between-reconnects', has_active_run: true }],
    });
    await new Promise(resolve => setImmediate(resolve));

    assert.equal(snapshotCalls, 2, 'the newer stream state must queue its own snapshot');
    assert.equal(
        bgRuns.value['changed-between-reconnects'],
        undefined,
        'snapshot 1 must not discard events using snapshot 2\'s wider ceiling',
    );

    resolvers[1]({ sessions: [] });
    await new Promise(resolve => setImmediate(resolve));
});

test('native reconnect after event-log restart clears stale state and accepts reset IDs', async () => {
    let resolveSnapshot;
    const snapshot = new Promise(resolve => { resolveSnapshot = resolve; });
    let snapshotCalls = 0;
    active = await loadHook({
        listSessions: () => {
            snapshotCalls++;
            return snapshot;
        },
    });
    const { openAgentEventsStream } = active.mod;
    const { bgRuns } = active.queue;

    openAgentEventsStream('agent-1');
    const es = active.created[0];
    es._emitOpen();
    assert.equal(snapshotCalls, 0, 'the boot snapshot already covers the initial open');
    es._emit('stream_state', {
        stream_epoch: 'epoch-a',
        replay_gap: false,
        epoch_mismatch: false,
        requires_reconciliation: false,
    });

    es._emit('session_activity_started', {
        session_id: 'stale-before-restart',
        run_id: 'old-run',
        has_active_run: true,
    }, 900);
    assert.ok(bgRuns.value['stale-before-restart']);

    // A second `open` on the same EventSource is the browser's native
    // reconnect. The gateway's event IDs have reset, but reconciliation is
    // independent of the old cursor/epoch.
    es._emit('stream_state', {
        stream_epoch: 'epoch-b',
        replay_gap: false,
        epoch_mismatch: false,
        requires_reconciliation: false,
    });
    assert.equal(snapshotCalls, 1);
    es._emit('session_activity_started', {
        session_id: 'fresh-after-restart',
        run_id: 'new-run',
        has_active_run: true,
    }, 1);
    resolveSnapshot({ sessions: [] });
    await new Promise(resolve => setImmediate(resolve));

    assert.equal(
        bgRuns.value['stale-before-restart'],
        undefined,
        'the authoritative post-restart snapshot clears stale pre-restart activity',
    );
    assert.ok(
        bgRuns.value['fresh-after-restart'],
        'the reset event ID is buffered and applied after reconciliation',
    );
});
