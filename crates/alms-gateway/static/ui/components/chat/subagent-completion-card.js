import { html, useSignal } from '../../deps.js';
import { navigateToSubagentSession } from '../../state/subagents.js';

/** Max characters for the result preview before truncation. */
const RESULT_PREVIEW_LEN = 200;

/** Format a duration in milliseconds to a human-readable string. */
function fmtDuration(ms) {
    if (ms == null) return '';
    if (ms < 1000) return ms + 'ms';
    if (ms < 60000) return (ms / 1000).toFixed(1) + 's';
    const mins = Math.floor(ms / 60000);
    const secs = Math.round((ms % 60000) / 1000);
    return mins + 'm ' + secs + 's';
}

function statusLabel(status) {
    switch (status) {
        case 'done': return 'Completed';
        case 'fail': return 'Failed';
        case 'cancelled': return 'Cancelled';
        default: return 'Completed';
    }
}

function statusIcon(status) {
    switch (status) {
        case 'done': return '\u2713';
        case 'fail': return '\u2717';
        case 'cancelled': return '\u2013';
        default: return '\u2713';
    }
}

/**
 * Persistent subagent completion card rendered in the chat after a subagent
 * finishes. Replaces the plain "Subagent 'X' completed." system message
 * with a rich summary including name, task, status, tool count, duration,
 * result preview, and a "View session" button for drill-down navigation.
 *
 * Props:
 *   name        - string, subagent display name
 *   task        - string, the task description given to the subagent
 *   status      - 'done' | 'fail' | 'cancelled'
 *   toolCount   - number, how many tools the subagent executed
 *   durationMs  - number|null, wall-clock duration in milliseconds
 *   sessionId   - string|null, the subagent's session UUID (for drill-down)
 *   summary     - string, brief result summary from the backend
 */
export function SubagentCompletionCard({ name, task, status, toolCount, durationMs, sessionId, summary }) {
    const expanded = useSignal(false);

    const statusCls = `sa-card--${status || 'done'}`;
    const icon = statusIcon(status);
    const label = statusLabel(status);
    const duration = fmtDuration(durationMs);

    const isLong = summary && summary.length > RESULT_PREVIEW_LEN;
    const displaySummary = (!expanded.value && isLong)
        ? summary.slice(0, RESULT_PREVIEW_LEN) + '\u2026'
        : summary;

    const onViewSession = (e) => {
        e.stopPropagation();
        if (sessionId) {
            navigateToSubagentSession(sessionId);
        }
    };

    return html`
        <div class="sa-card ${statusCls}">
            <div class="sa-card-header">
                <span class="sa-card-icon">${icon}</span>
                <span class="sa-card-badge">${label}</span>
                <span class="sa-card-label">Subagent</span>
                ${duration && html`<span class="sa-card-meta">${duration}</span>`}
                ${toolCount > 0 && html`<span class="sa-card-meta">${toolCount} tool${toolCount !== 1 ? 's' : ''}</span>`}
            </div>
            <div class="sa-card-name">${name || 'subagent'}</div>
            ${task && html`<div class="sa-card-task">${task}</div>`}
            ${summary && html`
                <div class="sa-card-body">
                    <div class="sa-card-summary">${displaySummary}</div>
                    ${isLong && html`
                        <button class="sa-card-toggle" onClick=${() => { expanded.value = !expanded.value; }}>
                            ${expanded.value ? 'Show less' : 'Show more'}
                        </button>
                    `}
                </div>
            `}
            ${sessionId && html`
                <div class="sa-card-actions">
                    <button class="sa-card-view-btn" onClick=${onViewSession}>
                        View session \u2192
                    </button>
                </div>
            `}
        </div>
    `;
}
