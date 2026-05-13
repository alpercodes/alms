import { get, post } from './client.js';

export const createRun = (body) => post('/runs', body);

export const getRun = (runId) => get(`/runs/${runId}`);

export const listRuns = (sessionId, limit = 20) =>
    get(`/runs?session_id=${sessionId}&limit=${limit}`);

export const cancelRun = (runId) => post(`/runs/${runId}/cancel`);

// Fetch accumulated extended-thinking ("reasoning") text for an in-flight
// run. Used by `loadSession` to rehydrate the reasoning panel on page
// reload (#1043). Returns `{ run_id, text, last_event_id }`. `text` may be
// empty when the run has produced no reasoning yet; `last_event_id` may be
// null in that case. When non-null, the caller should pass it as the SSE
// `last_event_id` so the live stream does not double-emit deltas that are
// already reflected in `text`.
export const getRunReasoning = (runId) => get(`/runs/${runId}/reasoning`);

export const listApprovals = (sessionId) =>
    get(`/approvals?session_id=${sessionId}`);

export const listAgentRuns = (agentId, limit = 50) =>
    get(`/runs?agent_id=${agentId}&limit=${limit}`);
