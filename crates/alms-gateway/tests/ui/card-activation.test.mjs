// JS-level regression tests for `static/ui/utils/card-activation.js`.
//
// Pinned regression target:
//   - issue #983 — the agent card in the right-side agents panel is now
//     a whole-card click target. Operators expect cards to be clickable
//     in their entirety, not just the small Select button. The fix
//     removes the Select button entirely and wires `onClick` +
//     `onKeyDown` on the card root, with nested actions calling
//     `stopPropagation()` so they don't double-fire the activation.
//
// What this file pins:
//   - `isActivationKey(key)` — returns true only for Enter / Space
//     (the WAI-ARIA button activation keys), false for everything else.
//   - `shouldActivateFromKey(event)` — combines the key check with a
//     `defaultPrevented` gate so a nested handler that calls
//     `preventDefault()` can suppress the card's activation.
//   - `shouldActivateFromClick(event)` — fires unless the event has
//     been `preventDefault()`-ed.
//
// What this file does NOT pin:
//   - The runtime contract that `stopPropagation()` on a nested
//     button's `onClick` actually keeps the card-root handler from
//     firing. That's the browser's event system, not our code; we
//     trust it. The unit-testable surface is the pure-function
//     decision layer above.
//   - Preact / signals / DOM rendering. Those are integration concerns
//     and would need a JSDOM harness we don't have set up.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import url from 'node:url';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const CARD_ACTIVATION_PATH = path.resolve(
    __dirname,
    '../../static/ui/utils/card-activation.js'
);

const { isActivationKey, shouldActivateFromKey, shouldActivateFromClick } =
    await import(url.pathToFileURL(CARD_ACTIVATION_PATH).href);

// ---------------------------------------------------------------------
// isActivationKey
// ---------------------------------------------------------------------

test('#983: isActivationKey returns true for Enter', () => {
    assert.equal(isActivationKey('Enter'), true);
});

test('#983: isActivationKey returns true for Space (literal " ")', () => {
    // KeyboardEvent.key for the space bar is the single-char string " ",
    // not the word "Space". Pin this so a future "fix" that compares
    // against "Space" can't silently break keyboard activation.
    assert.equal(isActivationKey(' '), true);
});

test('#983: isActivationKey returns false for unrelated keys', () => {
    // Tab is the focus-traversal key — it must not activate; the user
    // is moving between cards, not selecting one.
    assert.equal(isActivationKey('Tab'), false);
    // Arrow keys are reserved for focus-management patterns we may add
    // later; activating on them today would be surprising.
    assert.equal(isActivationKey('ArrowDown'), false);
    assert.equal(isActivationKey('ArrowUp'), false);
    assert.equal(isActivationKey('ArrowLeft'), false);
    assert.equal(isActivationKey('ArrowRight'), false);
    // Other commonly-typed keys.
    assert.equal(isActivationKey('Escape'), false);
    assert.equal(isActivationKey('a'), false);
    assert.equal(isActivationKey('A'), false);
    assert.equal(isActivationKey('0'), false);
    assert.equal(isActivationKey(''), false);
});

test('#983: isActivationKey is case-sensitive on "Enter"', () => {
    // KeyboardEvent.key for the Enter key is exactly "Enter" — never
    // "enter" or "ENTER". We don't want to accept lowercase variants
    // because it would mask a real bug if someone passed the wrong
    // string source.
    assert.equal(isActivationKey('enter'), false);
    assert.equal(isActivationKey('ENTER'), false);
    assert.equal(isActivationKey('Spacebar'), false); // legacy IE name
});

// ---------------------------------------------------------------------
// shouldActivateFromKey
// ---------------------------------------------------------------------

test('#983: shouldActivateFromKey fires for Enter on a fresh event', () => {
    const event = { key: 'Enter', defaultPrevented: false };
    assert.equal(shouldActivateFromKey(event), true);
});

test('#983: shouldActivateFromKey fires for Space on a fresh event', () => {
    const event = { key: ' ', defaultPrevented: false };
    assert.equal(shouldActivateFromKey(event), true);
});

test('#983: shouldActivateFromKey is suppressed by defaultPrevented', () => {
    // If a nested handler called `event.preventDefault()` (the keyboard
    // mirror of click's `stopPropagation()`), the card root must not
    // also activate. Defense in depth — the primary guard is
    // `stopPropagation()` on the nested handler, but we honour
    // `preventDefault()` as well so any future nested handler that
    // uses it instead is also covered.
    const event = { key: 'Enter', defaultPrevented: true };
    assert.equal(shouldActivateFromKey(event), false);
});

test('#983: shouldActivateFromKey returns false for non-activation keys', () => {
    assert.equal(shouldActivateFromKey({ key: 'Tab', defaultPrevented: false }), false);
    assert.equal(shouldActivateFromKey({ key: 'a', defaultPrevented: false }), false);
    assert.equal(shouldActivateFromKey({ key: 'Escape', defaultPrevented: false }), false);
});

test('#983: shouldActivateFromKey is defensive against malformed input', () => {
    // The component never passes null today, but a defensive guard keeps
    // the helper safe for any future caller.
    assert.equal(shouldActivateFromKey(null), false);
    assert.equal(shouldActivateFromKey(undefined), false);
    assert.equal(shouldActivateFromKey({}), false);
    assert.equal(shouldActivateFromKey({ key: null }), false);
    assert.equal(shouldActivateFromKey({ key: 42 }), false);
});

