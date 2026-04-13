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
import { getPendingMessage, clearPendingMessage } from '../state/pending-messages.js';

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
        // Reconcile pending user messages: if the user sent a message and
        // switched sessions before the backend persisted it, the history
        // fetch will not contain it.  Re-inject it so the user sees their
        // own message when switching back.  (Fixes message-loss on rapid
        // session switch.)
        const pending = getPendingMessage(sessionId);
        if (pending) {
            // Determine whether the backend has persisted the pending
            // message by checking the run's status.  The runs list was
            // fetched in step 1 and is available in runs.value.
            //
            // If the pending entry has a runId, look up that run.  A run
            // that has progressed past "queued" means the agent loop has
            // started and will have persisted the user message to the
            // session history -- so the loaded history is trustworthy.
            //
            // This avoids false-positive deduplication via text matching:
            // if the user sends identical text twice and switches away
            // before the second is persisted, text comparison would
            // incorrectly match the first occurrence and drop the second.
            let alreadyPersisted = false;
            if (pending.runId) {
                const run = runs.value.find(r => r.run_id === pending.runId);
                // "running", "finished", "error", "cancelled" all mean the
                // agent loop started (or completed) and the user message
                // was persisted to the session DB.  Only "queued" means
                // the message may not yet be in the history.
                alreadyPersisted = run && run.status !== 'queued';
            } else {
                // runId not yet available (createRun response has not
                // returned).  Fall back to text matching as a best-effort
                // check -- this window is very short (HTTP round-trip).
                const lastUserMsg = mapped.findLast(m => m.type === 'user');
                alreadyPersisted = lastUserMsg && lastUserMsg.text === pending.text;
            }

            if (alreadyPersisted) {
                // Backend has persisted it -- no longer pending.
                clearPendingMessage(sessionId);
            } else {
                // Re-inject the user message at the end of the mapped
                // history (before any thinking indicator added in step 3).
                mapped.push({
                    id: nextMsgId(),
                    type: 'user',
                    role: 'user',
                    text: pending.text,
                    sealed: true,
                });
                console.debug(`[${logPrefix}] re-injected pending user message for session`, sessionId);
            }
        }

        replaceMessages(mapped);
        lastEventId = historyData.last_event_id ?? null;
    } catch (err) {
        if (isStale()) return;
        replaceMessages([{ id: nextMsgId(), type: 'error', text: `Failed to load message history: ${err.error?.message || err.message || 'unknown error'}` }]);
    }

    // Step 3: If a run is in-progress, append a thinking indicator and
    // reconstruct pending approval prompts from the server so the user
    // can still approve/deny waiting tool calls. (Fixes #487 Bug 2)
    if (activeRunId.value) {
        if (!chatMessages.value.some(m => m.type === 'thinking')) {
            appendMessage({ id: nextMsgId(), type: 'thinking' });
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
