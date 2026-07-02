// JS-level tests for `rehydrateSubagentsFromHistory` in
// `static/ui/state/subagents.js`.
//
// Pinned regression target:
//   - issue #1041 — subagent live status panel disappears on page reload
//     or session switch. The fix rebuilds `activeSubagents` from the
//     freshly-loaded chat history after `replaceMessages` so a still-
//     in-flight `invoke_agent` row re-creates the SubagentBar chip
//     without waiting for a terminal SSE event.
//
// The module under test imports `signal` from `../deps.js` (Preact
// signals via the index.html import-map) and `activeSessionId` from
// `./sessions.js`. Neither is reachable from Node, so we rewrite the
// two top-level imports to a local signal stub and an unused stub
// before loading the module via dynamic `import()`. The signal stub
// matches the minimal `.value` getter / setter shape the module uses
// so the rehydration write surfaces back through the exported
// `activeSubagents` value.

import { test, beforeEach, afterEach, mock } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import url from 'node:url';
// Shared history.js loader — see _load-history-module.mjs for the rewrite
// strategy. Used by the #1041 DM follow-up tests below to drive the full
// `mapHistoryMessages` -> `groupDmReasoningBlocks` -> rehydrate pipeline.
import { mapHistoryMessages, groupDmReasoningBlocks } from
    './_load-history-module.mjs';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const SUBAGENTS_JS_PATH = path.resolve(
    __dirname,
    '../../static/ui/state/subagents.js'
);

/**
 * Minimal Preact-signal stub for the `signal(initial)` call sites
 * inside `subagents.js`. Returns an object with a `.value` property
 * whose getter / setter store and return a single backing field. The
 * stub does not implement subscribers, effects, or batching — none of
 * which `rehydrateSubagentsFromHistory` exercises.
 */
const SIGNAL_STUB = `
function signal(initial) {
    let v = initial;
    return {
        get value() { return v; },
        set value(next) { v = next; },
    };
}
`;

/**
 * Load `subagents.js` under Node by rewriting its two top-level imports
 * and skipping the dynamic-import-only navigation helpers (which pull
 * in a fan of UI modules that are not part of the unit under test).
 *
 * The rewrite is intentionally narrow — the regex matches the exact
 * import lines, so a future refactor of `subagents.js` that changes
 * the import shape will fail this loader rather than silently load a
 * broken module.
 */
async function loadSubagentsModule() {
    const src = fs.readFileSync(SUBAGENTS_JS_PATH, 'utf8');

    const signalImportRe =
        /^import\s+\{\s*signal\s*\}\s+from\s+['"][^'"]+['"];?\s*$/m;
    if (!signalImportRe.test(src)) {
        throw new Error(
            'subagents.js: expected a top-level `import { signal } from ...` line — '
            + 'test rewrite would not apply. Update subagents-rehydrate.test.mjs '
            + 'if the import shape changed.'
        );
    }

    const sessionsImportRe =
        /^import\s+\{\s*activeSessionId\s*\}\s+from\s+['"][^'"]+['"];?\s*$/m;
    if (!sessionsImportRe.test(src)) {
        throw new Error(
            'subagents.js: expected a top-level `import { activeSessionId } from ...` line — '
            + 'test rewrite would not apply. Update subagents-rehydrate.test.mjs '
            + 'if the import shape changed.'
        );
    }

    const stubbed = src
        .replace(signalImportRe, SIGNAL_STUB)
        // `activeSessionId` is only read inside `doSessionSwitch`, which
        // is itself only called from the navigation helpers and is not
        // exercised by these tests. Stubbing with a dummy signal keeps
        // the module load self-contained.
        .replace(sessionsImportRe, 'const activeSessionId = { get value() { return null; }, set value(_) {} };');

    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'alms-subagents-test-'));
    const tmpFile = path.join(tmpDir, 'subagents.mjs');
    fs.writeFileSync(tmpFile, stubbed, 'utf8');

    return await import(url.pathToFileURL(tmpFile).href + '?cb=' + Date.now() + '-' + Math.random());
}

let mod;

beforeEach(async () => {
    // Fresh module per test so `activeSubagents` state from a previous
    // test doesn't bleed across cases.
    mod = await loadSubagentsModule();
});

afterEach(() => {
    // Restore real timers between tests so the A1-4 fake-timer cases below
    // don't leak `mock.timers` into the synchronous rehydrate tests.
    if (mock.timers && mock.timers.reset) {
        mock.timers.reset();
    }
});

// ---------------------------------------------------------------------------
// #1041: foreground invoke_agent rehydration.
// ---------------------------------------------------------------------------

