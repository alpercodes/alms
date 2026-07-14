/**
 * Session-scoped write API for normalized chat messages.
 *
 * chatMessages is a read-only selector. Every mutation below resolves a
 * concrete session and dispatches a typed reducer action through the state
 * bridge. Authoritative history loads pass the session explicitly; live
 * handlers use the currently-bound session selected by loadSession().
 */

import { entityState } from './entity-state.js';

function resolveSessionId(sessionId) {
    return sessionId || entityState.messageSessionId.value;
}

function requireSessionId(sessionId) {
    const resolved = resolveSessionId(sessionId);
    if (!resolved) {
        throw new Error('Cannot mutate messages without a bound session');
    }
    return resolved;
}

/**
 * Install an authoritative message snapshot and bind it to the visible chat.
 * Passing an empty array without a session only unbinds the current selector.
 */
export function replaceMessages(msgs, sessionId = null) {
    if (sessionId) {
        entityState.replaceMessages(sessionId, msgs);
    } else if (msgs.length === 0) {
        entityState.unbindMessages();
    } else {
        entityState.replaceMessages(requireSessionId(null), msgs);
    }
}

/** Remove all cached and optimistic messages for one session. */
export function clearMessages(sessionId) {
    entityState.clearMessages(sessionId);
}

/** Append one or more messages to the currently visible session. */
export function appendMessage(...msgs) {
    appendMessagesToSession(requireSessionId(null), ...msgs);
}

/** Append messages to a specific session without changing visible selection. */
export function appendMessagesToSession(sessionId, ...msgs) {
    entityState.transformMessages(requireSessionId(sessionId), current => [...current, ...msgs]);
}

/** Find the last message matching predicate and replace it atomically. */
export function updateMessage(predicate, updater, sessionId = null) {
    let found = false;
    entityState.transformMessages(requireSessionId(sessionId), current => {
        const idx = current.findLastIndex(predicate);
        if (idx < 0) return current;
        const copy = [...current];
        copy[idx] = updater(copy[idx]);
        found = true;
        return copy;
    });
    return found;
}

/** Keep only messages accepted by predicate. */
export function filterMessages(predicate, sessionId = null) {
    entityState.transformMessages(requireSessionId(sessionId), current => {
        const filtered = current.filter(predicate);
        return filtered.length === current.length ? current : filtered;
    });
}

/** Apply one atomic transformation to a session's ordered message list. */
export function transformMessages(transformer, sessionId = null) {
    entityState.transformMessages(requireSessionId(sessionId), transformer);
}
