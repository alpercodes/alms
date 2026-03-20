import { html, useSignal } from '../../deps.js';

/** Extract a human-readable one-liner for common tools. */
function toolSummary(tool, params) {
    if (!params) return '';
    switch (tool) {
        case 'shell_exec':
            if (params.argv) return params.argv.join(' ');
            return params.command || '';
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
            return params.expression || '';
        case 'echo':
            return params.text || '';
        default: {
            const entries = Object.entries(params);
            return entries.map(([k, v]) => {
                const val = typeof v === 'string' ? v : JSON.stringify(v);
                return entries.length > 1 ? `${k}=${val}` : val;
            }).join(' ');
        }
    }
}

export function ToolRow({ tool, params, status, result, id, sourceAgent }) {
    const expanded = useSignal(false);

    const toggle = () => { expanded.value = !expanded.value; };

    const isSubagent = !!sourceAgent;
    const isInvokeAgent = tool === 'invoke_agent';
    const rowClass = isInvokeAgent ? 'tool-row subagent-header'
        : isSubagent ? 'tool-row subagent-nested'
        : 'tool-row';

    const summary = toolSummary(tool, params);

    // invoke_agent gets special rendering
    if (isInvokeAgent) {
        const name = params?.name || params?.subagent_name || 'subagent';
        const task = params?.task || '';
        const taskPreview = task.slice(0, 100) + (task.length > 100 ? '\u2026' : '');

        if (status === 'running') {
            return html`
                <div class="${rowClass}" onClick=${toggle}>
                    <span class="subagent-spinner"></span>
                    <span class="subagent-badge">${name}</span> ${taskPreview}
                    ${expanded.value ? html`<pre class="tool-row-detail">${JSON.stringify(params, null, 2)}</pre>` : ''}
                </div>
            `;
        }
        const icon = status === 'done' ? '\u2713' : '\u2717';
        const resultText = result ? (typeof result === 'string' ? result : JSON.stringify(result)).slice(0, 200) : '';
        return html`
            <div class="${rowClass} ${status}" onClick=${toggle}>
                <span>${icon}</span>
                <span class="subagent-badge">${name}</span>
                ${expanded.value
                    ? html`<pre class="tool-row-detail">${resultText}</pre>`
                    : resultText ? html` \u2192 ${resultText.slice(0, 80)}${resultText.length > 80 ? '\u2026' : ''}` : ''
                }
            </div>
        `;
    }

    // Regular tool (or subagent's inner tool)
    const truncSummary = summary.slice(0, 120) + (summary.length > 120 ? '\u2026' : '');

    if (status === 'running') {
        return html`
            <div class="${rowClass}" onClick=${toggle}>
                ${isSubagent ? html`<span class="subagent-tag">[${sourceAgent}]</span>` : ''}
                <span style="color: var(--warning);">$</span>
                <span class="tool-name">${tool}</span>
                <span class="tool-summary">${truncSummary}</span>
                ${expanded.value ? html`<pre class="tool-row-detail">${JSON.stringify(params, null, 2)}</pre>` : ''}
            </div>
        `;
    }

    const icon = status === 'done' ? '\u2713' : '\u2717';
    const resultText = result ? (typeof result === 'string' ? result : JSON.stringify(result)) : '';
    const truncResult = resultText.slice(0, 200) + (resultText.length > 200 ? '\u2026' : '');

    return html`
        <div class="${rowClass} ${status}" onClick=${toggle}>
            ${isSubagent ? html`<span class="subagent-tag">[${sourceAgent}]</span>` : ''}
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
