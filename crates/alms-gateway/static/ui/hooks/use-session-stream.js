/**
 * Session-level SSE stream -- persistent connection across runs.
 *
 * Opens EventSource to /sessions/{sessionId}/events and handles all
 * event types. Stays open across runs -- notification runs, background
 * subagent completions, and job runs all arrive on the same stream.
 *
 * Key invariants enforced by this module:
 *
 *   1. Every message object written to chatMessages has a stable `id`
 *      (via nextMsgId()) so that Preact's VDOM reconciler can correctly
 *      match DOM nodes when messages are inserted or removed.
 *
 *   2. When multiple signal writes happen in the same synchronous
 *      handler, they are wrapped in batch() to collapse Preact
 *      re-renders into a single pass and avoid intermediate visual
 *      states.
 *
 *   3. tool_end only writes chatMessages.value when a matching tool
 *      message was actually found (avoids unnecessary re-renders for
 *      subagent-only tool events).
 *
 *   4. The `on()` event wrapper captures `selectGeneration` at stream-open
 *      time and discards any event whose generation no longer matches the
 *      current value.  This prevents stale SSE events (arriving during
 *      the close→reopen window of a session switch) from mutating state
 *      for the wrong session.  The rAF-scheduled `flushDeltaBuffer` path
 *      has the same guard to cover the token_delta→rAF async gap.
 */

import { batch, signal } from '../deps.js';
import { chatMessages, nextMsgId } from '../state/chat.js';
import { appendMessage, updateMessage, filterMessages, transformMessages } from '../state/chat-actions.js';
import {
    activeRunId,
    bumpRunListGeneration,
    upsertRun,
    setRunStatus,
} from '../state/runs.js';
import { trackSubagentStart, trackSubagentEnd, trackSubagentActivity, findSubagentByToolInvocationId, findSubagentBySessionId, setSubagentSessionId, activeSubagents } from '../state/subagents.js';
import { agentPhase, setAgentPhase, clearAgentPhase, setDmContext, revertPhase, dmPeer } from '../state/agent-status.js';
import { messageQueue } from '../state/queue.js';
import { activeSessionId, activeSession, dmParticipants } from '../state/sessions.js';
import { normalizeApproval } from '../utils/approvals.js';
import { selectGeneration } from '../state/select-generation.js';
import { confirmOptimisticMessage } from '../state/pending-messages.js';
import { DM_END_REASON_LABELS } from '../utils/constants.js';
import {
    markStreamDead,
    clearStreamDead,
    registerSessionReconnect,
} from '../state/stream-health.js';
import {
    setSealedReasoningRunIds,
    getSealedReasoningRunIds,
    clearSealedReasoningRunIds,
} from '../state/reasoning-dedupe.js';

/**
 * Per-run DM **reasoning** accumulation buffer — the text shown inside the
 * collapsible `DmReasoningBlock`. Keyed by run_id.
 *
 * This holds ONLY what the backend persists as DM reasoning (so the live
 * collapsible matches the reload collapsible, #1157/#1162):
 *
 *   1. Every `reasoning_delta` (extended-thinking trace) for the run.
 *   2. Visible `token_delta` text from a turn that *also* made a tool call —
 *      i.e. "thinking out loud" that precedes a tool, which the runtime
 *      persists as a `message_type: "reasoning"` row (see
 *      `persist_assistant_tool_calls` in loop_impl.rs). That text is
 *      "committed" into this buffer at the next `tool_start` boundary.
 *
 * It deliberately EXCLUDES the run's trailing visible text — the implicit DM
 * reply (#1156). That text streams as `token_delta` after the last tool
 * boundary and is delivered to the peer as the `dm_message` bubble; the
 * runtime does NOT persist it as a reasoning row (`finish_run` in mod.rs
 * persists only the final-turn extended-thinking trace, and only when it is
 * distinct from the reply). Painting it here too made the reply render twice
 * live (once in the collapsible, once as the bubble) — mis-attributed to the
 * wrong agent when participants hadn't resolved, and partial-then-full while
 * the stream was mid-flight. The trailing visible text lives in
 * `dmPendingReplyBuffers` below until a tool boundary promotes it (intermediate
 * thinking) or the run ends (implicit reply → discarded from the collapsible).
 *
 * Uses a Preact signal so that DmReasoningBlock components re-render when new
 * committed reasoning text arrives.
 */
export const dmThinkingBuffers = signal(new Map());

/**
 * Per-run buffer for the trailing visible `token_delta` text accumulated since
 * the run's last tool boundary (or run start). Keyed by run_id.
 *
 * This is the candidate **implicit DM reply** (#1156): the agent's final
 * assistant text IS the reply, delivered as a `dm_message` bubble. It must NOT
 * render in the reasoning collapsible (that is the #1157/#1162 double-render).
 *
 * Lifecycle:
 *   - DM `token_delta` appends here (never directly into `dmThinkingBuffers`).
 *   - A DM `tool_start` "commits" the pending text into `dmThinkingBuffers`
 *     (it preceded a tool call, so the runtime persisted it as reasoning) and
 *     clears the pending slot for the next turn.
 *   - Run end discards whatever remains here (the implicit reply, already shown
 *     as the bubble) and clears the slot.
 *
 * Not a signal: nothing renders it, so it needs no reactivity. Kept at module
 * scope alongside `deltaBuffer` and cleared per stream teardown in
 * `closeSessionStream`.
 */
const dmPendingReplyBuffers = new Map();

/**
 * DM / peer-triggered runs, keyed by run-id, valued by the `peer:<name>`
 * `source` their `run_created` carried. Membership is read by `isDmEvent` as a
 * third proof a run is a DM run; the recorded source is read by `run_started`
 * to label the run's `dm_reasoning` block.
 *
 * Why the source is retained and not just the id (#1166): `run_started` used to
 * recover the source by scanning `chatMessages` for a queued `thinking` row.
 * That row is swept run-id-blind by `sealLastAgent` (which every run end and
 * every `dm_message` calls), so a run queued behind a busy agent routinely lost
 * its source before it started — and the block was then labelled with the
 * UI-selected `activeAgent`. Render state is the wrong place to keep wire
 * facts; this map is the same per-stream lifetime as the id set it replaces.
 *
 * Why this is needed (#1162 sym-2 / sym-1 attach race): `isDmEvent` otherwise
 * relies on `activeSession.session_type === 'dm'` OR an existing live
 * `dm_reasoning` block. Both can be absent when the SSE stream delivers
 * `run_created` + its deltas before `loadSession` resolves the session as a DM
 * — and `run_created` only creates the `dm_reasoning` block when the session is
 * already resolved, so the block-existence fallback is chicken-and-egg. In that
 * window a DM run's `token_delta` / `reasoning_delta` (including the buffered-
 * fallback re-emit) fall through to the non-DM path and paint a VISIBLE bubble
 * that then double-renders against the `dm_message`. The `peer:` source on
 * `run_created` is an unambiguous, attach-timing-independent signal that the run
 * is a DM run, so recording it closes the gap. Cleared per stream teardown.
 */
const peerRunSources = new Map();

/**
 * Derive the correct agent name for a DM reasoning block from the
 * run's source field and the DM participants list.
 *
 * In a DM between Alice and Bob:
 *   - source "peer:Alice" means Alice sent a message, so BOB is reasoning
 *   - source "peer:Bob"   means Bob sent a message, so ALICE is reasoning
 *
 * Fixes #692 — previously used `activeAgent.value?.name` which is the
 * agent selected in the UI dropdown, not necessarily the one reasoning.
 *
 * ## Attribution safety for `peer:` sources (#1162 sym-1)
 *
 * For a `peer:X` source the reasoning agent is unambiguously "the participant
 * that is NOT X" — but naming it requires the resolved participants list. If
 * `dmParticipants` has not resolved yet (the SSE stream can deliver
 * `run_created` before `loadSession` populates the session metadata),
 * falling back to `activeAgent` is a coin flip: it is the UI-selected agent,
 * which is the reasoning agent only when the operator happens to be viewing
 * that side. Guessing wrong mis-attributed the block to the *sender* (Alice),
 * which — combined with the old habit of streaming the reply into the
 * collapsible — is exactly the "reply shows first as an Alice message" report.
 * So for `peer:` sources we return `null` (the block renders the neutral
 * "Agent reasoning" label) rather than risk a wrong name.
 *
 * ## Why there is no `activeAgent` fallback at all (#1166)
 *
 * #1164 left an `activeAgent` fallback here for "NON-peer sources
 * (user-initiated runs), where the active agent genuinely is the one
 * reasoning". Both halves of that premise are false on a DM session:
 *
 *   1. There are no user-initiated DM runs. `POST /runs` on any `dm:` session
 *      is unconditionally rejected with `DM_SESSION_NOT_DIRECTLY_RUNNABLE`
 *      (runs/lifecycle.rs), so every run this function is asked about was
 *      peer-triggered. A source that is not `peer:`-shaped here means the
 *      source was LOST, not that it was user-initiated.
 *   2. The active agent need not be a participant. `navigateToSession`
 *      deliberately does NOT `switchAgent` for DM rows ("the operator might
 *      be reading alice's chat and quickly peeking at a DM-from-bob"), so
 *      `activeAgent` while a DM is open is whatever the operator last
 *      selected — frequently a third agent that is not in the conversation.
 *
 * So the fallback could not just pick the wrong side, it could label the
 * block with an agent that is not in the DM at all. Every caller is inside a
 * DM branch, so an unrecognised source now yields the same neutral label the
 * unresolved-participants case already used.
 *
 * @param {string|null} source - the run's source field (e.g. "peer:Alice")
 * @returns {string|null} the name of the agent doing the reasoning, or null
 *   when it cannot be determined without risk of mis-attribution
 */
function dmReasoningAgentName(source) {
    if (source && source.startsWith('peer:')) {
        const peerName = source.slice(5);
        const participants = dmParticipants.value;
        // The peer triggered the run — the OTHER participant is reasoning.
        //
        // The membership test is load-bearing, not defensive: without it a
        // `peerName` matching NEITHER participant still satisfied the ternary
        // and returned `participants[0]` — a name borrowed from an arbitrary
        // index, with no detector. That was the one path by which this
        // function could still name a non-owner, which the "never a guess"
        // rule below does not permit; it also SHADOWED the strictly better
        // `agentName` term that `run_started` ranks after this one.
        if (participants.length >= 2 && participants.includes(peerName)) {
            return participants[0] === peerName ? participants[1] : participants[0];
        }
        // Participants unresolved, or the peer is not one of them: prefer the
        // neutral label over a wrong name.
        return null;
    }
    // Missing / unrecognised source: neutral label, never a guess (#1166).
    return null;
}

/**
 * Decide whether a `tool_start` (or sibling) event belongs to a DM
 * conversation, robust to `activeSession` resolution timing.
 *
 * B9 (#1154): gating the DM grouping branch purely on
 * `activeSession.value?.session_type === 'dm'` races with SSE
 * attach/reconnect. On a fresh stream open (or a backoff reconnect that
 * replays buffered events from `last_event_id`), `activeSession` may not
 * have resolved yet — `listSessions` / `loadSession` populate the
 * `sessions` / `crossAgentSessions` signals asynchronously, and a reconnect
 * can fire its first `tool_start` before that lands. When the gate reads
 * `false` mid-replay, the tool takes the NON-DM branch and gets appended as
 * a standalone tool row that never joins its `dm_reasoning` block — the
 * "sometimes tool calls render outside the collapsible" symptom.
 *
 * The fix is to OR in an event-driven signal that does not depend on session
 * metadata being resolved: if a `dm_reasoning` block already exists in
 * `chatMessages` for this run's `run_id`, the stream is unambiguously in a
 * DM run (DM reasoning blocks are only ever created on DM sessions, by
 * `run_created` / `run_started` / the lazy `tool_start` race path), so the
 * tool must group into it regardless of attach/reconnect ordering. The
 * `activeSession` check stays as the primary fast path for the common case.
 *
 * @param {string|null} runId - the event's run_id (already resolved with the
 *   `data.run_id || activeRunId.value` fallback by the caller)
 * @returns {boolean}
 */
