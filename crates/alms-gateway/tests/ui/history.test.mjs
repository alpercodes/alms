// JS-level tests for `static/ui/utils/history.js`.
//
// `history.js` is a browser ES module that imports from a sibling state
// module which in turn pulls in `deps.js` (preact + signals + htm + marked +
// dompurify via the import-map in `index.html`). None of that is reachable
// from Node, so this test loads the module's source as text and rewrites the
// two top-level imports (`nextMsgId` from `state/chat.js`, `DM_END_REASON_LABELS`
// from `./constants.js`) with locally-defined stubs / inlined values. The
// rewritten module is written to an OS tempdir and then loaded via dynamic
// `import()` so we exercise the real exported `mapHistoryMessages` /
// `groupDmReasoningBlocks` rather than a re-implementation. If `history.js`'s
// import shape ever changes, the regex below will fail loudly rather than
// silently skipping the rewrite.
//
// Pinned regression target:
//   - issue #898 / fix/898-dm-reasoning-reload — the `isReasoningText` branch
//     must consume `metadata.reasoning_blocks[*].text` when present so the
//     extended-thinking trace persisted by reasoning-capable models survives a
//     page reload (instead of falling back to the visible assistant text).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import url from 'node:url';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const HISTORY_JS_PATH = path.resolve(
    __dirname,
    '../../static/ui/utils/history.js'
);

/**
 * Load `history.js` under Node by stubbing its two top-level imports.
 * Returns { mapHistoryMessages, groupDmReasoningBlocks }.
 */
async function loadHistoryModule() {
    const src = fs.readFileSync(HISTORY_JS_PATH, 'utf8');

    // Replace the `nextMsgId` import with a deterministic counter so test
    // assertions on entry shapes don't have to deal with random IDs.
    const nextMsgIdImportRe =
        /^import\s+\{\s*nextMsgId\s*\}\s+from\s+['"][^'"]+['"];?\s*$/m;
    if (!nextMsgIdImportRe.test(src)) {
        throw new Error(
            'history.js: expected a top-level `import { nextMsgId } from ...` line — '
            + 'test rewrite would not apply. Update history.test.mjs if the import shape changed.'
        );
    }

    // Replace the constants import with an inlined object so we don't have to
    // load constants.js (which is small but would still pull a second file).
    const constantsImportRe =
        /^import\s+\{\s*DM_END_REASON_LABELS\s*\}\s+from\s+['"][^'"]+['"];?\s*$/m;
    if (!constantsImportRe.test(src)) {
        throw new Error(
            'history.js: expected a top-level `import { DM_END_REASON_LABELS } from ...` line — '
            + 'test rewrite would not apply. Update history.test.mjs if the import shape changed.'
        );
    }

    const stubbed = src
        .replace(
            nextMsgIdImportRe,
            'let __msgSeq = 0; function nextMsgId() { return "msg-" + (++__msgSeq); }'
        )
        .replace(
            constantsImportRe,
            'const DM_END_REASON_LABELS = { ignored: "no further replies", '
            + 'depth_exceeded: "message limit reached", '
            + 'user_cancelled: "cancelled by user", errored: "run failed" };'
        );

    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'alms-history-test-'));
    const tmpFile = path.join(tmpDir, 'history.mjs');
    fs.writeFileSync(tmpFile, stubbed, 'utf8');

    // Importing via the file:// URL avoids the OS-specific path quoting issues
    // that bite when feeding Windows paths to dynamic import().
    return await import(url.pathToFileURL(tmpFile).href);
}

const { mapHistoryMessages, groupDmReasoningBlocks } = await loadHistoryModule();

// ---------------------------------------------------------------------------
// #898: extended-thinking text on DM reload.
// ---------------------------------------------------------------------------
//
// The runtime persists DM assistant turns (for reasoning-capable models) as
// Role::User with metadata.message_type === "reasoning". The visible text
// goes into `content`; the extended-thinking trace goes into
// `metadata.reasoning_blocks: [{text: "..."}]`. On reload, mapHistoryMessages
// must consume the `reasoning_blocks` value so the DM thinking pane shows the
// chain-of-thought (matching the live render after #897), not the visible
// reply text.

