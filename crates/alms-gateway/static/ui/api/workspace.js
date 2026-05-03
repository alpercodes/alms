import { get, post, put } from './client.js';

export const getWorkspace = (agentId) =>
    get(`/agents/${agentId}/workspace`);

export const updateWorkspaceFile = (agentId, file, content) =>
    put(`/agents/${agentId}/workspace/${file}`, { content });

// #858: ask the gateway to open the agent's workspace directory in the host
// file explorer (Windows Explorer / Finder / xdg-open). The endpoint takes
// no body — `agentId` is the only input, the workspace path is resolved
// server-side so the browser cannot smuggle a free-form path.
export const openWorkspaceInExplorer = (agentId) =>
    post(`/agents/${agentId}/workspace/open`, {});
