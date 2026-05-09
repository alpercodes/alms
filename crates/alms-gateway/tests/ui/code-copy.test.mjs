// JS-level regression tests for `static/ui/utils/code-copy.js`.
//
// Pinned regression target:
//   - issue #986 — every fenced code block in agent messages gets a
//     copy-to-clipboard button. The pure-function logic that decides
//     _what_ to put on the clipboard lives in `code-copy.js` and is
//     unit-testable; the side-effecting half (button injection,
//     `navigator.clipboard.writeText`, visual feedback) lives in
//     `decorate-code-blocks.js` and needs a real browser harness, so
//     it's smoke-tested manually instead.
//
// What this file pins:
//   - `extractCopyText` reads from the inner `<code>` child when
//     present, so the language tag (which marked folds into a class
//     attribute) never lands on the clipboard.
//   - The trailing newline marked appends to every fenced block is
//     stripped (operators pasting into a terminal almost never want
//     the auto-newline tacked on).
//   - Multi-line content + internal blank lines + indentation survive
//     untouched.
//   - Defensive cases — null / undefined / empty / non-element inputs
//     all return `''` rather than throwing.
//   - `hasAsyncClipboard` returns true when the global `navigator`
//     exposes `clipboard.writeText`, false when missing — the gate
//     used by the decorator to choose between the modern API and the
//     legacy `document.execCommand('copy')` fallback.
//
// What this file does NOT pin:
//   - The clipboard write itself (`navigator.clipboard.writeText`) —
//     side-effecting, browser-only, smoke-tested manually.
//   - The icon swap / "Copied" feedback — DOM-mutation, browser-only.
//   - The decorator's button-injection pass — DOM-mutation, browser-only.
//
// Pattern follows `card-activation.test.mjs` and other pure-function
// regression tests under `tests/ui/`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import url from 'node:url';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const CODE_COPY_PATH = path.resolve(
    __dirname,
    '../../static/ui/utils/code-copy.js'
);

const { extractCopyText, hasAsyncClipboard } =
    await import(url.pathToFileURL(CODE_COPY_PATH).href);

// ---------------------------------------------------------------------
// Test helpers — minimal `<pre>` element shims
// ---------------------------------------------------------------------

/**
 * Build a `<pre>`-shaped stub with the given inner-`<code>` text. The
 * shape matches what `extractCopyText` actually reads:
 *   - `querySelector('code')` returns the code child (or null).
 *   - `textContent` is the concatenated text of the entire block.
 *
 * Marked's output for a fenced block ` ```rust\nfn main(){}\n``` ` is
 * `<pre><code class="language-rust">fn main(){}\n</code></pre>` —
 * the trailing `\n` lives inside the `<code>`. We mirror that here.
 */
function makePre({ codeText, noCodeChild }) {
    if (noCodeChild) {
        return {
            querySelector: () => null,
            textContent: codeText,
        };
    }
    const codeNode = { textContent: codeText };
    return {
        querySelector: (sel) => (sel === 'code' ? codeNode : null),
        textContent: codeText,
    };
}

// ---------------------------------------------------------------------
// extractCopyText — happy paths
// ---------------------------------------------------------------------

test('#986: extractCopyText reads the inner <code> textContent', () => {
    const pre = makePre({ codeText: 'cargo test --all' });
    assert.equal(extractCopyText(pre), 'cargo test --all');
});

test('#986: extractCopyText strips the single trailing newline marked appends', () => {
    // Fenced block: ```rust\nfn main(){}\n``` → marked emits the inner
    // text with a trailing `\n`. The operator pasting into a shell
    // doesn't want that auto-newline.
    const pre = makePre({ codeText: 'fn main() { println!("hi"); }\n' });
    assert.equal(
        extractCopyText(pre),
        'fn main() { println!("hi"); }',
    );
});

test('#986: extractCopyText strips a single trailing CRLF', () => {
    // Defensive — Windows-line-ending agent output should round-trip
    // the same as `\n`-terminated output. Internal CRLFs are
    // preserved (only the final pair is stripped).
    const pre = makePre({ codeText: 'line1\r\nline2\r\n' });
    assert.equal(extractCopyText(pre), 'line1\r\nline2');
});

test('#986: extractCopyText preserves internal newlines and indentation', () => {
    const codeText = [
        'fn main() {',
        '    let x = 1;',
        '    let y = 2;',
        '    println!("{}", x + y);',
        '}',
        '', // marked's trailing newline → empty final segment
    ].join('\n');
    assert.equal(
        extractCopyText(makePre({ codeText })),
        [
            'fn main() {',
            '    let x = 1;',
            '    let y = 2;',
            '    println!("{}", x + y);',
            '}',
        ].join('\n'),
    );
});

test('#986: extractCopyText preserves blank lines inside the block', () => {
    // Blank lines the agent intentionally added must survive. Only
    // _trailing_ newline stripping happens — internal whitespace is
    // load-bearing (e.g. paragraph breaks in a docstring).
    const codeText = [
        'first paragraph',
        '',
        'second paragraph',
        '',
    ].join('\n');
    assert.equal(
        extractCopyText(makePre({ codeText })),
        'first paragraph\n\nsecond paragraph',
    );
});

test('#986: extractCopyText keeps double-trailing-newline as one trailing newline', () => {
    // Only ONE trailing newline is stripped. If the block legitimately
    // ends with two (operator wrote `code\n\n`), the second survives so
    // the paste preserves the visible whitespace.
    const pre = makePre({ codeText: 'cmd\n\n' });
    assert.equal(extractCopyText(pre), 'cmd\n');
});

