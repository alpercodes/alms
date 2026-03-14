import { get, post } from './client.js';

export const listSessions = (agentId) =>
    get(`/sessions${agentId ? `?agent_id=${agentId}` : ''}`);

export const createSession = (agentId, contextId) =>
    post('/sessions', { agent_id: agentId, context_id: contextId });

export const getSessionMessages = (sessionId) =>
    get(`/sessions/${sessionId}/messages`);
