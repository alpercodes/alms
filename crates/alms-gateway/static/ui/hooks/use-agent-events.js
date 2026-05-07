/**
 * Agent-scoped SSE stream — drives the cross-session activity indicator
 * on the sidebar (#856).
 *
 * Opens a single EventSource to `/agents/{agentId}/events` per active
 * agent. The feed carries `session_activity_started` /
 * `session_activity_ended` events for runs across **all** of the agent's
 * sessions, which lets the sidebar light up the yellow blinking dot on
 * any session — not just the currently-viewed one.
 *
 * Lifecycle:
 *   1. `boot()` / `switchAgent()` calls `openAgentEventsStream(agentId)`
 *      after seeding `bgRuns` from the `has_active_run` snapshot returned
 *      by `GET /sessions`.
 *   2. While open, `session_activity_started` writes `bgRuns[sessionId]`,
 *      `session_activity_ended` removes it. `hasActiveRun()` in
 *      `session-list.js` reads `bgRuns` to decide whether to render the
 *      `.has-run` class.
 *   3. `switchAgent()` calls `closeAgentEventsStream()` before opening
 *      the new agent's feed; `bgRuns` is cleared and re-seeded from the
 *      new agent's `/sessions` snapshot.
 *
 * The synthetic-`ended` semantics from the backend (pre-cancelled runs
 * emit only `session_activity_ended` without a paired `started`) are
 * naturally handled — `removeBgRun` is a no-op when nothing was ever
 * written for that sessionId.
 *
 * Notes:
 *   - Auth uses the `?token=` query parameter (mirrors `openSessionStream`)
 *     because EventSource cannot set custom headers.
 *   - Reconnect uses `?last_event_id=<n>` for the manual reconnect path
 *     (browser auto-reconnect uses the `Last-Event-ID` header automatically).
 *   - This stream is independent of the per-session SSE stream
 *     (`openSessionStream`) which handles the chat view's events.
 */

import { setBgRun, removeBgRun } from '../state/queue.js';
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
 * @param {object} [opts]
 * @param {number|string} [opts.lastEventId] — replay cursor (used by
 *   the internal retry path)
 */
export function openAgentEventsStream(agentId, opts) {
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
    const qs = params.toString();
    const url = `/agents/${agentId}/events${qs ? '?' + qs : ''}`;

    const es = new EventSource(url);
    activeAgentEs = es;
    activeAgentId = agentId;
    retryCount = 0;
    lastSeenEventId = (opts && opts.lastEventId != null) ? opts.lastEventId : null;
    // Defer clearing the dead flag until the connection actually
    // opens — see the `'open'` listener below. Constructing the
    // EventSource is optimistic; clearing here would briefly hide
    // the banner during the construction window even when the
    // server is still unreachable, then the next exhaustion would
    // bring it back, which reads as a flicker to the operator
    // (#907 review, Suggestion 2).
    es.addEventListener('open', () => {
        if (es !== activeAgentEs) return;
        clearStreamDead('agent-events');
    });

    /**
     * Wrap a handler so we (a) discard events for a stale stream
     * (the agent may have been switched between the EventSource
     * dispatch and the handler executing) and (b) track the highest
     * numeric event ID for manual reconnect.
     */
    const on = (type, handler) => es.addEventListener(type, (e) => {
        if (es !== activeAgentEs) return;
        const id = e.lastEventId;
        if (id && /^\d+$/.test(id)) {
            lastSeenEventId = id;
        }
        try {
            handler(e);
        } catch (err) {
            console.error('[agent-events]', type, 'handler failed:', err);
        }
    });

    on('session_activity_started', (e) => {
        const data = JSON.parse(e.data);
        if (!data.session_id) return;
        setBgRun(data.session_id, { runId: data.run_id || null, finished: false });
    });

    on('session_activity_ended', (e) => {
        const data = JSON.parse(e.data);
        if (!data.session_id) return;
        // Idempotent: pre-cancelled runs emit `ended` without a paired
        // `started`, so `bgRuns[session_id]` may not exist. removeBgRun
        // is a no-op in that case.
        removeBgRun(data.session_id);
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
            // Track the timer id so the manual-reconnect path can
            // cancel it (see the clearTimeout at the top of
            // openAgentEventsStream / closeAgentEventsStream).
            backoffTimer = setTimeout(() => {
                backoffTimer = null;
                // Only reopen if this agent is still the active one.
                if (activeAgentId === reopenAgentId) {
                    openAgentEventsStream(reopenAgentId, { lastEventId: reopenLastId });
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
    if (aid) {
        openAgentEventsStream(aid, { lastEventId: replayId });
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
        activeAgentEs.close();
        activeAgentEs = null;
    }
    activeAgentId = null;
    lastSeenEventId = null;
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
