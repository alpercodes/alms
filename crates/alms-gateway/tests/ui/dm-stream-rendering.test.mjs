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
function trackSubagentActivity() {}
function findSubagentByToolInvocationId() { return null; }
function findSubagentBySessionId() { return null; }
function setSubagentSessionId() {}
const activeSubagents = signal({});

// agent-status (inert beyond what handlers call)
const agentPhase = signal({ phase: null, detail: null });
function setAgentPhase() {}
// Observable so tests can assert the phase-clear runs even when the live
// dm_ended banner is suppressed (#1215/#1218).
const clearAgentPhaseCalls = signal(0);
function clearAgentPhase() { clearAgentPhaseCalls.value++; }
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
export const __test = { signal, chatMessages, activeRunId, activeSession, dmParticipants, activeAgent, dmPeer, clearAgentPhaseCalls };
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

/**
 * Simulate a same-session EventSource reconnect: `openSessionStream` is called
 * again for the SAME session id WITHOUT a fresh `sealedReasoningRunIds` set —
 * exactly what the auto-backoff `onerror` reopen and the manual
 * `reconnectSessionStream` paths do (`{ lastEventId }` only). The module's
 * internal `closeSessionStream()` clears the per-run buffers; the reconnect
 * carry-over must preserve any not-yet-promoted pending DM reply text. Returns
 * the new FakeEventSource so the caller drives post-reconnect events on it.
 *
 * NOTE: `chatMessages` / `activeSession` are NOT reset here — they survive a
 * reconnect in production (history stays rendered, session metadata stays
 * resolved), so leaving the stub signals intact mirrors the real reconnect.
 */
function reconnectStream(sessionId) {
    mod.openSessionStream(sessionId);
    return FakeEventSource.last;
}

/** Reset all stub signals between tests. */
function reset() {
    T.chatMessages.value = [];
    T.activeRunId.value = null;
    T.activeSession.value = null;
    T.dmParticipants.value = [];
    T.activeAgent.value = null;
    T.dmPeer.value = null;
    T.clearAgentPhaseCalls.value = 0;
    mod.dmThinkingBuffers.value = new Map();
    // Fully tear down any open stream so module-private per-run state does not
    // leak between tests. `closeSessionStream` clears `dmPendingReplyBuffers`
    // AND nulls the module-scope `activeStreamSessionId`. The latter matters
    // since the #1157 reconnect carry-over recovers `dmPendingReplyBuffers`
    // BEFORE the internal close whenever the next open is for the SAME session
    // id — without this teardown, a test that left pending text under a session
    // id reused by the next test would have that stale text carried into the
    // next test (the carry-over firing on what looks like a same-session
    // reconnect). Nulling `activeStreamSessionId` here makes every subsequent
    // `openStream(...)` look like a session SWITCH (no carry), modelling the
    // fresh-page-load isolation each test assumes.
    mod.closeSessionStream();
}

// ---------------------------------------------------------------------------
// B8: thinking/reasoning deltas bucket by the event's own run_id.
//
// #1157/#1162 changed the contract for visible `token_delta`: in a DM it is
// the implicit reply (delivered as the `dm_message` bubble), NOT collapsible
// reasoning, so it no longer lands in `dmThinkingBuffers`. `reasoning_delta`
// (genuine extended thinking) keeps its B8 run_id bucketing — that is the
// canonical collapsible content and what the reload path persists. The
// token_delta B8 invariant is preserved structurally: reply text from two
// overlapping runs can no longer cross-contaminate a collapsible because it
// never enters one (see the #1157 block below for the observable assertion).
// ---------------------------------------------------------------------------

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

