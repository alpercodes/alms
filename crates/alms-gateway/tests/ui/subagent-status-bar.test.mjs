// Node-side tests for the Subagent status bar (redesign of the old
// live-content SubagentBar — relates to #1180, subsumes #1186).
//
// The bar is now a lightweight STATUS indicator: each chip renders a concise
// label derived from the subagent's most recent coarse activity signal (the
// ephemeral `subagent_activity` SSE events: kind = reasoning / writing /
// tool_start / tool_end, tool name only on tool_start). The subagent's
// streamed reasoning/token content is NO LONGER forwarded to the parent at
// all — the full transcript streams to the subagent's own session (#1184),
// reached by clicking the chip (navigateToSubagentSession).
//
// Pinned regression targets:
//   1. A tagged `subagent_activity` signal lands on the matching bar entry
//      (`activeSubagents[key].activity`) — named and unnamed (key migration).
//   2. Tagged `reasoning_delta` / `token_delta` / `tool_start` / `tool_end`
//      (replays from pre-status-bar event logs) are DROPPED: no bar write, no
//      leak into the parent's chat / reasoning view (#1170 invariant), and a
//      tagged `tool_end` must not mis-close a running PARENT tool row.
//   3. #1186 subsumption: with no reasoning text rendered, a buffered-
//      fallback re-emit can produce no duplicated bar content — the entry
//      stores only the latest {kind, tool}.
//   4. The #1183 startup race: an early signal (before the entry exists) is
//      buffered (latest-wins) and applied at entry creation; buffers are
//      evicted on completion / clear and LRU-capped.
//   5. `toolsUsed` counts tool_start signals and feeds the completion card.
//   6. `subagentStatusLabel` maps entry state to the user-facing labels.
//
// Strategy: same harness as dm-stream-rendering.test.mjs — read the real
// `use-session-stream.js`, rewrite its top-level imports with a stub block
// that imports the REAL `state/subagents.js` (also rewritten for Node), and
// drive the exported `openSessionStream` through a fake EventSource.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import url from 'node:url';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const STREAM_JS_PATH = path.resolve(
    __dirname,
    '../../static/ui/hooks/use-session-stream.js'
);
const SUBAGENTS_JS_PATH = path.resolve(
    __dirname,
    '../../static/ui/state/subagents.js'
);
const STATUS_LABEL_PATH = path.resolve(
    __dirname,
    '../../static/ui/utils/subagent-status.js'
);

// ---------------------------------------------------------------------------
// Load the REAL `state/subagents.js` under Node, rewriting its two top-level
// imports (signal from deps.js, activeSessionId from sessions.js). Mirrors the
// loader in `subagents-rehydrate.test.mjs`.
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

function loadRealSubagentsAsTempFile() {
    const src = fs.readFileSync(SUBAGENTS_JS_PATH, 'utf8');

    const signalImportRe =
        /^import\s+\{\s*signal\s*\}\s+from\s+['"][^'"]+['"];?\s*$/m;
    if (!signalImportRe.test(src)) {
        throw new Error(
            'subagents.js: expected a top-level `import { signal } from ...` line — '
            + 'update subagent-status-bar.test.mjs if the import shape changed.'
        );
    }
    const sessionsImportRe =
        /^import\s+\{\s*activeSessionId\s*\}\s+from\s+['"][^'"]+['"];?\s*$/m;
    if (!sessionsImportRe.test(src)) {
        throw new Error(
            'subagents.js: expected a top-level `import { activeSessionId } from ...` line — '
            + 'update subagent-status-bar.test.mjs if the import shape changed.'
        );
    }
    // The cancel-confirm lifecycle hooks (Codex P2, PR #1192) are exercised
    // end-to-end by subagent-cancel.test.mjs; this suite stubs them as
    // no-ops so the module loads without pulling in api/sessions.js.
    const cancelImportRe =
        /^import\s+\{\s*clearCancelConfirmForSession\s*,\s*dismissSubagentCancel\s*\}\s+from\s+['"][^'"]+['"];?\s*$/m;
    if (!cancelImportRe.test(src)) {
        throw new Error(
            'subagents.js: expected a top-level `import { clearCancelConfirmForSession, '
            + 'dismissSubagentCancel } from ...` line — '
            + 'update subagent-status-bar.test.mjs if the import shape changed.'
        );
    }

    const stubbed = src
        .replace(signalImportRe, SIGNAL_STUB)
        .replace(sessionsImportRe,
            'const activeSessionId = { get value() { return null; }, set value(_) {} };')
        .replace(cancelImportRe,
            'const clearCancelConfirmForSession = () => {};\n'
            + 'const dismissSubagentCancel = () => {};');

    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'alms-sb-real-'));
    const tmpFile = path.join(tmpDir, 'subagents.mjs');
    fs.writeFileSync(tmpFile, stubbed, 'utf8');
    return url.pathToFileURL(tmpFile).href;
}

const realSubagentsUrl = loadRealSubagentsAsTempFile();

// ---------------------------------------------------------------------------
// Stub block injected in place of the stream module's top-level imports. The
// subagent functions are the REAL ones (imported from the rewritten temp
// module above); everything else the handlers touch but we don't assert on is
// a minimal stub. `dmThinkingBuffers` is exported by the stream module itself.
// ---------------------------------------------------------------------------
const STUB_PRELUDE = `
import {
    trackSubagentStart,
    trackSubagentEnd,
    trackSubagentActivity,
    findSubagentByToolInvocationId,
    findSubagentBySessionId,
    setSubagentSessionId,
    activeSubagents,
    clearAllSubagents,
    rehydrateSubagentsFromHistory,
} from ${JSON.stringify(realSubagentsUrl)};

function signal(initial) {
    return { value: initial };
}
function batch(fn) { return fn(); }

// chat state
const chatMessages = signal([]);
let __msgSeq = 0;
function nextMsgId() { return 'msg-' + (++__msgSeq); }

function appendMessage(...msgs) {
    chatMessages.value = [...chatMessages.value, ...msgs];
}
function updateMessage(predicate, updater) {
    const msgs = chatMessages.value;
    const idx = msgs.findLastIndex(predicate);
    if (idx < 0) return false;
    const copy = [...msgs];
    copy[idx] = updater(copy[idx]);
    chatMessages.value = copy;
    return true;
}
function filterMessages(predicate) {
    const msgs = chatMessages.value;
    const filtered = msgs.filter(predicate);
    if (filtered.length !== msgs.length) chatMessages.value = filtered;
}
function transformMessages(fn) { chatMessages.value = fn(chatMessages.value); }

// runs state
const activeRunId = signal(null);
function bumpRunListGeneration() {}

// agent-status (inert beyond what handlers call)
const agentPhase = signal({ phase: null, detail: null });
function setAgentPhase() {}
function clearAgentPhase() {}
function setDmContext() {}
function revertPhase() {}
const dmPeer = signal(null);

// queue
const messageQueue = signal([]);

// sessions
const activeSessionId = signal(null);
const activeSession = signal(null);
const dmParticipants = signal([]);

// agents
const activeAgent = signal(null);

// misc inert helpers
function normalizeApproval(d) { return { approvalId: d.approval_id, tool: d.tool, params: d.params, runId: d.run_id }; }
let selectGeneration = 0;
function confirmOptimisticMessage() {}
function rollbackOptimisticMessage() {}
function saveQueue() {}
const DM_END_REASON_LABELS = { ignored: 'no further replies', depth_exceeded: 'message limit reached', user_cancelled: 'cancelled by user', errored: 'run failed' };
function markStreamDead() {}
function clearStreamDead() {}
function registerSessionReconnect() {}
const __sealedSets = new Map();
function setSealedReasoningRunIds(id, set) { __sealedSets.set(id, set); }
function getSealedReasoningRunIds(id) { return __sealedSets.get(id) || null; }
function clearSealedReasoningRunIds(id) { __sealedSets.delete(id); }

// Test hooks — exported so the harness can reach the live signals and the real
// subagent module surface.
export const __test = {
    signal, chatMessages, activeRunId, activeSession, dmParticipants,
    activeAgent, dmPeer, activeSubagents, trackSubagentStart,
    trackSubagentActivity, trackSubagentEnd, clearAllSubagents,
    rehydrateSubagentsFromHistory,
};
`;

