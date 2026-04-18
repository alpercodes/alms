/**
 * DM Conversation View -- dedicated UI for agent-to-agent DM exchanges.
 *
 * Renders messages as a two-party thread with each agent's messages
 * visually distinguished by side positioning and color accents.
 * The input area is hidden since users cannot inject messages into DMs.
 *
 * Relates to #604.
 */

import { html, useEffect, useRef, useState, signal, effect, renderMarkdown } from '../../deps.js';
import { chatMessages } from '../../state/chat.js';
import { activeSessionId, dmParticipants } from '../../state/sessions.js';
import { activeAgent } from '../../state/agents.js';
import { activeRunId } from '../../state/runs.js';
import { dmPeer } from '../../state/agent-status.js';
import { scrollToBottom } from '../../utils/format.js';
import { ToolRow } from './tool-row.js';
import { dmThinkingBuffers } from '../../hooks/use-session-stream.js';
import { cancelDm } from '../../api/sessions.js';
import { DM_END_REASON_LABELS } from '../../utils/constants.js';

/** Tracks whether a cancel-DM request is currently in flight.
 *  Module-level: shared across session views. If the user switches
 *  sessions mid-request, the new session may briefly show "Stopping...". */
const cancellingDm = signal(false);

/**
 * Determine which "side" a message belongs to in the DM view.
 * The first participant in the alphabetically-sorted participants array
 * is rendered on the left, the second on the right.
 *
 * @param {object} msg - chat message entry
 * @param {string[]} participants - alphabetically sorted participant names
 * @param {string|null} perspectiveAgent - name of the active agent whose
 *   session we are viewing; used to assign the correct side for messages
 *   without an explicit fromAgent.
 */
function messageSide(msg, participants, perspectiveAgent) {
    // DM messages from peer agents have fromAgent set
    if (msg.fromAgent) {
        return msg.fromAgent === participants[0] ? 'left' : 'right';
    }
    // Agent (assistant) messages without fromAgent: these are from the
    // perspective agent (the active agent whose session we loaded).
    // Use the active agent name to determine which side they belong on,
    // since participants are alphabetically sorted and the perspective
    // agent may be either participants[0] or participants[1].
    if (msg.type === 'agent' || msg.role === 'assistant') {
        if (perspectiveAgent) {
            return perspectiveAgent === participants[0] ? 'left' : 'right';
        }
        return 'left';
    }
    if (msg.type === 'user' || msg.role === 'user') {
        if (perspectiveAgent) {
            // User (incoming peer) messages are on the opposite side
            return perspectiveAgent === participants[0] ? 'right' : 'left';
        }
        return 'right';
    }
    return 'center';
}

function DmMessage({ msg, participants, perspectiveAgent }) {
    const side = messageSide(msg, participants, perspectiveAgent);
    const agentName = msg.fromAgent || (side === 'left' ? participants[0] : participants[1]) || '?';

    const rendered = renderMarkdown(msg.text || '');

    return html`
        <div class="dm-msg dm-msg-${side}">
            <div class="dm-msg-name">${agentName}</div>
            <div class="dm-msg-bubble markdown-body"
                 dangerouslySetInnerHTML=${{ __html: rendered }} />
        </div>
    `;
}

function DmDivider({ text }) {
    return html`
        <div class="dm-ended-banner">
            <span class="dm-ended-label">${text}</span>
        </div>
    `;
}

/**
 * Return true when `thinkingText` is exactly equal (after trimming) to the
 * `message` argument of any `send_message` tool call in `tools`. Used to
 * suppress the thinking-text pane when the model emitted the reply text as
 * a standalone text block immediately before calling `send_message` with
 * the same content -- we do not want to render the same string both as the
 * thinking pane and as the delivered DM bubble. (Fixes #738)
 *
 * Conservative: exact equality only, so legitimate reasoning that happens
 * to echo the message as a substring is preserved.
 *
 * @param {string} thinkingText
 * @param {Array<object>} tools
 * @returns {boolean}
 */
function thinkingMatchesSendMessageContent(thinkingText, tools) {
    if (!thinkingText) return false;
    const trimmed = thinkingText.trim();
    if (!trimmed) return false;
    for (const t of tools || []) {
        if (t.tool !== 'send_message') continue;
        const msg = t.params && typeof t.params.message === 'string'
            ? t.params.message.trim() : '';
        if (msg && msg === trimmed) return true;
    }
    return false;
}

/**
 * Collapsible reasoning block for DM conversations.
 * Groups the agent's thinking text and tool calls for a single run.
 * Collapsed by default; expanding reveals thinking text and ToolRow components.
 */
