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

export function ErrorMessage({ text, code }) {
    return html`
        <div class="msg msg-error">
            <div class="msg-error-icon">\u26A0</div>
            <div class="msg-error-body">
                <div class="msg-error-title">Error</div>
                <div class="msg-error-text">${text}</div>
            </div>
        </div>
    `;
}

export function WarningMessage({ text, code }) {
    return html`
        <div class="msg msg-warning">
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
