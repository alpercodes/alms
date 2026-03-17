import { html } from '../../deps.js';

export function Message({ type, role, text, sealed }) {
    const cls = type === 'user' ? 'user' : 'agent';
    const label = type === 'user' ? '>' : '$';
    const streaming = type === 'agent' && sealed === false;
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

export function ErrorMessage({ text }) {
    return html`
        <div class="msg agent">
            <div class="msg-label" style="color: var(--error);">!</div>
            <div class="msg-body" style="border-left-color: var(--error); color: var(--error);">
                ${text}
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
