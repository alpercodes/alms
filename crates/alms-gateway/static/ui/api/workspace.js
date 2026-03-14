import { get, put } from './client.js';

export const getWorkspace = (agentId) =>
    get(`/agents/${agentId}/workspace`);

export const updateWorkspaceFile = (agentId, file, content) =>
    put(`/agents/${agentId}/workspace/${file}`, { content });
