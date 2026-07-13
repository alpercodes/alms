// Node-side tests for the DM RELOAD render path in
// `static/ui/utils/load-session.js` (the reload analog of the #1164 live
// hardening — see the two reload bugs this file pins below).
//
// `loadSession` drives the full reload pipeline: fetch history + session
// tool-calls, map via the REAL `mapHistoryMessages`, group via the REAL
// `groupDmReasoningBlocks`, optionally seed in-flight run text, then open
// the SSE stream and restore the agent phase. Mirroring the loader pattern
// of `dm-stream-rendering.test.mjs`, this test reads the module source,
// strips its top-level imports, and prepends a stub prelude — with one
// twist: the `history.js` pipeline is injected REAL (via
// `_load-history-module.mjs`) because the bugs live in whether/`how`
// `loadSession` invokes it, and asserting against the genuine grouped
// output is what makes the regression pins meaningful.
//
// Pinned regression targets (both share one root cause — PR #1010 moved DM
// sessions out of the per-agent `sessions` list into `crossAgentSessions`,
// and `loadSession`'s bare `sessions.value.find(...)` never matched a DM
// again, leaving `isDmSession` permanently false):
//
//   Bug 1 (reload) — with `isDmSession` false, `groupDmReasoningBlocks`
//     was skipped, so persisted DM tool rows rendered as standalone
//     sibling rows OUTSIDE the reasoning collapsible (the defensive
//     fallback + console.warn in DmConversationView) and
//     `dm_reasoning_text` entries vanished entirely.
//
//   Bug 2 (reload) — the step-3 in-flight text/reasoning rehydration is
//     designed to be SKIPPED for DM sessions (the run's trailing visible
//     text is the implicit DM reply, #1156, delivered as the `dm_message`
//     bubble). With the flag false it ran anyway on a mid-run DM load and
//     seeded the partial reply as an unsealed `type:'agent'` entry with no
//     `fromAgent` — which the DM view attributes to the UI-selected
//     perspective agent. When the delivered `dm_message` landed with the
//     real sender, the same reply rendered TWICE, once per agent (the
//     #1164 mis-attributed duplicate resurfacing on reload), and
//     `sealLastAgent` kept the seed alive on the finished conversation.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import url from 'node:url';
import { mapHistoryMessages, groupDmReasoningBlocks } from './_load-history-module.mjs';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const LOAD_SESSION_JS_PATH = path.resolve(
    __dirname,
    '../../static/ui/utils/load-session.js'
);

// Inject the REAL history.js pipeline for the stub prelude to pick up.
// Must be set before the rewritten module is imported (the prelude reads
// it at module-eval time).
globalThis.__realHistory = { mapHistoryMessages, groupDmReasoningBlocks };

