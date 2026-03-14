import { fetchSettings } from '../api/settings.js';
import { listSessions, createSession } from '../api/sessions.js';
import { getSessionMessages } from '../api/sessions.js';
import { agents, activeAgentId } from '../state/agents.js';
import { sessions, activeSessionId } from '../state/sessions.js';
import { serverDefaults } from '../state/settings.js';
import { chatMessages } from '../state/chat.js';

const AGENT_KEY = 'alms_active_agent';

/**
 * Boot sequence: load settings, agents, sessions, and chat history.
 * Called once on app mount.
 */
export async function boot() {
    try {
        const data = await fetchSettings();
        serverDefaults.value = data.defaults || {};
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
 * Load sessions for an agent, select the latest, and load its chat history.
 */
export async function loadAgentSessions(agentId) {
    try {
        const list = await listSessions(agentId);
        sessions.value = list;

        if (list.length > 0) {
            const latest = list[0];
            activeSessionId.value = latest.id;
            await loadHistory(latest.id);
        } else {
            // Create a first session
            const ctx = new Date().toISOString().slice(0, 16).replace('T', ' ');
            const sess = await createSession(agentId, ctx);
            sessions.value = [sess];
            activeSessionId.value = sess.id;
            chatMessages.value = [];
        }
    } catch (err) {
        console.error('[loadAgentSessions] failed:', err);
    }
}

/**
 * Load chat history for a session into chatMessages.
 */
async function loadHistory(sessionId) {
    try {
        const msgs = await getSessionMessages(sessionId);
        chatMessages.value = (msgs || []).map(m => ({
            type: m.role === 'user' ? 'user' : 'agent',
            role: m.role,
            text: m.content || '',
        }));
    } catch (err) {
        console.error('[loadHistory] failed:', err);
        chatMessages.value = [];
    }
}

/**
 * Switch to a different agent: update state, load sessions.
 */
export async function switchAgent(agentId) {
    activeAgentId.value = agentId;
    localStorage.setItem(AGENT_KEY, agentId);
    chatMessages.value = [];
    await loadAgentSessions(agentId);
}
