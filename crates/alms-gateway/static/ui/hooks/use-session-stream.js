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
import { activeRunId, bumpRunListGeneration } from '../state/runs.js';
import { trackSubagentStart, trackSubagentEnd, trackSubagentTool, findSubagentByToolInvocationId, setSubagentSessionId, activeSubagents } from '../state/subagents.js';
import { agentPhase, setAgentPhase, clearAgentPhase, setDmContext, revertPhase, dmPeer } from '../state/agent-status.js';
import { messageQueue } from '../state/queue.js';
import { activeSessionId, activeSession, dmParticipants } from '../state/sessions.js';
import { activeAgent } from '../state/agents.js';
import { normalizeApproval } from '../utils/approvals.js';
import { selectGeneration } from '../state/select-generation.js';
import { clearPendingMessage } from '../state/pending-messages.js';
import { DM_END_REASON_LABELS } from '../utils/constants.js';

/**
 * Per-run DM thinking text accumulation buffer.
 * Keyed by run_id, values are accumulated token_delta text.
 * Uses a Preact signal so that DmReasoningBlock components
 * re-render when new thinking text arrives.
 */
export const dmThinkingBuffers = signal(new Map());

/**
 * Derive the correct agent name for a DM reasoning block from the
 * run's source field and the DM participants list.
 *
 * In a DM between Alice and Bob:
 *   - source "peer:Alice" means Alice sent a message, so BOB is reasoning
 *   - source "peer:Bob"   means Bob sent a message, so ALICE is reasoning
 *
 * Falls back to the active agent's name for non-peer sources (e.g. user-
 * initiated runs) or when participants are not available.
 *
 * Fixes #692 — previously used `activeAgent.value?.name` which is the
 * agent selected in the UI dropdown, not necessarily the one reasoning.
 *
 * @param {string|null} source - the run's source field (e.g. "peer:Alice")
 * @returns {string|null} the name of the agent doing the reasoning
 */