// ---------------------------------------------------------------------------
// Stub block injected in place of the module's top-level imports. Minimal
// signals, recording stubs for the side-effect surfaces we assert on
// (openSessionStream, agent-phase setters, run-scoped GETs), and the REAL
// history pipeline injected from the harness.
// ---------------------------------------------------------------------------
const STUB_PRELUDE = `
function signal(initial) { return { value: initial }; }

// ---- REAL history.js pipeline (injected by the test harness) ----
const { mapHistoryMessages, groupDmReasoningBlocks } = globalThis.__realHistory;

// ---- API stubs: delegate to the per-test config in globalThis.__lsApi ----
async function getSession(id) { return globalThis.__lsApi.getSession(id); }
async function getSessionMessages(id) { return globalThis.__lsApi.getSessionMessages(id); }
async function getSessionToolCalls(id) { return globalThis.__lsApi.getSessionToolCalls(id); }
async function listRuns(id, limit) { return globalThis.__lsApi.listRuns(id, limit); }
async function listApprovals(id) { return globalThis.__lsApi.listApprovals(id); }
async function listAgentRuns(id, limit) { return globalThis.__lsApi.listAgentRuns(id, limit); }
async function getRun(id) { return globalThis.__lsApi.getRun(id); }
async function getRunReasoning(id) {
    __calls.getRunReasoning.push(id);
    return globalThis.__lsApi.getRunReasoning(id);
}
async function getRunText(id) {
    __calls.getRunText.push(id);
    return globalThis.__lsApi.getRunText(id);
}

// ---- chat state (mirrors state/chat.js + chat-actions.js) ----
const chatMessages = signal([]);
let __msgSeq = 0;
function nextMsgId() { return 'msg-' + (++__msgSeq); }
function replaceMessages(msgs) { chatMessages.value = msgs; }
function appendMessage(...msgs) { chatMessages.value = [...chatMessages.value, ...msgs]; }
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

// ---- runs state ----
const activeRunId = signal(null);
const runs = signal([]);
function syncActiveRun() {
    activeRunId.value = runs.value.find(run => run.status === 'running')?.run_id
        || runs.value.find(run => run.status === 'queued')?.run_id
        || null;
}
function replaceRuns(_sessionId, nextRuns) {
    runs.value = nextRuns;
    syncActiveRun();
}
function clearRuns() {
    runs.value = [];
    activeRunId.value = null;
}
function upsertRun(run) {
    if (!run?.run_id) return;
    const current = runs.value.find(existing => existing.run_id === run.run_id) || {};
    runs.value = [
        { ...current, ...run },
        ...runs.value.filter(existing => existing.run_id !== run.run_id),
    ];
    syncActiveRun();
}
function setRunStatus(runId, status, options) {
    upsertRun({ run_id: runId, session_id: options?.sessionId, status });
}function upsertSession(session, scope) {
    const target = scope === 'cross' ? crossAgentSessions : sessions;
    target.value = [
        ...target.value.filter(existing => existing.id !== session.id),
        session,
    ];
}

// ---- recorded side-effect surfaces ----
const __calls = {
    getRunReasoning: [],
    getRunText: [],
    streamOpens: [],
    phase: [],
};
function openSessionStream(sessionId, opts) { __calls.streamOpens.push({ sessionId, opts }); }
function registerSessionContractReconciler(reconciler) { globalThis.__lsReconciler = reconciler; }
function setAgentPhase(phase, detail) { __calls.phase.push(['setAgentPhase', phase, detail ?? null]); }
function clearAgentPhase() { __calls.phase.push(['clearAgentPhase']); }
function setDmContext(peer) { __calls.phase.push(['setDmContext', peer]); }

// ---- sessions / agents state ----
const sessions = signal([]);
const crossAgentSessions = signal([]);
const activeSessionId = signal(null);
const activeAgent = signal(null);

// ---- inert helpers ----
function getPendingMessage() { return null; }
function clearPendingMessage() {}
function rehydrateSubagentsFromHistory() {}
const parentSessionId = signal(null);
function historyCoversSeal(historyHWM, sealEventId) {
    if (historyHWM == null || sealEventId == null) return false;
    const hwm = Number(historyHWM);
    const seal = Number(sealEventId);
    if (!Number.isFinite(hwm) || !Number.isFinite(seal)) return false;
    return hwm >= seal;
}
function normalizeApproval(a) {
    return { approvalId: a.approval_id, tool: a.tool, params: a.params, runId: a.run_id };
}

// Test hooks — exported so the harness can reach the live signals + recordings.
export const __test = {
    chatMessages, activeRunId, runs, sessions, crossAgentSessions,
    activeSessionId, activeAgent, parentSessionId, __calls,
};
`;

/**
 * Load `load-session.js` under Node with its imports replaced by the stub
 * prelude. Returns the live module namespace (exports `loadSession`,
 * `__test`).
 */
