import { html, render, signal, effect, useEffect, useRef, useSignal } from './deps.js';
import { boot } from './hooks/use-boot.js';
import { Header } from './components/header.js';
import { Sidebar } from './components/sidebar/index.js';
import { Message, ErrorMessage, WarningMessage, SystemMessage, DmEndedMessage, TokenBadge } from './components/chat/message.js';
import { ToolRow, ToolGroup } from './components/chat/tool-row.js';
import { ContextDebugRow } from './components/chat/context-debug-row.js';
import { ApprovalCard } from './components/chat/approval-card.js';
import { JobCompletionCard } from './components/chat/job-completion-card.js';
import { MessageQueue } from './components/chat/message-queue.js';
import { InputArea } from './components/chat/input-area.js';
import { chatMessages } from './state/chat.js';
import { PanelContainer } from './components/panel/index.js';
import { SettingsModal } from './components/settings-modal.js';
import { OnboardingView } from './components/onboarding.js';
import { agents, activeAgent } from './state/agents.js';
import { SubagentBar } from './components/chat/subagent-bar.js';
import { AgentHeaderBar } from './components/chat/agent-header-bar.js';
import { scrollToBottom } from './utils/format.js';
import { sessionSwitchLoading, agentSwitchLoading, bootRetryAvailable, setRunBoot } from './state/loading.js';

// ── Dynamic page title ──
effect(() => {
    const agent = activeAgent.value;
    document.title = agent ? `ALMS - ${agent.name}` : 'ALMS';
});

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

    // Auto-scroll when messages change.
    // Use effect() from @preact/signals instead of useEffect with
    // [chatMessages.value] -- under the optimised re-render path,
    // Preact can skip hook re-evaluation and the useEffect would
    // never fire.  effect() subscribes directly to the signal graph.
    //
    // The scroll is deferred via requestAnimationFrame so it runs
    // after Preact has committed the new DOM elements.  Without this,
    // scrollHeight still reflects the old content and the container
    // does not scroll to the bottom when switching sessions (#562).
    useEffect(() => {
        let rafId = 0;
        const dispose = effect(() => {
            chatMessages.value; // subscribe to the signal
            cancelAnimationFrame(rafId);
            rafId = requestAnimationFrame(() => {
                scrollToBottom(messagesRef.current);
            });
        });
        return () => { cancelAnimationFrame(rafId); dispose(); };
    }, []);

    // Compute grouping inline — useMemo with signal.value as a dependency
    // can miss updates because @preact/signals may re-render the component
    // through an optimised path that skips hook re-evaluation, causing
    // tool messages added to chatMessages to never appear in the render
    // output.  groupMessages is O(n) on a small array, so the cost of
    // recomputing on every render is negligible.
    const grouped = groupMessages(chatMessages.value);

    return html`
        <div id="chat">
            <${AgentHeaderBar} />
            <div id="messages" role="log" aria-live="polite" ref=${messagesRef}>
                ${agentSwitchLoading.value && html`
                    <div class="loading-state">Loading agent...</div>
                `}
                ${!agentSwitchLoading.value && sessionSwitchLoading.value && html`
                    <div class="loading-state">Loading session...</div>
                `}
                ${!sessionSwitchLoading.value && !agentSwitchLoading.value && chatMessages.value.length === 0 && html`
                    <div class="empty-state">
                        No messages yet. Send a message to start.
                    </div>
                `}
                ${grouped.map((item) => {
                    if (item._isToolGroup) {
                        return html`
                            <${ToolGroup} key=${item.key} count=${item.tools.length}>
                                ${item.tools.map(t => html`<${ToolRow} key=${t.id} ...${t} />`)}
                            <//>
                        `;
                    }
                    const m = item;
                    if (m.type === 'user' || m.type === 'agent') {
                        return html`<${Message} key=${m.id} type=${m.type} text=${m.text} sealed=${m.sealed} fromAgent=${m.fromAgent} />`;
                    }
                    if (m.type === 'tool') {
                        return html`<${ToolRow} key=${m.id} ...${m} />`;
                    }
                    if (m.type === 'context_debug') {
                        return html`<${ContextDebugRow} key=${m.id} ...${m} />`;
                    }
                    if (m.type === 'approval') {
                        return html`<${ApprovalCard} key=${m.id} ...${m} />`;
                    }
                    if (m.type === 'job_completed') {
                        return html`<${JobCompletionCard} key=${m.id} jobName=${m.jobName} status=${m.status} summary=${m.summary} ts=${m.ts} />`;
                    }
                    if (m.type === 'image') {
                        // DM images carry fromAgent — treat them as agent
                        // messages so they render on the correct side. (#546)
                        const isDmImage = !!(m.fromAgent);
                        const cls = (m.role === 'user' && !isDmImage) ? 'user' : 'agent';
                        const agentName = activeAgent.value?.name;
                        const label = (m.role === 'user' && !isDmImage) ? '>'
                            : m.fromAgent ? `${m.fromAgent} $`
                            : (agentName ? `${agentName} $` : '$');
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
                        // Other synthetic markers: render as system message
                        return html`<${SystemMessage} key=${m.id} text=${m.text} />`;
                    }
                    if (m.type === 'tokens') {
                        return html`<${TokenBadge} key=${m.id} usage=${m.usage} />`;
                    }
                    if (m.type === 'thinking') {
                        let label = 'Thinking';
                        if (m.queuedBehind > 0) {
                            label = 'Agent is busy -- your message is queued';
                        } else if (m.source && m.source.startsWith('peer:')) {
                            label = 'Replying to message from ' + m.source.slice(5);
                        } else if (m.source === 'job') {
                            label = 'Running scheduled job';
                        } else if (m.source === 'subagent') {
                            label = 'Processing subagent result';
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

function doRunBoot() {
    bootRetryAvailable.value = false;
    status.value = 'connecting...';
    boot().then(() => {
        status.value = 'connected';
    }).catch(() => {
        status.value = 'offline';
        bootRetryAvailable.value = true;
    });
}

// Register so other modules can trigger a retry via the loading module
setRunBoot(doRunBoot);

doRunBoot();
