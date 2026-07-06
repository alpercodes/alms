import { html, useSignal, useEffect, renderMarkdown } from '../../deps.js';
import { getRun } from '../../api/runs.js';
import { shouldFetchFullOutput, resolveDisplayedSummary } from '../../utils/job-summary.js';

/**
 * Collapsible card for scheduled job completion messages.
 *
 * Visually distinct from regular chat messages -- shows the job name,
 * status badge, formatted summary (with markdown), and timestamp.
 * Long output is collapsed by default with a toggle.
 *
 * The stored summary is capped by the backend (JOB_SUMMARY_MAX_CHARS = 4000).
 * When it hits that cap the full agent output still lives on the run record;
 * on the first expand the card fetches it via GET /runs/{runId} and shows the
 * complete text, degrading gracefully to the stored summary on any failure
 * (#1196).
 *
 * Props:
 *   jobName  - string, the job prompt/name (may be truncated by backend)
 *   status   - 'success' | 'error' | 'cancelled'
 *   summary  - string, the job output (may contain markdown; may be truncated)
 *   ts        - string, ISO 8601 timestamp (optional)
 *   runId     - string, the run that produced this completion (optional; used
 *               to fetch the full persisted output on expand)
 *   truncated - bool, whether the stored summary was capped and the full
 *               output is fetchable via GET /runs/{runId} (optional)
 */

const COLLAPSE_THRESHOLD = 150;

function formatTimestamp(ts) {
    if (!ts) return '';
    try {
        const d = new Date(ts);
        return d.toLocaleTimeString(undefined, {
            hour: '2-digit',
            minute: '2-digit',
        });
    } catch {
        return '';
    }
}

function statusLabel(status) {
    switch (status) {
        case 'success': return 'Completed';
        case 'error': return 'Failed';
        case 'cancelled': return 'Cancelled';
        default: return 'Finished';
    }
}

function statusIcon(status) {
    switch (status) {
        case 'success': return '\u2713';   // checkmark
        case 'error': return '\u2717';     // cross
        case 'cancelled': return '\u2013'; // en-dash
        default: return '\u2022';          // bullet
    }
}

export function JobCompletionCard({ jobName, status, summary, ts, runId, truncated }) {
    const expanded = useSignal(false);
    // Full persisted output, fetched lazily on the first expand when the
    // stored summary was truncated (#1196). `null` until fetched; the render
    // falls back to the stored `summary` while null.
    const fullSummary = useSignal(null);
    const fetching = useSignal(false);

    const isLong = summary && summary.length > COLLAPSE_THRESHOLD;
    const showBody = !isLong || expanded.value;

    // On first expand, fetch the complete output via GET /runs/{runId} — but
    // only when the stored summary appears truncated and we have a run id.
    // Any failure (run evicted from memory after a restart, network error)
    // leaves `fullSummary` as the stored text, so the card degrades to what
    // it already showed.
    useEffect(() => {
        if (!expanded.value) return;
        if (fullSummary.value !== null || fetching.value) return;
        if (!shouldFetchFullOutput({ runId, summary, truncated })) return;

        fetching.value = true;
        let cancelled = false;
        getRun(runId)
            .then((run) => {
                if (cancelled) return;
                fullSummary.value = resolveDisplayedSummary(summary, run && run.response);
            })
            .catch(() => {
                if (cancelled) return;
                fullSummary.value = summary || '';
            })
            .finally(() => {
                if (!cancelled) fetching.value = false;
            });
        return () => { cancelled = true; };
    }, [expanded.value, runId, summary, truncated]);

    // Prefer the fetched full output once it lands; otherwise the stored
    // summary (which may end with the backend's "..." truncation marker).
    const effectiveSummary = fullSummary.value != null ? fullSummary.value : summary;

    const statusCls = `job-card--${status || 'success'}`;
    const time = formatTimestamp(ts);
    const icon = statusIcon(status);
    const label = statusLabel(status);

    const toggle = () => {
        expanded.value = !expanded.value;
    };

    // Render summary with markdown for completed jobs
    const renderedSummary = effectiveSummary
        ? renderMarkdown(effectiveSummary)
        : '';

    return html`
        <div class="job-card ${statusCls}">
            <div class="job-card-header">
                <span class="job-card-icon">${icon}</span>
                <span class="job-card-badge">${label}</span>
                <span class="job-card-label">Scheduled Job</span>
                ${time && html`<span class="job-card-time">${time}</span>`}
            </div>
            <div class="job-card-name">${jobName || 'unnamed job'}</div>
            ${summary && html`
                <div class="job-card-body">
                    ${showBody
                        ? html`<div class="job-card-summary markdown-body"
                                     dangerouslySetInnerHTML=${{ __html: renderedSummary }} />`
                        : html`<div class="job-card-summary-truncated">
                                ${summary.slice(0, COLLAPSE_THRESHOLD)}...
                            </div>`
                    }
                    ${isLong && html`
                        <button class="job-card-toggle" onClick=${toggle}>
                            ${expanded.value ? 'Show less' : 'Show more'}
                        </button>
                    `}
                </div>
            `}
        </div>
    `;
}