async function loadStreamModule() {
    let src = fs.readFileSync(STREAM_JS_PATH, 'utf8');

    // Strip every top-level import (single-line and multi-line block forms).
    src = src.replace(/^import\s+\{[\s\S]*?\}\s+from\s+['"][^'"]+['"];?\s*$/gm, '');
    src = src.replace(/^import\s+[^{][^;]*?from\s+['"][^'"]+['"];?\s*$/gm, '');

    if (/^\s*import\s/m.test(src)) {
        throw new Error(
            'use-session-stream.js: a top-level import survived the rewrite — '
            + 'update subagent-status-bar.test.mjs if the import shape changed.'
        );
    }

    const stubbed = STUB_PRELUDE + '\n' + src;

    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'alms-sb-stream-'));
    const tmpFile = path.join(tmpDir, 'stream.mjs');
    fs.writeFileSync(tmpFile, stubbed, 'utf8');
    return await import(url.pathToFileURL(tmpFile).href);
}

// ---------------------------------------------------------------------------
// Fake browser globals.
// ---------------------------------------------------------------------------
class FakeEventSource {
    constructor(u) {
        this.url = u;
        this.readyState = 1;
        this._listeners = new Map();
        FakeEventSource.last = this;
    }
    addEventListener(type, fn) {
        if (!this._listeners.has(type)) this._listeners.set(type, []);
        this._listeners.get(type).push(fn);
    }
    close() { this.readyState = 2; }
    emit(type, data, lastEventId) {
        const evt = {
            data: typeof data === 'string' ? data : JSON.stringify(data),
            lastEventId: lastEventId != null ? String(lastEventId) : 'ephemeral-1',
        };
        for (const fn of this._listeners.get(type) || []) fn(evt);
    }
}
FakeEventSource.CLOSED = 2;
FakeEventSource.OPEN = 1;

globalThis.EventSource = FakeEventSource;
globalThis.localStorage = { getItem() { return null; }, setItem() {}, removeItem() {} };
globalThis.__almsContracts = {
    parseSseJsonPayload(_type, raw) {
        return JSON.parse(raw);
    },
};
globalThis.requestAnimationFrame = (fn) => { fn(); return 1; };
globalThis.cancelAnimationFrame = () => {};

const mod = await loadStreamModule();
const T = mod.__test;
const { subagentStatusLabel } = await import(url.pathToFileURL(STATUS_LABEL_PATH).href);

function openStream(sessionId) {
    mod.openSessionStream(sessionId);
    return FakeEventSource.last;
}

function reset() {
    T.chatMessages.value = [];
    T.activeRunId.value = null;
    T.activeSession.value = null;
    T.dmParticipants.value = [];
    T.activeAgent.value = null;
    T.dmPeer.value = null;
    // Clears entries AND the pending early-activity buffers (no cross-test bleed).
    T.clearAllSubagents();
    mod.dmThinkingBuffers.value = new Map();
    mod.closeSessionStream();
}

// ===========================================================================
// subagent_activity drives the bar entry's status.
// ===========================================================================

test('subagent_activity sets the entry activity (named subagent)', () => {
    reset();
    T.trackSubagentStart('reviewer', 'review the diff', 'inv-1');
    const es = openStream('sess-1');

    es.emit('subagent_activity', { source_agent: 'reviewer', kind: 'reasoning' });
    assert.deepEqual(T.activeSubagents.value.reviewer.activity,
        { kind: 'reasoning', tool: null });

    es.emit('subagent_activity', { source_agent: 'reviewer', kind: 'tool_start', tool: 'shell' });
    assert.deepEqual(T.activeSubagents.value.reviewer.activity,
        { kind: 'tool_start', tool: 'shell' },
        'the LATEST signal wins — status reflects the most recent activity');

    es.emit('subagent_activity', { source_agent: 'reviewer', kind: 'writing' });
    assert.deepEqual(T.activeSubagents.value.reviewer.activity,
        { kind: 'writing', tool: null });
});

test('subagent_activity migrates the unnamed entry key to the backend label', () => {
    reset();
    // Unnamed subagent: tool_start registered it under
    // "subagent-{toolInvocationId_prefix}", but the backend labels forwarded
    // signals with "subagent-{task_id_prefix}" — a DIFFERENT id. The signal
    // must migrate the entry to the backend key so it lands on the right chip.
    T.trackSubagentStart('subagent', 'some task', 'abcdef0123456789');
    const startKey = 'subagent-' + 'abcdef01';
    assert.ok(T.activeSubagents.value[startKey], 'entry starts under the tool-id-prefixed key');

    const es = openStream('sess-1');
    const backendLabel = 'subagent-deadbeef';
    es.emit('subagent_activity', { source_agent: backendLabel, kind: 'reasoning' });

    assert.equal(T.activeSubagents.value[startKey], undefined,
        'the start-time key is migrated away to the backend-assigned label');
    const migrated = T.activeSubagents.value[backendLabel];
    assert.ok(migrated, 'entry now lives under the backend-assigned label');
    assert.deepEqual(migrated.activity, { kind: 'reasoning', tool: null });
});

test('a subagent_activity for a gone/unknown subagent never creates a chip', () => {
    reset();
    const es = openStream('sess-1');

    es.emit('subagent_activity', { source_agent: 'reviewer', kind: 'writing' });

    assert.deepEqual(T.activeSubagents.value, {},
        'a signal for an unknown subagent must not resurrect/create a chip');
});

// ===========================================================================
// Concurrent UNNAMED subagents: identity-exact chip resolution via the parent
// invoke_agent correlator (#1190 Codex P2, structural gap). Unnamed chips are
// keyed by the PARENT invoke_agent tool-invocation-id, but the backend labels
// signals with the TASK id — a first-match `subagent-*` fallback can migrate
// subagent B's status onto subagent A's chip, and the migration sticks. The
// signal now carries `parent_tool_invocation_id` (same id as
// `subagent_started` / the entry's stored toolInvocationId) so resolution is
// exact; the label fallback remains only for correlator-less legacy signals.
// ===========================================================================

