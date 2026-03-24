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
import { bumpSelectGeneration } from '../state/select-generation.js';

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
    }
}

/**
 * Load sessions for an agent, select the latest, load its history + runs.
 */
async function loadAgentSessions(agentId) {
    const gen = ++switchGeneration;

    try {
        const data = await listSessions(agentId);
        if (gen !== switchGeneration) return; // stale — discard
        const agentSessions = data.sessions || [];
        sessions.value = agentSessions;

        if (agentSessions.length > 0) {
            const selected = loadActiveSession(agentId, agentSessions);
            activeSessionId.value = selected.id;
            // Re-persist in case the session list changed
            saveActiveSession(agentId, selected.id);
            const [lastEventId] = await Promise.all([
                loadHistory(selected.id),
                loadRunHistory(selected.id),
            ]);
            if (gen !== switchGeneration) return; // stale — discard
            // Open persistent session stream — skip replay of events
            // already reflected in the loaded message history.
            openSessionStream(selected.id, { lastEventId });
        } else {
            // Create a first session
            const ctx = 'web-chat-' + Date.now();
            const resp = await createSession(agentId, ctx);
            if (gen !== switchGeneration) return; // stale — discard
            const reloaded = await listSessions(agentId);
            if (gen !== switchGeneration) return; // stale — discard
            sessions.value = reloaded.sessions || [];
            activeSessionId.value = resp.session_id;
            chatMessages.value = [];
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
 * Load chat history for a session.
 * Returns the last SSE event ID from the server (if available) so the
 * caller can pass it to openSessionStream and skip duplicate replay.
 */
async function loadHistory(sessionId) {
    try {
        const data = await getSessionMessages(sessionId);
        chatMessages.value = mapHistoryMessages(data.messages || []);
        return data.last_event_id ?? null;
    } catch (err) {
        console.error('[loadHistory] failed:', err);
        chatMessages.value = [{ type: 'error', text: `Failed to load message history: ${err.message || 'unknown error'}` }];
        return null;
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
    bumpSelectGeneration(); // invalidate any in-flight selectSession() fetches

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
