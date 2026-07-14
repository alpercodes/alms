/**
 * Per-session composer state persistence (drafts + queued messages).
 *
 * Both the in-progress draft (text the operator has typed but not yet sent)
 * and the send-while-busy queue live in component-local / global-signal
 * state today. When the operator switches sessions the chat view either
 * unmounts the composer (draft loss, #981) or wipes `messageQueue.value`
 * to `[]` in the navigation reducer (queue loss, #975) — both bugs are the
 * same architectural shape: client-side state with view-bound lifetime.
 *
 * This module persists both kinds of state to `localStorage` keyed by
 * `session_id` so they survive a session-switch round-trip and a full
 * page reload.
 *
 * Storage keys:
 *   - alms.composer.draft.<session_id>    — string, the unsent draft text
 *   - alms.composer.queue.<session_id>    — array of { text } items
 *
 * Caps:
 *   - Draft: MAX_DRAFT_LEN bytes — defensive ceiling, only enforced when
 *     the browser actually refuses the full write for quota reasons. A
 *     100 KB pasted draft round-trips fine in realistic browsers; the
 *     truncation path emits a `console.warn` because the textarea value
 *     no longer matches what restore-on-next-mount will produce.
 *   - Queue: MAX_QUEUE_ITEMS entries / MAX_QUEUE_BYTES total bytes —
 *     additional sends past the cap are dropped on the floor (the live
 *     UI never enforced a cap either, but unbounded persistence would
 *     compound across sessions).
 *
 * Robustness: every read/write is wrapped in a try/catch. localStorage can
 * throw from quota-exceeded, private-browsing modes, or test environments
 * where `window` is undefined. The composer must keep working even when
 * persistence is broken — failure to save is not failure to send.
 */

const DRAFT_PREFIX = 'alms.composer.draft.';
const QUEUE_PREFIX = 'alms.composer.queue.';

// Per-message draft cap: 64 KB is plenty for any sane composer entry and
// safely under the typical 5 MB localStorage budget per origin.
const MAX_DRAFT_LEN = 64 * 1024;

// Queue caps: at most 50 items or ~256 KB total — whichever is hit first.
// In practice a busy operator queues 2-3 follow-ups; the cap exists to
// keep storage bounded if a user spams sends during a long-running task.
const MAX_QUEUE_ITEMS = 50;
const MAX_QUEUE_BYTES = 256 * 1024;

function hasStorage() {
    try {
        return typeof window !== 'undefined' && !!window.localStorage;
    } catch {
        return false;
    }
}

function draftKey(sessionId) {
    return DRAFT_PREFIX + sessionId;
}

function queueKey(sessionId) {
    return QUEUE_PREFIX + sessionId;
}

/**
 * Load the persisted draft for a session. Returns '' when no draft is
 * stored, the session id is missing, or storage is unavailable.
 *
 * @param {string|null|undefined} sessionId
 * @returns {string}
 */
export function loadDraft(sessionId) {
    if (!sessionId || !hasStorage()) return '';
    try {
        const raw = localStorage.getItem(draftKey(sessionId));
        return typeof raw === 'string' ? raw : '';
    } catch {
        return '';
    }
}

/**
 * Persist (or clear) the draft for a session. Empty drafts remove the
 * key so a page reload doesn't restore stale empties.
 *
 * Try-full-write-then-catch-quota strategy: the MAX_DRAFT_LEN cap is a
 * defensive ceiling, not the actual browser limit (modern browsers are
 * happy to accept 64 KB writes). We try the full text first, and only
 * fall back to truncation when the browser actually refuses for quota
 * reasons. This keeps a 100 KB pasted draft fully recoverable on
 * realistic browsers, and only loses the tail in the rare quota-pressure
 * edge case the cap was originally guarding.
 *
 * @param {string|null|undefined} sessionId
 * @param {string} text
 */
