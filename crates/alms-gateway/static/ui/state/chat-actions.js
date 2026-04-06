/**
 * Centralized write API for chatMessages.
 *
 * Every mutation to chatMessages.value SHOULD go through one of these
 * five functions.  Each performs a single atomic signal write so that
 * Preact sees exactly one new array reference per logical operation.
 *
 * The only deliberate exception is the initial `chatMessages.value = []`
 * reset performed during session/agent switches -- those are simple
 * enough to remain inline where they improve readability.
 *
 * Functions:
 *   replaceMessages(msgs)             -- set the entire array
 *   appendMessage(...msgs)            -- push one or more messages
 *   updateMessage(predicate, updater) -- find last match, apply updater
 *   filterMessages(predicate)         -- keep only matching messages
 *   transformMessages(fn)             -- arbitrary (msgs) => newMsgs
 *
 * Relates to #521, #527.
 */

import { chatMessages } from './chat.js';

/**
 * Replace the entire chatMessages array.
 *
 * Used for history loads (REST API -> mapped messages) and error states.
 *
 * @param {Array} msgs - The new message array
 */
export function replaceMessages(msgs) {
    chatMessages.value = msgs;
}

/**
 * Append one or more messages to the end of chatMessages.
 *
 * Accepts a variable number of message objects.  All are appended in a
 * single signal write so Preact re-renders only once.
 *
 * @param {...object} msgs - Message objects to append
 */
export function appendMessage(...msgs) {
    chatMessages.value = [...chatMessages.value, ...msgs];
}

/**
 * Find the last message matching `predicate` and apply `updater` to it.
 *
 * If no message matches, the signal is not written (avoids unnecessary
 * re-renders).  Returns true if a match was found and updated.
 *
 * @param {function} predicate - (msg) => boolean
 * @param {function} updater   - (msg) => newMsg (must return a new object)
 * @returns {boolean} Whether a matching message was found and updated
 */
export function updateMessage(predicate, updater) {
    const msgs = chatMessages.value;
    const idx = msgs.findLastIndex(predicate);
    if (idx < 0) return false;
    const copy = [...msgs];
    copy[idx] = updater(copy[idx]);
    chatMessages.value = copy;
    return true;
}

/**
 * Keep only messages that match `predicate` (remove the rest).
 *
 * Only writes the signal if at least one message was removed.
 *
 * @param {function} predicate - (msg) => boolean (true = keep)
 */
export function filterMessages(predicate) {
    const msgs = chatMessages.value;
    const filtered = msgs.filter(predicate);
    if (filtered.length !== msgs.length) {
        chatMessages.value = filtered;
    }
}

/**
 * Apply an arbitrary transformation to the message array.
 *
 * This is the escape hatch for compound operations that combine
 * filtering, mapping, and appending in a single atomic write --
 * e.g. flushDeltaBuffer (filter thinking + update-or-append agent)
 * or handleRunEnd (map approvals + multi-push).
 *
 * The transformer receives the current messages array and must return
 * a new array.  The result is written unconditionally (the caller is
 * responsible for short-circuiting if no change is needed).
 *
 * @param {function} fn - (msgs) => newMsgs
 */
export function transformMessages(fn) {
    chatMessages.value = fn(chatMessages.value);
}
