// Node-side tests for the subagent live-activity panel fix
// (issue #1149 — "subagent live panel doesn't show live activity during a
// running subagent").
//
// The bug: while a subagent (spawned via invoke_agent) is running, the
// SubagentBar panel sat on "Waiting for activity..." until the subagent
// finished. Subagents DO stream — the parent's session SSE stream carries the
// subagent's `reasoning_delta` events tagged with `source_agent` — but the
// frontend's `reasoning_delta` handler in `use-session-stream.js` early-returned
// on `source_agent` (to keep subagent reasoning out of the PARENT's main chat /
// reasoning view) without ever surfacing it in the panel.
//
// The fix tees a subagent-tagged `reasoning_delta` into that subagent's
// `activeSubagents` entry (`liveActivity`) via `trackSubagentReasoning`, while
// STILL returning before the parent-rendering path — so the panel lights up live
// but the parent's reasoning trace stays clean (the #1170 / `get_run_reasoning`
// invariant).
//
// Pinned regression targets (#1149):
//   1. A subagent-tagged `reasoning_delta` surfaces on the matching panel entry
//      (`activeSubagents[key].liveActivity`).
//   2. It does NOT leak into the parent's main-view reasoning — neither the
//      parent chat messages (`chatMessages`) nor the DM reasoning collapsible
//      (`dmThinkingBuffers`).
//   3. A parent-tagged (no `source_agent`) `reasoning_delta` keeps reaching the
//      parent reasoning view and never touches the panel.
//   4. The real `trackSubagentReasoning` key-migration + bounding behaviour.
//
// Strategy: like `dm-stream-rendering.test.mjs`, `use-session-stream.js` is a
// browser ES module with many imports + EventSource/localStorage/rAF reliance,
// so we read its source, rewrite the top-level imports with a self-contained
// stub block, and drive the real exported `openSessionStream` through a fake
// EventSource. The crucial difference: the subagent stubs are NOT inert — the
// stub block `import`s the REAL `state/subagents.js` (also rewritten for Node)
// so `trackSubagentReasoning` / `activeSubagents` run end-to-end and the panel
// state is observable.

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

// ---------------------------------------------------------------------------
// Load the REAL `state/subagents.js` under Node, rewriting its two top-level
// imports (signal from deps.js, activeSessionId from sessions.js). Mirrors the
// loader in `subagents-rehydrate.test.mjs`. We need the real module so the
// stream handler's tee writes through the genuine `trackSubagentReasoning` and
// the panel state surfaces back through the genuine `activeSubagents` signal.
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
            + 'update subagent-live-panel.test.mjs if the import shape changed.'
        );
    }
    const sessionsImportRe =
        /^import\s+\{\s*activeSessionId\s*\}\s+from\s+['"][^'"]+['"];?\s*$/m;
    if (!sessionsImportRe.test(src)) {
        throw new Error(
            'subagents.js: expected a top-level `import { activeSessionId } from ...` line — '
            + 'update subagent-live-panel.test.mjs if the import shape changed.'
        );
    }

    const stubbed = src
        .replace(signalImportRe, SIGNAL_STUB)
        .replace(sessionsImportRe,
            'const activeSessionId = { get value() { return null; }, set value(_) {} };');

    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'alms-sa-real-'));
    const tmpFile = path.join(tmpDir, 'subagents.mjs');
    fs.writeFileSync(tmpFile, stubbed, 'utf8');
    return url.pathToFileURL(tmpFile).href;
}

const realSubagentsUrl = loadRealSubagentsAsTempFile();