test('concurrent unnamed subagents: B\'s signal never lands on A\'s chip (exact correlator resolution)', () => {
    reset();
    // Two unnamed subagents in flight concurrently. Iteration order of
    // activeSubagents follows insertion — A first — which is exactly what
    // made the first-match fallback grab A's chip for B's signal.
    T.trackSubagentStart('subagent', 'task A', 'aaaa000011112222');
    T.trackSubagentStart('subagent', 'task B', 'bbbb000011112222');
    const keyA = 'subagent-aaaa0000';
    const keyB = 'subagent-bbbb0000';
    assert.ok(T.activeSubagents.value[keyA]);
    assert.ok(T.activeSubagents.value[keyB]);

    const es = openStream('sess-1');

    // B's signal arrives FIRST (arbitrary order — e.g. the attach-time
    // snapshot replay iterates a DashMap). It carries B's parent correlator
    // and B's backend task label.
    es.emit('subagent_activity', {
        source_agent: 'subagent-btask456', kind: 'writing',
        parent_tool_invocation_id: 'bbbb000011112222',
    });

    // A's chip is untouched — B's status must NOT have migrated onto it.
    const entryA = T.activeSubagents.value[keyA];
    assert.ok(entryA, 'A\'s chip still lives under its own key');
    assert.equal(entryA.activity, null,
        'B\'s activity must never attach to A\'s chip');
    // B's chip got the status, migrated to B's backend label.
    const entryB = T.activeSubagents.value['subagent-btask456'];
    assert.ok(entryB, 'B\'s chip migrated to B\'s backend label');
    assert.equal(entryB.toolInvocationId, 'bbbb000011112222',
        'the migrated entry is genuinely B (parent invocation id preserved)');
    assert.deepEqual(entryB.activity, { kind: 'writing', tool: null });
    assert.equal(T.activeSubagents.value[keyB], undefined,
        'B\'s start-time key migrated away');

    // A's signal then lands on A's chip.
    es.emit('subagent_activity', {
        source_agent: 'subagent-atask123', kind: 'tool_start', tool: 'shell',
        tool_invocation_id: 'tid-a1',
        parent_tool_invocation_id: 'aaaa000011112222',
    });
    const migratedA = T.activeSubagents.value['subagent-atask123'];
    assert.ok(migratedA, 'A\'s chip migrated to A\'s backend label');
    assert.equal(migratedA.toolInvocationId, 'aaaa000011112222');
    assert.deepEqual(migratedA.activity, { kind: 'tool_start', tool: 'shell' });
    assert.equal(migratedA.toolsUsed, 1);
    // B unchanged by A's signal.
    assert.deepEqual(T.activeSubagents.value['subagent-btask456'].activity,
        { kind: 'writing', tool: null });
});

test('concurrent unnamed subagents: snapshot replay resolves exactly in BOTH orders', () => {
    for (const order of [['A', 'B'], ['B', 'A']]) {
        reset();
        T.trackSubagentStart('subagent', 'task A', 'aaaa000011112222');
        T.trackSubagentStart('subagent', 'task B', 'bbbb000011112222');
        const es = openStream('sess-1');

        const signals = {
            A: {
                source_agent: 'subagent-atask123', kind: 'tool_start',
                tool: 'shell', tool_invocation_id: 'tid-a1',
                parent_tool_invocation_id: 'aaaa000011112222',
            },
            B: {
                source_agent: 'subagent-btask456', kind: 'writing',
                parent_tool_invocation_id: 'bbbb000011112222',
            },
        };

        // Live signals land once...
        es.emit('subagent_activity', signals[order[0]]);
        es.emit('subagent_activity', signals[order[1]]);
        // ...then an EventSource reconnect replays the CURRENT snapshot of
        // both subagents — again in this order.
        es.emit('subagent_activity', signals[order[0]]);
        es.emit('subagent_activity', signals[order[1]]);

        const a = T.activeSubagents.value['subagent-atask123'];
        const b = T.activeSubagents.value['subagent-btask456'];
        assert.ok(a, `[order ${order}] A resolves to its own chip`);
        assert.ok(b, `[order ${order}] B resolves to its own chip`);
        assert.equal(a.toolInvocationId, 'aaaa000011112222',
            `[order ${order}] A's chip is keyed to A's parent invocation`);
        assert.equal(b.toolInvocationId, 'bbbb000011112222',
            `[order ${order}] B's chip is keyed to B's parent invocation`);
        assert.deepEqual(a.activity, { kind: 'tool_start', tool: 'shell' },
            `[order ${order}] A shows A's status`);
        assert.deepEqual(b.activity, { kind: 'writing', tool: null },
            `[order ${order}] B shows B's status`);
        assert.equal(a.toolsUsed, 1,
            `[order ${order}] the snapshot replay did not recount A's tool`);
        assert.equal(b.toolsUsed, 0,
            `[order ${order}] A's tool never leaked onto B's count`);
        assert.equal(Object.keys(T.activeSubagents.value).length, 2,
            `[order ${order}] exactly the two chips remain — no ghost entries`);
    }
});

test('a signal matching a TERMINAL entry is DROPPED — a same-named re-invocation starts fresh (#1190 r4 Codex P2)', () => {
    reset();
    T.trackSubagentStart('reviewer', 'first run', 'inv-1');
    const es = openStream('sess-1');

    // First invocation uses a tool, then completes.
    es.emit('subagent_activity', {
        source_agent: 'reviewer', kind: 'tool_start', tool: 'shell',
        tool_invocation_id: 'tid-1', parent_tool_invocation_id: 'inv-1',
    });
    es.emit('subagent_completed', {
        subagent_name: 'reviewer', status: 'done',
        subagent_session_id: 'sub-1', summary: 'ok',
    });
    assert.equal(T.activeSubagents.value.reviewer.status, 'done');

    // A stale signal for the FINISHED invocation straggles in (e.g. a
    // snapshot replay racing completion). Its correlator resolves to the
    // terminal entry: the signal must be DROPPED — neither applied to the
    // terminal chip nor buffered under the name. Buffering it is the bug:
    // the next same-named invocation within the 30s pending window would
    // consume the name-keyed buffer at entry creation and seed the fresh
    // chip with the completed run's activity/tool count.
    es.emit('subagent_activity', {
        source_agent: 'reviewer', kind: 'tool_start', tool: 'fs_read',
        tool_invocation_id: 'tid-9', parent_tool_invocation_id: 'inv-1',
    });
    assert.equal(T.activeSubagents.value.reviewer.status, 'done');
    assert.deepEqual(T.activeSubagents.value.reviewer.activity,
        { kind: 'tool_start', tool: 'shell' },
        'the terminal chip is untouched by the stale signal');
    assert.equal(T.activeSubagents.value.reviewer.toolsUsed, 1,
        'the terminal chip count is untouched by the stale signal');

    // Re-invoke the same-named subagent within the pending window: the
    // fresh chip must NOT inherit the finished invocation's status.
    T.trackSubagentStart('reviewer', 'second run', 'inv-2');
    const fresh = T.activeSubagents.value.reviewer;
    assert.equal(fresh.status, 'running');
    assert.equal(fresh.activity, null,
        'the fresh chip starts on "Starting…" — no stale activity carryover');
    assert.equal(fresh.toolsUsed, 0,
        'no stale tool-count carryover from the completed invocation');
});