test('#898: dm_reasoning_text uses metadata.reasoning_blocks when present', () => {
    const msgs = [
        {
            type: 'text',
            role: 'user',
            content: 'visible reply text',
            timestamp: '2026-05-02T10:00:00Z',
            metadata: {
                message_type: 'reasoning',
                from_agent: 'iris',
                run_id: 'run-1',
                reasoning_blocks: [
                    { text: 'Step 1: think hard.\n' },
                    { text: 'Step 2: still thinking.' },
                ],
            },
        },
    ];

    const entries = mapHistoryMessages(msgs, { isDm: true });
    assert.equal(entries.length, 1);
    const e = entries[0];
    assert.equal(e.type, 'dm_reasoning_text');
    assert.equal(
        e.text,
        'Step 1: think hard.\nStep 2: still thinking.',
        'reasoning_blocks text must win over m.content for reasoning-capable models',
    );
    assert.equal(e.fromAgent, 'iris');
    assert.equal(e.runId, 'run-1');
});

test('#898: dm_reasoning_text falls back to m.content when reasoning_blocks is absent', () => {
    // Non-reasoning model path: runtime never wrote reasoning_blocks because
    // there was no extended-thinking trace. The visible text must still
    // populate the entry so non-reasoning DM history keeps rendering.
    const msgs = [
        {
            type: 'text',
            role: 'user',
            content: 'plain visible text',
            timestamp: '2026-05-02T10:00:00Z',
            metadata: {
                message_type: 'reasoning',
                from_agent: 'iris',
                run_id: 'run-2',
            },
        },
    ];

    const entries = mapHistoryMessages(msgs, { isDm: true });
    assert.equal(entries.length, 1);
    assert.equal(entries[0].type, 'dm_reasoning_text');
    assert.equal(entries[0].text, 'plain visible text');
});

test('#898: empty reasoning_blocks array falls back to m.content', () => {
    // Defensive: if a future runtime writes [] (no thinking emitted on this
    // turn but the metadata key is present), don't drop the visible text.
    const msgs = [
        {
            type: 'text',
            role: 'user',
            content: 'fallback content',
            timestamp: '2026-05-02T10:00:00Z',
            metadata: {
                message_type: 'reasoning',
                from_agent: 'iris',
                run_id: 'run-3',
                reasoning_blocks: [],
            },
        },
    ];

    const entries = mapHistoryMessages(msgs, { isDm: true });
    assert.equal(entries.length, 1);
    assert.equal(entries[0].text, 'fallback content');
});

test('#898: reasoning_blocks with empty/non-string text values falls back to m.content', () => {
    // Defensive against malformed entries — if every block has empty text the
    // joined string is "", which the fallback path treats as "use m.content".
    const msgs = [
        {
            type: 'text',
            role: 'user',
            content: 'fallback text',
            timestamp: '2026-05-02T10:00:00Z',
            metadata: {
                message_type: 'reasoning',
                from_agent: 'iris',
                run_id: 'run-4',
                reasoning_blocks: [{ text: '' }, { foo: 'bar' }, null],
            },
        },
    ];

    const entries = mapHistoryMessages(msgs, { isDm: true });
    assert.equal(entries.length, 1);
    assert.equal(entries[0].text, 'fallback text');
});

test('#898: end-to-end through groupDmReasoningBlocks — thinking text is the trace', () => {
    // Simulates a full reasoning-capable DM run: a reasoning row carrying both
    // the visible content and the extended-thinking trace, plus a tool_call
    // tied to the same run via the `reasoning` message_type. After grouping,
    // the DmReasoningBlock's thinkingText must be the trace, not the visible
    // reply text — that's the live/reload divergence the issue is about.
    const msgs = [
        {
            type: 'text',
            role: 'user',
            content: 'I will help with that.',
            timestamp: '2026-05-02T10:00:01Z',
            metadata: {
                message_type: 'reasoning',
                from_agent: 'iris',
                run_id: 'run-5',
                reasoning_blocks: [{ text: 'Considering the user request...' }],
            },
        },
        {
            type: 'tool_call',
            tool: 'send_message',
            params: { message: 'I will help with that.' },
            timestamp: '2026-05-02T10:00:02Z',
            metadata: {
                message_type: 'reasoning',
                from_agent: 'iris',
                run_id: 'run-5',
                tool_call_id: 'call_1',
                tool_invocation_id: 'inv_1',
            },
        },
    ];

    const flat = mapHistoryMessages(msgs, { isDm: true });
    const grouped = groupDmReasoningBlocks(flat);

    const block = grouped.find(e => e.type === 'dm_reasoning');
    assert.ok(block, 'expected a dm_reasoning block after grouping');
    assert.equal(block.runId, 'run-5');
    assert.equal(block.agentName, 'iris');
    assert.equal(
        block.thinkingText,
        'Considering the user request...',
        'thinkingText must carry the extended-thinking trace, not the visible reply',
    );
});

