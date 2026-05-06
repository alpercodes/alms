import { html } from '../../deps.js';
import { messageQueue } from '../../state/queue.js';
import { activeSessionId } from '../../state/sessions.js';
import { saveQueue } from '../../state/composer-storage.js';

function removeQueued(index) {
    const next = messageQueue.value.filter((_, i) => i !== index);
    messageQueue.value = next;
    // Mirror the in-memory mutation into per-session storage so the
    // operator's manual removal survives a session-switch round-trip.
    // (#975)
    saveQueue(activeSessionId.value, next);
}

export function MessageQueue() {
    const queue = messageQueue.value;
    if (queue.length === 0) return null;

    return html`
        <div id="message-queue">
            ${queue.map((item, i) => html`
                <div class="queued-msg">
                    <span class="queued-msg-label">queued</span>
                    <span class="queued-msg-text">${item.text}</span>
                    <button class="queued-msg-remove" title="Remove from queue"
                            onClick=${() => removeQueued(i)}>\u00d7</button>
                </div>
            `)}
        </div>
    `;
}
