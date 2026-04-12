import { html, batch, useSignal } from '../../deps.js';
import { sessions, activeSessionId, showNotifications } from '../../state/sessions.js';
import { activeAgentId } from '../../state/agents.js';
import { replaceMessages } from '../../state/chat-actions.js';
import { activeRunId, selectedRunId, runs } from '../../state/runs.js';
import { bgRuns, messageQueue } from '../../state/queue.js';
import { auditEvents } from '../../state/audit.js';
import { sessionSwitchLoading } from '../../state/loading.js';
import { listSessions, createSession, deleteSession } from '../../api/sessions.js';
import { openSessionStream, closeSessionStream } from '../../hooks/use-session-stream.js';
import { clearAllSubagents, parentSessionId } from '../../state/subagents.js';
import { saveActiveSession } from '../../hooks/use-boot.js';
import { selectGeneration, bumpSelectGeneration } from '../../state/select-generation.js';
import { loadSession } from '../../utils/load-session.js';
import { closeSidebar } from '../header.js';

/**
 * Session type metadata: icon character and CSS class suffix.
 */
const SESSION_TYPE_META = {
    chat:         { icon: '\u25B8', cls: '',              label: 'Chat session' },
    dm:           { icon: '\u2194', cls: 'dm',            label: 'DM conversation' },
    notification: { icon: '\u26A1', cls: 'notification',  label: 'Notification session' },
    job:          { icon: '\u23F0', cls: 'job',           label: 'Job session' },
    subagent:     { icon: '\u2699', cls: 'subagent',      label: 'Subagent session' },
    telegram:     { icon: '\u2709', cls: 'telegram',      label: 'Telegram session' },
};

function getSessionMeta(session) {
    return SESSION_TYPE_META[session.session_type] || SESSION_TYPE_META.chat;
}

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
    clearAllSubagents();
    parentSessionId.value = null; // clear breadcrumb on manual session switch
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
        const data = await listSessions(activeAgentId.value, {
            includeDms: true,
            includeNotifications: showNotifications.value,
        });
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
            clearAllSubagents();
        });
        openSessionStream(resp.session_id);
    } catch (err) {
        console.error('[newSession] failed:', err);
    }
}

/**
 * Format a DM session label from its participants array.
 * e.g. ["alice", "bob"] -> "alice <-> bob"
 */
function dmLabel(session) {
    const parts = session.participants;
    if (Array.isArray(parts) && parts.length >= 2) {
        return parts.join(' <-> ');
    }
    // Fallback: use context_id
    return session.context_id || session.id.slice(0, 8);
}

/**
 * Format a notification session label from agent_name or context_id.
 */
function notificationLabel(session) {
    if (session.agent_name) {
        return session.agent_name + ' notifications';
    }
    return session.context_id || session.id.slice(0, 8);
}

/**
 * Derive a human-readable label for any session type.
 */
function sessionLabel(session) {
    if (session.session_type === 'dm') return dmLabel(session);
    if (session.session_type === 'notification') return notificationLabel(session);
    return session.context_id || session.id.slice(0, 8);
}

function SessionItem({ session }) {
    const confirming = useSignal(false);
    const deleteTimer = useSignal(null);
    const isActive = session.id === activeSessionId.value;
    const meta = getSessionMeta(session);
    const typeClass = meta.cls ? ' session-item-' + meta.cls : '';

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
                    clearAllSubagents();
                });
            }
            // Refresh session list
            const data = await listSessions(activeAgentId.value, {
                includeDms: true,
                includeNotifications: showNotifications.value,
            });
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

    const label = sessionLabel(session);
    const typeLabel = session.session_type !== 'chat' ? '\nType: ' + session.session_type : '';

    return html`
        <div class="session-item${typeClass} ${isActive ? 'active' : ''} ${hasActiveRun(session.id) ? 'has-run' : ''}"
             role="option"
             aria-selected=${isActive}
             tabindex="0"
             title=${'ID: ' + session.id + '\nContext: ' + session.context_id + typeLabel}
             onClick=${() => selectSession(session.id)}
             onKeyDown=${(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); selectSession(session.id); } }}>
            ${session.session_type !== 'chat' && html`<span class="session-type-icon session-type-icon-${meta.cls || 'default'}" aria-hidden="true" title=${meta.label}>${meta.icon}</span>`}
            <span class="session-label">${label}</span>
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

/**
 * Toggle notification session visibility.  Persists the choice to
 * localStorage and reloads the session list from the API.
 */
async function toggleNotifications() {
    const next = !showNotifications.value;
    showNotifications.value = next;
    localStorage.setItem('alms_show_notifications', String(next));

    if (!activeAgentId.value) return;

    try {
        const data = await listSessions(activeAgentId.value, {
            includeDms: true,
            includeNotifications: next,
        });
        sessions.value = data.sessions || [];
    } catch (err) {
        console.error('[toggleNotifications] reload failed:', err);
    }
}

/**
 * Section divider with label and optional line, matching the DM divider style.
 */
function SectionDivider({ label, cls }) {
    return html`
        <div class="session-section-divider ${cls || ''}">
            <span class="session-section-divider-label">${label}</span>
        </div>
    `;
}

export function SessionList() {
    const allSessions = sessions.value;
    const chatSessions = allSessions.filter(s => s.session_type !== 'dm' && s.session_type !== 'notification');
    const dmSessions = allSessions.filter(s => s.session_type === 'dm');
    const notifSessions = allSessions.filter(s => s.session_type === 'notification');
    const showNotif = showNotifications.value;

    return html`
        <div class="sidebar-section" style="flex:0 0 auto">
            <div class="sidebar-label">
                Sessions
                <button class="notif-toggle ${showNotif ? 'active' : ''}"
                        title=${showNotif ? 'Hide notification sessions' : 'Show notification sessions'}
                        aria-pressed=${showNotif}
                        onClick=${toggleNotifications}>\u26A1</button>
            </div>
            <div id="session-list" role="listbox" aria-label="Sessions">
                ${chatSessions.length === 0 && dmSessions.length === 0 && notifSessions.length === 0
                    ? html`<div class="run-empty">No sessions</div>`
                    : null
                }
                ${chatSessions.map(s => html`
                    <${SessionItem} key=${s.id} session=${s} />
                `)}
                ${dmSessions.length > 0 && html`
                    <${SectionDivider} label="DM conversations" cls="session-divider-dm" />
                    ${dmSessions.map(s => html`
                        <${SessionItem} key=${s.id} session=${s} />
                    `)}
                `}
                ${showNotif && notifSessions.length > 0 && html`
                    <${SectionDivider} label="Notifications" cls="session-divider-notification" />
                    ${notifSessions.map(s => html`
                        <${SessionItem} key=${s.id} session=${s} />
                    `)}
                `}
            </div>
            <button id="new-session-btn" onClick=${newSession}>+ New session</button>
        </div>
    `;
}
