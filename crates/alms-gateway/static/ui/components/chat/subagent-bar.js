import { html, useSignal } from '../../deps.js';
import { activeSubagents } from '../../state/subagents.js';

const PREVIEW_LEN = 100;

function toolSummary(tool) {
    if (!tool.params) return '';
    switch (tool.tool) {
        case 'shell':
        case 'shell_exec':
            if (tool.params.command) return tool.params.command;
            return tool.params.argv ? tool.params.argv.join(' ') : '';
        case 'fs_read': return tool.params.path || '';
        case 'fs_write': return tool.params.path || '';
        case 'fs_list': return tool.params.path || '.';
        default: return '';
    }
}

function SubagentPanel({ name, info, onClose }) {
    const icon = info.status === 'running' ? '\u23f3'
        : info.status === 'done' ? '\u2713' : '\u2717';

    return html`
        <div class="sa-panel">
            <div class="sa-panel-header">
                <span class="sa-panel-name">${icon} ${name}</span>
                <span class="sa-panel-task">${info.task.slice(0, PREVIEW_LEN)}${info.task.length > PREVIEW_LEN ? '\u2026' : ''}</span>
                <button class="sa-panel-close" onClick=${onClose}>\u00d7</button>
            </div>
            <div class="sa-panel-tools">
                ${info.tools.length === 0
                    ? html`<div class="sa-panel-empty">Waiting for activity...</div>`
                    : info.tools.map(t => {
                        const statusIcon = t.status === 'running' ? '\u23f3'
                            : t.status === 'done' ? '\u2713' : '\u2717';
                        const summary = toolSummary(t);
                        const truncSummary = summary.slice(0, 80) + (summary.length > 80 ? '\u2026' : '');
                        return html`
                            <div class="sa-tool-row ${t.status || ''}">
                                <span>${statusIcon}</span>
                                <span class="sa-tool-name">${t.tool}</span>
                                <span class="sa-tool-summary">${truncSummary}</span>
                            </div>
                        `;
                    })
                }
            </div>
        </div>
    `;
}

export function SubagentBar() {
    const expanded = useSignal(null); // name of expanded subagent, or null

    const entries = Object.entries(activeSubagents.value);
    if (entries.length === 0) return null;


    return html`
        <div class="sa-bar">
            ${entries.map(([name, info]) => {
                const isRunning = info.status === 'running';
                const icon = isRunning ? '\u23f3' : info.status === 'done' ? '\u2713' : '\u2717';
                const toolCount = info.tools.length;
                const lastTool = info.tools[info.tools.length - 1];
                const activity = lastTool && lastTool.status === 'running'
                    ? lastTool.tool
                    : '';

                return html`
                    <button class="sa-chip ${isRunning ? 'running' : info.status}"
                            onClick=${() => { expanded.value = expanded.value === name ? null : name; }}>
                        <span>${icon}</span>
                        <span class="sa-chip-name">${name}</span>
                        ${activity && html`<span class="sa-chip-activity">${activity}</span>`}
                        ${toolCount > 0 && html`<span class="sa-chip-count">${toolCount}</span>`}
                    </button>
                `;
            })}
            ${expanded.value && activeSubagents.value[expanded.value] && html`
                <${SubagentPanel}
                    name=${expanded.value}
                    info=${activeSubagents.value[expanded.value]}
                    onClose=${() => { expanded.value = null; }} />
            `}
        </div>
    `;
}
