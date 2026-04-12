import { get, post, del } from './client.js';

export const listSessions = (agentId, opts) => {
    const params = new URLSearchParams();
    if (agentId) params.set('agent_id', agentId);
    if (opts && opts.includeDms) params.set('include_dms', 'true');
    if (opts && opts.includeNotifications) params.set('include_notifications', 'true');
    const qs = params.toString();
    return get(`/sessions${qs ? '?' + qs : ''}`);
};

export const createSession = (agentId, contextId) =>
    post('/sessions', { agent_id: agentId, context_id: contextId });

export const getSessionMessages = (sessionId) =>
    get(`/sessions/${sessionId}/messages`);

export const deleteSession = (sessionId) =>
    del(`/sessions/${sessionId}`);

/**
 * Fetch all tool call records across all runs for a session.
 * Returns { session_id, tool_calls: [...] } where each entry is a
 * flattened ToolCallRecord with an additional run_id field.
 */
export const getSessionToolCalls = (sessionId) =>
    get(`/sessions/${sessionId}/tool-calls`);
