/**
 * The conversation-summary problem of the session being viewed.
 *
 * A summary failure is a state of the session, not an event in it: the same
 * failure comes back after every run until someone fixes it, and it has no
 * business in the transcript. So this holds one current value, which the
 * `summary_status` SSE event replaces, rather than appending a chat message
 * per failure.
 *
 * The session id is kept with the value so a banner from the session just left
 * never shows on the one being opened, before the new stream has said anything.
 *
 * Dismissal is by signature, not by session: dismissing "OpenRouter has no key"
 * in one session should not bring it back in the next, while a different
 * failure, or the same one after it has cleared and returned, still shows. It
 * lasts until the page reloads, so a problem that is still there is not hidden
 * for good by one click.
 */

import { signal } from '../deps.js';

export const summaryFailure = signal(null);
const dismissed = signal(new Set());

function signatureOf(failure) {
    return `${failure.code}|${failure.provider}|${failure.model}`;
}

/** Apply a validated `summary_status` payload. */
export function applySummaryStatus(data) {
    if (data.failure) {
        summaryFailure.value = { sessionId: data.session_id, ...data.failure };
        return;
    }
    const current = summaryFailure.value;
    if (current && current.sessionId === data.session_id) {
        // Fixed, so if it ever comes back it is news again.
        const remaining = new Set(dismissed.value);
        remaining.delete(signatureOf(current));
        dismissed.value = remaining;
        summaryFailure.value = null;
    }
}

/** A session stream (re)opened: only what it now sends is current. */
export function clearSummaryFailure() {
    summaryFailure.value = null;
}

export function dismissSummaryFailure() {
    const current = summaryFailure.value;
    if (current) dismissed.value = new Set(dismissed.value).add(signatureOf(current));
}

/** The failure to show for `sessionId`, or null. */
export function visibleSummaryFailure(sessionId) {
    const current = summaryFailure.value;
    if (!current || current.sessionId !== sessionId) return null;
    return dismissed.value.has(signatureOf(current)) ? null : current;
}
