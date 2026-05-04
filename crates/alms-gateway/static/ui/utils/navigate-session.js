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
import { activeSessionId } from '../state/sessions.js';
import { activeRunId, selectedRunId } from '../state/runs.js';
import { replaceMessages } from '../state/chat-actions.js';
import { messageQueue } from '../state/queue.js';
import { auditEvents } from '../state/audit.js';
import { sessionSwitchLoading } from '../state/loading.js';
import { activeAgentId } from '../state/agents.js';
import { clearAllSubagents, parentSessionId } from '../state/subagents.js';
import { closeSessionStream } from '../hooks/use-session-stream.js';
import { saveActiveSession } from '../hooks/use-boot.js';
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
