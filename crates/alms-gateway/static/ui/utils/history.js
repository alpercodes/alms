import { nextMsgId } from '../state/chat.js';

/**
 * Parse a persisted job notification marker into structured fields.
 *
 * The backend persists job completion markers with the format:
 *   [Scheduled job {label}] {job_name}\n{summary}
 *
 * where label is "completed", "failed", or "finished".
 *
 * When metadata contains `job_status`, that is the authoritative source
 * (one of "success", "error", "cancelled", "unknown").  The text label
 * is only used as a fallback for markers persisted before this field
 * was added.
 *
 * @param {string} content - raw marker text
 * @param {object} [metadata] - message metadata (may contain job_status)
 * @returns {{ jobName: string, status: string, summary: string }}
 */
function parseJobNotification(content, metadata) {
    // Match: [Scheduled job completed] My job prompt\nSummary text
    const match = content.match(/^\[Scheduled job (\w+)\]\s*(.*)/s);
    if (!match) {
        const mdStatus = metadata && metadata.job_status;
        return { jobName: content, status: mdStatus || 'success', summary: '' };
    }
    const label = match[1]; // completed | failed | finished
    const rest = match[2] || '';

    // Prefer the authoritative status from metadata when available.
    const mdStatus = metadata && metadata.job_status;
    const status = mdStatus
        ? mdStatus
        : label === 'failed' ? 'error'
        : label === 'completed' ? 'success'
        : label === 'finished' ? 'cancelled'
        : 'success';

    // Split on first newline: job name vs summary
    const nlIdx = rest.indexOf('\n');
    if (nlIdx >= 0) {
        return {
            jobName: rest.slice(0, nlIdx).trim(),
            status,
            summary: rest.slice(nlIdx + 1).trim(),
        };
    }
    return { jobName: rest.trim(), status, summary: '' };
}

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
    // Also build a secondary index by tool_invocation_id (when present in
    // metadata) so that matching works even if tool_call_id is missing on
    // either side. (Fixes #509)
    const resultMap = new Map();
    const resultByInvocationId = new Map();
    for (const m of msgs) {
        if (m.type === 'tool_result' && m.tool_id) {
            resultMap.set(m.tool_id, m);
        }
        if (m.type === 'tool_result' && m.metadata && m.metadata.tool_invocation_id) {
            resultByInvocationId.set(m.metadata.tool_invocation_id, m);
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

            // DM messages from peer agents are stored as role "user" with
            // metadata.message_type="dm" and metadata.from_agent set.
            // Render them as agent messages (left side) so they are not
            // confused with human-user messages (right side). (#546)
            const isDm = m.role === 'user'
                && m.metadata && m.metadata.message_type === 'dm'
                && m.metadata.from_agent;

            const type = isSynthetic ? 'notification'
                : isDm ? 'agent'
                : (m.role === 'user' ? 'user' : 'agent');

            // Parse job_notification markers into structured fields for
            // the JobCompletionCard component.  The persisted format is:
            //   [Scheduled job {label}] {job_name}\n{summary}
            // where label is "completed", "failed", or "finished".
            if (isSynthetic && m.metadata.type === 'job_notification') {
                const content = m.content || '';
                const parsed = parseJobNotification(content, m.metadata);
                entries.push({
                    id: nextMsgId(),
                    type: 'job_completed',
                    jobName: parsed.jobName,
                    status: parsed.status,
                    summary: parsed.summary,
                    ts: m.timestamp || null,
                    metadata: m.metadata || null,
                });
            } else {
                entries.push({
                    id: nextMsgId(),
                    type,
                    role: m.role,
                    text: m.content || '',
                    metadata: m.metadata || null,
                    sealed: true,
                    // Carry the sender name so Message can show it as the label.
                    fromAgent: isDm ? m.metadata.from_agent : undefined,
                });
            }
        } else if (m.type === 'tool_call') {
            const callId = (m.metadata && m.metadata.tool_call_id) || null;
            // Prefer tool_invocation_id as the message ID so that history-
            // reconstructed entries use the same ID as live SSE tool_start
            // events. This eliminates the fallback matching in tool_end.
            const invocationId = (m.metadata && m.metadata.tool_invocation_id) || null;
            // Match tool_result by LLM tool_call_id (primary), falling back
            // to tool_invocation_id when the primary key is absent. (#509)
            const matched = (callId ? resultMap.get(callId) : null)
                || (invocationId ? resultByInvocationId.get(invocationId) : null);
            entries.push({
                id: invocationId || callId || nextMsgId(),
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
            // DM image messages use the same metadata pattern as text. (#546)
            const isDmImg = m.role === 'user'
                && m.metadata && m.metadata.message_type === 'dm'
                && m.metadata.from_agent;
            entries.push({
                id: nextMsgId(),
                type: 'image',
                role: isDmImg ? 'assistant' : m.role,
                url: m.url || '',
                alt: m.alt || '',
                sealed: true,
                fromAgent: isDmImg ? m.metadata.from_agent : undefined,
            });
        }
        // tool_result entries are consumed via resultMap -- skip them here
    }
    return entries;
}
