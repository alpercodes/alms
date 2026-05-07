/**
 * Thin yellow banner rendered at the top of the layout when at
 * least one SSE stream has exhausted its retry budget (#907).
 *
 * Visibility is driven by the `streamDead` signal in
 * `state/stream-health.js`; the click handler calls
 * `reconnectAllStreams()` which fans out to whatever reconnect
 * callbacks the SSE hooks have registered. A successful reopen
 * causes the hook to call `clearStreamDead(source)` which flips
 * the signal back to `false` and unmounts the banner.
 *
 * The banner is intentionally lean (no close button, no progress
 * spinner) — the intent is "I noticed live updates stopped, click
 * to fix it" not "modal that demands user attention". A second
 * click after the streams are healthy again is harmless because
 * `reconnectAllStreams` re-uses the existing reconnect callbacks
 * which are themselves idempotent.
 */

import { html } from '../deps.js';
import {
    streamDead,
    reconnectAllStreams,
} from '../state/stream-health.js';

/**
 * Render the stream-dead banner when at least one SSE stream has
 * exhausted its retry budget. Returns `null` (no DOM) on the happy
 * path so the banner has zero footprint when streams are healthy.
 */
export function StreamDeadBanner() {
    if (!streamDead.value) return null;
    return html`
        <button
            type="button"
            class="stream-dead-banner"
            role="alert"
            aria-live="polite"
            onClick=${reconnectAllStreams}
            title="Click to reconnect live updates"
        >
            <span class="stream-dead-banner-icon" aria-hidden="true">⚠</span>
            <span class="stream-dead-banner-text">
                Live updates disconnected — click to reconnect or reload.
            </span>
        </button>
    `;
}