async function loadLoadSessionModule() {
    let src = fs.readFileSync(LOAD_SESSION_JS_PATH, 'utf8');

    // Strip every top-level import (single-line and multi-line block forms).
    src = src.replace(/^import\s+\{[\s\S]*?\}\s+from\s+['"][^'"]+['"];?\s*$/gm, '');
    src = src.replace(/^import\s+[^{][^;]*?from\s+['"][^'"]+['"];?\s*$/gm, '');

    if (/^\s*import\s/m.test(src)) {
        throw new Error(
            'load-session.js: a top-level import survived the rewrite — '
            + 'update load-session-dm.test.mjs if the import shape changed.'
        );
    }

    const stubbed = STUB_PRELUDE + '\n' + src;

    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'alms-load-session-test-'));
    const tmpFile = path.join(tmpDir, 'load-session.mjs');
    fs.writeFileSync(tmpFile, stubbed, 'utf8');
    return await import(url.pathToFileURL(tmpFile).href);
}

const mod = await loadLoadSessionModule();
const T = mod.__test;

// ---------------------------------------------------------------------------
// Fixtures — the canonical persisted shape of an implicit-reply DM exchange
// (alice → bob → alice), exactly as the backend writes it:
//   - `dm` bubbles from `MessageBus::send` (role user, message_type "dm",
//     from_agent, NO run_id — the bus doesn't run inside a run).
//   - reasoning rows / tool_call / tool_result rows from
//     `persist_assistant_tool_calls` / `persist_one_tool_result`
//     (role user, message_type "reasoning", from_agent, run_id).
//   - the final-turn extended-thinking trace from `finish_run`
//     (empty content + metadata.reasoning_blocks).
// ---------------------------------------------------------------------------

const R1 = 'run-bob-1';
const R2 = 'run-alice-1';

function dmBubble(from, text, ts) {
    return {
        role: 'user', type: 'text', content: text, timestamp: ts,
        metadata: { from_agent: from, from_agent_id: 'id-' + from, message_type: 'dm' },
    };
}
function reasoningRow(from, runId, { content = '', blocks = null } = {}, ts) {
    const md = {
        message_type: 'reasoning', from_agent: from,
        from_agent_id: 'id-' + from, run_id: runId,
    };
    if (blocks) md.reasoning_blocks = blocks.map(t => ({ text: t }));
    return { role: 'user', type: 'text', content, timestamp: ts, metadata: md };
}
function toolCallRow(from, runId, tool, callId, invId, ts) {
    return {
        role: 'user', type: 'tool_call', tool, params: { q: 1 }, timestamp: ts,
        metadata: {
            tool_call_id: callId, tool_invocation_id: invId,
            message_type: 'reasoning', from_agent: from,
            from_agent_id: 'id-' + from, run_id: runId,
        },
    };
}
function toolResultRow(from, runId, callId, invId, ts) {
    return {
        role: 'user', type: 'tool_result', tool_id: callId, result: { ok: true },
        ok: true, timestamp: ts,
        metadata: {
            ok: true, tool_invocation_id: invId,
            message_type: 'reasoning', from_agent: from,
            from_agent_id: 'id-' + from, run_id: runId,
        },
    };
}

/** A finished two-turn DM exchange with a tool call in each turn. */
function dmHistoryFixture() {
    return [
        dmBubble('alice', 'hey bob, status?', '2026-07-01T10:00:00+00:00'),
        // bob's run R1: pre-tool thinking, a tool, the final-turn trace…
        reasoningRow('bob', R1, { content: 'Let me check the logs first.' }, '2026-07-01T10:00:05+00:00'),
        toolCallRow('bob', R1, 'shell_exec', 'call_b1', 'inv_b1', '2026-07-01T10:00:06+00:00'),
        toolResultRow('bob', R1, 'call_b1', 'inv_b1', '2026-07-01T10:00:07+00:00'),
        reasoningRow('bob', R1, { blocks: ['Logs are clean; summarize.'] }, '2026-07-01T10:00:09+00:00'),
        // …and the implicit reply delivered via MessageBus::send.
        dmBubble('bob', 'All green. Deployed at 09:40.', '2026-07-01T10:00:10+00:00'),
        // alice's run R2: tool-only turn, then her reply.
        toolCallRow('alice', R2, 'read_session', 'call_a1', 'inv_a1', '2026-07-01T10:00:12+00:00'),
        toolResultRow('alice', R2, 'call_a1', 'inv_a1', '2026-07-01T10:00:13+00:00'),
        dmBubble('alice', 'thanks, closing the loop.', '2026-07-01T10:00:15+00:00'),
    ];
}

