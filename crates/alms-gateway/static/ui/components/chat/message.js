import { html, renderMarkdown } from '../../deps.js';
import { activeAgent } from '../../state/agents.js';

export function Message({ type, role, text, sealed }) {
    const cls = type === 'user' ? 'user' : 'agent';
    const agentName = activeAgent.value?.name;
    const label = type === 'user' ? '>' : (agentName ? `${agentName} $` : '$');
    const streaming = type === 'agent' && sealed === false;

    // Render Markdown for sealed (finished) agent messages only.
    // While streaming, use plain text with pre-wrap to avoid running
    // marked.parse() + DOMPurify.sanitize() on every animation frame.
    if (type === 'agent' && sealed) {
        const rendered = renderMarkdown(text || '');
        return html`
            <div class="msg ${cls}">
                <div class="msg-label">${label}</div>
                <div class="msg-body markdown-body"
                     dangerouslySetInnerHTML=${{ __html: rendered }} />
            </div>
        `;
    }

    return html`
        <div class="msg ${cls}">
            <div class="msg-label">${label}</div>
            <div class="msg-body ${streaming ? 'streaming-cursor' : ''}">${text}</div>
        </div>
    `;
}

export function TokenBadge({ usage }) {
    if (!usage) return null;
    const p = usage.prompt_tokens || 0;
    const c = usage.completion_tokens || 0;
    if (p + c === 0) return null;
    return html`<div class="msg-tokens">${p}p + ${c}c tokens</div>`;
}

export function ErrorMessage({ text, code }) {
    const codeCls = code ? `msg-error--${code.toLowerCase()}` : '';
    return html`
        <div class="msg msg-error ${codeCls}" data-code=${code || ''}>
            <div class="msg-error-icon">\u274C</div>
            <div class="msg-error-body">
                <div class="msg-error-title">Error</div>
                <div class="msg-error-text">${text}</div>
            </div>
        </div>
    `;
}

export function WarningMessage({ text, code }) {
    const codeCls = code ? `msg-warning--${code.toLowerCase()}` : '';
    return html`
        <div class="msg msg-warning ${codeCls}" data-code=${code || ''}>
            <div class="msg-warning-icon">\u26A0</div>
            <div class="msg-warning-body">
                <div class="msg-warning-title">Warning</div>
                <div class="msg-warning-text">${text}</div>
            </div>
        </div>
    `;
}

export function SystemMessage({ text }) {
    return html`
        <div style="font-size: var(--text-xs); color: var(--text-disabled); text-align: center;">
            ${text}
        </div>
    `;
}

export function DmEndedMessage({ peer, reason }) {
    return html`
        <div class="dm-ended-banner">
            <span class="dm-ended-label">DM conversation with ${peer} ended</span>
            <span class="dm-ended-reason">${reason}</span>
        </div>
    `;
}
