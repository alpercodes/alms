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
 */

import { batch } from '../deps.js';
import { chatMessages, nextMsgId } from '../state/chat.js';
import { activeRunId } from '../state/runs.js';
import { trackSubagentStart, trackSubagentEnd, trackSubagentTool, clearCompletedSubagents } from '../state/subagents.js';
import { messageQueue } from '../state/queue.js';
import { activeSessionId } from '../state/sessions.js';
import { normalizeApproval } from '../utils/approvals.js';

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
    const msgs = chatMessages.value.filter(m => m.type !== 'thinking');
    const copy = [...msgs];
    const last = copy[copy.length - 1];
    if (last && last.type === 'agent' && !last.sealed) {
        copy[copy.length - 1] = { ...last, text: last.text + pending };
    } else {
        copy.push({ id: nextMsgId(), type: 'agent', role: 'assistant', text: pending, sealed: false });
    }
    chatMessages.value = copy;
}

function scheduleFlush() {
    if (flushTimer === null) {
        flushTimer = requestAnimationFrame(flushDeltaBuffer);
    }
}

function sealLastAgent() {
    const msgs = chatMessages.value;
    const hasThinking = msgs.some(m => m.type === 'thinking');
    const filtered = hasThinking ? msgs.filter(m => m.type !== 'thinking') : msgs;
    const last = filtered[filtered.length - 1];
    if (last && last.type === 'agent' && !last.sealed) {
        const updated = [...filtered];
        updated[updated.length - 1] = { ...last, sealed: true };
        chatMessages.value = updated;
    } else if (hasThinking) {
        chatMessages.value = filtered;
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
     */
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
        }
        handler(e);
    });

    // -- run_created: a new run was created on this session --
    on('run_created', (e) => {
        const data = JSON.parse(e.data);
        const queuedBehind = data.queued_behind || 0;
        sawTokenDelta = false;

        if (data.is_notification) {
            // Notification run from subagent completion or peer message --
            // show thinking indicator with source context
            batch(() => {
                activeRunId.value = data.run_id;
                chatMessages.value = [...chatMessages.value, {
                    id: nextMsgId(), type: 'thinking', source: data.source, queuedBehind,
                }];
            });
        } else if (queuedBehind > 0) {
            // User-initiated run but agent is busy -- update the existing
            // thinking indicator (added by startRun) with queue position
            batch(() => {
                activeRunId.value = data.run_id;
                const msgs = [...chatMessages.value];
                const idx = msgs.findLastIndex(m => m.type === 'thinking');
                if (idx >= 0) {
                    msgs[idx] = { ...msgs[idx], queuedBehind };
                }
                chatMessages.value = msgs;
            });
        } else {
            activeRunId.value = data.run_id;
        }
        // else: user-initiated, queue empty -- thinking indicator from startRun is fine
    });

    // -- run_started: the run has been dequeued and is now executing --
    on('run_started', (_e) => {
        // Transition thinking indicator from "queued" to active "Thinking..."
        const msgs = [...chatMessages.value];
        const idx = msgs.findLastIndex(m => m.type === 'thinking');
        if (idx >= 0 && msgs[idx].queuedBehind > 0) {
            msgs[idx] = { ...msgs[idx], queuedBehind: 0 };
            chatMessages.value = msgs;
        }
    });

    // -- status: agent phase update --
    // Phase values correspond to constants in alms-runtime/src/events.rs:
    //   PHASE_BUILDING_CONTEXT = "building_context"
    //   PHASE_SUMMARIZING      = "summarizing"
    //   PHASE_CALLING_LLM      = "calling_llm"
    //   PHASE_EXECUTING_TOOLS  = "executing_tools"
    on('status', (e) => {
        const data = JSON.parse(e.data);
        const msgs = [...chatMessages.value];
        const idx = msgs.findLastIndex(m => m.type === 'thinking');
        if (idx >= 0) {
            msgs[idx] = { ...msgs[idx], phase: data.phase, phaseDetail: data.detail || null };
            chatMessages.value = msgs;
        } else {
            // Thinking indicator was removed by token_delta flush or tool_start
            // (e.g. on iteration 2+ of the agent loop). Re-add it so the user
            // sees the current phase ("Running tools...", "Thinking...", etc.).
            msgs.push({ id: nextMsgId(), type: 'thinking', phase: data.phase, phaseDetail: data.detail || null });
            chatMessages.value = msgs;
        }
    });

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

            const startedAt = Date.now();

            if (data.tool === 'invoke_agent') {
                sealLastAgent();
                const name = data.params?.name || data.params?.subagent_name || 'subagent';
                const task = data.params?.task || '';
                chatMessages.value = [...chatMessages.value, {
                    id: toolId, type: 'tool', tool: 'invoke_agent', params: data.params,
                    status: 'running', startedAt,
                }];
                trackSubagentStart(name, task);
            } else if (data.source_agent) {
                trackSubagentTool(data.source_agent, {
                    id: toolId, tool: data.tool, params: data.params, status: 'running',
                });
            } else {
                sealLastAgent();
                chatMessages.value = [...chatMessages.value, {
                    id: toolId, type: 'tool', tool: data.tool, params: data.params,
                    status: 'running', startedAt,
                }];
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

            const msgs = [...chatMessages.value];
            // Primary match: by tool_invocation_id (exact ID correlation).
            // Fallback: if the primary match fails (e.g. tool message was
            // reconstructed from history with a different ID scheme), fall
            // back to matching the last running tool message.
            let idx = matchId
                ? msgs.findLastIndex(m => m.type === 'tool' && m.id === matchId)
                : -1;
            if (idx < 0) {
                idx = msgs.findLastIndex(m => m.type === 'tool' && m.status === 'running');
            }
            if (idx >= 0) {
                const durationMs = msgs[idx].startedAt ? endedAt - msgs[idx].startedAt : null;
                msgs[idx] = { ...msgs[idx], status, result: data.result, durationMs };
                if (msgs[idx].tool === 'invoke_agent') {
                    const name = msgs[idx].params?.name || msgs[idx].params?.subagent_name;
                    const resultObj = typeof data.result === 'object' ? data.result : null;
                    const isBackground = resultObj && resultObj.task_id;
                    if (name && !isBackground) {
                        trackSubagentEnd(name, status);
                    }
                }
                chatMessages.value = msgs;
            }
            // When no matching tool message was found (e.g. subagent-only tool
            // events), skip the chatMessages write to avoid an unnecessary
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
            // (Fixes #487 Bug 2 — prevents duplicate approval prompts)
            const alreadyExists = chatMessages.value.some(
                m => m.type === 'approval' && m.approvalId === data.approval_id
            );
            if (!alreadyExists) {
                const norm = normalizeApproval(data);
                chatMessages.value = [...chatMessages.value, {
                    id: nextMsgId(), type: 'approval', approvalId: norm.approvalId,
                    tool: norm.tool, params: norm.params, runId: norm.runId,
                    resolved: false,
                }];
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
            chatMessages.value = [...chatMessages.value, {
                id: nextMsgId(), type: 'system',
                text: `Subagent '${name}' ${label}.`,
            }];
        });
    });

    // -- job_completed --
    on('job_completed', (e) => {
        const data = JSON.parse(e.data);
        const name = data.job_name || 'job';
        const status = data.status === 'success' ? 'completed'
            : data.status === 'cancelled' ? 'cancelled' : 'failed';
        const summary = data.summary ? `: ${data.summary}` : '';
        chatMessages.value = [...chatMessages.value, {
            id: nextMsgId(), type: 'system',
            text: `Scheduled job ${status} -- ${name}${summary}`,
        }];
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
        chatMessages.value = [...chatMessages.value, {
            id: nextMsgId(), type: 'dm_ended', peer, reason,
        }];
    });

    // -- approval_resolved --
    on('approval_resolved', (e) => {
        const data = JSON.parse(e.data);
        const msgs = [...chatMessages.value];
        const idx = msgs.findLastIndex(m => m.type === 'approval' && m.approvalId === data.approval_id);
        if (idx >= 0) {
            msgs[idx] = { ...msgs[idx], resolved: true, decision: data.decision };
            chatMessages.value = msgs;
        }
    });

    // -- context_debug: full context window snapshot (debug mode) --
    on('context_debug', (e) => {
        batch(() => {
            const data = JSON.parse(e.data);
            chatMessages.value = [...chatMessages.value, {
                id: nextMsgId(),
                type: 'context_debug',
                messages: data.messages,
                toolNames: data.tool_names,
                totalTokens: data.total_tokens,
                systemTokens: data.system_tokens,
                historyMessageCount: data.history_message_count,
            }];
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
            chatMessages.value = [...chatMessages.value, { id: nextMsgId(), type: 'warning', code, text: msg }];
        });
    });

    // -- run_finished / run_error / run_cancelled --
    const handleRunEnd = (status) => (e) => {
        batch(() => {
            flushDeltaBuffer();
            sealLastAgent();
            const data = e.data ? JSON.parse(e.data) : {};

            // Resolve any pending approval cards for this run.
            // When a run ends (cancelled, error, or finished), any unresolved
            // approval prompts are stale and must be dismissed so the user is
            // not left with dangling Approve/Deny buttons.  (Fixes #487 Bug 1)
            //
            // Scoped to the ending run's ID so concurrent runs (future) do not
            // accidentally dismiss each other's approval cards.  Approval cards
            // without a runId (legacy) are always resolved as a fallback.
            const endingRunId = data.run_id || null;
            const decision = status === 'cancelled' ? 'cancelled'
                : status === 'error' ? 'cancelled' : 'expired';
            let msgs = chatMessages.value;
            const isStaleApproval = (m) =>
                m.type === 'approval' && !m.resolved
                && (!m.runId || !endingRunId || m.runId === endingRunId);
            const hasUnresolved = msgs.some(isStaleApproval);
            if (hasUnresolved) {
                msgs = msgs.map(m =>
                    isStaleApproval(m)
                        ? { ...m, resolved: true, decision }
                        : m
                );
                chatMessages.value = msgs;
            }

            // Collect all new messages to append in a single batch to avoid
            // multiple intermediate re-renders (error + tokens + activeRunId).
            const append = [];

            if (status === 'error') {
                const code = data.error?.code || 'INTERNAL';
                const rawMsg = typeof data.error === 'string'
                    ? data.error : (data.error?.message || 'Run failed');
                const text = friendlyErrorMessage(code, rawMsg);
                append.push({ id: nextMsgId(), type: 'error', code, text });
            }
            if (status === 'cancelled') {
                append.push({ id: nextMsgId(), type: 'system', text: '(run cancelled)' });
            }
            if (status === 'finished' && !sawTokenDelta) {
                // Only show "(run completed)" for runs that had no streamed
                // response (e.g. DM runs using send_message, tool-only runs).
                // Normal chat runs already display the streamed text.
                append.push({ id: nextMsgId(), type: 'system', text: '(run completed)' });
            }

            const usage = (data.prompt_tokens || data.completion_tokens)
                ? { prompt_tokens: data.prompt_tokens || 0, completion_tokens: data.completion_tokens || 0 }
                : data.usage;
            if (usage) {
                append.push({ id: nextMsgId(), type: 'tokens', usage });
            }

            if (append.length > 0) {
                chatMessages.value = [...chatMessages.value, ...append];
            }
            activeRunId.value = null;
        });

        clearCompletedSubagents();

        // Process queued user messages via dynamic import
        // (avoids circular dependency with input-area.js)
        if (messageQueue.value.length > 0) {
            const next = messageQueue.value[0];
            messageQueue.value = messageQueue.value.slice(1);
            import('../components/chat/input-area.js').then(mod => {
                if (mod.startRun) mod.startRun(next.text);
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
    if (activeSessionEs) {
        activeSessionEs.close();
        activeSessionEs = null;
    }
}

export function isSessionStreamOpen() {
    return activeSessionEs !== null;
}
