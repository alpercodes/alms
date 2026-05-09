// Pure-function helpers for the code-block copy-to-clipboard affordance.
//
// Pinned regression target:
//   - issue #986 — every fenced code block in an agent message gets a
//     copy-icon button at the top-right. Click → write the block's raw
//     contents to the clipboard. The hard part is _decorating_ the
//     post-render DOM (the markdown body is injected via
//     `dangerouslySetInnerHTML` so we can't render Preact components
//     inside it) and _what text to put on the clipboard_.
//
// This module owns the second half: the pure-function logic that
// extracts the clipboard payload from a `<pre>` element. Marked already
// strips the fence syntax (the ```` ``` ````) and folds the language tag
// into a `class="language-foo"` on the `<code>` child, so our job is
// reduced to:
//
//   1. Read text from the inner `<code>` if present (so the language
//      tag — which is a class, not text — never lands on the clipboard).
//   2. Fall back to the `<pre>`'s own textContent if there's no `<code>`
//      child (defensive — marked always emits one for fenced blocks but
//      a future custom renderer might not).
//   3. Trim a single trailing newline if present (marked appends one to
//      every fenced block; the operator pasting into a terminal almost
//      never wants the auto-newline tacked on).
//
// What this file does NOT own:
//   - The actual `navigator.clipboard.writeText` call. That lives in
//     the click handler in `Message` / `decorate-code-blocks.js` and is
//     side-effecting — not unit-testable under Node without a browser
//     harness. We smoke-test it manually in the browser.
//   - The visual feedback (icon swap, "Copied" label). Same reason.
//   - Decorating the DOM (finding `<pre>` elements, injecting buttons).
//     That's `decorate-code-blocks.js`, which uses the helpers here.
//
// Test surface (`code-copy.test.mjs`):
//   - extractCopyText returns the inner-`<code>` text when present.
//   - extractCopyText falls back to the `<pre>`'s textContent when no
//     inner `<code>` exists.
//   - The single trailing newline marked always appends is stripped.
//   - Internal whitespace, blank lines, and multi-line content survive
//     unchanged.
//   - Empty / null / non-element inputs return an empty string rather
//     than throwing.

/**
 * Extract the text payload to put on the clipboard for a given code-
 * block `<pre>` element.
 *
 * The element shape this function relies on is intentionally narrow so
 * Node-level tests can pass a plain `{ querySelector, textContent }`
 * stub without standing up a full JSDOM. In the browser the same shape
 * is satisfied by every `HTMLPreElement`.
 *
 * @param {{ querySelector?: (sel: string) => { textContent?: string } | null,
 *           textContent?: string } | null | undefined} preEl
 * @returns {string} clipboard payload — never null, never undefined.
 */
export function extractCopyText(preEl) {
    if (!preEl) return '';

    // Marked emits `<pre><code class="language-foo">...</code></pre>`
    // for fenced blocks. The `<code>` child holds the actual text; its
    // class carries the language tag (not text), so reading textContent
    // off the `<code>` automatically excludes the language artifact.
    let raw = '';
    if (typeof preEl.querySelector === 'function') {
        const code = preEl.querySelector('code');
        if (code && typeof code.textContent === 'string') {
            raw = code.textContent;
        }
    }
    // Fallback: no `<code>` child (defensive — marked always emits one
    // for fenced blocks, but indented code or a custom renderer might
    // skip it). Read the `<pre>`'s own textContent.
    if (!raw && typeof preEl.textContent === 'string') {
        raw = preEl.textContent;
    }
    if (!raw) return '';

    // Marked appends a trailing newline to every fenced block. The
    // operator pasting into a terminal / editor almost never wants that
    // auto-newline; strip a single trailing `\n` (or `\r\n`) but leave
    // any further blank lines the agent intentionally added intact.
    if (raw.endsWith('\r\n')) {
        raw = raw.slice(0, -2);
    } else if (raw.endsWith('\n')) {
        raw = raw.slice(0, -1);
    }

    return raw;
}

/**
 * True iff the host environment has the modern async clipboard API.
 * The decorator falls back to `document.execCommand('copy')` (or
 * graceful no-op) when this returns false. Pure check — pulled out so
 * the decorator stays straightforward and the gate is explicit.
 *
 * @returns {boolean}
 */
export function hasAsyncClipboard() {
    return !!(typeof navigator !== 'undefined'
        && navigator.clipboard
        && typeof navigator.clipboard.writeText === 'function');
}
