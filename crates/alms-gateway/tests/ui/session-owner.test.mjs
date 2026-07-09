// Pinned regression for issue #1212: a job session (owned by agent A)
// showed a peer's name on its assistant messages because the label fell
// back to `activeAgent` — and opening a job session from the cross-agent
// Jobs sidebar group deliberately does NOT switch the active agent
// (see utils/navigate-session.js), so `activeAgent` can point at any
// other agent the operator had selected.
//
// The fix derives attribution from the SESSION'S owner instead:
// `utils/session-owner.js::sessionOwnerName(session, agents)`, consumed
// via the `activeSessionOwnerName` computed in state/sessions.js with an
// `activeAgent` fallback. These tests exercise the pure helper — the
// computed and the Message/app.js consumers are one Preact-signals layer
// above, which isn't vendored into the Node test runtime (same split as
// agent-name.test.mjs).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import url from 'node:url';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const OWNER_UTIL_PATH = path.resolve(
    __dirname,
    '../../static/ui/utils/session-owner.js'
);

const { sessionOwnerName } = await import(
    url.pathToFileURL(OWNER_UTIL_PATH).href
);

const NIL_UUID = '00000000-0000-0000-0000-000000000000';

const AGENTS = [
    { id: 'agent-a-id', name: 'alice' },
    { id: 'agent-b-id', name: 'bob' },
];

test('#1212: job session resolves to the OWNING agent, not the active one', () => {
    // The headline case: alice's job session opened while bob is the
    // active agent. The owner derivation must return alice — the caller
    // only falls back to activeAgent (bob) when this returns null.
    const jobSession = {
        id: 'sess-1',
        agent_id: 'agent-a-id',
        context_id: 'job_123e4567-e89b-12d3-a456-426614174000',
        session_type: 'job',
    };
    assert.equal(sessionOwnerName(jobSession, AGENTS), 'alice');
});

test('#1212: ordinary chat session resolves to its own agent (no behaviour change)', () => {
    // For a normal chat session the owner IS the active agent, so the
    // owner-first label is identical to the pre-fix label.
    const chatSession = {
        id: 'sess-2',
        agent_id: 'agent-b-id',
        context_id: 'web-chat',
        session_type: 'chat',
    };
    assert.equal(sessionOwnerName(chatSession, AGENTS), 'bob');
});

test('#1212: notification session prefers the backend-enriched agent_name', () => {
    // Notification envelopes carry agent_name extracted from the
    // `notifications:{agent}` context_id — authoritative even if the
    // agent_id lookup would also resolve.
    const notifSession = {
        id: 'sess-3',
        agent_id: 'agent-a-id',
        agent_name: 'alice',
        context_id: 'notifications:alice',
        session_type: 'notification',
    };
    assert.equal(sessionOwnerName(notifSession, AGENTS), 'alice');
});

test('#1212: DM session (nil-sentinel agent_id) has no single owner', () => {
    // DM sessions are stored under AgentId::nil(), which never matches a
    // real agent. Returning null keeps the DM view's per-message
    // fromAgent / participants attribution authoritative — the fix must
    // NOT regress DM-perspective rendering.
    const dmSession = {
        id: 'sess-4',
        agent_id: NIL_UUID,
        context_id: 'dm:alice:bob',
        session_type: 'dm',
        participants: ['alice', 'bob'],
    };
    assert.equal(sessionOwnerName(dmSession, AGENTS), null);
});

test('#1212: unresolvable agent_id returns null (caller falls back)', () => {
    // Deleted agent / stale envelope: fall back to the previous
    // activeAgent-based behaviour rather than rendering a blank label.
    const orphan = {
        id: 'sess-5',
        agent_id: 'gone-agent-id',
        context_id: 'job_dead',
        session_type: 'job',
    };
    assert.equal(sessionOwnerName(orphan, AGENTS), null);
});

test('#1212: null/missing inputs are handled defensively', () => {
    assert.equal(sessionOwnerName(null, AGENTS), null);
    assert.equal(sessionOwnerName(undefined, AGENTS), null);
    // Session with no agent_id at all (legacy shape).
    assert.equal(sessionOwnerName({ id: 'sess-6', context_id: 'web' }, AGENTS), null);
    // Agents list not loaded yet (boot race).
    const jobSession = { id: 'sess-7', agent_id: 'agent-a-id', context_id: 'job_x' };
    assert.equal(sessionOwnerName(jobSession, []), null);
    assert.equal(sessionOwnerName(jobSession, null), null);
});