// ---------------------------------------------------------------------------
// Stub block injected in place of the stream module's top-level imports. The
// subagent functions are the REAL ones (imported from the rewritten temp
// module above); everything else the handlers touch but we don't assert on is a
// minimal stub. `dmThinkingBuffers` is exported by the stream module itself.
// ---------------------------------------------------------------------------
const STUB_PRELUDE = `
import {
    trackSubagentStart,
    trackSubagentEnd,
    trackSubagentTool,
    trackSubagentReasoning,
    findSubagentByToolInvocationId,
    findSubagentBySessionId,
    setSubagentSessionId,
    activeSubagents,
    clearAllSubagents,
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
function clearPendingMessage() {}
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
    activeAgent, dmPeer, activeSubagents, trackSubagentStart, trackSubagentReasoning,
    trackSubagentEnd, clearAllSubagents,
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
            + 'update subagent-live-panel.test.mjs if the import shape changed.'
        );
    }

    const stubbed = STUB_PRELUDE + '\n' + src;

    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'alms-sa-stream-'));
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
globalThis.requestAnimationFrame = (fn) => { fn(); return 1; };
globalThis.cancelAnimationFrame = () => {};

const mod = await loadStreamModule();
const T = mod.__test;

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
    // Clears entries AND the #1183 pending early-reasoning buffers (no cross-test bleed).
    T.clearAllSubagents();
    mod.dmThinkingBuffers.value = new Map();
    mod.closeSessionStream();
}

// ===========================================================================
// Integration: a subagent-tagged reasoning_delta on the parent stream surfaces
// on the panel entry and does NOT leak into the parent's main-view reasoning.
// ===========================================================================

test('#1149: subagent reasoning_delta surfaces on the panel entry (named subagent)', () => {
    reset();
    // A named subagent "reviewer" is running — register it as the live
    // tool_start path would (key === display name for named subagents).
    T.trackSubagentStart('reviewer', 'review the diff', 'inv-1');
    const es = openStream('sess-1');

    // The parent stream forwards the subagent's reasoning, tagged with
    // source_agent === the backend label (the agent name for named subagents).
    es.emit('reasoning_delta', { source_agent: 'reviewer', text: 'Analysing the change set' });

    const entry = T.activeSubagents.value['reviewer'];
    assert.ok(entry, 'reviewer panel entry exists');
    assert.equal(entry.liveActivity, 'Analysing the change set',
        'the subagent reasoning is teed onto the panel entry liveActivity');
});

test('#1149: subagent reasoning_delta does NOT leak into the parent main-view reasoning', () => {
    reset();
    T.trackSubagentStart('reviewer', 'review the diff', 'inv-1');
    const es = openStream('sess-1');

    es.emit('reasoning_delta', { source_agent: 'reviewer', text: 'private subagent thinking' });

    // Parent main chat: no agent message / reasoning bubble may have been
    // created from the subagent's reasoning (the #1170 invariant — subagent
    // reasoning belongs only in the panel, never the parent reasoning trace).
    const parentReasoningLeaked = T.chatMessages.value.some(
        m => m.type === 'agent' && (m.reasoning || '').includes('private subagent thinking')
    );
    assert.equal(parentReasoningLeaked, false,
        'subagent reasoning must NOT appear in any parent agent message reasoning');
    assert.equal(T.chatMessages.value.length, 0,
        'subagent reasoning must NOT create any parent chat message at all');

    // DM collapsible: the subagent reasoning must never enter the DM reasoning
    // buffer either (the other surface the parent reasoning could land in).
    for (const [, text] of mod.dmThinkingBuffers.value) {
        assert.ok(!text.includes('private subagent thinking'),
            'subagent reasoning must NOT enter the DM reasoning collapsible');
    }
});

test('#1149: parent reasoning_delta (no source_agent) reaches the parent view and NOT the panel', () => {
    reset();
    // A subagent is running, but the delta below is the PARENT agent's own
    // reasoning (no source_agent) — it must render in the parent reasoning view
    // and must not touch the subagent panel.
    T.trackSubagentStart('reviewer', 'review the diff', 'inv-1');
    const es = openStream('sess-1');

    es.emit('reasoning_delta', { run_id: 'run-parent', text: 'parent agent reasoning' });

    // Parent main-view: a reasoning-bearing agent message was created.
    const parentMsg = T.chatMessages.value.find(
        m => m.type === 'agent' && (m.reasoning || '').includes('parent agent reasoning')
    );
    assert.ok(parentMsg, 'parent reasoning renders in the parent main-view (unchanged behaviour)');

    // The subagent panel entry must be untouched by the parent's reasoning.
    const entry = T.activeSubagents.value['reviewer'];
    assert.ok(entry, 'reviewer entry still present');
    assert.equal(entry.liveActivity || '', '',
        'a parent (non-source_agent) reasoning delta must NOT bleed into the subagent panel');
});

test('#1149: unnamed subagent reasoning_delta migrates the panel key and surfaces (backend label)', () => {
    reset();
    // Unnamed subagent: tool_start registered it under
    // "subagent-{toolInvocationId_prefix}", but the backend forwards events
    // labelled with "subagent-{task_id_prefix}" — a DIFFERENT id. The tee must
    // migrate the entry to the backend key (mirroring trackSubagentTool) so the
    // reasoning lands on the right chip.
    T.trackSubagentStart('subagent', 'some task', 'abcdef0123456789');
    const startKey = 'subagent-' + 'abcdef01'; // toolInvocationId prefix
    assert.ok(T.activeSubagents.value[startKey], 'entry starts under the tool-id-prefixed key');

    const es = openStream('sess-1');
    const backendLabel = 'subagent-deadbeef'; // task_id prefix label
    es.emit('reasoning_delta', { source_agent: backendLabel, text: 'thinking unnamed' });

    assert.equal(T.activeSubagents.value[startKey], undefined,
        'the start-time key is migrated away to the backend-assigned label');
    const migrated = T.activeSubagents.value[backendLabel];
    assert.ok(migrated, 'entry now lives under the backend-assigned label');
    assert.equal(migrated.liveActivity, 'thinking unnamed',
        'unnamed subagent reasoning surfaces on the migrated panel entry');
});

test('#1149: a reasoning_delta for a gone/unknown subagent is a no-op (no chip resurrected)', () => {
    reset();
    const es = openStream('sess-1');

    // No subagent registered — a late forwarded delta must not create an entry.
    es.emit('reasoning_delta', { source_agent: 'reviewer', text: 'late delta' });

    assert.deepEqual(T.activeSubagents.value, {},
        'a forwarded delta for an unknown subagent must not resurrect/create a chip');
});

// ===========================================================================
// Unit: the real trackSubagentReasoning bounding behaviour.
// ===========================================================================

test('#1149: liveActivity is bounded to the most recent tail (long reasoning trace)', () => {
    reset();
    T.trackSubagentStart('reviewer', 'task', 'inv-1');

    // Feed more than the 2000-char cap in chunks; only the tail must survive.
    const chunk = 'x'.repeat(500);
    const tailMarker = 'END_OF_THOUGHT';
    for (let i = 0; i < 6; i++) T.trackSubagentReasoning('reviewer', chunk); // 3000 chars
    T.trackSubagentReasoning('reviewer', tailMarker);

    const entry = T.activeSubagents.value['reviewer'];
    assert.ok(entry.liveActivity.length <= 2000,
        'liveActivity is bounded to the cap (<= 2000 chars)');
    assert.ok(entry.liveActivity.endsWith(tailMarker),
        'the most-recent reasoning (the tail) is preserved');
});

test('#1149: an empty reasoning delta is ignored (no spurious panel write)', () => {
    reset();
    T.trackSubagentStart('reviewer', 'task', 'inv-1');
    const before = T.activeSubagents.value['reviewer'];

    T.trackSubagentReasoning('reviewer', '');

    assert.equal(T.activeSubagents.value['reviewer'], before,
        'an empty delta is a no-op — the entry object reference is unchanged');
});

// ===========================================================================
// #1183: an early background-subagent reasoning_delta (arriving before the
// entry-creating tool_start) must be buffered and flushed, not dropped.
// ===========================================================================

test('#1183: reasoning_delta arriving BEFORE the entry exists is buffered, not dropped (named)', () => {
    reset();
    const es = openStream('sess-1');

    // The subagent's first reasoning delta races ahead of the parent's
    // tool_start — no entry exists yet.
    es.emit('reasoning_delta', { source_agent: 'reviewer', text: 'early thinking. ' });
    assert.deepEqual(T.activeSubagents.value, {},
        'the early delta must NOT create a chip on its own');

    // The parent's tool_start (invoke_agent) handler now creates the entry.
    T.trackSubagentStart('reviewer', 'review the diff', 'inv-1');
    const entry = T.activeSubagents.value['reviewer'];
    assert.ok(entry, 'reviewer panel entry exists');
    assert.equal(entry.liveActivity, 'early thinking. ',
        'the buffered early reasoning is flushed into the new entry (not dropped)');

    // Subsequent live deltas append after the flushed prefix, in order.
    es.emit('reasoning_delta', { source_agent: 'reviewer', text: 'more thinking' });
    assert.equal(T.activeSubagents.value['reviewer'].liveActivity,
        'early thinking. more thinking',
        'later deltas append after the flushed early reasoning');
});

test('#1183: early reasoning for an UNNAMED subagent flushes in order via the key migration', () => {
    reset();
    const es = openStream('sess-1');
    const backendLabel = 'subagent-deadbeef'; // task_id-prefix label

    // Early delta under the backend label — no entry yet.
    es.emit('reasoning_delta', { source_agent: backendLabel, text: 'early ' });
    assert.deepEqual(T.activeSubagents.value, {}, 'no chip created by the early delta');

    // tool_start registers the entry under the tool-id-prefixed key (which
    // differs from the backend label), so the flush cannot happen at start
    // time — it happens on the NEXT resolved delta after the key migration.
    T.trackSubagentStart('subagent', 'some task', 'abcdef0123456789');
    es.emit('reasoning_delta', { source_agent: backendLabel, text: 'later' });

    const entry = T.activeSubagents.value[backendLabel];
    assert.ok(entry, 'entry migrated to the backend-assigned label');
    assert.equal(entry.liveActivity, 'early later',
        'the buffered early reasoning precedes the live delta (order preserved)');
});

test('#1183: the early-reasoning buffer is tail-bounded like liveActivity', () => {
    reset();
    const es = openStream('sess-1');

    const chunk = 'y'.repeat(500);
    for (let i = 0; i < 6; i++) {
        es.emit('reasoning_delta', { source_agent: 'reviewer', text: chunk }); // 3000 chars
    }
    es.emit('reasoning_delta', { source_agent: 'reviewer', text: 'TAIL_MARK' });

    T.trackSubagentStart('reviewer', 'task', 'inv-1');
    const entry = T.activeSubagents.value['reviewer'];
    assert.ok(entry.liveActivity.length <= 2000,
        'the flushed buffer respects the liveActivity cap (<= 2000 chars)');
    assert.ok(entry.liveActivity.endsWith('TAIL_MARK'),
        'the most-recent buffered reasoning (the tail) is preserved');
});

test('#1183: clearAllSubagents evicts pending buffers (session switch)', () => {
    reset();
    const es = openStream('sess-1');

    es.emit('reasoning_delta', { source_agent: 'reviewer', text: 'from the previous session' });
    T.clearAllSubagents();

    T.trackSubagentStart('reviewer', 'task', 'inv-1');
    assert.equal(T.activeSubagents.value['reviewer'].liveActivity, '',
        'a previous session buffered reasoning must not flush after a clear');
});

test('#1183: a completion evicts the pending buffer for that label (no stale flush into a re-invocation)', () => {
    reset();
    const es = openStream('sess-1');

    // A late / replayed delta for a subagent whose chip is already gone.
    es.emit('reasoning_delta', { source_agent: 'reviewer', text: 'stale replayed delta' });
    // Its completion arrives (entry long removed) — must still evict.
    T.trackSubagentEnd('reviewer', 'done');

    T.trackSubagentStart('reviewer', 'task', 'inv-2');
    assert.equal(T.activeSubagents.value['reviewer'].liveActivity, '',
        'a stale buffer must never flush into a later re-invocation');
});

test('#1183: pending buffers are capped — the least-recently-written label is evicted', () => {
    reset();
    const es = openStream('sess-1');

    // 9 distinct labels against a cap of 8: the oldest ('agent-0') is evicted.
    for (let i = 0; i < 9; i++) {
        es.emit('reasoning_delta', { source_agent: 'agent-' + i, text: 'buffered ' + i });
    }

    T.trackSubagentStart('agent-0', 'task', 'inv-a');
    assert.equal(T.activeSubagents.value['agent-0'].liveActivity, '',
        'the oldest label was evicted at the cap');
    T.trackSubagentStart('agent-8', 'task', 'inv-b');
    assert.equal(T.activeSubagents.value['agent-8'].liveActivity, 'buffered 8',
        'a recently-written label is retained and flushes normally');
});