// ---------------------------------------------------------------------------
// Sanity: non-DM, non-reasoning agent messages still pick up reasoning_blocks
// via the existing agent-message path (#767).  This pins the unrelated branch
// at history.js:314-320 so a future refactor that touches both branches at
// once can't quietly regress the regular non-DM reasoning panel.
// ---------------------------------------------------------------------------

test('#767 sanity: agent messages expose metadata.reasoning_blocks as `reasoning`', () => {
    const msgs = [
        {
            type: 'text',
            role: 'assistant',
            content: 'visible reply',
            timestamp: '2026-05-02T10:00:00Z',
            metadata: {
                reasoning_blocks: [{ text: 'plan A' }, { text: ' plan B' }],
            },
        },
    ];

    const entries = mapHistoryMessages(msgs, { isDm: false });
    assert.equal(entries.length, 1);
    const e = entries[0];
    assert.equal(e.type, 'agent');
    assert.equal(e.text, 'visible reply');
    assert.equal(e.reasoning, 'plan A plan B');
});

// ---------------------------------------------------------------------------
// #1076: DM reasoning attribution must not drop tool entries that arrived
// without `isReasoning` set on them.
// ---------------------------------------------------------------------------
//
// Two persistence paths can produce DM tool entries:
//   1. session_messages rows with `metadata.message_type === "reasoning"`
//      (handled by the `tool_call` branch in mapHistoryMessages)
//   2. run_tool_calls rows merged in via the `sessionToolCalls` block —
//      these set `isReasoning: isDm || undefined`.
//
// Before #1076, `groupDmReasoningBlocks` required BOTH `runId` AND
// `isReasoning`. Any tool entry slipped through without `isReasoning` (for
// example, a session_messages tool_call row whose metadata lacked the
// `message_type: reasoning` marker — possible for legacy rows, or when a
// later runtime change writes the tool call before tagging it) rendered as
// a stand-alone sibling row instead of being grouped under the agent's
// collapsible. Drop the `isReasoning` gate so attribution stays consistent
// regardless of which storage path the entry came through.

test('#1076: tool entries with runId but no isReasoning still group into dm_reasoning block', () => {
    // Build a flat entry list directly (we are testing the grouping pass,
    // not the upstream mapping path). `mapHistoryMessages` produces
    // `isReasoning: true` for entries that come through the reasoning
    // metadata path; here we simulate the "leaky" path where a tool entry
    // arrives with a runId but without the flag (the bug condition).
    const flat = [
        {
            id: 'msg-1',
            type: 'dm_reasoning_text',
            runId: 'run-A',
            fromAgent: 'iris',
            text: 'thinking about response',
            ts: '2026-05-14T12:00:01Z',
        },
        {
            id: 'msg-2',
            type: 'tool',
            tool: 'fs_read',
            params: { path: 'foo.txt' },
            status: 'done',
            result: 'contents',
            runId: 'run-A',
            // isReasoning intentionally omitted to model the bug condition.
            fromAgent: 'iris',
            ts: '2026-05-14T12:00:02Z',
        },
    ];

    const grouped = groupDmReasoningBlocks(flat);

    // The text + the tool must collapse into ONE dm_reasoning block — no
    // stray sibling tool row left behind.
    assert.equal(grouped.length, 1, 'tool without isReasoning must be absorbed into the block');
    const block = grouped[0];
    assert.equal(block.type, 'dm_reasoning');
    assert.equal(block.runId, 'run-A');
    assert.equal(block.agentName, 'iris');
    assert.equal(block.tools.length, 1);
    assert.equal(block.tools[0].tool, 'fs_read');
    assert.equal(block.thinkingText, 'thinking about response');
});

