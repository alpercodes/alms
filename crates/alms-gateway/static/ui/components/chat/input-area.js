import { html, useRef } from '../../deps.js';
import { activeSessionId } from '../../state/sessions.js';
import { activeAgentId, agents } from '../../state/agents.js';
import { activeRunId, runs } from '../../state/runs.js';
import { nextMsgId } from '../../state/chat.js';
import { appendMessage, transformMessages } from '../../state/chat-actions.js';
import { messageQueue } from '../../state/queue.js';
import { localSettings } from '../../state/settings.js';
import { createRun, cancelRun as apiCancelRun } from '../../api/runs.js';
import { savePendingMessage, setPendingRunId, clearPendingMessage } from '../../state/pending-messages.js';
import { IconSend, IconStop } from '../../utils/icons.js';
import { ComposerAdvanced } from './composer-advanced.js';

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
    const agentId = activeAgentId.value;
    if (!agentId) {
        transformMessages(msgs =>
            [...msgs,
             { id: nextMsgId(), type: 'error', text: 'Select an agent before sending a message.' }]
        );
        return;
    }

    appendMessage(
        { id: nextMsgId(), type: 'user', role: 'user', text },
        { id: nextMsgId(), type: 'thinking', pending: true },
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
            agent_id: agentId,
            input: { type: 'text', text },
        };
        const settings = localSettings.value;
        // Per-run `provider` / `model` are intentionally NOT forwarded
        // from `localSettings` (#865). The settings modal and composer
        // Advanced expander both write these keys to localStorage, and
        // stale values silently squashed per-agent overrides on every
        // run (per-run > per-agent in resolution precedence). Until a
        // deliberate "use this provider/model for this chat" UX exists,
        // provider/model resolution is owned by per-agent + server
        // defaults only. Other per-run overrides remain forwarded —
        // they aren't the bug source and don't share this footgun.
        if (settings.max_tokens != null) runBody.max_tokens = settings.max_tokens;
        if (settings.posture) runBody.posture = settings.posture;
        // debug_mode is tri-state on the composer (Inherit / On / Off):
        // null -> omit (inherit), true -> enable, false -> explicitly
        // disable (overrides an agent-level `true` for this one run).
        if (settings.debug_mode != null) runBody.debug_mode = !!settings.debug_mode;
        // Reasoning / thinking overrides (#804 Slice C). Send `0` on the
        // wire as an explicit-disable signal — only `null`/`undefined`
        // means "inherit per-agent / server default". This preserves the
        // `None` vs `Some(0)` distinction the backend three-layer
        // precedence relies on.
        if (settings.thinking_budget_tokens != null) {
            runBody.thinking_budget_tokens = settings.thinking_budget_tokens;
        }
        if (settings.reasoning_effort) {
            runBody.reasoning_effort = settings.reasoning_effort;
        }
        if (settings.gemini_thinking_budget != null) {
            runBody.gemini_thinking_budget = settings.gemini_thinking_budget;
        }

        const runResp = await createRun(runBody);
        // Attach the run ID to the pending message so reconciliation can
        // match by run ID instead of text content (avoids false-positive
        // deduplication when the user sends identical text twice).
        if (sessionId && runResp?.run_id) {
            setPendingRunId(sessionId, runResp.run_id);
        }
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
    if (!text || !activeSessionId.value || !activeAgentId.value) return;
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
    const hasActiveAgent = !!activeAgentId.value;
    const canSend = hasAgent && hasActiveAgent && hasSession;
    const isRunning = !!activeRunId.value;
    const placeholder = hasActiveAgent ? 'Send a message...' : 'Select an agent to send a message';

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
            <${ComposerAdvanced} />
            <div class="input-container">
                <textarea id="prompt" ref=${promptRef} rows="1"
                          placeholder=${placeholder}
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
