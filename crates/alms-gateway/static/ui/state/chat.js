import { signal } from '../deps.js';

// Chat messages for the active session.
// Entries: { type: 'user'|'agent'|'tool'|'approval'|'error'|'system', role?, text?, ... }
export const chatMessages = signal([]);