test('#1076: tool-only run (no thinking text) groups into a block when runId is set', () => {
    // A DM turn where the model went straight to a tool call without
    // emitting any chain-of-thought. The block must still materialise so
    // the per-turn affordance is consistent across runs — DmReasoningBlock
    // takes care of suppressing the header when truly empty, but a block
    // with a real tool call inside is NOT empty.
    const flat = [
        {
            id: 'msg-1',
            type: 'tool',
            tool: 'shell',
            params: { command: 'ls' },
            status: 'done',
            result: 'a.txt b.txt',
            runId: 'run-B',
            fromAgent: 'aero',
            ts: '2026-05-14T12:01:00Z',
        },
    ];

    const grouped = groupDmReasoningBlocks(flat);
    assert.equal(grouped.length, 1);
    const block = grouped[0];
    assert.equal(block.type, 'dm_reasoning');
    assert.equal(block.agentName, 'aero');
    assert.equal(block.tools.length, 1);
    assert.equal(block.thinkingText, '');
});

test('#1076: tools WITHOUT a runId still pass through as sibling rows (defensive fallback)', () => {
    // The post-#1076 grouping only requires a `runId`; entries that lack
    // one (legacy / malformed rows) must still render somewhere instead of
    // disappearing. DmConversationView renders these as sibling tool rows.
    //
    // Post-#1154 nuance: a run-less tool is only absorbed when it directly
    // FOLLOWS an open run. As the very first entry there is no run to
    // attribute it to, so it stays a sibling — this case is unchanged.
    const flat = [
        {
            id: 'msg-1',
            type: 'tool',
            tool: 'shell',
            status: 'done',
            // runId intentionally absent.
            ts: '2026-05-14T12:02:00Z',
        },
    ];

    const grouped = groupDmReasoningBlocks(flat);
    assert.equal(grouped.length, 1);
    assert.equal(grouped[0].type, 'tool', 'rowless tool must survive grouping as a sibling entry');
});

// ---------------------------------------------------------------------------
// B9 (#1154): a DM tool entry that arrived WITHOUT a runId (absent / empty
// metadata.run_id, no run_tool_calls enrichment) but which directly follows
// an open run must be absorbed into that run's dm_reasoning block instead of
// escaping to the standalone sibling-row fallback in DmConversationView.
// ---------------------------------------------------------------------------

test('#1154 B9: run-less tool following an open run is absorbed into that block', () => {
    // A reasoning row + a run-tagged tool open run-A; a SECOND tool then
    // arrives with no runId (the leaky persistence shape). It directly
    // follows run-A's entries, so it must join run-A's block rather than
    // leak out as a sibling.
    const flat = [
        {
            id: 'msg-1',
            type: 'dm_reasoning_text',
            runId: 'run-A',
            fromAgent: 'iris',
            text: 'planning the edit',
            ts: '2026-06-10T12:00:00Z',
        },
        {
            id: 'msg-2',
            type: 'tool',
            tool: 'fs_read',
            params: { path: 'a.txt' },
            status: 'done',
            runId: 'run-A',
            fromAgent: 'iris',
            ts: '2026-06-10T12:00:01Z',
        },
        {
            id: 'msg-3',
            type: 'tool',
            tool: 'fs_write',
            params: { path: 'a.txt' },
            status: 'done',
            // runId intentionally absent — the bug condition.
            ts: '2026-06-10T12:00:02Z',
        },
    ];

    const grouped = groupDmReasoningBlocks(flat);

    // Everything collapses into ONE dm_reasoning block — no sibling tool row.
    assert.equal(grouped.length, 1, 'run-less tool after an open run must be absorbed, not leaked');
    const block = grouped[0];
    assert.equal(block.type, 'dm_reasoning');
    assert.equal(block.runId, 'run-A');
    assert.equal(block.tools.length, 2, 'both the run-tagged and the run-less tool belong to the block');
    assert.deepEqual(block.tools.map(t => t.tool), ['fs_read', 'fs_write']);
});

