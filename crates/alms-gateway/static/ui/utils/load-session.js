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

import { getSession, getSessionMessages, getSessionToolCalls } from '../api/sessions.js';
import { listRuns, listApprovals, listAgentRuns, getRun, getRunReasoning, getRunText } from '../api/runs.js';
import { mapHistoryMessages, groupDmReasoningBlocks } from './history.js';
import { historyCoversSeal } from './reasoning-coverage.js';
import { normalizeApproval } from './approvals.js';
import { chatMessages, nextMsgId } from '../state/chat.js';
import { replaceMessages, appendMessage, updateMessage, filterMessages } from '../state/chat-actions.js';
import { activeRunId, runs, replaceRuns, setRunStatus, upsertRun } from '../state/runs.js';
import { openSessionStream, registerSessionContractReconciler } from '../hooks/use-session-stream.js';
import { setAgentPhase, clearAgentPhase, setDmContext } from '../state/agent-status.js';
import { sessions, crossAgentSessions, activeSessionId, upsertSession } from '../state/sessions.js';
import { activeAgent } from '../state/agents.js';
import { getPendingMessages, confirmOptimisticMessage } from '../state/pending-messages.js';
import { rehydrateSubagentsFromHistory, parentSessionId } from '../state/subagents.js';

/** Internal session types that `/sessions` (list) deliberately filters out
 *  via `is_internal_context_id`. When the resolver-led boot path lands on
 *  one of these as the active session, the envelope must be injected into
 *  `sessions.value` directly so `activeSession` / `isInternalSession`
 *  resolve correctly — `loadSession` is the single chokepoint where that
 *  happens. (#1065) */
const INTERNAL_SESSION_TYPES = new Set(['subagent', 'job', 'episodic', 'notification']);

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
 * Resolve the session's metadata envelope (session_type, participants, …)
 * for `loadSession`'s DM branching.
 *
 * Resolution order:
 *   1. The step-0 `GET /session/{id}` envelope — authoritative, fetched at
 *      the top of every load.
 *   2. The per-agent `sessions` list.
 *   3. The cross-agent `crossAgentSessions` list — REQUIRED for DM /
 *      notification sessions, which PR #1010 (sidebar accordion split)
 *      moved OUT of the per-agent `sessions` list into their own
 *      cross-agent signal.
 *
 * The list fallbacks mirror the `activeSession` computed in
 * `state/sessions.js` and only matter when the envelope fetch failed
 * (older backend without `GET /session/{id}`, transient error).
 *
 * ## Why this helper exists (the #1010 regression behind the DM reload bugs)
 *
 * `loadSession` used to resolve the DM flag with a bare
 * `sessions.value.find(...)`. That was correct when it was written (#690,
 * 2026-04-13: one flat sidebar list), but PR #1010 (2026-05-09) split DMs
 * into `crossAgentSessions` — after which the lookup NEVER found a DM
 * session and `isDmSession` was silently `false` on every load. Three
 * consumers broke at once:
 *
 *   1. `groupDmReasoningBlocks` was skipped, so reloaded DM tool rows
 *      rendered as standalone sibling rows outside the reasoning
 *      collapsible (the "ungrouped DM tool" fallback + console.warn in
 *      DmConversationView) and `dm_reasoning_text` entries vanished
 *      (no render branch for them outside a block).
 *   2. The step-3 in-flight text/reasoning rehydration — explicitly
 *      designed to be SKIPPED for DM sessions — ran anyway on a mid-run
 *      DM load, seeding the run's partial implicit reply (#1156) as an
 *      unsealed `type:'agent'` entry with NO `fromAgent`. The DM view
 *      attributes such entries to the UI-selected perspective agent, so
 *      when the delivered `dm_message` bubble landed with the real
 *      sender, the same reply rendered TWICE — once per agent (the #1164
 *      mis-attributed-duplicate symptom, resurfacing on the reload path).
 *      `sealLastAgent` then sealed the seed at run end, so the duplicate
 *      persisted on the finished conversation until the next full reload.
 *   3. The step-5 phase restore never recognised the DM, so reloading a
 *      running DM showed the generic "Thinking…" header instead of
 *      "Chatting with {peer}…".
 *
 * @param {string} sessionId
 * @param {object|null} envelope - step-0 `GET /session/{id}` response
 * @returns {object|null}
 */
