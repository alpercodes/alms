import { h, html, render, signal, useEffect, useRef, useSignal } from './deps.js';
import { boot } from './hooks/use-boot.js';
import { Header } from './components/header.js';
import { Sidebar } from './components/sidebar/index.js';
import { Message, ErrorMessage, WarningMessage, SystemMessage, DmEndedMessage, TokenBadge } from './components/chat/message.js';
import { ToolRow, ToolGroup } from './components/chat/tool-row.js';
import { ApprovalCard } from './components/chat/approval-card.js';
import { MessageQueue } from './components/chat/message-queue.js';
import { InputArea } from './components/chat/input-area.js';
import { chatMessages } from './state/chat.js';
import { PanelContainer } from './components/panel/index.js';
import { SettingsModal } from './components/settings-modal.js';
import { OnboardingView } from './components/onboarding.js';
import { agents, activeAgent } from './state/agents.js';
import { SubagentBar } from './components/chat/subagent-bar.js';
import { scrollToBottom } from './utils/format.js';

// ── App status ──
export const status = signal('connecting...');

/**
 * Group consecutive tool messages for parallel display.
 * Returns the original array with consecutive tool runs replaced by
 * group marker objects ({ _isToolGroup: true, tools: [...], key }).
 * Single-tool runs are left as-is (ToolGroup renders them unwrapped).
 */
function groupMessages(msgs) {
    const result = [];
    let i = 0;
    while (i < msgs.length) {
        if (msgs[i].type === 'tool') {
            const group = [];
            while (i < msgs.length && msgs[i].type === 'tool') {
                group.push(msgs[i]);
                i++;
            }
            if (group.length > 1) {
                result.push({
                    _isToolGroup: true,
                    key: 'tg-' + group[0].id,
                    tools: group,
                });
            } else {
                result.push(group[0]);
            }
        } else {
            result.push(msgs[i]);
            i++;
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

    

    return html`
        <div id="chat">
            <div id="messages" ref=${messagesRef}>
                ${chatMessages.value.length === 0 && html`
                    <div style="color: var(--text-disabled); font-style: italic; padding: var(--space-8); text-align: center; font-size: var(--text-sm);">
                        No messages yet. Send a message to start.
                    </div>
                `}
                ${groupMessages(chatMessages.value).map((item) => {
                    if (item._isToolGroup) {
                        return html`
                            <${ToolGroup} key=${item.key} count=${item.tools.length}>
                                ${item.tools.map(t => html`<${ToolRow} key=${t.id} ...${t} />`)}
                            <//>
                        `;
                    }
                    const m = item;
                    if (m.type === 'user' || m.type === 'agent') {
                        return html`<${Message} key=${m.id} type=${m.type} text=${m.text} sealed=${m.sealed} />`;
                    }
                    if (m.type === 'tool') {
                        return html`<${ToolRow} key=${m.id} ...${m} />`;
                    }
                    if (m.type === 'approval') {
                        return html`<${ApprovalCard} key=${m.id} ...${m} />`;
                    }
                    if (m.type === 'image') {
                        const cls = m.role === 'user' ? 'user' : 'agent';
                        const agentName = activeAgent.value?.name;
                        const label = m.role === 'user' ? '>' : (agentName ? `${agentName} $` : '$');
                        return html`
                            <div key=${m.id} class="msg ${cls}">
                                <div class="msg-label">${label}</div>
                                <div class="msg-body">
                                    ${m.url
                                        ? html`<img src=${m.url} alt=${m.alt || ''} style="max-width:100%;border-radius:8px;" />`
                                        : `[Image${m.alt ? ': ' + m.alt : ''}]`
                                    }
                                    ${m.alt && html`<div style="font-size:var(--text-xs);color:var(--text-secondary);margin-top:var(--space-2);">${m.alt}</div>`}
                                </div>
                            </div>
                        `;
                    }
                    if (m.type === 'error') {
                        return html`<${ErrorMessage} key=${m.id} text=${m.text} code=${m.code} />`;
                    }
                    if (m.type === 'warning') {
                        return html`<${WarningMessage} key=${m.id} text=${m.text} code=${m.code} />`;
                    }
                    if (m.type === 'system') {
                        return html`<${SystemMessage} key=${m.id} text=${m.text} />`;
                    }
                    if (m.type === 'dm_ended') {
                        return html`<${DmEndedMessage} key=${m.id} peer=${m.peer} reason=${m.reason} />`;
                    }
                    if (m.type === 'notification') {
                        // Synthetic system markers restored from session history.
                        // Route to the correct visual component based on metadata.type.
                        const md = m.metadata || {};
                        if (md.type === 'dm_ended_notification') {
                            const reasonLabels = { 'ignored': 'no further replies', 'depth_exceeded': 'message limit reached' };
                            return html`<${DmEndedMessage} key=${m.id} peer=${md.peer || 'unknown'} reason=${reasonLabels[md.reason] || md.reason || 'conversation ended'} />`;
                        }
                        // job_notification and other synthetic markers: render as system message
                        return html`<${SystemMessage} key=${m.id} text=${m.text} />`;
                    }
                    if (m.type === 'tokens') {
                        return html`<${TokenBadge} key=${m.id} usage=${m.usage} />`;
                    }
                    if (m.type === 'thinking') {
                        let label = 'Thinking';
                        if (m.queuedBehind > 0) {
                            const n = m.queuedBehind;
                            label = 'Agent is busy -- your message is queued (' + n + ' ahead)';
                        } else if (m.source && m.source.startsWith('peer:')) {
                            label = 'Replying to message from ' + m.source.slice(5);
                        } else if (m.source === 'job') {
                            label = 'Running scheduled job';
                        } else if (m.source === 'subagent') {
                            label = 'Processing subagent result';
                        // Phase values match constants in alms-runtime/src/events.rs
                        // (PHASE_BUILDING_CONTEXT, PHASE_SUMMARIZING, PHASE_CALLING_LLM, PHASE_EXECUTING_TOOLS)
                        } else if (m.phase === 'building_context') {
                            label = 'Building context';
                        } else if (m.phase === 'summarizing') {
                            label = 'Summarizing history';
                        } else if (m.phase === 'calling_llm') {
                            label = 'Thinking';
                        } else if (m.phase === 'executing_tools') {
                            label = m.phaseDetail
                                ? 'Running ' + m.phaseDetail
                                : 'Running tools';
                        }
                        const thinkingName = activeAgent.value?.name || 'Agent';
                        return html`
                            <div key=${m.id} class="msg agent">
                                <div class="msg-label">${thinkingName} $</div>
                                <div class="msg-body thinking-indicator">${label}</div>
                            </div>
                        `;
                    }
                    return null;
                })}
            </div>
            <${MessageQueue} />
            <${SubagentBar} />
            <${InputArea} />
        </div>
    `;
}


function App() {
    const settingsOpen = useSignal(false);
    const hasAgents = agents.value.length > 0;
    return html`
        <${Header} status=${status} onOpenSettings=${() => { settingsOpen.value = true; }} />
        ${hasAgents
            ? html`
                <div id="main">
                    <${Sidebar} />
                    <${ChatView} />
                    <${PanelContainer} />
                </div>`
            : html`<${OnboardingView} />`
        }
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
