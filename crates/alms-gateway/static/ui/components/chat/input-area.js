import { html, useRef } from '../../deps.js';
import { activeSessionId } from '../../state/sessions.js';
import { activeAgentId, agents } from '../../state/agents.js';
import { activeRunId, runs } from '../../state/runs.js';
import { nextMsgId } from '../../state/chat.js';
import { appendMessage, transformMessages } from '../../state/chat-actions.js';
import { messageQueue } from '../../state/queue.js';
import { localSettings } from '../../state/settings.js';
import { createRun, cancelRun as apiCancelRun } from '../../api/runs.js';
import { savePendingMessage, clearPendingMessage } from '../../state/pending-messages.js';
import { IconSend, IconStop } from '../../utils/icons.js';

/**
 * Start a new run with the given text.
 *
 * @param {string} text -- the user message to send
 * @param {object} [opts]
 * @param {string} [opts.sessionId] -- override for activeSessionId.value.
 *   Used by handleRunEnd's queued-message path to avoid a microtask gap
 *   where activeSessionId could change between dequeue and execution
 *   (see issue #526).
 */
export async function startRun(text, opts) {
    const sessionId = opts?.sessionId || activeSessionId.value;

    appendMessage(
        { id: nextMsgId(), type: 'user', role: 'user', text },
        { id: nextMsgId(), type: 'thinking' },
    );

    // Track the optimistically-appended user message so it can be
    // re-injected if the user switches sessions before the backend
    // persists it to the session history.  (Fixes message-loss on
    // rapid session switch while agent is thinking.)
    if (sessionId) {
        savePendingMessage(sessionId, text);
    }

    try {
        const runBody = {
            session_id: sessionId,
            input: { type: 'text', text },
        };
        const settings = localSettings.value;
        if (settings.provider) runBody.provider = settings.provider;
        if (settings.model) runBody.model = settings.model;
        if (settings.max_tokens != null) runBody.max_tokens = settings.max_tokens;
        if (settings.posture) runBody.posture = settings.posture;
        if (settings.debug_mode) runBody.debug_mode = true;

        await createRun(runBody);
        // No need to open a per-run SSE stream — the session stream
        // (opened by use-boot.js) receives all events automatically.
        // run_created → token_delta → run_finished all arrive there.
    } catch (err) {
        // Run creation failed -- no run will persist the message.
        if (sessionId) clearPendingMessage(sessionId);
        transformMessages(msgs =>
            [...msgs.filter(m => m.type !== 'thinking'),
             { id: nextMsgId(), type: 'error', text: `Failed to start run: ${err.error?.message || err.message || err.status || 'unknown error'}` }]
        );
        console.error('[startRun] failed:', err);
    }
}

function sendMessage(promptRef) {
    const text = promptRef.current.value.trim();
    if (!text || !activeSessionId.value) return;
    promptRef.current.value = '';
    promptRef.current.style.height = 'auto';

    if (activeRunId.value) {
        messageQueue.value = [...messageQueue.value, { text }];
        promptRef.current.focus();
        return;
    }

    startRun(text);
}

async function cancelCurrentRun() {
    if (!activeRunId.value) return;
    try {
        await apiCancelRun(activeRunId.value);
    } catch { /* SSE event will handle UI */ }
}

export function InputArea() {
    const promptRef = useRef(null);
    const hasAgent = agents.value.length > 0;
    const hasSession = !!activeSessionId.value;
    const canSend = hasAgent && hasSession;
    const isRunning = !!activeRunId.value;

    const onKeyDown = (e) => {
        if (e.key === 'Enter' && !e.shiftKey) {
            e.preventDefault();
            sendMessage(promptRef);
        }
    };

    const onInput = () => {
        const el = promptRef.current;
        if (el) {
            el.style.height = 'auto';
            el.style.height = Math.min(el.scrollHeight, 150) + 'px';
        }
    };

    return html`
        <div id="input-area">
            <div class="input-container">
                <textarea id="prompt" ref=${promptRef} rows="1"
                          placeholder="Send a message..."
                          aria-label="Message input"
                          disabled=${!canSend}
                          onKeyDown=${onKeyDown}
                          onInput=${onInput}></textarea>
                ${isRunning
                    ? html`<button id="cancel-run" title="Stop run" aria-label="Stop run"
                                   onClick=${cancelCurrentRun}><${IconStop} /></button>`
                    : html`<button id="send" disabled=${!canSend}
                                   title="Send (Enter)" aria-label="Send message"
                                   onClick=${() => sendMessage(promptRef)}><${IconSend} /></button>`
                }
            </div>
        </div>
    `;
}