export function saveDraft(sessionId, text) {
    if (!sessionId || !hasStorage()) return;
    if (!text) {
        try {
            localStorage.removeItem(draftKey(sessionId));
        } catch {
            // ignore
        }
        return;
    }
    try {
        localStorage.setItem(draftKey(sessionId), text);
    } catch {
        // Likely QuotaExceededError — fall back to a truncated write so
        // the operator at least keeps the typed prefix. Surface a warn
        // because the user-visible textarea no longer matches the value
        // that will be restored on next mount.
        try {
            const clipped = text.length > MAX_DRAFT_LEN ? text.slice(0, MAX_DRAFT_LEN) : text;
            localStorage.setItem(draftKey(sessionId), clipped);
            if (clipped.length < text.length) {
                console.warn(
                    '[composer-storage] draft truncated from %d to %d chars on save (storage quota)',
                    text.length, clipped.length,
                );
            }
        } catch {
            // Storage genuinely unwritable — could be private browsing /
            // disabled localStorage, or origin-wide quota pressure on a
            // draft that was already under MAX_DRAFT_LEN (so the retry
            // didn't actually shrink it). Either way the draft is no
            // longer being persisted; surface a warn so a "draft not
            // surviving reload" failure is observable in the console
            // rather than silently swallowed. (Tim's re-review on PR
            // #990, finding #2.)
            console.warn(
                '[composer-storage] draft not persisted (storage rejected both full and truncated writes); in-memory textarea is still authoritative',
            );
        }
    }
}

/**
 * Remove the draft entry for a session (called on send success).
 *
 * @param {string|null|undefined} sessionId
 */
export function clearDraft(sessionId) {
    if (!sessionId || !hasStorage()) return;
    try {
        localStorage.removeItem(draftKey(sessionId));
    } catch {
        // ignore
    }
}

/**
 * Load the persisted queue for a session. Returns `[]` when no queue is
 * stored, the session id is missing, storage is unavailable, or the
 * stored value is corrupt / not an array.
 *
 * @param {string|null|undefined} sessionId
 * @returns {Array<{ text: string }>}
 */
export function loadQueue(sessionId) {
    if (!sessionId || !hasStorage()) return [];
    try {
        const raw = localStorage.getItem(queueKey(sessionId));
        if (!raw) return [];
        const parsed = JSON.parse(raw);
        if (!Array.isArray(parsed)) return [];
        // Defensive: filter out malformed entries so a corrupt blob can't
        // jam the drain loop in use-session-stream.js, and cap total
        // length so a tampered storage entry with millions of items
        // can't blow the messageQueue signal. The save-side cap protects
        // benign overflow; this cap protects adversarial input.
        return parsed
            .slice(0, MAX_QUEUE_ITEMS)
            .filter(item => item && typeof item.text === 'string')
            .map(item => ({ text: item.text }));
    } catch {
        return [];
    }
}

/**
 * Persist (or clear) the queue for a session. An empty array removes the
 * key entirely so a page reload doesn't restore an empty queue blob.
 *
 * Caps: items capped to MAX_QUEUE_ITEMS, total serialised JSON capped to
 * MAX_QUEUE_BYTES (overflow tail dropped). Writing failures (quota,
 * disabled storage) degrade silently — the live signal is still the
 * source of truth at runtime.
 *
 * @param {string|null|undefined} sessionId
 * @param {Array<{ text: string }>} queue
 */
export function saveQueue(sessionId, queue) {
    if (!sessionId || !hasStorage()) return;
    try {
        if (!Array.isArray(queue) || queue.length === 0) {
            localStorage.removeItem(queueKey(sessionId));
            return;
        }
        // Trim to MAX_QUEUE_ITEMS first (cheap), then byte-trim if the
        // JSON serialisation still overruns MAX_QUEUE_BYTES.
        let clipped = queue.slice(0, MAX_QUEUE_ITEMS).map(item => ({ text: item.text }));
        let serialised = JSON.stringify(clipped);
        while (serialised.length > MAX_QUEUE_BYTES && clipped.length > 0) {
            clipped = clipped.slice(0, -1);
            serialised = JSON.stringify(clipped);
        }
        if (clipped.length === 0) {
            localStorage.removeItem(queueKey(sessionId));
            return;
        }
        localStorage.setItem(queueKey(sessionId), serialised);
    } catch {
        // ignore
    }
}

