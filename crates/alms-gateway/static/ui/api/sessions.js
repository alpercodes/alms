import { get, post, del } from './client.js';

export const listSessions = (agentId, opts) => {
    const params = new URLSearchParams();
    if (agentId) params.set('agent_id', agentId);
    if (opts && opts.includeDms) params.set('include_dms', 'true');
    const qs = params.toString();
    return get(`/sessions${qs ? '?' + qs : ''}`);
};

export const createSession = (agentId, contextId) =>
    post('/sessions', { agent_id: agentId, context_id: contextId });

export const getSessionMessages = (sessionId) =>
    get(`/sessions/${sessionId}/messages`);

export const deleteSession = (sessionId) =>
    del(`/sessions/${sessionId}`);
