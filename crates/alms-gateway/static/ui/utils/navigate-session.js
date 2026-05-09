// Shared session-navigation helper.
//
// Three call sites previously duplicated this exact dance:
//   - components/sidebar/session-list.js  (`selectSession`)
//   - components/panel/runs-tab.js        (`navigateToSession`)
//   - utils/tool-output.js                (formerly an `<a href="?session_id=...">`
//                                          that the SPA ignored — see #952 review)
//
// Centralising it here means:
//   1. tool-output's "View full session" link routes through the same
//      in-app navigator as the sidebar — no more hard browser navigation
//      that the SPA boot path silently rewrites back to the default
//      session.
//   2. selectGeneration / SSE teardown / loadSession plumbing lives in
//      one place, so the three call sites can't drift.
//
// The behaviour matches the previous `navigateToSession` implementation
// in runs-tab.js, plus the `closeSidebar()` call that session-list.js
// did before delegating (no-op when the sidebar isn't open, important
// when navigating from the chat surface on mobile).

import { batch } from '../deps.js';
import { activeSessionId, sessions, crossAgentSessions } from '../state/sessions.js';
import { activeRunId, selectedRunId } from '../state/runs.js';
import { replaceMessages } from '../state/chat-actions.js';
import { messageQueue } from '../state/queue.js';
import { auditEvents } from '../state/audit.js';
import { sessionSwitchLoading } from '../state/loading.js';
import { activeAgentId, agents } from '../state/agents.js';
import { clearAllSubagents, parentSessionId } from '../state/subagents.js';
import { closeSessionStream } from '../hooks/use-session-stream.js';
import { saveActiveSession, switchAgent } from '../hooks/use-boot.js';
import { selectGeneration, bumpSelectGeneration } from '../state/select-generation.js';
import { loadSession } from './load-session.js';
import { closeSidebar } from '../components/header.js';

/**
 * Navigate to `sessionId` via the same in-app path used by the sidebar
 * and runs-tab. Safe to call from any handler — early-returns if
 * `sessionId` is already active.
 *
 * @param {string} sessionId
 * @param {{ logPrefix?: string }} [opts]
 */
export async function navigateToSession(sessionId, opts) {
    if (!sessionId) return;
    if (sessionId === activeSessionId.value) return;

    closeSidebar(); // no-op when the sidebar overlay isn't open

    // Cross-agent navigation auto-switch (Tim re-review on PR #1010,
    // upgrades the previous `console.warn` from the same review path).
    //
    // The accordion ties expansion to `activeAgentId`, so navigating
    // to a chat session whose `agent_id` doesn't match the active
    // agent would leave the session highlighted inside a collapsed
    // accordion group with no visible cue. Now we silently switch the
    // active agent to the session's owner before loading it; the
    // operator's accordion expansion follows because expansion is
    // derived from `activeAgentId` (see session-list.js + use-boot.js).
    //
    // DM / notification surfaces are explicitly skipped — they live
    // in their own cross-agent sections regardless of active agent,
    // so clicking them shouldn't yank the active agent around (the
    // operator might be reading alice's chat and quickly peeking at
    // a DM-from-bob; jumping to bob would be intrusive).
    //
    // Side-effect note: `switchAgent` resets messages / queue / runs
    // / subagents and bumps `selectGeneration`. We then return early —
    // `switchAgent` takes care of loading the targeted session via
    // its `targetSessionId` opt, so re-running the loadSession path
    // below would race against the freshly opened session stream.
    const target =
        sessions.value.find(s => s.id === sessionId)
        || crossAgentSessions.value.find(s => s.id === sessionId);
    if (target
        && target.session_type !== 'dm'
        && target.session_type !== 'notification'
        && target.agent_id
        && activeAgentId.value
        && target.agent_id !== activeAgentId.value
        && agents.value.some(a => a.id === target.agent_id)) {
        await switchAgent(target.agent_id, { targetSessionId: sessionId });
        return;
    }

    const gen = bumpSelectGeneration();

    closeSessionStream();
    batch(() => {
        activeSessionId.value = sessionId;
        activeRunId.value = null;
        selectedRunId.value = null;
        replaceMessages([]);
        messageQueue.value = [];
        auditEvents.value = null;
        clearAllSubagents();
        parentSessionId.value = null;
        sessionSwitchLoading.value = true;
    });

    saveActiveSession(activeAgentId.value, sessionId);

    try {
        await loadSession(sessionId, {
            isStale: () => gen !== selectGeneration,
            logPrefix: (opts && opts.logPrefix) || 'navigateToSession',
        });
    } finally {
        if (gen === selectGeneration) {
            sessionSwitchLoading.value = false;
        }
    }
}
