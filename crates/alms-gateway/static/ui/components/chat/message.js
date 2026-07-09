import { html, useSignal, useRef, useEffect, renderMarkdown } from '../../deps.js';
import { activeAgent } from '../../state/agents.js';
import { activeSessionOwnerName } from '../../state/sessions.js';
import { filterMessages } from '../../state/chat-actions.js';
import { fmtMessageTime } from '../../utils/format.js';
import { decorateCodeBlocks } from '../../utils/decorate-code-blocks.js';

/**
 * Per-message timestamp (#855) — small grey text rendered next to the
 * author label. The full ISO string is exposed as a `title` attribute so
 * hovering reveals precise debug info. Returns null when no timestamp is
 * available, so callers can render unconditionally without leaving an
 * empty span behind.
 */
export function MessageTimestamp({ ts }) {
    if (!ts) return null;
    const formatted = fmtMessageTime(ts);
    if (!formatted) return null;
    return html`<span class="msg-timestamp" title=${ts}>${formatted}</span>`;
}

/**
 * Provider-neutral reasoning / extended-thinking panel.
 *
 * Populated by the `reasoning_delta` SSE stream from the Anthropic
 * extended-thinking adapter (issue #767). Designed so #768
 * (OpenAI/DeepSeek/xAI) and #769 (Gemini) can reuse the same event type
 * and component without changes — the only thing that varies between
 * providers is where the text comes from.
 *
 * Defaults to **collapsed** per issue #767's UX requirement — extended
 * thinking is usually noise the user doesn't want to see until they ask.
 */
export function ReasoningPanel({ text, live }) {
    // Hooks must run on every render — never return above them.
    const expanded = useSignal(false);
    if (!text) return null;
    const onToggle = () => { expanded.value = !expanded.value; };
    const tokenHint = text.length > 0 ? ` (${text.length} chars)` : '';
    const title = live ? 'Thinking\u2026' : 'Reasoning';
    const arrow = expanded.value ? '\u25BC' : '\u25B6';
    return html`
        <div class="reasoning-panel ${live ? 'reasoning-panel--live' : ''} ${expanded.value ? 'reasoning-panel--open' : ''}">
            <button class="reasoning-panel-toggle" onClick=${onToggle}
                    aria-expanded=${expanded.value}>
                <span class="reasoning-panel-arrow">${arrow}</span>
                <span class="reasoning-panel-glyph">\u{1F4AD}</span>
                <span class="reasoning-panel-title">${title}</span>
                <span class="reasoning-panel-hint">${tokenHint}</span>
            </button>
            ${expanded.value && html`
                <div class="reasoning-panel-body">
                    <pre class="reasoning-panel-text">${text}</pre>
                </div>
            `}
        </div>
    `;
}

/**
 * Sealed-agent-message Markdown body — wraps the
 * `dangerouslySetInnerHTML` block so we can attach a ref and run the
 * code-block copy-button decorator after the inner HTML lands. (#986)
 *
 * The decorator is idempotent and side-effecting; running it inside a
 * useEffect ties the work to mount + html-prop changes, so a future
 * code path that hot-swaps the body text (no current callsite) still
 * gets the new blocks decorated.
 */
function MarkdownBody({ html: htmlStr }) {
    const ref = useRef(null);
    useEffect(() => {
        decorateCodeBlocks(ref.current);
    }, [htmlStr]);
    return html`
        <div class="msg-body markdown-body" ref=${ref}
             dangerouslySetInnerHTML=${{ __html: htmlStr }} />
    `;
}