/** Default API config: finished session, no envelope endpoint (404). */
function makeApi(overrides) {
    return {
        getSession: async () => { throw new Error('404 not found'); },
        getSessionMessages: async () => ({ messages: dmHistoryFixture(), last_event_id: 42 }),
        getSessionToolCalls: async () => ({ tool_calls: [] }),
        listRuns: async () => ({ runs: [] }),
        listApprovals: async () => ({ approvals: [] }),
        listAgentRuns: async () => ({ runs: [] }),
        getRun: async () => ({}),
        getRunReasoning: async () => ({ text: '', last_event_id: null, terminal: false }),
        getRunText: async () => ({ text: '', last_event_id: null }),
        ...overrides,
    };
}

const DM_SESSION_ID = 'dm-sess-1';
const DM_ENVELOPE = {
    id: DM_SESSION_ID,
    context_id: 'dm:alice:bob',
    session_type: 'dm',
    participants: ['alice', 'bob'],
    has_active_run: false,
};

function reset() {
    T.chatMessages.value = [];
    T.activeRunId.value = null;
    T.runs.value = [];
    T.sessions.value = [];
    T.crossAgentSessions.value = [];
    T.activeSessionId.value = null;
    T.activeAgent.value = null;
    T.parentSessionId.value = null;
    T.__calls.getRunReasoning.length = 0;
    T.__calls.getRunText.length = 0;
    T.__calls.streamOpens.length = 0;
    T.__calls.phase.length = 0;
}

async function runLoadSession() {
    await mod.loadSession(DM_SESSION_ID, {
        isStale: () => false,
        logPrefix: 'test',
    });
}

/** Shared assertions for the grouped DM reload render. */
function assertGroupedDmRender() {
    const msgs = T.chatMessages.value;

    // Bug 1 pin: ZERO standalone tool rows — every tool must live inside a
    // dm_reasoning block (the standalone-tool fallback in
    // DmConversationView, with its console.warn, must stay dead).
    const strayTools = msgs.filter(m => m.type === 'tool');
    assert.equal(strayTools.length, 0,
        `no DM tool row may escape the reasoning collapsible; escaped: ${JSON.stringify(strayTools.map(t => t.tool))}`);

    // Reasoning-text entries must have been folded into blocks too.
    assert.equal(msgs.filter(m => m.type === 'dm_reasoning_text').length, 0,
        'raw dm_reasoning_text entries must be grouped into dm_reasoning blocks');

    // Two runs → two blocks, correctly attributed, carrying their tools.
    const blocks = msgs.filter(m => m.type === 'dm_reasoning');
    assert.equal(blocks.length, 2, 'one dm_reasoning block per run');
    const bobBlock = blocks.find(b => b.runId === R1);
    const aliceBlock = blocks.find(b => b.runId === R2);
    assert.ok(bobBlock, 'bob\'s run must have a block');
    assert.ok(aliceBlock, 'alice\'s run must have a block');
    assert.equal(bobBlock.agentName, 'bob');
    assert.equal(aliceBlock.agentName, 'alice');
    assert.deepEqual(bobBlock.tools.map(t => t.tool), ['shell_exec']);
    assert.deepEqual(aliceBlock.tools.map(t => t.tool), ['read_session']);
    assert.ok(bobBlock.thinkingText.includes('Let me check the logs first.'),
        'pre-tool thinking-out-loud text belongs in the collapsible');

    // Bug 2 pin (attribution + exactly-once): each delivered message renders
    // exactly once, attributed to its real sender via fromAgent — never a
    // second perspective-side copy.
    const bubbles = msgs.filter(m => m.type === 'agent');
    assert.equal(bubbles.length, 3, 'three delivered DM messages → three bubbles');
    for (const b of bubbles) {
        assert.ok(b.fromAgent, 'every DM bubble must carry fromAgent for side attribution');
    }
    const replyCopies = bubbles.filter(m => (m.text || '').includes('All green. Deployed at 09:40.'));
    assert.equal(replyCopies.length, 1, 'the implicit reply renders exactly once');
    assert.equal(replyCopies[0].fromAgent, 'bob', 'the implicit reply is attributed to its sender');
}

