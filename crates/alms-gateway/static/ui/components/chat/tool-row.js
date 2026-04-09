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
        case 'send_message':
            return params.to ? `to ${params.to}` : '';
        case 'invoke_agent':
            return params.name || params.subagent_name || '';
        case 'read_session':
        case 'read_subagent_session':
            return params.session_id ? params.session_id.slice(0, 8) + '...' : '';
        case 'list_agents':
        case 'list_my_sessions':
            return '';
        case 'read_messages':
            return params.from ? `from ${params.from}` : '';
        case 'ignore_message':
            return params.from ? `from ${params.from}` : '';
        default: {
            const entries = Object.entries(params);
            return entries.map(([k, v]) => {
                const val = typeof v === 'string' ? v : JSON.stringify(v);
                return entries.length > 1 ? `${k}=${val}` : val;
            }).join(' ');
        }
    }
}

const SUMMARY_LEN = 80;

/** Format a duration in milliseconds to a human-readable string. */
function fmtDuration(ms) {
    if (ms == null) return '';
    if (ms < 1000) return ms + 'ms';
    if (ms < 60000) return (ms / 1000).toFixed(1) + 's';
    const mins = Math.floor(ms / 60000);
    const secs = Math.round((ms % 60000) / 1000);
    return mins + 'm ' + secs + 's';
}

/** Format JSON for display in the detail panel. */
function formatJson(val) {
    if (val == null) return '';
    if (typeof val === 'string') {
        // Try to parse as JSON for pretty display
        try {
            const parsed = JSON.parse(val);
            return JSON.stringify(parsed, null, 2);
        } catch {
            return val;
        }
    }
    return JSON.stringify(val, null, 2);
}

/** Determine the tool icon based on tool name. */
function toolIcon(tool) {
    switch (tool) {
        case 'shell':
        case 'shell_exec':
            return '$';
        case 'fs_read':
            return 'R';
        case 'fs_write':
            return 'W';
        case 'fs_list':
            return 'L';
        case 'workspace_write':
            return 'W';
        case 'http_get':
            return 'H';
        case 'send_message':
            return 'DM';
        case 'invoke_agent':
            return 'IA';
        case 'read_session':
        case 'read_subagent_session':
            return 'RS';
        case 'list_agents':
            return 'LA';
        case 'list_my_sessions':
            return 'LS';
        case 'read_messages':
            return 'RM';
        case 'ignore_message':
            return 'IG';
        case 'math':
            return '#';
        case 'echo':
            return 'E';
        default:
            return 'T';
    }
}


export function ToolRow({ tool, params, status, result, id, sourceAgent, durationMs }) {
    const expanded = useSignal(false);
    const toggle = (e) => {
        e.stopPropagation();
        expanded.value = !expanded.value;
    };

    const summary = toolSummary(tool, params);
    const truncSummary = summary.length > SUMMARY_LEN
        ? summary.slice(0, SUMMARY_LEN) + '\u2026'
        : summary;

    const isRunning = status === 'running';
    const isFail = status === 'fail';
    const isDone = status === 'done';
    const isDm = tool === 'send_message';

    const statusCls = isFail ? 'tc-fail' : isDone ? 'tc-done' : 'tc-running';

    const chevron = expanded.value ? '\u25BC' : '\u25B6';
    const icon = toolIcon(tool);
    const duration = fmtDuration(durationMs);

    const resultText = formatJson(result);
    const paramsText = formatJson(params);

    return html`
        <div class="tc-row ${statusCls} ${isDm ? 'tc-dm' : ''}" role="button" tabindex="0"
             onClick=${toggle} onKeyDown=${(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggle(e); } }}>
            <div class="tc-header">
                <span class="tc-chevron">${chevron}</span>
                ${isRunning
                    ? html`<span class="tc-spinner"></span>`
                    : html`<span class="tc-icon">${icon}</span>`
                }
                <span class="tc-name">${tool}</span>
                ${truncSummary && html`<span class="tc-summary">${truncSummary}</span>`}
                <span class="tc-spacer"></span>
                ${duration && html`<span class="tc-duration">${duration}</span>`}
                ${isFail && html`<span class="tc-status-badge tc-badge-fail">failed</span>`}
                ${isDone && html`<span class="tc-status-icon">\u2713</span>`}
            </div>
            ${expanded.value && html`
                <div class="tc-detail" onClick=${(e) => e.stopPropagation()}>
                    ${paramsText && html`
                        <div class="tc-detail-section">
                            <div class="tc-detail-label">Parameters</div>
                            <pre class="tc-detail-content">${paramsText}</pre>
                        </div>
                    `}
                    ${resultText && html`
                        <div class="tc-detail-section">
                            <div class="tc-detail-label">${isFail ? 'Error' : 'Result'}</div>
                            <pre class="tc-detail-content ${isFail ? 'tc-detail-error' : ''}">${resultText}</pre>
                        </div>
                    `}
                </div>
            `}
        </div>
    `;
}


/**
 * ToolGroup wraps consecutive tool calls that were executed in parallel
 * (i.e. appear adjacent in the message list with no text between them).
 * Renders them inside a shared container with a subtle visual grouping.
 */
export function ToolGroup({ children, count }) {
    if (count <= 1) return children;
    return html`
        <div class="tc-group">
            <div class="tc-group-label">${count} tools in parallel</div>
            ${children}
        </div>
    `;
}
