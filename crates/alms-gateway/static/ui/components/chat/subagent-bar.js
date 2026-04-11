import { html, useSignal } from '../../deps.js';
import { activeSubagents } from '../../state/subagents.js';
import { toolSummary } from '../../utils/tool-summary.js';
import { SUBAGENT_PREVIEW_LEN, TOOL_SUMMARY_LEN } from '../../utils/constants.js';

function SubagentPanel({ name, info, onClose }) {
    const icon = info.status === 'done' ? '\u2713' : info.status === 'fail' ? '\u2717' : null;
    const label = info.displayName || name;

    return html`
        <div class="sa-panel">
            <div class="sa-panel-header">
                <span class="sa-panel-name">${info.status === 'running' ? html`<span class="tc-spinner"></span>` : icon} ${label}</span>
                <span class="sa-panel-task">${info.task.slice(0, SUBAGENT_PREVIEW_LEN)}${info.task.length > SUBAGENT_PREVIEW_LEN ? '\u2026' : ''}</span>
                <button class="sa-panel-close" onClick=${onClose}>\u00d7</button>
            </div>
            <div class="sa-panel-tools">
                ${info.tools.length === 0
                    ? html`<div class="sa-panel-empty">Waiting for activity...</div>`
                    : info.tools.map(t => {
                        const summary = toolSummary(t.tool, t.params);
                        const truncSummary = summary.slice(0, TOOL_SUMMARY_LEN) + (summary.length > TOOL_SUMMARY_LEN ? '\u2026' : '');
                        return html`
                            <div class="sa-tool-row ${t.status || ''}">
                                ${t.status === 'running'
                                    ? html`<span class="tc-spinner"></span>`
                                    : html`<span>${t.status === 'done' ? '\u2713' : '\u2717'}</span>`
                                }
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
                const icon = info.status === 'done' ? '\u2713' : '\u2717';
                const toolCount = info.tools.length;
                const lastTool = info.tools[info.tools.length - 1];
                const activity = lastTool && lastTool.status === 'running'
                    ? lastTool.tool
                    : '';
                const label = info.displayName || name;

                return html`
                    <button class="sa-chip ${isRunning ? 'running' : info.status}"
                            onClick=${() => { expanded.value = expanded.value === name ? null : name; }}>
                        ${isRunning
                            ? html`<span class="tc-spinner"></span>`
                            : html`<span>${icon}</span>`
                        }
                        <span class="sa-chip-name">${label}</span>
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
