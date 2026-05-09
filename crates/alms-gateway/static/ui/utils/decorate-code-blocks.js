// Side-effecting DOM decorator that injects a copy-to-clipboard button
// into every fenced code block inside a markdown-rendered message body.
//
// Why a post-render mutation pass and not a Preact component?
//   The agent-message body is rendered via `dangerouslySetInnerHTML` —
//   it's a sanitized HTML string from `marked.parse() + DOMPurify`, so
//   Preact never sees the DOM tree underneath. We can't drop a Preact
//   component inside it. The two practical alternatives are:
//
//     1. Override marked's code renderer to embed a button. DOMPurify
//        strips inline `onclick` handlers (and unknown attributes), so
//        a delegated handler at the body level would still need to
//        find the button via class — that gives no real win over (2).
//     2. Walk the post-render DOM, decorate each `<pre>` with a
//        button + click handler. This is the simplest path: no marked
//        renderer override, no fight with DOMPurify, and the button
//        is a real DOM element with a real listener that we control.
//
//   The decorator is idempotent — re-running it on the same body is a
//   no-op once buttons are present. That keeps it safe to call from a
//   `useEffect` that re-fires on text updates (which never happens for
//   sealed messages today, but defends against a future change).
//
// Pinned regression target:
//   - issue #986 — copy button on every code block in agent messages.
//
// Pure logic (extractCopyText, hasAsyncClipboard) lives in
// `code-copy.js` and is unit-tested in `code-copy.test.mjs`. This file
// is intentionally not unit-tested — its surface is `document` /
// `navigator.clipboard` / event listeners, all of which require a real
// browser. Manual smoke-test confirmation is noted in the PR.

import { extractCopyText, hasAsyncClipboard } from './code-copy.js';

// Marker class so the decorator is idempotent — re-running it on a
// body that already has buttons is a no-op.
const DECORATED_FLAG = 'cb-copy-decorated';

// CSS class names — kept in one place so the matching styles in
// `styles/chat.css` can be grep'd from one search term.
const WRAPPER_CLS = 'code-block-wrapper';
const BUTTON_CLS = 'code-block-copy';
const BUTTON_COPIED_CLS = 'code-block-copy--copied';

// Shared aria-live region for screen-reader announcements on copy
// success. One node per page (lazily created on first success), reused
// for every subsequent copy. `aria-live="polite"` so the announcement
// queues behind in-progress speech rather than interrupting it; the
// message is wiped to '' between announcements so SR reads it fresh
// even when the same text ("Copied to clipboard") repeats.
const LIVE_REGION_ID = 'alms-code-copy-live';

/**
 * Get-or-create the shared aria-live region used to announce copy
 * success to screen-reader users. Returns null when there's no
 * `document` (test harness without a DOM), in which case the caller
 * just skips the announcement.
 */
function ensureLiveRegion() {
    if (typeof document === 'undefined') return null;
    let node = document.getElementById(LIVE_REGION_ID);
    if (node) return node;
    node = document.createElement('div');
    node.id = LIVE_REGION_ID;
    node.setAttribute('aria-live', 'polite');
    node.setAttribute('role', 'status');
    // Visually hidden but reachable by SR. Avoids `display: none` /
    // `visibility: hidden` which most SRs skip.
    node.style.position = 'absolute';
    node.style.width = '1px';
    node.style.height = '1px';
    node.style.padding = '0';
    node.style.margin = '-1px';
    node.style.overflow = 'hidden';
    node.style.clip = 'rect(0, 0, 0, 0)';
    node.style.whiteSpace = 'nowrap';
    node.style.border = '0';
    document.body.appendChild(node);
    return node;
}

/**
 * Announce copy success to screen-reader users via the shared aria-live
 * region. Wipes-then-sets the message so repeat announcements fire
 * even when the text is identical to the previous one.
 */
function announceCopySuccess() {
    const node = ensureLiveRegion();
    if (!node) return;
    node.textContent = '';
    // Defer the actual text set so the wipe is observed first; some
    // SRs coalesce consecutive same-frame mutations and would skip
    // the announcement otherwise.
    setTimeout(() => {
        node.textContent = 'Copied to clipboard';
    }, 50);
}

/**
 * Build the SVG markup for the default ("copy") icon. Inline SVG so it
 * inherits `currentColor` and matches the stroke-based aesthetic of
 * `utils/icons.js`. Two overlapping rounded rectangles — the universal
 * "copy to clipboard" glyph.
 */
function copyIconMarkup() {
    return [
        '<svg width="14" height="14" viewBox="0 0 20 20" fill="none" ',
        'stroke="currentColor" stroke-width="1.5" stroke-linecap="round" ',
        'stroke-linejoin="round" aria-hidden="true">',
        '<rect x="7" y="7" width="10" height="10" rx="1.5"/>',
        '<path d="M5 13H4a1 1 0 01-1-1V4a1 1 0 011-1h8a1 1 0 011 1v1"/>',
        '</svg>',
    ].join('');
}

/**
 * Build the SVG markup for the post-copy ("checkmark") icon. Same 14×14
 * sizing so the swap doesn't shift layout.
 */