test('correlator with no id-match falls back to the EXACT direct label hit — not buffered forever (#1190 r4 Tim)', () => {
    reset();
    // Nonstandard creation path: the entry's stored toolInvocationId is
    // missing (legacy tool_start without an id), so the signal's correlator
    // can never id-match it — but the label matches directly, which is
    // itself identity-exact (only the first-match LOOP is ambiguous).
    T.trackSubagentStart('reviewer', 'task', undefined);
    assert.equal(T.activeSubagents.value.reviewer.toolInvocationId, null);

    const es = openStream('sess-1');
    es.emit('subagent_activity', {
        source_agent: 'reviewer', kind: 'writing',
        parent_tool_invocation_id: 'inv-X',
    });
    assert.deepEqual(T.activeSubagents.value.reviewer.activity,
        { kind: 'writing', tool: null },
        'the direct label hit resolves the chip instead of buffering the '
        + 'signal forever on "Starting…"');
});

test('a straggler whose correlator mismatches the label entry\'s stored id is DROPPED (#1190 r5 Codex P2)', () => {
    reset();
    const es = openStream('sess-1');

    // Invocation inv-1 of the named subagent runs and completes...
    T.trackSubagentStart('reviewer', 'first run', 'inv-1');
    es.emit('subagent_completed', {
        subagent_name: 'reviewer', status: 'done',
        subagent_session_id: 'sub-1', summary: 'ok',
    });
    // ...and inv-2 re-invokes under the same name, OVERWRITING the entry:
    // the stored toolInvocationId is now inv-2, status running.
    T.trackSubagentStart('reviewer', 'second run', 'inv-2');
    assert.equal(T.activeSubagents.value.reviewer.toolInvocationId, 'inv-2');
    assert.equal(T.activeSubagents.value.reviewer.status, 'running');

    // A delayed/snapshot activity for INV-1 arrives. Its id-match finds
    // nothing (inv-1's entry is gone), and the entry under the label stores
    // a DIFFERENT id — so this is provably a straggler from a finished
    // invocation. It must be DROPPED: taking the label hit would pin inv-1's
    // stale activity/tool count onto inv-2's fresh chip — the exact
    // stale-signal class the correlator exists to prevent.
    es.emit('subagent_activity', {
        source_agent: 'reviewer', kind: 'tool_start', tool: 'fs_read',
        tool_invocation_id: 'tid-stale', parent_tool_invocation_id: 'inv-1',
    });
    const fresh = T.activeSubagents.value.reviewer;
    assert.equal(fresh.activity, null,
        'inv-1\'s stale activity must not land on inv-2\'s fresh chip');
    assert.equal(fresh.toolsUsed, 0,
        'inv-1\'s stale tool count must not land on inv-2\'s fresh chip');

    // inv-2's OWN signals still resolve exactly (id-match).
    es.emit('subagent_activity', {
        source_agent: 'reviewer', kind: 'writing',
        parent_tool_invocation_id: 'inv-2',
    });
    assert.deepEqual(T.activeSubagents.value.reviewer.activity,
        { kind: 'writing', tool: null });
});

test('toolsUsed counts tool_start signals (feeds the completion card)', () => {
    reset();
    T.trackSubagentStart('reviewer', 'task', 'inv-1');
    const es = openStream('sess-1');

    es.emit('subagent_activity', { source_agent: 'reviewer', kind: 'tool_start', tool: 'shell' });
    es.emit('subagent_activity', { source_agent: 'reviewer', kind: 'tool_end' });
    es.emit('subagent_activity', { source_agent: 'reviewer', kind: 'tool_start', tool: 'fs_read' });
    es.emit('subagent_activity', { source_agent: 'reviewer', kind: 'reasoning' });

    assert.equal(T.activeSubagents.value.reviewer.toolsUsed, 2,
        'only tool_start signals increment the tool count');

    // The completion card takes its toolCount from the entry.
    es.emit('subagent_completed', {
        subagent_name: 'reviewer', status: 'done',
        subagent_session_id: 'sub-sess-1', summary: 'done',
    });
    const card = T.chatMessages.value.find(m => m.type === 'subagent_completed');
    assert.ok(card, 'completion card rendered');
    assert.equal(card.toolCount, 2, 'completion card reflects the counted tools');
});

test('parallel same-tool invocations both count — distinct ids, no interposed tool_end (#1190 Codex)', () => {
    reset();
    T.trackSubagentStart('reviewer', 'task', 'inv-1');
    const es = openStream('sess-1');

    // FullControl/Autonomous mode: `run_tool_calls_parallel` starts
    // non-conflicting tool calls concurrently, so two tool_starts for the
    // SAME tool can arrive back-to-back with NO interposed tool_end. The old
    // same-tool/current-activity heuristic collapsed the second one as a
    // "replay" — undercounting. Distinct invocation ids disambiguate.
    es.emit('subagent_activity', {
        source_agent: 'reviewer', kind: 'tool_start', tool: 'shell',
        tool_invocation_id: 'tid-1',
    });
    es.emit('subagent_activity', {
        source_agent: 'reviewer', kind: 'tool_start', tool: 'shell',
        tool_invocation_id: 'tid-2',
    });
    assert.equal(T.activeSubagents.value.reviewer.toolsUsed, 2,
        'two parallel invocations of the same tool are two tools');
});

test('a snapshot-replayed tool_start (same invocation id) does not inflate toolsUsed (#1190 Codex P2 + Tim)', () => {
    reset();
    T.trackSubagentStart('reviewer', 'task', 'inv-1');
    const es = openStream('sess-1');

    es.emit('subagent_activity', {
        source_agent: 'reviewer', kind: 'tool_start', tool: 'shell',
        tool_invocation_id: 'tid-1',
    });
    assert.equal(T.activeSubagents.value.reviewer.toolsUsed, 1);

    // EventSource reconnect mid-tool: `attach_session_stream` replays the
    // subagent's CURRENT activity as an ordinary `subagent_activity` — same
    // invocation id as the live signal, which is what marks it as a replay
    // rather than a new invocation.
    es.emit('subagent_activity', {
        source_agent: 'reviewer', kind: 'tool_start', tool: 'shell',
        tool_invocation_id: 'tid-1',
    });
    assert.equal(T.activeSubagents.value.reviewer.toolsUsed, 1,
        'an already-counted invocation id must not count again');
    assert.deepEqual(T.activeSubagents.value.reviewer.activity,
        { kind: 'tool_start', tool: 'shell' },
        'the activity itself still refreshes');

    // Repeated reconnects during the same long tool call do not drift.
    es.emit('subagent_activity', {
        source_agent: 'reviewer', kind: 'tool_start', tool: 'shell',
        tool_invocation_id: 'tid-1',
    });
    es.emit('subagent_activity', {
        source_agent: 'reviewer', kind: 'tool_start', tool: 'shell',
        tool_invocation_id: 'tid-1',
    });
    assert.equal(T.activeSubagents.value.reviewer.toolsUsed, 1);
});

