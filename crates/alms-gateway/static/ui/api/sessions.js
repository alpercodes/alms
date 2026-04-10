import { get, post, del } from './client.js';

export const listSessions = (agentId) =>
    get(`/sessions${agentId ? `?agent_id=${agentId}` : ''}`);

export const createSession = (agentId, contextId) =>
    post('/sessions', { agent_id: agentId, context_id: contextId });

export const getSessionMessages = (sessionId) =>
    get(`/sessions/${sessionId}/messages`);

export const deleteSession = (sessionId) =>
    del(`/sessions/${sessionId}`);