function isDmEvent(runId) {
    if (activeSession.value?.session_type === 'dm') return true;
    if (runId) {
        // Source-driven proof (#1162): `run_created` recorded this run's
        // `peer:` source, so it is a DM run regardless of whether
        // `activeSession` has resolved or a `dm_reasoning` block exists yet.
        // This closes the attach-race window where a DM run's deltas (incl. the
        // buffered-fallback re-emit) would otherwise fall through to the non-DM
        // path and double-render against the `dm_message` bubble.
        if (peerRunSources.has(runId)) return true;
        // Event-driven fallback: a live DM reasoning block for this run is proof
        // the run is a DM run even when activeSession hasn't resolved yet.
        return chatMessages.value.some(
            m => m.type === 'dm_reasoning' && m.runId === runId
        );
    }
    return false;
}

/**
 * Map error codes to user-friendly messages.
 * Falls back to the raw message if the code is not recognised.
 */
function friendlyErrorMessage(code, rawMsg) {
    switch (code) {
        case 'AUTH':
            return 'Authentication failed -- check your API key in Settings.';
        case 'RATE_LIMIT':
            return 'Rate limited by the LLM provider -- wait a moment and try again.';
        case 'TIMEOUT':
            return 'Request timed out -- the LLM provider did not respond in time.';
        default:
            return rawMsg;
    }
}

let activeSessionEs = null;
/**
 * The sessionId the currently-open stream belongs to. Tracked at module
 * scope so the per-session reasoning-dedupe suppress-set (#1135) can be
 * cleaned up correctly: when `openSessionStream` switches to a DIFFERENT
 * session it drops the previous session's stored set, but a same-session
 * reopen (EventSource reconnect) preserves it. `closeSessionStream` clears
 * the current session's entry on genuine teardown.
 */
let activeStreamSessionId = null;
let sessionRetryCount = 0;
const MAX_SESSION_RETRIES = 10;
/**
 * In-flight backoff timer scheduled by `es.onerror` after a transient
 * network blip. Stored at module scope so a manual reconnect path
 * (`reconnectSessionStream`, banner click, `online` event) or an
 * explicit `closeSessionStream()` can cancel the pending reopen and
 * avoid a redundant double-open of an already-healthy stream
 * (see #907 review — Suggestion 1).
 */
let backoffTimer = null;
let deltaBuffer = '';
let flushTimer = null;
/**
 * The selectGeneration value captured when the current stream was opened.
 * Used by the rAF-scheduled flushDeltaBuffer() to discard buffered deltas
 * that belong to a stale stream (i.e. the user switched sessions between
 * the token_delta event and the rAF callback).
 *
 * Not checked by the synchronous closeSessionStream() flush path -- that
 * flush is intentional (draining the buffer for the current session before
 * switching away).
 */
let streamGeneration = -1;
/**
 * Whether any token_delta events were received for the current run.
 * Reset on run_created, set on token_delta. Used to suppress the
 * "(run completed)" system message for normal chat runs where the
 * streamed response is already visible.
 */
let sawTokenDelta = false;
/**
 * Highest numeric SSE event ID seen on the current stream -- used for
 * manual reconnect via `?last_event_id=<n>`.
 *
 * Only numeric IDs (from persisted session events) are stored here.
 * Ephemeral events use synthetic non-numeric IDs (e.g. "ephemeral-42")
 * that are deliberately excluded so the browser never sends them as
 * `Last-Event-Id` during native EventSource auto-reconnect.
 */
let lastSeenEventId = null;

let lastSeenStreamEpoch = null;
let sessionContractReconciler = null;

/**
 * Register the authoritative reload used after a malformed persisted frame.
 * Kept as an injected callback to avoid coupling the stream module to the
 * much larger session-loader dependency graph.
 */
export function registerSessionContractReconciler(reconciler) {
    sessionContractReconciler = reconciler;
}

function flushDeltaBuffer() {
    flushTimer = null;
    if (!deltaBuffer) return;
    const pending = deltaBuffer;
    deltaBuffer = '';
    transformMessages(prev => {
        const msgs = prev.filter(m => m.type !== 'thinking');
        const copy = [...msgs];
        const last = copy[copy.length - 1];
        if (last && last.type === 'agent' && !last.sealed) {
            copy[copy.length - 1] = { ...last, text: last.text + pending };
        } else {
            // Capture the timestamp of the first delta so the per-message
            // timestamp (#855) reflects when the agent started speaking.
            copy.push({
                id: nextMsgId(), type: 'agent', role: 'assistant',
                text: pending, sealed: false, ts: new Date().toISOString(),
            });
        }
        // Defensive check: log if tool messages were lost during buffer flush.
        // The only messages that should be removed are 'thinking' indicators.
        const prevTools = prev.filter(m => m.type === 'tool').length;
        const copyTools = copy.filter(m => m.type === 'tool').length;
        if (copyTools < prevTools) {
            console.warn('[flushDeltaBuffer] tool message count decreased:', prevTools, '->', copyTools);
        }
        return copy;
    });
}

function scheduleFlush() {
    if (flushTimer === null) {
        // Capture the generation at schedule time so the rAF callback
        // can detect if the session was switched before the frame fires.
        const gen = streamGeneration;
        // Wrap the rAF callback in batch() so the signal write inside
        // flushDeltaBuffer and any subsequent Preact re-render are
        // coalesced into a single pass.  Without batch(), the signal
        // write triggers an immediate synchronous re-render before the
        // rAF callback returns, which can cause a brief intermediate
        // visual state if another SSE event is queued right after.
        flushTimer = requestAnimationFrame(() => {
            if (gen !== selectGeneration) {
                // Session was switched between token_delta and rAF --
                // discard the buffered deltas to avoid writing to the
                // wrong session's chatMessages.
                flushTimer = null;
                deltaBuffer = '';
                return;
            }
            batch(flushDeltaBuffer);
        });
    }
}

/**
 * Promote a DM run's pending visible-reply text into its reasoning buffer.
 *
 * Called on a DM `tool_start`: any visible `token_delta` text accumulated in
 * `dmPendingReplyBuffers` since the last tool boundary preceded this tool
 * call, so it was "thinking out loud" that the runtime persists as a
 * `message_type: "reasoning"` row (`persist_assistant_tool_calls` in
 * loop_impl.rs). Committing it into `dmThinkingBuffers` keeps the live
 * collapsible in step with the reload collapsible. The pending slot is then
 * cleared so the NEXT turn's trailing text (the eventual implicit reply) is
 * tracked independently.
 *
 * No-op when there is no pending text for the run.
 *
 * @param {string|null} runId
 */
function commitDmPendingReplyToReasoning(runId) {
    if (!runId) return;
    const pending = dmPendingReplyBuffers.get(runId);
    if (!pending) return;
    dmPendingReplyBuffers.delete(runId);
    const prev = dmThinkingBuffers.value;
    const next = new Map(prev);
    next.set(runId, (next.get(runId) || '') + pending);
    dmThinkingBuffers.value = next;
}

/**
 * Discard a DM run's pending visible-reply text without committing it.
 *
 * Called on run end: the trailing visible text (everything since the last
 * tool boundary) is the implicit DM reply (#1156), already delivered to the
 * peer as the `dm_message` bubble and intentionally NOT persisted as a
 * reasoning row by the runtime. Dropping it here is what keeps the reply from
 * rendering twice live (#1157/#1162). No-op when the run has no pending text.
 *
 * @param {string|null} runId
 */
function discardDmPendingReply(runId) {
    if (!runId) return;
    dmPendingReplyBuffers.delete(runId);
}

function sealLastAgent() {
    const msgs = chatMessages.value;
    const hasThinking = msgs.some(m => m.type === 'thinking');
    const filtered = hasThinking ? msgs.filter(m => m.type !== 'thinking') : msgs;
    const last = filtered[filtered.length - 1];
    if (last && last.type === 'agent' && !last.sealed) {
        transformMessages(() => {
            const updated = [...filtered];
            updated[updated.length - 1] = { ...last, sealed: true };
            // Defensive check: log if tool messages were lost during seal.
            const prevTools = msgs.filter(m => m.type === 'tool').length;
            const updatedTools = updated.filter(m => m.type === 'tool').length;
            if (updatedTools < prevTools) {
                console.warn('[sealLastAgent] tool message count decreased:', prevTools, '->', updatedTools);
            }
            return updated;
        });
    } else if (hasThinking) {
        transformMessages(() => {
            // Defensive check: log if tool messages were lost during thinking removal.
            const prevTools = msgs.filter(m => m.type === 'tool').length;
            const filteredTools = filtered.filter(m => m.type === 'tool').length;
            if (filteredTools < prevTools) {
                console.warn('[sealLastAgent] tool message count decreased:', prevTools, '->', filteredTools);
            }
            return filtered;
        });
    }
}

/**
 * Open a persistent session-level SSE stream.
 * Stays open across runs -- all events for this session arrive here.
 *
 * @param {string} sessionId
 * @param {{ lastEventId?: number, streamEpoch?: string, sealedReasoningRunIds?: Set<string> }} [opts]
 *   Replay cursor, stream epoch, and load-time reasoning dedupe state.
 */
