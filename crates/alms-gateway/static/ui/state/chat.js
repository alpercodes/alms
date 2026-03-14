import { signal } from 'https://esm.sh/@preact/signals@1.3.0';

// Chat messages for the active session.
// Entries: { type: 'user'|'agent'|'tool'|'approval'|'error'|'system', role?, text?, ... }
export const chatMessages = signal([]);