test('#1041: rehydrates named foreground subagent with status=running', () => {
    const messages = [
        {
            id: 'inv-foo-1',
            type: 'tool',
            tool: 'invoke_agent',
            params: { name: 'reviewer', task: 'Review the PR' },
            status: 'running',
            ts: '2026-05-12T10:00:00Z',
        },
    ];

    mod.rehydrateSubagentsFromHistory(messages);

    const state = mod.activeSubagents.value;
    assert.deepEqual(Object.keys(state), ['reviewer']);
    const entry = state.reviewer;
    assert.equal(entry.status, 'running');
    assert.equal(entry.displayName, 'reviewer');
    assert.equal(entry.task, 'Review the PR');
    assert.equal(entry.toolInvocationId, 'inv-foo-1');
    assert.equal(entry.sessionId, null,
        'foreground in-flight has no session_id until the result returns');
    assert.equal(entry.activity, null,
        'a rehydrated chip has no activity signal until the next live one');
    assert.equal(entry.toolsUsed, 0);
    assert.equal(typeof entry.startedAt, 'number');
});

test('#1041: rehydrates unnamed foreground subagent under a synthetic key', () => {
    const messages = [
        {
            id: 'inv-deadbeef-cafe',
            type: 'tool',
            tool: 'invoke_agent',
            params: { task: 'Do work' },
            status: 'running',
            ts: '2026-05-12T10:00:00Z',
        },
    ];

    mod.rehydrateSubagentsFromHistory(messages);

    const state = mod.activeSubagents.value;
    const keys = Object.keys(state);
    assert.equal(keys.length, 1);
    // Unnamed subagents key on `subagent-{first-8-chars-of-invocation-id}`
    // so concurrent unnamed invocations don't collide.
    assert.equal(keys[0], 'subagent-inv-dead');
    assert.equal(state[keys[0]].displayName, 'subagent');
    assert.equal(state[keys[0]].toolInvocationId, 'inv-deadbeef-cafe');
});

test('#1041: skips foreground invoke_agent rows that already completed', () => {
    const messages = [
        {
            id: 'inv-done',
            type: 'tool',
            tool: 'invoke_agent',
            params: { name: 'reviewer', task: 'Review' },
            status: 'done',
            result: { response: 'Looks good', session_id: 'subsess-1' },
            ts: '2026-05-12T10:00:00Z',
        },
    ];

    mod.rehydrateSubagentsFromHistory(messages);
    assert.deepEqual(mod.activeSubagents.value, {},
        'completed foreground subagents must not appear in the bar');
});

// ---------------------------------------------------------------------------
// #1041: background invoke_agent rehydration.
// ---------------------------------------------------------------------------

test('#1041: rehydrates a background subagent that has not completed', () => {
    const messages = [
        {
            id: 'inv-bg',
            type: 'tool',
            tool: 'invoke_agent',
            // Background subagent: parent's tool row completes immediately
            // with { task_id, session_id }, but the subagent is still
            // running until a `subagent_completed` marker arrives.
            params: { name: 'worker', task: 'Long-running task', background: true },
            status: 'done',
            result: { task_id: 'task-1', session_id: 'subsess-bg' },
            ts: '2026-05-12T10:00:00Z',
        },
    ];

    mod.rehydrateSubagentsFromHistory(messages);

    const state = mod.activeSubagents.value;
    assert.deepEqual(Object.keys(state), ['worker']);
    const entry = state.worker;
    assert.equal(entry.status, 'running');
    assert.equal(entry.sessionId, 'subsess-bg',
        'background subagent has its session_id from the parent result');
});

test('#1041: skips a background subagent that has a matching completion marker', () => {
    const messages = [
        {
            id: 'inv-bg-done',
            type: 'tool',
            tool: 'invoke_agent',
            params: { name: 'worker', task: 'Long task', background: true },
            status: 'done',
            result: { task_id: 'task-1', session_id: 'subsess-bg-done' },
            ts: '2026-05-12T10:00:00Z',
        },
        {
            id: 'completion-1',
            type: 'subagent_completed',
            name: 'worker',
            sessionId: 'subsess-bg-done',
            status: 'done',
            summary: 'task finished',
        },
    ];

    mod.rehydrateSubagentsFromHistory(messages);
    assert.deepEqual(mod.activeSubagents.value, {},
        'a background subagent paired with a completion marker must not '
        + 're-appear on the bar — the marker is already rendered in the chat');
});

// ---------------------------------------------------------------------------
// codex P1 on PR #1049: named subagents reuse the same session_id across
// invocations (see alms-coordinator test_named_subagent_persistent_session),
// so an older subagent_completed marker MUST NOT incorrectly suppress a
// newer still-running background invocation against the same session.
// Pairing must be in chronological order, one-to-one, not Set-membership.
// ---------------------------------------------------------------------------

