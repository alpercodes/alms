// Node-side tests for the DM-rendering fixes in `static/ui/hooks/use-session-stream.js`
// (issue #1154, frontend slice B8 / B9-live / B10).
//
// `use-session-stream.js` is a browser ES module with 16 imports (deps.js,
// many state signal modules) plus runtime reliance on `EventSource`,
// `localStorage`, and `requestAnimationFrame`. None of that is reachable from
// Node. Mirroring the loader pattern used by `history.test.mjs`, this test
// reads the module source as text, rewrites its top-level imports with a
// single self-contained stub block (a tiny signal implementation + minimal
// function stubs), installs fake browser globals, and then loads the rewritten
// module via dynamic import(). We then drive the real exported
// `openSessionStream` through a fake EventSource so the actual event handlers
// run against our stub signals.
//
// Pinned regression targets (all #1154):
//   B8  — DM thinking/reasoning deltas must bucket by the event's own
//         `run_id`, not the mutable `activeRunId`, so two overlapping DM runs
//         can't cross-contaminate each other's collapsible.
//   B9  — the live `tool_start` DM gate must not depend on `activeSession`
//         resolution timing: a tool whose run already owns a `dm_reasoning`
//         block must group into it even when activeSession hasn't resolved.
//   B10 — duplicate `dm_conversation_ended` events must render ONE banner.

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

// ---------------------------------------------------------------------------
// Stub block injected in place of the module's 16 top-level imports.
// A minimal synchronous signal (no batching needed — `batch(fn)` just runs
// `fn()`), the chat-action writers, and inert stubs for everything the
// handlers touch but we don't assert on.
// ---------------------------------------------------------------------------
const STUB_PRELUDE = `
function signal(initial) {
    return { value: initial };
}
function batch(fn) { return fn(); }

// chat state
const chatMessages = signal([]);
let __msgSeq = 0;
function nextMsgId() { return 'msg-' + (++__msgSeq); }

// chat-action writers (mirror state/chat-actions.js exactly)
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

// subagents (inert)
function trackSubagentStart() {}
function trackSubagentEnd() {}
function trackSubagentTool() {}
function findSubagentByToolInvocationId() { return null; }
function findSubagentBySessionId() { return null; }
function setSubagentSessionId() {}
const activeSubagents = signal({});

// agent-status (inert beyond what handlers call)
const agentPhase = signal({ phase: null, detail: null });
function setAgentPhase() {}
function clearAgentPhase() {}
function setDmContext() {}
function revertPhase() {}
const dmPeer = signal(null);

// queue
const messageQueue = signal([]);

// sessions — the signals B8/B9/B10 read
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

// Test hooks — exported so the harness can reach the live signals.
export const __test = { signal, chatMessages, activeRunId, activeSession, dmParticipants };
`;

/**
 * Load `use-session-stream.js` under Node with all 16 imports replaced by the
 * stub prelude above. Returns the live module namespace plus the `__test`
 * handle exposing the stub signals.
 */
async function loadStreamModule() {
    let src = fs.readFileSync(STREAM_JS_PATH, 'utf8');

    // Strip every top-level import (single-line and multi-line block forms).
    // Multi-line: `import {\n ... \n} from '...';`
    src = src.replace(/^import\s+\{[\s\S]*?\}\s+from\s+['"][^'"]+['"];?\s*$/gm, '');
    // Single-line default/namespace imports (none expected, but be safe).
    src = src.replace(/^import\s+[^{][^;]*?from\s+['"][^'"]+['"];?\s*$/gm, '');

    // Sanity: the import surface must be gone, or the handlers would throw on
    // an undefined reference and the test would mislead. Fail loudly instead.
    if (/^\s*import\s/m.test(src)) {
        throw new Error(
            'use-session-stream.js: a top-level import survived the rewrite — '
            + 'update dm-stream-rendering.test.mjs if the import shape changed.'
        );
    }

    const stubbed = STUB_PRELUDE + '\n' + src;

    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'alms-stream-test-'));
    const tmpFile = path.join(tmpDir, 'stream.mjs');
    fs.writeFileSync(tmpFile, stubbed, 'utf8');
    return await import(url.pathToFileURL(tmpFile).href);
}

// ---------------------------------------------------------------------------
// Fake browser globals.
// ---------------------------------------------------------------------------

