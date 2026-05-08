// Pure-function helpers for "whole card is a click target" UX.
//
// Pinned regression target:
//   - issue #983 — operators expect agent cards to be click-targets in
//     their entirety, not just the small Select button. The fix removes
//     the Select button entirely and wires `onClick` + `onKeyDown` on
//     the card root. Nested actions (delete, edit, set-default) call
//     `stopPropagation()` so a click on them doesn't double-fire the
//     card activation.
//
// The runtime side of "click on a nested action doesn't bubble" is the
// browser's event system + `stopPropagation()` calls on the inner
// handlers. We can't unit-test that under Node without a DOM, but we
// CAN unit-test the pure-function shape of the activation decision so
// a future refactor can't silently regress the keyboard or
// `defaultPrevented` semantics.
//
// What we pin here:
//   - `isActivationKey(key)` — Enter / Space are the canonical
//     "activate the focused button-like element" keys per WAI-ARIA
//     button pattern. We return false for everything else (including
//     Tab, arrow keys, modifier-decorated Enter combinations are out
//     of scope at this layer — the caller decides).
//   - `shouldActivateFromKey(event)` — combines the key check with a
//     defensive `defaultPrevented` gate. If a nested handler has
//     already called `event.preventDefault()` (the keyboard mirror of
//     `stopPropagation()` for click), don't double-fire.
//   - `shouldActivateFromClick(event)` — same `defaultPrevented` gate
//     for click-driven activation. Nested action buttons that need to
//     prevent the card from also activating call `stopPropagation()`
//     (so the card's `onClick` never fires) OR `preventDefault()`
//     (which we honour here as belt-and-suspenders).

/**
 * True for the canonical "activate this button-like element" keys.
 * Mirrors the WAI-ARIA button pattern — Enter and Space activate;
 * nothing else does.
 *
 * @param {string} key — `KeyboardEvent.key`
 * @returns {boolean}
 */
export function isActivationKey(key) {
    return key === 'Enter' || key === ' ';
}

/**
 * Decide whether a `keydown` event on the card root should fire the
 * card's activation handler. Pure function — no DOM access, no state.
 *
 * Returns `true` only when:
 *   - the event's `.key` is Enter or Space, AND
 *   - the event has not already been `preventDefault()`-ed by a nested
 *     handler (defense in depth — `stopPropagation()` is the primary
 *     guard, but `preventDefault()` is an equally valid escape hatch).
 *
 * @param {{ key: string, defaultPrevented?: boolean }} event
 * @returns {boolean}
 */
export function shouldActivateFromKey(event) {
    if (!event || typeof event.key !== 'string') return false;
    if (event.defaultPrevented) return false;
    return isActivationKey(event.key);
}

/**
 * Decide whether a `click` event on the card root should fire the
 * card's activation handler. Pure function.
 *
 * Returns `true` unless the event has been `preventDefault()`-ed.
 * Nested action buttons primarily call `stopPropagation()` (so this
 * handler is never invoked), but we honour `preventDefault()` here as
 * belt-and-suspenders for any future nested handler that uses it
 * instead.
 *
 * @param {{ defaultPrevented?: boolean }} event
 * @returns {boolean}
 */
export function shouldActivateFromClick(event) {
    if (!event) return false;
    if (event.defaultPrevented) return false;
    return true;
}