function resolveSessionMeta(sessionId, envelope) {
    return envelope
        || sessions.value.find(s => s.id === sessionId)
        || crossAgentSessions.value.find(s => s.id === sessionId)
        || null;
}

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
 * @param {boolean} [opts.requireAuthoritativeSnapshot=false] - Fail closed
 *   when any cursor-relevant snapshot request fails. Used when recovering
 *   from a rejected SSE frame before advancing past that frame.
 * @returns {Promise<void>}
 */
export async function loadSession(sessionId, opts) {
    const isStale = opts.isStale;
    const logPrefix = opts.logPrefix || 'loadSession';

    // The step-0 metadata envelope, kept for `resolveSessionMeta` so the
    // DM branching below (grouping, in-flight-seed skip, phase restore)
    // reads the authoritative `session_type` / `participants` instead of
    // depending on which sidebar list happens to hold this session.
    let sessionEnvelope = null;

    // Step 0: Fetch the single-session metadata envelope (#1065).
    //
    // Two reasons this has to come before step 1:
    //  - For internal session types (subagent / job / episodic /
    //    notification) the active session is intentionally filtered out
    //    of `/sessions` by `is_internal_context_id`, so the sidebar's
    //    `sessions.value` does not contain it. Injecting the envelope
    //    here is what lets `activeSession` / `isInternalSession` resolve
    //    on the resolver-led boot path (reload while a subagent session
    //    is active) — without it the read-only banner stays suppressed.
    //  - `parent_session_id` is the backend's authoritative parent
    //    pointer for subagent sessions. Populating `parentSessionId.value`
    //    from it here is what renders the "← Back to parent session"
    //    breadcrumb after reload (the drill-down path sets this via
    //    `navigateToSubagentSession`; the boot path had no equivalent
    //    until the backend exposed `parent_session_id` in #1067).
    //
    // Non-fatal: if the envelope fetch fails we fall through to existing
    // behaviour. Older backends without `GET /session/{id}` will 404 here
    // and the rest of the load proceeds normally.
    try {
        const session = await getSession(sessionId);
        if (isStale()) return;
        sessionEnvelope = session || null;

        // Inject the active session into `sessions.value` when it is an
        // internal type that the `/sessions` filter excludes. Bypasses
        // the filter for this single envelope only — the sidebar still
        // hides internal sessions from its list.
        if (session
            && INTERNAL_SESSION_TYPES.has(session.session_type)
            && !sessions.value.some(s => s.id === session.id)) {
            upsertSession(session, 'pinned');
        }

        // Inject an envelope-resolved DM into `crossAgentSessions.value`
        // when it is not already there (Codex P2 #2 on PR #1193 — the
        // companion of `resolveSessionMeta`). `resolveSessionMeta` fixes
        // loadSession's LOCAL DM flag, but two other consumers derive
        // DM-ness from the shared `activeSession` computed (which reads
        // `sessions` + `crossAgentSessions`, NOT the envelope):
        //
        //   - `app.js` picks `DmConversationView` via the `isDmSession`
        //     computed — with `activeSession` unresolved, a DM loads into
        //     the NORMAL chat view.
        //   - `use-session-stream.js::isDmEvent` keys its primary fast
        //     path off `activeSession.value?.session_type === 'dm'` —
        //     unresolved means live DM events can mis-classify during the
        //     attach race.
        //
        // In the envelope-only case (fresh reload / deep-link into a DM
        // before the sidebar's cross-agent fetch lands, or when that fetch
        // failed) both would disagree with loadSession's own (correct) DM
        // flag. Injecting the envelope — which carries `session_type` AND
        // `participants` (`enrich_session_json` is shared between the list
        // and single-session endpoints, so the shape matches a list entry
        // and `dmParticipants` / the header label resolve too) — makes all
        // three consumers agree.
        //
        // Sidebar blast radius: none beyond one legitimate row appearing
        // early. The id-guard prevents duplicates, and the boot path's
        // authoritative cross-agent scope replacement supersedes the
        // injected entry when the full list lands (the DM
        // is in that list anyway).
        if (session
            && session.session_type === 'dm'
            && !crossAgentSessions.value.some(s => s.id === session.id)) {
            upsertSession(session, 'cross');
        }

        // Populate the breadcrumb pointer from the backend. `parent_session_id`
        // is only emitted on subagent envelopes (uuid-or-null); for non-
        // subagent sessions the field is omitted, so reset to null so a
        // stale breadcrumb from a previous subagent view doesn't linger.
        if (session && Object.prototype.hasOwnProperty.call(session, 'parent_session_id')) {
            parentSessionId.value = session.parent_session_id ?? null;
        } else {
            parentSessionId.value = null;
        }
    } catch (err) {
        if (isStale()) return;
        // Non-fatal — log and continue. The read-only banner / breadcrumb
        // may not render, but the session itself still loads via the
        // existing messages / runs / SSE path below.
        console.warn(`[${logPrefix}] Failed to fetch session metadata:`, err);
        if (opts.requireAuthoritativeSnapshot) throw err;
    }

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
        replaceRuns(sessionId, loaded);
    } catch (err) {
        if (isStale()) return;
        if (opts.requireAuthoritativeSnapshot) throw err;
        replaceRuns(sessionId, []);
    }

    // Step 2: Fetch message history and session-level tool calls in
    // parallel.  The tool call records enrich tool rows for DM sessions
    // where tool calls are stored only in run_tool_calls, not in
    // session_messages.  (#609, #632, #634)
    let lastEventId = opts.minimumLastEventId ?? null;
    // Hoisted so step 3 (in-flight reasoning rehydration for #1043) can
    // branch on session type without re-deriving it.
    let isDmSession = false;
    // Load-time terminal-scoped dedupe set (#1133, Layer 3). Holds the
    // run-ids whose final-turn reasoning is already sealed into the assistant
    // bubble rehydrated by the messages GET in step 2; the `reasoning_delta`
    // SSE handler drops any replayed delta carrying one of these run-ids so a
    // run that went terminal during the load does not double-render. Populated
    // and gated in step 3 (see the build site for the coverage logic), then
    // threaded into `openSessionStream` opts at step 4. Built here keyed off
    // the reasoning GET's `run_id` because sealed history messages carry none.
    const sealedReasoningRunIds = new Set();
    try {
        const [historyData, toolCallData] = await Promise.all([
            getSessionMessages(sessionId),
            getSessionToolCalls(sessionId).catch(err => {
                // Non-fatal: the endpoint may not exist on older backends.
                console.warn(`[${logPrefix}] Failed to load session tool calls:`, err);
                if (opts.requireAuthoritativeSnapshot) throw err;
                return { tool_calls: [] };
            }),
        ]);
        if (isStale()) return;

        const rawMsgs = historyData.messages || [];
        const sessionToolCalls = toolCallData.tool_calls || [];
        // Resolve DM flag early so mapHistoryMessages can annotate
        // merged tool entries with isReasoning for DM sessions.
        //
        // MUST go through `resolveSessionMeta` (envelope first, then BOTH
        // sidebar lists): DM sessions live in `crossAgentSessions`, not the
        // per-agent `sessions` list, since PR #1010 — a bare
        // `sessions.value.find(...)` here left this flag permanently false
        // for DMs, skipping the reasoning-block grouping below (tools
        // rendered as ungrouped sibling rows after reload) and disabling
        // the step-3 DM seeding skip (the reload mis-attributed-duplicate
        // bug). See `resolveSessionMeta` for the full regression story.
        const sessionMeta = resolveSessionMeta(sessionId, sessionEnvelope);
        isDmSession = sessionMeta?.session_type === 'dm';

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
        const pendingMessages = getPendingMessages(sessionId);
        for (const pending of pendingMessages) {
            // The backend pre-persists the input before enqueueing the run.
            // A known correlated run therefore makes the loaded history
            // authoritative for this exact optimistic message. Without a run
            // ID we preserve the local entity rather than text-matching: two
            // identical concurrent sends must never settle one another.
            const alreadyPersisted = pending.runId
                ? runs.value.some(run => run.run_id === pending.runId)
                : false;

            if (alreadyPersisted) {
                confirmOptimisticMessage(sessionId, { messageId: pending.messageId });
            } else {
                console.debug(
                    '[loadSession] preserving pending user message for session',
                    sessionId,
                    pending.messageId,
                );
            }
        }
        // For DM sessions, group reasoning entries into collapsible blocks.
        const grouped = isDmSession ? groupDmReasoningBlocks(mapped) : mapped;
        replaceMessages(grouped, sessionId);
        // Rehydrate the SubagentBar from any still-in-flight invoke_agent
        // tool rows in the freshly-loaded history (#1041). Pass the
        // PRE-grouping `mapped` array, not the post-grouping `grouped`
        // array: in DM sessions, `groupDmReasoningBlocks` folds tool rows
        // flagged with `isReasoning: true` (which includes every
        // session-tool-call-record-merged invoke_agent row in DM history —
        // see `history.js::mapHistoryMessages` line 522) into a single
        // `dm_reasoning` block entry, hiding the underlying tool row from
        // any consumer that filters by `type === 'tool'`. Without this
        // change, `rehydrateSubagentsFromHistory` (which looks for
        // `m.type === 'tool' && m.tool === 'invoke_agent'`) sees zero
        // invoke_agent rows in DM sessions and the SubagentBar chip never
        // re-appears after reload. Found by codex P2 + Tim on PR #1049.
        // The function only reads `type`, `tool`, `id`, `ts`, `status`,
        // `params`, and `result`, all of which live on the pre-grouping
        // entries; the chronological-order invariant the function depends
        // on is preserved by `mapHistoryMessages` itself.
        rehydrateSubagentsFromHistory(mapped);
        const historyEventId = historyData.last_event_id ?? null;
        if (historyEventId != null && (lastEventId == null || historyEventId > lastEventId)) {
            lastEventId = historyEventId;
        }
    } catch (err) {
        if (isStale()) return;
        replaceMessages([{ id: nextMsgId(), type: 'error', text: `Failed to load message history: ${err.error?.message || err.message || 'unknown error'}` }], sessionId);
        if (opts.requireAuthoritativeSnapshot) throw err;
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
                    if (opts.requireAuthoritativeSnapshot) throw err;
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
            if (opts.requireAuthoritativeSnapshot) throw err;
        }

        // Rehydrate the in-flight turn's extended-thinking text (#1043,
        // refined per-turn under #1077). This block MUST stay after the
        // thinking-indicator and pending-approval appends above: the live
        // `reasoning_delta` SSE handler attaches each incoming chunk to
        // the most recent unsealed assistant entry at the tail of
        // `chatMessages`, so the rehydrated seed must be the last append
        // before `openSessionStream` fires. Inserting it earlier would
        // let the thinking / approval rows land after it and break the
        // handler's tail lookup.
        //
        // Reasoning is streamed via `reasoning_delta` SSE events but only
        // persisted to the message store at end-of-turn (sealed onto the
        // closing assistant message's `reasoning_blocks` metadata), so a
        // mid-turn reload would otherwise miss every delta that fired
        // before the reload. The dedicated endpoint returns the concatenation
        // of `reasoning_delta` events for the CURRENT TURN ONLY — deltas
        // belonging to already-sealed prior turns are filtered out by a
        // per-run turn boundary (the latest parent-agent `tool_start` /
        // `tool_end` event id; subagent tool events do not move the boundary).
        // That scoping is what keeps prior-turn reasoning from rendering
        // twice on reload: once from the sealed assistant message's
        // `reasoning_blocks` (rehydrated by the messages GET in step 2),
        // and once from the seed appended below. The endpoint also returns
        // the maximum event_id of any included delta; advancing
        // `lastEventId` past that mark makes the subsequent SSE replay
        // skip exactly the events already reflected in the rehydrated
        // text, so the live append in the `reasoning_delta` handler does
        // not double-count. The SEED half of this block is skipped for DM
        // sessions — those route reasoning through `dmThinkingBuffers` and
        // a separate dm_reasoning block layout (out of scope for #1043 /
        // #1077) — but the terminal-only reconciliation is not; see the
        // "DM split" note below.
        //
        // Race mitigation: re-check the run status from the listRuns
        // snapshot before seeding. If the run terminated between step 1
        // (listRuns) and this fetch (e.g. it finished while the page was
        // mid-load), the messages GET in step 2 has already picked up the
        // final assistant message with sealed reasoning_blocks. Seeding an
        // additional unsealed assistant entry on top would render the
        // reasoning twice briefly until the run_finished SSE event arrives
        // and the live handler stops appending. Skip the seed in that
        // case — the persisted message is the authoritative record. The
        // event-id handoff is still safe to apply because it only
        // advances the SSE cursor past events the UI no longer needs to
        // replay.
        //
        // ## DM split (Codex P2 on PR #1193)
        //
        // The block below is intentionally NOT gated on `!isDmSession` as a
        // whole. Two different concerns live here and only ONE is DM-scoped:
        //
        //   - The in-flight text/reasoning SEED (+ its `last_event_id`
        //     cursor bumps) stays NON-DM ONLY. Under implicit DM replies
        //     (#1156) the run's trailing visible text IS the reply,
        //     delivered as the `dm_message` bubble — seeding it here painted
        //     a perspective-side duplicate (the #1164 symptom on reload),
        //     and bumping the cursor would swallow replayable events the DM
        //     live path still needs.
        //   - The TERMINAL-ONLY reconciliation (#1133 Layers 3 + 4: the
        //     suppress-set add, the `activeRunId` clear, and the stray
        //     thinking-row removal) must run for EVERY session type,
        //     including DM. A DM run that `listRuns` reported as running
        //     but which finished before the messages GET sampled its HWM
        //     has its terminal SSE event swallowed by the replay cursor —
        //     without this reconciliation the DM view is left with a stuck
        //     active-run marker and a "Thinking…" row until a manual
        //     reload. The reconciliation reads only the `terminal` flag /
        //     `seal_event_id` from the reasoning GET and never touches the
        //     seed surfaces, so it is safe for the DM layout.
        {
            // Capture the active run-id once. Layer 4 (#1133) may null
            // `activeRunId.value` mid-block when the reasoning GET reports a
            // terminal run, so the GETs below and the dedupe-set add must read
            // this stable local rather than the signal after it is cleared.
            const reasoningRunId = activeRunId.value;
            const activeRunForReasoning = runs.value.find(
                r => r.run_id === reasoningRunId
            );
            const runStillLive = activeRunForReasoning
                && (activeRunForReasoning.status === 'running'
                    || activeRunForReasoning.status === 'queued');
            try {
                const reasoningData = await getRunReasoning(reasoningRunId);
                if (isStale()) return;
                // Terminal-run handling (#1133, Layers 3 + 4). Key the spinner
                // clear (Layer 4) and the thinking-row removal (C2) off the
                // authoritative `terminal` flag, never off empty-text —
                // empty-text is overloaded (a live run with no post-boundary
                // reasoning this turn also returns empty text + null cursor)
                // and would false-positive on a live run. The suppress-set add
                // (Layer 3) additionally gates on `seal_event_id` history
                // coverage — see the sub-race A/B note below.
                //
                // Runs for DM sessions too — see the "DM split" note above.
                // For DM the Layer-3 add is defence-in-depth: the stream's
                // DM delta branch never consults the set, but a replayed
                // delta that falls through to the non-DM path during the
                // attach race (activeSession unresolved, `peerRunIds` empty
                // on a fresh load) is correctly dropped by it instead of
                // spawning a spurious unsealed bubble.
                if (reasoningData?.terminal === true) {
                    // Layer 3 (gated on history coverage — #1133 Codex #3 /
                    // sub-race B). This run's reasoning is sealed onto the
                    // assistant bubble ONLY IF the step-2 messages GET captured
                    // it. Two sub-races of "run went terminal during the load",
                    // split by which side of that GET the completion landed on:
                    //
                    //  A. Run finished BEFORE the messages GET resolved → the
                    //     sealed reasoning is in the loaded history; its
                    //     trailing replayed deltas would double-render →
                    //     suppress via the set.
                    //  B. Run finished AFTER it resolved → history does NOT
                    //     contain the reasoning, yet the run already reports
                    //     `terminal: true`. Suppressing here would drop the
                    //     replayed deltas that are its only remaining source →
                    //     it renders ZERO times until a manual reload.
                    //
                    // `historyCoversSeal` distinguishes them via `seal_event_id`
                    // (the terminal event's id): `historyHWM >= seal_event_id`
                    // ⟺ the messages GET ran after the seal ⟺ sub-race A. See
                    // reasoning-coverage.js for the ordering that makes this
                    // sound. NOTE a per-delta `eventId <= HWM` gate is NOT
                    // enough: in a sub-race-B variant every delta can sit below
                    // the HWM (reasoning finished before the messages GET) while
                    // the SEALED message was appended after it — a per-delta
                    // gate would drop those deltas yet history would not render
                    // them. The terminal-event position is the only correct
                    // anchor.
                    //
                    // `lastEventId` is still the step-2 HWM here — a terminal
                    // run's reasoning/text GETs return null cursors, so the
                    // step-3 bumps below have not moved it yet.
                    if (historyCoversSeal(lastEventId, reasoningData.seal_event_id)) {
                        sealedReasoningRunIds.add(reasoningRunId);
                    }
                    // Layer 4: if the run finished between step-1 `listRuns`
                    // and the step-2 messages GET, its terminal SSE event was
                    // swallowed by the messages-GET HWM, so `run_finished` /
                    // `run_cancelled` never replays and `handleRunEnd` never
                    // clears the spinner. Clear the active-run marker
                    // authoritatively here. A genuinely-live run reports
                    // `terminal: false`, keeps `activeRunId`, and gets
                    // `run_finished` from the live stream as normal.
                    try {
                        const terminalRun = await getRun(reasoningRunId);
                        if (isStale()) return;
                        upsertRun(terminalRun);
                    } catch (runStatusError) {
                        console.warn(
                            '[loadSession] terminal run status refresh failed:',
                            runStatusError,
                        );
                        setRunStatus(reasoningRunId, 'completed', {
                            sessionId,
                        });
                    }
                    // Layer 4 (C2): clearing `activeRunId` stops the input-area
                    // spinner but does NOT remove the inline `type:'thinking'`
                    // chat row seeded above for this run. In the swallowed-
                    // terminal-event window this block handles, the SSE-driven
                    // `sealLastAgent` / `flushDeltaBuffer` that normally drops
                    // that row never fires, so the "Thinking…" bubble would
                    // stick forever. Remove it here, scoped to this run via the
                    // stable `reasoningRunId` local, mirroring the live handlers.
                    filterMessages(
                        m => !(m.type === 'thinking' && m.runId === reasoningRunId)
                    );
                }
                // In-flight reasoning seed + cursor bump — NON-DM ONLY (see
                // the "DM split" note above): the DM layout routes live
                // reasoning through `dmThinkingBuffers` into the
                // `dm_reasoning` collapsible, never a chat-pane bubble.
                if (!isDmSession) {
                    if (runStillLive && reasoningData?.text) {
                        appendMessage({
                            id: nextMsgId(),
                            type: 'agent',
                            role: 'assistant',
                            text: '',
                            reasoning: reasoningData.text,
                            sealed: false,
                            ts: new Date().toISOString(),
                        });
                    }
                    if (reasoningData?.last_event_id != null
                        && (lastEventId == null || reasoningData.last_event_id > lastEventId)) {
                        lastEventId = reasoningData.last_event_id;
                    }
                }
            } catch (err) {
                console.warn(`[${logPrefix}] Failed to load in-flight reasoning:`, err);
                if (opts.requireAuthoritativeSnapshot) throw err;
            }

            // Rehydrate the in-flight turn's visible assistant reply text
            // (#1107). Exact analog of the reasoning SEED above; same
            // MUST-be-last placement, and NON-DM ONLY as a whole — unlike
            // the reasoning GET, this endpoint serves no terminal-
            // reconciliation purpose (the `terminal` flag + `seal_event_id`
            // come from the reasoning GET), so for DM sessions it is
            // skipped entirely: its only outputs are the seed (the partial
            // implicit reply — the #1164 duplicate) and a cursor bump the
            // DM live path must not take.
            // The dedicated endpoint returns the concatenation of
            // `token_delta` text for the CURRENT TURN ONLY — deltas
            // belonging to already-sealed prior turns are dropped by the
            // backend buffer's per-turn boundary (cleared on parent-agent
            // `tool_start` / `tool_end`), mirroring the reasoning
            // endpoint's #1077 turn-scoping contract.
            //
            // Tail-merge semantics: the reasoning seed above may have
            // already appended an unsealed assistant entry. If so, we
            // fold the rehydrated text into that same entry's `text`
            // field so the chat pane renders ONE bubble carrying both
            // reasoning + text (matching the live handlers, which both
            // target the same tail unsealed entry). If no reasoning seed
            // landed (reasoning empty for this turn), we append a fresh
            // unsealed entry with `reasoning: ''`. Either way the result
            // is the single tail unsealed entry that `flushDeltaBuffer`
            // and `reasoning_delta` will append further chunks to as the
            // live SSE stream resumes — no double-bubble, no orphaned
            // append target.
            //
            // Race mitigation: re-check the run-still-live status from
            // the listRuns snapshot just as the reasoning block does. If
            // the run terminated between step 1 and this fetch, the
            // messages GET in step 2 has already picked up the final
            // assistant message with its full visible text, and seeding
            // an additional unsealed entry would render the text twice
            // until `run_finished` arrives. Skip the seed in that case.
            // The `last_event_id` handoff is still safe to apply.
            if (!isDmSession) {
                try {
                    const textData = await getRunText(reasoningRunId);
                    if (isStale()) return;
                    if (runStillLive && textData?.text) {
                        const merged = updateMessage(
                            m => m.type === 'agent' && !m.sealed,
                            m => ({ ...m, text: (m.text || '') + textData.text }),
                        );
                        if (!merged) {
                            appendMessage({
                                id: nextMsgId(),
                                type: 'agent',
                                role: 'assistant',
                                text: textData.text,
                                reasoning: '',
                                sealed: false,
                                ts: new Date().toISOString(),
                            });
                        }
                    }
                    if (textData?.last_event_id != null
                        && (lastEventId == null || textData.last_event_id > lastEventId)) {
                        lastEventId = textData.last_event_id;
                    }
                } catch (err) {
                    console.warn(`[${logPrefix}] Failed to load in-flight text:`, err);
                    if (opts.requireAuthoritativeSnapshot) throw err;
                }
            }
        }
    }

    // Step 4: Open persistent session stream, skipping replay of events
    // already reflected in the loaded message history.
    //
    // IMPORTANT: openSessionStream() calls closeSessionStream() internally,
    // which calls clearAgentPhase(). Any phase restoration must happen
    // AFTER this call, not before — otherwise it gets wiped immediately.
    if (isStale()) return;
    // Thread the load-time sealed-reasoning dedupe set (#1133, Layer 3)
    // into the stream so its `reasoning_delta` handler can drop replayed
    // deltas for runs whose reasoning is already sealed into history.
    openSessionStream(sessionId, {
        lastEventId,
        streamEpoch: opts.streamEpoch ?? null,
        sealedReasoningRunIds,
    });

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
            // Same `resolveSessionMeta` contract as the step-2 DM flag: a DM
            // session is only present in `crossAgentSessions` (PR #1010), so
            // the previous bare `sessions.value.find(...)` never matched and
            // a running DM reloaded into the generic "Thinking…" header
            // instead of "Chatting with {peer}…". The envelope also carries
            // `participants` for DM sessions, so the peer derivation works
            // even when neither sidebar list has resolved yet.
            const session = resolveSessionMeta(sessionId, sessionEnvelope);
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

registerSessionContractReconciler(async (sessionId, boundaryEventId, streamEpoch) => {
    await loadSession(sessionId, {
        isStale: () => activeSessionId.value !== sessionId,
        logPrefix: 'contract-reconcile',
        minimumLastEventId: boundaryEventId,
        streamEpoch,
        requireAuthoritativeSnapshot: true,
    });
});
