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
import { activePanel } from './state/panel.js';

// ── App status ──
export const status = signal('connecting...');

// ── Chat view ──
function ChatView() {
    const messagesRef = useRef(null);

    // Auto-scroll when messages change
    useEffect(() => {
        const el = messagesRef.current;
        if (el) el.scrollTop = el.scrollHeight;
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
                    return null;
                })}
            </div>
            <${MessageQueue} />
            <${InputArea} />
        </div>
    `;
}

// ── Panel placeholder (Phase 4 will replace with Drawer) ──
function Panel() {
    if (!activePanel.value) return null;
    return html`
        <div id="panel" class="open">
            <div class="panel-body" style="display:flex">
                <div style="color: var(--text-disabled); font-style: italic; font-size: var(--text-sm);">
                    ${activePanel.value.charAt(0).toUpperCase() + activePanel.value.slice(1)} panel — coming in Phase 4
                </div>
            </div>
        </div>
    `;
}

function App() {
    return html`
        <${Header} status=${status} onOpenSettings=${() => {}} />
        <div id="main">
            <${Sidebar} />
            <${ChatView} />
            <${Panel} />
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