test('distinct sequential tool invocations still count', () => {
    reset();
    T.trackSubagentStart('reviewer', 'task', 'inv-1');
    const es = openStream('sess-1');

    es.emit('subagent_activity', {
        source_agent: 'reviewer', kind: 'tool_start', tool: 'shell',
        tool_invocation_id: 'tid-1',
    });
    es.emit('subagent_activity', {
        source_agent: 'reviewer', kind: 'tool_end', tool_invocation_id: 'tid-1',
    });
    es.emit('subagent_activity', {
        source_agent: 'reviewer', kind: 'tool_start', tool: 'shell',
        tool_invocation_id: 'tid-2',
    });
    assert.equal(T.activeSubagents.value.reviewer.toolsUsed, 2,
        'the same tool re-run under a fresh invocation id is a NEW invocation');

    es.emit('subagent_activity', {
        source_agent: 'reviewer', kind: 'tool_end', tool_invocation_id: 'tid-2',
    });
    es.emit('subagent_activity', {
        source_agent: 'reviewer', kind: 'tool_start', tool: 'fs_read',
        tool_invocation_id: 'tid-3',
    });
    assert.equal(T.activeSubagents.value.reviewer.toolsUsed, 3,
        'a different tool counts too');
});

test('unnamed subagent: buffered early tool_start id survives into the entry — snapshot replay does not double count', () => {
    reset();
    const es = openStream('sess-1');

    // Early signal for an UNNAMED subagent (backend task label) arrives
    // BEFORE the entry exists (#1183) and is buffered WITH its invocation id.
    es.emit('subagent_activity', {
        source_agent: 'subagent-deadbeef', kind: 'tool_start', tool: 'shell',
        tool_invocation_id: 'tid-1',
    });

    // Entry creation consumes the buffer: toolsUsed seeds to 1 AND the id is
    // remembered as counted.
    T.trackSubagentStart('subagent', 'task', 'abcdef0123456789');
    const startKey = 'subagent-abcdef01';
    assert.equal(T.activeSubagents.value[startKey].toolsUsed, 1);

    // A reconnect snapshot replays the SAME in-progress tool_start: it must
    // resolve (key migration) and refresh the status without recounting.
    es.emit('subagent_activity', {
        source_agent: 'subagent-deadbeef', kind: 'tool_start', tool: 'shell',
        tool_invocation_id: 'tid-1',
    });
    const migrated = T.activeSubagents.value['subagent-deadbeef'];
    assert.ok(migrated, 'entry migrated to the backend label');
    assert.equal(migrated.toolsUsed, 1,
        'the buffered id was already counted at entry creation — the snapshot '
        + 'replay must not double count');

    // A genuinely new parallel invocation still counts.
    es.emit('subagent_activity', {
        source_agent: 'subagent-deadbeef', kind: 'tool_start', tool: 'shell',
        tool_invocation_id: 'tid-2',
    });
    assert.equal(T.activeSubagents.value['subagent-deadbeef'].toolsUsed, 2);
});

// ===========================================================================
// Content events tagged source_agent are DROPPED — no bar write, no parent
// leak (#1170 invariant; replays from pre-status-bar event logs).
// ===========================================================================

test('tagged reasoning_delta is dropped: no bar write, no parent leak (#1186 subsumed)', () => {
    reset();
    T.trackSubagentStart('reviewer', 'review the diff', 'inv-1');
    const es = openStream('sess-1');
    const before = T.activeSubagents.value.reviewer;

    es.emit('reasoning_delta', { source_agent: 'reviewer', text: 'private subagent thinking' });

    // No bar surface renders reasoning text anymore — the entry is untouched.
    assert.equal(T.activeSubagents.value.reviewer, before,
        'a tagged reasoning_delta must not modify the bar entry at all');

    // And it must not leak into any parent surface (the #1170 invariant).
    assert.equal(T.chatMessages.value.length, 0,
        'subagent reasoning must NOT create any parent chat message');
    for (const [, text] of mod.dmThinkingBuffers.value) {
        assert.ok(!text.includes('private subagent thinking'),
            'subagent reasoning must NOT enter the DM reasoning collapsible');
    }
});

test('parent reasoning_delta (no source_agent) still reaches the parent view, not the bar', () => {
    reset();
    T.trackSubagentStart('reviewer', 'review the diff', 'inv-1');
    const es = openStream('sess-1');

    es.emit('reasoning_delta', { run_id: 'run-parent', text: 'parent agent reasoning' });

    const parentMsg = T.chatMessages.value.find(
        m => m.type === 'agent' && (m.reasoning || '').includes('parent agent reasoning')
    );
    assert.ok(parentMsg, 'parent reasoning renders in the parent main-view (unchanged)');
    assert.deepEqual(T.activeSubagents.value.reviewer.activity, null,
        'a parent reasoning delta must NOT bleed into the subagent status');
});

test('tagged tool_start/tool_end are dropped and cannot touch parent tool rows', () => {
    reset();
    // The PARENT has a running invoke_agent tool row (the foreground case).
    T.chatMessages.value = [{
        id: 'parent-inv-1', type: 'tool', tool: 'invoke_agent',
        params: { name: 'reviewer', task: 't' }, status: 'running', startedAt: Date.now(),
    }];
    T.trackSubagentStart('reviewer', 't', 'parent-inv-1');
    const es = openStream('sess-1');

    // Replayed tagged tool events from a pre-status-bar event log.
    es.emit('tool_start', {
        source_agent: 'reviewer', tool_invocation_id: 'sub-tool-1',
        tool: 'shell', params: { cmd: 'ls' },
    });
    es.emit('tool_end', {
        source_agent: 'reviewer', tool_invocation_id: 'sub-tool-1',
        ok: true, result: { output: 'subagent result' },
    });

    // No new parent chat rows, and the PARENT's running invoke_agent row must
    // not be closed by the subagent's tool_end (the tagged tool_end used to
    // fall through to the last-running-tool fallback).
    assert.equal(T.chatMessages.value.length, 1, 'no parent rows added');
    const parentRow = T.chatMessages.value[0];
    assert.equal(parentRow.status, 'running',
        'a tagged tool_end must never mis-close a running PARENT tool row');
    assert.equal(parentRow.result, undefined,
        'the subagent result must not be attached to the parent row');
    // And the bar entry gains no activity from raw tagged tool events —
    // status comes exclusively from subagent_activity.
    assert.deepEqual(T.activeSubagents.value.reviewer.activity, null);
});

