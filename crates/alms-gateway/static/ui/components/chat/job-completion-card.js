import { html, useSignal, renderMarkdown } from '../../deps.js';

/**
 * Collapsible card for scheduled job completion messages.
 *
 * Visually distinct from regular chat messages -- shows the job name,
 * status badge, formatted summary (with markdown), and timestamp.
 * Long output is collapsed by default with a toggle.
 *
 * Props:
 *   jobName  - string, the job prompt/name (may be truncated by backend)
 *   status   - 'success' | 'error' | 'cancelled'
 *   summary  - string, the job output (may contain markdown)
 *   ts       - string, ISO 8601 timestamp (optional)
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

export function JobCompletionCard({ jobName, status, summary, ts }) {
    const expanded = useSignal(false);
    const isLong = summary && summary.length > COLLAPSE_THRESHOLD;
    const showBody = !isLong || expanded.value;

    const statusCls = `job-card--${status || 'success'}`;
    const time = formatTimestamp(ts);
    const icon = statusIcon(status);
    const label = statusLabel(status);

    const toggle = () => {
        expanded.value = !expanded.value;
    };

    // Render summary with markdown for completed jobs
    const renderedSummary = summary
        ? renderMarkdown(summary)
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
