/**
 * Global cross-agent session-activity SSE stream — drives the
 * cross-session active-run indicator (blinking yellow dot) on the sidebar
 * (#856, made cross-agent in #1211).
 *
 * Opens a single EventSource to `/events/session-activity`. Unlike the
 * per-agent feed (`/agents/{id}/events`) this stream carries
 * `session_activity_started` / `session_activity_ended` for runs across
 * **every** agent's sessions. That is load-bearing: the sidebar surfaces
 * sessions owned by agents OTHER than the currently-active one — the
 * cross-agent Jobs / Direct-messages / Notifications sections — so a
 * per-agent feed could never light their dot (#1211: a run on another
 * agent's session never reached the active agent's feed, so the dot only
 * lit on the currently-selected session).
 *
 * Lifecycle:
 *   1. `boot()` / `switchAgent()` calls `openAgentEventsStream(agentId)`
 *      after seeding `bgRuns` from the `has_active_run` snapshot of BOTH
 *      the active agent's sessions AND the cross-agent surfaces returned
 *      by `GET /sessions`.
 *   2. While open, both activity event types apply the backend's
 *      authoritative `has_active_run` value. This keeps overlapping runs
 *      cardinality-safe. `hasActiveRun()` in
 *      `session-list.js` reads `bgRuns` to decide whether to render the
 *      `.has-run` class.
 *   3. `switchAgent()` calls `closeAgentEventsStream()` before reopening;
 *      `bgRuns` is cleared and re-seeded from the fresh snapshot. The feed
 *      is global, so `agentId` is retained only as the teardown/reopen
 *      scoping token (not part of the URL).
 *
 * The synthetic-`ended` semantics from the backend (pre-cancelled runs
 * emit only `session_activity_ended` without a paired `started`) are
 * naturally handled — the normalized reducer ignores an unknown run id
 * while preserving any other active runs for that session.
 *
 * Notes:
 *   - Auth uses the `?token=` query parameter (mirrors `openSessionStream`)
 *     because EventSource cannot set custom headers.
 *   - Reconnect uses `?last_event_id=<n>` for the manual reconnect path
 *     (browser auto-reconnect uses the `Last-Event-ID` header automatically).
 *   - This stream is independent of the per-session SSE stream
 *     (`openSessionStream`) which handles the chat view's events.
 */

import { listSessions } from '../api/sessions.js';
import {
    replaceActivitySnapshot,
    applyActivityEvent,
    beginActivityReconciliation,
    commitActivityReconciliation,
    abortActivityReconciliation,
} from '../state/queue.js';
import {
    markStreamDead,
    clearStreamDead,
    registerAgentEventsReconnect,
} from '../state/stream-health.js';

let activeAgentEs = null;
let activeAgentId = null;
let retryCount = 0;
const MAX_RETRIES = 10;
let lastSeenEventId = null;
let lastSeenStreamEpoch = null;
/**
 * In-flight backoff timer scheduled by `es.onerror`. Stored at module
 * scope so the manual-reconnect path (banner click, `online` event,
 * `closeAgentEventsStream`) can cancel a pending reopen and avoid
 * double-opening an already-healthy stream (#907 review,
 * Suggestion 1).
 */
let backoffTimer = null;


/**
 * Open the agent-scoped SSE stream.  Closes any previously open stream
 * first so only one is active at a time.
 *
 * @param {string} agentId
 * @param {{ lastEventId?: number|string, streamEpoch?: string, reconcileOnOpen?: boolean }} [opts]
 *   Replay and reconciliation options.
 */
