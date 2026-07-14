// JS-level regression tests for `static/ui/state/composer-storage.js`.
//
// Pinned regression target:
//   - issue #981 — composer draft is lost when the operator switches
//     sessions and switches back. The fix persists drafts per-session
//     in `localStorage`; this file pins the round-trip + clear-on-send
//     contract so a future refactor can't silently regress to the
//     "draft evaporates on view unmount" shape.
//
//   - issue #975 — queued messages (sent while an agent is mid-run) are
//     lost when the operator switches sessions. Same architectural fix:
//     queue persisted per-session in `localStorage` and re-hydrated on
//     mount. This file pins the queue round-trip, the per-session
//     scoping (no bleed across sessions), the deletion sweep, and the
//     queue-bound caps.
//
// The module under test has no imports — it touches `localStorage`
// directly through the global / `window.localStorage` lookup. Tests
// install an in-memory stub via `globalThis.window`, then load the
// module via dynamic `import()` (see history.test.mjs for the same
// pattern). Each test resets the stub so they don't leak state.

import { test, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import url from 'node:url';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const COMPOSER_STORAGE_PATH = path.resolve(
    __dirname,
    '../../static/ui/state/composer-storage.js'
);

/**
 * Minimal in-memory localStorage shim. Mirrors the parts of the Web
 * Storage API the module under test actually uses (getItem, setItem,
 * removeItem). Unrelated APIs are intentionally omitted so a future
 * call to `localStorage.clear()` or `.key()` would throw and surface
 * as a test failure rather than silently work against the stub.
 */
function makeStubStorage() {
    const map = new Map();
    return {
        getItem(key) {
            return map.has(key) ? map.get(key) : null;
        },
        setItem(key, value) {
            map.set(key, String(value));
        },
        removeItem(key) {
            map.delete(key);
        },
        // Test helper — not part of the Web Storage API.
        _entries() {
            return Array.from(map.entries());
        },
        _size() {
            return map.size;
        },
    };
}

/**
 * Install the storage stub on globalThis.window so the module-under-
 * test's `typeof window !== 'undefined' && window.localStorage` guard
 * resolves to the stub.
 */
function installStorageStub() {
    const storage = makeStubStorage();
    globalThis.window = { localStorage: storage };
    // Some Node test runners alias `localStorage` directly; not strictly
    // required for the module under test, but matches a browser-like
    // global so any future change that reads `localStorage` directly
    // also lands on the stub.
    globalThis.localStorage = storage;
    return storage;
}

function uninstallStorageStub() {
    delete globalThis.window;
    delete globalThis.localStorage;
}

/**
 * Dynamically import a fresh copy of the module under test. Node caches
 * ESM imports by URL, so we tack on a cache-busting query parameter to
 * guarantee each test gets a clean module evaluation against its own
 * storage stub.
 */
async function loadComposerStorage() {
    const fileUrl = url.pathToFileURL(COMPOSER_STORAGE_PATH).href
        + '?cb=' + Date.now() + '-' + Math.random();
    return await import(fileUrl);
}

let storage;

beforeEach(() => {
    storage = installStorageStub();
});

afterEach(() => {
    uninstallStorageStub();
});

// ---------------------------------------------------------------------
// Draft round-trip — issue #981
// ---------------------------------------------------------------------

test('draft round-trip: save then load returns the same text', async () => {
    const m = await loadComposerStorage();
    m.saveDraft('session-A', 'half-typed message');
    assert.equal(m.loadDraft('session-A'), 'half-typed message');
});

test('draft load returns empty string when no draft is stored', async () => {
    const m = await loadComposerStorage();
    assert.equal(m.loadDraft('session-A'), '');
});

test('draft is scoped per session — no bleed across session ids', async () => {
    const m = await loadComposerStorage();
    m.saveDraft('session-A', 'A-text');
    m.saveDraft('session-B', 'B-text');
    assert.equal(m.loadDraft('session-A'), 'A-text');
    assert.equal(m.loadDraft('session-B'), 'B-text');
    assert.equal(m.loadDraft('session-C'), '');
});

test('draft is cleared when sendMessage clears it', async () => {
    const m = await loadComposerStorage();
    m.saveDraft('session-A', 'about to send');
    m.clearDraft('session-A');
    assert.equal(m.loadDraft('session-A'), '');
});

test('saving an empty string removes the draft entry', async () => {
    // Operator types text then deletes it back to empty — saveDraft
    // should remove the storage entry rather than persist a stale "".
    const m = await loadComposerStorage();
    m.saveDraft('session-A', 'hi');
    assert.equal(storage._size(), 1);
    m.saveDraft('session-A', '');
    assert.equal(storage._size(), 0);
    assert.equal(m.loadDraft('session-A'), '');
});

test('draft survives module reload (round-trip across import)', async () => {
    // Two separate module evaluations against the same storage stub
    // mimics a page reload (state persists but the JS module is re-
    // evaluated). The stub is reused across the loads, only the module
    // is re-imported.
    const first = await loadComposerStorage();
    first.saveDraft('session-A', 'persisted across reload');

    const second = await loadComposerStorage();
    assert.equal(second.loadDraft('session-A'), 'persisted across reload');
});

test('draft is stored verbatim when storage accepts the write', async () => {
    // Tim's review (#990): the MAX_DRAFT_LEN cap is a defensive ceiling,
    // not the actual browser limit. Modern browsers happily accept 64 KB
    // writes against a 5 MB origin quota, so a 100 KB draft should
    // round-trip fully when storage doesn't push back.
    const m = await loadComposerStorage();
    const huge = 'x'.repeat(100 * 1024);
    m.saveDraft('session-A', huge);
    assert.equal(m.loadDraft('session-A'), huge);
});

test('saveDraft warns and does not throw when under-cap text fails origin-wide quota (Tim re-review #2)', async () => {
    // Tim's re-review on PR #990, finding #2: if the input text length
    // is already <= MAX_DRAFT_LEN but storage still throws (origin-wide
    // quota pressure unrelated to this single key's size), the retry
    // doesn't actually shrink anything (`clipped === text`), throws
    // again, and the inner catch used to swallow it silently — no
    // console.warn, no signal at all. The fix surfaces a warn on the
    // inner-catch path so a "draft not persisting" failure is observable
    // in the operator's console.
    //
    // Contract: storage throws on an under-cap draft → console.warn
    // fires, draft is not persisted, no exception bubbles to the caller.
    const m = await loadComposerStorage();
    const MAX_DRAFT_LEN = 64 * 1024;
    const underCap = 'normal-sized draft';
    assert.ok(underCap.length < MAX_DRAFT_LEN, 'fixture must be under cap');

    let warnCount = 0;
    let lastWarn = null;
    const origWarn = console.warn;
    console.warn = (...args) => {
        warnCount += 1;
        lastWarn = args;
    };

    // Reject every write — simulates origin-wide quota pressure where
    // even a tiny key can't be written. Both the full-write attempt and
    // the truncated-retry land here; the second hit triggers the inner
    // catch (the path Tim flagged).
    storage.setItem = () => { throw new Error('QuotaExceededError (origin-wide)'); };

    try {
        assert.doesNotThrow(
            () => m.saveDraft('session-A', underCap),
            'saveDraft must not bubble storage errors to the caller',
        );
    } finally {
        console.warn = origWarn;
    }

    assert.equal(warnCount, 1, 'inner-catch path must surface exactly one warn');
    assert.ok(lastWarn, 'console.warn must be called');
    assert.match(
        String(lastWarn[0]),
        /not persisted/,
        'warn message must signal that the draft is not being persisted',
    );

    // Draft was never written — restore-on-next-mount returns empty.
    // (We have to remove our throwing setItem patch first, otherwise
    // loadDraft's getItem read still works against the original map but
    // the entry was never written so we get null either way.)
    assert.equal(m.loadDraft('session-A'), '', 'draft must not be persisted');
});

test('draft falls back to truncation on quota-exceeded with a console.warn', async () => {
    // Pin the truncate-and-retry path that fires only when the browser
    // genuinely refuses the full write. We simulate quota pressure by
    // throwing on any write longer than MAX_DRAFT_LEN, then accepting
    // the retried (truncated) write. The fallback emits console.warn so
    // the operator-facing surface is not silent. (Tim's review on #990.)
    const m = await loadComposerStorage();
    const MAX_DRAFT_LEN = 64 * 1024;
    const huge = 'x'.repeat(100 * 1024);

    let warned = null;
    const origWarn = console.warn;
    console.warn = (...args) => { warned = args; };

    const realSetItem = storage.setItem.bind(storage);
    storage.setItem = (key, value) => {
        if (typeof value === 'string' && value.length > MAX_DRAFT_LEN) {
            throw new Error('QuotaExceededError');
        }
        return realSetItem(key, value);
    };

    try {
        m.saveDraft('session-A', huge);
    } finally {
        console.warn = origWarn;
    }

    const loaded = m.loadDraft('session-A');
    assert.equal(loaded.length, MAX_DRAFT_LEN, 'fallback should truncate to MAX_DRAFT_LEN');
    assert.ok(huge.startsWith(loaded), 'truncated draft should be a prefix of the input');
    assert.ok(warned, 'truncation should emit a console.warn');
    assert.match(String(warned[0]), /truncated/, 'warn message should mention truncation');
});

test('loadDraft returns "" when sessionId is missing or null', async () => {
    const m = await loadComposerStorage();
    assert.equal(m.loadDraft(null), '');
    assert.equal(m.loadDraft(undefined), '');
    assert.equal(m.loadDraft(''), '');
});

// ---------------------------------------------------------------------
// Queue round-trip — issue #975
// ---------------------------------------------------------------------

test('queue round-trip: save then load returns the same items', async () => {
    const m = await loadComposerStorage();
    const q = [{ text: 'first' }, { text: 'second' }];
    m.saveQueue('session-A', q);
    assert.deepEqual(m.loadQueue('session-A'), q);
});

test('queue load returns [] when no queue is stored', async () => {
    const m = await loadComposerStorage();
    assert.deepEqual(m.loadQueue('session-A'), []);
});

test('queue is scoped per session — no bleed across session ids', async () => {
    const m = await loadComposerStorage();
    m.saveQueue('session-A', [{ text: 'A1' }, { text: 'A2' }]);
    m.saveQueue('session-B', [{ text: 'B1' }]);
    assert.deepEqual(m.loadQueue('session-A'), [{ text: 'A1' }, { text: 'A2' }]);
    assert.deepEqual(m.loadQueue('session-B'), [{ text: 'B1' }]);
    assert.deepEqual(m.loadQueue('session-C'), []);
});

test('saving an empty queue removes the storage entry', async () => {
    // Once the queue drains during a run-finish, persisting `[]` should
    // clear the key rather than leave behind an empty array blob.
    const m = await loadComposerStorage();
    m.saveQueue('session-A', [{ text: 'pending' }]);
    assert.equal(storage._size(), 1);
    m.saveQueue('session-A', []);
    assert.equal(storage._size(), 0);
});

test('queue survives module reload (round-trip across import)', async () => {
    const first = await loadComposerStorage();
    first.saveQueue('session-A', [{ text: 'queued before reload' }]);

    const second = await loadComposerStorage();
    assert.deepEqual(
        second.loadQueue('session-A'),
        [{ text: 'queued before reload' }]
    );
});

test('queue load is robust against a corrupt JSON blob', async () => {
    // Direct write of a non-JSON string simulates a partial write or
    // tampering. The loader must return [] rather than throwing — a
    // corrupt entry shouldn't crash the composer mount path.
    storage.setItem('alms.composer.queue.session-A', 'not-json{');
    const m = await loadComposerStorage();
    assert.deepEqual(m.loadQueue('session-A'), []);
});

test('queue load filters malformed entries', async () => {
    // Defensive parsing — a stored array containing non-{text} entries
    // should still produce a clean queue with only the valid items.
    storage.setItem('alms.composer.queue.session-A', JSON.stringify([
        { text: 'good' },
        { notText: 'bad' },
        null,
        { text: 42 },          // wrong type
        { text: 'another good' },
    ]));
    const m = await loadComposerStorage();
    assert.deepEqual(
        m.loadQueue('session-A'),
        [{ text: 'good' }, { text: 'another good' }]
    );
});

test('queue cap drops overflow tail items', async () => {
    const m = await loadComposerStorage();
    // Build a queue larger than MAX_QUEUE_ITEMS (50). Save, load, and
    // check that the persisted view is bounded.
    const q = [];
    for (let i = 0; i < 100; i++) q.push({ text: 'msg-' + i });
    m.saveQueue('session-A', q);
    const loaded = m.loadQueue('session-A');
    assert.ok(loaded.length <= 50, 'queue should be capped to MAX_QUEUE_ITEMS');
    // The retained items should be the head (the operator's first sends)
    // rather than the tail — the drain order in use-session-stream.js is
    // FIFO so dropping the tail preserves the order the user expects.
    assert.equal(loaded[0].text, 'msg-0');
});

test('byte cap removes the storage entry when a single item exceeds MAX_QUEUE_BYTES', async () => {
    // Tim's review (#990): pin the silent-drop-on-write branch where a
    // lone item is so large that JSON serialisation of `[{text}]` exceeds
    // MAX_QUEUE_BYTES. The byte-trim loop slices down to length 0, then
    // saveQueue() removes the storage key entirely. The in-memory queue
    // is unaffected — the live signal stays the source of truth at
    // runtime, persistence just degrades.
    const m = await loadComposerStorage();
    const giant = { text: 'x'.repeat(300 * 1024) }; // 300 KB > 256 KB cap
    m.saveQueue('session-A', [giant]);
    // Storage entry removed (no key written) — load returns [].
    assert.deepEqual(m.loadQueue('session-A'), []);
    assert.equal(storage._size(), 0);
});

test('byte cap drops oversize tail items but retains the head', async () => {
    // Tim's review (#990): pin the byte-trim loop's tail-drop semantics.
    // Two normal items + one giant tail item should leave the two normal
    // items intact in storage; the giant gets sliced off until the JSON
    // string fits under MAX_QUEUE_BYTES.
    const m = await loadComposerStorage();
    const queue = [
        { text: 'normal-1' },
        { text: 'normal-2' },
        { text: 'g'.repeat(300 * 1024) }, // alone exceeds the byte cap
    ];
    m.saveQueue('session-A', queue);
    const loaded = m.loadQueue('session-A');
    assert.deepEqual(
        loaded,
        [{ text: 'normal-1' }, { text: 'normal-2' }],
        'head items should survive, oversize tail should be dropped',
    );
});

test('queue load caps a tampered storage blob to MAX_QUEUE_ITEMS', async () => {
    // Tim's review (#990): the save-side cap protects benign overflow,
    // but not a hand-tampered storage entry. The load-side cap is the
    // adversarial-input defence — a 100,000-item array in the storage
    // blob should hydrate as at most MAX_QUEUE_ITEMS items so the live
    // messageQueue signal can't be blown by a malicious entry.
    const huge = [];
    for (let i = 0; i < 100000; i++) huge.push({ text: 'tamper-' + i });
    storage.setItem('alms.composer.queue.session-A', JSON.stringify(huge));
    const m = await loadComposerStorage();
    const loaded = m.loadQueue('session-A');
    assert.ok(loaded.length <= 50, 'load-side cap should bound the in-memory hydration');
    assert.equal(loaded[0].text, 'tamper-0');
});

test('queue load returns [] when sessionId is missing or null', async () => {
    const m = await loadComposerStorage();
    assert.deepEqual(m.loadQueue(null), []);
    assert.deepEqual(m.loadQueue(undefined), []);
    assert.deepEqual(m.loadQueue(''), []);
});

// ---------------------------------------------------------------------
// Accepted queue-head consumption - run creation correlation race
// ---------------------------------------------------------------------

test('consumeAcceptedQueueHead retains a rejected queued submission', async () => {
    const m = await loadComposerStorage();
    const head = { text: 'M1' };
    const queue = [head, { text: 'M2' }];

    const retained = m.consumeAcceptedQueueHead(queue, head, false);

    assert.equal(retained, queue, 'rejected submission keeps the original queue');
    assert.deepEqual(retained, [head, { text: 'M2' }]);
});

test('consumeAcceptedQueueHead removes only the accepted exact head', async () => {
    const m = await loadComposerStorage();
    const head = { text: 'same' };
    const duplicate = { text: 'same' };
    const queue = [head, duplicate];

    const remaining = m.consumeAcceptedQueueHead(queue, head, true);

    assert.deepEqual(remaining, [duplicate]);
    assert.equal(remaining[0], duplicate, 'identical text must not consume the next object');
});

test('consumeAcceptedQueueHead ignores a stale drain candidate', async () => {
    const m = await loadComposerStorage();
    const queue = [{ text: 'replacement' }];

    const retained = m.consumeAcceptedQueueHead(queue, { text: 'replacement' }, true);

    assert.equal(retained, queue, 'a different head object must remain queued');
});

// ---------------------------------------------------------------------
// Session deletion sweep — both #975 and #981 acceptance criterion
// ---------------------------------------------------------------------

test('clearComposerState wipes both draft and queue for a session', async () => {
    const m = await loadComposerStorage();
    m.saveDraft('session-A', 'unsent draft');
    m.saveQueue('session-A', [{ text: 'queued msg' }]);
    // Other sessions' state must not be touched.
    m.saveDraft('session-B', 'B draft');
    m.saveQueue('session-B', [{ text: 'B queued' }]);

    m.clearComposerState('session-A');

    assert.equal(m.loadDraft('session-A'), '');
    assert.deepEqual(m.loadQueue('session-A'), []);
    assert.equal(m.loadDraft('session-B'), 'B draft');
    assert.deepEqual(m.loadQueue('session-B'), [{ text: 'B queued' }]);
});

// ---------------------------------------------------------------------
// planMountDrain — pinned regression for the orphan-queue fix
// (Tim's review on PR #990, the critical-fix half of #975)
// ---------------------------------------------------------------------
//
// `planMountDrain` is the pure-function decision layer the InputArea
// mount effect calls into. We can't easily spin up a Preact tree under
// Node (no preact / signals / htm in the test runtime) so we extracted
// the orphan-drain logic into this pure function and pin its contract
// here. The InputArea callsite is a thin wrapper that does the side
// effects (signal write, storage write, startRun) — see
// components/chat/input-area.js useEffect block.
//
// The two scenarios Tim called out:
//   - "queue restored on mount after run completed → auto-drains"
//   - "run_finished arrives before InputArea mounts → queue still drained"
// both reduce to the same case from this function's perspective:
// `restoredQueue.length > 0 && !activeRunId` → return drain plan.

test('planMountDrain returns no-op when restoredQueue is empty', async () => {
    const m = await loadComposerStorage();
    const plan = m.planMountDrain({
        restoredQueue: [],
        activeRunId: null,
        activeAgentId: 'agent_x',
    });
    assert.deepEqual(plan, { drain: false, head: null, remaining: [] });
});

test('planMountDrain returns no-op when restoredQueue is null/undefined', async () => {
    const m = await loadComposerStorage();
    assert.deepEqual(
        m.planMountDrain({ restoredQueue: null, activeRunId: null, activeAgentId: 'agent_x' }),
        { drain: false, head: null, remaining: [] },
    );
    assert.deepEqual(
        m.planMountDrain({ restoredQueue: undefined, activeRunId: null, activeAgentId: 'agent_x' }),
        { drain: false, head: null, remaining: [] },
    );
});

test('planMountDrain skips drain when a run is already active', async () => {
    // The SSE drain in use-session-stream.js will handle this queue when
    // the live run finishes. The mount effect must NOT start a second
    // run in parallel, but must still hydrate the visible queue (the
    // remaining is the full restoredQueue).
    const m = await loadComposerStorage();
    const restoredQueue = [{ text: 'M1' }, { text: 'M2' }];
    const plan = m.planMountDrain({
        restoredQueue,
        activeRunId: 'run_123',
        activeAgentId: 'agent_x',
    });
    assert.equal(plan.drain, false);
    assert.equal(plan.head, null);
    assert.deepEqual(plan.remaining, restoredQueue);
});

test('planMountDrain drains head when queue restored without active run (#975 cross-session-return)', async () => {
    // Scenario: user started a run on A, queued M1, switched to B,
    // the run on A finished while on B (SSE drain skipped because the
    // in-memory queue was [] from the navigate-session wipe), came
    // back to A. Without the mount-effect drain M1 would sit in the
    // queue forever.
    const m = await loadComposerStorage();
    const plan = m.planMountDrain({
        restoredQueue: [{ text: 'M1' }],
        activeRunId: null,
        activeAgentId: 'agent_x',
    });
    assert.equal(plan.drain, true);
    assert.deepEqual(plan.head, { text: 'M1' });
    assert.deepEqual(plan.remaining, []);
});

test('planMountDrain drains head and preserves remainder for multi-item queue', async () => {
    // FIFO drain — head fires now, rest is persisted back to storage
    // and waits for the run-end SSE event. Same shape as the SSE drain
    // block in use-session-stream.js so the two paths are symmetric.
    const m = await loadComposerStorage();
    const plan = m.planMountDrain({
        restoredQueue: [{ text: 'M1' }, { text: 'M2' }, { text: 'M3' }],
        activeRunId: null,
        activeAgentId: 'agent_x',
    });
    assert.equal(plan.drain, true);
    assert.deepEqual(plan.head, { text: 'M1' });
    assert.deepEqual(plan.remaining, [{ text: 'M2' }, { text: 'M3' }]);
});

test('planMountDrain handles the page-reload-during-run race (#975 #2)', async () => {
    // Tim's #2 race: on reload, `boot()`'s SSE stream can deliver
    // `run_finished` *before* InputArea's mount effect runs. By the
    // time the mount effect fires, the SSE handler has already cleared
    // `activeRunId.value` (so we observe `activeRunId: null`) and
    // skipped its own drain because the in-memory queue was still []
    // at the time. The mount effect calls planMountDrain with the
    // freshly-loaded queue and `activeRunId: null`, and we drain.
    //
    // From planMountDrain's perspective this is the same input as the
    // cross-session-return case — it's the *call ordering* that
    // differs, not the inputs. Pinning it explicitly so the test
    // documents Tim's #2 race.
    const m = await loadComposerStorage();
    const plan = m.planMountDrain({
        restoredQueue: [{ text: 'queued-during-run' }],
        activeRunId: null,
        activeAgentId: 'agent_x',
    });
    assert.equal(plan.drain, true);
    assert.deepEqual(plan.head, { text: 'queued-during-run' });
});

test('planMountDrain skips drain when no agent is active (Tim re-review #1)', async () => {
    // Tim's re-review on PR #990, finding #1: `startRun` short-circuits
    // with an error message when `activeAgentId.value` is null. Without
    // this gate, the mount effect would peel the head off the queue and
    // persist the remainder, then `startRun` silently throws away the
    // peeled head. The fix is defense-in-depth: not reachable in normal
    // boot ordering (use-boot.js sets the agent before the session) but
    // worth pinning so a future refactor of boot ordering can't regress.
    //
    // Contract: no active agent + restored queue non-empty → no drain,
    // remaining is the full queue (so the caller restores it intact and
    // doesn't lose anything).
    const m = await loadComposerStorage();
    const restoredQueue = [{ text: 'M1' }, { text: 'M2' }];
    const planNull = m.planMountDrain({
        restoredQueue,
        activeRunId: null,
        activeAgentId: null,
    });
    assert.equal(planNull.drain, false, 'no agent → no drain');
    assert.equal(planNull.head, null, 'no agent → no peeled head');
    assert.deepEqual(
        planNull.remaining,
        restoredQueue,
        'no agent → queue stays intact (caller restores full queue)',
    );

    // undefined treated the same as null — defensive against either
    // shape from the signal.
    const planUndef = m.planMountDrain({
        restoredQueue,
        activeRunId: null,
        activeAgentId: undefined,
    });
    assert.equal(planUndef.drain, false);
    assert.deepEqual(planUndef.remaining, restoredQueue);
});

// ---------------------------------------------------------------------
// Robustness — localStorage may be absent or throw
// ---------------------------------------------------------------------

test('loadDraft / loadQueue gracefully handle missing localStorage', async () => {
    // Tear the stub down before loading the module; the typeof-window
    // guard inside the module should keep every API safe.
    uninstallStorageStub();

    const m = await loadComposerStorage();
    // None of these should throw; reads should return the empty
    // sentinel; writes should be no-ops.
    assert.doesNotThrow(() => m.saveDraft('session-A', 'hello'));
    assert.doesNotThrow(() => m.saveQueue('session-A', [{ text: 'q' }]));
    assert.equal(m.loadDraft('session-A'), '');
    assert.deepEqual(m.loadQueue('session-A'), []);
    assert.doesNotThrow(() => m.clearComposerState('session-A'));
});

test('saveQueue swallows quota-exceeded errors', async () => {
    // Simulate a quota-exceeded localStorage by patching the stub's
    // setItem to throw. The saveQueue caller should not see the error.
    const m = await loadComposerStorage();
    storage.setItem = () => { throw new Error('QuotaExceeded'); };
    assert.doesNotThrow(() => m.saveQueue('session-A', [{ text: 'msg' }]));
});
