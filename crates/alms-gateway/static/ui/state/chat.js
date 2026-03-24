import { signal } from '../deps.js';

// Chat messages for the active session.
// Entries: { id: string, type: 'user'|'agent'|'tool'|'approval'|'error'|'warning'|'system'|'thinking'|'tokens', ... }
export const chatMessages = signal([]);

// Monotonically increasing counter for stable message IDs.
// Using a counter (rather than crypto.randomUUID()) is intentional: it is
// fast, deterministic within a session, and avoids key collisions that would
// occur with index-based keys when messages are inserted or removed.
let _msgSeq = 0;

/**
 * Generate a unique, stable message ID.
 * Every message in chatMessages MUST have an `id` field set to the return
 * value of this function so that Preact's VDOM reconciler can correctly
 * match DOM nodes across re-renders.
 */
export function nextMsgId() {
    return 'msg-' + (++_msgSeq);
}
