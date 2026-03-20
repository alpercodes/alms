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

/**
 * SubagentGroup — wraps an invoke_agent row + its nested subagent tool rows.
 * Click the header to expand/collapse all nested tools.
 * Click individual nested tools to expand their detail.
 */
export function SubagentGroup({ header, children }) {
    const expanded = useSignal(false);
    const toggle = (e) => {
        e.stopPropagation();
        expanded.value = !expanded.value;
    };

    const { params, status, result } = header;
    const name = params?.name || params?.subagent_name || 'subagent';
    const task = params?.task || '';
    const taskPreview = task.slice(0, 100) + (task.length > 100 ? '\u2026' : '');
    const hasChildren = children && children.length > 0;

    if (status === 'running') {
        return html`
            <div class="subagent-group">
                <div class="tool-row subagent-header" onClick=${toggle}>
                    <span class="subagent-spinner"></span>
                    <span class="subagent-badge">${name}</span>
                    ${taskPreview}
                    ${hasChildren ? html`<span class="subagent-count">${children.length}</span>` : ''}
                </div>
                ${expanded.value && children && children.map((c, i) =>
                    html`<${ToolRow} key=${i} ...${c} />`
                )}
            </div>
        `;
    }

    const icon = status === 'done' ? '\u2713' : '\u2717';
    const resultText = result ? (typeof result === 'string' ? result : JSON.stringify(result)).slice(0, 200) : '';

    return html`
        <div class="subagent-group">
            <div class="tool-row subagent-header ${status}" onClick=${toggle}>
                <span>${icon}</span>
                <span class="subagent-badge">${name}</span>
                ${!expanded.value && resultText
                    ? html` \u2192 ${resultText.slice(0, 80)}${resultText.length > 80 ? '\u2026' : ''}`
                    : ''
                }
                ${hasChildren ? html`<span class="subagent-count">${children.length}</span>` : ''}
            </div>
            ${expanded.value && html`
                ${resultText && html`<pre class="tool-row-detail subagent-result">${resultText}</pre>`}
                ${children && children.map((c, i) =>
                    html`<${ToolRow} key=${i} ...${c} />`
                )}
            `}
        </div>
    `;
}

export function ToolRow({ tool, params, status, result, id, sourceAgent }) {
    const expanded = useSignal(false);
    const toggle = (e) => {
        e.stopPropagation();
        expanded.value = !expanded.value;
    };

    const isSubagent = !!sourceAgent;
    const summary = toolSummary(tool, params);
    const truncSummary = summary.slice(0, 120) + (summary.length > 120 ? '\u2026' : '');
    const rowClass = isSubagent ? 'tool-row subagent-nested' : 'tool-row';

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
    const truncResult = resultText.slice(0, 300) + (resultText.length > 300 ? '\u2026' : '');

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
