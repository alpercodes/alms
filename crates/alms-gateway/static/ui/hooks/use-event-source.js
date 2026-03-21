import { chatMessages } from '../state/chat.js';
import { activeRunId } from '../state/runs.js';
import { trackSubagentStart, trackSubagentEnd, trackSubagentTool, clearCompletedSubagents } from '../state/subagents.js';
import { getRun } from '../api/runs.js';

// Active foreground EventSource reference
let activeEs = null;

// Run ID for the active stream (local copy so error handler can reference it
// even if activeRunId signal has already been touched)
let activeStreamRunId = null;

// ── Streaming delta buffer ──
// Accumulate token deltas and flush to signal at display refresh rate
// (~60fps), avoiding a full Preact re-render on every single delta event.
let deltaBuffer = '';
let flushTimer = null;

function flushDeltaBuffer() {
    flushTimer = null;
    if (!deltaBuffer) return;
    const pending = deltaBuffer;
    deltaBuffer = '';
    // Remove thinking indicator on first real content
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

/**
 * Open a foreground SSE stream for a run.
 * Updates chatMessages signal as events arrive.
 * @param {string} runId
 * @param {{ onDone: Function }} options
 * @returns {{ close: Function }}
 */
const MAX_SSE_RETRIES = 5;

export function openForegroundStream(runId, { onDone, _retryCount = 0 } = {}) {
    closeActiveStream();

    // EventSource cannot send Authorization headers — use ?token= query param
    const token = localStorage.getItem('alms_auth_token');
    const url = token
        ? `/runs/${runId}/events?token=${encodeURIComponent(token)}`
        : `/runs/${runId}/events`;
    const es = new EventSource(url);
    activeEs = es;
    activeStreamRunId = runId;
    activeRunId.value = runId;

    es.addEventListener('token_delta', (e) => {
        const data = JSON.parse(e.data);
        // Intentionally suppress all subagent token_delta events.
        // Mixing text from multiple concurrent LLM streams into the
        // parent's chat bubble would produce garbled, interleaved output.
        // Subagent work is represented by tool rows instead.
        if (data.source_agent) return;
        const delta = data.delta;
        deltaBuffer += delta;
        scheduleFlush();
    });

    es.addEventListener('tool_start', (e) => {
        flushDeltaBuffer();
        const data = JSON.parse(e.data);
        const toolId = data.tool_invocation_id || data.call_id || data.tool;

        if (data.tool === 'invoke_agent') {
            // Show simple "subagent invoked" in chat
            sealLastAgent();
            const name = data.params?.name || data.params?.subagent_name || 'subagent';
            const task = data.params?.task || '';
            chatMessages.value = [...chatMessages.value, {
                type: 'tool',
                tool: 'invoke_agent',
                params: data.params,
                status: 'running',
                id: toolId,
            }];
            trackSubagentStart(name, task);
        } else if (data.source_agent) {
            // Subagent tool: track in status bar, don't clutter chat
            trackSubagentTool(data.source_agent, {
                id: toolId,
                tool: data.tool,
                params: data.params,
                status: 'running',
            });
        } else {
            // Regular tool: show in chat
            sealLastAgent();
            chatMessages.value = [...chatMessages.value, {
                type: 'tool',
                tool: data.tool,
                params: data.params,
                status: 'running',
                id: toolId,
            }];
        }
    });

    es.addEventListener('tool_end', (e) => {
        const data = JSON.parse(e.data);
        const matchId = data.tool_invocation_id;
        const status = data.ok ? 'done' : 'fail';

        if (data.source_agent) {
            // Update subagent tool status in the status bar
            trackSubagentTool(data.source_agent, {
                id: matchId,
                status,
                result: data.result,
            });
        }

        // Check if this is an invoke_agent completing
        const msgs = [...chatMessages.value];
        const idx = matchId
            ? msgs.findLastIndex(m => m.type === 'tool' && m.id === matchId)
            : msgs.findLastIndex(m => m.type === 'tool' && m.status === 'running');
        if (idx >= 0) {
            msgs[idx] = { ...msgs[idx], status, result: data.result };
            // If invoke_agent finished, update subagent tracking.
            // But NOT for background dispatch — tool_end fires immediately
            // with a task_id result while the subagent is still running.
            if (msgs[idx].tool === 'invoke_agent') {
                const name = msgs[idx].params?.name || msgs[idx].params?.subagent_name;
                const resultObj = typeof data.result === 'string' ? (() => { try { return JSON.parse(data.result); } catch { return null; } })() : data.result;
                const isBackground = resultObj && resultObj.task_id;
                if (name && !isBackground) {
                    trackSubagentEnd(name, status);
                }
            }
        }
        chatMessages.value = msgs;
    });

    es.addEventListener('approval_required', (e) => {
        flushDeltaBuffer();
        const data = JSON.parse(e.data);
        chatMessages.value = [...chatMessages.value, {
            type: 'approval',
            approvalId: data.approval_id,
            tool: data.capability,
            params: data.request,
            resolved: false,
        }];
        sealLastAgent();
    });

    es.addEventListener('approval_resolved', (e) => {
        const data = JSON.parse(e.data);
        const msgs = [...chatMessages.value];
        const idx = msgs.findLastIndex(m => m.type === 'approval' && m.approvalId === data.approval_id);
        if (idx >= 0) {
            msgs[idx] = { ...msgs[idx], resolved: true, decision: data.decision };
        }
        chatMessages.value = msgs;
    });

    const finishHandler = (status) => (e) => {
        flushDeltaBuffer(); // flush any remaining text
        sealLastAgent();
        const data = e.data ? JSON.parse(e.data) : {};
        if (status === 'error') {
            const errMsg = typeof data.error === 'string'
                ? data.error
                : (data.error?.message || 'Run failed');
            chatMessages.value = [...chatMessages.value, {
                type: 'error',
                text: errMsg,
            }];
        }
        if (status === 'cancelled') {
            chatMessages.value = [...chatMessages.value, {
                type: 'system',
                text: '(run cancelled)',
            }];
        }
        // Add token badge if available (Rust sends flat fields, not nested usage)
        const usage = (data.prompt_tokens || data.completion_tokens)
            ? { prompt_tokens: data.prompt_tokens || 0, completion_tokens: data.completion_tokens || 0 }
            : data.usage;
        if (usage) {
            chatMessages.value = [...chatMessages.value, {
                type: 'tokens',
                usage,
            }];
        }
        closeActiveStream();
        activeRunId.value = null;
        clearCompletedSubagents();
        if (onDone) onDone(status, data);
    };

    es.addEventListener('run_finished', finishHandler('finished'));
    es.addEventListener('run_error', finishHandler('error'));
    es.addEventListener('run_cancelled', finishHandler('cancelled'));

    es.onerror = () => {
        // EventSource auto-reconnects (readyState === CONNECTING === 0),
        // but if the connection is permanently lost (readyState === CLOSED === 2),
        // no further reconnection attempts occur.  In that case we must
        // poll the run status and recover — otherwise activeRunId is never
        // cleared and the UI gets stuck showing the thinking indicator.
        if (es.readyState === EventSource.CLOSED) {
            const stuckRunId = activeStreamRunId;
            if (!stuckRunId) return;
            getRun(stuckRunId).then(data => {
                const st = data && data.status;
                if (st === 'completed' || st === 'failed' || st === 'cancelled') {
                    // Run already finished server-side — clean up the UI.
                    flushDeltaBuffer();
                    sealLastAgent();
                    closeActiveStream();
                    activeRunId.value = null;
                    clearCompletedSubagents();
                    if (onDone) onDone(st === 'completed' ? 'finished' : st === 'failed' ? 'error' : st, data);
                } else {
                    // Run is still active — try to reopen the SSE stream.
                    if (_retryCount >= MAX_SSE_RETRIES) {
                        console.error('[SSE] Max retries reached, giving up');
                        closeActiveStream();
                        activeRunId.value = null;
                        clearCompletedSubagents();
                        chatMessages.value = [...chatMessages.value, {
                            type: 'error', text: 'Lost connection to server. Refresh to retry.',
                        }];
                        return;
                    }
                    const delay = Math.min(2000 * Math.pow(2, _retryCount), 15000);
                    setTimeout(() => {
                        if (activeRunId.value === stuckRunId) {
                            openForegroundStream(stuckRunId, { onDone, _retryCount: _retryCount + 1 });
                        }
                    }, delay);
                }
            }).catch(() => {
                // Server unreachable — retry with backoff.
                if (_retryCount >= MAX_SSE_RETRIES) {
                    console.error('[SSE] Max retries reached (server unreachable)');
                    closeActiveStream();
                    activeRunId.value = null;
                    clearCompletedSubagents();
                    chatMessages.value = [...chatMessages.value, {
                        type: 'error', text: 'Server unreachable. Refresh to retry.',
                    }];
                    return;
                }
                const delay = Math.min(5000 * Math.pow(2, _retryCount), 30000);
                setTimeout(() => {
                    if (activeRunId.value === stuckRunId) {
                        openForegroundStream(stuckRunId, { onDone, _retryCount: _retryCount + 1 });
                    }
                }, delay);
            });
        }
    };

    return { close: () => closeActiveStream() };
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

export function closeActiveStream() {
    if (flushTimer !== null) {
        cancelAnimationFrame(flushTimer);
        flushTimer = null;
    }
    flushDeltaBuffer();
    if (activeEs) {
        activeEs.close();
        activeEs = null;
    }
    activeStreamRunId = null;
}