function dmReasoningAgentName(source) {
    if (source && source.startsWith('peer:')) {
        const peerName = source.slice(5);
        const participants = dmParticipants.value;
        if (participants.length >= 2) {
            // The peer triggered the run — the OTHER participant is reasoning.
            return participants[0] === peerName ? participants[1] : participants[0];
        }
    }
    // Fallback: best effort from the active agent selector.
    return activeAgent.value?.name || null;
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
let sessionRetryCount = 0;
const MAX_SESSION_RETRIES = 10;
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
 * @param {object} [opts]
 * @param {number} [opts.lastEventId] -- skip replay of events up to (and
 *   including) this ID. Used when the client already loaded history via
 *   the REST API and only needs new live events going forward.
 */
export function openSessionStream(sessionId, opts) {
    closeSessionStream();
    if (!sessionId) return;

    const token = localStorage.getItem('alms_auth_token');
    const params = new URLSearchParams();
    if (token) params.set('token', token);
    if (opts && opts.lastEventId != null) params.set('last_event_id', String(opts.lastEventId));
    const qs = params.toString();
    const url = `/sessions/${sessionId}/events${qs ? '?' + qs : ''}`;
    const es = new EventSource(url);
    activeSessionEs = es;
    sessionRetryCount = 0;
    lastSeenEventId = (opts && opts.lastEventId != null) ? opts.lastEventId : null;

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
        // Track highest numeric ID for manual reconnect
        if (id && /^\d+$/.test(id)) {
            lastSeenEventId = id;
        }
        // Deduplicate: skip events we have already processed (but always
        // allow ephemeral events through -- they are never replayed and
        // their IDs are not stored in the set).
        if (id && !id.startsWith('ephemeral-')) {
            if (seenEventIds.has(id)) return;
            seenEventIds.add(id);
            // Prevent unbounded growth (#510): once the set exceeds the
            // cap, evict the oldest ~20% of entries.  Set preserves
            // insertion order, so we delete from the front to shed stale
            // IDs while keeping recent ones that a browser auto-reconnect
            // replay would contain.
            if (seenEventIds.size > SEEN_IDS_MAX) {
                const evictCount = Math.floor(SEEN_IDS_MAX * 0.2);
                let i = 0;
                for (const oldId of seenEventIds) {
                    if (i++ >= evictCount) break;
                    seenEventIds.delete(oldId);
                }
                console.debug('[sse-dedup] evicted', evictCount, 'stale IDs, size:', seenEventIds.size);
            }
        }
        handler(e);
    });

    // -- run_created: a new run was created on this session --
    on('run_created', (e) => {
        const data = JSON.parse(e.data);
        const queuedBehind = data.queued_behind || 0;
        sawTokenDelta = false;
        bumpRunListGeneration();

        // Cross-channel DM awareness: when the run source starts with
        // "peer:", the agent is responding to a DM from another agent.
        // Set the DM context so the header bar shows "Chatting with
        // {peer}..." as the fallback phase.  More specific phases (tool
        // execution) will temporarily override it, reverting when done.
        const isDm = activeSession.value?.session_type === 'dm';
        if (data.source && data.source.startsWith('peer:')) {
            setDmContext(data.source.slice(5));
        }

        if (isDm && data.run_id) {
            // DM sessions with queued runs: show a thinking indicator with
            // queue state instead of a live reasoning block. The reasoning
            // block will be created when run_started fires. (#691)
            if (queuedBehind > 0) {
                batch(() => {
                    activeRunId.value = data.run_id;
                    // Header bar is NOT updated here -- it should keep
                    // showing the agent's current activity (e.g. a DM with
                    // another peer).  The inline thinking indicator handles
                    // the queued state via queuedBehind. (#693)
                    appendMessage({
                        id: nextMsgId(), type: 'thinking', source: data.source, queuedBehind,
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
                    activeRunId.value = data.run_id;
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
            batch(() => {
                activeRunId.value = data.run_id;
                appendMessage({
                    id: nextMsgId(), type: 'thinking', source: data.source, queuedBehind,
                });
            });
        } else if (queuedBehind > 0) {
            // User-initiated run but agent is busy -- update the existing
            // thinking indicator (added by startRun) with queue position.
            // Header bar keeps its current state (the agent's real
            // activity); the inline indicator shows queue status. (#693)
            // Clear `pending` so it transitions from "Sending..." to
            // "Queued..." immediately. (#704)
            batch(() => {
                activeRunId.value = data.run_id;
                updateMessage(
                    m => m.type === 'thinking' && m.pending,
                    m => ({ ...m, queuedBehind, pending: false }),
                );
            });
        } else {
            // User-initiated, queue empty -- clear `pending` so the
            // indicator transitions from "Sending..." to "Thinking...".
            // (#704)
            batch(() => {
                activeRunId.value = data.run_id;
                updateMessage(
                    m => m.type === 'thinking' && m.pending,
                    m => ({ ...m, pending: false }),
                );
            });
        }
    });

    // -- run_started: the run has been dequeued and is now executing --
    on('run_started', (e) => {
        const data = JSON.parse(e.data);
        const isDm = activeSession.value?.session_type === 'dm';

        if (isDm && data.run_id) {
            // DM session: replace the queued thinking indicator with a
            // live reasoning block now that the run is actually executing.
            // Extract the source from the queued thinking indicator (set
            // by run_created) so we can derive the correct agent name.
            // Fixes #692 — was using activeAgent which is always the
            // UI-selected agent, not necessarily the one reasoning.
            const thinkingMsg = chatMessages.value.find(
                m => m.type === 'thinking' && m.queuedBehind > 0
            );
            const agentName = dmReasoningAgentName(thinkingMsg?.source);
            transformMessages(msgs => {
                const filtered = msgs.filter(m => !(m.type === 'thinking' && m.queuedBehind > 0));
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

    // -- status: agent phase update (live indicator in header bar) --
    // Note: no source_agent filter needed -- subagent status events are
    // routed to their own session streams, not the parent's.  If that
    // routing ever changes, the header bar would flash subagent phases
    // and a source_agent guard would need to be added here.
    on('status', (e) => {
        const data = JSON.parse(e.data);
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
        const data = JSON.parse(e.data);
        if (data.source_agent) return; // suppress subagent interleaving
        const isDm = activeSession.value?.session_type === 'dm';
        if (isDm) {
            // For DM sessions: accumulate thinking text into the per-run
            // buffer instead of the main chat delta buffer. The reasoning
            // text is displayed inside collapsible DmReasoningBlock
            // components, not as standalone agent messages. This prevents
            // ghost messages that vanish on reload. (#685)
            const runId = activeRunId.value;
            if (runId) {
                const prev = dmThinkingBuffers.value;
                const next = new Map(prev);
                next.set(runId, (next.get(runId) || '') + data.delta);
                dmThinkingBuffers.value = next;
            }
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
        const data = JSON.parse(e.data);
        if (data.source_agent) return; // subagent reasoning is suppressed
        const delta = data.text || '';
        if (!delta) return;
        const isDm = activeSession.value?.session_type === 'dm';
        if (isDm) {
            // For DM sessions: accumulate reasoning text into the per-run
            // thinking buffer instead of mutating chatMessages.  The
            // DmReasoningBlock for the active run reads this buffer and
            // displays the text inside the collapsible "thinking" pane.
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
            const runId = activeRunId.value;
            if (runId) {
                const prev = dmThinkingBuffers.value;
                const next = new Map(prev);
                next.set(runId, (next.get(runId) || '') + delta);
                dmThinkingBuffers.value = next;
            }
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

    // -- tool_start --
    on('tool_start', (e) => {
        batch(() => {
            flushDeltaBuffer();
            const data = JSON.parse(e.data);
            const toolId = data.tool_invocation_id || data.call_id || nextMsgId();
            const runId = data.run_id || activeRunId.value || null;
            // Diagnostic: log tool count before insertion for #501 investigation.
            const toolCountBefore = chatMessages.value.filter(m => m.type === 'tool').length;
            console.debug('[tool_start]', data.tool, 'id=' + toolId,
                'tool count before insertion:', toolCountBefore);

            const startedAt = Date.now();
            const isDm = activeSession.value?.session_type === 'dm';

            if (isDm && !data.source_agent) {
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
            } else if (data.tool === 'invoke_agent') {
                sealLastAgent();
                const name = data.params?.name || data.params?.subagent_name || 'subagent';
                const task = data.params?.task || '';
                appendMessage({
                    id: toolId, type: 'tool', tool: 'invoke_agent', params: data.params,
                    status: 'running', startedAt, runId,
                });
                trackSubagentStart(name, task, toolId);
            } else if (data.source_agent) {
                trackSubagentTool(data.source_agent, {
                    id: toolId, tool: data.tool, params: data.params, status: 'running',
                });
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
            const data = JSON.parse(e.data);
            const matchId = data.tool_invocation_id;
            const status = data.ok ? 'done' : 'fail';

            if (data.source_agent) {
                trackSubagentTool(data.source_agent, { id: matchId, status, result: data.result });
            }

            const endedAt = Date.now();

            const applyToolEnd = (m) => {
                const durationMs = m.startedAt ? endedAt - m.startedAt : null;
                return { ...m, status, result: data.result, durationMs };
            };

            // DM reasoning blocks: update the matching tool inside the
            // block's tools array rather than updating a standalone message.
            const isDm = activeSession.value?.session_type === 'dm';
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
            if (found) {
                // Check if the matched tool was invoke_agent and track subagent end.
                // Re-read from the signal since updateMessage already wrote it.
                const updated = chatMessages.value;
                const updatedMsg = matchId
                    ? updated.find(m => m.type === 'tool' && m.id === matchId)
                    : updated.findLast(m => m.type === 'tool' && m.status === status);
                if (updatedMsg && updatedMsg.tool === 'invoke_agent') {
                    const resultObj = typeof data.result === 'object' ? data.result : null;
                    const isBackground = resultObj && resultObj.task_id;
                    if (!isBackground) {
                        // Resolve the subagent name: prefer the explicit name
                        // from params, fall back to looking up the entry by the
                        // invoke_agent tool_invocation_id (handles unnamed
                        // subagents whose bar entry may have been renamed by
                        // trackSubagentTool to the backend-assigned label).
                        const name = updatedMsg.params?.name
                            || updatedMsg.params?.subagent_name
                            || findSubagentByToolInvocationId(matchId);
                        if (name) {
                            // Capture session_id from invoke_agent result for
                            // drill-down navigation before ending the subagent.
                            if (resultObj && resultObj.session_id) {
                                setSubagentSessionId(name, resultObj.session_id);
                            }
                            trackSubagentEnd(name, status);
                        }
                    }
                }
            } else if (!data.source_agent) {
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
            const data = JSON.parse(e.data);
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

    // -- subagent_completed --
    on('subagent_completed', (e) => {
        batch(() => {
            const data = JSON.parse(e.data);
            const name = data.subagent_name || 'subagent';
            const status = data.status || 'done';
            const sessionId = data.subagent_session_id || null;
            const summary = data.summary || '';

            // Look up subagent entry for metadata (task, tool count, duration)
            const entry = activeSubagents.value[name]
                || Object.values(activeSubagents.value).find(
                    e => e.displayName === name || (name === 'subagent' && e.status === 'running')
                );

            const task = entry ? entry.task : '';
            const toolCount = entry ? entry.tools.length : 0;
            const durationMs = entry && entry.startedAt ? Date.now() - entry.startedAt : null;

            // If subagent_session_id is provided, store it on the subagent entry
            if (sessionId) {
                setSubagentSessionId(name, sessionId);
            }

            // Update SubagentBar (stays visible until auto-remove delay)
            trackSubagentEnd(name, status);

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
        const data = JSON.parse(e.data);
        appendMessage({
            id: nextMsgId(),
            type: 'job_completed',
            jobName: data.job_name || 'job',
            status: data.status || 'success',
            summary: data.summary || '',
            ts: data.ts || null,
        });
    });

    // -- dm_message: live DM message from peer agent (#632) --
    on('dm_message', (e) => {
        batch(() => {
            flushDeltaBuffer();
            sealLastAgent();
            const data = JSON.parse(e.data);
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
        const data = JSON.parse(e.data);
        const peer = data.peer || 'unknown';
        const reason = DM_END_REASON_LABELS[data.reason] || data.reason || 'conversation ended';
        appendMessage({
            id: nextMsgId(), type: 'dm_ended', peer, reason,
        });
        // Reset the status bar -- the DM conversation is over, so the
        // "Chatting with {peer}..." phase and dmPeer context must be
        // cleared to return to idle state.
        clearAgentPhase();
    });

    // -- dm_activity_started: DM run started on behalf of this agent (#659) --
    // Forwarded from the DM session to the webchat session so the UI can
    // show "Chatting with {peer}..." even when viewing the main session.
    on('dm_activity_started', (e) => {
        const data = JSON.parse(e.data);
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
        const data = JSON.parse(e.data);
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
        const data = JSON.parse(e.data);
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
    // execution) or immediately after deny (ok=false, "denied by user").
    on('approval_resolved', (e) => {
        const data = JSON.parse(e.data);
        filterMessages(
            m => !(m.type === 'approval' && m.approvalId === data.approval_id),
        );
    });

    // -- context_debug: full context window snapshot (debug mode) --
    on('context_debug', (e) => {
        batch(() => {
            const data = JSON.parse(e.data);
            appendMessage({
                id: nextMsgId(),
                type: 'context_debug',
                messages: data.messages,
                toolNames: data.tool_names,
                totalTokens: data.total_tokens,
                systemTokens: data.system_tokens,
                historyMessageCount: data.history_message_count,
            });
        });
    });

    // -- run_warning (non-fatal, e.g. max iterations) --
    // Subagent warnings (source_agent set) are suppressed in the parent
    // chat -- they are visible via the SubagentBar drill-down into the
    // subagent's own session.  (#602)
    on('run_warning', (e) => {
        const data = JSON.parse(e.data);
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
            const data = e.data ? JSON.parse(e.data) : {};
            const isDm = activeSession.value?.session_type === 'dm';

            // Build the approval-resolution-and-append phase via
            // transformMessages so it results in a single signal write.
            // Note: flushDeltaBuffer() and sealLastAgent() above may each
            // write to chatMessages.value independently, but this section
            // (approval resolution + appended status/error/token messages)
            // is collapsed into one write to avoid intermediate states.
            const endingRunId = data.run_id || null;

            // Read and save thinking text BEFORE deleting from buffer,
            // so the transformMessages callback below can use it.
            // (C1 fix: previously the delete happened first, then the
            // callback read the updated signal and got empty string.)
            let savedThinkingText = '';
            if (isDm && endingRunId) {
                savedThinkingText = dmThinkingBuffers.value.get(endingRunId) || '';
                if (savedThinkingText) {
                    const next = new Map(dmThinkingBuffers.value);
                    next.delete(endingRunId);
                    dmThinkingBuffers.value = next;
                }
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
            activeRunId.value = null;

            // Preserve DM context across runs (#688): when the agent is
            // in a DM conversation, individual run endings should NOT
            // clear the "Chatting with..." status. The status only clears
            // when the entire DM conversation ends (dm_conversation_ended).
            // For non-DM runs, clear the phase as before.
            if (dmPeer.value) {
                setAgentPhase('dm', dmPeer.value);
            } else {
                clearAgentPhase();
            }

            // The run has ended -- the user message is either persisted
            // (finished/error after execution started) or was never
            // persisted (cancelled before execution).  Either way, the
            // session history is now the source of truth.
            //
            // Use the stream's closure-captured sessionId (not
            // activeSessionId.value) because the user may have switched
            // to a different session before this run ended.
            clearPendingMessage(sessionId);
        });

        // Process queued user messages via dynamic import
        // (avoids circular dependency with input-area.js)
        if (messageQueue.value.length > 0) {
            const next = messageQueue.value[0];
            messageQueue.value = messageQueue.value.slice(1);
            // Capture activeSessionId synchronously before the async
            // import().then() microtask gap -- the value could change
            // if the user switches sessions between now and when the
            // .then() callback fires.  (Fixes #526)
            const capturedSessionId = activeSessionId.value;
            import('../components/chat/input-area.js').then(mod => {
                if (mod.startRun) mod.startRun(next.text, { sessionId: capturedSessionId });
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
                return;
            }
            const delay = Math.min(2000 * Math.pow(2, sessionRetryCount - 1), 30000);
            setTimeout(() => {
                if (activeSessionId.value === sessionId) {
                    openSessionStream(sessionId, { lastEventId: lastSeenEventId });
                }
            }, delay);
        }
    };
}

export function closeSessionStream() {
    if (flushTimer !== null) {
        cancelAnimationFrame(flushTimer);
        flushTimer = null;
    }
    flushDeltaBuffer();
    // Reset per-run state so it does not carry over to the next session.
    sawTokenDelta = false;
    clearAgentPhase();
    if (activeSessionEs) {
        activeSessionEs.close();
        activeSessionEs = null;
    }
}

export function isSessionStreamOpen() {
    return activeSessionEs !== null;
}
