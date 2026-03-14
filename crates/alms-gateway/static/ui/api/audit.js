import { get } from './client.js';

export const getAudit = (sessionId, limit = 50) =>
    get(`/audit?session_id=${sessionId}&limit=${limit}`);
