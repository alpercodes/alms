/**
 * Session-level SSE stream — persistent connection across runs.
 *
 * Opens EventSource to /sessions/{sessionId}/events and handles all
 * event types. Stays open across runs — notification runs, background
 * subagent completions, and job runs all arrive on the same stream.
 */

import { chatMessages } from '../state/chat.js';
import { activeRunId } from '../state/runs.js';
import { trackSubagentStart, trackSubagentEnd, trackSubagentTool, clearCompletedSubagents } from '../state/subagents.js';
import { messageQueue } from '../state/queue.js';
import { activeSessionId } from '../state/sessions.js';

/**
 * Map error codes to user-friendly messages.
 * Falls back to the raw message if the code is not recognised.
 */
function friendlyErrorMessage(code, rawMsg) {
    switch (code) {
        case 'AUTH':
            return 'Authentication failed \u2014 check your API key in Settings.';
        case 'RATE_LIMIT':
            return 'Rate limited by the LLM provider \u2014 wait a moment and try again.';
        case 'TIMEOUT':
            return 'Request timed out \u2014 the LLM provider did not respond in time.';
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
 * Highest SSE event ID seen on the current stream — used for manual reconnect.
 *
 * Type note: this value may be a number (when seeded from the REST API's
 * `last_event_id` integer field) or a string (when updated from the SSE
 * spec's `e.lastEventId`).  Both are coerced to a string via `String()`
 * before being sent as the `?last_event_id` query parameter, so the mixed
 * representation is harmless in practice.
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
        copy.push({ type: 'agent', role: 'assistant', text: pending, sealed: false });
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
 * Stays open across runs — all events for this session arrive here.
 *
 * @param {string} sessionId
 * @param {object} [opts]
 * @param {number} [opts.lastEventId] — skip replay of events up to (and
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

    /** Wrap an event handler to track the highest seen SSE event ID. */
    const on = (type, handler) => es.addEventListener(type, (e) => {
        if (e.lastEventId) lastSeenEventId = e.lastEventId;
        handler(e);
    });

    // ── run_created: a new run was created on this session ──
    on('run_created', (e) => {
        const data = JSON.parse(e.data);
        activeRunId.value = data.run_id;
        const queuedBehind = data.queued_behind || 0;

        if (data.is_notification) {
            // Notification run from subagent completion or peer message —
            // show thinking indicator with source context
            chatMessages.value = [...chatMessages.value, {
                type: 'thinking', source: data.source, queuedBehind,
            }];
        } else if (queuedBehind > 0) {
            // User-initiated run but agent is busy — update the existing
            // thinking indicator (added by startRun) with queue position
            const msgs = [...chatMessages.value];
            const idx = msgs.findLastIndex(m => m.type === 'thinking');
            if (idx >= 0) {
                msgs[idx] = { ...msgs[idx], queuedBehind };
            }
            chatMessages.value = msgs;
        }
        // else: user-initiated, queue empty — thinking indicator from startRun is fine
    });

    // ── run_started: the run has been dequeued and is now executing ──
    on('run_started', (e) => {
        // Transition thinking indicator from "queued" to active "Thinking..."
        const msgs = [...chatMessages.value];
        const idx = msgs.findLastIndex(m => m.type === 'thinking');
        if (idx >= 0 && msgs[idx].queuedBehind > 0) {
            msgs[idx] = { ...msgs[idx], queuedBehind: 0 };
            chatMessages.value = msgs;
        }
    });

    // ── status: agent phase update ──
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
            msgs.push({ type: 'thinking', phase: data.phase, phaseDetail: data.detail || null });
            chatMessages.value = msgs;
        }
    });

    // ── token_delta ──
    on('token_delta', (e) => {
        const data = JSON.parse(e.data);
        if (data.source_agent) return; // suppress subagent interleaving
        deltaBuffer += data.delta;
        scheduleFlush();
    });

    // ── tool_start ──
    on('tool_start', (e) => {
        flushDeltaBuffer();
        const data = JSON.parse(e.data);
        const toolId = data.tool_invocation_id || data.call_id || data.tool;

        if (data.tool === 'invoke_agent') {
            sealLastAgent();
            const name = data.params?.name || data.params?.subagent_name || 'subagent';
            const task = data.params?.task || '';
            chatMessages.value = [...chatMessages.value, {
                type: 'tool', tool: 'invoke_agent', params: data.params,
                status: 'running', id: toolId,
            }];
            trackSubagentStart(name, task);
        } else if (data.source_agent) {
            trackSubagentTool(data.source_agent, {
                id: toolId, tool: data.tool, params: data.params, status: 'running',
            });
        } else {
            sealLastAgent();
            chatMessages.value = [...chatMessages.value, {
                type: 'tool', tool: data.tool, params: data.params,
                status: 'running', id: toolId,
            }];
        }
    });

    // ── tool_end ──
    on('tool_end', (e) => {
        const data = JSON.parse(e.data);
        const matchId = data.tool_invocation_id;
        const status = data.ok ? 'done' : 'fail';

        if (data.source_agent) {
            trackSubagentTool(data.source_agent, { id: matchId, status, result: data.result });
        }

        const msgs = [...chatMessages.value];
        const idx = matchId
            ? msgs.findLastIndex(m => m.type === 'tool' && m.id === matchId)
            : msgs.findLastIndex(m => m.type === 'tool' && m.status === 'running');
        if (idx >= 0) {
            msgs[idx] = { ...msgs[idx], status, result: data.result };
            if (msgs[idx].tool === 'invoke_agent') {
                const name = msgs[idx].params?.name || msgs[idx].params?.subagent_name;
                const resultObj = typeof data.result === 'object' ? data.result : null;
                const isBackground = resultObj && resultObj.task_id;
                if (name && !isBackground) {
                    trackSubagentEnd(name, status);
                }
            }
        }
        chatMessages.value = msgs;
    });

    // ── approval_required ──
    on('approval_required', (e) => {
        flushDeltaBuffer();
        const data = JSON.parse(e.data);
        chatMessages.value = [...chatMessages.value, {
            type: 'approval', approvalId: data.approval_id,
            tool: data.capability, params: data.request, resolved: false,
        }];
        sealLastAgent();
    });

    // ── subagent_completed ──
    on('subagent_completed', (e) => {
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
            type: 'system',
            text: `Subagent '${name}' ${label}.`,
        }];
    });

    // ── job_completed ──
    on('job_completed', (e) => {
        const data = JSON.parse(e.data);
        const name = data.job_name || 'job';
        const status = data.status === 'success' ? 'completed'
            : data.status === 'cancelled' ? 'cancelled' : 'failed';
        const summary = data.summary ? `: ${data.summary}` : '';
        chatMessages.value = [...chatMessages.value, {
            type: 'system',
            text: `Scheduled job ${status} — ${name}${summary}`,
        }];
    });

    // ── approval_resolved ──
    on('approval_resolved', (e) => {
        const data = JSON.parse(e.data);
        const msgs = [...chatMessages.value];
        const idx = msgs.findLastIndex(m => m.type === 'approval' && m.approvalId === data.approval_id);
        if (idx >= 0) {
            msgs[idx] = { ...msgs[idx], resolved: true, decision: data.decision };
        }
        chatMessages.value = msgs;
    });

    // ── run_warning (non-fatal, e.g. max iterations) ──
    on('run_warning', (e) => {
        flushDeltaBuffer();
        sealLastAgent();
        const data = JSON.parse(e.data);
        const code = data.warning?.code || 'UNKNOWN';
        const msg = data.warning?.message || 'Warning';
        chatMessages.value = [...chatMessages.value, { type: 'warning', code, text: msg }];
    });

    // ── run_finished / run_error / run_cancelled ──
    const handleRunEnd = (status) => (e) => {
        flushDeltaBuffer();
        sealLastAgent();
        const data = e.data ? JSON.parse(e.data) : {};

        if (status === 'error') {
            const code = data.error?.code || 'INTERNAL';
            const rawMsg = typeof data.error === 'string'
                ? data.error : (data.error?.message || 'Run failed');
            const text = friendlyErrorMessage(code, rawMsg);
            chatMessages.value = [...chatMessages.value, { type: 'error', code, text }];
        }
        if (status === 'cancelled') {
            chatMessages.value = [...chatMessages.value, { type: 'system', text: '(run cancelled)' }];
        }

        const usage = (data.prompt_tokens || data.completion_tokens)
            ? { prompt_tokens: data.prompt_tokens || 0, completion_tokens: data.completion_tokens || 0 }
            : data.usage;
        if (usage) {
            chatMessages.value = [...chatMessages.value, { type: 'tokens', usage }];
        }

        activeRunId.value = null;
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
    if (activeSessionEs) {
        activeSessionEs.close();
        activeSessionEs = null;
    }
}

export function isSessionStreamOpen() {
    return activeSessionEs !== null;
}
