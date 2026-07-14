/**
 * Explicit optimistic-message lifecycle backed by the normalized store.
 *
 * The reducer owns both the pending correlation record and the visible
 * message entity, so session switches and authoritative history reloads
 * cannot lose or duplicate an in-flight user message.
 */

import { entityState } from './entity-state.js';

export function beginOptimisticMessage(sessionId, text, message, companions = []) {
    const ts = message.ts || new Date().toISOString();
    entityState.beginOptimisticMessage(
        sessionId,
        { ...message, ts, optimistic: true },
        {
            messageId: message.id,
            text,
            runId: null,
            ts,
        },
        companions,
    );
}

export function setPendingRunId(sessionId, messageId, runId) {
    entityState.linkOptimisticMessage(sessionId, messageId, runId);
}

export function confirmOptimisticMessage(sessionId, correlation) {
    entityState.confirmOptimisticMessage(sessionId, correlation);
}

export function rollbackOptimisticMessage(sessionId, correlation) {
    entityState.rollbackOptimisticMessage(sessionId, correlation);
}

export function getPendingMessages(sessionId) {
    return entityState.getPendingMessages(sessionId);
}
