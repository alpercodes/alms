/**
 * Map API message history to chatMessages entries.
 *
 * Pairs tool_call messages with their matching tool_result by
 * tool_call_id / tool_id rather than positional lookahead. This
 * correctly handles parallel tool calls where the stored order is
 * [call_A, call_B, result_A, result_B].
 */
export function mapHistoryMessages(msgs) {
    // First pass: index all tool_result messages by their tool_id so we
    // can match them to tool_call messages regardless of position.
    const resultMap = new Map();
    for (const m of msgs) {
        if (m.type === 'tool_result' && m.tool_id) {
            resultMap.set(m.tool_id, m);
        }
    }

    // Second pass: build the chat entries. Tool results are consumed via
    // the map lookup — any that remain unmatched are skipped (same as the
    // old "standalone tool_result" behavior).
    const entries = [];
    for (const m of msgs) {
        if (m.type === 'text' || !m.type) {
            // Legacy messages without type field, or explicit text
            entries.push({
                type: m.role === 'user' ? 'user' : 'agent',
                role: m.role,
                text: m.content || '',
                sealed: true,
            });
        } else if (m.type === 'tool_call') {
            const callId = (m.metadata && m.metadata.tool_call_id) || null;
            const matched = callId ? resultMap.get(callId) : null;
            entries.push({
                type: 'tool',
                tool: m.tool,
                params: m.params,
                status: matched ? (matched.ok ? 'done' : 'fail') : 'done',
                result: matched ? matched.result : null,
                id: callId || m.tool,
            });
        }
        // tool_result entries are consumed via resultMap — skip them here
    }
    return entries;
}