// ---------------------------------------------------------------------
// extractCopyText — language-tag exclusion (the headline test for #986)
// ---------------------------------------------------------------------

test('#986: extractCopyText ignores the language tag (it lives on the class, not in text)', () => {
    // Marked's output for ```rust\nfn main(){}\n``` is:
    //   <pre><code class="language-rust">fn main(){}\n</code></pre>
    // The "rust" tag is a class on the <code>, not text. Reading
    // textContent off the <code> automatically excludes it. We pin
    // that contract by building a stub where the only place the
    // language tag could possibly come from is the (absent) class —
    // textContent is just the body. The extracted clipboard payload
    // must NOT contain "rust".
    const pre = makePre({ codeText: 'fn main() { println!("hello"); }\n' });
    const out = extractCopyText(pre);
    assert.doesNotMatch(out, /^rust\b/, 'language tag must not prefix the payload');
    assert.doesNotMatch(out, /language-/, 'class artifact must not leak into the payload');
    assert.equal(out, 'fn main() { println!("hello"); }');
});

// ---------------------------------------------------------------------
// extractCopyText — fallback path (no inner <code>)
// ---------------------------------------------------------------------

test('#986: extractCopyText falls back to <pre> textContent when no <code> child', () => {
    // Defensive — marked always emits a <code> child for fenced
    // blocks, but a future custom renderer or an indented code block
    // might not. The fallback reads the <pre>'s own textContent so we
    // never silently produce an empty clipboard.
    const pre = makePre({ codeText: 'no-code-child content', noCodeChild: true });
    assert.equal(extractCopyText(pre), 'no-code-child content');
});

test('#986: extractCopyText fallback also strips trailing newline', () => {
    // Same trim semantics on the fallback path — consistent UX
    // regardless of which branch fires.
    const pre = makePre({ codeText: 'shell command\n', noCodeChild: true });
    assert.equal(extractCopyText(pre), 'shell command');
});

// ---------------------------------------------------------------------
// extractCopyText — defensive / boundary cases
// ---------------------------------------------------------------------

test('#986: extractCopyText returns "" for null / undefined input', () => {
    assert.equal(extractCopyText(null), '');
    assert.equal(extractCopyText(undefined), '');
});

test('#986: extractCopyText returns "" for empty content', () => {
    assert.equal(extractCopyText(makePre({ codeText: '' })), '');
});

test('#986: extractCopyText returns "" for a stub with neither querySelector nor textContent', () => {
    // Pathological input — a non-element shape. Must not throw, must
    // return the empty sentinel.
    assert.equal(extractCopyText({}), '');
});

test('#986: extractCopyText handles a stub where querySelector returns null and textContent is set', () => {
    // querySelector exists (so the function calls it) but returns null
    // (no <code> child found). The fallback path reads textContent.
    const pre = {
        querySelector: () => null,
        textContent: 'fallback text\n',
    };
    assert.equal(extractCopyText(pre), 'fallback text');
});

test('#986: extractCopyText handles a stub where querySelector throws-shaped (non-function)', () => {
    // Defensive: if querySelector is undefined or not callable, fall
    // through to textContent. The `typeof === 'function'` gate keeps
    // us from a TypeError.
    const pre = {
        querySelector: 'not-a-function',
        textContent: 'still works\n',
    };
    assert.equal(extractCopyText(pre), 'still works');
});

// ---------------------------------------------------------------------
// hasAsyncClipboard — environment probe
// ---------------------------------------------------------------------

test('#986: hasAsyncClipboard returns false when navigator is missing', () => {
    // Node's default global has no `navigator`, so the gate must
    // return false rather than throwing on the access. The decorator
    // relies on this to fall through to the legacy execCommand path.
    const original = globalThis.navigator;
    delete globalThis.navigator;
    try {
        assert.equal(hasAsyncClipboard(), false);
    } finally {
        if (original !== undefined) globalThis.navigator = original;
    }
});

test('#986: hasAsyncClipboard returns false when clipboard API is missing', () => {
    const original = globalThis.navigator;
    globalThis.navigator = {}; // no .clipboard
    try {
        assert.equal(hasAsyncClipboard(), false);
    } finally {
        if (original === undefined) delete globalThis.navigator;
        else globalThis.navigator = original;
    }
});

test('#986: hasAsyncClipboard returns true when navigator.clipboard.writeText exists', () => {
    const original = globalThis.navigator;
    globalThis.navigator = { clipboard: { writeText: () => Promise.resolve() } };
    try {
        assert.equal(hasAsyncClipboard(), true);
    } finally {
        if (original === undefined) delete globalThis.navigator;
        else globalThis.navigator = original;
    }
});

test('#986: hasAsyncClipboard returns false when writeText is not a function', () => {
    // Defensive — some browsers expose `navigator.clipboard` but only
    // `read`/`readText` (older Safari). The decorator's gate must
    // distinguish "API exists" from "writeText specifically exists".
    const original = globalThis.navigator;
    globalThis.navigator = { clipboard: { readText: () => Promise.resolve('') } };
    try {
        assert.equal(hasAsyncClipboard(), false);
    } finally {
        if (original === undefined) delete globalThis.navigator;
        else globalThis.navigator = original;
    }
});