test('#1049 codex P1: older completion marker does not suppress newer same-session re-invocation', () => {
    // Named subagent `reviewer` invoked twice in background mode against
    // its persistent session. The first invocation completed (T=1); the
    // second invocation (T=5) is still running at reload time. The old
    // Set<sessionId>-based logic would see `subsess-reviewer` in the
    // completed set and skip BOTH invocations, leaving SubagentBar empty
    // while real work is in flight. The corrected pairing pops the
    // earliest queued row for the matching session, so the T=1 marker
    // pairs with the T=0 invocation and the T=5 invocation survives.
    const messages = [
        {
            id: 'inv-bg-old',
            type: 'tool',
            tool: 'invoke_agent',
            params: { name: 'reviewer', task: 'First review', background: true },
            status: 'done',
            result: { task_id: 'task-old', session_id: 'subsess-reviewer' },
            ts: '2026-05-12T10:00:00Z',
        },
        {
            id: 'completion-old',
            type: 'subagent_completed',
            name: 'reviewer',
            sessionId: 'subsess-reviewer',
            status: 'done',
            summary: 'first review done',
        },
        {
            id: 'inv-bg-new',
            type: 'tool',
            tool: 'invoke_agent',
            // Same session_id — named subagents reuse their session by
            // design (see alms-coordinator/src/lib.rs:1042-1046,
            // test_named_subagent_persistent_session).
            params: { name: 'reviewer', task: 'Second review', background: true },
            status: 'done',
            result: { task_id: 'task-new', session_id: 'subsess-reviewer' },
            ts: '2026-05-12T10:05:00Z',
        },
    ];

    mod.rehydrateSubagentsFromHistory(messages);

    const state = mod.activeSubagents.value;
    assert.deepEqual(Object.keys(state), ['reviewer'],
        'the newer still-running invocation must re-create the bar chip '
        + 'even though an older completion marker shares its session_id');
    const entry = state.reviewer;
    assert.equal(entry.status, 'running');
    assert.equal(entry.task, 'Second review',
        'the surviving entry must reflect the second (still-running) '
        + 'invocation, not the first');
    assert.equal(entry.toolInvocationId, 'inv-bg-new',
        'invocation id must be the second invocation');
    assert.equal(entry.sessionId, 'subsess-reviewer');
    assert.equal(entry.startedAt, Date.parse('2026-05-12T10:05:00Z'),
        'startedAt must come from the newer invocation row');
});

test('#1049 codex P1: completion marker before any invocation is harmless', () => {
    // Defensive: a stray completion marker with no queued invocation to
    // pair against (e.g. legacy history with the invoke_agent row purged
    // but the marker retained) must not crash and must not block a
    // later invocation against the same session_id.
    const messages = [
        {
            id: 'orphan-completion',
            type: 'subagent_completed',
            name: 'reviewer',
            sessionId: 'subsess-reviewer',
            status: 'done',
            summary: 'orphan',
        },
        {
            id: 'inv-bg-after',
            type: 'tool',
            tool: 'invoke_agent',
            params: { name: 'reviewer', task: 'New task', background: true },
            status: 'done',
            result: { task_id: 'task-after', session_id: 'subsess-reviewer' },
            ts: '2026-05-12T11:00:00Z',
        },
    ];

    mod.rehydrateSubagentsFromHistory(messages);
    const state = mod.activeSubagents.value;
    assert.deepEqual(Object.keys(state), ['reviewer'],
        'a still-running invocation after an orphan completion marker '
        + 'must still surface on the bar');
    assert.equal(state.reviewer.toolInvocationId, 'inv-bg-after');
});

test('#1049 codex P1: two same-session completions pair against two invocations one-to-one', () => {
    // Three sequential invocations against the same persistent session,
    // first two completed, the third still running. The pairing must
    // consume two completion markers against the two earliest queued
    // rows so the still-running third invocation survives the pass.
    //
    // Note (Tim's review on PR #1049, suggestion 1): this sequence is
    // strict-paired — every invocation is paired against its
    // completion marker before the next invocation arrives — so FIFO
    // (`.shift()`) and LIFO (`.pop()`) produce identical output. What
    // this test actually pins is the *pairing-count* contract: one
    // completion marker terminates exactly one queued invocation, and
    // an older completion marker cannot incorrectly suppress a newer
    // unpaired invocation against the same `session_id`. That contract
    // is what fixes the codex P1 bug. FIFO is the implementation
    // strategy and matches the chronological mental model, but the
    // assertion below would pass under LIFO too.
    const messages = [
        {
            id: 'inv-1',
            type: 'tool',
            tool: 'invoke_agent',
            params: { name: 'reviewer', task: 'Task A', background: true },
            status: 'done',
            result: { task_id: 'task-a', session_id: 'subsess-r' },
            ts: '2026-05-12T10:00:00Z',
        },
        {
            id: 'completion-a',
            type: 'subagent_completed',
            sessionId: 'subsess-r',
            status: 'done',
            summary: 'A done',
        },
        {
            id: 'inv-2',
            type: 'tool',
            tool: 'invoke_agent',
            params: { name: 'reviewer', task: 'Task B', background: true },
            status: 'done',
            result: { task_id: 'task-b', session_id: 'subsess-r' },
            ts: '2026-05-12T10:01:00Z',
        },
        {
            id: 'completion-b',
            type: 'subagent_completed',
            sessionId: 'subsess-r',
            status: 'done',
            summary: 'B done',
        },
        {
            id: 'inv-3',
            type: 'tool',
            tool: 'invoke_agent',
            params: { name: 'reviewer', task: 'Task C', background: true },
            status: 'done',
            result: { task_id: 'task-c', session_id: 'subsess-r' },
            ts: '2026-05-12T10:02:00Z',
        },
    ];

    mod.rehydrateSubagentsFromHistory(messages);
    const state = mod.activeSubagents.value;
    assert.deepEqual(Object.keys(state), ['reviewer']);
    assert.equal(state.reviewer.task, 'Task C',
        'only the unpaired third invocation must survive (one-to-one pairing)');
    assert.equal(state.reviewer.toolInvocationId, 'inv-3');
});

