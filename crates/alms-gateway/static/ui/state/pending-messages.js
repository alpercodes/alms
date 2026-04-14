/**
 * Tracks user messages that have been optimistically appended to the chat
 * but may not yet be persisted in the session history on the backend.
 *
 * When the user sends a message and immediately switches sessions, the
 * locally-appended user message is wiped by replaceMessages([]).  If the
 * user switches back before the backend has persisted the message to the
 * session (which happens asynchronously during the agent loop), the
 * history reload from the REST API will not contain it.
 *
 * This module solves the race by remembering the pending message text per
 * session.  After loadSession() fetches history, it calls
 * reconcilePendingMessage() to re-inject the message if it was lost.
 *
 * Lifecycle:
 *   1. startRun() -> savePendingMessage(sessionId, text)
 *   2. Session switch away -> chatMessages wiped, pending survives
 *   3. Session switch back -> loadSession() fetches history
 *   4. loadSession() calls reconcilePendingMessage() which checks if the
 *      run ID appears in the loaded history (via run_created events):
 *      - If yes: the backend persisted it, clear the pending entry
 *      - If no: re-inject it at the end of the message list
 *   5. run_finished/error/cancelled -> clearPendingMessage(sessionId)
 *      (the run is done -- the message was either persisted or the run
 *      failed before persisting it; either way, the history is now the
 *      source of truth)
 */

/**
 * Map of session ID -> { text: string, runId: string | null }
 *
 * Only one pending message per session is tracked because the UI queues
 * additional messages behind an active run (messageQueue), so at most one
 * un-persisted user message can exist per session at any time.
 *
 * The runId field enables definitive deduplication: when loadSession()
 * reconciles pending messages against the loaded history, it matches by
 * run ID (not by text content), avoiding false positives when the user
 * sends identical text twice.  The runId is null initially (set by
 * savePendingMessage before the HTTP response returns) and upgraded to
 * the real value once createRun() resolves or run_created arrives.
 */
const pendingMessages = new Map();

/**
 * Save a pending user message for a session.
 * Called by startRun() after optimistically appending the message.
 * The runId is initially null and should be set via setPendingRunId()
 * once the createRun() response returns the run ID.
 *
 * @param {string} sessionId
 * @param {string} text - the user's message text
 */
export function savePendingMessage(sessionId, text) {
    pendingMessages.set(sessionId, { text, runId: null });
}

/**
 * Attach the run ID to an existing pending message.
 * Called after createRun() returns successfully so the pending entry
 * can be matched definitively by run ID during reconciliation.
 *
 * @param {string} sessionId
 * @param {string} runId
 */
export function setPendingRunId(sessionId, runId) {
    const entry = pendingMessages.get(sessionId);
    if (entry) {
        entry.runId = runId;
    }
}

/**
 * Clear the pending message for a session.
 * Called when the run ends (finished/error/cancelled) since the message
 * is either persisted or the run failed before persisting.
 *
 * @param {string} sessionId
 */
export function clearPendingMessage(sessionId) {
    pendingMessages.delete(sessionId);
}

/**
 * Get the pending message for a session (if any).
 *
 * @param {string} sessionId
 * @returns {{ text: string, runId: string | null } | undefined}
 */
export function getPendingMessage(sessionId) {
    return pendingMessages.get(sessionId);
}