export function Message({ type, role, text, sealed, fromAgent, reasoning, ts }) {
    const cls = type === 'user' ? 'user' : 'agent';
    // Prefer the agent that OWNS the active session over the sidebar's
    // active agent (#1212): job/subagent sessions are cross-agent
    // read-only surfaces that don't switch the active agent when opened,
    // so `activeAgent` can point at a completely different agent while
    // the operator reads agent A's job session. For ordinary chat
    // sessions the owner IS the active agent, so nothing changes; when
    // the owner can't be resolved (DM sessions, boot races) we fall back
    // to the previous behaviour.
    const agentName = activeSessionOwnerName.value || activeAgent.value?.name;
    // DM messages carry a fromAgent name — use it as the label so the
    // user can see which agent sent the message.  Falls back to the
    // session-owner / active agent name for normal assistant messages.
    // (#546, #1212)
    const label = type === 'user' ? '>'
        : fromAgent ? `${fromAgent} $`
        : (agentName ? `${agentName} $` : '$');
    const streaming = type === 'agent' && sealed === false;
    // The reasoning panel renders above the visible text for both streaming
    // and sealed messages. When empty it renders nothing, so we can
    // include it unconditionally here.
    const panel = (type === 'agent' && reasoning)
        ? html`<${ReasoningPanel} text=${reasoning} live=${streaming} />`
        : null;

    // Reasoning-only assistant turns (extended thinking with no body text,
    // e.g. when the model emits only thinking + tool_use blocks) would
    // otherwise leave an empty `<div class="msg-body markdown-body">` below
    // the reasoning panel — the .msg-body border-left + padding renders that
    // empty element as an orphan white-line stub. Suppress the body element
    // entirely when there's no text to show. (#853)
    const hasBody = typeof text === 'string' && text.trim().length > 0;

    // Per-message timestamp (#855) sits in the same row as the author
    // label so it stays unobtrusive and matches the conventional
    // Slack/Discord pattern. Suppressed while a message is still streaming
    // — the agent hasn't actually "spoken" yet, so the timestamp would
    // attach itself to a partial response.
    const showTs = ts && !streaming;

    // Render Markdown for sealed (finished) agent messages only.
    // While streaming, use plain text with pre-wrap to avoid running
    // marked.parse() + DOMPurify.sanitize() on every animation frame.
    if (type === 'agent' && sealed) {
        const rendered = hasBody ? renderMarkdown(text) : '';
        return html`
            <div class="msg ${cls}">
                <div class="msg-label-row">
                    <div class="msg-label">${label}</div>
                    ${showTs && html`<${MessageTimestamp} ts=${ts} />`}
                </div>
                ${panel}
                ${hasBody && html`<${MarkdownBody} html=${rendered} />`}
            </div>
        `;
    }

    return html`
        <div class="msg ${cls}">
            <div class="msg-label-row">
                <div class="msg-label">${label}</div>
                ${showTs && html`<${MessageTimestamp} ts=${ts} />`}
            </div>
            ${panel}
            ${(hasBody || streaming) && html`
                <div class="msg-body ${streaming ? 'streaming-cursor' : ''}">${text}</div>
            `}
        </div>
    `;
}

export function TokenBadge({ usage }) {
    if (!usage) return null;
    const p = usage.prompt_tokens || 0;
    const c = usage.completion_tokens || 0;
    if (p + c === 0) return null;
    // reasoning_tokens is only present when the provider reports chain-of-thought
    // usage separately (OpenAI o-series, DeepSeek R1, xAI reasoning variants).
    // Absent for non-reasoning runs; in that case the badge keeps its old shape.
    const r = usage.reasoning_tokens;
    const reasoningPart = (typeof r === 'number' && r > 0) ? ` + ${r}r` : '';
    return html`<div class="msg-tokens">${p}p + ${c}c${reasoningPart} tokens</div>`;
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
 * ended and the next began.
 *
 * Completed runs render as a very subtle thin line with no text (the
 * normal/expected case does not need a label).  Failed and cancelled
 * runs keep the labeled treatment since those are actionable.
 *
 * Props:
 *   status - 'completed' | 'cancelled' | 'failed'
 *   error  - optional error message (for failed runs)
 */
export function RunBoundary({ status, error }) {
    // Completed runs: subtle thin-line separator, no text.
    if (!status || status === 'completed') {
        return html`<div class="run-boundary run-boundary--completed" />`;
    }

    const statusCls = status === 'failed' ? 'run-boundary--failed'
        : status === 'cancelled' ? 'run-boundary--cancelled'
        : '';
    const label = status === 'failed' ? 'run failed'
        : status === 'cancelled' ? 'run cancelled'
        : `run ${status}`;
    return html`
        <div class="run-boundary ${statusCls}">
            <span class="run-boundary-label">${label}</span>
        </div>
        ${status === 'failed' && error && html`
            <div class="run-boundary-error">${error}</div>
        `}
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