// ---------------------------------------------------------------------------
// Robustness: existing live-state preservation, malformed inputs.
// ---------------------------------------------------------------------------

test('#1041: preserves existing live SSE-populated entries (no overwrite)', () => {
    // Simulate a live SSE `tool_start` having already populated the bar
    // (e.g. between `replaceMessages` and the rehydration call). The
    // rehydration must not clobber the live entry's activity status,
    // tool count, or its authoritative start time.
    mod.trackSubagentStart('reviewer', 'live task', 'inv-live');
    mod.trackSubagentActivity('reviewer', 'tool_start', 'fs_read');
    const liveBefore = mod.activeSubagents.value.reviewer;
    const liveStartedAt = liveBefore.startedAt;
    const liveActivity = liveBefore.activity;

    const messages = [
        {
            id: 'inv-from-history',
            type: 'tool',
            tool: 'invoke_agent',
            params: { name: 'reviewer', task: 'history task' },
            status: 'running',
            ts: '2026-05-12T09:00:00Z',
        },
    ];
    mod.rehydrateSubagentsFromHistory(messages);

    const state = mod.activeSubagents.value;
    assert.equal(state.reviewer.task, 'live task',
        'live entry must win over the history rehydration');
    assert.equal(state.reviewer.startedAt, liveStartedAt,
        'live entry start time must be preserved');
    assert.equal(state.reviewer.activity, liveActivity,
        'live entry activity status must be preserved');
    assert.equal(state.reviewer.toolsUsed, 1,
        'live entry tool count must be preserved');
});

test('#1041: empty / non-array input is a no-op', () => {
    mod.rehydrateSubagentsFromHistory([]);
    assert.deepEqual(mod.activeSubagents.value, {});

    mod.rehydrateSubagentsFromHistory(null);
    assert.deepEqual(mod.activeSubagents.value, {});

    mod.rehydrateSubagentsFromHistory(undefined);
    assert.deepEqual(mod.activeSubagents.value, {});
});

test('#1041: ignores non-tool and non-invoke_agent messages', () => {
    const messages = [
        { id: 'u1', type: 'user', text: 'hi' },
        { id: 'a1', type: 'agent', text: 'hello' },
        { id: 't1', type: 'tool', tool: 'fs_read', params: { path: 'x' }, status: 'running' },
        { id: 's1', type: 'system', text: '(run completed)' },
    ];
    mod.rehydrateSubagentsFromHistory(messages);
    assert.deepEqual(mod.activeSubagents.value, {},
        'only invoke_agent tool rows are eligible for rehydration');
});

test('#1041: best-effort startedAt uses the message timestamp when present', () => {
    const messages = [
        {
            id: 'inv-ts',
            type: 'tool',
            tool: 'invoke_agent',
            params: { name: 'reviewer', task: 'task' },
            status: 'running',
            ts: '2026-05-12T10:00:00Z',
        },
    ];
    const expectedStart = Date.parse('2026-05-12T10:00:00Z');
    mod.rehydrateSubagentsFromHistory(messages);
    assert.equal(mod.activeSubagents.value.reviewer.startedAt, expectedStart);
});

test('#1041: startedAt falls back to now when timestamp is missing', () => {
    const before = Date.now();
    const messages = [
        {
            id: 'inv-no-ts',
            type: 'tool',
            tool: 'invoke_agent',
            params: { name: 'reviewer', task: 'task' },
            status: 'running',
        },
    ];
    mod.rehydrateSubagentsFromHistory(messages);
    const after = Date.now();
    const startedAt = mod.activeSubagents.value.reviewer.startedAt;
    assert.ok(startedAt >= before && startedAt <= after,
        'fallback startedAt should be wall-clock now');
});

// ---------------------------------------------------------------------------
// Integration with the live SSE handlers: rehydrated entry must remain
// reachable through the existing `tool_end` / `subagent_completed` paths.
// ---------------------------------------------------------------------------

test('#1041: rehydrated foreground entry is findable by toolInvocationId for tool_end', () => {
    const messages = [
        {
            id: 'inv-find',
            type: 'tool',
            tool: 'invoke_agent',
            params: { task: 'unnamed task' }, // unnamed -> synthetic key
            status: 'running',
            ts: '2026-05-12T10:00:00Z',
        },
    ];
    mod.rehydrateSubagentsFromHistory(messages);

    // The SSE tool_end handler for invoke_agent resolves the bar entry
    // via findSubagentByToolInvocationId when params lacks `name` (unnamed
    // / ephemeral subagents). This must succeed against a rehydrated
    // entry so the bar transitions to its terminal state correctly when
    // the foreground subagent finishes post-reload.
    const found = mod.findSubagentByToolInvocationId('inv-find');
    assert.ok(found, 'rehydrated entry must be discoverable by invocation id');
    assert.match(found, /^subagent-inv-find/);
});