test('#1154 B8: reasoning_delta falls back to activeRunId when run_id omitted (legacy)', () => {
    reset();
    T.activeSession.value = { session_type: 'dm' };
    const es = openStream('sess-1');

    T.activeRunId.value = 'run-legacy';
    es.emit('reasoning_delta', { text: 'no run id here' });

    assert.equal(mod.dmThinkingBuffers.value.get('run-legacy'), 'no run id here',
        'legacy backends without run_id still bucket reasoning under activeRunId');
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
// #1215/#1218: suppress_banner decouples the live banner from the phase-clear.
// The web-chat forward sets it when the DM-end notification RUN is itself the
// visible notification in that chat; the frontend must then clear the phase
// but render NO banner (the live half of "initiator gets both"). DM-session
// emissions never set the flag, so their banner still renders.
// ---------------------------------------------------------------------------

test('#1215/#1218: suppress_banner=true clears the phase but renders NO live banner', () => {
    reset();
    T.activeSession.value = { session_type: 'web' };
    const es = openStream('sess-1');

    es.emit('dm_conversation_ended', {
        peer: 'bob', reason: 'ignored', context_id: 'dm:alice:bob', suppress_banner: true,
    });

    const banners = T.chatMessages.value.filter(m => m.type === 'dm_ended');
    assert.equal(banners.length, 0,
        'suppress_banner=true must NOT append a live dm_ended banner (the run is the notification)');
    assert.ok(T.clearAgentPhaseCalls.value >= 1,
        'the phase must STILL clear when the banner is suppressed');
});

test('#1215/#1218: a DM-session emission (no suppress_banner) still renders the banner and clears', () => {
    reset();
    T.activeSession.value = { session_type: 'dm' };
    const es = openStream('sess-1');

    es.emit('dm_conversation_ended', {
        peer: 'bob', reason: 'ignored', context_id: 'dm:alice:bob',
    });

    const banners = T.chatMessages.value.filter(m => m.type === 'dm_ended');
    assert.equal(banners.length, 1,
        'without suppress_banner the DM-session-view banner must still render');
    assert.ok(T.clearAgentPhaseCalls.value >= 1, 'phase clears as before');
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

// ---------------------------------------------------------------------------
// #1157 / #1162: the implicit-reply live-render path.
//
// Under implicit DM replies (#1156) the agent's final assistant text IS the
// reply: it streams as visible `token_delta` and is delivered to the peer as
// the `dm_message` bubble. The runtime does NOT persist that text as a
// reasoning row (`finish_run` persists only the distinct extended-thinking
// trace), so on reload the collapsible holds reasoning only and the reply is
// the bubble. The live path used to also paint every DM `token_delta` into the
// reasoning collapsible (`dmThinkingBuffers`), so the reply rendered twice
// live — surfacing as a double (#1157), a wrong-agent attribution before
// participants resolved (#1162 sym-1), and a partial-then-full mid-stream
// (#1162 sym-2). The fix routes visible reply text through a separate pending
// buffer that the collapsible never reads; a tool boundary promotes pre-tool
// "thinking out loud" into the collapsible (matching the reload persistence),
// and run end discards the trailing reply text.
//
// These tests drive the real handlers through the live SSE sequence and assert
// the OBSERVABLE outcome (the sealed `dm_reasoning` block + the `dm_message`
// bubble) rather than the private pending buffer.
// ---------------------------------------------------------------------------

/** All `dm_message`/agent bubbles carrying the given text, in order. */
function bubblesWithText(text) {
    return T.chatMessages.value.filter(
        m => m.type === 'agent' && (m.text || '') === text,
    );
}

/** The single live-or-sealed dm_reasoning block for a run (or undefined). */
function reasoningBlock(runId) {
    return T.chatMessages.value.find(
        m => m.type === 'dm_reasoning' && m.runId === runId,
    );
}

test('#1157: an implicit DM reply renders ONCE (bubble) — never in the collapsible', () => {
    reset();
    T.activeSession.value = { session_type: 'dm', participants: ['alice', 'bob'] };
    T.dmParticipants.value = ['alice', 'bob'];
    T.activeAgent.value = { name: 'alice' };
    const es = openStream('dm-sess');

    // Alice's outgoing message, then Bob's peer-triggered run.
    es.emit('dm_message', { message: 'hi bob', from_agent: 'alice', from_agent_id: 'a' });
    es.emit('run_created', { run_id: 'R', source: 'peer:alice' });
    // Bob's reply streams as visible token_delta (implicit reply, no send_message).
    es.emit('token_delta', { run_id: 'R', delta: 'Hello ' });
    es.emit('token_delta', { run_id: 'R', delta: 'Alice!' });
    es.emit('run_finished', { run_id: 'R' });
    // The completion gate delivers the same text as the bubble.
    es.emit('dm_message', { message: 'Hello Alice!', from_agent: 'bob', from_agent_id: 'b' });

    // The reply text appears exactly once, as Bob's bubble.
    const replies = bubblesWithText('Hello Alice!');
    assert.equal(replies.length, 1, 'the reply must render exactly once');
    assert.equal(replies[0].fromAgent, 'bob', 'the single copy is attributed to Bob');

    // The sealed reasoning block must NOT contain the reply text.
    const block = reasoningBlock('R');
    assert.ok(block, 'a dm_reasoning block exists for the run');
    assert.equal(block.thinkingText, '',
        'the collapsible must hold no reply text (it was the bubble, not reasoning)');
    assert.equal(block.isLive, false, 'the block is sealed on run end');
});

test('#1157: while the reply is still streaming, the collapsible stays empty (no partial leak)', () => {
    reset();
    T.activeSession.value = { session_type: 'dm', participants: ['alice', 'bob'] };
    T.dmParticipants.value = ['alice', 'bob'];
    T.activeAgent.value = { name: 'alice' };
    const es = openStream('dm-sess');

    es.emit('run_created', { run_id: 'R', source: 'peer:alice' });
    es.emit('token_delta', { run_id: 'R', delta: 'Partial repl' });

    // #1162 sym-2: the live collapsible must not show the partial reply.
    const block = reasoningBlock('R');
    assert.ok(block && block.isLive, 'a live reasoning block exists mid-stream');
    const liveThinking = mod.dmThinkingBuffers.value.get('R') || '';
    assert.equal(liveThinking, '',
        'mid-stream the reasoning buffer must be empty — the reply is not reasoning');
});

test('#1162 sym-1: a peer-triggered run is never attributed to the sender when participants are unresolved', () => {
    reset();
    // The SSE stream delivers run_created BEFORE loadSession populates the
    // session participants (the documented attach race).
    T.activeSession.value = { session_type: 'dm', participants: ['alice', 'bob'] };
    T.dmParticipants.value = []; // unresolved
    T.activeAgent.value = { name: 'alice' }; // UI dropdown on the sender
    const es = openStream('dm-sess');

    es.emit('run_created', { run_id: 'R', source: 'peer:alice' });

    const block = reasoningBlock('R');
    assert.ok(block, 'a reasoning block exists for the run');
    // Pre-fix this was 'alice' (the sender) via the activeAgent fallback.
    assert.notEqual(block.agentName, 'alice',
        'the block must NOT be attributed to the sender (alice)');
    assert.equal(block.agentName, null,
        'with participants unresolved the block uses the neutral label, not a wrong name');
});

test('#1162 sym-1: once participants are resolved, a peer-triggered run is attributed to the replier', () => {
    reset();
    T.activeSession.value = { session_type: 'dm', participants: ['alice', 'bob'] };
    T.dmParticipants.value = ['alice', 'bob']; // resolved
    T.activeAgent.value = { name: 'alice' };
    const es = openStream('dm-sess');

    es.emit('run_created', { run_id: 'R', source: 'peer:alice' });

    const block = reasoningBlock('R');
    assert.ok(block, 'a reasoning block exists for the run');
    assert.equal(block.agentName, 'bob',
        'peer:alice means alice sent the message, so bob is the one replying/reasoning');
});

test('#1157/#1162: intermediate visible text before a tool is committed to the collapsible; the trailing reply is not', () => {
    reset();
    T.activeSession.value = { session_type: 'dm', participants: ['alice', 'bob'] };
    T.dmParticipants.value = ['alice', 'bob'];
    T.activeAgent.value = { name: 'alice' };
    const es = openStream('dm-sess');

    es.emit('dm_message', { message: 'compute 2+2', from_agent: 'alice', from_agent_id: 'a' });
    es.emit('run_created', { run_id: 'R', source: 'peer:alice' });
    // Bob thinks out loud (visible text) THEN calls a tool — the runtime
    // persists this pre-tool text as reasoning, so it belongs in the collapsible.
    es.emit('token_delta', { run_id: 'R', delta: 'Let me calculate. ' });
    es.emit('tool_start', { run_id: 'R', tool: 'math', params: { expr: '2+2' }, tool_invocation_id: 'inv-1' });
    es.emit('tool_end', { run_id: 'R', ok: true, result: { value: 4 }, tool_invocation_id: 'inv-1' });
    // Bob's final reply streams after the tool boundary — this is the implicit reply.
    es.emit('token_delta', { run_id: 'R', delta: 'The answer is 4!' });
    es.emit('run_finished', { run_id: 'R' });
    es.emit('dm_message', { message: 'The answer is 4!', from_agent: 'bob', from_agent_id: 'b' });

    const block = reasoningBlock('R');
    assert.ok(block, 'a sealed reasoning block exists');
    assert.equal(block.thinkingText, 'Let me calculate. ',
        'pre-tool "thinking out loud" is committed into the collapsible at the tool boundary');
    assert.equal(block.tools.length, 1, 'the math tool is grouped into the block');
    assert.equal(block.tools[0].status, 'done', 'the tool is flipped to done by tool_end');

    // The final reply is the bubble only — never in the collapsible.
    assert.ok(!block.thinkingText.includes('The answer is 4!'),
        'the trailing implicit reply must NOT be sealed into the collapsible');
    const replies = bubblesWithText('The answer is 4!');
    assert.equal(replies.length, 1, 'the reply renders exactly once as the bubble');
    assert.equal(replies[0].fromAgent, 'bob');
});

test('#1157: genuine reasoning_delta is preserved in the collapsible (implicit-reply run)', () => {
    reset();
    T.activeSession.value = { session_type: 'dm', participants: ['alice', 'bob'] };
    T.dmParticipants.value = ['alice', 'bob'];
    T.activeAgent.value = { name: 'alice' };
    const es = openStream('dm-sess');

    es.emit('run_created', { run_id: 'R', source: 'peer:alice' });
    // Extended-thinking trace (reasoning_delta) — this IS collapsible content.
    es.emit('reasoning_delta', { run_id: 'R', text: 'Pondering the greeting…' });
    // Visible reply text (token_delta) — this is the bubble, not the collapsible.
    es.emit('token_delta', { run_id: 'R', delta: 'Hi there!' });
    es.emit('run_finished', { run_id: 'R' });
    es.emit('dm_message', { message: 'Hi there!', from_agent: 'bob', from_agent_id: 'b' });

    const block = reasoningBlock('R');
    assert.ok(block, 'a sealed reasoning block exists');
    assert.equal(block.thinkingText, 'Pondering the greeting…',
        'reasoning_delta text is kept in the collapsible');
    assert.ok(!block.thinkingText.includes('Hi there!'),
        'the visible reply text is NOT mixed into the reasoning');
    assert.equal(bubblesWithText('Hi there!').length, 1, 'the reply renders once as the bubble');
});

test('#1157/#1162: two overlapping implicit-reply runs keep their replies out of each other\'s collapsible', () => {
    reset();
    T.activeSession.value = { session_type: 'dm', participants: ['alice', 'bob'] };
    T.dmParticipants.value = ['alice', 'bob'];
    T.activeAgent.value = { name: 'alice' };
    const es = openStream('dm-sess');

    // Two DM runs overlap. activeRunId points at run-2 while run-1's reply
    // is still streaming — the pre-#1154 cross-contamination scenario.
    es.emit('run_created', { run_id: 'run-1', source: 'peer:alice' });
    es.emit('run_created', { run_id: 'run-2', source: 'peer:bob' });
    T.activeRunId.value = 'run-2';
    es.emit('token_delta', { run_id: 'run-1', delta: 'reply for run-1' });
    es.emit('token_delta', { run_id: 'run-2', delta: 'reply for run-2' });
    es.emit('run_finished', { run_id: 'run-1' });
    es.emit('run_finished', { run_id: 'run-2' });

    // Neither sealed collapsible may contain ANY reply text — both replies are
    // bubbles, so cross-contamination is structurally impossible.
    const b1 = reasoningBlock('run-1');
    const b2 = reasoningBlock('run-2');
    assert.ok(b1 && b2, 'both runs have reasoning blocks');
    assert.equal(b1.thinkingText, '', 'run-1 collapsible holds no reply text');
    assert.equal(b2.thinkingText, '', 'run-2 collapsible holds no reply text');
});

// ---------------------------------------------------------------------------
// #1162 sym-2 (reopened): the cut-off-then-full duplicate that survived #1164.
//
// Root cause is BACKEND, not the #1164 frontend token-routing: a DM agent on
// minimax-m3 via OpenRouter (the #1163 provider) streams a PARTIAL of its reply,
// then its stream faults and the runtime falls back to a buffered `complete()`
// that returns the FULL response. The abandoned partial was already painted
// live (token_delta / reasoning_delta), and the buffered full text is delivered
// separately — for a DM run as the `dm_message` bubble — so the partial lingered
// as the cut-off copy in front of the full copy.
//
// The fix: when the faulted stream emitted ≥1 delta, the runtime emits a
// `stream_reset` SSE (this run), then re-emits the buffered content/reasoning as
// fresh deltas. The UI's `stream_reset` handler drops the run's partial so the
// re-emit rebuilds a single clean render that matches reload (live === reload).
//
// These drive the REAL handlers through the minimax-shaped sequence and assert
// the observable single render. They FAIL on pre-fix `develop`, where there is
// no `stream_reset` handler: the partial is never retracted, so the reply
// renders twice (partial + bubble).
// ---------------------------------------------------------------------------

/** Total visible reply text painted across all unsealed+sealed agent bubbles. */
function allAgentBubbleText() {
    return T.chatMessages.value
        .filter(m => m.type === 'agent')
        .map(m => m.text || '');
}

test('#1162 sym-2: stream_reset discards the streamed partial; the buffered reply renders ONCE (reasoning-as-reply, steady-state DM)', () => {
    reset();
    T.activeSession.value = { session_type: 'dm', participants: ['alice', 'bob'] };
    T.dmParticipants.value = ['alice', 'bob'];
    T.activeAgent.value = { name: 'alice' };
    const es = openStream('dm-sess');

    es.emit('dm_message', { message: 'hi bob', from_agent: 'alice', from_agent_id: 'a' });
    es.emit('run_created', { run_id: 'R', source: 'peer:alice' });

    // minimax-m3 streams a PARTIAL — here via reasoning_delta (its reasoning
    // channel carries the answer) — then its stream faults mid-flight.
    es.emit('reasoning_delta', { run_id: 'R', text: 'Hello ' });
    es.emit('reasoning_delta', { run_id: 'R', text: 'Ali' });
    // Pre-fix the partial 'Hello Ali' now sits in the live collapsible buffer.

    // Runtime falls back to buffered `complete()`: retract the partial, then
    // re-emit the FULL buffered response. Buffered minimax returns distinct
    // content + a (shorter) reasoning trace, so a dm_message IS delivered.
    es.emit('stream_reset', { run_id: 'R' });
    es.emit('reasoning_delta', { run_id: 'R', text: 'Greeting the user.' });
    es.emit('token_delta', { run_id: 'R', delta: 'Hello Alice!' });
    es.emit('run_finished', { run_id: 'R' });
    es.emit('dm_message', { message: 'Hello Alice!', from_agent: 'bob', from_agent_id: 'b' });

    // The reply renders exactly once — as Bob's bubble.
    const replies = bubblesWithText('Hello Alice!');
    assert.equal(replies.length, 1, 'the reply renders exactly once (no cut-off-then-full)');
    assert.equal(replies[0].fromAgent, 'bob');

    // The abandoned partial 'Hello Ali' is gone from EVERY surface: no agent
    // bubble carries it, and the sealed collapsible holds only the re-emitted
    // reasoning (never the reply text).
    assert.ok(
        !allAgentBubbleText().some(t => t.includes('Hello Ali') && t !== 'Hello Alice!'),
        'the streamed partial must not survive as a bubble',
    );
    const block = reasoningBlock('R');
    assert.ok(block, 'a sealed reasoning block exists for the run');
    assert.equal(block.thinkingText, 'Greeting the user.',
        'the collapsible holds only the re-emitted reasoning, not the abandoned partial or the reply');
    assert.ok(!block.thinkingText.includes('Hello Ali'),
        'the abandoned partial reasoning was cleared on stream_reset');
});

test('#1162 sym-2: stream_reset clears the non-DM fallthrough partial bubble (unresolved-session attach race)', () => {
    reset();
    // The attach race: run_created/deltas arrive BEFORE activeSession resolves
    // as 'dm', so isDmEvent is false and the partial falls through to a VISIBLE
    // growing agent bubble — the worst-case cut-off partial. minimax then
    // faults and falls back to buffered.
    const es = openStream('dm-sess');

    es.emit('run_created', { run_id: 'R', source: 'peer:alice' });
    es.emit('reasoning_delta', { run_id: 'R', text: 'thinking ' });
    es.emit('token_delta', { run_id: 'R', delta: 'Hello ' });
    es.emit('token_delta', { run_id: 'R', delta: 'Ali' });
    // Pre-fix: a visible unsealed bubble now reads 'Hello Ali' (the cut-off).

    es.emit('stream_reset', { run_id: 'R' });
    // The partial bubble must be gone after the reset.
    assert.equal(
        allAgentBubbleText().filter(t => t.length > 0).length, 0,
        'stream_reset removes the abandoned non-DM partial bubble',
    );

    // Buffered full response re-streams, then the dm_message bubble lands.
    es.emit('token_delta', { run_id: 'R', delta: 'Hello Alice!' });
    es.emit('run_finished', { run_id: 'R' });
    es.emit('dm_message', { message: 'Hello Alice!', from_agent: 'bob', from_agent_id: 'b' });

    const replies = bubblesWithText('Hello Alice!');
    assert.equal(replies.length, 1, 'the reply renders exactly once after the reset + re-emit');
    assert.ok(
        !allAgentBubbleText().some(t => t.includes('Hello Ali') && t !== 'Hello Alice!'),
        'no cut-off partial bubble survives',
    );
});

test('#1162 sym-2: a clean stream (no stream_reset) is unaffected — the fix is inert when there is no fallback', () => {
    reset();
    T.activeSession.value = { session_type: 'dm', participants: ['alice', 'bob'] };
    T.dmParticipants.value = ['alice', 'bob'];
    T.activeAgent.value = { name: 'alice' };
    const es = openStream('dm-sess');

    // The common path: the stream succeeds, no fallback, no stream_reset. The
    // #1164 behaviour must be preserved exactly (reply once as the bubble,
    // genuine reasoning in the collapsible).
    es.emit('run_created', { run_id: 'R', source: 'peer:alice' });
    es.emit('reasoning_delta', { run_id: 'R', text: 'Pondering.' });
    es.emit('token_delta', { run_id: 'R', delta: 'Hi there!' });
    es.emit('run_finished', { run_id: 'R' });
    es.emit('dm_message', { message: 'Hi there!', from_agent: 'bob', from_agent_id: 'b' });

    assert.equal(bubblesWithText('Hi there!').length, 1, 'reply once (no regression to #1164)');
    const block = reasoningBlock('R');
    assert.equal(block.thinkingText, 'Pondering.', 'genuine reasoning still in the collapsible');
});

// ---------------------------------------------------------------------------
// #1162 (Codex P2 attach-race follow-up): a peer DM `run_created` arriving
// BEFORE `activeSession` resolves must still create a `dm_reasoning` block.
//
// A peer DM `run_created` is ALWAYS `is_notification: true` AND carries
// `source: "peer:<name>"` (notifications.rs `enqueue_triggered_run` for
// `MessageSource::Agent`). In the documented attach race `activeSession` has not
// resolved to a DM yet, so the bare `isDm` read in `run_created` was false and
// the run fell to the `is_notification` arm — appending a bare `thinking`
// (queuedBehind:0) row instead of a `dm_reasoning` block. Live `reasoning_delta`
// (correctly bucketed by `isDmEvent` via the `peerRunIds` set) then had no block
// to render into, and `run_finished` (which only SEALS an existing block) had
// nothing to seal — so the run's reasoning was DROPPED live and only reappeared
// on reload (history rebuilds the block from the persisted reasoning row). A
// subagent/job notification run (also `is_notification: true`, but a NON-`peer:`
// source) must keep its plain thinking indicator — the `peer:` prefix is the
// discriminator.
//
// Pre-fix these reproduce the orphaned-thinking / dropped-reasoning window; the
// fix routes the run through the DM block-creation path via `isPeerSource`.
// ---------------------------------------------------------------------------

/** The single live-or-sealed dm_reasoning block for a run, or undefined. */
function thinkingRow(runId) {
    return T.chatMessages.value.find(
        m => m.type === 'thinking' && m.runId === runId,
    );
}

test('#1162 attach race: a peer DM run_created with unresolved session creates a dm_reasoning block (not an orphan thinking row)', () => {
    reset();
    // activeSession is still null — the attach race: run_created (and its
    // deltas) land before loadSession resolves the session as a DM.
    T.activeSession.value = null;
    const es = openStream('dm-sess');

    // A peer DM run_created is ALWAYS is_notification:true with a peer: source.
    es.emit('run_created', { run_id: 'R', source: 'peer:alice', is_notification: true });

    // The block must exist immediately so live reasoning has somewhere to land.
    const block = reasoningBlock('R');
    assert.ok(block && block.isLive,
        'a live dm_reasoning block is created even before activeSession resolves');
    assert.equal(thinkingRow('R'), undefined,
        'no bare thinking indicator is appended for a peer DM run (that was the orphan)');
});

test('#1162 attach race: live reasoning survives to the sealed block (was dropped pre-fix)', () => {
    reset();
    T.activeSession.value = null; // unresolved through the whole run
    const es = openStream('dm-sess');

    es.emit('run_created', { run_id: 'R', source: 'peer:alice', is_notification: true });
    // Genuine extended-thinking trace streams while the session is unresolved.
    es.emit('reasoning_delta', { run_id: 'R', text: 'Pondering the greeting.' });
    es.emit('run_finished', { run_id: 'R' });
    // The implicit reply lands as the dm_message bubble (separate row).
    es.emit('dm_message', { message: 'Hello Alice!', from_agent: 'bob', from_agent_id: 'b' });

    const block = reasoningBlock('R');
    assert.ok(block, 'a sealed dm_reasoning block exists for the run');
    assert.equal(block.isLive, false, 'the block is sealed on run end');
    // THE KEY ASSERTION: pre-fix there was no block to seal, so this reasoning
    // was dropped live (only a reload rebuilt it). Now it is preserved.
    assert.equal(block.thinkingText, 'Pondering the greeting.',
        'the run reasoning is sealed into the collapsible live, matching the reload render');
    // And no orphan thinking row survives the run end.
    assert.equal(thinkingRow('R'), undefined, 'no orphan thinking indicator lingers');
    // The reply still renders exactly once as Bob's bubble.
    assert.equal(bubblesWithText('Hello Alice!').length, 1, 'the reply renders once as the bubble');
});

test('#1162 attach race: a QUEUED peer DM run gets its block at run_started even if still unresolved', () => {
    reset();
    T.activeSession.value = null; // unresolved through run_created AND run_started
    const es = openStream('dm-sess');

    // Queued peer DM run: run_created appends only a thinking indicator (the
    // C1 invariant — the block is deferred to run_started for queued runs).
    es.emit('run_created', { run_id: 'R', source: 'peer:alice', is_notification: true, queued_behind: 1 });
    assert.ok(thinkingRow('R'), 'a queued run shows a thinking indicator at run_created');
    assert.equal(reasoningBlock('R'), undefined,
        'a queued run does NOT get a dm_reasoning block at run_created (deferred to run_started)');

    // run_started fires while activeSession is STILL unresolved. `run_started`
    // carries no source, so it relies on `peerRunIds` (recorded by run_created)
    // via the run_id-aware `isDmEvent` gate.
    es.emit('run_started', { run_id: 'R' });
    const block = reasoningBlock('R');
    assert.ok(block && block.isLive,
        'run_started creates the dm_reasoning block via peerRunIds even when activeSession is unresolved');
    assert.equal(thinkingRow('R'), undefined,
        'the queued thinking indicator is replaced by the block at run_started');

    // Reasoning then lands in the block and seals correctly.
    es.emit('reasoning_delta', { run_id: 'R', text: 'Thinking hard.' });
    es.emit('run_finished', { run_id: 'R' });
    es.emit('dm_message', { message: 'Done!', from_agent: 'bob', from_agent_id: 'b' });
    const sealed = reasoningBlock('R');
    assert.equal(sealed.thinkingText, 'Thinking hard.',
        'the queued-run reasoning is preserved into the sealed block');
});

test('#1162 attach race guard: a non-peer notification run (subagent completion) still shows a plain thinking indicator', () => {
    reset();
    // A subagent-completion notification run: is_notification:true but the
    // source is NOT a peer: DM. This must NOT be misrouted into a DM block.
    T.activeSession.value = { session_type: 'chat' };
    const es = openStream('chat-sess');

    es.emit('run_created', { run_id: 'N', source: 'subagent', is_notification: true });

    assert.ok(thinkingRow('N'),
        'a non-peer notification run still appends a plain thinking indicator');
    assert.equal(reasoningBlock('N'), undefined,
        'no dm_reasoning block is fabricated for a non-peer notification run');
});

// ---------------------------------------------------------------------------
// #1157/#1162 reconnect carry-over (Codex P2 #2 / Tim Suggestion 1).
//
// The #1157 fix routes visible DM `token_delta` into the per-run
// `dmPendingReplyBuffers` (which the collapsible never reads), promoted into
// the collapsible on `tool_start` and discarded at run end. But `token_delta`
// is ephemeral — a same-session EventSource reconnect resumes from the numeric
// `last_event_id` cursor and does NOT replay it, and `closeSessionStream`
// clears `dmPendingReplyBuffers`. So a reconnect that lands AFTER pre-tool
// "thinking out loud" text streamed but BEFORE the `tool_start` that promotes
// it used to LOSE that text from the live collapsible until a full reload
// (pre-#1157 it lived in `dmThinkingBuffers`, which survives a reconnect via
// `carriedSealedReasoning`). The fix carries the pending buffer across a
// same-session reconnect, mirroring `carriedSealedReasoning` — keeping the text
// STILL-PENDING (not committed), so the promote/discard rules still apply.
//
// These tests simulate `closeSessionStream`/reopen mid-turn via
// `reconnectStream` and assert the observable outcome. They FAIL against the
// pre-fix head where `closeSessionStream` drops the pending buffer.
// ---------------------------------------------------------------------------

test('#1157 reconnect: pending pre-tool text SURVIVES a same-session reconnect and is promoted by a later tool_start', () => {
    reset();
    T.activeSession.value = { session_type: 'dm', participants: ['alice', 'bob'] };
    T.dmParticipants.value = ['alice', 'bob'];
    T.activeAgent.value = { name: 'alice' };
    const es = openStream('dm-sess');

    es.emit('dm_message', { message: 'compute 2+2', from_agent: 'alice', from_agent_id: 'a' });
    es.emit('run_created', { run_id: 'R', source: 'peer:alice' });
    // Bob thinks out loud (visible text) — this precedes a tool, so the runtime
    // persists it as a reasoning row; it must end up in the collapsible.
    es.emit('token_delta', { run_id: 'R', delta: 'Let me calculate. ' });

    // --- The stream drops mid-turn and reconnects BEFORE the tool_start. ---
    const es2 = reconnectStream('dm-sess');

    // Guard (no #1157 regression): the carried text must NOT have been sealed
    // or committed into the collapsible by the reconnect — it stays pending.
    assert.equal(mod.dmThinkingBuffers.value.get('R') || '', '',
        'the carried pre-tool text must remain PENDING after reconnect, not be committed to the collapsible');
    const blockMid = reasoningBlock('R');
    assert.ok(blockMid, 'the dm_reasoning block survives the reconnect (it lives in chatMessages)');
    assert.equal(blockMid.thinkingText, '',
        'the live block shows no thinking text yet (the pending reply is not reasoning until a tool boundary)');
    // And it must not have leaked out as a standalone agent bubble either.
    assert.equal(bubblesWithText('Let me calculate. ').length, 0,
        'the carried pending text must not render as a standalone bubble');

    // --- After the reconnect the tool boundary arrives. ---
    es2.emit('tool_start', { run_id: 'R', tool: 'math', params: { expr: '2+2' }, tool_invocation_id: 'inv-1' });
    es2.emit('tool_end', { run_id: 'R', ok: true, result: { value: 4 }, tool_invocation_id: 'inv-1' });
    es2.emit('token_delta', { run_id: 'R', delta: 'The answer is 4!' });
    es2.emit('run_finished', { run_id: 'R' });
    es2.emit('dm_message', { message: 'The answer is 4!', from_agent: 'bob', from_agent_id: 'b' });

    const block = reasoningBlock('R');
    assert.ok(block, 'a sealed reasoning block exists');
    // THE KEY ASSERTION: the pre-tool text survived the reconnect and was
    // promoted by the post-reconnect tool_start. Pre-fix the buffer was dropped
    // on reconnect, so this would be '' and the assertion FAILS.
    assert.equal(block.thinkingText, 'Let me calculate. ',
        'pre-tool text carried across the reconnect must be promoted into the collapsible by the later tool_start');
    assert.equal(block.tools.length, 1, 'the math tool grouped into the block');
    assert.equal(block.tools[0].status, 'done', 'the tool is flipped to done by tool_end');

    // The trailing reply is still the bubble only — the carry-over did NOT turn
    // it into a double-render.
    assert.ok(!block.thinkingText.includes('The answer is 4!'),
        'the trailing implicit reply must NOT be sealed into the collapsible');
    const replies = bubblesWithText('The answer is 4!');
    assert.equal(replies.length, 1, 'the reply renders exactly once as the bubble');
    assert.equal(replies[0].fromAgent, 'bob');
});

test('#1157 reconnect: a carried PURE trailing reply is still discarded at run end (no #1157 on the reconnect path)', () => {
    reset();
    T.activeSession.value = { session_type: 'dm', participants: ['alice', 'bob'] };
    T.dmParticipants.value = ['alice', 'bob'];
    T.activeAgent.value = { name: 'alice' };
    const es = openStream('dm-sess');

    es.emit('dm_message', { message: 'hi bob', from_agent: 'alice', from_agent_id: 'a' });
    es.emit('run_created', { run_id: 'R', source: 'peer:alice' });
    // Bob's implicit reply streams as visible token_delta (no tool this turn).
    es.emit('token_delta', { run_id: 'R', delta: 'Hello ' });

    // --- Reconnect mid-reply, AFTER the (only) turn's visible text streamed. ---
    // This is the dangerous case Tim called out: a commit-on-close would seal
    // this reply into the collapsible here. The carry-over must keep it pending.
    const es2 = reconnectStream('dm-sess');
    assert.equal(mod.dmThinkingBuffers.value.get('R') || '', '',
        'the reconnect must NOT commit the trailing reply into the collapsible (that would be #1157 on reconnect)');

    // The reply finishes streaming after the reconnect (carried text survives,
    // so the post-reconnect delta appends to it).
    es2.emit('token_delta', { run_id: 'R', delta: 'Alice!' });
    es2.emit('run_finished', { run_id: 'R' });
    es2.emit('dm_message', { message: 'Hello Alice!', from_agent: 'bob', from_agent_id: 'b' });

    // Run end discards the pending reply: the collapsible holds NO reply text.
    const block = reasoningBlock('R');
    assert.ok(block, 'a sealed reasoning block exists for the run');
    assert.equal(block.thinkingText, '',
        'the implicit reply (carried across the reconnect) is discarded at run end, not sealed into the collapsible');
    assert.equal(block.isLive, false, 'the block is sealed on run end');

    // The reply renders exactly once — as Bob's bubble.
    const replies = bubblesWithText('Hello Alice!');
    assert.equal(replies.length, 1, 'the reply renders exactly once, as the bubble');
    assert.equal(replies[0].fromAgent, 'bob');
});

test('#1157 reconnect: a session SWITCH does NOT carry pending text into the new session', () => {
    reset();
    T.activeSession.value = { session_type: 'dm', participants: ['alice', 'bob'] };
    T.dmParticipants.value = ['alice', 'bob'];
    T.activeAgent.value = { name: 'alice' };
    const es = openStream('dm-sess-A');

    es.emit('run_created', { run_id: 'R', source: 'peer:alice' });
    es.emit('token_delta', { run_id: 'R', delta: 'pending text for session A' });

    // --- Operator switches to a DIFFERENT session. The pending text belongs to
    // session A's run and must NOT bleed into session B. The carry-over is
    // gated on `sessionId === activeStreamSessionId`, so a switch drops it
    // (unlike a same-session reconnect, which reuses the same id). A real
    // switch rebuilds history via `loadSession`, so clear `chatMessages` to
    // mirror that — leaving session A's stale block in place would only
    // confound the run-id reuse below. ---
    const esB = openStream('dm-sess-B');
    T.chatMessages.value = [];

    // A fresh run in session B that happens to reuse the same run id must start
    // clean — no stale pending text from session A may be promoted into its
    // collapsible at the tool boundary.
    esB.emit('run_created', { run_id: 'R', source: 'peer:carol' });
    esB.emit('tool_start', { run_id: 'R', tool: 'fs_read', params: { path: 'x' }, tool_invocation_id: 'inv-9' });

    const block = reasoningBlock('R');
    assert.ok(block, 'a reasoning block exists in session B');
    assert.equal(block.thinkingText, '',
        'session A pending text must NOT carry across a session switch and promote into session B');
});