/** Minimal EventSource that records listeners and lets tests fire events. */
class FakeEventSource {
    constructor(url) {
        this.url = url;
        this.readyState = 1;
        this._listeners = new Map();
        FakeEventSource.last = this;
    }
    addEventListener(type, fn) {
        if (!this._listeners.has(type)) this._listeners.set(type, []);
        this._listeners.get(type).push(fn);
    }
    close() { this.readyState = 2; }
    /** Dispatch an SSE event to all registered handlers. */
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

/** Open a stream and return the FakeEventSource it created. */
function openStream(sessionId) {
    mod.openSessionStream(sessionId);
    return FakeEventSource.last;
}

/** Reset all stub signals between tests. */
function reset() {
    T.chatMessages.value = [];
    T.activeRunId.value = null;
    T.activeSession.value = null;
    T.dmParticipants.value = [];
    mod.dmThinkingBuffers.value = new Map();
}

// ---------------------------------------------------------------------------
// B8: thinking/reasoning deltas bucket by the event's own run_id.
// ---------------------------------------------------------------------------

test('#1154 B8: token_delta buckets thinking text by event run_id, not activeRunId', () => {
    reset();
    T.activeSession.value = { session_type: 'dm' };
    const es = openStream('sess-1');

    // Two overlapping DM runs. activeRunId points at run-2 (a later
    // run_created overwrote it), but a delta for run-1 is still arriving.
    T.activeRunId.value = 'run-2';
    es.emit('token_delta', { run_id: 'run-1', delta: 'run-1 thinking ' });
    es.emit('token_delta', { run_id: 'run-2', delta: 'run-2 thinking ' });
    es.emit('token_delta', { run_id: 'run-1', delta: 'more run-1' });

    const buffers = mod.dmThinkingBuffers.value;
    assert.equal(buffers.get('run-1'), 'run-1 thinking more run-1',
        'run-1 deltas must land in run-1 bucket despite activeRunId=run-2');
    assert.equal(buffers.get('run-2'), 'run-2 thinking ',
        'run-2 deltas stay in run-2 bucket');
});

test('#1154 B8: reasoning_delta buckets by event run_id', () => {
    reset();
    T.activeSession.value = { session_type: 'dm' };
    const es = openStream('sess-1');

    T.activeRunId.value = 'run-2';
    es.emit('reasoning_delta', { run_id: 'run-1', text: 'reasoning A' });
    es.emit('reasoning_delta', { run_id: 'run-2', text: 'reasoning B' });

    const buffers = mod.dmThinkingBuffers.value;
    assert.equal(buffers.get('run-1'), 'reasoning A');
    assert.equal(buffers.get('run-2'), 'reasoning B');
});

test('#1154 B8: falls back to activeRunId when the event omits run_id (legacy)', () => {
    reset();
    T.activeSession.value = { session_type: 'dm' };
    const es = openStream('sess-1');

    T.activeRunId.value = 'run-legacy';
    es.emit('token_delta', { delta: 'no run id here' });

    assert.equal(mod.dmThinkingBuffers.value.get('run-legacy'), 'no run id here',
        'legacy backends without run_id still bucket under activeRunId');
});

// ---------------------------------------------------------------------------
// B9 (live): tool_start groups into a dm_reasoning block even when
// activeSession has NOT resolved yet, as long as the run already owns a block.
// ---------------------------------------------------------------------------

test('#1154 B9: tool_start groups into the run block when activeSession is unresolved', () => {
    reset();
    // Simulate the attach/reconnect race: activeSession is still null...
    T.activeSession.value = null;
    // ...but a dm_reasoning block for run-X already exists (created earlier
    // by run_created/run_started before the reconnect, or replayed first).
    T.chatMessages.value = [{
        id: 'b1', type: 'dm_reasoning', runId: 'run-X',
        agentName: 'iris', thinkingText: '', tools: [], status: 'running', isLive: true,
    }];
    const es = openStream('sess-1');

    es.emit('tool_start', { run_id: 'run-X', tool: 'fs_read', params: { path: 'a' }, tool_invocation_id: 'inv-1' });

    const msgs = T.chatMessages.value;
    // No standalone tool row should have been appended.
    const standalone = msgs.filter(m => m.type === 'tool');
    assert.equal(standalone.length, 0, 'tool must NOT leak out as a standalone row during the race');
    const block = msgs.find(m => m.type === 'dm_reasoning' && m.runId === 'run-X');
    assert.ok(block, 'block still present');
    assert.equal(block.tools.length, 1, 'tool grouped into the run block');
    assert.equal(block.tools[0].tool, 'fs_read');
});

test('#1154 B9: non-DM tool_start with no matching block still renders standalone', () => {
    reset();
    // Genuine non-DM session, no dm_reasoning blocks anywhere.
    T.activeSession.value = { session_type: 'chat' };
    const es = openStream('sess-1');

    es.emit('tool_start', { run_id: 'run-Y', tool: 'shell', params: { command: 'ls' }, tool_invocation_id: 'inv-2' });

    const msgs = T.chatMessages.value;
    const standalone = msgs.filter(m => m.type === 'tool');
    assert.equal(standalone.length, 1, 'non-DM tool renders as a standalone row (unchanged)');
    assert.equal(msgs.some(m => m.type === 'dm_reasoning'), false, 'no DM block fabricated on a chat session');
});

test('#1154 B9: tool_end flips the grouped tool to done when activeSession is unresolved (S5)', () => {
    reset();
    // Same attach/reconnect race as the tool_start case: activeSession null,
    // but a dm_reasoning block for run-X already exists.
    T.activeSession.value = null;
    T.chatMessages.value = [{
        id: 'b1', type: 'dm_reasoning', runId: 'run-X',
        agentName: 'iris', thinkingText: '', tools: [], status: 'running', isLive: true,
    }];
    const es = openStream('sess-1');

    es.emit('tool_start', { run_id: 'run-X', tool: 'fs_read', params: { path: 'a' }, tool_invocation_id: 'inv-1' });
    es.emit('tool_end', { run_id: 'run-X', ok: true, result: { ok: true }, tool_invocation_id: 'inv-1' });

    const msgs = T.chatMessages.value;
    // No standalone tool row may have leaked out on either tool_start OR tool_end.
    const standalone = msgs.filter(m => m.type === 'tool');
    assert.equal(standalone.length, 0,
        'tool_end must NOT spawn a standalone row when the tool lives inside the DM block');
    const block = msgs.find(m => m.type === 'dm_reasoning' && m.runId === 'run-X');
    assert.ok(block, 'block still present');
    assert.equal(block.tools.length, 1, 'tool stays grouped in the run block');
    assert.equal(block.tools[0].status, 'done',
        'tool_end must flip the grouped tool to done via the run_id-aware DM gate');
});

// ---------------------------------------------------------------------------
// B10: duplicate dm_conversation_ended events render ONE banner.
// ---------------------------------------------------------------------------

test('#1154 B10: duplicate dm_conversation_ended (same context_id) yields one banner', () => {
    reset();
    T.activeSession.value = { session_type: 'dm' };
    const es = openStream('sess-1');

    const payload = { peer: 'bob', reason: 'depth_exceeded', context_id: 'dm:alice:bob' };
    es.emit('dm_conversation_ended', payload);
    es.emit('dm_conversation_ended', payload); // intentional backend duplicate

    const banners = T.chatMessages.value.filter(m => m.type === 'dm_ended');
    assert.equal(banners.length, 1, 'a single conversation-end must render exactly one banner');
    assert.equal(banners[0].peer, 'bob');
    assert.equal(banners[0].contextId, 'dm:alice:bob');
});

test('#1154 B10: dedupes on peer+reason when context_id is absent (legacy backend)', () => {
    reset();
    T.activeSession.value = { session_type: 'dm' };
    const es = openStream('sess-1');

    es.emit('dm_conversation_ended', { peer: 'bob', reason: 'ignored' });
    es.emit('dm_conversation_ended', { peer: 'bob', reason: 'ignored' });

    const banners = T.chatMessages.value.filter(m => m.type === 'dm_ended');
    assert.equal(banners.length, 1, 'legacy events without context_id still dedupe on peer+reason');
});

test('#1154 B10: a second conversation with IDENTICAL context_id/peer/reason still renders its own banner (positional dedupe)', () => {
    reset();
    T.activeSession.value = { session_type: 'dm' };
    const es = openStream('sess-1');

    // `context_id` is PAIR-stable ("dm:<a>:<b>"), NOT per-conversation: a
    // persistent DM session holds many lifecycles, all carrying the same
    // context_id, peer, and (from a tiny label set) reason. The dedupe is
    // positional — it must only suppress a banner that is the TRAILING state
    // with no DM activity between it and the prior end. Here a brand-new DM
    // run (`run_created` inserts a live `dm_reasoning` block — exactly what
    // begins a new conversation on the wire) sits between the two ends, so
    // the second end is legitimate and MUST render.
    const payload = { peer: 'bob', reason: 'depth_exceeded', context_id: 'dm:alice:bob' };
    es.emit('dm_conversation_ended', payload);           // first conversation ends
    es.emit('run_created', { run_id: 'run-2', source: 'peer:bob' }); // NEW conversation begins
    es.emit('dm_conversation_ended', payload);           // second, genuinely separate end

    const msgs = T.chatMessages.value;
    assert.ok(
        msgs.some(m => m.type === 'dm_reasoning' && m.runId === 'run-2'),
        'the intervening run_created must have inserted a dm_reasoning block',
    );
    const banners = msgs.filter(m => m.type === 'dm_ended');
    assert.equal(banners.length, 2,
        'a real second end after intervening DM activity must render its own banner '
        + 'even though context_id/peer/reason are identical to the first');
});

test('#1154 B10: a genuinely different conversation (different peer) still renders its own banner', () => {
    reset();
    T.activeSession.value = { session_type: 'dm' };
    const es = openStream('sess-1');

    es.emit('dm_conversation_ended', { peer: 'bob', reason: 'depth_exceeded', context_id: 'dm:alice:bob' });
    es.emit('dm_conversation_ended', { peer: 'carol', reason: 'ignored', context_id: 'dm:alice:carol' });

    const banners = T.chatMessages.value.filter(m => m.type === 'dm_ended');
    assert.equal(banners.length, 2, 'distinct conversations must each render a banner');
});

// ---------------------------------------------------------------------------
// C1 (Tim re-review of #1155): the positional dedupe activity-break must stop
// on EVERY real chatMessages entry type, not just `dm_reasoning`. A delivered
// peer DM message is stored as `type: 'agent'` (both the live `dm_message`
// handler and the history.js reload mapper), and a queued response run inserts
// only a `type: 'thinking'` indicator (the live `dm_reasoning` block is
// deferred to `run_started`). Breaking only on `dm_reasoning` left those
// entries invisible to the scan, so a legitimate second end after them was
// wrongly suppressed by the pair-stable context_id arm.
//
// Pre-fix (HEAD 49edbb5) this asserts ONE banner and FAILS; post-fix it
// asserts TWO and passes.
// ---------------------------------------------------------------------------

test('#1155 C1: legitimate second end is NOT suppressed by an intervening dm_message + queued-run thinking indicator', () => {
    reset();
    T.activeSession.value = { session_type: 'dm' };
    const es = openStream('sess-1');

    const payload = { peer: 'bob', reason: 'depth_exceeded', context_id: 'dm:alice:bob' };

    // 1. First conversation ends → banner #1.
    es.emit('dm_conversation_ended', payload);

    // 2. The peer sends a NEW message → stored as type 'agent' (NOT
    //    'dm_message'). The old break check ignored this entirely.
    es.emit('dm_message', { message: 'hey again', from_agent: 'bob', from_agent_id: 'agent-bob' });

    // 3. The response run is QUEUED behind another run, so run_created appends
    //    only a `type: 'thinking'` indicator — the real dm_reasoning block is
    //    deferred to run_started. The old break check ignored this too.
    es.emit('run_created', { run_id: 'run-2', source: 'peer:bob', queued_behind: 1 });

    // 4. The operator clicks Stop → a legitimate user_cancelled end fires.
    es.emit('dm_conversation_ended', { peer: 'bob', reason: 'user_cancelled', context_id: 'dm:alice:bob' });

    const msgs = T.chatMessages.value;
    // Confirm the intervening activity is the kind the OLD break check missed:
    // an 'agent' DM message and a 'thinking' queued indicator, with NO
    // dm_reasoning block between the two ends.
    assert.ok(
        msgs.some(m => m.type === 'agent' && m.fromAgent === 'bob'),
        'the peer dm_message must be stored as a type "agent" entry',
    );
    assert.ok(
        msgs.some(m => m.type === 'thinking' && m.runId === 'run-2'),
        'the queued response run must insert a type "thinking" indicator (no dm_reasoning block)',
    );
    assert.equal(
        msgs.some(m => m.type === 'dm_reasoning'), false,
        'a queued run must NOT have inserted a dm_reasoning block — that arm is deferred to run_started',
    );

    const banners = msgs.filter(m => m.type === 'dm_ended');
    assert.equal(banners.length, 2,
        'the second, legitimate end after a peer message + queued-run thinking indicator '
        + 'must render its own banner and NOT be suppressed by the pair-stable context_id');
    assert.equal(banners[1].reason, 'cancelled by user',
        'the second banner carries the distinct user_cancelled reason label');
});
