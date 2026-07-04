import { signal } from '../deps.js';
import { cancelSubagent } from '../api/sessions.js';

/**
 * Shared confirm-then-cancel state machine for the two subagent cancel
 * surfaces: the ✕ control on RUNNING Subagent-status-bar chips
 * (`components/chat/subagent-bar.js`) and the "Cancel subagent" button in
 * the drilled-down subagent session view's breadcrumb header (`app.js`).
 *
 * Both surfaces are SESSION-keyed — the chip carries the subagent's
 * `sessionId` (from `subagent_started` / the invoke_agent result / reload
 * rehydration) and the drill-down view IS that session — so one module
 * drives both, and the UI tests exercise the whole decision layer here
 * without a DOM harness (the #983 `card-activation.js` testing pattern).
 *
 * The confirm step is the in-app inline two-step used by the session-list
 * delete button (`components/sidebar/session-list.js`): the first click
 * arms a per-session pending state that swaps the control into explicit
 * Yes / No buttons; choosing No (or letting the `CONFIRM_REVERT_MS`
 * auto-revert fire, or arming a different session's confirm) makes NO API
 * call. Only an explicit Yes while that session's confirm is armed calls
 * `cancelSubagent`. Native `window.confirm` is deliberately not used.
 *
 * Cancellation itself is fire-the-request-and-let-SSE-render: the backend
 * flips the subagent to `cancelled` and the EXISTING
 * `subagent_completed → 'cancelled'` chip path (#1190) / `run_cancelled`
 * on the subagent's own session render the result. No new result state
 * machine here.
 */

/** How long an armed confirm stays open before it self-dismisses (ms).
 *  Mirrors the sidebar delete-confirm's auto-revert so a misclick on the
 *  tiny chip ✕ can't leave a live-fire Yes button parked on screen. */
export const CONFIRM_REVERT_MS = 4000;

/**
 * The subagent session id whose cancel-confirm is currently armed, or
 * `null`. At most ONE confirm is armed at a time (arming another session
 * replaces it) — matching how the operator actually interacts with the
 * bar, and keeping "exactly one live-fire Yes button on screen" true.
 */
export const pendingCancelSessionId = signal(null);

/** Auto-revert timer for the armed confirm (see CONFIRM_REVERT_MS). */
let revertTimer = null;

function clearRevertTimer() {
    if (revertTimer) {
        clearTimeout(revertTimer);
        revertTimer = null;
    }
}

/**
 * Whether a chip/entry should render a cancel affordance at all.
 *
 * Guards Atlas's two display rules in one place:
 *   - only RUNNING subagents offer cancel (terminal chips don't), and
 *   - the control is hidden until the subagent's `sessionId` is known
 *     (it can lag the chip briefly — same window in which the chip's
 *     navigate click is a no-op). Never renders a control that could
 *     fire with a null/undefined id.
 *
 * @param {{status: string, sessionId: string|null}|null|undefined} info -
 *   an `activeSubagents` entry.
 * @returns {boolean}
 */
export function showCancelControl(info) {
    return !!(info && info.status === 'running' && info.sessionId);
}

/**
 * Whether the cancel-confirm is currently armed for `sessionId`.
 * `false` for null/undefined ids even if (impossibly) the pending signal
 * held a nullish value — a nullish id must never look confirmable.
 */
export function isCancelPending(sessionId) {
    return !!sessionId && pendingCancelSessionId.value === sessionId;
}

/**
 * Arm the confirm step for a subagent session (first click on ✕ / Cancel).
 *
 * Makes NO API call. Returns `false` (and arms nothing) for a nullish
 * session id — the unknown-session-id guard. Re-arms the auto-revert
 * timer, replacing any previously armed confirm.
 *
 * @param {string|null|undefined} sessionId
 * @returns {boolean} whether the confirm was armed.
 */
export function requestSubagentCancel(sessionId) {
    if (!sessionId) return false;
    clearRevertTimer();
    pendingCancelSessionId.value = sessionId;
    revertTimer = setTimeout(() => {
        revertTimer = null;
        pendingCancelSessionId.value = null;
    }, CONFIRM_REVERT_MS);
    return true;
}

/**
 * Dismiss the armed confirm (the "No" button / explicit dismissal).
 * Makes NO API call.
 */
export function dismissSubagentCancel() {
    clearRevertTimer();
    pendingCancelSessionId.value = null;
}

/**
 * Clear the armed confirm IF (and only if) it targets `sessionId` — the
 * chip-lifecycle hook (Codex P2, PR #1192).
 *
 * Named subagents reuse the SAME session id across re-invocations
 * (`derive_subagent_identity` is deterministic for named subagents), so an
 * armed confirm that survived the subagent's terminal transition or chip
 * removal would leave the NEXT invocation's chip rendering pre-armed —
 * its Yes button would live-fire a cancel of the fresh run with no
 * confirming first click, defeating the confirm guard entirely.
 *
 * `state/subagents.js` calls this at every terminal status transition
 * (`trackSubagentEnd`) and at chip auto-removal; `clearAllSubagents`
 * dismisses unconditionally on session switches. A non-matching / nullish
 * `sessionId` is a no-op so an unrelated chip's lifecycle can never
 * dismiss a confirm the operator armed on a different subagent.
 */
export function clearCancelConfirmForSession(sessionId) {
    if (sessionId && pendingCancelSessionId.value === sessionId) {
        dismissSubagentCancel();
    }
}

/**
 * Confirm the armed cancel (the "Yes" button): calls the backend's
 * session-keyed cancel endpoint for `sessionId`.
 *
 * Only fires when THIS session's confirm is currently armed — a stale Yes
 * (confirm already auto-reverted, or re-armed for a different session)
 * makes no call. The armed state is cleared BEFORE the request so a
 * double-click cannot fire twice (the backend 404s a second cancel
 * anyway, but the UI shouldn't emit it).
 *
 * Errors are logged, not rethrown: the interesting "already finished"
 * race arrives as a 404 and the chip's terminal state renders via the
 * normal `subagent_completed` path regardless.
 *
 * @param {string|null|undefined} sessionId
 * @returns {Promise<boolean>} whether the API call was made and succeeded.
 */
export async function confirmSubagentCancel(sessionId) {
    if (!isCancelPending(sessionId)) return false;
    dismissSubagentCancel();
    try {
        await cancelSubagent(sessionId);
        return true;
    } catch (err) {
        console.error('[confirmSubagentCancel] cancel failed for session',
            sessionId, err);
        return false;
    }
}