// ---------------------------------------------------------------------------
// Bug 1 — reload grouping.
// ---------------------------------------------------------------------------

test('bug 1: DM session resolved via crossAgentSessions groups tools into dm_reasoning blocks', async () => {
    reset();
    // Production shape post-#1010: the DM envelope lives ONLY in the
    // cross-agent list — the per-agent `sessions` list holds chats only.
    // The envelope endpoint fails (old backend) to force the list fallback.
    T.crossAgentSessions.value = [DM_ENVELOPE];
    globalThis.__lsApi = makeApi();

    await runLoadSession();
    assertGroupedDmRender();
});

test('bug 1: DM session resolved via the GET /session/{id} envelope groups even when neither sidebar list has it', async () => {
    reset();
    // Neither list resolves (e.g. resolver-led boot before the sidebar
    // fetches land) — the step-0 envelope is the authoritative source.
    globalThis.__lsApi = makeApi({
        getSession: async () => DM_ENVELOPE,
    });

    await runLoadSession();
    assertGroupedDmRender();
});

test('regression shape: DM session in the per-agent sessions list still groups', async () => {
    reset();
    // The pre-#1010 shape — kept working via the fallback chain.
    T.sessions.value = [DM_ENVELOPE];
    globalThis.__lsApi = makeApi();

    await runLoadSession();
    assertGroupedDmRender();
});

test('envelope-only DM is injected into crossAgentSessions (activeSession / app.js / isDmEvent agree)', async () => {
    // Codex P2 #2 on PR #1193: `resolveSessionMeta` fixes loadSession's
    // LOCAL DM flag, but `app.js` (DmConversationView selection) and
    // `use-session-stream.js::isDmEvent` derive DM-ness from the shared
    // `activeSession` computed, which reads `sessions` +
    // `crossAgentSessions` — NOT the envelope. In the envelope-only case
    // the DM must therefore be injected into `crossAgentSessions` so all
    // three consumers agree.
    reset();
    // Neither sidebar list has the session (fresh reload / deep-link into
    // a DM before the cross-agent fetch lands, or that fetch failed).
    globalThis.__lsApi = makeApi({
        getSession: async () => DM_ENVELOPE,
    });

    await runLoadSession();

    const injected = T.crossAgentSessions.value.filter(s => s.id === DM_SESSION_ID);
    assert.equal(injected.length, 1,
        'the envelope-resolved DM must be injected into crossAgentSessions exactly once');
    assert.equal(injected[0].session_type, 'dm');
    assert.deepEqual(injected[0].participants, ['alice', 'bob'],
        'participants must be carried so dmParticipants / the header label resolve');
    // DMs belong to the cross-agent surface — the per-agent chat list
    // stays untouched (only INTERNAL_SESSION_TYPES go there).
    assert.equal(T.sessions.value.length, 0,
        'DM injection must not touch the per-agent sessions list');

    // Idempotence: loading the same session again (or a DM already present
    // from the sidebar fetch) must not duplicate the row.
    await runLoadSession();
    assert.equal(
        T.crossAgentSessions.value.filter(s => s.id === DM_SESSION_ID).length, 1,
        'reloading must not duplicate the injected DM');
});

// ---------------------------------------------------------------------------
// Bug 2 — mid-run reload must NOT seed the in-flight implicit reply.
// ---------------------------------------------------------------------------

