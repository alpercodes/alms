import { html, batch } from '../../deps.js';
import { sessions, activeSessionId } from '../../state/sessions.js';
import { activeAgentId } from '../../state/agents.js';
import { replaceMessages } from '../../state/chat-actions.js';
import { activeRunId, runs } from '../../state/runs.js';
import { bgRuns, messageQueue } from '../../state/queue.js';
import { auditEvents } from '../../state/audit.js';
import { listSessions, createSession } from '../../api/sessions.js';
import { openSessionStream, closeSessionStream } from '../../hooks/use-session-stream.js';
import { saveActiveSession } from '../../hooks/use-boot.js';
import { selectGeneration, bumpSelectGeneration } from '../../state/select-generation.js';
import { loadSession } from '../../utils/load-session.js';
import { closeSidebar } from '../header.js';

function hasActiveRun(sessionId) {
    if (sessionId === activeSessionId.value && activeRunId.value) return true;
    const bg = bgRuns.value[sessionId];
    return bg && !bg.finished;
}

async function selectSession(sessionId) {
    if (sessionId === activeSessionId.value) return;

    closeSidebar(); // auto-close sidebar overlay on mobile

    const gen = bumpSelectGeneration();

    closeSessionStream();
    activeSessionId.value = sessionId;
    activeRunId.value = null;
    replaceMessages([]);
    messageQueue.value = [];
    auditEvents.value = null;

    // Persist the selection for this agent
    saveActiveSession(activeAgentId.value, sessionId);

    // Delegate the run/history/approval/SSE loading to the shared
    // loadSession() function, passing a stale-check callback tied
    // to the shared selectGeneration counter.
    await loadSession(sessionId, {
        isStale: () => gen !== selectGeneration,
        logPrefix: 'selectSession',
    });
}

async function newSession() {
    if (!activeAgentId.value) return;

    // Close old stream immediately and invalidate in-flight selectSession()
    // fetches so they do not overwrite the new session state.
    closeSessionStream();
    bumpSelectGeneration();

    try {
        const ctx = 'web-chat-' + Date.now();
        const resp = await createSession(activeAgentId.value, ctx);
        // Reload sessions
        const data = await listSessions(activeAgentId.value);
        batch(() => {
            sessions.value = data.sessions || [];
            activeSessionId.value = resp.session_id;
            saveActiveSession(activeAgentId.value, resp.session_id);
            activeRunId.value = null;
            replaceMessages([]);
            messageQueue.value = [];
            runs.value = [];
            auditEvents.value = null;
        });
        openSessionStream(resp.session_id);
    } catch (err) {
        console.error('[newSession] failed:', err);
    }
}

export function SessionList() {
    return html`
        <div class="sidebar-section" style="flex:0 0 auto">
            <div class="sidebar-label">Sessions</div>
            <div id="session-list">
                ${sessions.value.length === 0
                    ? html`<div class="run-empty">No sessions</div>`
                    : sessions.value.map(s => html`
                        <div class="session-item ${s.id === activeSessionId.value ? 'active' : ''} ${hasActiveRun(s.id) ? 'has-run' : ''}"
                             title=${'ID: ' + s.id + '\nContext: ' + s.context_id}
                             onClick=${() => selectSession(s.id)}>
                            ${s.context_id || s.id.slice(0, 8)}
                        </div>
                    `)
                }
            </div>
            <button id="new-session-btn" onClick=${newSession}>+ New session</button>
        </div>
    `;
}
