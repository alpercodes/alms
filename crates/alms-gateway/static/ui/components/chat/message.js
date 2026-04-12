import { html, useSignal, renderMarkdown } from '../../deps.js';
import { activeAgent } from '../../state/agents.js';
import { filterMessages } from '../../state/chat-actions.js';

export function Message({ type, role, text, sealed, fromAgent }) {
    const cls = type === 'user' ? 'user' : 'agent';
    const agentName = activeAgent.value?.name;
    // DM messages carry a fromAgent name — use it as the label so the
    // user can see which agent sent the message.  Falls back to the
    // active agent name for normal assistant messages. (#546)
    const label = type === 'user' ? '>'
        : fromAgent ? `${fromAgent} $`
        : (agentName ? `${agentName} $` : '$');
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

export function WarningMessage({ id, text, code }) {
    const collapsed = useSignal(false);
    const dismissed = useSignal(false);

    if (dismissed.value) return null;

    const onToggle = () => { collapsed.value = !collapsed.value; };
    const onDismiss = (e) => {
        e.stopPropagation();
        dismissed.value = true;
        if (id) filterMessages(m => m.id !== id);
    };

    const collapsedCls = collapsed.value ? 'msg-warning--collapsed' : '';
    return html`
        <div class="msg msg-warning ${collapsedCls}" data-code=${code || ''}>
            <div class="msg-warning-icon">\u26A0\uFE0F</div>
            <div class="msg-warning-body">
                <div class="msg-warning-header" onClick=${onToggle}>
                    <div class="msg-warning-title">Warning</div>
                    ${code && html`<span class="msg-warning-code">${code}</span>`}
                    <button class="msg-warning-toggle"
                            title=${collapsed.value ? 'Expand' : 'Collapse'}
                            aria-label=${collapsed.value ? 'Expand warning' : 'Collapse warning'}
                            aria-expanded=${!collapsed.value}>
                        ${collapsed.value ? '\u25B6' : '\u25BC'}
                    </button>
                    <button class="msg-warning-dismiss" onClick=${onDismiss}
                            title="Dismiss" aria-label="Dismiss warning">
                        \u2715
                    </button>
                </div>
                ${!collapsed.value && html`
                    <div class="msg-warning-text">${text}</div>
                `}
            </div>
        </div>
    `;
}

export function SystemMessage({ text }) {
    return html`
        <div class="msg-system">
            ${text}
        </div>
    `;
}

/**
 * Run boundary divider -- rendered between runs to show where one run
 * ended and the next began.  Visually similar to dm-ended banners:
 * a subtle centered label with horizontal rules on each side.
 *
 * Props:
 *   status - 'completed' | 'cancelled' | 'failed'
 *   error  - optional error message (for failed runs)
 */
export function RunBoundary({ status, error }) {
    const statusCls = status === 'failed' ? 'run-boundary--failed'
        : status === 'cancelled' ? 'run-boundary--cancelled'
        : '';
    const label = status === 'failed' ? 'run failed'
        : status === 'cancelled' ? 'run cancelled'
        : 'run completed';
    return html`
        <div class="run-boundary ${statusCls}">
            <span class="run-boundary-label">${label}</span>
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
