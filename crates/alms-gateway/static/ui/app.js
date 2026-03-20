import { h, html, render, signal, useEffect, useRef, useSignal } from './deps.js';
import { boot } from './hooks/use-boot.js';
import { Header } from './components/header.js';
import { Sidebar } from './components/sidebar/index.js';
import { Message, ErrorMessage, SystemMessage, TokenBadge } from './components/chat/message.js';
import { ToolRow, SubagentGroup } from './components/chat/tool-row.js';
import { ApprovalCard } from './components/chat/approval-card.js';
import { MessageQueue } from './components/chat/message-queue.js';
import { InputArea } from './components/chat/input-area.js';
import { chatMessages } from './state/chat.js';
import { PanelContainer } from './components/panel/index.js';
import { SettingsModal } from './components/settings-modal.js';
import { scrollToBottom } from './utils/format.js';

// ── App status ──
export const status = signal('connecting...');

/**
 * Group invoke_agent tool rows with their subagent-nested tool rows.
 * Matches by sourceAgent identity (name from invoke_agent params) rather
 * than array adjacency, so interleaved events from concurrent tools are
 * correctly grouped.
 */
function groupMessages(msgs) {
    // First pass: find all invoke_agent headers and their subagent names
    const invokeIndices = new Map(); // index -> subagent name
    for (let i = 0; i < msgs.length; i++) {
        const m = msgs[i];
        if (m.type === 'tool' && m.tool === 'invoke_agent') {
            const name = m.params?.name || m.params?.subagent_name || null;
            if (name) invokeIndices.set(i, name);
        }
    }

    // Second pass: assign each sourceAgent tool to its invoke_agent header
    const childrenOf = new Map(); // invoke index -> [tool messages]
    const consumed = new Set();   // indices consumed as children

    for (let i = 0; i < msgs.length; i++) {
        const m = msgs[i];
        if (m.type === 'tool' && m.sourceAgent && !invokeIndices.has(i)) {
            // Find the LAST matching invoke_agent header by name
            // (handles duplicate names from re-invocations)
            let matchIdx = -1;
            for (const [idx, name] of invokeIndices) {
                if (m.sourceAgent === name) matchIdx = idx;
            }
            if (matchIdx >= 0) {
                if (!childrenOf.has(matchIdx)) childrenOf.set(matchIdx, []);
                childrenOf.get(matchIdx).push(m);
                consumed.add(i);
            }
        }
    }

    // Third pass: build result
    const result = [];
    for (let i = 0; i < msgs.length; i++) {
        if (consumed.has(i)) continue;
        const m = msgs[i];
        if (invokeIndices.has(i)) {
            result.push({
                type: 'subagent-group',
                header: m,
                children: childrenOf.get(i) || [],
            });
        } else {
            result.push(m);
        }
    }
    return result;
}

// ── Chat view ──
function ChatView() {
    const messagesRef = useRef(null);

    // Auto-scroll when messages change
    useEffect(() => {
        scrollToBottom(messagesRef.current);
    }, [chatMessages.value]);

    const grouped = groupMessages(chatMessages.value);

    return html`
        <div id="chat">
            <div id="messages" ref=${messagesRef}>
                ${grouped.length === 0 && html`
                    <div style="color: var(--text-disabled); font-style: italic; padding: var(--space-8); text-align: center; font-size: var(--text-sm);">
                        No messages yet. Send a message to start.
                    </div>
                `}
                ${grouped.map((m, i) => {
                    if (m.type === 'subagent-group') {
                        const sgKey = 'sg-' + (m.header.id || i);
                        return html`<${SubagentGroup} key=${sgKey} header=${m.header} children=${m.children} />`;
                    }
                    const key = m.id || i;
                    if (m.type === 'user' || m.type === 'agent') {
                        return html`<${Message} key=${key} type=${m.type} text=${m.text} sealed=${m.sealed} />`;
                    }
                    if (m.type === 'tool') {
                        return html`<${ToolRow} key=${key} ...${m} />`;
                    }
                    if (m.type === 'approval') {
                        return html`<${ApprovalCard} key=${key} ...${m} />`;
                    }
                    if (m.type === 'error') {
                        return html`<${ErrorMessage} key=${key} text=${m.text} />`;
                    }
                    if (m.type === 'system') {
                        return html`<${SystemMessage} key=${key} text=${m.text} />`;
                    }
                    if (m.type === 'tokens') {
                        return html`<${TokenBadge} key=${key} usage=${m.usage} />`;
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
    const settingsOpen = useSignal(false);
    return html`
        <${Header} status=${status} onOpenSettings=${() => { settingsOpen.value = true; }} />
        <div id="main">
            <${Sidebar} />
            <${ChatView} />
            <${PanelContainer} />
        </div>
        <${SettingsModal} open=${settingsOpen.value} onClose=${() => { settingsOpen.value = false; }} />
    `;
}

// Mount and boot
render(html`<${App} />`, document.getElementById('app'));

boot().then(() => {
    status.value = 'connected';
}).catch(() => {
    status.value = 'offline';
});