export function openSessionStream(sessionId, opts) {
    const requestedStreamEpoch = opts && opts.streamEpoch != null
        ? String(opts.streamEpoch)
        : null;
    // Recover the reasoning-dedupe suppress-set (#1135) BEFORE the internal
    // `closeSessionStream()` below clears the per-session store, so a
    // same-session EventSource reconnect (which reopens via this function
    // without an `opts.sealedReasoningRunIds`) does not lose the set. A fresh
    // `loadSession` for the same session supplies a new set in `opts` and
    // supersedes this. For a session SWITCH (different `sessionId`) we ignore
    // the prior session's set — its entry is dropped by `closeSessionStream`
    // tearing down the old `activeStreamSessionId`.
    const carriedSealedReasoning = (sessionId && (!opts || !(opts.sealedReasoningRunIds instanceof Set)))
        ? getSealedReasoningRunIds(sessionId)
        : null;

    // Carry the per-run pending DM reply buffers across a same-session
    // EventSource reconnect, mirroring the `carriedSealedReasoning` pattern
    // immediately above (#1157/#1162 follow-up). The reconnect paths
    // (auto-backoff `onerror`, manual `reconnectSessionStream`) reopen with
    // `{ lastEventId }` only and resume from the numeric event cursor — but
    // `token_delta` is ephemeral and is NOT replayed by that cursor. So if a
    // reconnect lands AFTER pre-tool "thinking out loud" text streamed but
    // BEFORE the `tool_start` that promotes it into the collapsible, the
    // unconditional `dmPendingReplyBuffers.clear()` in `closeSessionStream`
    // below would drop that not-yet-promoted text — lost from the live view
    // until a full reload reconstructs it from the persisted reasoning row.
    // Pre-#1157 that text lived in `dmThinkingBuffers`, which already survives
    // a reconnect via `carriedSealedReasoning`; moving it to a buffer that the
    // close clears reintroduced a live-not-equal-reload gap on the reconnect
    // path. Recovering it here (BEFORE the internal close clears the map) and
    // re-installing it after the reopen closes that gap.
    //
    // Same-session ONLY: gated on `sessionId === activeStreamSessionId` (the
    // module-scope id still holds the PREVIOUS stream's session at this point,
    // reassigned further down). On a session SWITCH the pending text belongs to
    // the session being left and must not bleed into the new session's runs, so
    // it is intentionally dropped by the close. The `opts`-has-no-fresh-
    // `sealedReasoningRunIds` clause matches `carriedSealedReasoning`: a fresh
    // `loadSession` (which DOES pass that set) reconstructs any pre-tool text
    // from the persisted reasoning row, so carrying a stale live buffer over a
    // reload would double-count — only the in-process reconnect carries.
    //
    // CRITICAL (#1157 must not regress): this carries the text as STILL-PENDING
    // — it is re-seeded into `dmPendingReplyBuffers`, never committed into
    // `dmThinkingBuffers`. The same `tool_start`-promote / run-end-discard rules
    // then apply post-reconnect, so a trailing implicit reply that survives the
    // reconnect is still discarded at run end and never sealed into the
    // collapsible. Committing-on-close instead would seal the reply into the
    // collapsible on any reconnect that lands after the last tool — exactly the
    // #1157 double-render — so the carry-over (not a flush) is the only correct
    // shape.
    const carriedPendingReplies = (sessionId && sessionId === activeStreamSessionId
        && (!opts || !(opts.sealedReasoningRunIds instanceof Set)))
        ? new Map(dmPendingReplyBuffers)
        : null;

    // Carry the DM/peer run-source map across a same-session reconnect for the
    // same reason (#1162): it drives `isDmEvent`, and `run_created` (which
    // populates it from the `peer:` source) is non-ephemeral but may sit at or
    // before the reconnect cursor, so it would NOT replay. Losing it on a
    // reconnect that lands mid-run would re-expose the attach-race fall-through
    // for the run's remaining deltas (incl. the buffered-fallback re-emit) and,
    // since #1166, would also cost a still-queued run the source `run_started`
    // labels its collapsible with.
    // Same gating as the pending-reply carry-over: same-session, no fresh load
    // (a fresh `loadSession` replays `run_created` from history, repopulating it).
    const carriedPeerRunSources = (sessionId && sessionId === activeStreamSessionId
        && (!opts || !(opts.sealedReasoningRunIds instanceof Set)))
        ? new Map(peerRunSources)
        : null;

    closeSessionStream();
    if (!sessionId) return;

    // Cancel any pending backoff reopen scheduled by a previous
    // onerror — the manual-reconnect path (banner click, `online`
    // event, or this fresh open) supersedes it. Without this guard
    // the in-flight setTimeout would still fire a few seconds later
    // and double-open the now-healthy stream (#907 review,
    // Suggestion 1). closeSessionStream() above already cancels via
    // the same guard, so this site is belt-and-braces — it pins the
    // contract at the obvious entry point so a future caller that
    // bypasses closeSessionStream cannot regress the cancel.
    if (backoffTimer !== null) {
        clearTimeout(backoffTimer);
        backoffTimer = null;
    }

    const token = localStorage.getItem('alms_auth_token');
    const params = new URLSearchParams();
    if (token) params.set('token', token);
    if (opts && opts.lastEventId != null) params.set('last_event_id', String(opts.lastEventId));
    if (requestedStreamEpoch) params.set('stream_epoch', requestedStreamEpoch);
    const qs = params.toString();
    const url = `/sessions/${sessionId}/events${qs ? '?' + qs : ''}`;
    const es = new EventSource(url);
    activeSessionEs = es;
    activeStreamSessionId = sessionId;
    sessionRetryCount = 0;
    lastSeenEventId = (opts && opts.lastEventId != null) ? opts.lastEventId : null;

    lastSeenStreamEpoch = requestedStreamEpoch;
    // Reasoning-dedupe suppress-set (#1135). A fresh `loadSession` passes the
    // freshly-built set in `opts.sealedReasoningRunIds`; record it under this
    // session id so the reconnect paths (auto-backoff `onerror`, manual
    // `reconnectSessionStream`) can recover it after the originating `opts`
    // object is gone. Both reconnect paths call `openSessionStream` WITHOUT a
    // suppress-set — for those reopens `carriedSealedReasoning` (recovered
    // above, before the internal close cleared the store) is re-recorded so
    // the set survives the reconnect rather than being lost. The Layer-3 guard
    // below reads the recovered set (not `opts`) so it keeps firing across
    // reconnects within the stream's lifetime.
    if (opts && opts.sealedReasoningRunIds instanceof Set) {
        setSealedReasoningRunIds(sessionId, opts.sealedReasoningRunIds);
    } else if (carriedSealedReasoning instanceof Set) {
        setSealedReasoningRunIds(sessionId, carriedSealedReasoning);
    }
    const sealedReasoningRunIds = getSealedReasoningRunIds(sessionId);

    // Re-install the per-run pending DM reply buffers recovered above, so a
    // same-session reconnect preserves the not-yet-promoted pre-tool text that
    // `closeSessionStream` just cleared (#1157/#1162 follow-up). Re-seeded as
    // STILL-PENDING (into `dmPendingReplyBuffers`, never `dmThinkingBuffers`),
    // so the `tool_start`-promote / run-end-discard rules still apply post-
    // reconnect and a trailing implicit reply is never sealed into the
    // collapsible. Mirrors the `carriedSealedReasoning` re-record above; the
    // recovery already gated on same-session + no-fresh-load, so this is an
    // unconditional repopulate of whatever was carried. Per-run keys are
    // preserved verbatim, so overlapping-run isolation is intact.
    if (carriedPendingReplies instanceof Map) {
        for (const [runId, text] of carriedPendingReplies) {
            dmPendingReplyBuffers.set(runId, text);
        }
    }
    // Re-install the carried DM/peer run-source map (mirrors the pending-reply
    // re-seed above) so `isDmEvent` keeps classifying the run's deltas as DM
    // across the reconnect (#1162).
    if (carriedPeerRunSources instanceof Map) {
        for (const [runId, source] of carriedPeerRunSources) {
            peerRunSources.set(runId, source);
        }
    }
    // Defer clearing the dead-state flag until the connection
    // actually opens — see the `'open'` listener below. Constructing
    // an EventSource is optimistic and does not mean the server is
    // reachable; if we cleared synchronously here, a manual
    // reconnect that hit a still-broken backend would briefly drop
    // the banner and then re-show it on the next exhaustion cycle
    // (~30 s), which reads as "I clicked it and it lied to me" to
    // the operator (#907 review, Suggestion 2).
    es.addEventListener('open', () => {
        if (es !== activeSessionEs) return;
        clearStreamDead('session');
    });

    // Capture the current selectGeneration so SSE handlers can detect
    // stale events from a previous session's stream.  If the user
    // switches sessions (which bumps selectGeneration), all handlers
    // on this stream become no-ops.
    streamGeneration = selectGeneration;

    /**
     * Set of SSE event IDs already processed on this stream.
     *
     * When the browser's native EventSource auto-reconnect replays events
     * (e.g. after a transient network blip), the handlers would otherwise
     * run a second time, causing duplicate chat messages or visual flashes.
     * Tracking seen IDs makes every handler idempotent against replays.
     *
     * Ephemeral IDs (prefixed "ephemeral-") are excluded from this set
     * because they are never reused and would only waste memory.
     *
     * Bounded to SEEN_IDS_MAX entries to prevent unbounded memory growth
     * on long-lived connections (see #510).  When the cap is exceeded the
     * oldest ~20% of entries are evicted.  Since Set preserves insertion
     * order, the evicted entries are the oldest IDs -- recent IDs (which
     * a browser auto-reconnect replay would contain) remain in the set.
     * The cap is set to 2500 to provide good coverage of the server's
     * 5000-event replay window (SESSION_EVENT_LOG_MAX).
     */
    const SEEN_IDS_MAX = 2500;
    const seenEventIds = new Set();
    let contractRecovering = false;

    const reconcileFromSnapshot = async (eventId, streamEpoch, reason, error = null) => {
        if (contractRecovering || streamGeneration !== selectGeneration) return;
        contractRecovering = true;
        es.close();
        if (error) {
            console.error('[session-events] rejected live event; reconciling:', error);
        } else {
            console.warn('[session-events] authoritative reconciliation:', reason);
        }

        if (!sessionContractReconciler) {
            markStreamDead('session');
            return;
        }

        try {
            // Reload REST snapshots before reopening. The malformed frame is
            // used as the minimum cursor so it is not replayed forever; all
            // later persisted frames replay onto the authoritative snapshot.
            await sessionContractReconciler(sessionId, eventId, streamEpoch);
        } catch (reconcileError) {
            console.error('[session-events] contract reconciliation failed:', reconcileError);
            markStreamDead('session');
        }
    };

    const recoverFromContractViolation = (eventId, error) =>
        reconcileFromSnapshot(eventId, lastSeenStreamEpoch, 'contract violation', error);

    /**
     * Wrap an event handler to track the highest seen SSE event ID and
     * deduplicate replayed events.
     *
     * Only numeric IDs (from the session event log) are tracked for
     * reconnect. Ephemeral events (token_delta, status) use synthetic
     * non-UUID IDs (e.g. "ephemeral-42") that the backend will not
     * accept as a replay cursor.
     */
    const on = (type, handler) => es.addEventListener(type, (e) => {
        // Generation guard: discard events from a stale stream.
        // When the user switches sessions, bumpSelectGeneration() is
        // called before the new stream opens.  Any events still arriving
        // from the old EventSource (before it fully closes) will have a
        // streamGeneration that no longer matches selectGeneration.
        if (streamGeneration !== selectGeneration) return;

        const id = e.lastEventId;
        const numericId = id && /^\d+$/.test(id) ? Number(id) : null;
        const shouldRemember = id && !id.startsWith('ephemeral-');
        if (shouldRemember && seenEventIds.has(id)) return;

        try {
            const contracts = globalThis.__almsContracts;
            if (!contracts) throw new Error('Frontend contract bridge is not installed');
            const validated = contracts.parseSseJsonPayload(type, e.data);
            handler({ data: validated, lastEventId: e.lastEventId });

            // Commit replay bookkeeping only after validation and state
            // mutation succeed. Otherwise a malformed frame would be skipped
            // forever while leaving the UI at its pre-event state.
            if (numericId != null) lastSeenEventId = id;
            if (shouldRemember) seenEventIds.add(id);

            if (seenEventIds.size > SEEN_IDS_MAX) {
                const evictCount = Math.floor(SEEN_IDS_MAX * 0.2);
                let i = 0;
                for (const oldId of seenEventIds) {
                    if (i++ >= evictCount) break;
                    seenEventIds.delete(oldId);
                }
                console.debug('[sse-dedup] evicted', evictCount, 'stale IDs, size:', seenEventIds.size);
            }
        } catch (err) {
            const recoveryCursor = numericId != null
                ? numericId
                : (/^\d+$/.test(String(lastSeenEventId)) ? Number(lastSeenEventId) : null);
            void recoverFromContractViolation(recoveryCursor, err);
        }
    });

    const eventCursor = (event) => /^\d+$/.test(event.lastEventId)
        ? Number(event.lastEventId)
        : null;
    on('stream_state', (e) => {
        const data = e.data;
        const previousEpoch = lastSeenStreamEpoch;
        const epochChanged = Boolean(
            previousEpoch
            && data.stream_epoch
            && previousEpoch !== data.stream_epoch
        );
        if (data.stream_epoch) {
            lastSeenStreamEpoch = data.stream_epoch;
        }
        if (data.requires_reconciliation || epochChanged) {
            const hasNewest = Number.isSafeInteger(data.newest);
            const boundary = hasNewest
                ? data.newest
                : (epochChanged ? null : eventCursor(e));
            void reconcileFromSnapshot(
                boundary,
                data.stream_epoch || lastSeenStreamEpoch,
                epochChanged ? 'stream epoch changed' : 'replay gap',
            );
        }
    });

    // -- run_created: a new run was created on this session --
    on('run_created', (e) => {
        const data = e.data;
        const queuedBehind = data.queued_behind;
        upsertRun(
            {
                run_id: data.run_id,
                session_id: data.session_id,
                status: 'queued',
                ...(queuedBehind > 0 ? { queue_position: queuedBehind } : {}),
            },
            eventCursor(e),
            lastSeenStreamEpoch,
        );
        sawTokenDelta = false;
        bumpRunListGeneration();

        // Cross-channel DM awareness: when the run source starts with
        // "peer:", the agent is responding to a DM from another agent.
        // Set the DM context so the header bar shows "Chatting with
        // {peer}..." as the fallback phase.  More specific phases (tool
        // execution) will temporarily override it, reverting when done.
        const isDm = activeSession.value?.session_type === 'dm';
        const isPeerSource = !!(data.source && data.source.startsWith('peer:'));
        if (isPeerSource) {
            setDmContext(data.source.slice(5));
            // Record the run as a DM run so `isDmEvent` classifies its deltas
            // correctly even before `activeSession` resolves or a `dm_reasoning`
            // block exists (#1162 attach race). The contracted run_id is its stable key.
            // The source is retained (not just the id) so a run that is still
            // QUEUED here can be attributed when `run_started` fires later,
            // without depending on its thinking row having survived (#1166).
            peerRunSources.set(data.run_id, data.source);
        }

        // Take the DM block-creation path whenever this run is known to be a DM
        // run, even if `activeSession` has not resolved to a DM yet (#1162
        // attach race). A peer DM `run_created` ALWAYS carries both
        // `source: "peer:<name>"` AND `is_notification: true` (notifications.rs
        // — `enqueue_triggered_run` for `MessageSource::Agent`), while a non-DM
        // notification run (subagent completion) is `is_notification: true` with
        // a NON-`peer:` source. So the `peer:` prefix is the unambiguous
        // discriminator that keeps subagent/job notifications on the thinking-
        // indicator branch below while routing genuine DM runs here. Without
        // this, an attach-race peer run fell to the `is_notification` branch and
        // got a bare `thinking` (queuedBehind:0) row instead of a `dm_reasoning`
        // block: live `reasoning_delta` (bucketed by `isDmEvent` via `peerRunSources`)
        // then had no block to render into, and `run_finished` had no block to
        // seal — so the run's reasoning was dropped live until a reload rebuilt
        // it from history.
        const isDmRun = isDm || isPeerSource;

        if (isDmRun) {
            // DM sessions with queued runs: show a thinking indicator with
            // queue state instead of a live reasoning block. The reasoning
            // block will be created when run_started fires. (#691)
            if (queuedBehind > 0) {
                batch(() => {
                    // Header bar is NOT updated here -- it should keep
                    // showing the agent's current activity (e.g. a DM with
                    // another peer).  The inline thinking indicator handles
                    // the queued state via queuedBehind. (#693)
                    //
                    // runId is stored on the thinking message so the
                    // `run_queue_position` SSE handler (#831) can locate
                    // the right indicator to decrement when an upstream
                    // run completes.
                    appendMessage({
                        id: nextMsgId(), type: 'thinking', source: data.source,
                        queuedBehind, runId: data.run_id,
                    });
                });
            } else {
                // DM sessions: insert a live reasoning block entry instead of
                // a thinking indicator. The block collects tool calls and
                // thinking text as they arrive, then is sealed on run end.
                // Derive the reasoning agent's name from the source field:
                // "peer:<name>" means <name> sent a message and the OTHER
                // participant is doing the reasoning.  Fall back to the
                // active agent for non-peer sources (e.g. user-initiated).
                // Fixes #692 — was using activeAgent which is always the
                // UI-selected agent, not necessarily the one reasoning.
                const agentName = dmReasoningAgentName(data.source);
                batch(() => {
                    appendMessage({
                        id: nextMsgId(),
                        type: 'dm_reasoning',
                        runId: data.run_id,
                        agentName: agentName,
                        thinkingText: '',
                        tools: [],
                        status: 'running',
                        isLive: true,
                    });
                });
            }
        } else if (data.is_notification) {
            // Notification run from subagent completion or peer message --
            // show thinking indicator with source context. The inline
            // indicator handles queue state via queuedBehind; the header
            // bar keeps showing the agent's current activity. (#693)
            //
            // runId is stored on the thinking message so the
            // `run_queue_position` SSE handler (#831) can locate the right
            // indicator to decrement when an upstream run completes.
            batch(() => {
                appendMessage({
                    id: nextMsgId(), type: 'thinking', source: data.source,
                    queuedBehind, runId: data.run_id,
                });
            });
        } else if (queuedBehind > 0) {
            // User-initiated run but agent is busy -- update the existing
            // thinking indicator (added by startRun) with queue position.
            // Header bar keeps its current state (the agent's real
            // activity); the inline indicator shows queue status. (#693)
            // Clear `pending` so it transitions from "Sending..." to
            // "Queued..." immediately. (#704)
            //
            // runId is stamped onto the thinking message so the
            // `run_queue_position` SSE handler (#831) can locate the right
            // indicator to decrement when an upstream run completes.
            batch(() => {
                updateMessage(
                    m => m.type === 'thinking' && m.pending,
                    m => ({ ...m, queuedBehind, pending: false, runId: data.run_id }),
                );
            });
        } else {
            // User-initiated, queue empty -- clear `pending` so the
            // indicator transitions from "Sending..." to "Thinking...".
            // (#704)
            batch(() => {
                updateMessage(
                    m => m.type === 'thinking' && m.pending,
                    m => ({ ...m, pending: false }),
                );
            });
        }
    });

    // -- run_started: the run has been dequeued and is now executing --
    on('run_started', (e) => {
        const data = e.data;
        upsertRun(
            {
                run_id: data.run_id,
                session_id: data.session_id,
                status: 'running',
                queue_position: undefined,
            },
            eventCursor(e),
            lastSeenStreamEpoch,
        );
        // Gate via `isDmEvent` (run_id-aware) rather than a bare `activeSession`
        // read, mirroring the delta / run_finished handlers (#1162 attach race).
        // `run_started` carries no `source`, so it cannot re-detect `peer:` on
        // its own — but `run_created` already recorded this run in
        // `peerRunSources`, which `isDmEvent` ORs in. Without this, a QUEUED
        // peer DM run whose `activeSession` is still unresolved at run_started
        // time would fall to the non-DM `else` branch and never get its
        // `dm_reasoning` block (the
        // queued-run analogue of the non-queued `run_created` drop fixed above).
        const isDm = isDmEvent(data.run_id);

        if (isDm) {
            // DM session: replace the queued thinking indicator with a
            // live reasoning block now that the run is actually executing,
            // labelled with the agent doing the reasoning.
            // Fixes #692 — was using activeAgent which is always the
            // UI-selected agent, not necessarily the one reasoning.
            //
            // #1166 — three inputs, every one of them keyed to THIS run:
            //
            //  1. `peerRunSources`: the `peer:<name>` source THIS run's
            //     `run_created` carried, held off-render so no sweep can
            //     reach it. Absent only when `run_created` predates the
            //     stream (the operator opened the DM mid-conversation) or
            //     was dropped by a reconnect cursor.
            //  2. `thinkingMsg.agentName`: the run's OWNING AGENT, taken by
            //     `loadSession` step 3 straight off the run record's
            //     `agent_id`. This is the stronger input of the two — a wire
            //     fact with no derivation, where term 1 is a `peer:` name put
            //     through `dmParticipants`. It is ranked SECOND anyway because
            //     it exists only on the reload path, so leading with term 1
            //     keeps the ordinary live turn resolving on one term instead
            //     of falling through every time. In a two-participant DM they
            //     cannot disagree, and term 1's one failure mode (a `peer:`
            //     name that is not a participant) now returns null rather
            //     than a wrong name, so it can no longer shadow this term.
            //  3. `thinkingMsg.source`: legacy shape, kept so a row stamped by
            //     an older frontend build still attributes.
            //
            // The previous code had only (3), and read it off "the FIRST
            // queued thinking row" rather than this run's. That failed two ways:
            // a second concurrently-queued run derived its name from the first
            // run's source, and `sealLastAgent` (called by every run end and
            // every `dm_message`) sweeps queued rows run-id-blind, so a run
            // queued behind a busy agent reached `run_started` with no row at
            // all. Both fell through to the UI-selected `activeAgent`.
            //
            // The sweep below is likewise keyed on this run's `run_id` so
            // starting one run no longer deletes another's queue chip (which
            // also left `run_queue_position` (#831) nothing to decrement).
            // Both producers of a queued DM thinking row stamp `runId` (the
            // `run_created` handler above and `loadSession` step 3), so
            // routing by run_id has no un-stamped rows to strand.
            //
            // DECLARED RESIDUAL — this closes the ATTRIBUTION consequence, not
            // the whole chip loss. `sealLastAgent` (and `flushDeltaBuffer`)
            // still drop EVERY `thinking` row unconditionally, on every
            // `dm_message` and every run end, so a run that is still
            // legitimately queued keeps losing its position chip and
            // `run_queue_position` still finds nothing to update. Attribution
            // no longer depends on that row surviving — that is what
            // `peerRunSources` above is for — but the #831 chip itself is
            // still swept. Fixing it means scoping two shared, non-DM helpers
            // and adding a run-id-scoped removal at run end so a
            // cancelled-before-dispatch run does not strand a chip; that is a
            // #831 change with non-DM blast radius, deliberately not smuggled
            // into a DM attribution fix. Tracked separately (see #1321).
            const thinkingMsg = chatMessages.value.find(
                m => m.type === 'thinking' && m.queuedBehind > 0
                    && m.runId === data.run_id
            );
            const agentName = dmReasoningAgentName(peerRunSources.get(data.run_id))
                || thinkingMsg?.agentName
                || dmReasoningAgentName(thinkingMsg?.source);
            transformMessages(msgs => {
                const filtered = msgs.filter(m => !(
                    m.type === 'thinking' && m.queuedBehind > 0
                    && m.runId === data.run_id
                ));
                // Only add reasoning block if one does not already exist
                // for this run (it may already exist if run was not queued).
                if (!filtered.some(m => m.type === 'dm_reasoning' && m.runId === data.run_id)) {
                    filtered.push({
                        id: nextMsgId(),
                        type: 'dm_reasoning',
                        runId: data.run_id,
                        agentName: agentName,
                        thinkingText: '',
                        tools: [],
                        status: 'running',
                        isLive: true,
                    });
                }
                return filtered;
            });
        } else {
            // Non-DM: transition thinking indicator from "queued" to
            // active "Thinking..."
            updateMessage(
                m => m.type === 'thinking' && m.queuedBehind > 0,
                m => ({ ...m, queuedBehind: 0 }),
            );
        }

        // Set header bar to the appropriate active phase now that the
        // run is executing. DM runs with a peer get "Chatting with...",
        // others get "Thinking..." until the first real status event
        // arrives. (#691)
        if (dmPeer.value) {
            setAgentPhase('dm', dmPeer.value);
        } else {
            setAgentPhase('calling_llm', null);
        }
        bumpRunListGeneration();
    });

    // -- run_queue_position: live queue-position decrement (#831) --
    //
    // Fires every time a run ahead of this one in the per-agent queue
    // finishes, fails, or is cancelled. `position` is 1-indexed and
    // matches the semantic of `run_created.queued_behind` ("1 = next up").
    // Position 0 is never emitted -- the existing `run_started` handler
    // clears the chip when this run is dequeued.
    //
    // The run is identified by `data.run_id`, which both the `run_created`
    // SSE handler and the `loadSession` reload path stamp onto the
    // thinking-indicator message. The contracted run_id provides exact routing.
    on('run_queue_position', (e) => {
        const data = e.data;
        const position = data.position;
        upsertRun(
            {
                run_id: data.run_id,
                session_id: data.session_id,
                status: 'queued',
                queue_position: position,
            },
            eventCursor(e),
            lastSeenStreamEpoch,
        );
        updateMessage(
            m => m.type === 'thinking' && m.queuedBehind > 0
                && m.runId === data.run_id,
            m => ({ ...m, queuedBehind: position }),
        );
    });

    // -- status: agent phase update (live indicator in header bar) --
    // Note: no source_agent filter needed -- subagent status events are
    // routed to their own session streams, not the parent's.  If that
    // routing ever changes, the header bar would flash subagent phases
    // and a source_agent guard would need to be added here.
    on('status', (e) => {
        const data = e.data;
        console.debug('[status]', data.phase, data.detail || '');

        // During DM runs, ALL phases are replaced with "Chatting with
        // {peer}..." (#688).  The internal tool use within a DM is
        // irrelevant to the top-level status -- "Chatting with..." should
        // persist until the DM conversation ends. Tool-level detail is
        // visible in the DM reasoning blocks within the DM session view.
        if (dmPeer.value) {
            setAgentPhase('dm', dmPeer.value);
            return;
        }
        setAgentPhase(data.phase, data.detail || null);
    });

    // -- token_delta --
    on('token_delta', (e) => {
        const data = e.data;
        if (data.source_agent) return; // suppress subagent interleaving
        // S3 (#1154): gate via `isDmEvent` (run_id-aware), not a bare
        // `activeSession` read. On a reconnect that replays buffered events
        // from `last_event_id`, `activeSession` may not have resolved yet, so
        // a bare check misroutes replayed DM `token_delta`s into the non-DM
        // path — they land in `deltaBuffer` and flush as a standalone ghost
        // bubble (the #685 symptom). `isDmEvent` ORs in the event-driven
        // proof (a live `dm_reasoning` block for this run) so the delta stays
        // on the DM path regardless of attach/reconnect ordering.
        const isDm = isDmEvent(data.run_id);
        if (isDm) {
            // INVARIANT (cross-layer, #1156 Option C): treating EVERY DM
            // `token_delta` as the discard-on-end pending reply is correct only
            // because every DM run is peer-triggered with an implicit reply
            // delivered as a `dm_message` bubble. `POST /runs` on any `dm:`
            // session is unconditionally rejected with
            // `DM_SESSION_NOT_DIRECTLY_RUNNABLE` (runs/lifecycle.rs ~L834), so
            // there is no user-/directly-initiated DM run whose visible response
            // would be silently dropped here with no bubble to replace it. If
            // that gate is ever relaxed (direct DM runs become possible), this
            // branch MUST be re-gated on a per-run peer/implicit-reply flag —
            // do NOT "fix" the absence of a bubble for a hypothetical non-peer
            // DM run by removing this buffering; that non-bug cannot occur today.
            //
            // For DM sessions: visible reply text does NOT go straight into the
            // reasoning collapsible. Under implicit replies (#1156) the run's
            // trailing visible text IS the reply, delivered as the `dm_message`
            // bubble — painting it in the collapsible too double-rendered it
            // (#1157), mis-attributed it to the wrong agent before participants
            // resolved (#1162 sym-1), and showed partial-then-full mid-stream
            // (#1162 sym-2). Buffer it as the *pending* reply instead; a later
            // `tool_start` promotes it into `dmThinkingBuffers` (it was
            // intermediate "thinking out loud" the runtime persists as
            // reasoning), and run end discards it (it was the implicit reply).
            //
            // B8 (#1154): bucket strictly by the event's own `run_id` rather
            // than `activeRunId.value`. Two DM runs can overlap, while the
            // derived active run can change whenever a queued run is created
            // or either run reaches a terminal state. Keying on that mutable
            // selection would land one agent's
            // text in the other agent's collapsible. The wire-level `run_id`
            // is the authoritative owner of the delta.
            const runId = data.run_id;
            dmPendingReplyBuffers.set(
                runId,
                (dmPendingReplyBuffers.get(runId) || '') + data.delta,
            );
            return;
        }
        sawTokenDelta = true;
        deltaBuffer += data.delta;
        scheduleFlush();
    });

    // -- reasoning_delta (issue #767) --
    //
    // Provider-neutral extended-thinking stream. Today only Anthropic's
    // `thinking_delta` emits these; #768/#769 will add OpenAI/Gemini.
    // Accumulate per-run reasoning text into a buffer attached to the
    // live agent message so `ReasoningPanel` can render it collapsed.
    on('reasoning_delta', (e) => {
        const data = e.data;
        if (data.source_agent) {
            // Subagent reasoning is suppressed from the PARENT's main chat /
            // reasoning view — it must never leak into the parent run's
            // reasoning trace (the #1170 / `get_run_reasoning` invariant) or
            // interleave into the parent's collapsible. The backend no longer
            // forwards subagent reasoning to the parent at all (the Subagent
            // status bar is driven by the coarse `subagent_activity` signal
            // instead, and the full text streams to the subagent's own
            // session, #1184) — this guard remains for tagged deltas replayed
            // from pre-status-bar session event logs.
            return;
        }
        const delta = data.text || '';
        if (!delta) return;
        // S3 (#1154): gate via `isDmEvent` (run_id-aware) for the same
        // reconnect-replay reason as the token_delta handler above — a bare
        // `activeSession` read would misroute a replayed DM `reasoning_delta`
        // into the non-DM path and spawn a spurious unsealed bubble.
        const isDm = isDmEvent(data.run_id);
        if (isDm) {
            // For DM sessions: reasoning IS the canonical collapsible content,
            // so accumulate it directly into `dmThinkingBuffers` (unlike the
            // visible `token_delta` reply, which is the bubble and is buffered
            // separately in `dmPendingReplyBuffers`). The DmReasoningBlock for
            // the active run reads this buffer and displays the text inside the
            // collapsible "thinking" pane. This matches the reload render,
            // where the runtime-persisted extended-thinking trace (`finish_run`
            // in mod.rs) becomes the block's thinkingText.
            //
            // Without this branch, the fallback path below would push a
            // brand-new agent placeholder message with empty `text` and
            // only a `reasoning` field — which DmConversationView routes
            // through DmMessage, producing an empty right-side bubble that
            // never gets content (the canonical message text arrives via
            // `dm_message`, on a separate row).  The empty bubble persists
            // until reload re-fetches history (where reasoning is grouped
            // into dm_reasoning blocks via groupDmReasoningBlocks and
            // never materialises as a stand-alone agent message). (#849)
            //
            // B8 (#1154): bucket by the event's own `run_id`, not the mutable
            // `activeRunId.value`. See the token_delta handler above — two
            // overlapping DM runs would otherwise cross-contaminate each
            // other's collapsible.
            const runId = data.run_id;
            const prev = dmThinkingBuffers.value;
            const next = new Map(prev);
            next.set(runId, (next.get(runId) || '') + delta);
            dmThinkingBuffers.value = next;
            return;
        }
        // Load-time terminal-scoped dedupe guard (#1133, Layer 3). For a run
        // whose final-turn reasoning is already sealed into history, its
        // trailing `reasoning_delta` events replay on stream open and would
        // double-render. `loadSession` records those run-ids in
        // `opts.sealedReasoningRunIds` (see its build site for the coverage
        // gating). Drop the replayed delta before it reaches EITHER sub-branch
        // below (append-to-tail and new-bubble), so it cannot create a second
        // unsealed bubble or land on a still-live run's tail. A live run is
        // never in the set, so its fresh deltas pass through untouched.
        //
        // #1135: the guard reads `sealedReasoningRunIds` recovered from the
        // per-session store at stream-open time rather than `opts` directly,
        // so it survives EventSource reconnects. The `on()` wrapper advances
        // `lastSeenEventId` before this handler runs, so a reconnect cursor
        // can land mid-replay of a sealed run's trailing deltas; without the
        // recovered set those re-replayed deltas would slip past the guard and
        // create a spurious unsealed bubble that persists until the next
        // reload (the run is terminal, so no future `run_finished` reseals it).
        if (data.run_id && sealedReasoningRunIds
            && sealedReasoningRunIds.has(data.run_id)) {
            return;
        }
        transformMessages(prev => {
            // Drop any transient "thinking" indicator like token_delta does,
            // so the reasoning panel doesn't race with the pre-stream
            // placeholder.
            const msgs = prev.filter(m => m.type !== 'thinking');
            const copy = [...msgs];
            const last = copy[copy.length - 1];
            if (last && last.type === 'agent' && !last.sealed) {
                copy[copy.length - 1] = {
                    ...last,
                    reasoning: (last.reasoning || '') + delta,
                };
            } else {
                // No live agent message yet — create a reasoning-only
                // placeholder so the thinking panel is visible immediately.
                copy.push({
                    id: nextMsgId(),
                    type: 'agent',
                    role: 'assistant',
                    text: '',
                    reasoning: delta,
                    sealed: false,
                    // Per-message timestamp (#855) — captured at the start
                    // of the assistant turn (first reasoning delta).
                    ts: new Date().toISOString(),
                });
            }
            return copy;
        });
    });

    // -- stream_reset (#1162 sym-2) --
    //
    // The run's LLM call painted a partial via `token_delta` / `reasoning_delta`,
    // then its stream faulted and the runtime fell back to a buffered
    // `complete()`. The buffered FULL response is about to re-stream (the
    // runtime re-emits it as fresh deltas immediately after this event), and
    // for a DM run it is also delivered as the `dm_message` bubble. Without
    // discarding the abandoned partial it lingers as the cut-off-then-full
    // duplicate (minimax-m3 on OpenRouter — same provider as #1163). Drop every
    // surface the partial could have landed in for THIS run so the re-emit
    // rebuilds a single clean render that matches reload (live === reload, the
    // #1164 invariant).
    on('stream_reset', (e) => {
        const data = e.data;
        // Subagent stream resets never reach here (the coordinator suppresses
        // them — the parent paints no subagent content at all now: the status
        // bar renders only coarse `subagent_activity` labels, so there is no
        // subagent partial to retract, #1186). Keep the `source_agent` guard
        // so a future forwarding change can't clear the PARENT's partial by
        // mistake.
        if (data.source_agent) return;
        const runId = data.run_id;
        batch(() => {
            // 1. Visible reply partial (DM path): the per-run pending buffer the
            //    `token_delta` handler accumulates. The re-emitted token_delta
            //    refills it; run end discards it (it is the implicit reply).
            dmPendingReplyBuffers.delete(runId);

            // 2. Partial reasoning painted into the live DM collapsible. Clear
            //    only this run's bucket — the re-emitted reasoning_delta refills
            //    it. The `dm_reasoning` block entry in chatMessages stays (it is
            //    the collapsible container, sealed on run end), so the live
            //    block simply shows empty thinking until the re-emit lands.
            if (dmThinkingBuffers.value.has(runId)) {
                const next = new Map(dmThinkingBuffers.value);
                next.delete(runId);
                dmThinkingBuffers.value = next;
            }

            // 3. Non-DM partial: the unflushed `deltaBuffer` plus the live
            //    unsealed agent bubble it flushes into (its `text` came from the
            //    abandoned `token_delta`s and its `reasoning` from the abandoned
            //    `reasoning_delta`s when this run fell through the non-DM path
            //    while `activeSession` was unresolved). Drop the buffer and the
            //    trailing unsealed agent message so the re-emitted token_delta
            //    starts a fresh bubble. A sealed bubble (a prior, completed
            //    turn) is never touched.
            deltaBuffer = '';
            transformMessages(prev => {
                const copy = [...prev];
                const last = copy[copy.length - 1];
                if (last && last.type === 'agent' && !last.sealed) {
                    copy.pop();
                    return copy;
                }
                return prev;
            });
        });
    });

    // -- tool_start --
    on('tool_start', (e) => {
        batch(() => {
            flushDeltaBuffer();
            const data = e.data;
            const toolId = data.tool_invocation_id;
            const runId = data.run_id;
            // Diagnostic: log tool count before insertion for #501 investigation.
            const toolCountBefore = chatMessages.value.filter(m => m.type === 'tool').length;
            console.debug('[tool_start]', data.tool, 'id=' + toolId,
                'tool count before insertion:', toolCountBefore);

            const startedAt = Date.now();
            // B9 (#1154): use the race-proof DM detector instead of gating
            // purely on activeSession resolution. A replayed tool_start on a
            // reconnect whose session metadata hasn't landed yet still groups
            // correctly when a dm_reasoning block already exists for this run.
            const isDm = isDmEvent(runId);

            if (isDm && !data.source_agent) {
                // A tool boundary closes the current turn: any visible reply
                // text streamed so far was intermediate "thinking out loud"
                // (the runtime persists it as reasoning), so promote it from
                // the pending-reply buffer into the collapsible's reasoning
                // buffer. Run end will instead discard the trailing pending
                // text — the implicit reply, shown as the bubble. (#1157/#1162)
                commitDmPendingReplyToReasoning(runId);

                // DM sessions: add the tool to the live reasoning block
                // for this run instead of inserting a standalone tool row.
                const toolEntry = {
                    id: toolId, type: 'tool', tool: data.tool, params: data.params,
                    status: 'running', startedAt, runId,
                };
                transformMessages(prev => {
                    const idx = prev.findIndex(
                        m => m.type === 'dm_reasoning' && m.runId === runId
                    );
                    if (idx >= 0) {
                        const block = prev[idx];
                        const updated = [...prev];
                        updated[idx] = {
                            ...block,
                            tools: [...block.tools, toolEntry],
                        };
                        return updated;
                    }
                    // Race defense: tool_start arrived before run_created
                    // for this runId -- lazily create a reasoning block.
                    return [...prev, {
                        id: nextMsgId(),
                        type: 'dm_reasoning',
                        runId: runId,
                        agentName: null,
                        thinkingText: '',
                        tools: [toolEntry],
                        status: 'running',
                        isLive: true,
                    }];
                });
                // DM tool starts do NOT update the header bar (#688).
                // "Chatting with {peer}..." is sticky during DMs.
            } else if (data.tool === 'invoke_agent' && !data.source_agent) {
                // The `!data.source_agent` clause is what keeps ARM ORDER from
                // being load-bearing (#1167 investigation). This arm sits ABOVE
                // the `data.source_agent` drop below, so without it a
                // subagent-tagged `invoke_agent` — alone among tagged tools —
                // reached `appendMessage` and rendered as a PARENT tool row,
                // contradicting the drop arm's own contract ("they must never
                // render as parent tool rows"). In a DM that is a top-level tool
                // row escaping the collapsible with `isDmEvent` TRUE — exactly
                // the #1167 shape, and not covered by the `isDmEvent === false`
                // reasoning that closes every other route to that fallback.
                //
                // Unreachable today: it needs a subagent to call `invoke_agent`,
                // and recursive spawning is not shipped. This clause is a
                // no-op on every path that exists now (an untagged invoke_agent
                // is unaffected; a tagged one now takes the drop arm like its
                // siblings) and closes the route before it can open.
                sealLastAgent();
                const name = data.params?.name || data.params?.subagent_name || 'subagent';
                const task = data.params?.task || '';
                appendMessage({
                    id: toolId, type: 'tool', tool: 'invoke_agent', params: data.params,
                    status: 'running', startedAt, runId,
                });
                trackSubagentStart(name, task, toolId);
                // #1105: backend may also embed `subagent_session_id` directly
                // on the `tool_start` event for invoke_agent (alternative
                // shape). When present, surface it immediately so the
                // Subagent status bar chip can navigate during the run
                // — the dedicated `subagent_started` handler below covers
                // the canonical event-based shape. Both paths are safe to
                // run together because `setSubagentSessionId` is idempotent.
                //
                // Resolve the target entry key by `toolId` first to avoid the
                // `setSubagentSessionId('subagent', ...)` fallback path, which
                // attaches to the first running unnamed entry it finds — wrong
                // target when multiple unnamed subagents run concurrently.
                // For named subagents `findSubagentByToolInvocationId` returns
                // the name directly; for unnamed it returns the
                // `subagent-<toolId_prefix>` key just registered by
                // `trackSubagentStart` above. Fall back to the resolved name
                // only if no entry was found (defensive — should not happen
                // since we just registered it).
                if (data.subagent_session_id) {
                    const key = findSubagentByToolInvocationId(toolId) || name;
                    setSubagentSessionId(key, data.subagent_session_id);
                }
            } else if (data.source_agent) {
                // Subagent-tagged tool events are no longer tracked here: the
                // Subagent status bar is driven by the backend's coarse
                // `subagent_activity` signal (which carries the tool name
                // without the params payload). This branch only sees tagged
                // events replayed from pre-status-bar session event logs and
                // deliberately drops them — they must never render as parent
                // tool rows.
            } else {
                sealLastAgent();
                appendMessage({
                    id: toolId, type: 'tool', tool: data.tool, params: data.params,
                    status: 'running', startedAt, runId,
                });

                // Update the header bar to show which specific tool is running.
                // This provides per-tool granularity beyond the batch-level
                // "executing_tools" status event from the backend.
                // Skip if DM context is active -- "Chatting with..." is
                // sticky during DMs (#688).
                if (!dmPeer.value) {
                    setAgentPhase('tool_active', data.tool);
                }
            }
        });
    });

    // -- tool_end --
    on('tool_end', (e) => {
        batch(() => {
            const data = e.data;
            const matchId = data.tool_invocation_id;
            const status = data.ok ? 'done' : 'fail';

            if (data.source_agent) {
                // Subagent-tagged tool_end: nothing to track (the status bar
                // is driven by `subagent_activity` signals now; this arrives
                // only as a replay from pre-status-bar event logs). Return
                // before the matching logic below — its last-running-tool
                // fallback could otherwise mis-close a PARENT tool row with
                // the subagent's result.
                return;
            }

            const endedAt = Date.now();

            const applyToolEnd = (m) => {
                const durationMs = m.startedAt ? endedAt - m.startedAt : null;
                return { ...m, status, result: data.result, durationMs };
            };

            // DM reasoning blocks: update the matching tool inside the
            // block's tools array rather than updating a standalone message.
            //
            // B9 (#1154): mirror the tool_start race-proofing. The matching
            // tool_start may have grouped the tool into a dm_reasoning block
            // via the event-driven detector even when activeSession hadn't
            // resolved; tool_end must look inside the blocks under the same
            // condition or it would fall through to the standalone-message
            // path and fail to find the (correctly grouped) tool.
            const isDm = isDmEvent(data.run_id);
            if (isDm && matchId && !data.source_agent) {
                let dmFound = false;
                transformMessages(prev => {
                    const copy = [...prev];
                    for (let i = 0; i < copy.length; i++) {
                        const m = copy[i];
                        if (m.type !== 'dm_reasoning') continue;
                        const toolIdx = m.tools.findIndex(t => t.id === matchId);
                        if (toolIdx >= 0) {
                            const updatedTools = [...m.tools];
                            updatedTools[toolIdx] = applyToolEnd(updatedTools[toolIdx]);
                            copy[i] = { ...m, tools: updatedTools };
                            dmFound = true;
                            break;
                        }
                    }
                    return copy;
                });
                if (dmFound) {
                    if (!data.source_agent) {
                        const { phase } = agentPhase.value;
                        if (phase === 'tool_active' || phase === 'executing_tools') {
                            revertPhase('calling_llm');
                        }
                    }
                    return;
                }
                // Fall through to standard matching if not found in any
                // reasoning block (defensive -- should not happen normally).
            }

            // Primary match: by tool_invocation_id (exact ID correlation).
            // Fallback: if the primary match fails (e.g. tool message was
            // reconstructed from history with a different ID scheme), fall
            // back to matching the last running tool message.
            let found = matchId && updateMessage(
                m => m.type === 'tool' && m.id === matchId,
                applyToolEnd,
            );
            if (!found) {
                found = updateMessage(
                    m => m.type === 'tool' && m.status === 'running',
                    applyToolEnd,
                );
            }
            // -- Subagent status-bar chip termination --
            //
            // A FOREGROUND `invoke_agent` has exactly ONE route to a terminal
            // chip: this event. `subagent_completed` is emitted only for
            // BACKGROUND subagents (`run_subagent` fires the completion
            // channel behind `handle.is_background`), so if this branch
            // no-ops the chip stays `running` until the whole map is cleared
            // by a session switch — i.e. it visibly outlives the subagent for
            // the rest of the parent run.
            //
            // Which is why the chip is resolved by the `tool_invocation_id`
            // correlator against the TRACKED ENTRY, not by re-finding the
            // chat row: the row match above deliberately falls back to "the
            // last running tool message" when the id correlation misses (a
            // row rebuilt from the `run_tool_calls` records is keyed by the
            // PROVIDER call id, not the invocation id — see
            // `mapHistoryMessages`), and looking the row back up STRICTLY by
            // `matchId` then returns `undefined` on exactly that path. Entries
            // are only ever created for `invoke_agent` calls, so a correlator
            // hit is itself proof that this event closes one — no chat row
            // required. Reading `chatMessages` again is a best-effort source
            // of the `name`/`subagent_name` params only.
            const subagentKey = findSubagentByToolInvocationId(matchId);
            const invokeMsg = matchId
                ? chatMessages.value.find(m => m.type === 'tool' && m.id === matchId)
                : chatMessages.value.findLast(m => m.type === 'tool' && m.status === status);
            if (subagentKey || invokeMsg?.tool === 'invoke_agent') {
                const resultObj = typeof data.result === 'object' ? data.result : null;
                // Background subagents return a `task_id` immediately and keep
                // running; their chip is terminated by `subagent_completed`.
                const isBackground = resultObj && resultObj.task_id;
                if (!isBackground) {
                    // Resolve the subagent name: prefer the explicit name from
                    // params, fall back to the entry resolved by the
                    // invoke_agent tool_invocation_id (handles unnamed
                    // subagents whose bar entry may have been migrated to the
                    // backend-assigned label by a forwarded
                    // `subagent_activity` signal).
                    const name = invokeMsg?.params?.name
                        || invokeMsg?.params?.subagent_name
                        || subagentKey;
                    if (name) {
                        const subagentSessionId = (resultObj && resultObj.session_id) || null;
                        // Capture session_id from invoke_agent result for
                        // drill-down navigation before ending the subagent.
                        if (subagentSessionId) {
                            setSubagentSessionId(name, subagentSessionId, matchId);
                        }
                        // Pass both correlators so the entry resolves
                        // identity-exactly (tool_invocation_id -> session id ->
                        // name), the same order the `subagent_completed`
                        // handler uses.
                        trackSubagentEnd(name, status, matchId, subagentSessionId);
                    }
                }
            }
            if (!found && !data.source_agent) {
                // tool_end arrived for a non-subagent tool, but no matching
                // tool message was found in chatMessages. This means the
                // tool_start message was lost or never arrived. Log for
                // diagnosis. (Relates to #501 Bug 4 investigation)
                console.warn('[tool_end] no matching tool message found for',
                    matchId, '- tool messages in chat:',
                    chatMessages.value.filter(m => m.type === 'tool').length);
            }
            // When no matching tool message was found for subagent-only tool
            // events, skip the chatMessages write to avoid an unnecessary
            // re-render with a new array reference.

            // Revert the header bar phase after tool completion.
            // For non-subagent tools: if other tools are still running,
            // the next tool_start will immediately set tool_active again.
            // If no more tools are running, the backend will emit a
            // calling_llm status event shortly.  Revert to DM fallback
            // (if in a DM run) or calling_llm as a reasonable default.
            if (!data.source_agent) {
                const { phase } = agentPhase.value;
                if (phase === 'tool_active' || phase === 'executing_tools') {
                    revertPhase('calling_llm');
                }
            }
        });
    });

    // -- approval_required --
    on('approval_required', (e) => {
        batch(() => {
            flushDeltaBuffer();
            sealLastAgent();
            const data = e.data;
            // Deduplicate: skip if an approval card with this ID already exists.
            // This can happen when the approval was reconstructed from the REST
            // API on session switch and then replayed by the SSE stream.
            // (Fixes #487 Bug 2 -- prevents duplicate approval prompts)
            const alreadyExists = chatMessages.value.some(
                m => m.type === 'approval' && m.approvalId === data.approval_id
            );
            if (!alreadyExists) {
                const norm = normalizeApproval(data);
                appendMessage({
                    id: nextMsgId(), type: 'approval', approvalId: norm.approvalId,
                    tool: norm.tool, params: norm.params, runId: norm.runId,
                    resolved: false,
                });
            }
        });
    });

    // -- subagent_activity --
    //
    // Coarse status signal for the Subagent status bar: the backend reduces a
    // running subagent's activity to `{ source_agent, kind, tool? }` where
    // `kind` is `reasoning` / `writing` / `tool_start` / `tool_end` (tool name
    // only on `tool_start`). Ephemeral (never persisted/replayed) and
    // deduplicated backend-side, so this handler fires roughly once per
    // activity transition. The bar renders the latest signal as a concise
    // label; the subagent's actual content streams to its OWN session (#1184).
    on('subagent_activity', (e) => {
        const data = e.data;
        // Defensive: a signal without a source label can't be routed to a
        // chip. The backend always tags these.
        // `tool_invocation_id` (tool kinds only) is the toolsUsed idempotency
        // key (#1190): the attach-time snapshot replay re-sends the current
        // in-progress tool_start with the SAME id, while parallel same-tool
        // invocations carry distinct ids. `parent_tool_invocation_id` is the
        // PARENT invoke_agent invocation id — the chip-resolution correlator
        // that makes unnamed-subagent routing identity-exact (same id as
        // `subagent_started` / the entry's stored toolInvocationId), so
        // concurrent unnamed subagents can never cross-migrate onto each
        // other's chips.
        trackSubagentActivity(data.source_agent, data.kind, data.tool || null,
            data.tool_invocation_id || null,
            data.parent_tool_invocation_id || null);
    });

    // -- subagent_started (#1105) --
    //
    // Surfaces a foreground subagent's `session_id` to the parent stream as
    // soon as `subagent::execute` creates the session, so the Subagent
    // status bar chip can navigate to the subagent session live (i.e. while
    // the subagent is still running).
    //
    // Payload shape (per #1105 issue body):
    //   { subagent_name, tool_invocation_id, subagent_session_id }
    //
    // Background subagents already get their session_id through `tool_end`
    // moments after dispatch, so emitting this event for them is harmless
    // and idempotent — `setSubagentSessionId` writes the same value either
    // way. The resolution order mirrors `tool_end` for invoke_agent:
    //   1. `subagent_name` from the event payload (named subagents)
    //   2. lookup by `tool_invocation_id` (unnamed subagents — covers the
    //      "subagent-<prefix>" key migration done by forwarded
    //      `subagent_activity` signals)
    //
    // If neither resolver returns a hit (malformed payload, out-of-order
    // delivery against an entry that has already been removed, etc.) the
    // handler no-ops. The previous fallback to the literal `'subagent'`
    // could attach the session_id to an unrelated running unnamed entry
    // via the first-match path in `setSubagentSessionId`, producing
    // incorrect drill-down links.
    on('subagent_started', (e) => {
        batch(() => {
            const data = e.data;
            const sessionId = data.subagent_session_id;
            const name = data.subagent_name
                || findSubagentByToolInvocationId(data.tool_invocation_id);
            if (!name) {
                console.warn('[subagent_started] cannot resolve target entry',
                    '— subagent_name:', data.subagent_name,
                    'tool_invocation_id:', data.tool_invocation_id);
                return;
            }
            setSubagentSessionId(name, sessionId);
        });
    });

    // -- subagent_completed --
    on('subagent_completed', (e) => {
        batch(() => {
            const data = e.data;
            const name = data.subagent_name || 'subagent';
            const status = data.status || 'done';
            const sessionId = data.subagent_session_id || null;
            const summary = data.summary || '';
            // A1-2 / #1125: the event now carries the parent's invoke_agent
            // tool_invocation_id (serialized only when present). It is the
            // only reliable disambiguator when two unnamed/ephemeral
            // subagents run concurrently — both arrive with
            // `subagent_name: null` → name "subagent", and the name-only
            // first-match fallback would end / attach the session id to the
            // WRONG chip. Pass it to the resolvers, which try
            // tool_invocation_id → session id → name in that order.
            const toolInvocationId = data.tool_invocation_id || null;

            // Look up subagent entry for metadata (task, tool count, duration)
            // using the same resolution order: tool_invocation_id first, then
            // session id, then the legacy name match.
            const entryKey = findSubagentByToolInvocationId(toolInvocationId)
                || findSubagentBySessionId(sessionId);
            const entry = (entryKey && activeSubagents.value[entryKey])
                || activeSubagents.value[name]
                || Object.values(activeSubagents.value).find(
                    e => e.displayName === name || (name === 'subagent' && e.status === 'running')
                );

            const task = entry ? entry.task : '';
            const toolCount = entry ? (entry.toolsUsed || 0) : 0;
            const durationMs = entry && entry.startedAt ? Date.now() - entry.startedAt : null;

            // If subagent_session_id is provided, store it on the subagent entry
            if (sessionId) {
                setSubagentSessionId(name, sessionId, toolInvocationId);
            }

            // Update the Subagent status bar (stays visible until auto-remove delay)
            trackSubagentEnd(name, status, toolInvocationId, sessionId);

            // Render a rich completion card instead of a plain system message
            appendMessage({
                id: nextMsgId(),
                type: 'subagent_completed',
                name,
                task,
                status,
                toolCount,
                durationMs,
                sessionId,
                summary,
            });
        });
    });

    // -- job_completed --
    on('job_completed', (e) => {
        const data = e.data;
        appendMessage({
            id: nextMsgId(),
            type: 'job_completed',
            jobName: data.job_name || 'job',
            status: data.status || 'success',
            summary: data.summary || '',
            ts: data.ts || null,
            // Deep-link handle (#1196): lets JobCompletionCard fetch the full
            // persisted output via GET /runs/{run_id} when the live summary was
            // truncated at the cap. `truncated` is the authoritative flag the
            // card keys its fetch decision on.
            runId: data.run_id || null,
            truncated: data.truncated,
            // Deep-link handles (#1213/#1217): the job's hidden session.
            // `jobSessionUuid` is the REAL SessionId the "Go to job session"
            // button navigates by (what GET /session/{id} resolves);
            // `jobSessionId` is the `job_{job_id}` context handle, kept for
            // identity only and NOT used as a navigation target.
            jobSessionUuid: data.job_session_uuid || null,
            jobSessionId: data.job_session_id || null,
        });
    });

    // -- dm_message: live DM message from peer agent (#632) --
    on('dm_message', (e) => {
        batch(() => {
            flushDeltaBuffer();
            sealLastAgent();
            const data = e.data;
            // Insert the message as an agent-role DM message with fromAgent
            // metadata so the DM conversation view can render it on the
            // correct side. Use type 'agent' to match what mapHistoryMessages
            // produces for DM messages (history.js maps isDm messages to
            // type 'agent' with fromAgent set). The sealed flag prevents
            // the delta buffer from appending streamed text onto this
            // message. (#650)
            appendMessage({
                id: nextMsgId(),
                type: 'agent',
                role: 'assistant',
                text: data.message,
                fromAgent: data.from_agent,
                fromAgentId: data.from_agent_id,
                sealed: true,
                // Per-message timestamp (#855) — prefer the SSE-supplied
                // event ts so the rendered time matches the persisted
                // message timestamp on the server side.
                ts: data.ts || new Date().toISOString(),
            });
        });
    });

    // -- dm_conversation_ended --
    on('dm_conversation_ended', (e) => {
        const data = e.data;
        const peer = data.peer || 'unknown';
        const reason = DM_END_REASON_LABELS[data.reason] || data.reason || 'conversation ended';
        // #1215/#1218: the web-chat forward sets `suppress_banner` when the
        // DM-end notification RUN is itself the visible notification in this
        // chat (the reloadable marker is suppressed for the same reason). We
        // still clear the phase below, but skip rendering a live `dm_ended`
        // banner so a live viewer sees only the run, not run + banner (the live
        // half of "initiator gets both"). DM-session emissions never set this
        // flag, so the DM-session-view banner is unaffected.
        const suppressBanner = data.suppress_banner === true;
        // B10 (#1154): the backend may intentionally emit MULTIPLE
        // `dm_conversation_ended` events for a SINGLE conversation-end (see
        // `runs/dm_lifecycle.rs` — both the depth/ignore trigger path and the
        // run-end terminal arm can fire one). Without a dedupe each event
        // appended its own "conversation ended" banner, so a single end
        // rendered two (or more) identical dividers.
        //
        // The dedupe is POSITIONAL, not key-based, because `context_id`
        // ("dm:<a>:<b>") is PAIR-stable, NOT a per-conversation identity: a
        // persistent DM session holds many conversation lifecycles, all
        // carrying the SAME context_id (verified at every backend emit site).
        // The reason label set is also tiny (depth_exceeded / ignored /
        // user_cancelled / errored), so two genuinely separate ends on one
        // session routinely share context_id + peer + reason. A key-based
        // dedupe would silently swallow the second legitimate end.
        //
        // Backend duplicates for ONE end arrive ADJACENT with no conversation
        // activity in between; a legitimate second end is always preceded by a
        // new conversation. So we only dedupe against the TRAILING banner —
        // scan backward and stop at the first real conversation entry
        // (legitimate new conversation → never suppress) or the first
        // `dm_ended` (compare it; if it matches, this is a duplicate). This
        // also covers the both-ignore race and never suppresses
        // reload-restored old banners (activity sits in between).
        //
        // The activity-break must list the actual `chatMessages` entry TYPES
        // that a DM conversation produces, NOT the SSE event names. A delivered
        // peer message is stored as `type: 'agent'` (live `dm_message` handler
        // + the `history.js` reload mapper, which both map isDm messages to
        // 'agent'); a user message is `'user'`; a queued-run indicator is
        // `'thinking'` (the live reasoning block is `'dm_reasoning'`, deferred
        // to `run_started` for queued runs). Breaking only on `'dm_reasoning'`
        // here would miss the 'agent'/'user'/'thinking' entries and could
        // SUPPRESS a legitimate second banner; listing every real entry type is
        // conservative in the safe direction (worst case is a benign duplicate,
        // never a missing banner). `'dm_message'` is kept for forward-safety
        // even though nothing emits that entry type today.
        const contextId = data.context_id || null;
        let alreadyEnded = false;
        for (let i = chatMessages.value.length - 1; i >= 0; i--) {
            const m = chatMessages.value[i];
            if (m.type === 'agent' || m.type === 'user' || m.type === 'thinking'
                || m.type === 'dm_message' || m.type === 'dm_reasoning') break;
            if (m.type === 'dm_ended') {
                alreadyEnded = (contextId && m.contextId === contextId)
                    || (m.peer === peer && m.reason === reason);
                break;
            }
        }
        if (!suppressBanner && !alreadyEnded) {
            appendMessage({
                id: nextMsgId(), type: 'dm_ended', peer, reason, contextId,
                // #1258: an interrupted end (cancel/failure) no longer starts
                // a run that would narrate itself, so the banner is where the
                // operator reads WHY. Absent for every other end.
                detail: data.detail || null,
            });
        }
        // Reset the status bar -- the DM conversation is over, so the
        // "Chatting with {peer}..." phase and dmPeer context must be
        // cleared to return to idle state. (Always runs, even on a deduped
        // duplicate, so a late duplicate still clears any lingering phase.)
        clearAgentPhase();
    });

    // -- dm_activity_started: DM run started on behalf of this agent (#659) --
    // Forwarded from the DM session to the webchat session so the UI can
    // show "Chatting with {peer}..." even when viewing the main session.
    on('dm_activity_started', (e) => {
        const data = e.data;
        if (data.peer) {
            setDmContext(data.peer);
        }
    });

    // -- dm_activity_status: DM run phase update forwarded to webchat (#688) --
    // ALWAYS maps to "Chatting with {peer}..." regardless of the phase.
    // The internal tool use within a DM is irrelevant to the top-level
    // status -- the user only cares that the agent is chatting with a peer.
    // Tool-level detail (e.g. "Running shell...") is visible when viewing
    // the DM session directly, not from the webchat session.
    on('dm_activity_status', (e) => {
        const data = e.data;
        const peer = dmPeer.value || data.peer;
        if (peer) {
            setAgentPhase('dm', peer);
        }
    });

    // -- dm_activity_ended: a single DM run finished (#688) --
    // This does NOT clear the DM status -- the conversation may still
    // have more turns. "Chatting with {peer}..." stays visible until
    // dm_conversation_ended arrives (signalling the entire conversation
    // is over) or until a non-DM run starts on this session.
    on('dm_activity_ended', (e) => {
        const data = e.data;
        const peer = dmPeer.value || data.peer;
        // Keep showing "Chatting with..." -- the DM is still active
        // (the peer agent may be formulating its next reply).
        if (peer) {
            setAgentPhase('dm', peer);
        }
    });

    // -- approval_resolved --
    //
    // Remove the approval card entirely so the live render matches the
    // reload render (which never materialises approval cards for resolved
    // approvals — listApprovals only returns pending ones, and session
    // history has no approval records).  Dropping the card also lets the
    // sibling tool rows collapse back into their parallel-tool group in
    // app.js `groupMessages`, matching the reload layout.  (#800)
    //
    // The tool row itself is updated separately via the `tool_end` event
    // emitted by the runtime after approve (success/fail based on tool
    // execution) or immediately after deny (`user_denied: true` result,
    // #1109 — the run then terminates via `run_cancelled`).
    on('approval_resolved', (e) => {
        const data = e.data;
        filterMessages(
            m => !(m.type === 'approval' && m.approvalId === data.approval_id),
        );
    });

    // -- context_debug: full context window snapshot (debug mode) --
    //
    // #1003: the event now carries `agent_id` and `agent_name` so the
    // UI can attribute the panel to the specific agent whose turn
    // produced the snapshot. This matters most for DM sessions where
    // two agents alternate turns on the same session and each emits
    // their own per-perspective context window — without attribution,
    // back-to-back panels are indistinguishable. `agent_name` is
    // optional on the wire (legacy unnamed runtimes serialise it as
    // `null`); the renderer falls back to "agent" in that case.
    on('context_debug', (e) => {
        batch(() => {
            const data = e.data;
            appendMessage({
                id: nextMsgId(),
                type: 'context_debug',
                messages: data.messages,
                toolNames: data.tool_names,
                totalTokens: data.total_tokens,
                systemTokens: data.system_tokens,
                historyMessageCount: data.history_message_count,
                agentId: data.agent_id,
                agentName: data.agent_name,
            });
        });
    });

    // -- run_warning (non-fatal, e.g. max iterations) --
    // Subagent warnings (source_agent set) are suppressed in the parent
    // chat -- they are visible via the Subagent status bar drill-down into the
    // subagent's own session.  (#602)
    on('run_warning', (e) => {
        const data = e.data;
        if (data.source_agent) return;
        batch(() => {
            flushDeltaBuffer();
            sealLastAgent();
            const code = data.warning?.code || 'UNKNOWN';
            const msg = data.warning?.message || 'Warning';
            appendMessage({ id: nextMsgId(), type: 'warning', code, text: msg });
        });
    });

    // -- run_finished / run_error / run_cancelled --
    const handleRunEnd = (status) => (e) => {
        bumpRunListGeneration();
        batch(() => {
            flushDeltaBuffer();
            sealLastAgent();
            const data = e.data;

            // Build the approval-resolution-and-append phase via
            // transformMessages so it results in a single signal write.
            // Note: flushDeltaBuffer() and sealLastAgent() above may each
            // write to chatMessages.value independently, but this section
            // (approval resolution + appended status/error/token messages)
            // is collapsed into one write to avoid intermediate states.
            const endingRunId = data.run_id || null;
            if (endingRunId) {
                const terminalStatus = status === 'finished'
                    ? 'completed'
                    : status === 'error' ? 'failed' : 'cancelled';
                setRunStatus(endingRunId, terminalStatus, {
                    sessionId,
                    cursor: eventCursor(e),
                    streamEpoch: lastSeenStreamEpoch,
                });
            }


            // S3 (#1154): gate via `isDmEvent` (run_id-aware), not a bare
            // `activeSession` read. On a reconnect that replays the terminal
            // `run_finished`/`run_error`/`run_cancelled` before `activeSession`
            // resolves, a bare check would seal the DM run on the non-DM path
            // — skipping the `savedThinkingText` read below, so the run's
            // final-turn reasoning is lost from its collapsible. `isDmEvent`
            // ORs in the live `dm_reasoning` block proof for this run.
            const isDm = isDmEvent(endingRunId || activeRunId.value);

            // Read and save thinking text BEFORE deleting from buffer,
            // so the transformMessages callback below can use it.
            // (C1 fix: previously the delete happened first, then the
            // callback read the updated signal and got empty string.)
            //
            // `dmThinkingBuffers` holds ONLY committed reasoning (reasoning_delta
            // plus any pre-tool visible text promoted at a tool boundary). The
            // run's trailing visible text — the implicit reply, already shown as
            // the `dm_message` bubble — sits in `dmPendingReplyBuffers` and is
            // discarded here, never sealed into the collapsible. That asymmetry
            // is the #1157/#1162 fix: live now matches the reload render, where
            // the runtime persists the reply as a `dm` bubble and only the
            // distinct extended-thinking trace as reasoning.
            let savedThinkingText = '';
            if (isDm && endingRunId) {
                savedThinkingText = dmThinkingBuffers.value.get(endingRunId) || '';
                if (savedThinkingText) {
                    const next = new Map(dmThinkingBuffers.value);
                    next.delete(endingRunId);
                    dmThinkingBuffers.value = next;
                }
                discardDmPendingReply(endingRunId);
            }

            transformMessages(prev => {
                const toolCountBefore = prev.filter(m => m.type === 'tool').length;

                // Drop any pending approval cards for this run.
                // When a run ends (cancelled, error, or finished), any unresolved
                // approval prompts are stale and must be dismissed so the user is
                // not left with dangling Approve/Deny buttons.  (Fixes #487 Bug 1)
                //
                // Scoped to the ending run's ID so concurrent runs (future) do not
                // accidentally dismiss each other's approval cards.  Approval cards
                // without a runId (legacy) are always dropped as a fallback.
                //
                // #800: removing the card instead of marking it resolved also
                // matches the reload path — `listApprovals` only returns currently-
                // pending approvals, so on reload these stale entries simply don't
                // exist.  The accompanying tool row is cancelled via isStuckTool
                // below, which matches the history render for an interrupted run.
                const isStaleApproval = (m) =>
                    m.type === 'approval' && !m.resolved
                    && (!m.runId || !endingRunId || m.runId === endingRunId);

                // NOTE: The defensive sweep that previously rewrote any
                // `m.type === 'tool' && m.status === 'running'` to
                // `cancelled` on run_end (added in PR #594 for #593) was
                // removed in #846. The runtime now guarantees that every
                // `tool_start` has a matching terminal `tool_end` event,
                // including the cancel-during-tool-execution case (the
                // synthesised `ToolEnd { ok: false, result: { error: 'run
                // cancelled' } }` emitted from the outer `select!` cancel
                // arm in `run_tool_calls`). The bandage was hiding a
                // contract violation rather than fixing it; with the
                // runtime fix in place the bandage is no longer needed,
                // and removing it ensures any future regression in the
                // runtime's terminal-event invariant surfaces immediately
                // (stuck spinner) rather than being silently masked.

                let msgs = prev.filter(m => !isStaleApproval(m)).map(m => {
                    // Seal live DM reasoning blocks for this run.
                    if (m.type === 'dm_reasoning' && m.runId === endingRunId && m.isLive) {
                        const finalThinking = savedThinkingText || m.thinkingText || '';
                        const blockStatus = status === 'error' ? 'failed'
                            : status === 'cancelled' ? 'cancelled' : 'done';
                        // Also cancel any still-running tools inside the block.
                        const sealedTools = m.tools.map(t =>
                            t.status === 'running' && blockStatus !== 'done'
                                ? { ...t, status: 'cancelled' } : t
                        );
                        return {
                            ...m,
                            status: blockStatus,
                            isLive: false,
                            thinkingText: finalThinking,
                            tools: sealedTools,
                        };
                    }
                    return m;
                });

                if (status === 'error') {
                    const code = data.error?.code || 'INTERNAL';
                    const rawMsg = typeof data.error === 'string'
                        ? data.error : (data.error?.message || 'Run failed');
                    const text = friendlyErrorMessage(code, rawMsg);
                    msgs = [...msgs, { id: nextMsgId(), type: 'error', code, text }];
                }
                if (status === 'cancelled') {
                    msgs = [...msgs, { id: nextMsgId(), type: 'system', text: '(run cancelled)' }];
                }
                if (status === 'finished' && !sawTokenDelta && !isDm) {
                    // Only show "(run completed)" for runs that had no streamed
                    // response (e.g. tool-only runs on non-DM sessions).
                    // Normal chat runs already display the streamed text.
                    // DM sessions suppress this because runs complete
                    // frequently (each agent reply is a separate run) and
                    // these transient system messages are never persisted,
                    // creating visual noise that vanishes on reload.
                    msgs = [...msgs, { id: nextMsgId(), type: 'system', text: '(run completed)' }];
                }

                const usage = (data.prompt_tokens || data.completion_tokens)
                    ? {
                        prompt_tokens: data.prompt_tokens || 0,
                        completion_tokens: data.completion_tokens || 0,
                        // reasoning_tokens is optional (OpenAI o-series / DeepSeek /
                        // xAI); stays undefined for non-reasoning runs and the
                        // TokenBadge hides it in that case.
                        reasoning_tokens: data.reasoning_tokens,
                        // Cache metrics (#766) — Anthropic-only; absent on
                        // other providers. TokenBadge and runs-tab hide
                        // them when undefined.
                        cache_creation_input_tokens: data.cache_creation_input_tokens,
                        cache_read_input_tokens: data.cache_read_input_tokens,
                    }
                    : data.usage;
                if (usage) {
                    msgs = [...msgs, { id: nextMsgId(), type: 'tokens', usage }];
                }

                // Defensive check: log if tool messages were lost.
                const toolCountAfter = msgs.filter(m => m.type === 'tool').length;
                if (toolCountAfter < toolCountBefore) {
                    console.warn('[handleRunEnd] tool message count decreased:', toolCountBefore, '->', toolCountAfter);
                }

                return msgs;
            });

            // Preserve DM context across runs (#688): when the agent is
            // in a DM conversation, individual run endings should NOT
            // clear the "Chatting with..." status. The status only clears
            // when the entire DM conversation ends (dm_conversation_ended).
            // For non-DM runs, clear the phase as before.
            if (dmPeer.value) {
                setAgentPhase('dm', dmPeer.value);
            } else if (activeRunId.value) {
                setAgentPhase('calling_llm', null);
            } else {
                clearAgentPhase();
            }

            // Run creation pre-persists the user input before execution, so
            // every terminal state (including cancellation) confirms it.
            // The session history is now the source of truth.
            //
            // Use the stream's closure-captured sessionId (not
            // activeSessionId.value) because the user may have switched
            // to a different session before this run ended.
            if (!endingRunId) {
                console.warn('[handleRunEnd] terminal event missing run_id; optimistic message left for reconciliation');
            } else {
                confirmOptimisticMessage(sessionId, { runId: endingRunId });
            }
        });

        // Process queued user messages via dynamic import
        // (avoids circular dependency with input-area.js)
        if (!activeRunId.value && messageQueue.value.length > 0) {
            const next = messageQueue.value[0];
            // Keep the head queued until acceptance. Use the stream's
            // sessionId — same reasoning as the queue drain itself: the
            // queue belongs to the run's session, not whatever session
            // the operator is currently looking at. (#975)
            import('../components/chat/input-area.js').then(async mod => {
                if (mod.startQueuedRun) {
                    await mod.startQueuedRun(next, sessionId);
                }
            }).catch(err => {
                console.error('[session-stream] Failed to process queued message:', err);
            });
        }
    };

    on('run_finished', handleRunEnd('finished'));
    on('run_error', handleRunEnd('error'));
    on('run_cancelled', handleRunEnd('cancelled'));

    es.onerror = () => {
        if (es.readyState === EventSource.CLOSED) {
            sessionRetryCount++;
            if (sessionRetryCount >= MAX_SESSION_RETRIES) {
                console.error('[session-stream] Max retries reached');
                // Surface the dead state to the user (#907). The banner
                // is rendered globally and offers click-to-reconnect;
                // the browser-level `online` event also calls back into
                // `reconnectAllStreams` to re-arm both hooks.
                markStreamDead('session');
                return;
            }
            const delay = Math.min(2000 * Math.pow(2, sessionRetryCount - 1), 30000);
            // Track the timer id so the manual-reconnect path can
            // cancel it (see the clearTimeout at the top of
            // openSessionStream / closeSessionStream). Null the slot
            // on fire so the cancel guard short-circuits cleanly.
            backoffTimer = setTimeout(() => {
                backoffTimer = null;
                if (activeSessionId.value === sessionId) {
                    openSessionStream(sessionId, { lastEventId: lastSeenEventId, streamEpoch: lastSeenStreamEpoch });
                }
            }, delay);
        }
    };
}