test('#1041: rehydrated entry transitions to "done" via trackSubagentEnd', () => {
    const messages = [
        {
            id: 'inv-end',
            type: 'tool',
            tool: 'invoke_agent',
            params: { name: 'reviewer', task: 'task' },
            status: 'running',
            ts: '2026-05-12T10:00:00Z',
        },
    ];
    mod.rehydrateSubagentsFromHistory(messages);
    assert.equal(mod.activeSubagents.value.reviewer.status, 'running');

    // Simulate the SSE tool_end -> trackSubagentEnd chain.
    mod.trackSubagentEnd('reviewer', 'done');
    assert.equal(mod.activeSubagents.value.reviewer.status, 'done');
});

// ---------------------------------------------------------------------------
// A1-4 / #1125: a completed-subagent chip can be left in a terminal
// (`done`/`fail`) status with NO pending auto-remove timer when its
// completion lands in the close→reopen window of a session switch — the
// 15s timer that `trackSubagentEnd` armed was `clearTimeout`'d by
// `clearAllSubagents()` while the entry itself survived / was re-seeded.
// Such a chip would stick on the bar until the next session switch. The fix
// makes `rehydrateSubagentsFromHistory` (the single load chokepoint, run on
// every reload and session-switch-back) re-arm an auto-remove timer for any
// entry already in a terminal status, so the stale chip self-heals.
//
// These tests use `node:test` fake timers to assert the timer is (re)armed
// and actually fires, mirroring the pattern in `agent-events-timer.test.mjs`.
// ---------------------------------------------------------------------------

/**
 * Reproduce the exact stuck state the audit describes: a terminal entry with
 * its auto-remove timer already cancelled. We get there deterministically by
 * driving the real module surface — `trackSubagentEnd` (arms the timer) then
 * `clearAllSubagents` (cancels every timer) — but then re-establishing the
 * terminal entry directly on the signal, standing in for the live/replayed
 * SSE write that re-seeds it after the clear. The entry now has a terminal
 * status and no pending timer: the leak.
 */
function seedTerminalChipWithoutTimer(mod, key, status) {
    // 1. A normal completion arms the 15s timer for `key`.
    mod.activeSubagents.value = {
        [key]: {
            status: 'running', task: 't',
            toolInvocationId: key, displayName: key,
            startedAt: Date.now(), sessionId: null,
            activity: null, toolsUsed: 0,
        },
    };
    mod.trackSubagentEnd(key, status);
    // 2. A session switch cancels every timer AND wipes the map.
    mod.clearAllSubagents();
    // 3. A racing SSE completion / re-seed re-establishes the terminal entry
    //    while the map is otherwise empty — but no fresh timer is armed
    //    (the live `trackSubagentEnd` ran against the now-cleared map and
    //    returned early, so the entry came back via another path). Stand in
    //    for that re-seed by writing the terminal entry straight onto the
    //    signal.
    mod.activeSubagents.value = {
        [key]: {
            status, task: 't',
            toolInvocationId: key, displayName: key,
            startedAt: Date.now(), sessionId: null,
            activity: null, toolsUsed: 0,
        },
    };
}

test('A1-4: rehydrate re-arms auto-remove timer for a stuck terminal chip', () => {
    mock.timers.enable({ apis: ['setTimeout'] });

    seedTerminalChipWithoutTimer(mod, 'reviewer', 'done');
    // The chip is terminal with no timer — ticking 60s does NOT remove it.
    mock.timers.tick(60000);
    assert.equal(
        mod.activeSubagents.value.reviewer.status, 'done',
        'precondition: stuck terminal chip survives a tick with no timer',
    );

    // Rehydrate (with the chip still in the freshly-loaded history is not
    // required — the sweep runs unconditionally). Pass an unrelated history
    // so the additive seed adds nothing; only the invariant sweep should act.
    mod.rehydrateSubagentsFromHistory([
        { id: 'other', type: 'tool', tool: 'invoke_agent',
          params: { name: 'other', task: 'x' }, status: 'done',
          result: {}, ts: '2026-05-12T10:00:00Z' },
    ]);

    // The sweep re-armed the timer; after REMOVE_DELAY_MS the chip is gone.
    mock.timers.tick(15000);
    assert.equal(
        mod.activeSubagents.value.reviewer, undefined,
        'rehydrate must re-arm the timer so the stuck terminal chip is removed',
    );
});

