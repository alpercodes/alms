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
