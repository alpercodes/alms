import { html, batch, useSignal } from '../../deps.js';
import { sessions, activeSessionId } from '../../state/sessions.js';
import { activeAgentId } from '../../state/agents.js';
import { replaceMessages } from '../../state/chat-actions.js';
import { activeRunId, selectedRunId, runs } from '../../state/runs.js';
import { bgRuns, messageQueue } from '../../state/queue.js';
import { auditEvents } from '../../state/audit.js';
import { sessionSwitchLoading } from '../../state/loading.js';
import { listSessions, createSession, deleteSession } from '../../api/sessions.js';
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
    selectedRunId.value = null;
    replaceMessages([]);
    messageQueue.value = [];
    auditEvents.value = null;
    sessionSwitchLoading.value = true;

    // Persist the selection for this agent
    saveActiveSession(activeAgentId.value, sessionId);

    // Delegate the run/history/approval/SSE loading to the shared
    // loadSession() function, passing a stale-check callback tied
    // to the shared selectGeneration counter.
    try {
        await loadSession(sessionId, {
            isStale: () => gen !== selectGeneration,
            logPrefix: 'selectSession',
        });
    } finally {
        if (gen === selectGeneration) {
            sessionSwitchLoading.value = false;
        }
    }
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
            selectedRunId.value = null;
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

function SessionItem({ session }) {
    const confirming = useSignal(false);
    const deleteTimer = useSignal(null);
    const isActive = session.id === activeSessionId.value;

    const onDeleteClick = (e) => {
        e.stopPropagation();
        confirming.value = true;
        deleteTimer.value = setTimeout(() => { confirming.value = false; }, 3000);
    };

    const onDeleteConfirm = async (e) => {
        e.stopPropagation();
        if (deleteTimer.value) { clearTimeout(deleteTimer.value); deleteTimer.value = null; }
        confirming.value = false;
        try {
            await deleteSession(session.id);
            // If we deleted the active session, clear it
            if (session.id === activeSessionId.value) {
                closeSessionStream();
                batch(() => {
                    activeSessionId.value = null;
                    activeRunId.value = null;
                    selectedRunId.value = null;
                    replaceMessages([]);
                    runs.value = [];
                    auditEvents.value = null;
                });
            }
            // Refresh session list
            const data = await listSessions(activeAgentId.value);
            sessions.value = data.sessions || [];
        } catch (err) {
            console.error('[deleteSession] failed:', err);
        }
    };

    const onDeleteCancel = (e) => {
        e.stopPropagation();
        if (deleteTimer.value) { clearTimeout(deleteTimer.value); deleteTimer.value = null; }
        confirming.value = false;
    };

    return html`
        <div class="session-item ${isActive ? 'active' : ''} ${hasActiveRun(session.id) ? 'has-run' : ''}"
             role="option"
             aria-selected=${isActive}
             tabindex="0"
             title=${'ID: ' + session.id + '\nContext: ' + session.context_id}
             onClick=${() => selectSession(session.id)}
             onKeyDown=${(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); selectSession(session.id); } }}>
            <span class="session-label">${session.context_id || session.id.slice(0, 8)}</span>
            ${confirming.value
                ? html`
                    <button class="session-delete-btn session-delete-confirm"
                            title="Confirm delete"
                            onClick=${onDeleteConfirm}
                            onKeyDown=${(e) => { if (e.key === 'Enter') { e.preventDefault(); onDeleteConfirm(e); } }}>Yes</button>
                    <button class="session-delete-btn"
                            title="Cancel"
                            onClick=${onDeleteCancel}
                            onKeyDown=${(e) => { if (e.key === 'Enter') { e.preventDefault(); onDeleteCancel(e); } }}>No</button>
                `
                : html`
                    <button class="session-delete-btn"
                            title="Delete session"
                            onClick=${onDeleteClick}
                            onKeyDown=${(e) => { if (e.key === 'Enter') { e.preventDefault(); onDeleteClick(e); } }}>\u00D7</button>
                `
            }
        </div>
    `;
}

export function SessionList() {
    return html`
        <div class="sidebar-section" style="flex:0 0 auto">
            <div class="sidebar-label">Sessions</div>
            <div id="session-list" role="listbox" aria-label="Sessions">
                ${sessions.value.length === 0
                    ? html`<div class="run-empty">No sessions</div>`
                    : sessions.value.map(s => html`
                        <${SessionItem} key=${s.id} session=${s} />
                    `)
                }
            </div>
            <button id="new-session-btn" onClick=${newSession}>+ New session</button>
        </div>
    `;
}
