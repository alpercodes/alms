// JS-level regression tests for `static/ui/utils/sidebar-grouping.js`.
//
// Pinned regression target:
//   - issue #980 — operators with multiple agents and many sessions per
//     agent need the sidebar grouped under per-agent collapsible
//     headers. At most one agent group is expanded at a time; clicking
//     another agent header collapses the previous and expands the new
//     one. Clicking the expanded agent's header collapses the body
//     (true toggle) without altering the active agent. The
//     default-expanded agent at boot is the currently-active one.
//
// The runtime side (Preact rendering, signal wiring, switchAgent
// side-effect) is integration territory we can't easily unit-test
// under Node. What we CAN pin here is the pure-function shape of the
// expand/collapse decision so a future refactor of the accordion
// can't silently break the "at most one at a time" contract or the
// default-expansion rule.
//
// Mirrors the `card-activation.test.mjs` pattern — pure-function
// predicate tests with a static `import` (no module reload required
// since the module has no internal state).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import url from 'node:url';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const SIDEBAR_GROUPING_PATH = path.resolve(
    __dirname,
    '../../static/ui/utils/sidebar-grouping.js'
);

const {
    expandAgent,
    defaultExpandedAgent,
    isAgentExpanded,
    groupSessionsByAgent,
    isOwnedByActiveAgent,
    sortCrossAgentRows,
    filterChatSessions,
} = await import(url.pathToFileURL(SIDEBAR_GROUPING_PATH).href);

// ---------------------------------------------------------------------
// expandAgent — the core "click another agent to switch groups" rule
// ---------------------------------------------------------------------

test('#980: expandAgent expands a fresh agent when nothing is expanded', () => {
    // Boot edge case — no agent expanded yet (e.g. after agent-deletion
    // wiped the active agent). First click should expand the chosen one.
    assert.equal(expandAgent(null, 'agent_a'), 'agent_a');
});

test('#980: expandAgent collapses the previous agent and expands the new one', () => {
    // The headline case: click on agent B while agent A is expanded.
    // Exactly-one-at-a-time means the result is just B.
    assert.equal(expandAgent('agent_a', 'agent_b'), 'agent_b');
});

test('#980: expandAgent toggles collapsed when the same agent is clicked again', () => {
    // Pinned design choice (Alper UX feedback on PR #1010): clicking the
    // already-expanded agent collapses the accordion body. The active
    // agent is unchanged — that's the click handler's decision in
    // session-list.js, which leaves `activeAgentId` untouched on this
    // branch so the chat view, dropdown, etc. still show the same
    // agent. This test pins the pure-function half of the toggle.
    assert.equal(expandAgent('agent_a', 'agent_a'), null);
});

test('#980: expandAgent re-expands the collapsed-but-active agent on second click', () => {
    // Continuation of the toggle: after collapsing the active agent
    // (expanded -> null), clicking the same agent's header again
    // expands it back. This is what makes the header behave like a
    // real two-state toggle from the operator's POV.
    let s = 'agent_a';
    s = expandAgent(s, 'agent_a'); // collapse
    assert.equal(s, null);
    s = expandAgent(s, 'agent_a'); // re-expand
    assert.equal(s, 'agent_a');
});

test('#980: expandAgent is a no-op for missing / falsy agentId', () => {
    // Defense in depth — the call site never passes null today, but
    // surfacing a no-op here keeps the helper safe.
    assert.equal(expandAgent('agent_a', null), 'agent_a');
    assert.equal(expandAgent('agent_a', undefined), 'agent_a');
    assert.equal(expandAgent('agent_a', ''), 'agent_a');
    // Same defence with no current expansion.
    assert.equal(expandAgent(null, null), null);
    assert.equal(expandAgent(null, undefined), null);
});

test('#980: expandAgent rejects non-string agentId', () => {
    // A signal that accidentally holds a number / object should not
    // squash the current expansion to garbage. Stay on the previous
    // value.
    assert.equal(expandAgent('agent_a', 42), 'agent_a');
    assert.equal(expandAgent('agent_a', { id: 'agent_b' }), 'agent_a');
});