/**
 * Reset the session-stream retry budget and reopen against the
 * currently-active session. Bound at module load to the global
 * stream-health pub/sub so the banner click and the browser
 * `online` event can both re-arm the stream without a full page
 * reload (#907).
 *
 * No-op when there is no active session — the stream will be
 * opened by the next `loadSession` call as part of normal session
 * navigation.
 */
function reconnectSessionStream() {
    sessionRetryCount = 0;
    const sid = activeSessionId.value;
    if (sid) {
        openSessionStream(sid, { lastEventId: lastSeenEventId, streamEpoch: lastSeenStreamEpoch });
    } else {
        // Even with no active session there's nothing to reconnect to,
        // but the dead flag should be cleared so the banner reflects
        // reality (no stream is currently dead because no stream is
        // currently desired).
        clearStreamDead('session');
    }
}

// Register the reconnect callback once at module load. The
// stream-health module dispatches to whatever is currently
// registered — last-write-wins semantics tolerate test-side
// re-imports without stacking handlers (see `registerSessionReconnect`).
registerSessionReconnect(reconnectSessionStream);

export function closeSessionStream() {
    if (flushTimer !== null) {
        cancelAnimationFrame(flushTimer);
        flushTimer = null;
    }
    flushDeltaBuffer();
    // Cancel any pending backoff reopen — closing the stream is an
    // explicit "stop attempting to maintain this connection" signal
    // (#907 review, Suggestion 1). Without this, a teardown
    // mid-backoff would leave the timer queued to reopen against a
    // stale sessionId.
    if (backoffTimer !== null) {
        clearTimeout(backoffTimer);
        backoffTimer = null;
    }
    // Reset per-run state so it does not carry over to the next session.
    sawTokenDelta = false;
    // Drop any per-run pending DM reply text — it is the implicit reply that
    // was (or will be) delivered as a `dm_message` bubble, never the
    // collapsible, so it must not leak into a different session's run on
    // reopen. (#1157/#1162)
    dmPendingReplyBuffers.clear();
    // Drop the DM/peer run-source map for the same reason — a different session's
    // runs must not inherit this session's DM classification. A same-session
    // reconnect re-installs it via the carry-over in `openSessionStream`. (#1162)
    peerRunSources.clear();
    clearAgentPhase();
    // Drop the per-session reasoning-dedupe suppress-set (#1135) for the
    // stream being torn down so the store does not accumulate entries across
    // session switches (no cross-session leakage, no unbounded growth). A
    // same-session EventSource reconnect re-records it in `openSessionStream`
    // from `carriedSealedReasoning`, which is recovered BEFORE this close
    // fires — so this teardown does not defeat the reconnect fix.
    if (activeStreamSessionId != null) {
        clearSealedReasoningRunIds(activeStreamSessionId);
        activeStreamSessionId = null;
    }
    if (activeSessionEs) {
        activeSessionEs.close();
        activeSessionEs = null;
    }
    // Closing the stream means we are no longer attempting to maintain
    // this connection, so the dead-state signal should reflect "absent"
    // rather than "dead". Without this, deleting the session that owns
    // a previously-dead stream would leave the banner up indefinitely
    // until the user re-opens any session. (#907)
    clearStreamDead('session');
}

export function isSessionStreamOpen() {
    return activeSessionEs !== null;
}
