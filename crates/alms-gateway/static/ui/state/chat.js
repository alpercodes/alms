import { signal } from '../deps.js';

// Chat messages for the active session.
// Entries: { type: 'user'|'agent'|'tool'|'approval'|'error'|'warning'|'system'|'thinking'|'tokens', ... }
export const chatMessages = signal([]);
