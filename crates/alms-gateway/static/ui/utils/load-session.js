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
 *   6. Restore agent phase (MUST be after step 5 — openSessionStream
 *      calls clearAgentPhase() internally during teardown).
 *      Includes cross-session DM visibility (#688): when viewing a non-DM
 *      session and the agent is in a DM on another session, the status
 *      bar shows "Chatting with {peer}..." via an async agent-runs check.
 */

import { getSessionMessages, getSessionToolCalls } from '../api/sessions.js';
import { listRuns, listApprovals, listAgentRuns, getRun } from '../api/runs.js';
import { mapHistoryMessages, groupDmReasoningBlocks } from './history.js';
import { normalizeApproval } from './approvals.js';
import { chatMessages, nextMsgId } from '../state/chat.js';
import { replaceMessages, appendMessage } from '../state/chat-actions.js';
import { activeRunId, runs } from '../state/runs.js';
import { openSessionStream } from '../hooks/use-session-stream.js';
import { setAgentPhase, clearAgentPhase, setDmContext } from '../state/agent-status.js';
import { sessions } from '../state/sessions.js';
import { activeAgent } from '../state/agents.js';
import { getPendingMessage, clearPendingMessage } from '../state/pending-messages.js';

/**
 * Maximum number of session runs to fetch when restoring active-run state.
 *
 * The backend `list_by_session` returns runs newest-first and truncates to
 * the requested limit (`crates/alms-gateway/src/server/run_manager.rs:
 * `list_by_session`). The active-run restore path needs to find a still-
 * running run even when many newer queued / terminal runs exist in the
 * same session, otherwise the run drops out of the fetch window and the
 * UI silently fails to rehydrate the thinking indicator / approvals.
 *
 * 200 covers ordinary "agent has been busy for a while" cases without
 * being unbounded; if a session genuinely accumulates more than 200
 * newer runs in front of an older still-running one, the user has bigger
 * problems than UI restoration. The history list itself can paginate
 * later if/when we add a dedicated UI for it.  (#735)
 */
const SESSION_RUNS_RESTORE_LIMIT = 200;

/**
 * Maximum number of agent-scoped runs to fetch when restoring the global
 * agent phase across sessions (the "Chatting with {peer}..." cross-session
 * status, #688). Same truncation concern as `SESSION_RUNS_RESTORE_LIMIT`,
 * but applied to `list_by_agent` (`crates/alms-gateway/src/server/
 * run_manager.rs::list_by_agent`).
 *
 * Smaller than the per-session window because the cross-session restore
 * is best-effort — if it misses, the next live SSE event will set the
 * status correctly. 100 still comfortably exceeds the previous hardcoded
 * 10 and covers realistic agent run rates. (#735)
 */
const AGENT_RUNS_RESTORE_LIMIT = 100;

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
    //
    // Use a larger fetch window (SESSION_RUNS_RESTORE_LIMIT) for the
    // active-run restore step so an older still-running run is not hidden
    // behind a newer queued / terminal backlog. (#735)
    try {
        const data = await listRuns(sessionId, SESSION_RUNS_RESTORE_LIMIT);
        if (isStale()) return;
        const loaded = data.runs || [];
        runs.value = loaded;

        // Prefer a `running` run over a `queued` one when both exist for
        // this session. The backend returns runs newest-first, so without
        // this preference a newer queued run would mask an older still-
        // running run, leaving the UI showing queued/idle while work is
        // already executing. (#735)
        const active = loaded.find(r => r.status === 'running')
            || loaded.find(r => r.status === 'queued');
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
        // Resolve DM flag early so mapHistoryMessages can annotate
        // merged tool entries with isReasoning for DM sessions.
        const sessionObj = sessions.value.find(s => s.id === sessionId);
        const isDmSession = sessionObj?.session_type === 'dm';

        const mapped = mapHistoryMessages(rawMsgs, {
            hasActiveRun: !!activeRunId.value,
            sessionToolCalls,
            isDm: isDmSession,
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
            // If the pending entry has a runId, look up that run.  The
            // backend pre-persists the user's message to the session
            // synchronously in `create_run`, BEFORE the run is enqueued,
            // so the loaded history is already the source of truth for
            // any run that exists in `runs.value` (queued, running, or
            // terminal).  If the runId is present and the run is found,
            // the message is guaranteed to be in the history.
            //
            // This avoids false-positive deduplication via text matching:
            // if the user sends identical text twice and switches away
            // before the second is persisted, text comparison would
            // incorrectly match the first occurrence and drop the second.
            let alreadyPersisted = false;
            if (pending.runId) {
                const run = runs.value.find(r => r.run_id === pending.runId);
                // Any known run means the backend received the request and
                // pre-persisted the input -- the history reflects it.
                alreadyPersisted = !!run;
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
                    // Per-message timestamp (#855) — best-effort: the
                    // pending message has no server timestamp yet, so use
                    // the moment the message was queued (Date.now() saved
                    // on `pending`) when available, falling back to "now".
                    ts: pending.ts || new Date().toISOString(),
                });
                console.debug(`[${logPrefix}] re-injected pending user message for session`, sessionId);
            }
        }

        // For DM sessions, group reasoning entries into collapsible blocks.
        const grouped = isDmSession ? groupDmReasoningBlocks(mapped) : mapped;
        replaceMessages(grouped);
        lastEventId = historyData.last_event_id ?? null;
    } catch (err) {
        if (isStale()) return;
        replaceMessages([{ id: nextMsgId(), type: 'error', text: `Failed to load message history: ${err.error?.message || err.message || 'unknown error'}` }]);
    }

    // Step 3: If a run is in-progress, append a thinking indicator and
    // reconstruct pending approval prompts from the server so the user
    // can still approve/deny waiting tool calls. (Fixes #487 Bug 2)
    //
    // Distinguish queued vs running: queued runs show "Agent is busy"
    // instead of "Thinking..." so the user knows the agent hasn't
    // started processing yet. (#691)
    if (activeRunId.value) {
        if (!chatMessages.value.some(m => m.type === 'thinking')) {
            const activeRun = runs.value.find(r => r.run_id === activeRunId.value);
            const isQueued = activeRun && activeRun.status === 'queued';
            // For queued runs, fetch the live queue_position from the
            // single-run endpoint so the chip shows "Queued — position N"
            // immediately on reload instead of "position 1" until the next
            // SSE decrement arrives. (#831)
            //
            // GET /runs/{id} returns `queue_position: Option<usize>` —
            // skip_serializing_if = "Option::is_none" — present and >= 1
            // only while the run is still queued. listRuns() does not
            // expose this field today, so the secondary fetch is required.
            //
            // Falls back to position 1 ("next up") if:
            //   - the run is queued but the fetch fails
            //   - the run has no queue_position (race: dequeued between
            //     listRuns and getRun) — `>0` triggers the chip and
            //     run_started will clear it microseconds later anyway
            let queuedBehind = isQueued ? 1 : 0;
            if (isQueued) {
                try {
                    const runDetail = await getRun(activeRunId.value);
                    if (isStale()) return;
                    if (typeof runDetail?.queue_position === 'number'
                        && runDetail.queue_position > 0) {
                        queuedBehind = runDetail.queue_position;
                    }
                } catch (err) {
                    console.warn(`[${logPrefix}] Failed to load queue position:`, err);
                }
            }
            // Stamp runId so the live `run_queue_position` SSE handler can
            // locate this indicator and decrement it in place.
            appendMessage({
                id: nextMsgId(), type: 'thinking',
                queuedBehind, runId: activeRunId.value,
            });
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
    //
    // IMPORTANT: openSessionStream() calls closeSessionStream() internally,
    // which calls clearAgentPhase(). Any phase restoration must happen
    // AFTER this call, not before — otherwise it gets wiped immediately.
    if (isStale()) return;
    openSessionStream(sessionId, { lastEventId });

    // Step 5: Restore agent phase for in-progress runs.
    //
    // Status events are ephemeral (not persisted to the session event
    // log), so when the user switches away from a session and then
    // switches back, the SSE stream replay contains no status event and
    // the header bar stays blank until the backend emits the next phase
    // update.  Setting a reasonable default here bridges the gap.
    //
    // This MUST happen after openSessionStream() because that function
    // calls closeSessionStream() -> clearAgentPhase() as part of its
    // teardown-then-open sequence.
    //
    // The placeholder phase set here is temporary — the next real
    // run_finished, run_error, or run_cancelled SSE event will clear it
    // via clearAgentPhase(), and any status event will override it with
    // the actual phase.
    //
    // Queued runs: the header bar should NOT show a "queued" phase.
    // The agent may be busy with another task (e.g. a DM with another
    // peer) and the header reflects the agent's CURRENT activity, not
    // this run's queue position.  The inline thinking indicator already
    // shows queue status via queuedBehind. (#693)
    //
    // Running runs: set a reasonable placeholder phase so the header
    // is not blank until the next real status SSE event arrives.
    if (activeRunId.value) {
        const activeRun = runs.value.find(r => r.run_id === activeRunId.value);
        const isQueued = activeRun && activeRun.status === 'queued';

        if (!isQueued) {
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
        }
        // else: queued -- leave header idle, SSE stream will provide
        // real status if/when the run starts on this session.
    } else {
        // No active run on this session -- check if the agent is busy
        // with a DM on a different session (cross-session visibility,
        // #688 / #703).  This makes the "Chatting with..." status
        // appear even when viewing the webchat session.
        //
        // The query is best-effort and async -- if it fails, the
        // status bar stays idle until the next dm_activity_started
        // SSE event arrives from the backend.
        const agentId = activeAgent.value?.id;
        if (agentId) {
            restoreGlobalAgentPhase(agentId, sessionId, isStale, logPrefix)
                .catch(err => console.warn(`[${logPrefix}] restoreGlobalAgentPhase uncaught:`, err));
        } else {
            clearAgentPhase();
        }
    }
}

/**
 * Check if the agent has any active DM runs across all sessions and
 * restore the "Chatting with..." status if so.
 *
 * This is a best-effort async call: if it fails or the session becomes
 * stale, the status bar simply stays idle until the next live SSE event.
 *
 * @param {string} agentId - The agent to check
 * @param {string} sessionId - The session being loaded (for DM peer derivation)
 * @param {function} isStale - Staleness checker
 * @param {string} logPrefix - Log label
 */
async function restoreGlobalAgentPhase(agentId, sessionId, isStale, logPrefix) {
    try {
        // Use AGENT_RUNS_RESTORE_LIMIT (not the previous hardcoded 10) so
        // the still-running DM run is not hidden behind a backlog of newer
        // queued / terminal runs for the same agent. (#735)
        const data = await listAgentRuns(agentId, AGENT_RUNS_RESTORE_LIMIT);
        if (isStale()) return;
        const agentRuns = data.runs || [];

        // Look for a running DM run (not queued) to derive the peer name.
        const activeDmRun = agentRuns.find(
            r => r.session_type === 'dm' && r.status === 'running'
        );
        if (activeDmRun && activeDmRun.context_id) {
            // context_id is "dm:<name1>:<name2>" -- derive the peer name
            // by finding the participant that is NOT the active agent.
            const agentName = activeAgent.value?.name;
            const parts = activeDmRun.context_id.split(':');
            if (parts.length >= 3 && parts[0] === 'dm' && agentName) {
                const peer = parts[1] === agentName ? parts[2] : parts[1];
                if (peer) {
                    setDmContext(peer);
                    console.debug(`[${logPrefix}] restored cross-session DM status: Chatting with ${peer}`);
                    return;
                }
            }
        }

        // No active DM run -- check for any non-DM running run.
        const activeRun = agentRuns.find(r => r.status === 'running');
        if (activeRun) {
            setAgentPhase('calling_llm', null);
        } else {
            clearAgentPhase();
        }
    } catch (err) {
        console.warn(`[${logPrefix}] Failed to check agent global status:`, err);
        // Fall back to idle -- next SSE event will update it.
        clearAgentPhase();
    }
}