test('A1-4: fail-status stuck chip is also swept on rehydrate', () => {
    mock.timers.enable({ apis: ['setTimeout'] });

    seedTerminalChipWithoutTimer(mod, 'subagent-abcdef01', 'fail');
    mock.timers.tick(60000);
    assert.equal(mod.activeSubagents.value['subagent-abcdef01'].status, 'fail');

    mod.rehydrateSubagentsFromHistory([]);

    mock.timers.tick(15000);
    assert.equal(
        mod.activeSubagents.value['subagent-abcdef01'], undefined,
        'fail-status stuck chip must be removed after the re-armed timer fires',
    );
});

test('A1-4: rehydrate runs the sweep even on the empty-history early return', () => {
    mock.timers.enable({ apis: ['setTimeout'] });

    seedTerminalChipWithoutTimer(mod, 'reviewer', 'done');
    // Empty history hits the `messages.length === 0` early return — the sweep
    // is placed BEFORE that guard precisely so the stale chip still heals.
    mod.rehydrateSubagentsFromHistory([]);

    mock.timers.tick(15000);
    assert.equal(
        mod.activeSubagents.value.reviewer, undefined,
        'empty-history rehydrate must still re-arm the terminal chip timer',
    );
});

test('A1-4: rehydrate does NOT schedule removal for a running entry', () => {
    mock.timers.enable({ apis: ['setTimeout'] });

    // A live in-flight chip (no timer — running chips never have one).
    mod.activeSubagents.value = {
        reviewer: {
            status: 'running', task: 't',
            toolInvocationId: 'reviewer', displayName: 'reviewer',
            startedAt: Date.now(), sessionId: null,
            activity: null, toolsUsed: 0,
        },
    };
    mod.rehydrateSubagentsFromHistory([]);

    // No timer should have been armed for a running entry: it survives.
    mock.timers.tick(60000);
    assert.equal(
        mod.activeSubagents.value.reviewer?.status, 'running',
        'running chip must not be auto-removed by the terminal sweep',
    );
});

test('A1-4: rehydrate does not double-arm an entry that already has a live timer', () => {
    mock.timers.enable({ apis: ['setTimeout'] });

    // A normal completion: entry is terminal WITH a live timer.
    mod.activeSubagents.value = {
        reviewer: {
            status: 'running', task: 't',
            toolInvocationId: 'reviewer', displayName: 'reviewer',
            startedAt: Date.now(), sessionId: null,
            activity: null, toolsUsed: 0,
        },
    };
    mod.trackSubagentEnd('reviewer', 'done');

    // Let 10s elapse, then rehydrate. The sweep must NOT reset the timer
    // (the entry already has one) — i.e. it must not extend the chip's life
    // by re-arming a fresh 15s window. 5s after rehydrate (15s total) the
    // original timer fires and the chip is removed.
    mock.timers.tick(10000);
    mod.rehydrateSubagentsFromHistory([]);
    mock.timers.tick(5000);
    assert.equal(
        mod.activeSubagents.value.reviewer, undefined,
        'existing timer must still fire on its original schedule (no re-arm)',
    );
});

// ---------------------------------------------------------------------------
// Tim's review on PR #1049, suggestion 2: defensive console.warn when the
// caller-side chronological-order invariant breaks. FIFO pairing relies on
// the post-`mapHistoryMessages` array being timestamp-ascending — if a
// future refactor of `mapHistoryMessages` or the `GET /sessions/{id}/
// messages` SQL drifts away from that ordering, we want a runtime signal
// rather than a silent miscount of pending background invocations.
// ---------------------------------------------------------------------------