export function openAgentEventsStream(agentId, opts) {
    const requestedEpoch = (opts && opts.streamEpoch != null)
        ? String(opts.streamEpoch)
        : null;
    closeAgentEventsStream();
    if (!agentId) return;

    // Cancel any pending backoff reopen scheduled by a previous
    // onerror — the manual-reconnect path (banner click, `online`
    // event, or this fresh open) supersedes it. Without this guard
    // the in-flight setTimeout would still fire later and
    // double-open the now-healthy stream (#907 review,
    // Suggestion 1). closeAgentEventsStream() above already cancels;
    // this site is belt-and-braces.
    if (backoffTimer !== null) {
        clearTimeout(backoffTimer);
        backoffTimer = null;
    }

    const token = localStorage.getItem('alms_auth_token');
    const params = new URLSearchParams();
    if (token) params.set('token', token);
    if (opts && opts.lastEventId != null) params.set('last_event_id', String(opts.lastEventId));
    if (requestedEpoch) params.set('stream_epoch', requestedEpoch);
    const qs = params.toString();
    // Global cross-agent feed (#1211) — NOT `/agents/${agentId}/events`.
    // `agentId` is retained above only as the teardown/reopen scoping
    // token (see `activeAgentId` and the onerror reopen guard), not as
    // part of the URL: the sidebar needs activity for sessions owned by
    // every agent it surfaces, so the feed cannot be scoped per agent.
    const url = `/events/session-activity${qs ? '?' + qs : ''}`;

    /** @type {EventSource & { _reconciliationRetryTimer?: number | null }} */
    const es = new EventSource(url);
    activeAgentEs = es;
    activeAgentId = agentId;
    retryCount = 0;
    lastSeenEventId = (opts && opts.lastEventId != null) ? opts.lastEventId : null;
    lastSeenStreamEpoch = requestedEpoch;
    let hasOpened = false;
    let reconciling = false;
    let queuedReconciliation = false;
    let queuedReplayCeiling = null;
    let queuedReplayEpoch = null;
    let reconciliationRetryCount = 0;
    let contractRecovering = false;

    const dispatchActivity = (type, data, eventId) =>
        applyActivityEvent(type, data, eventId, lastSeenStreamEpoch);

    const reconcileActivity = async (replayCeiling, streamEpoch = lastSeenStreamEpoch) => {
        const requestReplayCeiling = Number.isSafeInteger(replayCeiling)
            ? replayCeiling
            : null;
        const requestStreamEpoch = streamEpoch;
        if (reconciling) {
            // A stream can reconnect again while the previous authoritative
            // snapshot is still in flight. Keep that later reconciliation
            // separate: widening the first request's ceiling would let its
            // older snapshot discard events belonging to the newer stream.
            queuedReconciliation = true;
            if (queuedReplayEpoch !== requestStreamEpoch) {
                queuedReplayEpoch = requestStreamEpoch;
                queuedReplayCeiling = requestReplayCeiling;
            } else if (requestReplayCeiling != null) {
                queuedReplayCeiling = queuedReplayCeiling == null
                    ? requestReplayCeiling
                    : Math.max(queuedReplayCeiling, requestReplayCeiling);
            }
            return;
        }
        if (es !== activeAgentEs) return;
        reconciling = true;
        const reconciliationToken = beginActivityReconciliation(requestReplayCeiling, requestStreamEpoch);

        let snapshot = null;
        try {
            snapshot = await listSessions(null, { includeDms: true });
        } catch (err) {
            console.error('[agent-events] activity reconciliation failed:', err);
        }

        if (es !== activeAgentEs) {
            abortActivityReconciliation(reconciliationToken);
            return;
        }
        if (snapshot) {
            const committed = commitActivityReconciliation(reconciliationToken, snapshot.sessions || []);
            if (committed) {
                reconciliationRetryCount = 0;
            } else {
                queuedReconciliation = true;
                queuedReplayCeiling = requestReplayCeiling;
                queuedReplayEpoch = requestStreamEpoch;
            }
        } else {
            // Fail closed: discard buffered deltas and retry an authoritative
            // snapshot instead of deriving state from an incomplete recovery window.
            abortActivityReconciliation(reconciliationToken);
        }

        reconciling = false;

        const shouldRunQueuedReconciliation = queuedReconciliation;
        const nextReplayCeiling = queuedReplayCeiling;
        const nextReplayEpoch = queuedReplayEpoch;
        queuedReconciliation = false;
        queuedReplayCeiling = null;
        queuedReplayEpoch = null;
        if (shouldRunQueuedReconciliation && es === activeAgentEs) {
            void reconcileActivity(nextReplayCeiling, nextReplayEpoch);
            return;
        }

        if (!snapshot && es === activeAgentEs) {
            reconciliationRetryCount++;
            const delay = Math.min(1000 * Math.pow(2, reconciliationRetryCount - 1), 30000);
            es._reconciliationRetryTimer = setTimeout(() => {
                es._reconciliationRetryTimer = null;
                if (es === activeAgentEs) void reconcileActivity(null);
            }, delay);
        }
    };

    // Defer clearing the dead flag until the connection actually
    // opens — see the `'open'` listener below. Constructing the
    // EventSource is optimistic; clearing here would briefly hide
    // the banner during the construction window even when the
    // server is still unreachable, then the next exhaustion would
    // bring it back, which reads as a flicker to the operator
    // (#907 review, Suggestion 2).
    es.addEventListener('open', () => {
        if (es !== activeAgentEs) return;
        const shouldReconcile = hasOpened || Boolean(opts && opts.reconcileOnOpen);
        hasOpened = true;
        clearStreamDead('agent-events');
        if (shouldReconcile) {
            void reconcileActivity(null);
        }
    });

    const recoverFromContractViolation = async (eventId, error) => {
        if (contractRecovering || es !== activeAgentEs) return;
        contractRecovering = true;
        es.close();
        console.error('[agent-events] rejected live event; reconciling:', error);

        try {
            const snapshot = await listSessions(null, { includeDms: true });
            if (es !== activeAgentEs) return;
            replaceActivitySnapshot(snapshot.sessions || [], eventId, lastSeenStreamEpoch);

            // A malformed frame cannot become valid on replay. Skip only
            // that frame after replacing local state with the authoritative
            // snapshot; stream_state reconciles the retained tail.
            activeAgentEs = null;
            openAgentEventsStream(agentId, {
                lastEventId: eventId,
                streamEpoch: lastSeenStreamEpoch,
                reconcileOnOpen: true,
            });
        } catch (snapshotError) {
            console.error('[agent-events] contract reconciliation failed:', snapshotError);
            markStreamDead('agent-events');
        }
    };

    /**
     * Validate before mutation and advance the replay cursor only after
     * the event has been handled successfully.
     */
    const on = (type, handler) => es.addEventListener(type, (e) => {
        if (es !== activeAgentEs) return;
        const id = e.lastEventId;
        const numericId = id && /^\d+$/.test(id) ? Number(id) : null;
        try {
            const contracts = globalThis.__almsContracts;
            if (!contracts) throw new Error('Frontend contract bridge is not installed');
            const validated = contracts.parseSseJsonPayload(type, e.data);
            handler({ data: validated, lastEventId: e.lastEventId });
            if (numericId != null) lastSeenEventId = id;
        } catch (err) {
            if (numericId != null) {
                void recoverFromContractViolation(numericId, err);
            } else {
                console.error('[agent-events]', type, 'handler failed:', err);
                es.close();
                markStreamDead('agent-events');
            }
        }
    });

    on('session_activity_started', (e) => {
        const data = e.data;
        const eventId = /^\d+$/.test(e.lastEventId) ? Number(e.lastEventId) : null;
        dispatchActivity('session_activity_started', data, eventId);
    });

    on('session_activity_ended', (e) => {
        const data = e.data;
        const eventId = /^\d+$/.test(e.lastEventId) ? Number(e.lastEventId) : null;
        dispatchActivity('session_activity_ended', data, eventId);
    });

    on('stream_state', (e) => {
        const data = e.data;
        const epochChanged = Boolean(
            lastSeenStreamEpoch
            && data.stream_epoch
            && lastSeenStreamEpoch !== data.stream_epoch
        );
        if (data.stream_epoch) {
            lastSeenStreamEpoch = data.stream_epoch;
        }
        if (data.requires_reconciliation || epochChanged) {
            const replayCeiling = Number.isSafeInteger(data.newest) ? data.newest : null;
            void reconcileActivity(replayCeiling);
        }
    });

    es.onerror = () => {
        if (es.readyState === EventSource.CLOSED) {
            // EventSource auto-reconnect already replays via Last-Event-ID
            // when the browser triggers it, but if the connection is fully
            // closed (server-side abort, network teardown) we manually
            // reopen with exponential backoff up to MAX_RETRIES.
            retryCount++;
            if (retryCount >= MAX_RETRIES) {
                console.error('[agent-events] Max retries reached for agent', agentId);
                // Surface the dead state to the user (#907). The banner
                // exposes a click-to-reconnect path that calls back
                // into `reconnectAgentEventsStream` below; the browser
                // `online` event runs the same path automatically.
                markStreamDead('agent-events');
                return;
            }
            const delay = Math.min(2000 * Math.pow(2, retryCount - 1), 30000);
            const reopenAgentId = agentId;
            const reopenLastId = lastSeenEventId;
            const reopenEpoch = lastSeenStreamEpoch;
            // Track the timer id so the manual-reconnect path can
            // cancel it (see the clearTimeout at the top of
            // openAgentEventsStream / closeAgentEventsStream).
            backoffTimer = setTimeout(() => {
                backoffTimer = null;
                // Only reopen if this agent is still the active one.
                if (activeAgentId === reopenAgentId) {
                    openAgentEventsStream(reopenAgentId, {
                        lastEventId: reopenLastId,
                        streamEpoch: reopenEpoch,
                        reconcileOnOpen: true,
                    });
                }
            }, delay);
        }
    };
}

