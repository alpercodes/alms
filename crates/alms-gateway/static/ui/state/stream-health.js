/**
 * Global health signal for the SSE streams that power the live UI
 * (#907).
 *
 * Both `use-session-stream.js` and `use-agent-events.js` implement a
 * manual reconnect loop with `MAX_RETRIES = 10` and exponential
 * backoff capped at 30 s. Before this module landed, exhausting the
 * retry budget left only a `console.error` — the user had no idea
 * the live updates had stopped flowing, and the only path back to
 * a working stream was a full page reload.
 *
 * This module is the central pub/sub for "is at least one stream
 * currently considered dead?" plus the click-to-reconnect plumbing
 * the banner uses to re-arm both hooks without a reload.
 *
 * Design:
 *
 *   - `deadSources` is a `Set<string>` of dead stream names
 *     (`'session'` and/or `'agent-events'`). Each hook calls
 *     `markStreamDead(source)` when its retry budget is exhausted
 *     and `clearStreamDead(source)` whenever it opens a fresh
 *     `EventSource` (success path).
 *
 *   - `streamDead` mirrors `deadSources.size > 0` as a Preact signal
 *     so the banner component re-renders when the set transitions
 *     from empty → non-empty (banner up) or non-empty → empty
 *     (banner down). The signal value is intentionally a boolean,
 *     not the Set itself — components only need the binary "is the
 *     banner visible?" answer.
 *
 *   - `reconnectAllStreams()` is the click handler the banner
 *     binds to. The hooks register their "reset retry budget +
 *     reopen" callbacks via `registerSessionReconnect()` and
 *     `registerAgentEventsReconnect()` at module load, mirroring
 *     the `setRunBoot` pattern in `state/loading.js` to dodge
 *     import cycles (hooks already import this module; if this
 *     module imported the hooks back we'd build a cycle).
 *
 *   - `triggerOnlineReconnect()` is the `window.addEventListener
 *     ('online', ...)` callback. It does the same work as
 *     `reconnectAllStreams()` but is exposed under a separate name
 *     so test code can disambiguate the two re-arm paths if it
 *     ever needs to. Today they're identical.
 *
 * Pure-function contract (covered by `stream-health.test.mjs`):
 *
 *   - `markStreamDead(source)` adds `source` to `deadSources`,
 *     flips `streamDead` to `true` if it wasn't already.
 *   - `clearStreamDead(source)` removes `source`; flips
 *     `streamDead` to `false` only when the set becomes empty.
 *   - `markStreamDead(source)` is idempotent — calling it twice
 *     for the same source does not double-add.
 *   - `reconnectAllStreams()` invokes every registered reconnect
 *     callback exactly once per call.
 *   - Re-registering replaces the previous callback (last
 *     registration wins). This matters because hooks register on
 *     module load; HMR / test re-imports must not stack handlers.
 *
 * The signal write inside `markStreamDead` / `clearStreamDead` is
 * deliberately conditional (`if (was/!isDead) ...`) so signal
 * subscribers (the banner) only re-render on actual transitions,
 * not on every redundant mark/clear from the hooks.
 */

import { signal } from '../deps.js';

/**
 * Boolean signal: `true` whenever at least one SSE stream has
 * exhausted its retry budget and is no longer attempting to
 * reconnect on its own. Drives the `StreamDeadBanner` visibility.
 */
export const streamDead = signal(false);

/**
 * Internal set of dead stream sources. Module-private so callers
 * are forced to use the `markStreamDead` / `clearStreamDead`
 * helpers; that lets us keep the `streamDead` signal write
 * conditional on a real transition.
 */
const deadSources = new Set();

/**
 * Reconnect callbacks registered by each hook. The hooks call the
 * `register*` setters at module load time, the banner click and
 * the `online` event call `reconnectAllStreams()` which fans out
 * to whatever is currently registered.
 *
 * Defaults are no-ops so calling `reconnectAllStreams()` before
 * any hook has registered (test-side, or during early boot) does
 * not throw.
 */
