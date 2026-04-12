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
import { activeAgent } from '../../state/agents.js';
import { scrollToBottom } from '../../utils/format.js';

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
                if (m.type === 'warning') {
                    return html`<${DmDivider} key=${m.id} text=${m.text || 'Warning'} />`;
                }
                if (m.type === 'run_boundary') {
                    const label = m.status === 'failed' ? 'run failed'
                        : m.status === 'cancelled' ? 'run cancelled'
                        : 'run completed';
                    return html`<${DmDivider} key=${m.id} text=${label} />`;
                }
                if (m.type === 'subagent_completed') {
                    const text = `Subagent '${m.name || 'subagent'}' ${m.status === 'fail' ? 'failed' : 'completed'}`;
                    return html`<${DmDivider} key=${m.id} text=${text} />`;
                }
                if (m.type === 'job_completed') {
                    return html`<${DmDivider} key=${m.id} text=${`Job '${m.jobName || 'job'}' ${m.status || 'completed'}`} />`;
                }
                if (m.type === 'tool') {
                    // Tool calls in DMs -- show as a compact note with name
                    // and status. Params and result are available via expand
                    // in the non-DM view, but DM view keeps it minimal.
                    return html`
                        <div key=${m.id} class="dm-msg dm-msg-center">
                            <div class="dm-msg-tool">${m.tool}(${m.status})</div>
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
            <span class="dm-view-footer-text">This is a read-only view of an agent-to-agent conversation.</span>
        </div>
    `;
}
