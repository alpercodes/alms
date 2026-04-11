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

import { batch } from '../deps.js';
import { chatMessages, nextMsgId } from '../state/chat.js';
import { appendMessage, updateMessage, transformMessages } from '../state/chat-actions.js';
import { activeRunId } from '../state/runs.js';
import { trackSubagentStart, trackSubagentEnd, trackSubagentTool, findSubagentByToolInvocationId } from '../state/subagents.js';
import { setAgentPhase, clearAgentPhase } from '../state/agent-status.js';
import { messageQueue } from '../state/queue.js';
import { activeSessionId } from '../state/sessions.js';
import { normalizeApproval } from '../utils/approvals.js';
import { selectGeneration } from '../state/select-generation.js';

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
            copy.push({ id: nextMsgId(), type: 'agent', role: 'assistant', text: pending, sealed: false });
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

        // Cross-channel DM awareness: when the run source starts with
        // "peer:", the agent is responding to a DM from another agent.
        // Set the header bar phase to 'dm' with the peer name so the
        // user sees "Chatting with {peer}..." while this run executes.
        if (data.source && data.source.startsWith('peer:')) {
            setAgentPhase('dm', data.source.slice(5));
        }

        if (data.is_notification) {
            // Notification run from subagent completion or peer message --
            // show thinking indicator with source context
            batch(() => {
                activeRunId.value = data.run_id;
                appendMessage({
                    id: nextMsgId(), type: 'thinking', source: data.source, queuedBehind,
                });
            });
        } else if (queuedBehind > 0) {
            // User-initiated run but agent is busy -- update the existing
            // thinking indicator (added by startRun) with queue position
            batch(() => {
                activeRunId.value = data.run_id;
                updateMessage(
                    m => m.type === 'thinking',
                    m => ({ ...m, queuedBehind }),
                );
            });
        } else {
            activeRunId.value = data.run_id;
        }
        // else: user-initiated, queue empty -- thinking indicator from startRun is fine
    });

    // -- run_started: the run has been dequeued and is now executing --
    on('run_started', (_e) => {
        // Transition thinking indicator from "queued" to active "Thinking..."
        updateMessage(
            m => m.type === 'thinking' && m.queuedBehind > 0,
            m => ({ ...m, queuedBehind: 0 }),
        );
    });

    // -- status: agent phase update (live indicator in header bar) --
    on('status', (e) => {
        const data = JSON.parse(e.data);
        setAgentPhase(data.phase, data.detail || null);
    });

    // -- run_created: track DM-triggered runs for cross-channel awareness --
    // When source starts with "peer:", the agent is responding to a DM.
    // Set the phase to 'dm' so the header bar shows "Chatting with {peer}...".
    // This is handled inside the existing run_created handler below via
    // the dmPeerFromSource helper.

    // -- token_delta --
    on('token_delta', (e) => {
        const data = JSON.parse(e.data);
        if (data.source_agent) return; // suppress subagent interleaving
        sawTokenDelta = true;
        deltaBuffer += data.delta;
        scheduleFlush();
    });

    // -- tool_start --
    on('tool_start', (e) => {
        batch(() => {
            flushDeltaBuffer();
            const data = JSON.parse(e.data);
            const toolId = data.tool_invocation_id || data.call_id || nextMsgId();
            // Diagnostic: log tool count before insertion for #501 investigation.
            const toolCountBefore = chatMessages.value.filter(m => m.type === 'tool').length;
            console.debug('[tool_start]', data.tool, 'id=' + toolId,
                'tool count before insertion:', toolCountBefore);

            const startedAt = Date.now();

            if (data.tool === 'invoke_agent') {
                sealLastAgent();
                const name = data.params?.name || data.params?.subagent_name || 'subagent';
                const task = data.params?.task || '';
                appendMessage({
                    id: toolId, type: 'tool', tool: 'invoke_agent', params: data.params,
                    status: 'running', startedAt,
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
                    status: 'running', startedAt,
                });
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

            // Update SubagentBar (stays visible until notification run finishes)
            trackSubagentEnd(name, status);

            // Show system message in chat
            const label = status === 'done' ? 'completed'
                : status === 'fail' ? 'failed'
                : status === 'cancelled' ? 'cancelled' : 'completed';
            appendMessage({
                id: nextMsgId(), type: 'system',
                text: `Subagent '${name}' ${label}.`,
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

    // -- dm_conversation_ended --
    on('dm_conversation_ended', (e) => {
        const data = JSON.parse(e.data);
        const peer = data.peer || 'unknown';
        const reasonLabels = {
            'ignored': 'no further replies',
            'depth_exceeded': 'message limit reached',
        };
        const reason = reasonLabels[data.reason] || data.reason || 'conversation ended';
        appendMessage({
            id: nextMsgId(), type: 'dm_ended', peer, reason,
        });
    });

    // -- approval_resolved --
    on('approval_resolved', (e) => {
        const data = JSON.parse(e.data);
        updateMessage(
            m => m.type === 'approval' && m.approvalId === data.approval_id,
            m => ({ ...m, resolved: true, decision: data.decision }),
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
    on('run_warning', (e) => {
        batch(() => {
            flushDeltaBuffer();
            sealLastAgent();
            const data = JSON.parse(e.data);
            const code = data.warning?.code || 'UNKNOWN';
            const msg = data.warning?.message || 'Warning';
            appendMessage({ id: nextMsgId(), type: 'warning', code, text: msg });
        });
    });

    // -- run_finished / run_error / run_cancelled --
    const handleRunEnd = (status) => (e) => {
        batch(() => {
            flushDeltaBuffer();
            sealLastAgent();
            const data = e.data ? JSON.parse(e.data) : {};

            // Build the approval-resolution-and-append phase via
            // transformMessages so it results in a single signal write.
            // Note: flushDeltaBuffer() and sealLastAgent() above may each
            // write to chatMessages.value independently, but this section
            // (approval resolution + appended status/error/token messages)
            // is collapsed into one write to avoid intermediate states.
            const endingRunId = data.run_id || null;
            const decision = status === 'cancelled' ? 'cancelled'
                : status === 'error' ? 'cancelled' : 'expired';
            transformMessages(prev => {
                const toolCountBefore = prev.filter(m => m.type === 'tool').length;

                // Resolve any pending approval cards for this run.
                // When a run ends (cancelled, error, or finished), any unresolved
                // approval prompts are stale and must be dismissed so the user is
                // not left with dangling Approve/Deny buttons.  (Fixes #487 Bug 1)
                //
                // Scoped to the ending run's ID so concurrent runs (future) do not
                // accidentally dismiss each other's approval cards.  Approval cards
                // without a runId (legacy) are always resolved as a fallback.
                const isStaleApproval = (m) =>
                    m.type === 'approval' && !m.resolved
                    && (!m.runId || !endingRunId || m.runId === endingRunId);

                // Mark any still-running tool messages as cancelled.
                // When a run is cancelled (or errors) mid-tool-execution, the
                // backend emits run_cancelled but never emits tool_end for in-
                // flight tools, leaving the spinner animation stuck.  (Fixes #593)
                const isStuckTool = (m) =>
                    m.type === 'tool' && m.status === 'running';

                let msgs = prev.map(m => {
                    if (isStaleApproval(m)) {
                        return { ...m, resolved: true, decision };
                    }
                    if (isStuckTool(m)) {
                        return { ...m, status: 'cancelled' };
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
                if (status === 'finished' && !sawTokenDelta) {
                    // Only show "(run completed)" for runs that had no streamed
                    // response (e.g. DM runs using send_message, tool-only runs).
                    // Normal chat runs already display the streamed text.
                    msgs = [...msgs, { id: nextMsgId(), type: 'system', text: '(run completed)' }];
                }

                const usage = (data.prompt_tokens || data.completion_tokens)
                    ? { prompt_tokens: data.prompt_tokens || 0, completion_tokens: data.completion_tokens || 0 }
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
            clearAgentPhase();
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