/**
 * Remove the queue entry for a session.
 *
 * @param {string|null|undefined} sessionId
 */
export function clearQueue(sessionId) {
    if (!sessionId || !hasStorage()) return;
    try {
        localStorage.removeItem(queueKey(sessionId));
    } catch {
        // ignore
    }
}

/**
 * Discard both draft and queue for a session — called when the session
 * is deleted so storage doesn't leak entries for sessions that no longer
 * exist on the backend.
 *
 * @param {string|null|undefined} sessionId
 */
export function clearComposerState(sessionId) {
    clearDraft(sessionId);
    clearQueue(sessionId);
}

/**
 * Remove the exact queued head only after startRun accepted it.
 *
 * Object identity is intentional: identical queued text is valid, so a
 * delayed completion must never consume a newer entry just because its text
 * matches. Returning the original array signals that no mutation occurred.
 *
 * @param {Array<{ text: string }>} queue
 * @param {{ text: string }|null|undefined} expectedHead
 * @param {boolean} accepted
 * @returns {Array<{ text: string }>}
 */
export function consumeAcceptedQueueHead(queue, expectedHead, accepted) {
    if (!accepted || !Array.isArray(queue) || queue[0] !== expectedHead) {
        return queue;
    }
    return queue.slice(1);
}

/**
 * Decide what to do with a queue restored on InputArea mount.
 *
 * Pure function so the integration contract — "queue restored on mount
 * after run completed must auto-drain" (#975 Tim review on PR #990) —
 * can be unit-tested without spinning up a Preact tree. The caller
 * (InputArea's mount effect) starts the candidate without removing it.
 * consumeAcceptedQueueHead() commits the dequeue only after startRun accepts
 * the exact head.
 *
 * Decision matrix:
 *
 *   restoredQueue empty           → no drain, nothing to do
 *   restoredQueue present, run    → no drain (SSE handler will drain
 *      still in flight                when the run finishes)
 *   restoredQueue present, no agent → no drain (startRun would short-
 *                                    circuit and silently lose the head;
 *                                    restore the queue intact instead)
 *   restoredQueue present, no run, → drain head, persist remainder
 *      agent present                 (cross-session-return: run finished
 *                                    while operator was elsewhere)
 *                                    (page-reload race: run_finished SSE
 *                                    arrived before mount)
 *
 * @param {object} args
 * @param {Array<{ text: string }>|null|undefined} args.restoredQueue
 * @param {string|null|undefined} args.activeRunId
 * @param {string|null|undefined} args.activeAgentId — the currently
 *   selected agent. `startRun` short-circuits with an error message when
 *   `activeAgentId.value` is null, so peeling the head off the queue
 *   without an agent would silently drop it. In normal boot ordering
 *   the agent is set before the session (use-boot.js), so this gate is
 *   defense-in-depth rather than a frequent path. (Tim's re-review on
 *   PR #990, finding #1.)
 * @returns {{ drain: boolean, head: { text: string }|null, remaining: Array<{ text: string }> }}
 */
export function planMountDrain({ restoredQueue, activeRunId, activeAgentId }) {
    if (!Array.isArray(restoredQueue) || restoredQueue.length === 0) {
        return { drain: false, head: null, remaining: [] };
    }
    if (activeRunId) {
        // Run is still live on this session — the SSE drain block in
        // use-session-stream.js will handle the queue when it ends. The
        // mount effect should still hydrate the in-memory signal so the
        // UI shows the queued items, but it must not start a second run.
        return { drain: false, head: null, remaining: restoredQueue };
    }
    if (!activeAgentId) {
        // No agent selected — `startRun` would short-circuit on the
        // missing-agent guard and the peeled head would be lost. Restore
        // the queue intact and skip the drain; whoever sets the active
        // agent will land us back through this path on the next mount
        // cycle (or the SSE drain will fire if the agent kicks off a run
        // that completes after re-mount).
        return { drain: false, head: null, remaining: restoredQueue };
    }
    return {
        drain: true,
        head: restoredQueue[0],
        remaining: restoredQueue.slice(1),
    };
}
