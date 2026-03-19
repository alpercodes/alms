import { h, html, render, signal, useEffect, useRef } from './deps.js';
import { boot } from './hooks/use-boot.js';
import { Header } from './components/header.js';
import { Sidebar } from './components/sidebar/index.js';
import { Message, ErrorMessage, SystemMessage, TokenBadge } from './components/chat/message.js';
import { ToolRow } from './components/chat/tool-row.js';
import { ApprovalCard } from './components/chat/approval-card.js';
import { MessageQueue } from './components/chat/message-queue.js';
import { InputArea } from './components/chat/input-area.js';
import { chatMessages } from './state/chat.js';
import { PanelContainer } from './components/panel/index.js';
import { scrollToBottom } from './utils/format.js';

// ── App status ──
export const status = signal('connecting...');

// ── Chat view ──
function ChatView() {
    const messagesRef = useRef(null);

    // Auto-scroll when messages change
    useEffect(() => {
        scrollToBottom(messagesRef.current);
    }, [chatMessages.value]);

    return html`
        <div id="chat">
            <div id="messages" ref=${messagesRef}>
                ${chatMessages.value.length === 0 && html`
                    <div style="color: var(--text-disabled); font-style: italic; padding: var(--space-8); text-align: center; font-size: var(--text-sm);">
                        No messages yet. Send a message to start.
                    </div>
                `}
                ${chatMessages.value.map((m, i) => {
                    if (m.type === 'user' || m.type === 'agent') {
                        return html`<${Message} key=${i} type=${m.type} text=${m.text} sealed=${m.sealed} />`;
                    }
                    if (m.type === 'tool') {
                        return html`<${ToolRow} key=${i} ...${m} />`;
                    }
                    if (m.type === 'approval') {
                        return html`<${ApprovalCard} key=${i} ...${m} />`;
                    }
                    if (m.type === 'error') {
                        return html`<${ErrorMessage} key=${i} text=${m.text} />`;
                    }
                    if (m.type === 'system') {
                        return html`<${SystemMessage} key=${i} text=${m.text} />`;
                    }
                    if (m.type === 'tokens') {
                        return html`<${TokenBadge} key=${i} usage=${m.usage} />`;
                    }
                    if (m.type === 'thinking') {
                        return html`
                            <div key="thinking" class="msg agent">
                                <div class="msg-label">Agent</div>
                                <div class="msg-body thinking-indicator">Thinking</div>
                            </div>
                        `;
                    }
                    return null;
                })}
            </div>
            <${MessageQueue} />
            <${InputArea} />
        </div>
    `;
}


function App() {
    return html`
        <${Header} status=${status} onOpenSettings=${() => {}} />
        <div id="main">
            <${Sidebar} />
            <${ChatView} />
            <${PanelContainer} />
        </div>
    `;
}

// Mount and boot
render(html`<${App} />`, document.getElementById('app'));

boot().then(() => {
    status.value = 'connected';
}).catch(() => {
    status.value = 'offline';
});
