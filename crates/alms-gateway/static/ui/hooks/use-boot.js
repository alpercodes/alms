import { fetchSettings } from '../api/settings.js';
import { listSessions, createSession, getSessionMessages } from '../api/sessions.js';
import { mapHistoryMessages } from '../utils/history.js';
import { listRuns, listApprovals } from '../api/runs.js';
import { normalizeApproval } from '../utils/approvals.js';
import { agents, activeAgentId } from '../state/agents.js';
import { sessions, activeSessionId } from '../state/sessions.js';
import { activeRunId, runs } from '../state/runs.js';
import { serverDefaults } from '../state/settings.js';
import { chatMessages, nextMsgId } from '../state/chat.js';
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
            // Load runs first so activeRunId is set before history loads.
            // This lets mapHistoryMessages mark in-progress tool_calls as
            // 'running' instead of incorrectly defaulting to 'done'.
            await loadRunHistory(selected.id);
            if (gen !== switchGeneration) return; // stale — discard
            const lastEventId = await loadHistory(selected.id);
            if (gen !== switchGeneration) return; // stale — discard
            // If a run is in-progress, append a thinking indicator and
            // reconstruct pending approval prompts from the server so the
            // user can still approve/deny waiting tool calls. (Fixes #487 Bug 2)
            if (activeRunId.value) {
                if (!chatMessages.value.some(m => m.type === 'thinking')) {
                    chatMessages.value = [...chatMessages.value, { id: nextMsgId(), type: 'thinking' }];
                }
                try {
                    const approvalData = await listApprovals(selected.id);
                    if (gen !== switchGeneration) return; // stale — discard
                    const pending = approvalData.approvals || [];
                    if (pending.length > 0) {
                        const approvalMsgs = pending.map(a => {
                            const norm = normalizeApproval(a);
                            return {
                                id: nextMsgId(),
                                type: 'approval',
                                approvalId: norm.approvalId,
                                tool: norm.tool,
                                params: norm.params,
                                runId: norm.runId,
                                resolved: false,
                            };
                        });
                        chatMessages.value = [...chatMessages.value, ...approvalMsgs];
                    }
                } catch (err) {
                    console.warn('[loadAgentSessions] Failed to load pending approvals:', err);
                }
            }
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
        chatMessages.value = mapHistoryMessages(data.messages || [], {
            hasActiveRun: !!activeRunId.value,
        });
        return data.last_event_id ?? null;
    } catch (err) {
        console.error('[loadHistory] failed:', err);
        chatMessages.value = [{ id: nextMsgId(), type: 'error', text: `Failed to load message history: ${err.error?.message || err.message || 'unknown error'}` }];
        return null;
    }
}

/**
 * Load run history for a session.
 *
 * If any run is still queued or running, restores `activeRunId` so the
 * caller (loadAgentSessions) can append a thinking indicator after both
 * history and runs have loaded.  This covers page refresh and session
 * switch scenarios where the SSE `run_created` event has already been
 * sent and won't be replayed.
 */
async function loadRunHistory(sessionId) {
    try {
        const data = await listRuns(sessionId);
        const loaded = data.runs || [];
        runs.value = loaded;

        // Restore activeRunId from any in-progress run.
        const active = loaded.find(r => r.status === 'queued' || r.status === 'running');
        if (active) {
            activeRunId.value = active.run_id;
        }
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