function DmReasoningBlock({ runId, agentName, thinkingText, tools, status, isLive }) {
    const [expanded, setExpanded] = useState(false);

    // For live blocks, read accumulated thinking text from the signal buffer.
    const liveThinking = isLive ? (dmThinkingBuffers.value.get(runId) || '') : '';
    const rawThinking = thinkingText || liveThinking;

    // Some models emit their reply as a plain text block AND then call
    // `send_message` with the exact same string -- duplicating the content
    // once as "thinking" and once as the delivered message. Detect that
    // case and suppress the thinking display so the UI shows the message
    // only once (as the DM bubble). Fixes #738.
    const thinkingIsDuplicate = thinkingMatchesSendMessageContent(rawThinking, tools);
    const displayThinking = thinkingIsDuplicate ? '' : rawThinking;

    // Visible tools: hide successful send_message (the DM bubble is already shown).
    // Show send_message while running (with spinner) or on failure.
    const visibleTools = (tools || []).filter(
        t => !(t.tool === 'send_message' && t.status === 'done')
    );
    const toolCount = visibleTools.length;

    // Empty + sealed blocks are hidden entirely.
    if (!isLive && toolCount === 0 && (!displayThinking || !displayThinking.trim())) {
        return null;
    }

    const blockClass = 'dm-reasoning-block'
        + (status === 'failed' ? ' dm-reasoning-block--failed' : '')
        + (isLive ? ' dm-reasoning-block--live' : '');
    const toggle = expanded ? '\u25BC' : '\u25B6';
    const label = agentName
        ? `${agentName} reasoning -- ${toolCount} tool call${toolCount !== 1 ? 's' : ''}`
        : `Agent reasoning -- ${toolCount} tool call${toolCount !== 1 ? 's' : ''}`;

    return html`
        <div class=${blockClass}>
            <div class="dm-reasoning-header" onClick=${() => setExpanded(!expanded)}>
                <span class="dm-reasoning-toggle">${toggle}</span>
                <span class="dm-reasoning-summary">${label}</span>
                ${isLive && html`<span class="dm-reasoning-spinner" />`}
            </div>
            ${expanded && html`
                <div class="dm-reasoning-body">
                    ${displayThinking && displayThinking.trim() && html`
                        <pre class="dm-reasoning-thinking">${displayThinking}</pre>
                    `}
                    ${visibleTools.map(t => html`
                        <${ToolRow} key=${t.id} ...${t} />
                    `)}
                </div>
            `}
        </div>
    `;
}

/**
 * Cancel the active DM conversation on the current session.
 * Calls POST /sessions/{session_id}/cancel-dm, disables the button
 * while in flight, and lets the dm_conversation_ended SSE event
 * handle clearing DM state.
 */
async function handleCancelDm() {
    const sessionId = activeSessionId.value;
    if (!sessionId || cancellingDm.value) return;
    cancellingDm.value = true;
    try {
        await cancelDm(sessionId);
        // Success -- the dm_conversation_ended SSE event will handle
        // clearing activeRunId/dmPeer/agentPhase and appending the
        // "conversation ended" banner.
    } catch (err) {
        console.error('[cancel-dm] failed:', err);
    } finally {
        cancellingDm.value = false;
    }
}

