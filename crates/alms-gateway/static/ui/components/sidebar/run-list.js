import { html, computed } from '../../deps.js';
import { runs, activeRunId } from '../../state/runs.js';
import { fmtDate } from '../../utils/format.js';

const STATUS_ICONS = {
    completed: '\u2713',  // ✓
    failed:    '\u2717',  // ✗
    cancelled: '\u2298',  // ⊘
    running:   '\u22EF',  // ⋯
};

const sessionTokens = computed(() => {
    let prompt = 0, completion = 0;
    for (const r of runs.value) {
        if (r.usage) {
            prompt += r.usage.prompt_tokens || 0;
            completion += r.usage.completion_tokens || 0;
        }
    }
    return (prompt + completion > 0) ? `${prompt}p/${completion}c` : '';
});

function selectRun(runId) {
    activeRunId.value = activeRunId.value === runId ? null : runId;
    // Scroll to the first message from this run if visible in the chat area
    const el = document.querySelector(`[data-run-id="${runId}"]`);
    if (el) el.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
}

export function RunList() {
    return html`
        <div class="sidebar-section" id="run-history-section" style="flex:1">
            <div class="sidebar-label">
                Runs
                <span id="session-tokens">${sessionTokens.value}</span>
            </div>
            <div id="run-list" role="list" aria-label="Runs">
                ${runs.value.length === 0
                    ? html`<div class="run-empty">No runs yet</div>`
                    : runs.value.map(run => html`
                        <div class="run-item ${run.status} ${run.run_id === activeRunId.value ? 'selected' : ''}"
                             role="listitem"
                             tabindex="0"
                             onClick=${() => selectRun(run.run_id)}
                             onKeyDown=${(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); selectRun(run.run_id); } }}>
                            <span class="run-status">${STATUS_ICONS[run.status] || '\u00B7'}</span>
                            <span class="run-meta" title=${run.run_id}>
                                ${fmtDate(run.ts || run.started_at || '')}
                            </span>
                            <span class="run-tokens">
                                ${run.usage ? `${run.usage.prompt_tokens}+${run.usage.completion_tokens}` : ''}
                            </span>
                        </div>
                    `)
                }
            </div>
        </div>
    `;
}