test('#980: expandAgent transitions A -> B -> A correctly across two clicks', () => {
    // Sequenced application of the helper. State machine sanity check
    // for the back-and-forth pattern an operator hits when comparing
    // sessions across two agents.
    let s = null;
    s = expandAgent(s, 'agent_a'); // first click
    assert.equal(s, 'agent_a');
    s = expandAgent(s, 'agent_b'); // switch
    assert.equal(s, 'agent_b');
    s = expandAgent(s, 'agent_a'); // switch back
    assert.equal(s, 'agent_a');
});

// ---------------------------------------------------------------------
// defaultExpandedAgent — boot / agent-switch default state
// ---------------------------------------------------------------------

test('#980: defaultExpandedAgent matches the active agent at boot', () => {
    // At boot time, the operator just landed on a session that belongs
    // to the active agent. That group should be open by default so the
    // session list is visible without an extra click.
    assert.equal(defaultExpandedAgent('agent_a'), 'agent_a');
});

test('#980: defaultExpandedAgent returns null when no agent is active', () => {
    // Boot-time race / "all agents deleted" state — nothing should be
    // expanded; the accordion renders all agents collapsed.
    assert.equal(defaultExpandedAgent(null), null);
    assert.equal(defaultExpandedAgent(undefined), null);
    assert.equal(defaultExpandedAgent(''), null);
});

test('#980: defaultExpandedAgent rejects non-string activeAgentId', () => {
    assert.equal(defaultExpandedAgent(42), null);
    assert.equal(defaultExpandedAgent({}), null);
});

// ---------------------------------------------------------------------
// isAgentExpanded — the render-time predicate
// ---------------------------------------------------------------------

test('#980: isAgentExpanded is true only for the currently-expanded agent', () => {
    assert.equal(isAgentExpanded('agent_a', 'agent_a'), true);
    assert.equal(isAgentExpanded('agent_a', 'agent_b'), false);
});

test('#980: isAgentExpanded is false when nothing is expanded', () => {
    assert.equal(isAgentExpanded(null, 'agent_a'), false);
    assert.equal(isAgentExpanded(undefined, 'agent_a'), false);
});

test('#980: isAgentExpanded is false for falsy / non-string agentId', () => {
    assert.equal(isAgentExpanded('agent_a', null), false);
    assert.equal(isAgentExpanded('agent_a', undefined), false);
    assert.equal(isAgentExpanded('agent_a', ''), false);
    assert.equal(isAgentExpanded('agent_a', 42), false);
});

// ---------------------------------------------------------------------
// groupSessionsByAgent — flat list -> Map<agent_id, sessions[]>
// ---------------------------------------------------------------------

test('#980: groupSessionsByAgent groups a flat list by agent_id', () => {
    const sessions = [
        { id: 's1', agent_id: 'agent_a' },
        { id: 's2', agent_id: 'agent_b' },
        { id: 's3', agent_id: 'agent_a' },
    ];
    const grouped = groupSessionsByAgent(sessions);
    assert.equal(grouped.size, 2);
    assert.deepEqual(grouped.get('agent_a').map(s => s.id), ['s1', 's3']);
    assert.deepEqual(grouped.get('agent_b').map(s => s.id), ['s2']);
});

test('#980: groupSessionsByAgent preserves input order within each group', () => {
    // Sidebar relies on the backend's sort order (last-activity DESC)
    // — the grouping must not shuffle within an agent's bucket.
    const sessions = [
        { id: 's3', agent_id: 'agent_a' },
        { id: 's1', agent_id: 'agent_a' },
        { id: 's2', agent_id: 'agent_a' },
    ];
    const grouped = groupSessionsByAgent(sessions);
    assert.deepEqual(grouped.get('agent_a').map(s => s.id), ['s3', 's1', 's2']);
});

test('#980: groupSessionsByAgent drops sessions with missing agent_id', () => {
    // Sessions with no / falsy agent_id (DM sentinel sessions, malformed
    // entries) belong to a separate section — they're not part of the
    // per-agent grouping.
    const sessions = [
        { id: 's1', agent_id: 'agent_a' },
        { id: 's2', agent_id: null },
        { id: 's3' }, // no field
        { id: 's4', agent_id: '' },
    ];
    const grouped = groupSessionsByAgent(sessions);
    assert.equal(grouped.size, 1);
    assert.deepEqual(grouped.get('agent_a').map(s => s.id), ['s1']);
});

test('#980: groupSessionsByAgent returns an empty Map for empty / non-array input', () => {
    assert.equal(groupSessionsByAgent([]).size, 0);
    assert.equal(groupSessionsByAgent(null).size, 0);
    assert.equal(groupSessionsByAgent(undefined).size, 0);
    assert.equal(groupSessionsByAgent('not an array').size, 0);
});