test('bug 2: mid-run DM load skips the in-flight text/reasoning seed (no perspective-side duplicate)', async () => {
    reset();
    T.crossAgentSessions.value = [DM_ENVELOPE];
    T.activeAgent.value = { id: 'agent-alice', name: 'alice' };
    const RUNNING = 'run-live-1';
    globalThis.__lsApi = makeApi({
        getSession: async () => ({ ...DM_ENVELOPE, has_active_run: true }),
        listRuns: async () => ({ runs: [{ run_id: RUNNING, status: 'running' }] }),
        // The in-flight turn HAS streamed text — pre-fix this was seeded as
        // an unsealed agent entry with no fromAgent (perspective-side bubble)
        // and later double-rendered against the delivered dm_message.
        getRunReasoning: async () => ({ text: 'partial thinking', last_event_id: 90, terminal: false }),
        getRunText: async () => ({ text: 'partial implicit reply', last_event_id: 91 }),
    });

    await runLoadSession();

    // The reasoning GET fires exactly once — as the #1133 TERMINAL PROBE,
    // which runs for DM sessions too (Codex P2 on PR #1193) — but for a
    // still-live DM run its text must never be seeded and its cursor never
    // taken. The text GET serves no reconciliation purpose (terminal flag +
    // seal_event_id come from the reasoning GET), so for DM it must not
    // fire at all.
    assert.equal(T.__calls.getRunReasoning.length, 1,
        'getRunReasoning fires once for the DM terminal probe');
    assert.equal(T.__calls.getRunText.length, 0,
        'getRunText must not be called for a DM session');

    // And no unsealed perspective-side agent entry may exist.
    const seeded = T.chatMessages.value.filter(
        m => m.type === 'agent' && !m.sealed
    );
    assert.equal(seeded.length, 0,
        'no unsealed agent entry may be seeded into a DM session on reload');
    const replyCopies = T.chatMessages.value.filter(
        m => (m.text || '').includes('partial implicit reply')
            || (m.reasoning || '').includes('partial thinking')
    );
    assert.equal(replyCopies.length, 0,
        'neither the in-flight reply nor the in-flight reasoning may be painted from the run GETs');

    // The live run keeps its active marker + thinking row (only a TERMINAL
    // run is reconciled — see the dedicated test below).
    assert.equal(T.activeRunId.value, RUNNING,
        'a genuinely-live DM run must keep activeRunId');
    assert.equal(T.chatMessages.value.filter(m => m.type === 'thinking').length, 1,
        'a genuinely-live DM run keeps its thinking indicator');

    // The SSE cursor must stay at the messages HWM — the DM-skipped seed
    // paths must not take the run GETs' cursors and swallow the live
    // turn's replayable events.
    assert.equal(T.__calls.streamOpens.length, 1);
    assert.equal(T.__calls.streamOpens[0].opts.lastEventId, 42,
        'lastEventId must be the messages-GET HWM, not bumped by skipped seeds');
});