test('#1154 B9: run-less tool after a non-tool entry (run boundary) stays a sibling', () => {
    // A dm_message bubble between run-A's tool and a later run-less tool marks
    // a run boundary, so the later tool can no longer be safely attributed to
    // run-A. It must stay a sibling (and the render layer warns).
    const flat = [
        {
            id: 'msg-1',
            type: 'tool',
            tool: 'fs_read',
            status: 'done',
            runId: 'run-A',
            fromAgent: 'iris',
            ts: '2026-06-10T12:00:00Z',
        },
        {
            id: 'msg-2',
            type: 'agent',
            role: 'assistant',
            text: 'done reading',
            fromAgent: 'iris',
            ts: '2026-06-10T12:00:01Z',
        },
        {
            id: 'msg-3',
            type: 'tool',
            tool: 'shell',
            status: 'done',
            // runId absent AND preceded by a bubble — no open run to join.
            ts: '2026-06-10T12:00:02Z',
        },
    ];

    const grouped = groupDmReasoningBlocks(flat);

    // run-A's block + the bubble + the orphan sibling tool = 3 entries.
    const block = grouped.find(e => e.type === 'dm_reasoning');
    assert.ok(block, 'run-A must still form a block');
    assert.equal(block.tools.length, 1, 'only run-A\'s own tool belongs to the block');
    const siblingTool = grouped.find(e => e.type === 'tool');
    assert.ok(siblingTool, 'the post-boundary run-less tool must remain a sibling row');
    assert.equal(siblingTool.tool, 'shell');
});

// ---------------------------------------------------------------------------
// #1083: reload-path emission filter must mirror the render-time hide rule
// in DmReasoningBlock so a DM-reply turn whose only tool is `send_message`
// (status: done) and which has no thinking text still emits a
// `dm_reasoning` block.
// ---------------------------------------------------------------------------
//
// Symptom 1 from #1076 was fixed at the render layer by changing
// DmReasoningBlock's hide predicate to count the UNFILTERED tools array
// (`(tools || []).length > 0`). But the upstream emission filter in
// `groupDmReasoningBlocks` was still filtering `visibleTools` (excluding
// `send_message:done`) before deciding whether to emit a block at all.
//
// Result: live path emitted the block (SSE handler bypasses the grouper and
// goes straight to DmReasoningBlock), reload path silently dropped it
// (grouper's emission filter rejected before reaching the render). The
// collapsible blinked into existence on the live turn and vanished after
// reload — exactly the symptom #1076 set out to kill, surviving on the
// reload path.
//
// This test pins the reload-path behaviour: a `send_message:done`-only run
// with no thinking text MUST emit a dm_reasoning block. DmReasoningBlock's
// render-time rule then decides whether the header is visible.

test('#1083: send_message:done-only run with no thinking text still emits a dm_reasoning block', () => {
    // The exact shape of a DM-reply turn that fires the asymmetry: a single
    // `send_message` tool entry (status: done) attached to a runId, with
    // NO accompanying `dm_reasoning_text` entry. Before the fix, the
    // emission filter at history.js:676 counted only `visibleTools`
    // (send_message:done excluded), so the block was never emitted on
    // reload. After the fix, the emission filter mirrors the render-time
    // rule and emits the block; DmReasoningBlock then renders the
    // collapsible header (`hadAnyTool` is true).
    const flat = [
        {
            id: 'msg-1',
            type: 'tool',
            tool: 'send_message',
            params: { message: 'hello there' },
            status: 'done',
            result: 'ok',
            runId: 'run-1083',
            fromAgent: 'iris',
            ts: '2026-05-16T10:00:00Z',
        },
    ];

    const grouped = groupDmReasoningBlocks(flat);

    // Must emit a dm_reasoning block, not a sibling tool row.
    assert.equal(grouped.length, 1, 'send_message:done-only run must produce a single grouped block');
    const block = grouped[0];
    assert.equal(block.type, 'dm_reasoning', 'block must be of type dm_reasoning (not a stray tool row)');
    assert.equal(block.runId, 'run-1083');
    assert.equal(block.agentName, 'iris');
    assert.equal(block.tools.length, 1);
    assert.equal(block.tools[0].tool, 'send_message');
    assert.equal(block.tools[0].status, 'done');
    assert.equal(
        block.thinkingText,
        '',
        'thinkingText is empty — the render path decides visibility based on (tools || []).length',
    );
});