test('#980: groupSessionsByAgent skips null / non-object entries', () => {
    const sessions = [
        null,
        { id: 's1', agent_id: 'agent_a' },
        undefined,
        { id: 's2', agent_id: 'agent_b' },
    ];
    const grouped = groupSessionsByAgent(sessions);
    assert.equal(grouped.size, 2);
});

// ---------------------------------------------------------------------
// isOwnedByActiveAgent — cross-agent surface ownership predicate
// ---------------------------------------------------------------------
//
// Pinned regression target: cross-agent DM + notification listing
// (PR #1010 follow-up). Operators see DMs / notifications across
// every agent in their tenant, but rows owned by the currently-
// active agent are visually emphasised so they scan above the rest.
// This predicate is the single source of truth for "owned by the
// active agent" so the badge color, the sort ordering, and any
// future filter all read the same answer.

test('cross-agent: isOwnedByActiveAgent matches notification recipient', () => {
    // The agent_name on a notification session is the recipient — when
    // the active agent matches, the row is "ours".
    const session = { session_type: 'notification', agent_name: 'alice' };
    assert.equal(isOwnedByActiveAgent(session, 'alice'), true);
    assert.equal(isOwnedByActiveAgent(session, 'bob'), false);
});

test('cross-agent: isOwnedByActiveAgent matches DM participant', () => {
    // DMs have a participants array; ownership = active agent is in it.
    const session = { session_type: 'dm', participants: ['alice', 'bob'] };
    assert.equal(isOwnedByActiveAgent(session, 'alice'), true);
    assert.equal(isOwnedByActiveAgent(session, 'bob'), true);
    assert.equal(isOwnedByActiveAgent(session, 'charlie'), false);
});

test('cross-agent: isOwnedByActiveAgent is false for malformed cross-agent rows', () => {
    // Defensive: notification with no agent_name, DM with no participants.
    assert.equal(
        isOwnedByActiveAgent({ session_type: 'notification' }, 'alice'),
        false,
    );
    assert.equal(
        isOwnedByActiveAgent({ session_type: 'dm' }, 'alice'),
        false,
    );
    assert.equal(
        isOwnedByActiveAgent({ session_type: 'dm', participants: [] }, 'alice'),
        false,
    );
});

test('cross-agent: isOwnedByActiveAgent is false for non-cross-agent types', () => {
    // Chat / job / subagent rows are agent-keyed, not cross-agent — the
    // predicate is conservative and returns false rather than guessing.
    assert.equal(
        isOwnedByActiveAgent(
            { session_type: 'chat', agent_id: 'agent_a' },
            'alice',
        ),
        false,
    );
    assert.equal(
        isOwnedByActiveAgent(
            { session_type: 'subagent', agent_id: 'agent_a' },
            'alice',
        ),
        false,
    );
});

test('cross-agent: isOwnedByActiveAgent is false for missing / falsy active agent', () => {
    // Boot-time / all-agents-deleted state — no row should be marked
    // as "owned" when there's no active agent to compare against.
    const session = { session_type: 'notification', agent_name: 'alice' };
    assert.equal(isOwnedByActiveAgent(session, null), false);
    assert.equal(isOwnedByActiveAgent(session, undefined), false);
    assert.equal(isOwnedByActiveAgent(session, ''), false);
    assert.equal(isOwnedByActiveAgent(session, 42), false);
});

// ---------------------------------------------------------------------
// sortCrossAgentRows — active-agent-first stable sort
// ---------------------------------------------------------------------

test('cross-agent: sortCrossAgentRows pins active-agent rows above others', () => {
    // The headline case: backend returned [other-A, ours, other-B] in
    // last_activity DESC order. Result must be [ours, other-A, other-B]
    // — active-agent first, original order preserved within each bucket.
    const rows = [
        { id: 'r1', session_type: 'notification', agent_name: 'bob' },
        { id: 'r2', session_type: 'notification', agent_name: 'alice' },
        { id: 'r3', session_type: 'notification', agent_name: 'charlie' },
    ];
    const sorted = sortCrossAgentRows(rows, 'alice');
    assert.deepEqual(sorted.map(r => r.id), ['r2', 'r1', 'r3']);
});

