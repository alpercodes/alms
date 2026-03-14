import { h, render } from 'https://esm.sh/preact@10.24.3';
import { signal } from 'https://esm.sh/@preact/signals@1.3.0';
import htm from 'https://esm.sh/htm@3.1.1';
import { boot, switchAgent } from './hooks/use-boot.js';
import { agents, activeAgentId, activeAgent } from './state/agents.js';
import { sessions, activeSessionId } from './state/sessions.js';
import { serverDefaults } from './state/settings.js';
import { chatMessages } from './state/chat.js';

const html = htm.bind(h);

// Re-export for components to import from app.js
export { h, html, render, signal };

// ── App status ──
const status = signal('connecting...');

function Header() {
    const onAgentChange = (e) => switchAgent(e.target.value);

    return html`
        <header>
            <h1>ALMS</h1>
            <select id="agent-select" onChange=${onAgentChange} value=${activeAgentId.value || ''}>
                ${agents.value.map(a => html`
                    <option value=${a.id}>${a.name}${a.is_default ? ' *' : ''}</option>
                `)}
            </select>
            <span id="status" class=${status.value === 'connected' ? 'ok' : ''}>
                ${status.value}
            </span>
        </header>
    `;
}

function Sidebar() {
    return html`
        <div id="sidebar">
            <div class="sidebar-section">
                <div class="sidebar-label">Sessions</div>
                <div id="session-list">
                    ${sessions.value.map(s => html`
                        <div class="session-item ${s.id === activeSessionId.value ? 'active' : ''}"
                             onClick=${() => { activeSessionId.value = s.id; }}>
                            ${s.context_id || s.id.slice(0, 8)}
                        </div>
                    `)}
                </div>
            </div>
        </div>
    `;
}

function ChatView() {
    return html`
        <div id="chat">
            <div id="messages">
                ${chatMessages.value.length === 0 && html`
                    <div style="color: #484f58; font-style: italic; padding: 20px;">
                        No messages yet. Type below to start.
                    </div>
                `}
                ${chatMessages.value.map(m => {
                    if (m.type === 'user' || m.type === 'agent') {
                        return html`
                            <div class="msg ${m.type === 'user' ? 'user' : 'agent'}">
                                <div class="msg-label">${m.type === 'user' ? 'You' : 'Agent'}</div>
                                <div class="msg-body">${m.text}</div>
                            </div>
                        `;
                    }
                    if (m.type === 'tool') {
                        return html`
                            <div class="tool-row ${m.status}">
                                ${m.status === 'running' ? '\u25b6' : m.status === 'done' ? '\u2713' : '\u2717'} ${m.tool}
                            </div>
                        `;
                    }
                    if (m.type === 'error') {
                        return html`<div class="msg agent"><div class="msg-body" style="border-color: #f85149; color: #f85149;">${m.text}</div></div>`;
                    }
                    if (m.type === 'system') {
                        return html`<div style="font-size: 11px; color: #484f58; text-align: center;">${m.text}</div>`;
                    }
                    return null;
                })}
            </div>
            <div id="input-area">
                <textarea id="prompt" rows="1" placeholder="Message..." disabled></textarea>
                <button id="send" disabled>Send</button>
            </div>
        </div>
    `;
}

function App() {
    return html`
        <${Header} />
        <div id="main">
            <${Sidebar} />
            <${ChatView} />
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
