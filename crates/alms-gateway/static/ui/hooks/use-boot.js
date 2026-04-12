import { fetchSettings } from '../api/settings.js';
import { listSessions, createSession } from '../api/sessions.js';
import { agents, activeAgentId } from '../state/agents.js';
import { sessions, activeSessionId, showNotifications } from '../state/sessions.js';
import { activeRunId, selectedRunId, runs } from '../state/runs.js';
import { serverDefaults } from '../state/settings.js';
import { replaceMessages } from '../state/chat-actions.js';
import { messageQueue } from '../state/queue.js';
import { wsFiles } from '../state/workspace.js';
import { auditEvents } from '../state/audit.js';
import { agentSwitchLoading } from '../state/loading.js';
import { openSessionStream, closeSessionStream } from './use-session-stream.js';
import { bumpSelectGeneration } from '../state/select-generation.js';
import { loadSession } from '../utils/load-session.js';
import { clearAllSubagents } from '../state/subagents.js';

const AGENT_KEY = 'alms_active_agent';

/**
 * Generation counter for loadAgentSessions() concurrency guard.
 * Bumped at the start of each loadAgentSessions() call so that
 * rapid agent switches (A -> B -> A) discard stale fetches.
 */
let switchGeneration = 0;

function sessionStorageKey(agentId) {
    return `alms_active_session_${agentId}`;
}

export function saveActiveSession(agentId, sessionId) {
    if (agentId && sessionId) {
        localStorage.setItem(sessionStorageKey(agentId), sessionId);
    }
}

function loadActiveSession(agentId, agentSessions) {
    const stored = localStorage.getItem(sessionStorageKey(agentId));
    if (stored) {
        const match = agentSessions.find(s => s.id === stored);
        if (match) return match;
    }
    return agentSessions[0] || null;
}

/**
 * Boot sequence: load settings, agents, sessions, and chat history.
 */
export async function boot() {
    try {
        const data = await fetchSettings();
        serverDefaults.value = data;
        agents.value = data.agents || [];

        // Determine active agent: localStorage > default > first
        const saved = localStorage.getItem(AGENT_KEY);
        const defaultAgent = agents.value.find(a => a.is_default);
        const firstAgent = agents.value[0];
        const agent = agents.value.find(a => a.id === saved) || defaultAgent || firstAgent;

        if (agent) {
            activeAgentId.value = agent.id;
            localStorage.setItem(AGENT_KEY, agent.id);
            await loadAgentSessions(agent.id);
        }
    } catch (err) {
        console.error('[boot] failed:', err);
        throw err;
    }
}

/**
 * Load sessions for an agent, select the latest, load its history + runs.
 */
async function loadAgentSessions(agentId) {
    const gen = ++switchGeneration;

    try {
        const data = await listSessions(agentId, {
            includeDms: true,
            includeNotifications: showNotifications.value,
        });
        if (gen !== switchGeneration) return; // stale — discard
        const agentSessions = data.sessions || [];
        const dmCount = agentSessions.filter(s => s.session_type === 'dm').length;
        const notifCount = agentSessions.filter(s => s.session_type === 'notification').length;
        if (dmCount > 0 || notifCount > 0) {
            console.debug('[loadAgentSessions] loaded', agentSessions.length, 'sessions,', dmCount, 'DM,', notifCount, 'notification');
        }
        sessions.value = agentSessions;

        if (agentSessions.length > 0) {
            const selected = loadActiveSession(agentId, agentSessions);
            activeSessionId.value = selected.id;
            // Re-persist in case the session list changed
            saveActiveSession(agentId, selected.id);
            // Delegate the run/history/approval/SSE loading to the shared
            // loadSession() function, passing a stale-check callback tied
            // to this function's local switchGeneration counter.
            await loadSession(selected.id, {
                isStale: () => gen !== switchGeneration,
                logPrefix: 'loadAgentSessions',
            });
        } else {
            // Create a first session
            const ctx = 'web-chat-' + Date.now();
            const resp = await createSession(agentId, ctx);
            if (gen !== switchGeneration) return; // stale — discard
            const reloaded = await listSessions(agentId, {
                includeDms: true,
                includeNotifications: showNotifications.value,
            });
            if (gen !== switchGeneration) return; // stale — discard
            sessions.value = reloaded.sessions || [];
            activeSessionId.value = resp.session_id;
            replaceMessages([]);
            runs.value = [];
            // Open persistent session stream
            openSessionStream(resp.session_id);
        }
    } catch (err) {
        if (gen !== switchGeneration) return; // stale — discard
        console.error('[loadAgentSessions] failed:', err);
    }
}

/**
 * Switch to a different agent: reset state, load sessions.
 */
export async function switchAgent(agentId) {
    const agent = agents.value.find(a => a.id === agentId);
    if (!agent) return;

    closeSessionStream(); // close previous session stream
    bumpSelectGeneration(); // invalidate any in-flight selectSession() fetches

    activeAgentId.value = agentId;
    localStorage.setItem(AGENT_KEY, agentId);
    agentSwitchLoading.value = true;

    // Reset all state
    activeSessionId.value = null;
    activeRunId.value = null;
    selectedRunId.value = null;
    sessions.value = [];
    runs.value = [];
    replaceMessages([]);
    messageQueue.value = [];
    wsFiles.value = null;
    auditEvents.value = null;
    clearAllSubagents();

    // loadAgentSessions() bumps switchGeneration synchronously (before its
    // first await), so we start the call, then read the updated counter.
    const promise = loadAgentSessions(agentId);
    const gen = switchGeneration;
    try {
        await promise;
    } finally {
        if (gen === switchGeneration) {
            agentSwitchLoading.value = false;
        }
    }
}