/**
 * Reset the agent-events retry budget and reopen against the
 * currently-active agent. Registered with the global stream-health
 * pub/sub so the banner click and the browser `online` event can
 * both re-arm the stream without a full page reload (#907).
 */
function reconnectAgentEventsStream() {
    retryCount = 0;
    const aid = activeAgentId;
    const replayId = lastSeenEventId;
    const replayEpoch = lastSeenStreamEpoch;
    if (aid) {
        openAgentEventsStream(aid, {
            lastEventId: replayId,
            streamEpoch: replayEpoch,
            reconcileOnOpen: true,
        });
    } else {
        // No active agent — nothing to reconnect to. Clear the dead
        // flag so the banner reflects reality.
        clearStreamDead('agent-events');
    }
}

// Register the reconnect callback once at module load. Last-write-
// wins on the registration side, so test-side re-imports are safe
// (see `registerAgentEventsReconnect` in `state/stream-health.js`).
registerAgentEventsReconnect(reconnectAgentEventsStream);

/**
 * Close the active agent-scoped SSE stream (if any).
 */
export function closeAgentEventsStream() {
    if (activeAgentEs) {
        if (activeAgentEs._reconciliationRetryTimer != null) {
            clearTimeout(activeAgentEs._reconciliationRetryTimer);
            activeAgentEs._reconciliationRetryTimer = null;
        }
        activeAgentEs.close();
        activeAgentEs = null;
    }
    activeAgentId = null;
    lastSeenEventId = null;
    lastSeenStreamEpoch = null;
    retryCount = 0;
    // Cancel any pending backoff reopen — closing is an explicit
    // teardown signal. Without this, a teardown mid-backoff would
    // leave the timer queued to reopen against a stale agentId
    // (#907 review, Suggestion 1).
    if (backoffTimer !== null) {
        clearTimeout(backoffTimer);
        backoffTimer = null;
    }
    // Closing means we're no longer attempting to maintain the stream,
    // so the dead flag should be cleared. Otherwise switching agents
    // away from a dead stream would leave the banner up forever. (#907)
    clearStreamDead('agent-events');
}

/**
 * Test/diagnostic helper: whether the agent-events stream is currently
 * open.  Not part of the public API — used by future tests.
 */
export function isAgentEventsStreamOpen() {
    return activeAgentEs !== null;
}
