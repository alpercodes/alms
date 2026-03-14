// Central dependency re-exports.
// Actual CDN URLs are pinned in index.html's importmap — this file
// just re-exports so components import from one place.
import { h, render } from 'preact';
import { useRef, useEffect, useCallback, useMemo } from 'preact/hooks';
import { signal, computed, effect, batch, useSignal } from '@preact/signals';
import htm from 'htm';

const html = htm.bind(h);

export { h, render, signal, computed, effect, batch, useSignal, html, useRef, useEffect, useCallback, useMemo };