let sessionReconnect = () => {};
let agentEventsReconnect = () => {};

/**
 * Mark `source` as dead. Idempotent — second call for the same
 * source does not re-fire the signal. The signal write is gated
 * on a true empty → non-empty transition so banner subscribers
 * only re-render when the user-visible state actually changes.
 *
 * @param {string} source - stable label for the stream (e.g.
 *   `'session'` or `'agent-events'`).
 */
export function markStreamDead(source) {
    if (!source) return;
    if (deadSources.has(source)) return;
    const wasEmpty = deadSources.size === 0;
    deadSources.add(source);
    if (wasEmpty) {
        streamDead.value = true;
    }
}

/**
 * Clear `source` from the dead set. Flips `streamDead` to `false`
 * only when the set becomes empty (i.e. every previously-dead
 * stream has reported back as healthy). Idempotent.
 *
 * @param {string} source
 */
export function clearStreamDead(source) {
    if (!source) return;
    if (!deadSources.has(source)) return;
    deadSources.delete(source);
    if (deadSources.size === 0) {
        streamDead.value = false;
    }
}

/**
 * Test/diagnostic helper: snapshot the current dead-source set as
 * an array. Not part of the public API — used only by
 * `stream-health.test.mjs` to assert the underlying state.
 */
export function _deadSourcesSnapshot() {
    return Array.from(deadSources);
}

/**
 * Register the session-stream reconnect callback. The callback
 * should reset the hook's local `retryCount` and call
 * `openSessionStream(currentSessionId)` (or equivalent) so the
 * stream comes back online immediately.
 *
 * Last registration wins; this matches the
 * `setRunBoot`-in-`loading.js` shape and lets the hook re-register
 * cleanly across HMR / test reloads.
 *
 * @param {() => void} fn
 */
export function registerSessionReconnect(fn) {
    sessionReconnect = typeof fn === 'function' ? fn : () => {};
}

/**
 * Register the agent-events reconnect callback. Same contract as
 * `registerSessionReconnect`.
 *
 * @param {() => void} fn
 */
export function registerAgentEventsReconnect(fn) {
    agentEventsReconnect = typeof fn === 'function' ? fn : () => {};
}

/**
 * Re-arm both streams. Bound to the banner's click handler and to
 * the `window.addEventListener('online', ...)` callback. Each
 * registered reconnect callback is responsible for resetting its
 * own retry budget and calling its hook's open function — this
 * function only invokes them.
 *
 * Errors thrown by either callback are caught and logged so a
 * failure in one stream's reconnect path does not block the
 * other.
 */
export function reconnectAllStreams() {
    try {
        sessionReconnect();
    } catch (err) {
        console.error('[stream-health] session reconnect failed:', err);
    }
    try {
        agentEventsReconnect();
    } catch (err) {
        console.error('[stream-health] agent-events reconnect failed:', err);
    }
}

/**
 * `window.addEventListener('online', ...)` handler. Today this is
 * identical to `reconnectAllStreams()`, but the indirection lets
 * future code differentiate "user clicked the banner" from
 * "browser observed network came back" without breaking the
 * banner click handler signature.
 */
export function triggerOnlineReconnect() {
    reconnectAllStreams();
}

/**
 * Wire `triggerOnlineReconnect` to `window.online`. Called once
 * from `app.js` at boot. Returns a teardown function for tests /
 * HMR that need to detach the listener.
 *
 * The handler is deliberately attached unconditionally — even if
 * neither stream is currently dead, an `online` event after a
 * brief offline blip is a useful signal to proactively retry
 * (the streams may be in mid-backoff and resetting their budgets
 * cuts the wait short).
 *
 * @returns {() => void}
 */
export function installOnlineReconnectListener() {
    if (typeof window === 'undefined') return () => {};
    const handler = () => triggerOnlineReconnect();
    window.addEventListener('online', handler);
    return () => window.removeEventListener('online', handler);
}