export function DmConversationView() {
    const messagesRef = useRef(null);
    const participants = dmParticipants.value;

    // Auto-scroll when messages change
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

    const msgs = chatMessages.value;
    const perspectiveAgent = activeAgent.value ? activeAgent.value.name : null;
    const label = participants.length >= 2
        ? `${participants[0]} <-> ${participants[1]}`
        : 'DM conversation';

    // Show the cancel button when the DM has an active run or dmPeer is set
    // (meaning agents are conversing). The button is hidden when idle.
    const hasActiveRun = !!activeRunId.value;
    const hasDmPeer = !!dmPeer.value;
    const showCancel = hasActiveRun || hasDmPeer;
    const isCancelling = cancellingDm.value;

    return html`
        <div class="dm-view-header">
            <span class="dm-view-header-icon" aria-hidden="true">\u2194</span>
            <span class="dm-view-header-label">${label}</span>
            <span class="dm-view-header-badge">read-only</span>
        </div>
        <div class="dm-thread" ref=${messagesRef}>
            ${msgs.length === 0 && html`
                <div class="empty-state">No messages in this conversation yet.</div>
            `}
            ${msgs.map(m => {
                if (m.type === 'dm_ended') {
                    const text = `Conversation ended -- ${m.reason || 'ended'}`;
                    return html`<${DmDivider} key=${m.id} text=${text} />`;
                }
                if (m.type === 'system') {
                    return html`<${DmDivider} key=${m.id} text=${m.text} />`;
                }
                if (m.type === 'notification') {
                    const md = m.metadata || {};
                    if (md.type === 'dm_ended_notification') {
                        const text = `DM with ${md.peer || 'unknown'} ended -- ${DM_END_REASON_LABELS[md.reason] || md.reason || 'ended'}`;
                        return html`<${DmDivider} key=${m.id} text=${text} />`;
                    }
                    return html`<${DmDivider} key=${m.id} text=${m.text} />`;
                }
                if (m.type === 'error') {
                    return html`<div key=${m.id} class="dm-msg dm-msg-center"><div class="dm-msg-error">${m.text}</div></div>`;
                }
                if (m.type === 'tokens') {
                    return null; // suppress token badges in DM view
                }
                if (m.type === 'thinking') {
                    // Queued runs show a distinct message so the user knows
                    // the agent hasn't started processing yet. (#691)
                    let thinkLabel = 'Thinking\u2026';
                    if (m.pending) {
                        thinkLabel = 'Sending\u2026';
                    } else if (m.queuedBehind > 0) {
                        thinkLabel = 'Queued \u2014 waiting for agent\u2026';
                    } else if (m.source) {
                        // Show which agent is thinking when source info is available.
                        // DM thinking messages carry source like "peer:AgentName".
                        const name = m.source.startsWith('peer:') ? m.source.slice(5) : m.source;
                        if (name) thinkLabel = `${name} is thinking\u2026`;
                    }
                    return html`<div key=${m.id} class="dm-msg dm-msg-center"><div class="dm-msg-thinking">${thinkLabel}</div></div>`;
                }
                if (m.type === 'warning') {
                    return html`<${DmDivider} key=${m.id} text=${m.text || 'Warning'} />`;
                }
                if (m.type === 'run_boundary') {
                    // Completed runs are the normal case -- skip them to
                    // reduce noise.  Only show failed/cancelled boundaries.
                    if (!m.status || m.status === 'completed') return null;
                    const label = m.status === 'failed' ? 'run failed'
                        : m.status === 'cancelled' ? 'run cancelled'
                        : `run ${m.status}`;
                    return html`<${DmDivider} key=${m.id} text=${label} />`;
                }
                if (m.type === 'subagent_completed') {
                    const text = `Subagent '${m.name || 'subagent'}' ${m.status === 'fail' ? 'failed' : 'completed'}`;
                    return html`<${DmDivider} key=${m.id} text=${text} />`;
                }
                if (m.type === 'job_completed') {
                    return html`<${DmDivider} key=${m.id} text=${`Job '${m.jobName || 'job'}' ${m.status || 'completed'}`} />`;
                }
                if (m.type === 'dm_reasoning') {
                    return html`<${DmReasoningBlock} key=${m.id} ...${m} />`;
                }
                if (m.type === 'tool') {
                    if (m.tool === 'send_message') {
                        // Successful send_message content already rendered as DmMessage
                        // via dm_message SSE (live) / persisted session messages (reload).
                        // Only show the tool row if it errored so the user sees the failure.
                        if (m.status === 'done' && !m.error) return null;
                    }
                    // Tool calls in DMs: render a styled ToolRow on the
                    // side of the agent that executed them. Tool messages
                    // don't have fromAgent, so attribute them to the
                    // perspective agent (whose run produced the tool call).
                    // This matches the assistant-message side assignment
                    // logic in messageSide() for messages without fromAgent.
                    // (#652)
                    const toolSide = messageSide(
                        { type: 'agent', role: 'assistant' },
                        participants,
                        perspectiveAgent,
                    );
                    const toolAgentName = toolSide === 'left'
                        ? participants[0] : participants[1];
                    return html`
                        <div key=${m.id} class="dm-msg dm-msg-${toolSide} dm-msg-tool-row">
                            <div class="dm-msg-name">${toolAgentName || '?'}</div>
                            <${ToolRow} ...${m} />
                        </div>
                    `;
                }
                if (m.type === 'image') {
                    const side = messageSide(m, participants, perspectiveAgent);
                    const agentName = m.fromAgent || (side === 'left' ? participants[0] : participants[1]) || '?';
                    return html`
                        <div key=${m.id} class="dm-msg dm-msg-${side}">
                            <div class="dm-msg-name">${agentName}</div>
                            <div class="dm-msg-bubble">
                                ${m.url
                                    ? html`<img src=${m.url} alt=${m.alt || ''} class="dm-msg-image" />`
                                    : `[Image${m.alt ? ': ' + m.alt : ''}]`
                                }
                            </div>
                        </div>
                    `;
                }
                if (m.type === 'user' || m.type === 'agent') {
                    return html`<${DmMessage} key=${m.id} msg=${m} participants=${participants} perspectiveAgent=${perspectiveAgent} />`;
                }
                return null;
            })}
        </div>
        <div class="dm-view-footer">
            ${showCancel
                ? html`
                    <button class="dm-cancel-btn"
                            disabled=${isCancelling}
                            title="Stop this DM conversation"
                            aria-label="Stop conversation"
                            onClick=${handleCancelDm}>
                        <span class="dm-cancel-btn-icon" aria-hidden="true">\u25A0</span>
                        ${isCancelling ? 'Stopping\u2026' : 'Stop conversation'}
                    </button>
                `
                : html`
                    <span class="dm-view-footer-text">This is a read-only view of an agent-to-agent conversation.</span>
                `
            }
        </div>
    `;
}