function checkIconMarkup() {
    return [
        '<svg width="14" height="14" viewBox="0 0 20 20" fill="none" ',
        'stroke="currentColor" stroke-width="2" stroke-linecap="round" ',
        'stroke-linejoin="round" aria-hidden="true">',
        '<path d="M4 10l4 4 8-8"/>',
        '</svg>',
    ].join('');
}

/**
 * Fallback clipboard write for browsers without `navigator.clipboard`.
 * Uses the legacy `document.execCommand('copy')` against an off-screen
 * textarea. Returns true on success, false on failure (or if the legacy
 * API is also unavailable, which is rare in 2026 but graceful).
 *
 * @param {string} text
 * @returns {boolean}
 */
function legacyCopyFallback(text) {
    if (typeof document === 'undefined') return false;
    const textarea = document.createElement('textarea');
    textarea.value = text;
    // Off-screen positioning so it never flashes visible — but the
    // element must be in the DOM and selectable for execCommand to
    // work. `position: fixed` keeps it out of layout flow.
    textarea.style.position = 'fixed';
    textarea.style.top = '-9999px';
    textarea.style.left = '-9999px';
    textarea.setAttribute('readonly', '');
    textarea.setAttribute('aria-hidden', 'true');
    document.body.appendChild(textarea);
    let ok = false;
    try {
        textarea.select();
        textarea.setSelectionRange(0, text.length);
        ok = document.execCommand && document.execCommand('copy');
    } catch {
        ok = false;
    }
    document.body.removeChild(textarea);
    return !!ok;
}

/**
 * Show "Copied" feedback on the button for ~1.5s, then revert. The
 * button stores its pending revert timer on a property so a rapid
 * double-click resets cleanly instead of leaving a stale checkmark.
 */
function flashCopiedFeedback(button) {
    if (!button) return;
    if (button._copyRevertTimer) {
        clearTimeout(button._copyRevertTimer);
        button._copyRevertTimer = null;
    }
    button.classList.add(BUTTON_COPIED_CLS);
    button.innerHTML = checkIconMarkup();
    button.setAttribute('aria-label', 'Copied');
    button.title = 'Copied';
    announceCopySuccess();
    button._copyRevertTimer = setTimeout(() => {
        button.classList.remove(BUTTON_COPIED_CLS);
        button.innerHTML = copyIconMarkup();
        button.setAttribute('aria-label', 'Copy code');
        button.title = 'Copy code';
        button._copyRevertTimer = null;
    }, 1500);
}

/**
 * Click handler for a single copy button. Reads the clipboard payload
 * via `extractCopyText` then writes through whichever clipboard API is
 * available. On failure, leaves the button untouched (no false-positive
 * checkmark).
 */
function handleCopyClick(event, preEl, button) {
    event.preventDefault();
    event.stopPropagation();
    const text = extractCopyText(preEl);
    if (!text) return;

    if (hasAsyncClipboard()) {
        navigator.clipboard.writeText(text).then(
            () => flashCopiedFeedback(button),
            () => {
                // Async path rejected (permission denied, document not
                // focused, etc.) — try the legacy fallback before
                // giving up so the operator at least gets the copy.
                if (legacyCopyFallback(text)) {
                    flashCopiedFeedback(button);
                }
            },
        );
        return;
    }

    if (legacyCopyFallback(text)) {
        flashCopiedFeedback(button);
    }
}

/**
 * Decorate every fenced code block (`<pre>` element) inside the given
 * root with a copy-to-clipboard button. Idempotent — re-running on the
 * same root with the same blocks is a no-op.
 *
 * @param {HTMLElement | null | undefined} root — the markdown body
 *   element, e.g. `<div class="msg-body markdown-body">`. When null /
 *   undefined / not an element, returns immediately (defensive — the
 *   ref may not be attached yet on the first effect run).
 */
export function decorateCodeBlocks(root) {
    if (!root || typeof root.querySelectorAll !== 'function') return;

    const blocks = root.querySelectorAll('pre');
    for (let i = 0; i < blocks.length; i++) {
        const pre = blocks[i];
        if (pre.classList.contains(DECORATED_FLAG)) continue;

        // Wrap the `<pre>` in a positioning container so the absolutely
        // positioned button anchors to the block (not to the page or
        // the message body). The wrapper inherits the block-level
        // semantics; the button sits inside it.
        const parent = pre.parentNode;
        if (!parent) continue;

        // Skip empty blocks — nothing to copy, button would mislead.
        const hasText = (pre.textContent || '').trim().length > 0;
        if (!hasText) {
            pre.classList.add(DECORATED_FLAG);
            continue;
        }

        const wrapper = document.createElement('div');
        wrapper.className = WRAPPER_CLS;
        parent.insertBefore(wrapper, pre);
        wrapper.appendChild(pre);

        const button = document.createElement('button');
        button.type = 'button';
        button.className = BUTTON_CLS;
        button.setAttribute('aria-label', 'Copy code');
        button.title = 'Copy code';
        button.innerHTML = copyIconMarkup();
        button.addEventListener('click', (e) => handleCopyClick(e, pre, button));
        wrapper.appendChild(button);

        pre.classList.add(DECORATED_FLAG);
    }
}