test('terminal race: DM run that finished during the load window is reconciled (no stuck thinking row)', async () => {
    // Codex P2 on PR #1193: `listRuns` (step 1) reports the run as running,
    // but it finishes BEFORE the messages GET (step 2) samples its HWM — so
    // history already contains the final bubble and the SSE stream opens
    // PAST the swallowed `run_finished` event. Without the #1133 terminal
    // reconciliation running for DM sessions, the view is left with a stuck
    // activeRunId + "Thinking…" row until a manual reload.
    reset();
    T.crossAgentSessions.value = [DM_ENVELOPE];
    const DEAD = 'run-dead-1';
    globalThis.__lsApi = makeApi({
        getSession: async () => ({ ...DM_ENVELOPE, has_active_run: true }),
        // Stale step-1 snapshot: still says running.
        listRuns: async () => ({ runs: [{ run_id: DEAD, status: 'running' }] }),
        getRun: async () => ({ run_id: DEAD, status: 'completed' }),
        // The authoritative probe says terminal; seal_event_id 40 <= the
        // messages HWM 42 → history covers the seal (sub-race A) → the run
        // also belongs in the Layer-3 suppress set.
        getRunReasoning: async () => ({
            text: '', last_event_id: null, terminal: true, seal_event_id: 40,
        }),
        // Must not be consulted at all for a DM session.
        getRunText: async () => ({ text: 'should never be read', last_event_id: 99 }),
    });

    await runLoadSession();

    // Layer 4: active-run marker cleared, stray thinking row removed.
    assert.equal(T.activeRunId.value, null,
        'terminal DM run must clear activeRunId (swallowed run_finished never replays)');
    assert.equal(T.chatMessages.value.filter(m => m.type === 'thinking').length, 0,
        'terminal DM run must not leave a stuck thinking row');

    // Layer 3: the run is in the suppress set handed to the stream.
    assert.equal(T.__calls.streamOpens.length, 1);
    const opts = T.__calls.streamOpens[0].opts;
    assert.ok(opts.sealedReasoningRunIds instanceof Set
        && opts.sealedReasoningRunIds.has(DEAD),
        'history-covered terminal DM run joins the Layer-3 suppress set');

    // The seed surfaces stay untouched: no unsealed entry, no text-GET call,
    // cursor still at the messages HWM.
    assert.equal(T.__calls.getRunText.length, 0,
        'getRunText must not be called for a DM session');
    assert.equal(T.chatMessages.value.filter(m => m.type === 'agent' && !m.sealed).length, 0,
        'terminal reconciliation must not seed any unsealed entry');
    assert.equal(opts.lastEventId, 42,
        'terminal reconciliation must not move the SSE cursor');

    // And the reload render itself is still the fully-grouped DM layout.
    assertGroupedDmRender();
});

test('terminal probe failure preserves a queued sibling run', async () => {
    reset();
    T.crossAgentSessions.value = [DM_ENVELOPE];
    const DEAD = 'run-dead-probe';
    const SIBLING = 'run-queued-sibling';
    globalThis.__lsApi = makeApi({
        getSession: async () => ({ ...DM_ENVELOPE, has_active_run: true }),
        listRuns: async () => ({
            runs: [
                { run_id: DEAD, status: 'running' },
                { run_id: SIBLING, status: 'queued', queue_position: 1 },
            ],
        }),
        getRun: async () => { throw new Error('run probe unavailable'); },
        getRunReasoning: async () => ({
            text: '', last_event_id: null, terminal: true, seal_event_id: 40,
        }),
    });

    await runLoadSession();

    assert.equal(T.runs.value.find(run => run.run_id === DEAD)?.status, 'completed');
    assert.equal(T.runs.value.find(run => run.run_id === SIBLING)?.status, 'queued');
    assert.equal(T.activeRunId.value, SIBLING,
        'terminal-probe fallback must preserve queued sibling activity');
});
test('control: mid-run NON-DM load still seeds the in-flight text (skip is DM-scoped)', async () => {
    reset();
    const CHAT_ENVELOPE = {
        id: DM_SESSION_ID, context_id: 'web-chat-1',
        session_type: 'chat', has_active_run: true,
    };
    T.sessions.value = [CHAT_ENVELOPE];
    const RUNNING = 'run-live-2';
    globalThis.__lsApi = makeApi({
        getSession: async () => CHAT_ENVELOPE,
        getSessionMessages: async () => ({ messages: [], last_event_id: 7 }),
        listRuns: async () => ({ runs: [{ run_id: RUNNING, status: 'running' }] }),
        getRunReasoning: async () => ({ text: 'thinking…', last_event_id: 8, terminal: false }),
        getRunText: async () => ({ text: 'partial visible reply', last_event_id: 9 }),
    });

    await runLoadSession();

    assert.equal(T.__calls.getRunReasoning.length, 1, 'non-DM sessions keep the reasoning seed');
    assert.equal(T.__calls.getRunText.length, 1, 'non-DM sessions keep the text seed');
    const seeded = T.chatMessages.value.find(m => m.type === 'agent' && !m.sealed);
    assert.ok(seeded, 'non-DM mid-run load must seed the unsealed agent entry');
    assert.equal(seeded.text, 'partial visible reply');
    assert.equal(seeded.reasoning, 'thinking…');
    assert.equal(T.__calls.streamOpens[0].opts.lastEventId, 9,
        'non-DM path advances the cursor past the seeded deltas');
});

