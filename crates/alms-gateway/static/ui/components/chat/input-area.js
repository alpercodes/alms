import { html, useRef } from '../../deps.js';
import { activeSessionId } from '../../state/sessions.js';
import { activeAgentId, agents } from '../../state/agents.js';
import { activeRunId, runs } from '../../state/runs.js';
import { chatMessages } from '../../state/chat.js';
import { messageQueue } from '../../state/queue.js';
import { localSettings } from '../../state/settings.js';
import { createRun, cancelRun as apiCancelRun, listRuns } from '../../api/runs.js';
import { openForegroundStream, closeActiveStream } from '../../hooks/use-event-source.js';
import { IconSend, IconStop } from '../../utils/icons.js';

async function startRun(text) {
    chatMessages.value = [...chatMessages.value,
        { type: 'user', role: 'user', text },
        { type: 'thinking' },
    ];

    closeActiveStream();

    try {
        const runBody = {
            session_id: activeSessionId.value,
            input: { type: 'text', text },
        };
        const settings = localSettings.value;
        if (settings.model) runBody.model = settings.model;
        if (settings.max_tokens != null) runBody.max_tokens = settings.max_tokens;
        if (settings.posture) runBody.posture = settings.posture;

        const data = await createRun(runBody);
        const runId = data.run_id;

        openForegroundStream(runId, {
            onDone: () => {
                if (activeSessionId.value) {
                    listRuns(activeSessionId.value).then(d => {
                        runs.value = d.runs || [];
                    }).catch(() => {});
                }
                if (messageQueue.value.length > 0) {
                    const next = messageQueue.value[0];
                    messageQueue.value = messageQueue.value.slice(1);
                    startRun(next.text);
                }
            },
        });
    } catch (err) {
        // Remove thinking indicator and show error
        chatMessages.value = chatMessages.value
            .filter(m => m.type !== 'thinking')
            .concat([{ type: 'error', text: `Failed to start run: ${err.message || err.status || 'unknown error'}` }]);
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
