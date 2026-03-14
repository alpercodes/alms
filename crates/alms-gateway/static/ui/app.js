import { h, html, render, signal } from './deps.js';
import { boot } from './hooks/use-boot.js';
import { Header } from './components/header.js';
import { Sidebar } from './components/sidebar/index.js';
import { chatMessages } from './state/chat.js';
import { activePanel } from './state/panel.js';

// ── App status ──
const status = signal('connecting...');

// ── Chat view (Phase 3 will extract this to its own component) ──
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
                        return html`
                            <div class="msg agent">
                                <div class="msg-body" style="border-color: #f85149; color: #f85149;">
                                    ${m.text}
                                </div>
                            </div>
                        `;
                    }
                    if (m.type === 'system') {
                        return html`
                            <div style="font-size: 11px; color: #484f58; text-align: center;">
                                ${m.text}
                            </div>
                        `;
                    }
                    return null;
                })}
            </div>
            <div id="input-area">
                <textarea id="prompt" rows="2"
                          placeholder="Send a message... (Enter to send, Shift+Enter for newline)"
                          disabled></textarea>
                <button id="send" disabled>Send</button>
            </div>
        </div>
    `;
}

// ── Panel placeholder (Phase 4 will replace with real tabs) ──
function Panel() {
    if (!activePanel.value) return null;
    return html`
        <div id="panel" class="open">
            <div class="panel-body" style="display:flex">
                <div style="color: #484f58; font-style: italic;">
                    Panel: ${activePanel.value} (coming in Phase 4)
                </div>
            </div>
        </div>
    `;
}

function App() {
    return html`
        <${Header} status=${status} onOpenSettings=${() => {/* Phase 5 */}} />
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
