/**
 * Map API message history to chatMessages entries.
 * Pairs consecutive tool_call + tool_result into single tool entries.
 */
export function mapHistoryMessages(msgs) {
    const entries = [];
    for (let i = 0; i < msgs.length; i++) {
        const m = msgs[i];
        if (m.type === 'text' || !m.type) {
            // Legacy messages without type field, or explicit text
            entries.push({
                type: m.role === 'user' ? 'user' : 'agent',
                role: m.role,
                text: m.content || '',
                sealed: true,
            });
        } else if (m.type === 'tool_call') {
            // Look ahead for matching tool_result
            const next = msgs[i + 1];
            const hasResult = next && next.type === 'tool_result';
            entries.push({
                type: 'tool',
                tool: m.tool,
                params: m.params,
                status: hasResult ? (next.ok ? 'done' : 'fail') : 'done',
                result: hasResult ? next.result : null,
                id: (m.metadata && m.metadata.tool_call_id) || m.tool,
            });
            if (hasResult) i++; // skip the paired tool_result
        }
        // Standalone tool_result without preceding tool_call — skip
    }
    return entries;
}
