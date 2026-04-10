import { html, useSignal } from '../../deps.js';
import { PREVIEW_LEN } from '../../utils/constants.js';

/**
 * Collapsible debug display for the full LLM context window.
 *
 * Rendered when the backend sends a `context_debug` SSE event (debug mode
 * is enabled via Settings). Shows the system prompt, message history, tool
 * list, and token counts -- exactly what the LLM sees.
 */

/** Truncate text to a maximum length, adding ellipsis if needed. */
function truncate(text, max) {
    if (!text) return '';
    if (text.length <= max) return text;
    return text.slice(0, max) + '...';
}

/** Role label with color styling class. */
function roleClass(role) {
    switch (role) {
        case 'system': return 'cd-role-system';
        case 'user': return 'cd-role-user';
        case 'assistant': return 'cd-role-assistant';
        case 'tool': return 'cd-role-tool';
        default: return '';
    }
}

/** Format a number with commas. */
function fmt(n) {
    if (n == null) return '--';
    return Number(n).toLocaleString();
}

/** Single message in the context debug view. */
function ContextMessage({ msg, index }) {
    const expanded = useSignal(false);
    const role = msg.role || 'unknown';
    const content = msg.content || '';
    const hasToolCalls = msg.tool_calls && msg.tool_calls.length > 0;
    const hasToolCallId = !!msg.tool_call_id;
    const preview = truncate(content, PREVIEW_LEN);

    let label = `[${index}] ${role}`;
    if (hasToolCallId) {
        label += ` (tool_result)`;
    }
    if (hasToolCalls) {
        const names = msg.tool_calls.map(tc => tc.function?.name || '?').join(', ');
        label += ` -> ${names}`;
    }

    return html`
        <div class="cd-msg" role="button" tabindex="0"
             onClick=${(e) => { e.stopPropagation(); expanded.value = !expanded.value; }}
             onKeyDown=${(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); e.stopPropagation(); expanded.value = !expanded.value; } }}>
            <div class="cd-msg-header">
                <span class="cd-msg-chevron">${expanded.value ? '\u25BC' : '\u25B6'}</span>
                <span class="cd-msg-role ${roleClass(role)}">${role}</span>
                ${!expanded.value && preview && html`<span class="cd-msg-preview">${preview}</span>`}
            </div>
            ${expanded.value && html`
                <div class="cd-msg-body" onClick=${(e) => e.stopPropagation()}>
                    ${content && html`<pre class="cd-msg-content">${content}</pre>`}
                    ${hasToolCalls && html`
                        <div class="cd-msg-tools">
                            <div class="cd-section-label">Tool calls:</div>
                            ${msg.tool_calls.map((tc) => html`
                                <pre class="cd-msg-content">${tc.function?.name || '?'}(${tc.function?.arguments || ''})</pre>
                            `)}
                        </div>
                    `}
                </div>
            `}
        </div>
    `;
}


export function ContextDebugRow({ messages, toolNames, totalTokens, systemTokens, historyMessageCount }) {
    const expanded = useSignal(false);
    const toggle = (e) => {
        e.stopPropagation();
        expanded.value = !expanded.value;
    };

    const msgCount = Array.isArray(messages) ? messages.length : 0;

    return html`
        <div class="cd-row" role="button" tabindex="0"
             onClick=${toggle} onKeyDown=${(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggle(e); } }}>
            <div class="cd-header">
                <span class="cd-chevron">${expanded.value ? '\u25BC' : '\u25B6'}</span>
                <span class="cd-icon">CTX</span>
                <span class="cd-title">Context sent to LLM</span>
                <span class="cd-stats">
                    ${fmt(totalTokens)} tokens | ${msgCount} messages | ${(toolNames || []).length} tools
                </span>
            </div>
            ${expanded.value && html`
                <div class="cd-detail" onClick=${(e) => e.stopPropagation()}>
                    <!-- Token breakdown -->
                    <div class="cd-section">
                        <div class="cd-section-label">Token breakdown</div>
                        <div class="cd-token-grid">
                            <span class="cd-token-label">System prompt:</span>
                            <span class="cd-token-value">${fmt(systemTokens)}</span>
                            <span class="cd-token-label">History messages:</span>
                            <span class="cd-token-value">${historyMessageCount}</span>
                            <span class="cd-token-label">Total estimated:</span>
                            <span class="cd-token-value cd-token-total">${fmt(totalTokens)}</span>
                        </div>
                    </div>

                    <!-- Tools available -->
                    <div class="cd-section">
                        <div class="cd-section-label">Tools available (${(toolNames || []).length})</div>
                        <div class="cd-tool-list">
                            ${(toolNames || []).map(name => html`<span class="cd-tool-tag">${name}</span>`)}
                        </div>
                    </div>

                    <!-- Messages -->
                    <div class="cd-section">
                        <div class="cd-section-label">Messages (${msgCount})</div>
                        <div class="cd-messages">
                            ${(messages || []).map((msg, i) => html`
                                <${ContextMessage} key=${i} msg=${msg} index=${i} />
                            `)}
                        </div>
                    </div>
                </div>
            `}
        </div>
    `;
}
