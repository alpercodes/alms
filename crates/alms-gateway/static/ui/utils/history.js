import { nextMsgId } from '../state/chat.js';

/**
 * Map API message history to chatMessages entries.
 *
 * Pairs tool_call messages with their matching tool_result by
 * tool_call_id / tool_id rather than positional lookahead. This
 * correctly handles parallel tool calls where the stored order is
 * [call_A, call_B, result_A, result_B].
 *
 * Every entry receives a stable `id` via nextMsgId() so that Preact's
 * VDOM reconciler can correctly match DOM nodes across re-renders.
 *
 * @param {Array} msgs - messages from the session history API
 * @param {object} [opts]
 * @param {boolean} [opts.hasActiveRun] - true if a run is currently
 *   queued or running on this session. Used to mark unmatched tool_calls
 *   as 'running' so the UI shows a spinner instead of a checkmark.
 */
export function mapHistoryMessages(msgs, opts) {
    const hasActiveRun = opts && opts.hasActiveRun;
    // First pass: index all tool_result messages by their tool_id so we
    // can match them to tool_call messages regardless of position.
    const resultMap = new Map();
    for (const m of msgs) {
        if (m.type === 'tool_result' && m.tool_id) {
            resultMap.set(m.tool_id, m);
        }
    }

    // Second pass: build the chat entries. Tool results are consumed via
    // the map lookup -- any that remain unmatched are skipped (same as the
    // old "standalone tool_result" behavior).
    const entries = [];
    for (const m of msgs) {
        if (m.type === 'text' || !m.type) {
            // Synthetic system markers (job notifications, DM-ended markers)
            // are returned with role "system" and metadata.synthetic=true.
            // Render them as notification entries so the UI can style them
            // differently from agent/user messages.
            const isSynthetic = m.role === 'system'
                && m.metadata && m.metadata.synthetic;
            entries.push({
                id: nextMsgId(),
                type: isSynthetic ? 'notification' : (m.role === 'user' ? 'user' : 'agent'),
                role: m.role,
                text: m.content || '',
                metadata: m.metadata || null,
                sealed: true,
            });
        } else if (m.type === 'tool_call') {
            const callId = (m.metadata && m.metadata.tool_call_id) || null;
            const matched = callId ? resultMap.get(callId) : null;
            entries.push({
                id: callId || nextMsgId(),
                type: 'tool',
                tool: m.tool,
                params: m.params,
                // When no matching tool_result exists: if a run is still
                // active on this session, the tool is likely in-progress
                // so show it as 'running' (the tool_end SSE event will
                // update it). Otherwise default to 'done' (the result was
                // persisted elsewhere or the run completed before reload).
                status: matched ? (matched.ok ? 'done' : 'fail')
                    : (hasActiveRun ? 'running' : 'done'),
                result: matched ? matched.result : null,
            });
        } else if (m.type === 'image') {
            entries.push({
                id: nextMsgId(),
                type: 'image',
                role: m.role,
                url: m.url || '',
                alt: m.alt || '',
                sealed: true,
            });
        }
        // tool_result entries are consumed via resultMap -- skip them here
    }
    return entries;
}
