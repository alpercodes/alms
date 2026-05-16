import { nextMsgId } from '../state/chat.js';
import { DM_END_REASON_LABELS } from './constants.js';

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
 * Build an index of session-level tool call records so that history
 * messages of type "tool_call" can be enriched with params/result
 * even when the session_messages table only has bare content blocks.
 *
 * DM sessions store tool calls exclusively in the per-run
 * `run_tool_calls` table, so without this merge the tool rows would
 * appear empty after a page reload.
 *
 * The index is keyed by tool_id (the provider-assigned tool_call_id
 * that correlates assistant tool_call messages with their results).
 *
 * Each key maps to { call, result } where call is the assistant-role
 * record and result is the tool-role record.
 *
 * @param {Array} toolCalls - records from GET /sessions/{id}/tool-calls
 * @returns {Map<string, { call: object|null, result: object|null }>}
 */
function buildToolCallIndex(toolCalls) {
    const index = new Map();
    for (const tc of toolCalls) {
        const key = tc.tool_id;
        if (!key) continue;
        const entry = index.get(key) || { call: null, result: null, runId: null };
        if (tc.role === 'assistant' || tc.role === 'Assistant') {
            entry.call = tc;
            // Preserve run_id from the tool call record for reasoning
            // block grouping in DM sessions.
            if (tc.run_id) entry.runId = tc.run_id;
        } else if (tc.role === 'tool' || tc.role === 'Tool') {
            entry.result = tc;
            if (tc.run_id && !entry.runId) entry.runId = tc.run_id;
        }
        index.set(key, entry);
    }
    return index;
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
 * @param {Array} [opts.sessionToolCalls] - tool call records from
 *   GET /sessions/{id}/tool-calls, used to enrich tool rows that
 *   lack params/result in the session message history (DM sessions).
 */
export function mapHistoryMessages(msgs, opts) {
    const hasActiveRun = opts && opts.hasActiveRun;
    const isDm = opts && opts.isDm;
    const sessionToolCalls = (opts && opts.sessionToolCalls) || [];
    const toolCallIndex = sessionToolCalls.length > 0
        ? buildToolCallIndex(sessionToolCalls)
        : new Map();

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
    // entryTimestamps tracks the ISO timestamp parallel to each entry so
    // that reconstructed tool calls from session-level records can be
    // interleaved chronologically instead of appended to the end.
    const entries = [];
    const entryTimestamps = [];
    /** Push an entry and its corresponding source timestamp. */
    const pushEntry = (entry, ts) => {
        entries.push(entry);
        entryTimestamps.push(ts || null);
    };
    for (const m of msgs) {
        if (m.type === 'text' || !m.type) {
            // DM-ended markers are persisted by MessageBus::end_conversation
            // on the DM session with role "user", empty content, and
            // metadata.message_type === "dm_ended".  Map them to the
            // 'dm_ended' chat message type so DmConversationView renders
            // the correct divider banner after a page reload.
            const isDmEndedMarker = m.metadata
                && m.metadata.message_type === 'dm_ended';
            if (isDmEndedMarker) {
                const rawReason = m.metadata.reason || '';
                pushEntry({
                    id: nextMsgId(),
                    type: 'dm_ended',
                    peer: m.metadata.ended_by || 'unknown',
                    reason: DM_END_REASON_LABELS[rawReason] || rawReason || 'conversation ended',
                }, m.timestamp);
                continue;
            }

            // Reasoning text persisted from DM sessions (#685).
            // These are the agent's internal thinking text, stored as
            // Role::User with message_type="reasoning". Map them to a
            // special type that groupDmReasoningBlocks() can collect.
            //
            // For reasoning-capable models (Anthropic thinking_budget_tokens,
            // OpenAI reasoning_effort, Gemini thinking_budget — see #767/
            // #768/#769) the runtime persists the visible assistant text in
            // `content` and the extended-thinking trace in
            // `metadata.reasoning_blocks` (see `merge_reasoning_blocks` in
            // loop_impl.rs). Prefer the trace when present so the reload
            // render matches the live thinking pane (#897). Fall back to
            // `m.content` for non-reasoning models that have no
            // `reasoning_blocks` metadata. Fixes #898.
            const isReasoningText = m.metadata
                && m.metadata.message_type === 'reasoning';
            if (isReasoningText) {
                let text = '';
                if (Array.isArray(m.metadata.reasoning_blocks)) {
                    text = m.metadata.reasoning_blocks
                        .map(b => (b && typeof b.text === 'string') ? b.text : '')
                        .join('');
                }
                if (!text) text = m.content || '';
                pushEntry({
                    id: nextMsgId(),
                    type: 'dm_reasoning_text',
                    text,
                    fromAgent: m.metadata.from_agent || null,
                    runId: m.metadata.run_id || null,
                }, m.timestamp);
                continue;
            }

            // Synthetic system markers (job notifications, DM-ended markers,
            // run boundaries, run warnings, subagent completions) are
            // returned with role "system" and metadata.synthetic=true.
            // Route them to the correct chat message types.
            const isSynthetic = m.role === 'system'
                && m.metadata && m.metadata.synthetic;

            // DM messages from peer agents are stored as role "user" with
            // metadata.message_type="dm" and metadata.from_agent set.
            // Render them as agent messages (left side) so they are not
            // confused with human-user messages (right side). (#546)
            const isDm = m.role === 'user'
                && m.metadata && m.metadata.message_type === 'dm'
                && m.metadata.from_agent;

            // --- Synthetic marker routing ---

            // Parse job_notification markers into structured fields for
            // the JobCompletionCard component.
            if (isSynthetic && m.metadata.type === 'job_notification') {
                const content = m.content || '';
                const parsed = parseJobNotification(content, m.metadata);
                pushEntry({
                    id: nextMsgId(),
                    type: 'job_completed',
                    jobName: parsed.jobName,
                    status: parsed.status,
                    summary: parsed.summary,
                    ts: m.timestamp || null,
                    metadata: m.metadata || null,
                }, m.timestamp);
                continue;
            }

            // Run boundary markers -> run_boundary dividers between runs.
            // Failed/cancelled boundaries also carry `metadata.kind == "error"`
            // (issue #874) so the runtime backfills them into the LLM
            // context, but the UI still renders them as run_boundary
            // dividers (red border + label) — that visual treatment
            // already conveys "this run failed" without a separate
            // ErrorMessage block.
            if (isSynthetic && m.metadata.type === 'run_boundary') {
                const runStatus = m.metadata.status || 'completed';
                pushEntry({
                    id: nextMsgId(),
                    type: 'run_boundary',
                    status: runStatus,
                    runId: m.metadata.run_id || null,
                    error: m.metadata.error || null,
                    text: m.content || '',
                }, m.timestamp);
                continue;
            }

            // Error markers that don't have a paired run_boundary —
            // currently `runtime_init_error` for runs whose runtime failed
            // to construct before the agent loop started (#874). Render
            // them with the existing ErrorMessage component (red border,
            // X icon) so they match the SSE-time error toast styling.
            if (isSynthetic && m.metadata.kind === 'error') {
                pushEntry({
                    id: nextMsgId(),
                    type: 'error',
                    text: m.metadata.error
                        ? `${m.content}\n\n${m.metadata.error}`.trim()
                        : (m.content || 'Run error'),
                    code: m.metadata.error_kind || m.metadata.type || null,
                }, m.timestamp);
                continue;
            }

            // Run warning markers -> warning messages.
            if (isSynthetic && m.metadata.type === 'run_warning') {
                // Suppress subagent warnings in the parent chat (same as
                // the SSE handler in use-session-stream.js).
                if (m.metadata.source_agent) continue;
                pushEntry({
                    id: nextMsgId(),
                    type: 'warning',
                    code: m.metadata.code || 'UNKNOWN',
                    text: m.content || 'Warning',
                }, m.timestamp);
                continue;
            }

            // Subagent completion markers -> subagent_completed cards.
            // Rich metadata fields (task_description, tool_count,
            // duration_ms, summary, token_usage) are persisted by the
            // backend since PR #640.  Read them so the card renders the
            // same detail after a page reload as it does on the live SSE
            // event.
            if (isSynthetic && m.metadata.type === 'subagent_completion') {
                const md = m.metadata;
                pushEntry({
                    id: nextMsgId(),
                    type: 'subagent_completed',
                    name: md.subagent_name || 'subagent',
                    task: md.task_description || '',
                    status: md.status || 'done',
                    toolCount: md.tool_count || 0,
                    durationMs: md.duration_ms != null ? md.duration_ms : null,
                    sessionId: md.session_id || null,
                    summary: md.summary || '',
                }, m.timestamp);
                continue;
            }

            // DM-ended notification markers (persisted on the webchat
            // session, not the DM session itself).
            if (isSynthetic && m.metadata.type === 'dm_ended_notification') {
                pushEntry({
                    id: nextMsgId(),
                    type: 'notification',
                    role: 'system',
                    text: m.content || '',
                    metadata: m.metadata,
                    sealed: true,
                }, m.timestamp);
                continue;
            }

            // Fallback for other synthetic markers.
            const type = isSynthetic ? 'notification'
                : isDm ? 'agent'
                : (m.role === 'user' ? 'user' : 'agent');

            // Reconstruct persisted extended-thinking / reasoning blocks
            // (issue #767). Stored on the assistant-text message as
            // `metadata.reasoning_blocks = [{text: "..."}]` by the runtime;
            // the Message component consumes a single `reasoning` string.
            let reasoning = undefined;
            if (type === 'agent' && m.metadata && Array.isArray(m.metadata.reasoning_blocks)) {
                reasoning = m.metadata.reasoning_blocks
                    .map(b => (b && typeof b.text === 'string') ? b.text : '')
                    .join('');
                if (!reasoning) reasoning = undefined;
            }

            pushEntry({
                id: nextMsgId(),
                type,
                role: m.role,
                text: m.content || '',
                metadata: m.metadata || null,
                sealed: true,
                reasoning,
                // Carry the sender name so Message can show it as the label.
                fromAgent: isDm ? m.metadata.from_agent : undefined,
                // Per-message timestamp (#855) — passed through to the
                // Message component so each chat row can render a discreet
                // grey time stamp with the full ISO available on hover.
                ts: m.timestamp || null,
            }, m.timestamp);
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

            // Extract run_id from reasoning metadata for DM block grouping.
            const runId = (m.metadata && m.metadata.run_id) || null;
            // If the tool call has reasoning metadata, it belongs to a
            // DM reasoning block (persisted by the runtime for DM sessions).
            const isReasoning = m.metadata
                && m.metadata.message_type === 'reasoning';

            // Enrich from session-level tool call records when the message
            // history lacks params or the matched result is empty.  This is
            // critical for DM sessions where tool calls live only in
            // run_tool_calls.
            let toolName = m.tool;
            let params = m.params;
            let result = matched ? matched.result : null;
            let resultOk = matched ? matched.ok : null;
            let enrichedRunId = runId;

            if (callId && toolCallIndex.has(callId)) {
                const enriched = toolCallIndex.get(callId);
                if (enriched.call) {
                    if (!toolName) toolName = enriched.call.tool_name || toolName;
                    if (!params && enriched.call.params) {
                        try { params = JSON.parse(enriched.call.params); }
                        catch { params = null; }
                    }
                }
                if (enriched.result && result == null) {
                    try { result = JSON.parse(enriched.result.result); }
                    catch { result = enriched.result.result; }
                    // If we got a result from the tool call index, it completed.
                    if (resultOk == null) resultOk = true;
                }
                // Propagate run_id from the session tool call index.
                if (!enrichedRunId && enriched.runId) enrichedRunId = enriched.runId;
            }

            // Propagate from_agent from reasoning metadata so that
            // groupDmReasoningBlocks() can attribute tool-only groups
            // to the correct agent.  Fixes #692 — tool entries lacked
            // fromAgent, so groups with only tools had agentName: null.
            //
            // Fall back to the run_tool_calls record's from_agent column
            // when session-level metadata is missing (fixes #696 — the
            // fire-and-forget session-persistence path may drop metadata).
            let fromAgent = isReasoning && m.metadata && m.metadata.from_agent
                ? m.metadata.from_agent : undefined;
            if (!fromAgent && callId && toolCallIndex.has(callId)) {
                const enriched = toolCallIndex.get(callId);
                fromAgent = (enriched.call && enriched.call.from_agent)
                    || (enriched.result && enriched.result.from_agent)
                    || undefined;
            }

            pushEntry({
                id: invocationId || callId || nextMsgId(),
                type: 'tool',
                tool: toolName,
                params: params,
                // When no matching tool_result exists: if a run is still
                // active on this session, the tool is likely in-progress
                // so show it as 'running' (the tool_end SSE event will
                // update it). Otherwise default to 'done' (the result was
                // persisted elsewhere or the run completed before reload).
                status: resultOk != null ? (resultOk ? 'done' : 'fail')
                    : (hasActiveRun ? 'running' : 'done'),
                result: result,
                runId: enrichedRunId || undefined,
                isReasoning: isReasoning || undefined,
                fromAgent,
                // Per-message timestamp (#855) for tool rows.
                ts: m.timestamp || null,
            }, m.timestamp);
        } else if (m.type === 'image') {
            // DM image messages use the same metadata pattern as text. (#546)
            const isDmImg = m.role === 'user'
                && m.metadata && m.metadata.message_type === 'dm'
                && m.metadata.from_agent;
            pushEntry({
                id: nextMsgId(),
                type: 'image',
                role: isDmImg ? 'assistant' : m.role,
                url: m.url || '',
                alt: m.alt || '',
                sealed: true,
                fromAgent: isDmImg ? m.metadata.from_agent : undefined,
                // Per-message timestamp (#855) for image rows.
                ts: m.timestamp || null,
            }, m.timestamp);
        }
        // tool_result entries are consumed via resultMap -- skip them here
    }

    // --- Merge tool calls from session-level endpoint ---
    // For DM sessions, tool calls may not appear in session_messages at all
    // (they are only in run_tool_calls).  Reconstruct them from the
    // session-level tool call records if they were not already covered by
    // the message history.
    if (sessionToolCalls.length > 0) {
        const existingToolIds = new Set();
        for (const e of entries) {
            if (e.type === 'tool' && e.id) existingToolIds.add(e.id);
        }
        // Also track tool_ids that appeared as history messages.
        for (const m of msgs) {
            if (m.type === 'tool_call') {
                const callId = m.metadata && m.metadata.tool_call_id;
                if (callId) existingToolIds.add(callId);
                const invId = m.metadata && m.metadata.tool_invocation_id;
                if (invId) existingToolIds.add(invId);
            }
        }

        // Group session tool calls by tool_id, pairing calls with results.
        // Collect new entries with their timestamps so we can interleave
        // them chronologically with the existing entries instead of
        // appending them at the end.
        const newToolEntries = [];
        for (const [toolId, pair] of toolCallIndex) {
            if (existingToolIds.has(toolId)) continue;
            if (!pair.call) continue; // skip orphan results

            let params = null;
            if (pair.call.params) {
                try { params = JSON.parse(pair.call.params); }
                catch { /* leave null */ }
            }
            let result = null;
            let ok = null;
            if (pair.result && pair.result.result) {
                try { result = JSON.parse(pair.result.result); }
                catch { result = pair.result.result; }
                ok = true;
            }

            // Propagate from_agent from the run_tool_calls record so that
            // groupDmReasoningBlocks() can attribute reasoning blocks to
            // the correct agent even when session-level message persistence
            // was lost (fire-and-forget write failure). Fixes #696.
            const fromAgent = pair.call.from_agent
                || (pair.result && pair.result.from_agent)
                || undefined;

            newToolEntries.push({
                entry: {
                    id: toolId || nextMsgId(),
                    type: 'tool',
                    tool: pair.call.tool_name || 'unknown',
                    params: params,
                    status: ok != null ? (ok ? 'done' : 'fail')
                        : (hasActiveRun ? 'running' : 'done'),
                    result: result,
                    runId: pair.runId || undefined,
                    // For DM sessions: mark merged tool entries as reasoning
                    // so groupDmReasoningBlocks() can collect them even when
                    // session history persistence failed (fire-and-forget).
                    // Fixes #687 -- tool calls missing from reasoning blocks
                    // after reload when session-level persistence was lost.
                    isReasoning: isDm || undefined,
                    fromAgent,
                    // Per-message timestamp (#855).
                    ts: pair.call.timestamp || null,
                },
                ts: pair.call.timestamp || null,
            });
        }

        // Interleave new tool entries into the existing array by timestamp.
        // Both the existing entries (via entryTimestamps) and the new tool
        // entries are roughly chronological, so we walk forward through
        // the existing array once for each new entry (pointer never rewinds).
        if (newToolEntries.length > 0) {
            // Sort new entries by timestamp so we can merge in one pass.
            newToolEntries.sort((a, b) => {
                if (!a.ts && !b.ts) return 0;
                if (!a.ts) return 1;
                if (!b.ts) return -1;
                return a.ts < b.ts ? -1 : a.ts > b.ts ? 1 : 0;
            });
            let insertOffset = 0;
            for (const { entry, ts } of newToolEntries) {
                if (!ts) {
                    // No timestamp — append at the end.
                    entries.push(entry);
                    entryTimestamps.push(null);
                    continue;
                }
                // Find the first existing entry whose timestamp is after
                // this tool call's timestamp.
                let pos = entries.length;
                for (let i = insertOffset; i < entryTimestamps.length; i++) {
                    if (entryTimestamps[i] && entryTimestamps[i] > ts) {
                        pos = i;
                        break;
                    }
                }
                entries.splice(pos, 0, entry);
                entryTimestamps.splice(pos, 0, ts);
                // Advance the offset past the just-inserted position so the
                // next new entry starts scanning from after it.
                insertOffset = pos + 1;
            }
        }
    }

    return entries;
}

/**
 * Group reasoning-related entries by runId into collapsible dm_reasoning
 * blocks for DM sessions.
 *
 * Scans the flat entry list produced by mapHistoryMessages and collects
 * `dm_reasoning_text` entries and `tool` entries that share the same
 * `runId`. Each group is replaced with a single `dm_reasoning` block entry
 * at the position of the earliest member. Non-reasoning entries pass
 * through unchanged.
 *
 * Empty blocks (no tools, no thinking text) are omitted.
 *
 * ## Canonical DM-attribution surfaces (#1076)
 *
 * The two places that decide "is this content the agent's own vs. a
 * peer's" — and where they sit in the pipeline — are:
 *
 * 1. **Backend, context-build time**:
 *    `crates/alms-runtime/src/context.rs` — `apply_perspective()` flips
 *    `Role::User` <-> `Role::Assistant` based on `from_agent` metadata,
 *    and the `is_reasoning_message` filter above it strips ALL
 *    `message_type: "reasoning"` rows so peer reasoning never enters
 *    the LLM context. If you ever see peer chain-of-thought leaking
 *    into a model prompt, this is the surface to audit.
 *
 * 2. **Frontend, DM-session render**:
 *    - `groupDmReasoningBlocks` (this function) — buckets the flat
 *      history entry list by `runId` into `dm_reasoning` blocks. Live
 *      streaming has a parallel surface in
 *      `hooks/use-session-stream.js::on('tool_start')` which appends
 *      tool entries directly into the live block.
 *    - `components/chat/dm-conversation-view.js::DmReasoningBlock` —
 *      renders the collapsible header + body. Tools are rendered
 *      INSIDE the collapsible by design; the sibling `m.type === 'tool'`
 *      branch in `DmConversationView` is a defensive fallback for
 *      entries that couldn't be grouped, not the canonical path.
 *
 * @param {Array} entries - flat chat message entries from mapHistoryMessages
 * @returns {Array} entries with reasoning groups collapsed into dm_reasoning blocks
 */
export function groupDmReasoningBlocks(entries) {
    // Accumulator: runId -> { agentName, thinkingText, tools[], firstIdx }
    const groups = new Map();
    // Track which indices belong to a reasoning group.
    const reasoningIndices = new Set();

    for (let i = 0; i < entries.length; i++) {
        const e = entries[i];

        if (e.type === 'dm_reasoning_text' && e.runId) {
            reasoningIndices.add(i);
            const group = groups.get(e.runId) || {
                agentName: null, thinkingText: '', tools: [], firstIdx: i,
            };
            group.agentName = group.agentName || e.fromAgent;
            group.thinkingText = (group.thinkingText || '') + (e.text || '');
            if (!groups.has(e.runId)) groups.set(e.runId, group);
            continue;
        }

        if (e.type === 'tool' && e.runId) {
            // Every tool entry with a runId belongs to its run's reasoning
            // block — this function only runs on DM history, where the
            // canonical UX is that tool calls are grouped under the agent's
            // collapsible. The earlier `e.isReasoning` requirement caused
            // tools to leak out as sibling rows whenever the persistence
            // path that produced the entry happened to omit the flag (e.g.
            // session_messages entries without `metadata.message_type ===
            // "reasoning"` — possible for legacy rows or fire-and-forget
            // races). Drop the gate so attribution is consistent regardless
            // of which storage path the entry came through (#1076).
            reasoningIndices.add(i);
            const group = groups.get(e.runId) || {
                agentName: null, thinkingText: '', tools: [], firstIdx: i,
            };
            group.tools.push(e);
            // Propagate fromAgent from tool entries so groups with only
            // tools (no thinking text) still get the correct agent name.
            // Fixes #692 — tool entries now carry fromAgent from reasoning
            // metadata (see mapHistoryMessages).
            group.agentName = group.agentName || e.fromAgent;
            if (!groups.has(e.runId)) groups.set(e.runId, group);
            continue;
        }
    }

    if (groups.size === 0) return entries;

    // Build result: replace groups at their firstIdx position,
    // skip individual reasoning entries.
    const result = [];
    // Track which runIds have been inserted.
    const inserted = new Set();

    for (let i = 0; i < entries.length; i++) {
        if (reasoningIndices.has(i)) {
            const e = entries[i];
            const runId = e.runId;
            if (runId && groups.has(runId) && !inserted.has(runId)) {
                const group = groups.get(runId);
                // Skip empty blocks (no tools at all, no thinking text).
                //
                // Count the UNFILTERED tools array — `send_message:done`
                // entries still represent a real agent turn that must
                // surface the collapsible header. This mirrors the
                // render-time hide predicate in DmReasoningBlock
                // (`(tools || []).length > 0`) so the live path (which
                // bypasses this grouper and hits DmReasoningBlock directly)
                // and the reload path (which flows through this grouper)
                // agree on whether a block exists for a given run. Without
                // this alignment, a `send_message:done`-only run with no
                // thinking text emitted a collapsible live but lost it
                // after reload — the exact symptom #1076 set out to fix,
                // surviving on the reload path (#1083).
                if (group.tools.length > 0 || (group.thinkingText && group.thinkingText.trim())) {
                    result.push({
                        id: nextMsgId(),
                        type: 'dm_reasoning',
                        runId: runId,
                        agentName: group.agentName,
                        thinkingText: group.thinkingText || '',
                        tools: group.tools,
                        status: 'done',
                        isLive: false,
                    });
                }
                inserted.add(runId);
            }
            // Skip -- entry consumed into a group.
            continue;
        }
        result.push(entries[i]);
    }

    return result;
}
