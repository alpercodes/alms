import { html } from '../../deps.js';
import { messageQueue } from '../../state/queue.js';

function removeQueued(index) {
    messageQueue.value = messageQueue.value.filter((_, i) => i !== index);
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
