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

    const stubbed = src
        .replace(signalImportRe, SIGNAL_STUB)
        .replace(sessionsImportRe,
            'const activeSessionId = { get value() { return null; }, set value(_) {} };');

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
    activeAgent, dmPeer, activeSubagents, trackSubagentStart,
    trackSubagentActivity, trackSubagentEnd, clearAllSubagents,
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

test('a subagent_activity without source_agent is ignored (defensive)', () => {
    reset();
    T.trackSubagentStart('reviewer', 'task', 'inv-1');
    const es = openStream('sess-1');

    es.emit('subagent_activity', { kind: 'reasoning' });
    assert.equal(T.activeSubagents.value.reviewer.activity, null,
        'an unlabelled signal cannot be routed and must be dropped');
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
