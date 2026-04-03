import { html, useSignal } from '../../deps.js';

/** Extract a human-readable one-liner for common tools. */
function toolSummary(tool, params) {
    if (!params) return '';
    switch (tool) {
        case 'shell':
        case 'shell_exec':
            if (params.command) return params.command;
            if (params.argv) return params.argv.join(' ');
            return '';
        case 'fs_read':
            return params.path || '';
        case 'fs_write': {
            const mode = params.mode === 'append' ? '(append) ' : '';
            return `${mode}${params.path || ''}`;
        }
        case 'fs_list':
            return params.path || '.';
        case 'workspace_write':
            return `${params.file || ''}: ${(params.content || '').slice(0, 60)}`;
        case 'http_get':
            return params.url || '';
        case 'math':
            return params.operation ? params.operation + '(' + [params.a, params.b, params.n].filter(v => v !== undefined).join(', ') + ')' : '';
        case 'echo':
            return params.message || params.text || '';
        default: {
            const entries = Object.entries(params);
            return entries.map(([k, v]) => {
                const val = typeof v === 'string' ? v : JSON.stringify(v);
                return entries.length > 1 ? `${k}=${val}` : val;
            }).join(' ');
        }
    }
}

const PREVIEW_LEN = 120;
const DETAIL_LEN = 500;


export function ToolRow({ tool, params, status, result, id, sourceAgent }) {
    const expanded = useSignal(false);
    const toggle = (e) => {
        e.stopPropagation();
        expanded.value = !expanded.value;
    };

    const summary = toolSummary(tool, params);
    const truncSummary = summary.slice(0, PREVIEW_LEN) + (summary.length > PREVIEW_LEN ? '\u2026' : '');
    const rowClass = 'tool-row';

    if (status === 'running') {
        return html`
            <div class="${rowClass}" onClick=${toggle}>
                <span style="color: var(--warning);">$</span>
                <span class="tool-name">${tool}</span>
                <span class="tool-summary">${truncSummary}</span>
                ${expanded.value ? html`<pre class="tool-row-detail">${JSON.stringify(params, null, 2)}</pre>` : ''}
            </div>
        `;
    }

    const icon = status === 'done' ? '\u2713' : '\u2717';
    const resultText = result ? (typeof result === 'string' ? result : JSON.stringify(result)) : '';
    const truncResult = resultText.slice(0, DETAIL_LEN) + (resultText.length > DETAIL_LEN ? '\u2026' : '');

    return html`
        <div class="${rowClass} ${status}" onClick=${toggle}>
            <span>${icon}</span>
            <span class="tool-name">${tool}</span>
            <span class="tool-summary">${truncSummary}</span>
            ${expanded.value
                ? html`<pre class="tool-row-detail">${truncResult}</pre>`
                : ''
            }
        </div>
    `;
}
