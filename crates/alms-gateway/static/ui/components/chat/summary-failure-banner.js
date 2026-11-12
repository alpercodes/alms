/**
 * Banner at the top of the chat when the session's conversation summaries are
 * failing. The wording comes from the gateway (`SummaryFailure::message`),
 * which knows which provider was configured and whether a key is the fix.
 *
 * Renders nothing when there is no problem, and goes away by itself when the
 * gateway reports the problem fixed.
 */

import { html } from '../../deps.js';
import {
    dismissSummaryFailure,
    visibleSummaryFailure,
} from '../../state/summary-status.js';

export function SummaryFailureBanner({ sessionId, onOpenSettings }) {
    const failure = visibleSummaryFailure(sessionId);
    if (!failure) return null;
    return html`
        <div class="summary-failure-banner" role="status" data-code=${failure.code}>
            <span class="summary-failure-banner-icon" aria-hidden="true">⚠</span>
            <div class="summary-failure-banner-body">
                <div class="summary-failure-banner-text">${failure.message}</div>
                <div class="summary-failure-banner-note">
                    Your chat keeps working. Older messages just aren't being condensed.
                </div>
            </div>
            <button type="button" class="summary-failure-banner-action" onClick=${onOpenSettings}>
                Open Settings
            </button>
            <button type="button" class="summary-failure-banner-dismiss"
                    aria-label="Dismiss summary warning" title="Dismiss"
                    onClick=${dismissSummaryFailure}>
                ×
            </button>
        </div>
    `;
}