test('cross-agent: sortCrossAgentRows is a stable sort within each bucket', () => {
    // Two active-agent rows + two other-agent rows; original order must
    // be preserved within each bucket so the backend's last_activity
    // DESC ordering survives the tiebreak.
    const rows = [
        { id: 'a1', session_type: 'notification', agent_name: 'alice' },
        { id: 'b1', session_type: 'notification', agent_name: 'bob' },
        { id: 'a2', session_type: 'notification', agent_name: 'alice' },
        { id: 'b2', session_type: 'notification', agent_name: 'bob' },
    ];
    const sorted = sortCrossAgentRows(rows, 'alice');
    assert.deepEqual(sorted.map(r => r.id), ['a1', 'a2', 'b1', 'b2']);
});

test('cross-agent: sortCrossAgentRows leaves order untouched when no active agent', () => {
    // No active agent → every row is in the "other" bucket → no
    // movement, original order preserved.
    const rows = [
        { id: 'r1', session_type: 'notification', agent_name: 'bob' },
        { id: 'r2', session_type: 'notification', agent_name: 'alice' },
    ];
    assert.deepEqual(
        sortCrossAgentRows(rows, null).map(r => r.id),
        ['r1', 'r2'],
    );
    assert.deepEqual(
        sortCrossAgentRows(rows, '').map(r => r.id),
        ['r1', 'r2'],
    );
});

test('cross-agent: sortCrossAgentRows handles DM rows alongside notifications', () => {
    // DMs and notifications mix in the same input — the predicate
    // matches participation for DMs and recipient-equality for
    // notifications, both bubble up.
    const rows = [
        { id: 'n_other',  session_type: 'notification', agent_name: 'bob' },
        { id: 'dm_ours',  session_type: 'dm',           participants: ['alice', 'bob'] },
        { id: 'n_ours',   session_type: 'notification', agent_name: 'alice' },
        { id: 'dm_other', session_type: 'dm',           participants: ['bob', 'charlie'] },
    ];
    const sorted = sortCrossAgentRows(rows, 'alice');
    assert.deepEqual(sorted.map(r => r.id), ['dm_ours', 'n_ours', 'n_other', 'dm_other']);
});

test('cross-agent: sortCrossAgentRows returns empty array for non-array input', () => {
    assert.deepEqual(sortCrossAgentRows(null, 'alice'), []);
    assert.deepEqual(sortCrossAgentRows(undefined, 'alice'), []);
    assert.deepEqual(sortCrossAgentRows('not an array', 'alice'), []);
});

test('cross-agent: sortCrossAgentRows does not mutate its input', () => {
    // Defensive: callers pass slices straight from signal values; the
    // helper must not reorder the source.
    const rows = [
        { id: 'r1', session_type: 'notification', agent_name: 'bob' },
        { id: 'r2', session_type: 'notification', agent_name: 'alice' },
    ];
    const before = rows.map(r => r.id);
    sortCrossAgentRows(rows, 'alice');
    assert.deepEqual(rows.map(r => r.id), before);
});

// ---------------------------------------------------------------------
// filterChatSessions — per-agent chat-only filter (PR #1100 Codex P2)
// ---------------------------------------------------------------------
//
// Pinned regression target:
//   - Codex P2 on PR #1100 — `/sessions` always returns notification
//     rows after the opt-in toggle removal, so `loadAgentSessions`
//     must filter them out before assigning to `sessions.value`. The
//     boot-flow chat-creation fallback branches on
//     `agentSessions.length > 0`; without this filter, an agent
//     whose only session activity is a notification skips the
//     fallback and lands on a read-only notification view instead
//     of starting a fresh chat. These tests pin the pure-function
//     half of the fix — `loadAgentSessions` itself is integration
//     territory (signal wiring, SSE streams, network) that can't be
//     unit-tested cleanly under Node.

test('#1100: filterChatSessions drops notification rows', () => {
    // The headline regression: a per-agent `/sessions` response with
    // only a notification must produce an empty chat-session list so
    // the boot-flow chat-creation fallback fires.
    const raw = [
        { id: 'n1', session_type: 'notification', agent_id: 'agent_a' },
    ];
    assert.deepEqual(filterChatSessions(raw), []);
});