test('#1049 suggestion 2: out-of-order timestamps trigger a one-shot console.warn', () => {
    const captured = [];
    const origWarn = console.warn;
    console.warn = (...args) => { captured.push(args.join(' ')); };

    try {
        const messages = [
            {
                id: 'inv-1',
                type: 'tool',
                tool: 'invoke_agent',
                params: { name: 'reviewer', task: 'A' },
                status: 'running',
                ts: '2026-05-12T10:05:00Z',
            },
            {
                id: 'inv-2',
                type: 'tool',
                tool: 'invoke_agent',
                params: { name: 'other', task: 'B' },
                status: 'running',
                ts: '2026-05-12T10:00:00Z', // earlier than inv-1 → out of order
            },
            {
                id: 'inv-3',
                type: 'tool',
                tool: 'invoke_agent',
                params: { name: 'third', task: 'C' },
                status: 'running',
                ts: '2026-05-12T09:55:00Z', // also out of order, but warn must not double-fire
            },
        ];

        mod.rehydrateSubagentsFromHistory(messages);

        // Exactly one warn fires (one-shot), and it cites the PR
        // and Tim's suggestion so a future reader can find the
        // motivating discussion quickly.
        const matches = captured.filter(line =>
            line.includes('rehydrateSubagentsFromHistory') && line.includes('chronological'));
        assert.equal(matches.length, 1, 'exactly one chronological-order warning must fire');
        assert.match(matches[0], /#1049/);
    } finally {
        console.warn = origWarn;
    }
});

test('#1049 suggestion 2: in-order timestamps do not trigger the warning', () => {
    const captured = [];
    const origWarn = console.warn;
    console.warn = (...args) => { captured.push(args.join(' ')); };

    try {
        const messages = [
            {
                id: 'inv-1',
                type: 'tool',
                tool: 'invoke_agent',
                params: { name: 'a', task: 'A' },
                status: 'running',
                ts: '2026-05-12T10:00:00Z',
            },
            {
                id: 'inv-2',
                type: 'tool',
                tool: 'invoke_agent',
                params: { name: 'b', task: 'B' },
                status: 'running',
                ts: '2026-05-12T10:01:00Z',
            },
            {
                id: 'inv-3',
                type: 'tool',
                tool: 'invoke_agent',
                params: { name: 'c', task: 'C' },
                status: 'running',
                ts: '2026-05-12T10:02:00Z', // equal-or-later is fine
            },
        ];

        mod.rehydrateSubagentsFromHistory(messages);

        const matches = captured.filter(line =>
            line.includes('rehydrateSubagentsFromHistory') && line.includes('chronological'));
        assert.equal(matches.length, 0, 'no warning expected on in-order input');
    } finally {
        console.warn = origWarn;
    }
});

// ---------------------------------------------------------------------------
// #1041 DM follow-up (codex P2 + Tim on PR #1049): the call site in
// `utils/load-session.js` must pass the PRE-grouping `mapped` array to
// `rehydrateSubagentsFromHistory`, not the POST-grouping `grouped` array.
//
// In DM sessions, `mapHistoryMessages` annotates every session-tool-call-
// record-merged tool row with `isReasoning: true` (see history.js line 522).
// `groupDmReasoningBlocks` then collapses all such tool rows into a single
// `dm_reasoning` block entry, hiding the underlying `type: 'tool'` rows
// from any consumer that filters by message type. Passing the grouped
// array to the rehydration function would therefore see zero invoke_agent
// rows for DM sessions and silently drop the live status panel after
// reload — which is exactly the bug codex + Tim flagged on PR #1049.
//
// This test exercises the full pipeline: build a DM-session message
// history, run it through `mapHistoryMessages` with `isDm: true`, confirm
// `groupDmReasoningBlocks` does indeed hide the invoke_agent row, then
// assert the production call (against `mapped`) still rehydrates the chip.
// The grouped-array branch is asserted too as a regression anchor for the
// pre-fix behaviour — the fix is in the call site, not in the function.
// ---------------------------------------------------------------------------

test('#1041 DM follow-up: rehydrates in-flight background invoke_agent in DM '
    + 'session despite reasoning grouping (codex P2 / Tim on PR #1049)', () => {
    // Synthetic DM-session API payload: an invoke_agent tool call lives
    // only in the session-level `run_tool_calls` records (the DM-session
    // exclusion path, see CLAUDE.md "tool call persistence"). The
    // session-message stream has no `tool_call` row for it.
    const sessionMessages = [
        // Reasoning text from the parent agent. Triggers
        // groupDmReasoningBlocks since it shares run_id with the tool row.
        {
            type: 'text',
            role: 'user',
            content: 'I should delegate this to the worker.',
            timestamp: '2026-05-12T10:00:00Z',
            metadata: {
                message_type: 'reasoning',
                from_agent: 'parent',
                run_id: 'run-bg-1',
            },
        },
    ];
    // The invoke_agent tool call lives in the run_tool_calls table. The
    // session-level GET /sessions/{id}/tool-calls endpoint serves it as
    // an (assistant_call, tool_result) pair keyed by tool_id.
    const sessionToolCalls = [
        {
            tool_id: 'inv-dm-bg',
            role: 'assistant',
            tool_name: 'invoke_agent',
            params: JSON.stringify({
                name: 'worker',
                task: 'Long DM-spawned task',
                background: true,
            }),
            from_agent: 'parent',
            run_id: 'run-bg-1',
            timestamp: '2026-05-12T10:00:01Z',
        },
        {
            tool_id: 'inv-dm-bg',
            role: 'tool',
            // Background result: parent's tool row completes immediately
            // with { task_id, session_id }. The subagent itself is still
            // running until a separate subagent_completed marker arrives.
            result: JSON.stringify({
                task_id: 'task-dm-bg',
                session_id: 'subsess-worker',
            }),
            from_agent: 'parent',
            run_id: 'run-bg-1',
            timestamp: '2026-05-12T10:00:02Z',
        },
    ];

    // Stage 1: map the raw API payload with isDm: true so the merged
    // tool row picks up `isReasoning: true` (history.js line 522).
    const mapped = mapHistoryMessages(sessionMessages, {
        hasActiveRun: true,
        sessionToolCalls,
        isDm: true,
    });

    // Sanity-check the precondition the rehydrator depends on: the
    // mapped array DOES contain a top-level invoke_agent tool row.
    const mappedToolRows = mapped.filter(
        m => m.type === 'tool' && m.tool === 'invoke_agent'
    );
    assert.equal(mappedToolRows.length, 1,
        'precondition: mapHistoryMessages must surface the invoke_agent '
        + 'tool row at the top level before grouping');
    assert.equal(mappedToolRows[0].isReasoning, true,
        'precondition: DM tool rows must be flagged with isReasoning so '
        + 'groupDmReasoningBlocks knows to fold them');
    assert.equal(mappedToolRows[0].runId, 'run-bg-1',
        'precondition: tool row must carry run_id for grouping');

    // Stage 2: run the DM grouping pass — this is the load-session.js
    // DM branch (`isDmSession ? groupDmReasoningBlocks(mapped) : mapped`).
    const grouped = groupDmReasoningBlocks(mapped);

    // Confirm the bug-triggering condition: the invoke_agent tool row is
    // now hidden inside the dm_reasoning block, not at top level. Any
    // consumer that filters by `type === 'tool'` against `grouped` will
    // see zero invoke_agent rows. This is the receipt that the pre-fix
    // call site (`rehydrateSubagentsFromHistory(grouped)`) was reading
    // from the wrong array.
    const groupedToolRows = grouped.filter(
        m => m.type === 'tool' && m.tool === 'invoke_agent'
    );
    assert.equal(groupedToolRows.length, 0,
        'bug condition: groupDmReasoningBlocks must collapse the DM '
        + 'invoke_agent tool row into a dm_reasoning block so it is no '
        + 'longer reachable via type === "tool" at the top level');
    const reasoningBlocks = grouped.filter(m => m.type === 'dm_reasoning');
    assert.equal(reasoningBlocks.length, 1,
        'bug condition: the tool row must have been folded into exactly '
        + 'one dm_reasoning block');
    assert.equal(reasoningBlocks[0].tools.length, 1,
        'sanity: the reasoning block must contain the invoke_agent tool row');

    // Stage 3 (THE FIX): rehydration against `mapped` (the production
    // call site after the codex P2 fix) re-creates the SubagentBar chip
    // for the still-in-flight background invocation.
    mod.rehydrateSubagentsFromHistory(mapped);

    const stateAfterFix = mod.activeSubagents.value;
    assert.deepEqual(Object.keys(stateAfterFix), ['worker'],
        'fix: passing the pre-grouping `mapped` array rehydrates the chip');
    assert.equal(stateAfterFix.worker.status, 'running');
    assert.equal(stateAfterFix.worker.task, 'Long DM-spawned task');
    assert.equal(stateAfterFix.worker.sessionId, 'subsess-worker');
    assert.equal(stateAfterFix.worker.toolInvocationId, 'inv-dm-bg');
});

test('#1041 DM follow-up regression anchor: calling the rehydrator with '
    + 'the post-grouping array still misses the chip — this pins the '
    + 'pre-fix behaviour and proves the fix is at the call site, not '
    + 'in rehydrateSubagentsFromHistory itself', () => {
    // Same fixture as the test above, but this one asserts the negative:
    // running the rehydrator against the post-grouping `grouped` array
    // produces an empty bar. If a future refactor changes
    // `rehydrateSubagentsFromHistory` to also look inside dm_reasoning
    // blocks, this test will start failing and the assertion message will
    // explain that the call-site invariant in load-session.js can then
    // be loosened (or this regression-anchor test can be deleted).
    const sessionMessages = [
        {
            type: 'text',
            role: 'user',
            content: 'thinking...',
            timestamp: '2026-05-12T10:00:00Z',
            metadata: {
                message_type: 'reasoning',
                from_agent: 'parent',
                run_id: 'run-bg-2',
            },
        },
    ];
    const sessionToolCalls = [
        {
            tool_id: 'inv-dm-bg-2',
            role: 'assistant',
            tool_name: 'invoke_agent',
            params: JSON.stringify({
                name: 'worker',
                task: 'Another DM-spawned task',
                background: true,
            }),
            from_agent: 'parent',
            run_id: 'run-bg-2',
            timestamp: '2026-05-12T10:00:01Z',
        },
        {
            tool_id: 'inv-dm-bg-2',
            role: 'tool',
            result: JSON.stringify({
                task_id: 'task-dm-bg-2',
                session_id: 'subsess-worker-2',
            }),
            from_agent: 'parent',
            run_id: 'run-bg-2',
            timestamp: '2026-05-12T10:00:02Z',
        },
    ];

    const mapped = mapHistoryMessages(sessionMessages, {
        hasActiveRun: true,
        sessionToolCalls,
        isDm: true,
    });
    const grouped = groupDmReasoningBlocks(mapped);

    // Drive the rehydrator with the wrong (post-grouping) array. The
    // bar must stay empty — this is the behaviour that motivated the
    // call-site fix.
    mod.rehydrateSubagentsFromHistory(grouped);

    assert.deepEqual(mod.activeSubagents.value, {},
        'regression anchor: feeding the post-grouping array to the '
        + 'rehydrator drops DM subagent chips on the floor. The '
        + 'production fix is to pass `mapped` instead (see '
        + 'utils/load-session.js call site).');
});
