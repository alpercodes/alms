import { get, post } from './client.js';

export const createRun = (body) => post('/runs', body);

export const getRun = (runId) => get(`/runs/${runId}`);

export const listRuns = (sessionId, limit = 20) =>
    get(`/runs?session_id=${sessionId}&limit=${limit}`);

export const cancelRun = (runId) => post(`/runs/${runId}/cancel`);
