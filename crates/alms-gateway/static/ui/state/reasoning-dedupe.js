/**
 * Per-session reasoning-dedupe suppress-set store (#1135, follow-up to
 * #1133 / PR #1134 Layer 3).
 *
 * Background
 * ----------
 * The Layer-3 reasoning-dedupe suppress-set (`sealedReasoningRunIds`) holds
 * the run-ids whose final-turn reasoning is already sealed into the loaded
 * history. `loadSession` builds it (gated on history coverage — see
 * `reasoning-coverage.js`) and the `use-session-stream.js` `reasoning_delta`
 * handler drops any replayed delta carrying one of those run-ids, so a run
 * that went terminal during the load does not double-render as a spurious
 * unsealed reasoning bubble.
 *
 * The bug this module fixes
 * -------------------------
 * Before #1135 the suppress-set lived ONLY on the initial `openSessionStream`
 * `opts`. Both EventSource reconnect paths in `use-session-stream.js`
 * (auto-backoff `onerror` reopen and the manual `reconnectSessionStream`
 * banner / `online` re-arm) reopen with `{ lastEventId }` only — no
 * `sealedReasoningRunIds`. The `on()` wrapper advances `lastSeenEventId`
 * BEFORE the handler runs, so even a delta the Layer-3 guard drops still
 * advances the reconnect cursor. So if the load-stream replays a sealed run's
 * trailing deltas `e1..e3`, processes `e1` (cursor → e1, dropped by the
 * guard), then the connection dies before `e2`/`e3` arrive, the reconnect
 * reopens at `e1`, re-replays `e2`/`e3` with the suppress-set absent → they
 * are not dropped → a spurious unsealed reasoning bubble that persists until
 * the next full reload (the run is already terminal, so no future
 * `run_finished` reseals it).
 *
 * The fix
 * -------
 * Hoist the suppress-set to module scope keyed by `sessionId` for the
 * stream's lifetime — mirroring how `use-session-stream.js` keeps
 * `lastSeenEventId` at module scope so the reconnect paths can reuse it. On
 * stream open the hook records the incoming set here under the session id;
 * the auto-backoff and manual reconnect paths read it back so they reopen
 * with the SAME set. The entry is dropped on session teardown
 * (`closeSessionStream`) so there is no cross-session leakage and no
 * unbounded growth across session switches.
 *
 * Scoping is per-`sessionId` rather than a single module-global so that a
 * reconnect can never resurrect a stale set from a session the operator
 * already navigated away from — the reconnect paths look up by the session
 * id they are reopening against.
 *
 * Pure-function contract (covered by `reasoning-dedupe.test.mjs`):
 *   - `setSealedReasoningRunIds(sessionId, set)` stores `set` under
 *     `sessionId`; a falsy `sessionId` or a non-Set `set` is a no-op (the
 *     four `openSessionStream` callers that pass no suppress-set must stay
 *     no-op-safe).
 *   - `getSealedReasoningRunIds(sessionId)` returns the stored set, or
 *     `null` when none was recorded for that session.
 *   - `clearSealedReasoningRunIds(sessionId)` drops only that session's
 *     entry; `clearSealedReasoningRunIds()` (no arg) drops every entry
 *     (used on full stream teardown).
 *   - Re-recording for the same `sessionId` replaces the previous set
 *     (last-write-wins) — a fresh `loadSession` supersedes the old set.
 */

/**
 * Per-session suppress-set store. Keyed by `sessionId`, value is the
 * `Set<string>` of run-ids whose sealed reasoning must be suppressed on
 * replay. Module-scoped so the reconnect paths in `use-session-stream.js`
 * can recover the set without the originating `opts` object.
 */
const sealedReasoningBySession = new Map();

/**
 * Record the suppress-set for a session's stream lifetime.
 *
 * No-op when `sessionId` is falsy or `set` is not a Set — this keeps the
 * four `openSessionStream` callers that pass no suppress-set (new-session,
 * boot, session-list, reconnect-before-first-load) safe to call through the
 * same store without special-casing.
 *
 * @param {string} sessionId
 * @param {Set<string>} set
 */
export function setSealedReasoningRunIds(sessionId, set) {
    if (!sessionId || !(set instanceof Set)) return;
    sealedReasoningBySession.set(sessionId, set);
}

/**
 * Recover the suppress-set recorded for a session, or `null` if none was
 * recorded. The reconnect paths call this with the session id they are
 * reopening against so the Layer-3 guard keeps firing across reconnects.
 *
 * @param {string} sessionId
 * @returns {Set<string>|null}
 */
export function getSealedReasoningRunIds(sessionId) {
    if (!sessionId) return null;
    return sealedReasoningBySession.get(sessionId) || null;
}

/**
 * Drop the suppress-set for a session (on stream teardown) so the store does
 * not accumulate entries across session switches. Called with no argument it
 * clears every entry — used when the whole stream is torn down with no live
 * session to scope to.
 *
 * @param {string} [sessionId]
 */
export function clearSealedReasoningRunIds(sessionId) {
    if (sessionId == null) {
        sealedReasoningBySession.clear();
        return;
    }
    sealedReasoningBySession.delete(sessionId);
}

/**
 * Test-only snapshot of the currently-tracked session ids. Not used by
 * production code — exported so `reasoning-dedupe.test.mjs` can assert the
 * cleanup contract (no cross-session leakage, no unbounded growth) without
 * reaching into module internals.
 *
 * @returns {string[]}
 */
export function _trackedSessionsSnapshot() {
    return [...sealedReasoningBySession.keys()];
}