// ---------------------------------------------------------------------------
// #1196: scheduled-job completion markers.
// ---------------------------------------------------------------------------
//
// The backend persists job completions as a synthetic marker with content
// `[Scheduled job {label}] {job_name}\n{summary}` and metadata carrying
// {job_status, run_id, job_id, job_session_id}. `mapHistoryMessages` routes
// these to a `job_completed` entry via `parseJobNotification`. These tests pin
// the robust name/summary split (job_name is single-line by backend
// construction, so the first newline is the delimiter; a summary is free to be
// multi-line) and the run_id passthrough the card uses for fetch-on-expand.

function jobMarker(content, metadata) {
    return {
        type: 'text',
        role: 'system',
        content,
        timestamp: '2026-07-06T09:00:00Z',
        metadata: { synthetic: true, type: 'job_notification', ...metadata },
    };
}

test('#1196: job marker splits name/summary and surfaces run_id + truncated', () => {
    const entries = mapHistoryMessages([
        jobMarker(
            '[Scheduled job completed] nightly digest\nDid the thing.\nAnd more detail...',
            {
                job_status: 'success',
                run_id: 'run-abc',
                job_id: 'job-abc',
                job_session_id: 'job_abc',
                job_session_uuid: '11111111-2222-3333-4444-555555555555',
                truncated: true,
            },
        ),
    ]);
    assert.equal(entries.length, 1);
    const e = entries[0];
    assert.equal(e.type, 'job_completed');
    assert.equal(e.jobName, 'nightly digest');
    assert.equal(e.status, 'success');
    // The multi-line summary (including the backend's trailing ellipsis) is
    // preserved after the first-newline delimiter.
    assert.equal(e.summary, 'Did the thing.\nAnd more detail...');
    assert.equal(e.runId, 'run-abc', 'run_id must ride through to the card for fetch-on-expand');
    assert.equal(e.truncated, true, 'the authoritative truncated flag must ride through to the card');
    // The context handle is kept for identity, but the button navigates by the
    // REAL SessionId (`job_session_uuid`), not this handle (#1217).
    assert.equal(
        e.jobSessionId, 'job_abc',
        'job_session_id (context handle) must ride through for identity (#1213)',
    );
    assert.equal(
        e.jobSessionUuid, '11111111-2222-3333-4444-555555555555',
        'job_session_uuid (real SessionId) must ride through — the button navigates by it (#1217)',
    );
});

test('#1196: a [Scheduled job ...] line inside the summary does not hijack the header', () => {
    // Robustness: only the FIRST line is matched as the header, so a decoy
    // header-shaped line in the body cannot steal the name or status.
    const entries = mapHistoryMessages([
        jobMarker(
            '[Scheduled job completed] real job name\n'
            + 'Report:\n[Scheduled job failed] decoy line in the body',
            { job_status: 'success', run_id: 'run-def' },
        ),
    ]);
    assert.equal(entries.length, 1);
    const e = entries[0];
    assert.equal(e.jobName, 'real job name', 'name comes from the first line only');
    assert.equal(e.status, 'success', 'status from metadata, not the decoy label');
    assert.ok(
        e.summary.includes('[Scheduled job failed] decoy'),
        'the decoy line stays in the summary body',
    );
    assert.equal(e.runId, 'run-def');
});

test('#1196: legacy job marker without run_id yields runId null (graceful)', () => {
    // Markers persisted before the deep-link fields existed have no run_id;
    // the entry must still parse and simply carry runId: null (no fetch).
    const entries = mapHistoryMessages([
        jobMarker('[Scheduled job failed] some job\nrun failed: boom', { job_status: 'error' }),
    ]);
    assert.equal(entries.length, 1);
    const e = entries[0];
    assert.equal(e.type, 'job_completed');
    assert.equal(e.jobName, 'some job');
    assert.equal(e.status, 'error');
    assert.equal(e.summary, 'run failed: boom');
    assert.equal(e.runId, null);
    // No truncated field on a legacy marker — passes through as undefined so
    // shouldFetchFullOutput uses the (moot, runId-less) heuristic fallback.
    assert.equal(e.truncated, undefined);
    // No job_session_id / job_session_uuid on a legacy marker — both are null,
    // so the card's "Go to job session" button does not render (#1213/#1217).
    assert.equal(e.jobSessionId, null);
    assert.equal(e.jobSessionUuid, null);
});
