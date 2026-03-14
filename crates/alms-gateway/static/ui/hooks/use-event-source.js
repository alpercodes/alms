import { chatMessages } from '../state/chat.js';
import { activeRunId } from '../state/runs.js';

// Active foreground EventSource reference
let activeEs = null;

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
export function openForegroundStream(runId, { onDone } = {}) {
    closeActiveStream();

    // EventSource cannot send Authorization headers — use ?token= query param
    const token = localStorage.getItem('alms_auth_token');
    const url = token
        ? `/runs/${runId}/events?token=${encodeURIComponent(token)}`
        : `/runs/${runId}/events`;
    const es = new EventSource(url);
    activeEs = es;
    activeRunId.value = runId;

    es.addEventListener('token_delta', (e) => {
        const { delta } = JSON.parse(e.data);
        deltaBuffer += delta;
        scheduleFlush();
    });

    es.addEventListener('tool_start', (e) => {
        flushDeltaBuffer(); // flush pending text before tool row
        const data = JSON.parse(e.data);
        chatMessages.value = [...chatMessages.value, {
            type: 'tool',
            tool: data.tool,
            params: data.params,
            status: 'running',
            id: data.tool_invocation_id || data.call_id || data.tool,
        }];
        // Seal the previous agent message so new deltas start a new bubble
        sealLastAgent();
    });

    es.addEventListener('tool_end', (e) => {
        const data = JSON.parse(e.data);
        const msgs = [...chatMessages.value];
        const matchId = data.tool_invocation_id;
        const idx = matchId
            ? msgs.findLastIndex(m => m.type === 'tool' && m.id === matchId)
            : msgs.findLastIndex(m => m.type === 'tool' && m.status === 'running');
        if (idx >= 0) {
            msgs[idx] = { ...msgs[idx], status: data.ok ? 'done' : 'fail', result: data.result };
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
        if (onDone) onDone(status, data);
    };

    es.addEventListener('run_finished', finishHandler('finished'));
    es.addEventListener('run_error', finishHandler('error'));
    es.addEventListener('run_cancelled', finishHandler('cancelled'));

    es.onerror = () => {
        // EventSource auto-reconnects, but if the stream is truly dead
        // (server restart, etc.), the error fires repeatedly.
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
}