test('tagged token_delta stays suppressed from the parent view', () => {
    reset();
    const es = openStream('sess-1');
    es.emit('token_delta', { source_agent: 'reviewer', delta: 'subagent output' });
    assert.equal(T.chatMessages.value.length, 0,
        'tagged token_delta must not render in the parent view');
});

// ===========================================================================
// #1183 startup race: early signals are buffered (latest-wins) and applied at
// entry creation; buffers are evicted on completion / clear and LRU-capped.
// ===========================================================================

test('#1183: an early activity signal is buffered and applied at entry creation (named)', () => {
    reset();
    const es = openStream('sess-1');

    // The subagent's first signals race ahead of the parent's tool_start —
    // no entry exists yet. Latest-wins: only the most recent status matters.
    es.emit('subagent_activity', { source_agent: 'reviewer', kind: 'reasoning' });
    es.emit('subagent_activity', { source_agent: 'reviewer', kind: 'tool_start', tool: 'shell' });
    assert.deepEqual(T.activeSubagents.value, {},
        'early signals must NOT create a chip on their own');

    // The parent's tool_start (invoke_agent) handler now creates the entry.
    T.trackSubagentStart('reviewer', 'review the diff', 'inv-1');
    assert.deepEqual(T.activeSubagents.value.reviewer.activity,
        { kind: 'tool_start', tool: 'shell' },
        'the LATEST buffered signal is applied to the new entry');
});

test('#1183: early signal for an UNNAMED subagent applies at entry creation (Codex P2 on #1189)', () => {
    reset();
    const es = openStream('sess-1');
    const backendLabel = 'subagent-deadbeef';

    // The subagent's ONLY early signal: a single `reasoning` that wins the
    // startup race. The backend DEDUPS consecutive same-kind signals, so no
    // second `reasoning` will arrive to trigger the key migration — if entry
    // creation didn't consume the backend-labelled buffer, the chip would sit
    // on "Starting…" for the entire reasoning phase.
    es.emit('subagent_activity', { source_agent: backendLabel, kind: 'reasoning' });
    assert.deepEqual(T.activeSubagents.value, {}, 'no chip created by the early signal');

    // tool_start registers the entry under the tool-id-prefixed key (which
    // differs from the backend label) — the buffered signal must still apply.
    T.trackSubagentStart('subagent', 'some task', 'abcdef0123456789');
    const startKey = 'subagent-abcdef01';
    const entry = T.activeSubagents.value[startKey];
    assert.ok(entry, 'entry created under the tool-id-prefixed key');
    assert.deepEqual(entry.activity, { kind: 'reasoning', tool: null },
        'the backend-labelled buffered signal applies at entry creation');
    assert.equal(subagentStatusLabel(entry), 'Reasoning…',
        'the chip must show "Reasoning…" — NOT sit on "Starting…"');

    // A later live signal still migrates the key and supersedes the status.
    es.emit('subagent_activity', { source_agent: backendLabel, kind: 'writing' });
    assert.equal(T.activeSubagents.value[startKey], undefined,
        'the live signal migrates the entry to the backend label');
    const migrated = T.activeSubagents.value[backendLabel];
    assert.ok(migrated, 'entry now lives under the backend-assigned label');
    assert.deepEqual(migrated.activity, { kind: 'writing', tool: null });
});

test('a buffered tool_start increments toolsUsed like the live path (Tim nit on #1189)', () => {
    reset();
    const es = openStream('sess-1');

    // Named subagent: early tool_start buffered before the entry exists.
    es.emit('subagent_activity', { source_agent: 'reviewer', kind: 'tool_start', tool: 'shell' });
    T.trackSubagentStart('reviewer', 'task', 'inv-1');

    const entry = T.activeSubagents.value.reviewer;
    assert.deepEqual(entry.activity, { kind: 'tool_start', tool: 'shell' });
    assert.equal(subagentStatusLabel(entry), 'Using shell');
    assert.equal(entry.toolsUsed, 1,
        'a buffered tool_start must count toward toolsUsed (no completion-card undercount)');

    // A subsequent live tool_start keeps counting from there.
    es.emit('subagent_activity', { source_agent: 'reviewer', kind: 'tool_end' });
    es.emit('subagent_activity', { source_agent: 'reviewer', kind: 'tool_start', tool: 'fs_read' });
    assert.equal(T.activeSubagents.value.reviewer.toolsUsed, 2);

    // Non-tool buffered kinds do not count.
    T.clearAllSubagents();
    es.emit('subagent_activity', { source_agent: 'reviewer', kind: 'reasoning' });
    T.trackSubagentStart('reviewer', 'task', 'inv-2');
    assert.equal(T.activeSubagents.value.reviewer.toolsUsed, 0,
        'a buffered non-tool signal must not inflate the tool count');
});

test('an unnamed buffered tool_start applies at creation AND counts (both #1189 nits together)', () => {
    reset();
    const es = openStream('sess-1');
    const backendLabel = 'subagent-cafebabe';

    // Unnamed subagent's only early signal is a tool_start.
    es.emit('subagent_activity', { source_agent: backendLabel, kind: 'tool_start', tool: 'shell' });
    T.trackSubagentStart('subagent', 'some task', 'abcdef0123456789');

    const entry = T.activeSubagents.value['subagent-abcdef01'];
    assert.ok(entry, 'entry created under the tool-id-prefixed key');
    assert.equal(subagentStatusLabel(entry), 'Using shell',
        'the buffered tool_start must surface, not "Starting…"');
    assert.equal(entry.toolsUsed, 1, 'the buffered tool_start is counted');
});

test('a NAMED entry never consumes an unnamed backend-labelled buffer', () => {
    reset();
    const es = openStream('sess-1');

    // An unnamed background subagent's early signal is pending...
    es.emit('subagent_activity', { source_agent: 'subagent-deadbeef', kind: 'writing' });
    // ...when a NAMED subagent starts. Its chip must not steal the status.
    T.trackSubagentStart('reviewer', 'task', 'inv-1');
    assert.equal(T.activeSubagents.value.reviewer.activity, null,
        'named entries only consume their own label');

    // The unnamed buffer is still there for the unnamed entry.
    T.trackSubagentStart('subagent', 'other task', 'abcdef0123456789');
    assert.deepEqual(T.activeSubagents.value['subagent-abcdef01'].activity,
        { kind: 'writing', tool: null });
});

test('#1183: clearAllSubagents evicts pending signals (session switch)', () => {
    reset();
    const es = openStream('sess-1');

    es.emit('subagent_activity', { source_agent: 'reviewer', kind: 'writing' });
    T.clearAllSubagents();

    T.trackSubagentStart('reviewer', 'task', 'inv-1');
    assert.equal(T.activeSubagents.value.reviewer.activity, null,
        'a previous session buffered signal must not apply after a clear');
});

