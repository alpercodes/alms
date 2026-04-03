// Central dependency re-exports.
// Actual CDN URLs are pinned in index.html's importmap — this file
// just re-exports so components import from one place.
import { h, render } from 'preact';
import { useRef, useEffect, useCallback, useMemo } from 'preact/hooks';
import { signal, computed, effect, batch, useSignal } from '@preact/signals';
import htm from 'htm';
import { marked } from 'marked';
import DOMPurify from 'dompurify';

const html = htm.bind(h);

// Configure marked: disable mangle/headerIds to keep output clean
marked.setOptions({
    breaks: true,
    gfm: true,
});

/**
 * Render a Markdown string to sanitized HTML.
 */
export function renderMarkdown(src) {
    if (!src) return '';
    const raw = marked.parse(src);
    return DOMPurify.sanitize(raw);
}

export { h, render, signal, computed, effect, batch, useSignal, html, useRef, useEffect, useCallback, useMemo };