// ---------------------------------------------------------------------------
// Step-5 phase restore — running DM shows "Chatting with {peer}…" again.
// ---------------------------------------------------------------------------

test('phase restore: running DM run restores setDmContext(peer) instead of generic thinking', async () => {
    reset();
    T.crossAgentSessions.value = [DM_ENVELOPE];
    T.activeAgent.value = { id: 'agent-alice', name: 'alice' };
    globalThis.__lsApi = makeApi({
        getSession: async () => ({ ...DM_ENVELOPE, has_active_run: true }),
        listRuns: async () => ({ runs: [{ run_id: 'run-live-3', status: 'running' }] }),
    });

    await runLoadSession();

    const dmContextCalls = T.__calls.phase.filter(c => c[0] === 'setDmContext');
    assert.equal(dmContextCalls.length, 1,
        'a running DM must restore the DM phase via setDmContext');
    assert.equal(dmContextCalls[0][1], 'bob',
        'the peer is the participant that is not the active agent');
});

// ---------------------------------------------------------------------------
// Strict malformed-frame reconciliation must never reopen from partial state.
// ---------------------------------------------------------------------------

test('strict reconciliation keeps stream closed when the runs snapshot fails', async () => {
    reset();
    globalThis.__lsApi = makeApi({
        getSession: async () => DM_ENVELOPE,
        listRuns: async () => { throw new Error('runs unavailable'); },
    });

    await assert.rejects(
        () => mod.loadSession(DM_SESSION_ID, {
            isStale: () => false,
            logPrefix: 'strict-test',
            requireAuthoritativeSnapshot: true,
        }),
        /runs unavailable/,
    );
    assert.equal(T.__calls.streamOpens.length, 0);
});

test('strict DM reconciliation keeps stream closed when tool-call history fails', async () => {
    reset();
    globalThis.__lsApi = makeApi({
        getSession: async () => DM_ENVELOPE,
        getSessionToolCalls: async () => { throw new Error('tool snapshot unavailable'); },
    });

    await assert.rejects(
        () => mod.loadSession(DM_SESSION_ID, {
            isStale: () => false,
            logPrefix: 'strict-test',
            requireAuthoritativeSnapshot: true,
        }),
        /tool snapshot unavailable/,
    );
    assert.equal(T.__calls.streamOpens.length, 0);
});

test('contract reconciliation clears stale activity when every run is terminal', async () => {
    reset();
    T.activeSessionId.value = DM_SESSION_ID;
    T.activeRunId.value = 'run-terminal';
    T.chatMessages.value = [{
        id: 'stale-thinking', type: 'thinking', runId: 'run-terminal',
    }];
    globalThis.__lsApi = makeApi({
        getSession: async () => DM_ENVELOPE,
        getSessionMessages: async () => ({ messages: [], last_event_id: 52 }),
        listRuns: async () => ({
            runs: [{ run_id: 'run-terminal', status: 'completed' }],
        }),
    });

    await globalThis.__lsReconciler(DM_SESSION_ID, 47);

    assert.equal(T.activeRunId.value, null);
    assert.equal(
        T.chatMessages.value.some(message => message.type === 'thinking'),
        false,
    );
    assert.equal(
        T.__calls.phase.some(
            call => call[0] === 'setAgentPhase' || call[0] === 'setDmContext',
        ),
        false,
    );
    assert.deepEqual(T.__calls.phase.at(-1), ['clearAgentPhase']);
});
