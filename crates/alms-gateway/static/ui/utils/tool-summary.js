/**
 * Extract a human-readable one-liner for common tools.
 *
 * Shared between tool-row.js (main chat) and subagent-bar.js (subagent panel).
 *
 * @param {string} tool  - Tool name (e.g. 'shell', 'fs_read').
 * @param {object} params - Tool parameters object.
 * @returns {string} A short description of the tool invocation.
 */
export function toolSummary(tool, params) {
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
