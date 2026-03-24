import { html } from '../../deps.js';
import { sessions, activeSessionId } from '../../state/sessions.js';
import { activeAgentId } from '../../state/agents.js';
import { chatMessages } from '../../state/chat.js';
import { activeRunId, runs } from '../../state/runs.js';
import { bgRuns } from '../../state/queue.js';
import { auditEvents } from '../../state/audit.js';
import { listSessions, createSession, getSessionMessages } from '../../api/sessions.js';
import { listRuns } from '../../api/runs.js';
import { mapHistoryMessages } from '../../utils/history.js';
import { openSessionStream, closeSessionStream } from '../../hooks/use-session-stream.js';
import { saveActiveSession } from '../../hooks/use-boot.js';
import { selectGeneration, bumpSelectGeneration } from '../../state/select-generation.js';

function hasActiveRun(sessionId) {
    if (sessionId === activeSessionId.value && activeRunId.value) return true;
    const bg = bgRuns.value[sessionId];
    return bg && !bg.finished;
}

async function selectSession(sessionId) {
    if (sessionId === activeSessionId.value) return;

    const gen = bumpSelectGeneration();

    closeSessionStream();
    activeSessionId.value = sessionId;
    activeRunId.value = null;
    chatMessages.value = [];
    auditEvents.value = null;

    // Persist the selection for this agent
    saveActiveSession(activeAgentId.value, sessionId);

    // Load runs for new session
    try {
        const data = await listRuns(sessionId);
        if (gen !== selectGeneration) return; // stale — discard
        runs.value = data.runs || [];
    } catch {
        if (gen !== selectGeneration) return;
        runs.value = [];
    }

    // Load chat history
    let lastEventId = null;
    try {
        const data = await getSessionMessages(sessionId);
        if (gen !== selectGeneration) return; // stale — discard
        chatMessages.value = mapHistoryMessages(data.messages || []);
        // Capture the SSE high-water mark so we can skip replay of events
        // that are already reflected in the loaded messages.
        if (data.last_event_id != null) lastEventId = data.last_event_id;
    } catch (err) {
        if (gen !== selectGeneration) return;
        chatMessages.value = [{ type: 'error', text: `Failed to load message history: ${err.message || 'unknown error'}` }];
    }

    if (gen !== selectGeneration) return; // final guard before opening stream
    openSessionStream(sessionId, { lastEventId });
}

async function newSession() {
    if (!activeAgentId.value) return;
    try {
        const ctx = 'web-chat-' + Date.now();
        const resp = await createSession(activeAgentId.value, ctx);
        // Reload sessions
        const data = await listSessions(activeAgentId.value);
        sessions.value = data.sessions || [];
        activeSessionId.value = resp.session_id;
        saveActiveSession(activeAgentId.value, resp.session_id);
        activeRunId.value = null;
        chatMessages.value = [];
        runs.value = [];
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
