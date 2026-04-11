/**
 * DM Conversation View -- dedicated UI for agent-to-agent DM exchanges.
 *
 * Renders messages as a two-party thread with each agent's messages
 * visually distinguished by side positioning and color accents.
 * The input area is hidden since users cannot inject messages into DMs.
 *
 * Relates to #604.
 */

import { html, useEffect, useRef, effect, renderMarkdown } from '../../deps.js';
import { chatMessages } from '../../state/chat.js';
import { dmParticipants } from '../../state/sessions.js';
import { scrollToBottom } from '../../utils/format.js';

/**
 * Determine which "side" a message belongs to in the DM view.
 * The first participant in the alphabetically-sorted participants array
 * is rendered on the left, the second on the right.
 */
function messageSide(msg, participants) {
    // DM messages from peer agents have fromAgent set
    if (msg.fromAgent) {
        return msg.fromAgent === participants[0] ? 'left' : 'right';
    }
    // Agent (assistant) messages without fromAgent: these are from the
    // perspective agent. In DM sessions the perspective agent is the one
    // whose session we loaded. Map based on role.
    if (msg.type === 'agent' || msg.role === 'assistant') {
        // Without fromAgent, we cannot definitively know which agent.
        // Default to left (first participant).
        return 'left';
    }
    if (msg.type === 'user' || msg.role === 'user') {
        return 'right';
    }
    return 'center';
}

function DmMessage({ msg, participants }) {
    const side = messageSide(msg, participants);
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
    const label = participants.length >= 2
        ? `${participants[0]} <-> ${participants[1]}`
        : 'DM conversation';

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
                        const reasonLabels = { 'ignored': 'no further replies', 'depth_exceeded': 'message limit reached' };
                        const text = `DM with ${md.peer || 'unknown'} ended -- ${reasonLabels[md.reason] || md.reason || 'ended'}`;
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
                    return html`<div key=${m.id} class="dm-msg dm-msg-center"><div class="dm-msg-thinking">Thinking...</div></div>`;
                }
                if (m.type === 'tool') {
                    // Tool calls in DMs are minimal -- show as a compact note
                    return html`
                        <div key=${m.id} class="dm-msg dm-msg-center">
                            <div class="dm-msg-tool">${m.tool}(${m.status})</div>
                        </div>
                    `;
                }
                if (m.type === 'user' || m.type === 'agent') {
                    return html`<${DmMessage} key=${m.id} msg=${m} participants=${participants} />`;
                }
                return null;
            })}
        </div>
        <div class="dm-view-footer">
            <span class="dm-view-footer-text">This is a read-only view of an agent-to-agent conversation.</span>
        </div>
    `;
}