test('#1183: a completion evicts the pending signal for that label', () => {
    reset();
    const es = openStream('sess-1');

    // A late / replayed signal for a subagent whose chip is already gone.
    es.emit('subagent_activity', { source_agent: 'reviewer', kind: 'reasoning' });
    // Its completion arrives (entry long removed) — must still evict.
    T.trackSubagentEnd('reviewer', 'done');

    T.trackSubagentStart('reviewer', 'task', 'inv-2');
    assert.equal(T.activeSubagents.value.reviewer.activity, null,
        'a stale signal must never apply to a later re-invocation');
});

test('#1183: pending signals are capped — the least-recently-written label is evicted', () => {
    reset();
    const es = openStream('sess-1');

    // 9 distinct labels against a cap of 8: the oldest ('agent-0') is evicted.
    for (let i = 0; i < 9; i++) {
        es.emit('subagent_activity', { source_agent: 'agent-' + i, kind: 'reasoning' });
    }

    T.trackSubagentStart('agent-0', 'task', 'inv-a');
    assert.equal(T.activeSubagents.value['agent-0'].activity, null,
        'the oldest label was evicted at the cap');
    T.trackSubagentStart('agent-8', 'task', 'inv-b');
    assert.deepEqual(T.activeSubagents.value['agent-8'].activity,
        { kind: 'reasoning', tool: null },
        'a recently-written label is retained and applies normally');
});

// ===========================================================================
// subagentStatusLabel: the chip's user-facing status text.
// ===========================================================================

test('subagentStatusLabel maps activity kinds to the concise labels', () => {
    const running = (activity) => ({ status: 'running', activity });
    assert.equal(subagentStatusLabel(running(null)), 'Starting…');
    assert.equal(subagentStatusLabel(running({ kind: 'reasoning', tool: null })), 'Reasoning…');
    assert.equal(subagentStatusLabel(running({ kind: 'writing', tool: null })), 'Writing…');
    assert.equal(subagentStatusLabel(running({ kind: 'tool_start', tool: 'shell' })), 'Using shell');
    assert.equal(subagentStatusLabel(running({ kind: 'tool_start', tool: 'fs_read' })), 'Using fs_read');
    assert.equal(subagentStatusLabel(running({ kind: 'tool_start', tool: null })), 'Using tool');
    assert.equal(subagentStatusLabel(running({ kind: 'tool_end', tool: null })), 'Running…');
    // Unknown kinds from a newer backend degrade to the generic label.
    assert.equal(subagentStatusLabel(running({ kind: 'future_kind', tool: null })), 'Running…');
});

test('subagentStatusLabel terminal states override activity', () => {
    assert.equal(
        subagentStatusLabel({ status: 'done', activity: { kind: 'writing', tool: null } }),
        'Done');
    assert.equal(
        subagentStatusLabel({ status: 'fail', activity: { kind: 'tool_start', tool: 'shell' } }),
        'Failed');
    assert.equal(subagentStatusLabel(null), '');
});

test('cancelled is terminal: label overrides stale activity (#1189 follow-up P3)', () => {
    // A cancelled background subagent arrives via `subagent_completed` with
    // status "cancelled" (notifications.rs). Without the dedicated branch the
    // chip fell through to its stale activity for the whole auto-removal
    // grace period.
    assert.equal(
        subagentStatusLabel({ status: 'cancelled', activity: { kind: 'writing', tool: null } }),
        'Cancelled',
        'a cancelled chip must not keep reading "Writing…"');
    assert.equal(
        subagentStatusLabel({ status: 'cancelled', activity: null }),
        'Cancelled',
        'a cancelled chip must not fall through to "Starting…"');
});

test('subagent_completed with status "cancelled" ends the chip as Cancelled', () => {
    reset();
    T.trackSubagentStart('reviewer', 'review the diff', 'inv-1');
    const es = openStream('sess-1');

    es.emit('subagent_activity', { source_agent: 'reviewer', kind: 'writing' });
    es.emit('subagent_completed', {
        subagent_name: 'reviewer', status: 'cancelled',
        subagent_session_id: 'sub-sess-1', summary: '',
    });

    const entry = T.activeSubagents.value.reviewer;
    assert.equal(entry.status, 'cancelled',
        'the wire status must land on the entry verbatim');
    assert.equal(subagentStatusLabel(entry), 'Cancelled',
        'the chip must read "Cancelled", not the stale "Writing…"');
});

// ===========================================================================
// Reattach snapshot (#1189 follow-up): the frontend leg of the fix for the
// "chip stuck on Starting… while the subagent is actively writing" bug. On a
// reload / session switch back, history rehydration re-creates the chip with
// no activity, and the backend replays the subagent's CURRENT status to the
// newly-attached session stream as a synthetic `subagent_activity` — which
// must land on the rehydrated chip exactly like a live signal would.
// ===========================================================================

test('a snapshot signal arriving after reload rehydration updates the chip', () => {
    reset();
    // Reload while an unnamed background subagent is mid-write: rehydration
    // re-creates the chip from the persisted invoke_agent row, with no
    // activity — "Starting…".
    T.rehydrateSubagentsFromHistory([
        { id: 'inv-42', type: 'tool', tool: 'invoke_agent',
          params: { task: 'long investigation' }, status: 'done',
          result: { task_id: 'task-1', session_id: 'sub-sess-9' },
          ts: '2026-07-02T10:00:00Z' },
    ]);
    const startKey = 'subagent-' + String('inv-42').slice(0, 8);
    assert.ok(T.activeSubagents.value[startKey], 'rehydrate re-created the chip');
    assert.equal(subagentStatusLabel(T.activeSubagents.value[startKey]), 'Starting…');

    // The fresh stream attaches and the backend replays the status snapshot,
    // tagged with the backend task label (different from the rehydrate key).
    const es = openStream('sess-1');
    es.emit('subagent_activity', { source_agent: 'subagent-deadbeef', kind: 'writing' });

    const migrated = T.activeSubagents.value['subagent-deadbeef'];
    assert.ok(migrated, 'the chip migrated to the backend label');
    assert.equal(subagentStatusLabel(migrated), 'Writing…',
        'the reattach snapshot must lift the chip out of "Starting…"');
});

// ===========================================================================
// End-to-end shape: live status flow for a named subagent.
// ===========================================================================

test('status flow: Starting -> Reasoning -> Using shell -> Writing -> Done', () => {
    reset();
    T.trackSubagentStart('reviewer', 'review the diff', 'inv-1');
    const es = openStream('sess-1');

    const label = () => subagentStatusLabel(
        T.activeSubagents.value.reviewer
        || Object.values(T.activeSubagents.value)[0]);

    assert.equal(label(), 'Starting…');
    es.emit('subagent_activity', { source_agent: 'reviewer', kind: 'reasoning' });
    assert.equal(label(), 'Reasoning…');
    es.emit('subagent_activity', { source_agent: 'reviewer', kind: 'tool_start', tool: 'shell' });
    assert.equal(label(), 'Using shell');
    es.emit('subagent_activity', { source_agent: 'reviewer', kind: 'tool_end' });
    assert.equal(label(), 'Running…');
    es.emit('subagent_activity', { source_agent: 'reviewer', kind: 'writing' });
    assert.equal(label(), 'Writing…');
    es.emit('subagent_completed', {
        subagent_name: 'reviewer', status: 'done',
        subagent_session_id: 'sub-sess-1', summary: 'ok',
    });
    assert.equal(label(), 'Done');
});

