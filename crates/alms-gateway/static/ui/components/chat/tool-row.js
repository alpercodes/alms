import { html, useSignal } from '../../deps.js';

export function ToolRow({ tool, params, status, result, id }) {
    const expanded = useSignal(false);

    const toggle = () => { expanded.value = !expanded.value; };

    const paramStr = params ? Object.entries(params).map(([k, v]) =>
        typeof v === 'string' ? v : JSON.stringify(v)
    ).join(' ') : '';

    if (status === 'running') {
        return html`
            <div class="tool-row" onClick=${toggle}>
                <span style="color: var(--warning)">$</span> ${tool} ${paramStr.slice(0, 80)}${paramStr.length > 80 ? '\u2026' : ''}
                ${expanded.value ? html`<pre class="tool-row-detail">${JSON.stringify(params, null, 2)}</pre>` : ''}
            </div>
        `;
    }

    const icon = status === 'done' ? '\u2713' : '\u2717';
    const summary = result ? JSON.stringify(result).slice(0, 200) : '';
    return html`
        <div class="tool-row ${status}" onClick=${toggle}>
            <span>${icon}</span> ${tool}${expanded.value
                ? html`<pre class="tool-row-detail">${summary}</pre>`
                : summary ? html` \u2192 ${summary.slice(0, 80)}${summary.length > 80 ? '\u2026' : ''}` : ''
            }
        </div>
    `;
}
