import { html, useSignal } from '../../deps.js';

export function ToolRow({ tool, params, status, result, id, sourceAgent }) {
    const expanded = useSignal(false);

    const toggle = () => { expanded.value = !expanded.value; };

    const isSubagent = !!sourceAgent;
    // This tool name matches the Rust builtin registered in
    // alms-sandbox/src/builtins/invoke_agent.rs as "invoke_agent".
    // If the tool is ever renamed there, this comparison must be updated.
    const isInvokeAgent = tool === 'invoke_agent';
    const rowClass = isInvokeAgent ? 'tool-row subagent-header'
        : isSubagent ? 'tool-row subagent-nested'
        : 'tool-row';

    const entries = params ? Object.entries(params) : [];
    const paramStr = entries.map(([k, v]) => {
        const val = typeof v === 'string' ? v : JSON.stringify(v);
        return entries.length > 1 ? `${k}=${val}` : val;
    }).join(' ');

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
    if (status === 'running') {
        return html`
            <div class="${rowClass}" onClick=${toggle}>
                ${isSubagent ? html`<span class="subagent-tag">[${sourceAgent}]</span>` : ''}
                <span style="color: var(--warning)">$</span> ${tool} ${paramStr.slice(0, 80)}${paramStr.length > 80 ? '\u2026' : ''}
                ${expanded.value ? html`<pre class="tool-row-detail">${JSON.stringify(params, null, 2)}</pre>` : ''}
            </div>
        `;
    }

    const icon = status === 'done' ? '\u2713' : '\u2717';
    const summary = result ? JSON.stringify(result).slice(0, 200) : '';
    return html`
        <div class="${rowClass} ${status}" onClick=${toggle}>
            ${isSubagent ? html`<span class="subagent-tag">[${sourceAgent}]</span>` : ''}
            <span>${icon}</span> ${tool}${expanded.value
                ? html`<pre class="tool-row-detail">${summary}</pre>`
                : summary ? html` \u2192 ${summary.slice(0, 80)}${summary.length > 80 ? '\u2026' : ''}` : ''
            }
        </div>
    `;
}