// ===========================================================================
// Issue #1 — a FOREGROUND subagent chip must reach terminal on its own
// `tool_end`, independently of the parent run's remaining work.
//
// `subagent_completed` is emitted ONLY for background subagents
// (`run_subagent` fires the completion channel behind `handle.is_background`),
// so `tool_end` for `invoke_agent` is the one and only route a foreground
// chip has to a terminal status. The backend delivers it the moment the
// subagent finishes (pinned wire-side in
// `crates/alms-gateway/src/runs/subagent_chip_timing_tests.rs`), so any
// no-op in this handler is directly visible as "the chip lingers until the
// parent run ends".
//
// The failure mode pinned here: the handler closes the parent's tool ROW by
// `tool_invocation_id` and, when that misses, by a documented "last running
// tool message" fallback — but it then used to re-find the row STRICTLY by
// `tool_invocation_id` to decide whether to end the chip. On the fallback
// path that lookup is empty, so `trackSubagentEnd` was never called and the
// chip stayed `running` for the rest of the parent run.
// ===========================================================================

test('#1: a foreground chip goes terminal on tool_end even when the row id does not match', () => {
    reset();
    const es = openStream('sess-1');

    // Live tool_start: chip + row, both keyed by the invocation id.
    es.emit('tool_start', {
        run_id: 'run-1', tool_invocation_id: 'inv-1', tool: 'invoke_agent',
        params: { name: 'researcher', task: 'review the diff' },
    });
    assert.equal(T.activeSubagents.value.researcher.status, 'running');

    // Mid-run the chat rows are rebuilt (reload / snapshot reconcile / a
    // session switch back into the parent). `mapHistoryMessages` keys a row
    // recovered from the `run_tool_calls` records by the PROVIDER call id,
    // not the invocation id, so the row id no longer matches the live SSE
    // correlator. The chip is untouched — `rehydrateSubagentsFromHistory`
    // skips keys that already exist — so it still carries `inv-1`.
    T.chatMessages.value = [{
        id: 'call_provider_1', type: 'tool', tool: 'invoke_agent',
        params: { name: 'researcher', task: 'review the diff' },
        status: 'running', runId: 'run-1',
    }];

    // The subagent finishes and the parent's invoke_agent call returns.
    es.emit('tool_end', {
        run_id: 'run-1', tool_invocation_id: 'inv-1', ok: true,
        result: { response: 'looks good', session_id: 'sub-sess-1' },
    });

    const entry = T.activeSubagents.value.researcher;
    assert.equal(entry.status, 'done',
        'the chip must be terminal the moment tool_end lands — it is a '
        + 'foreground subagent, so nothing else will ever end it');
    assert.equal(entry.sessionId, 'sub-sess-1',
        'the terminal chip must keep its drill-down session link');
});

test('#1: the chip is terminal before the parent run ends, and run end is not what ends it', () => {
    reset();
    const es = openStream('sess-1');

    es.emit('tool_start', {
        run_id: 'run-1', tool_invocation_id: 'inv-1', tool: 'invoke_agent',
        params: { name: 'researcher', task: 'review the diff' },
    });

    // Control: the parent's own lifecycle must NOT be a terminal route for
    // the chip. A "fix" that simply swept chips at run end would satisfy the
    // assertion above and still be the reported bug.
    es.emit('token_delta', { run_id: 'run-1', delta: 'still working…' });
    es.emit('run_finished', { run_id: 'run-1', ok: true, ts: '2026-09-01T10:00:00Z' });
    assert.equal(T.activeSubagents.value.researcher.status, 'running',
        'a parent run ending must never be what flips a subagent chip');

    // A second parent run (the follow-up / notification turn) ending with the
    // subagent still in flight: same invariant.
    es.emit('run_finished', { run_id: 'run-2', ok: true, ts: '2026-09-01T10:00:01Z' });
    assert.equal(T.activeSubagents.value.researcher.status, 'running');

    // Only the subagent's own tool_end flips it — the parent's remaining work
    // is irrelevant either way.
    es.emit('tool_end', {
        run_id: 'run-1', tool_invocation_id: 'inv-1', ok: true,
        result: { response: 'looks good', session_id: 'sub-sess-1' },
    });
    assert.equal(T.activeSubagents.value.researcher.status, 'done');
});

test('#1: an UNNAMED foreground chip goes terminal via the invocation-id correlator', () => {
    reset();
    const es = openStream('sess-1');

    es.emit('tool_start', {
        run_id: 'run-1', tool_invocation_id: 'inv-77', tool: 'invoke_agent',
        params: { task: 'investigate the flake' },
    });
    // A forwarded activity signal migrates the entry to the backend label, so
    // the chip key no longer resembles anything on the tool row.
    es.emit('subagent_activity', {
        source_agent: 'subagent-cafebabe', kind: 'writing',
        parent_tool_invocation_id: 'inv-77',
    });
    assert.equal(T.activeSubagents.value['subagent-cafebabe'].status, 'running');

    // Same row-id mismatch as above, and an unnamed subagent has no `name`
    // param to fall back on — the invocation-id correlator is all there is.
    T.chatMessages.value = [{
        id: 'call_provider_2', type: 'tool', tool: 'invoke_agent',
        params: { task: 'investigate the flake' },
        status: 'running', runId: 'run-1',
    }];
    es.emit('tool_end', {
        run_id: 'run-1', tool_invocation_id: 'inv-77', ok: false,
        result: { error: 'subagent failed' },
    });

    assert.equal(T.activeSubagents.value['subagent-cafebabe'].status, 'fail',
        'an unnamed foreground chip must resolve through its stored '
        + 'tool_invocation_id, not through the parent chat row');
});

test('#1: a BACKGROUND tool_end still leaves the chip running (it waits for subagent_completed)', () => {
    reset();
    const es = openStream('sess-1');

    es.emit('tool_start', {
        run_id: 'run-1', tool_invocation_id: 'inv-9', tool: 'invoke_agent',
        params: { name: 'researcher', task: 'long investigation', background: true },
    });
    // Background dispatch returns immediately with a task_id. The chip must
    // stay running — the subagent has barely started.
    es.emit('tool_end', {
        run_id: 'run-1', tool_invocation_id: 'inv-9', ok: true,
        result: { task_id: 'task-1', session_id: 'sub-sess-2' },
    });
    assert.equal(T.activeSubagents.value.researcher.status, 'running',
        'a `task_id` result is the background marker: the chip waits for '
        + '`subagent_completed`');

    es.emit('subagent_completed', {
        subagent_name: 'researcher', status: 'done',
        subagent_session_id: 'sub-sess-2', summary: 'all done',
        tool_invocation_id: 'inv-9',
    });
    assert.equal(T.activeSubagents.value.researcher.status, 'done');
});
