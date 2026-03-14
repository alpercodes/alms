import { html, useSignal } from '../../deps.js';

export function ToolRow({ tool, params, status, result, id }) {
    const expanded = useSignal(false);

    const toggle = () => { expanded.value = !expanded.value; };

    if (status === 'running') {
        return html`
            <div class="tool-row" onClick=${toggle}>
                \u25b6 ${tool}${expanded.value ? html`<pre class="tool-row-detail">${JSON.stringify(params, null, 2)}</pre>` : ''}
            </div>
        `;
    }

    const icon = status === 'done' ? '\u2713' : '\u2717';
    const summary = result ? JSON.stringify(result).slice(0, 160) : '';
    return html`
        <div class="tool-row ${status}" onClick=${toggle}>
            ${icon} ${tool}${expanded.value
                ? html`<pre class="tool-row-detail">${summary}</pre>`
                : html` \u2014 ${summary.slice(0, 60)}${summary.length > 60 ? '\u2026' : ''}`
            }
        </div>
    `;
}
