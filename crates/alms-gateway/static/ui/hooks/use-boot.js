import { fetchSettings } from '../api/settings.js';
import { listSessions, createSession, getSessionMessages } from '../api/sessions.js';
import { mapHistoryMessages } from '../utils/history.js';
import { listRuns } from '../api/runs.js';
import { agents, activeAgentId } from '../state/agents.js';
import { sessions, activeSessionId } from '../state/sessions.js';
import { activeRunId, runs } from '../state/runs.js';
import { serverDefaults } from '../state/settings.js';
import { chatMessages } from '../state/chat.js';
import { messageQueue, bgRuns } from '../state/queue.js';
import { wsFiles } from '../state/workspace.js';
import { auditEvents } from '../state/audit.js';
import { openSessionStream, closeSessionStream } from './use-session-stream.js';

const AGENT_KEY = 'alms_active_agent';

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
    }
}

/**
 * Load sessions for an agent, select the latest, load its history + runs.
 */
async function loadAgentSessions(agentId) {
    try {
        const data = await listSessions(agentId);
        const agentSessions = data.sessions || [];
        sessions.value = agentSessions;

        if (agentSessions.length > 0) {
            const selected = loadActiveSession(agentId, agentSessions);
            activeSessionId.value = selected.id;
            saveActiveSession(agentId, selected.id);
            await Promise.all([
                loadHistory(selected.id),
                loadRunHistory(selected.id),
            ]);
            // Open persistent session stream
            openSessionStream(selected.id);
        } else {
            // Create a first session
            const ctx = 'web-chat-' + Date.now();
            const resp = await createSession(agentId, ctx);
            const reloaded = await listSessions(agentId);
            sessions.value = reloaded.sessions || [];
            activeSessionId.value = resp.session_id;
            chatMessages.value = [];
            runs.value = [];
            // Open persistent session stream
            openSessionStream(resp.session_id);
        }
    } catch (err) {
        console.error('[loadAgentSessions] failed:', err);
    }
}

/**
 * Load chat history for a session.
 */
async function loadHistory(sessionId) {
    try {
        const data = await getSessionMessages(sessionId);
        chatMessages.value = mapHistoryMessages(data.messages || []);
    } catch (err) {
        console.error('[loadHistory] failed:', err);
        chatMessages.value = [{ type: 'error', text: `Failed to load message history: ${err.message || 'unknown error'}` }];
    }
}

/**
 * Load run history for a session.
 */
async function loadRunHistory(sessionId) {
    try {
        const data = await listRuns(sessionId);
        runs.value = data.runs || [];
    } catch {
        runs.value = [];
    }
}

/**
 * Switch to a different agent: reset state, load sessions.
 */
export async function switchAgent(agentId) {
    const agent = agents.value.find(a => a.id === agentId);
    if (!agent) return;

    closeSessionStream(); // close previous session stream

    activeAgentId.value = agentId;
    localStorage.setItem(AGENT_KEY, agentId);

    // Reset all state
    activeSessionId.value = null;
    activeRunId.value = null;
    sessions.value = [];
    runs.value = [];
    chatMessages.value = [];
    messageQueue.value = [];
    wsFiles.value = null;
    auditEvents.value = null;

    await loadAgentSessions(agentId);
}
