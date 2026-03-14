import { h } from 'https://esm.sh/preact@10.24.3';
import { computed } from 'https://esm.sh/@preact/signals@1.3.0';
import htm from 'https://esm.sh/htm@3.1.1';
import { runs } from '../../state/runs.js';
import { fmtDate } from '../../utils/format.js';

const html = htm.bind(h);

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

export function RunList() {
    return html`
        <div class="sidebar-section" id="run-history-section" style="flex:1">
            <div class="sidebar-label">
                Runs
                <span id="session-tokens">${sessionTokens.value}</span>
            </div>
            <div id="run-list">
                ${runs.value.length === 0
                    ? html`<div class="run-empty">No runs yet</div>`
                    : runs.value.map(run => html`
                        <div class="run-item ${run.status}">
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