test('#1100: filterChatSessions keeps chat rows untouched', () => {
    // Chat sessions are the per-agent surface — they survive the filter.
    const raw = [
        { id: 'c1', session_type: 'chat', agent_id: 'agent_a' },
        { id: 'c2', session_type: 'chat', agent_id: 'agent_a' },
    ];
    assert.deepEqual(
        filterChatSessions(raw).map(s => s.id),
        ['c1', 'c2'],
    );
});

test('#1100: filterChatSessions keeps chats and drops notifications when mixed', () => {
    // Realistic shape: an agent with both ordinary chats and an
    // incoming notification. Only the chats stay; notifications belong
    // to the cross-agent sidebar section sourced from
    // `crossAgentSessions`, not the per-agent `sessions` signal.
    const raw = [
        { id: 'c1', session_type: 'chat', agent_id: 'agent_a' },
        { id: 'n1', session_type: 'notification', agent_id: 'agent_a' },
        { id: 'c2', session_type: 'chat', agent_id: 'agent_a' },
    ];
    assert.deepEqual(
        filterChatSessions(raw).map(s => s.id),
        ['c1', 'c2'],
    );
});

test('#1100: filterChatSessions preserves backend sort order', () => {
    // The backend returns `last_activity DESC` — the filter must not
    // shuffle the surviving rows. Pin this so a future refactor that
    // swaps `filter` for something fancier can't silently regress the
    // sidebar's "most recent first" ordering.
    const raw = [
        { id: 'c3', session_type: 'chat', agent_id: 'agent_a' },
        { id: 'n1', session_type: 'notification', agent_id: 'agent_a' },
        { id: 'c1', session_type: 'chat', agent_id: 'agent_a' },
        { id: 'c2', session_type: 'chat', agent_id: 'agent_a' },
    ];
    assert.deepEqual(
        filterChatSessions(raw).map(s => s.id),
        ['c3', 'c1', 'c2'],
    );
});

test('#1100: filterChatSessions returns empty for non-array input', () => {
    // Defensive: `data.sessions` could in theory be `undefined` if the
    // backend ever omits the field. Empty array is the safe fallback —
    // the boot fallback then creates a fresh chat as intended.
    assert.deepEqual(filterChatSessions(null), []);
    assert.deepEqual(filterChatSessions(undefined), []);
    assert.deepEqual(filterChatSessions('not an array'), []);
    assert.deepEqual(filterChatSessions({}), []);
});

test('#1100: filterChatSessions skips null / non-object entries', () => {
    // Defense in depth — malformed entries in the payload should not
    // crash the filter. Drop them; keep valid chat rows.
    const raw = [
        null,
        { id: 'c1', session_type: 'chat', agent_id: 'agent_a' },
        undefined,
        { id: 'n1', session_type: 'notification', agent_id: 'agent_a' },
    ];
    assert.deepEqual(
        filterChatSessions(raw).map(s => s.id),
        ['c1'],
    );
});

test('#1100: filterChatSessions returns a new array (does not mutate input)', () => {
    // Callers pass slices straight from signal values; the helper must
    // not mutate the source list.
    const raw = [
        { id: 'c1', session_type: 'chat', agent_id: 'agent_a' },
        { id: 'n1', session_type: 'notification', agent_id: 'agent_a' },
    ];
    const before = raw.map(s => s.id);
    const out = filterChatSessions(raw);
    assert.deepEqual(raw.map(s => s.id), before);
    assert.notEqual(out, raw);
});

test('#1100: filterChatSessions models the "notification-only agent" boot edge case', () => {
    // Direct simulation of the Codex P2 scenario: an agent whose only
    // session activity on `/sessions` is a notification. The filter
    // returns an empty array, so `loadAgentSessions` sees
    // `agentSessions.length === 0` and enters the chat-creation
    // fallback — exactly the pre-PR-#1100 behaviour the operator
    // expects.
    const notificationOnlyResponse = [
        {
            id: 'notif-1',
            session_type: 'notification',
            agent_id: 'agent_a',
            agent_name: 'alice',
            has_active_run: false,
        },
    ];
    const agentSessions = filterChatSessions(notificationOnlyResponse);
    assert.equal(agentSessions.length, 0);
    // Mirror the branch condition in `loadAgentSessions` to make the
    // assertion's intent unambiguous: this is the predicate that gates
    // the chat-creation fallback.
    const wouldCreateFallbackChat = agentSessions.length === 0;
    assert.equal(wouldCreateFallbackChat, true);
});
