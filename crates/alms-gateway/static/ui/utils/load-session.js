/**
 * Shared session-loading logic used by both boot (use-boot.js) and
 * session-switch (session-list.js).
 *
 * Extracted to eliminate duplication that caused maintenance overhead
 * (e.g. PR #518 diagnostic logging had to be added in both places).
 *
 * The loading sequence is:
 *   1. Fetch runs -> restore activeRunId for any in-progress run
 *   2. Fetch message history + session tool calls in parallel
 *   3. Merge tool call data into history -> mapHistoryMessages -> set chatMessages
 *   4. If active run: append thinking indicator + reconstruct approvals
 *   5. Open SSE stream with lastEventId to skip event replay
 */

import { getSessionMessages, getSessionToolCalls } from '../api/sessions.js';
import { listRuns, listApprovals } from '../api/runs.js';
import { mapHistoryMessages } from './history.js';
import { normalizeApproval } from './approvals.js';
import { chatMessages, nextMsgId } from '../state/chat.js';
import { replaceMessages, appendMessage } from '../state/chat-actions.js';
import { activeRunId, runs } from '../state/runs.js';
import { openSessionStream } from '../hooks/use-session-stream.js';
import { setAgentPhase, setDmContext } from '../state/agent-status.js';
import { sessions } from '../state/sessions.js';
import { activeAgent } from '../state/agents.js';

/**
 * Load a session's runs, chat history, pending approvals, and open SSE stream.
 *
 * Both the boot path and the session-switch path call this function after
 * setting up their own preconditions (state resets, generation bumps, etc.).
 *
 * @param {string} sessionId - The session to load
 * @param {object} opts
 * @param {function} opts.isStale - Returns true if a newer load has been
 *   initiated (wraps the caller's generation counter check). Checked at
 *   every async boundary to discard stale fetches.
 * @param {string} [opts.logPrefix='loadSession'] - Label for diagnostic log messages
 * @returns {Promise<void>}
 */
export async function loadSession(sessionId, opts) {
    const isStale = opts.isStale;
    const logPrefix = opts.logPrefix || 'loadSession';

    // Step 1: Fetch runs and restore activeRunId for any in-progress run.
    // This must happen before history loading so that mapHistoryMessages
    // can mark unmatched tool_calls as 'running' instead of 'done'.
    try {
        const data = await listRuns(sessionId);
        if (isStale()) return;
        const loaded = data.runs || [];
        runs.value = loaded;

        const active = loaded.find(r => r.status === 'queued' || r.status === 'running');
        if (active) {
            activeRunId.value = active.run_id;
        }
    } catch {
        if (isStale()) return;
        runs.value = [];
    }

    // Step 2: Fetch message history and session-level tool calls in
    // parallel.  The tool call records enrich tool rows for DM sessions
    // where tool calls are stored only in run_tool_calls, not in
    // session_messages.  (#609, #632, #634)
    let lastEventId = null;
    try {
        const [historyData, toolCallData] = await Promise.all([
            getSessionMessages(sessionId),
            getSessionToolCalls(sessionId).catch(err => {
                // Non-fatal: the endpoint may not exist on older backends.
                console.warn(`[${logPrefix}] Failed to load session tool calls:`, err);
                return { tool_calls: [] };
            }),
        ]);
        if (isStale()) return;

        const rawMsgs = historyData.messages || [];
        const sessionToolCalls = toolCallData.tool_calls || [];

        const mapped = mapHistoryMessages(rawMsgs, {
            hasActiveRun: !!activeRunId.value,
            sessionToolCalls,
        });

        // Diagnostic: log tool call counts for #501 investigation.
        const apiToolCalls = rawMsgs.filter(m => m.type === 'tool_call').length;
        const mappedTools = mapped.filter(m => m.type === 'tool').length;
        if (apiToolCalls > 0 || mappedTools > 0 || sessionToolCalls.length > 0) {
            console.debug(`[${logPrefix}] history loaded:`,
                rawMsgs.length, 'API messages,',
                apiToolCalls, 'tool_calls ->',
                mappedTools, 'tool rows,',
                sessionToolCalls.length, 'session tool call records');
        }
        replaceMessages(mapped);
        lastEventId = historyData.last_event_id ?? null;
    } catch (err) {
        if (isStale()) return;
        replaceMessages([{ id: nextMsgId(), type: 'error', text: `Failed to load message history: ${err.error?.message || err.message || 'unknown error'}` }]);
    }

    // Step 3: If a run is in-progress, append a thinking indicator,
    // restore the agent phase signal, and reconstruct pending approval
    // prompts from the server so the user can still approve/deny
    // waiting tool calls. (Fixes #487 Bug 2)
    //
    // Phase restoration: status events are ephemeral (not persisted to
    // the session event log), so when the user switches away from a
    // session and then switches back, the SSE stream replay contains no
    // status event and the header bar stays blank until the backend
    // happens to emit the next phase update.  Setting a reasonable
    // default here bridges the gap — the next real status event from
    // the SSE stream will override it with the actual phase.
    if (activeRunId.value) {
        if (!chatMessages.value.some(m => m.type === 'thinking')) {
            appendMessage({ id: nextMsgId(), type: 'thinking' });
        }

        // Restore agent phase based on session type.
        const session = sessions.value.find(s => s.id === sessionId);
        if (session && session.session_type === 'dm' && Array.isArray(session.participants)) {
            // DM session: derive the peer name by finding the participant
            // that is NOT the active agent, then set the DM context so
            // the status bar shows "Chatting with {peer}...".
            const agentName = activeAgent.value?.name;
            const peer = agentName
                ? session.participants.find(p => p !== agentName)
                : session.participants[0];
            if (peer) {
                setDmContext(peer);
            } else {
                setAgentPhase('calling_llm', null);
            }
        } else {
            setAgentPhase('calling_llm', null);
        }
        try {
            const approvalData = await listApprovals(sessionId);
            if (isStale()) return;
            const pending = approvalData.approvals || [];
            if (pending.length > 0) {
                const approvalMsgs = pending.map(a => {
                    const norm = normalizeApproval(a);
                    return {
                        id: nextMsgId(),
                        type: 'approval',
                        approvalId: norm.approvalId,
                        tool: norm.tool,
                        params: norm.params,
                        runId: norm.runId,
                        resolved: false,
                    };
                });
                appendMessage(...approvalMsgs);
            }
        } catch (err) {
            console.warn(`[${logPrefix}] Failed to load pending approvals:`, err);
        }
    }

    // Step 4: Open persistent session stream, skipping replay of events
    // already reflected in the loaded message history.
    if (isStale()) return;
    openSessionStream(sessionId, { lastEventId });
}