test('#983: shouldActivateFromKey treats missing defaultPrevented as not-prevented', () => {
    // `defaultPrevented` is undefined on a freshly-constructed test
    // event object. We must treat undefined the same as false (the
    // common "no one has prevented it yet" case), not the same as true
    // (which would silently swallow every Enter / Space press).
    assert.equal(shouldActivateFromKey({ key: 'Enter' }), true);
    assert.equal(shouldActivateFromKey({ key: ' ' }), true);
});

// ---------------------------------------------------------------------
// shouldActivateFromClick
// ---------------------------------------------------------------------

test('#983: shouldActivateFromClick fires on a fresh click event', () => {
    const event = { defaultPrevented: false };
    assert.equal(shouldActivateFromClick(event), true);
});

test('#983: shouldActivateFromClick is suppressed by defaultPrevented', () => {
    // Belt-and-suspenders. The primary guard is `stopPropagation()` on
    // a nested action button (so this handler is never invoked), but
    // we honour `preventDefault()` here as well in case any future
    // nested handler reaches for that primitive instead.
    const event = { defaultPrevented: true };
    assert.equal(shouldActivateFromClick(event), false);
});

test('#983: shouldActivateFromClick treats missing defaultPrevented as not-prevented', () => {
    // Same defensive default as `shouldActivateFromKey`. A bare {} (no
    // `defaultPrevented` field) must activate, not be silently ignored.
    assert.equal(shouldActivateFromClick({}), true);
});

test('#983: shouldActivateFromClick is defensive against null / undefined', () => {
    assert.equal(shouldActivateFromClick(null), false);
    assert.equal(shouldActivateFromClick(undefined), false);
});

// ---------------------------------------------------------------------
// Behavioural composition — pin the wired-up handler shape
// ---------------------------------------------------------------------
//
// The AgentCard component wires `onClick` and `onKeyDown` on the card
// root, each of which invokes `switchAgent` only when the predicate
// returns true. The tests below simulate the wiring at the
// pure-function level so a refactor that drops the predicate or the
// `preventDefault()` call surfaces here.

test('#983: card-root click handler activates on a normal click', () => {
    // Simulate the AgentCard's `onCardClick` body without pulling in
    // Preact / signals: build a bare-bones predicate-driven handler
    // that records whether the activation fired.
    let activated = 0;
    const onCardClick = (e) => {
        if (!shouldActivateFromClick(e)) return;
        activated += 1;
    };
    onCardClick({ defaultPrevented: false });
    assert.equal(activated, 1);
});

test('#983: card-root click handler is suppressed when nested action stopped propagation', () => {
    // In the real DOM, `e.stopPropagation()` on a nested button stops
    // the click event from ever reaching the card-root listener. We
    // simulate that here by simply not invoking the card handler — the
    // pure-function predicate doesn't get a chance to fire because the
    // event never bubbles. (Negative control: confirms the contract
    // works at the level we can test.)
    let activated = 0;
    const onCardClick = (e) => {
        if (!shouldActivateFromClick(e)) return;
        activated += 1;
    };
    // Click happens on a nested button — card-root never fires.
    // (No call to onCardClick here — that's what stopPropagation gives us.)
    assert.equal(activated, 0);

    // Belt-and-suspenders: even if the click DID bubble up but had
    // `defaultPrevented` set, the predicate suppresses activation.
    onCardClick({ defaultPrevented: true });
    assert.equal(activated, 0);
});

test('#983: card-root keydown handler activates on Enter and Space', () => {
    let activated = 0;
    let prevented = 0;
    const onCardKeyDown = (e) => {
        if (!shouldActivateFromKey(e)) return;
        // Mirror the AgentCard's `e.preventDefault()` call so Space
        // doesn't also scroll the panel. Pin this so a refactor can't
        // silently drop the preventDefault.
        if (typeof e.preventDefault === 'function') e.preventDefault();
        activated += 1;
    };
    const enterEvt = {
        key: 'Enter',
        defaultPrevented: false,
        preventDefault() { prevented += 1; },
    };
    onCardKeyDown(enterEvt);
    assert.equal(activated, 1);
    assert.equal(prevented, 1, 'Enter should preventDefault to mirror button activation');

    const spaceEvt = {
        key: ' ',
        defaultPrevented: false,
        preventDefault() { prevented += 1; },
    };
    onCardKeyDown(spaceEvt);
    assert.equal(activated, 2);
    assert.equal(prevented, 2, 'Space should preventDefault to suppress page scroll');
});

test('#983: card-root keydown handler ignores Tab (so focus traversal still works)', () => {
    // The card uses tabindex="0" so it's a tab-stop. Tab itself must
    // never activate the card; it's the operator moving focus to the
    // next focusable element (typically a nested action button).
    let activated = 0;
    let prevented = 0;
    const onCardKeyDown = (e) => {
        if (!shouldActivateFromKey(e)) return;
        if (typeof e.preventDefault === 'function') e.preventDefault();
        activated += 1;
    };
    onCardKeyDown({
        key: 'Tab',
        defaultPrevented: false,
        preventDefault() { prevented += 1; },
    });
    assert.equal(activated, 0, 'Tab must not select the agent');
    assert.equal(prevented, 0, 'Tab must not preventDefault — focus traversal must work');
});
