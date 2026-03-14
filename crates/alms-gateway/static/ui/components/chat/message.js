import { html } from '../../deps.js';

export function Message({ type, role, text }) {
    const cls = type === 'user' ? 'user' : 'agent';
    const label = type === 'user' ? 'You' : 'Agent';
    return html`
        <div class="msg ${cls}">
            <div class="msg-label">${label}</div>
            <div class="msg-body">${text}</div>
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
            <div class="msg-body" style="border-color: #f85149; color: #f85149;">
                ${text}
            </div>
        </div>
    `;
}

export function SystemMessage({ text }) {
    return html`
        <div style="font-size: 11px; color: #484f58; text-align: center;">
            ${text}
        </div>
    `;
}
