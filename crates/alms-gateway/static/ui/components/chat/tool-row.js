import { html, useSignal } from '../../deps.js';
import { toolSummary, fmtSize } from '../../utils/tool-summary.js';
import { renderToolOutput } from '../../utils/tool-output.js';
import { TOOL_SUMMARY_LEN } from '../../utils/constants.js';

/** Threshold (in characters) above which result text is truncated with a toggle. */
const RESULT_TRUNCATE_LEN = 500;

/** Format a duration in milliseconds to a human-readable string. */
function fmtDuration(ms) {
    if (ms == null) return '';
    if (ms < 1000) return ms + 'ms';
    if (ms < 60000) return (ms / 1000).toFixed(1) + 's';
    const mins = Math.floor(ms / 60000);
    const secs = Math.round((ms % 60000) / 1000);
    return mins + 'm ' + secs + 's';
}

/** Format JSON for display in the detail panel. */
function formatJson(val) {
    if (val == null) return '';
    if (typeof val === 'string') {
        // Try to parse as JSON for pretty display
        try {
            const parsed = JSON.parse(val);
            return JSON.stringify(parsed, null, 2);
        } catch {
            return val;
        }
    }
    return JSON.stringify(val, null, 2);
}

/**
 * Compute the byte size of a result for the size indicator.
 * Uses Blob to compute actual UTF-8 byte size.
 */
function resultByteSize(result) {
    if (result == null) return 0;
    const text = typeof result === 'string' ? result : JSON.stringify(result);
    return new Blob([text]).size;
}

/** Determine the tool icon based on tool name. */
function toolIcon(tool) {
    switch (tool) {
        case 'shell':
        case 'shell_exec':
            return '$';
        case 'fs_read':
            return 'R';
        case 'fs_write':
            return 'W';
        case 'fs_list':
            return 'L';
        case 'workspace_write':
            return 'W';
        case 'http_get':
            return 'H';
        case 'send_message':
            return 'DM';
        case 'invoke_agent':
            return 'IA';
        case 'read_session':
        case 'read_subagent_session':
            return 'RS';
        case 'list_agents':
            return 'LA';
        case 'list_my_sessions':
            return 'LS';
        case 'read_messages':
            return 'RM';
        case 'ignore_message':
            return 'IG';
        case 'math':
            return '#';
        case 'echo':
            return 'E';
        default:
            return 'T';
    }
}

/**
 * Render the structured parameters section for a specific tool type.
 * Returns an htm template for the params display.
 */
function renderParams(tool, params) {
    if (!params) return null;

    switch (tool) {
        case 'shell':
        case 'shell_exec':
            if (params.command) {
                return html`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Command</div>
                        <pre class="tc-detail-content tc-code-block">${params.command}</pre>
                    </div>
                `;
            }
            break;
        case 'fs_read':
            if (params.path) {
                return html`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Path</div>
                        <pre class="tc-detail-content">${params.path}</pre>
                    </div>
                `;
            }
            break;
        case 'fs_write':
            return html`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">
                        ${params.mode === 'append' ? 'Append to' : 'Write to'}
                    </div>
                    <pre class="tc-detail-content">${params.path || ''}</pre>
                </div>
                ${params.content && html`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Content</div>
                        <pre class="tc-detail-content tc-code-block">${params.content}</pre>
                    </div>
                `}
            `;
        case 'invoke_agent': {
            const name = params.name || params.subagent_name || '';
            return html`
                ${name && html`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Agent</div>
                        <pre class="tc-detail-content">${name}</pre>
                    </div>
                `}
                ${params.task && html`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Task</div>
                        <pre class="tc-detail-content">${params.task}</pre>
                    </div>
                `}
            `;
        }
        case 'http_get':
            if (params.url) {
                return html`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">URL</div>
                        <pre class="tc-detail-content">${params.url}</pre>
                    </div>
                `;
            }
            break;
        case 'send_message':
            return html`
                ${params.to && html`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">To</div>
                        <pre class="tc-detail-content">${params.to}</pre>
                    </div>
                `}
                ${params.message && html`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Message</div>
                        <pre class="tc-detail-content">${params.message}</pre>
                    </div>
                `}
            `;
    }

    // Fallback: raw JSON for tools without specialized rendering
    const text = formatJson(params);
    if (!text) return null;
    return html`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Parameters</div>
            <pre class="tc-detail-content">${text}</pre>
        </div>
    `;
}

/**
 * Raw-JSON renderer \u2014 used for tool outputs the structured dispatcher does
 * not handle, for failures, and as the universal "Raw" toggle target on
 * every tool (acceptance criterion 2 of #873). Kept identical to the
 * pre-#873 default so callers who toggle Raw see the exact same blob the
 * tool returned.
 */
function RawResultPane({ result, isFail, showFull, label, blockedTarget }) {
    const text = formatJson(result);
    if (!text) return null;
    const isLong = text.length > RESULT_TRUNCATE_LEN;
    const displayText = (!showFull.value && isLong)
        ? text.slice(0, RESULT_TRUNCATE_LEN) + '\u2026'
        : text;
    const expandedCls = showFull.value ? ' tc-detail-expanded' : '';
    const toggleFull = (e) => {
        e.stopPropagation();
        showFull.value = !showFull.value;
    };
    return html`
        ${blockedTarget && html`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Target</div>
                <pre class="tc-detail-content tc-code-block tc-detail-error">${blockedTarget}</pre>
            </div>
        `}
        <div class="tc-detail-section">
            <div class="tc-detail-label">${label}</div>
            <pre class="tc-detail-content${expandedCls} ${isFail ? 'tc-detail-error' : ''}">${displayText}</pre>
            ${isLong && !isFail && html`
                <button class="tc-show-more" onClick=${toggleFull}>
                    ${showFull.value ? 'Show less' : 'Show more'}
                </button>
            `}
        </div>
    `;
}

/**
 * Render the structured result section for a specific tool type.
 *
 * Path 1 (#873): the structured dispatcher in `utils/tool-output.js` owns
 * per-tool rendering. When it returns a template we show that, plus a small
 * "Raw" toggle so operators can drop down to the JSON blob for debugging.
 *
 * Path 2 (fallback): when the dispatcher returns null \u2014 either the tool
 * has no bespoke renderer or the payload shape didn't match \u2014 we fall
 * back to the raw renderer with a longer-truncation Show-more toggle.
 *
 * Path 3 (failures): on `isFail` we always show the raw error pane, never
 * the structured renderer (the payload shape on failure is the runtime's
 * `{error, target?}` shell rather than the success shape).
 */
function ResultSection({ tool, params, result, isFail, isCancelled, showFull }) {
    const showRaw = useSignal(false);

    if (result == null && !isFail) return null;

    // Classifier-extracted target on a blocked shell command (#758).
    const blockedTarget = (isFail
        && typeof result === 'object' && result !== null
        && typeof result.target === 'string' && result.target.length > 0)
        ? result.target
        : null;

    // Failure path \u2014 always raw, never structured.
    if (isFail) {
        return html`<${RawResultPane} result=${result} isFail=${true}
            showFull=${showFull} label="Error" blockedTarget=${blockedTarget} />`;
    }

    // Cancelled-with-partial-result path \u2014 the runtime persists a
    // partial result for debugging when a tool is cancelled mid-flight
    // (see "partial tool call records persisted on error/cancellation"
    // in the project state). Don't dress those up with a structured
    // renderer; they're partial by definition and the operator wants
    // to see the raw payload, not a misleading "ok" badge.
    if (isCancelled) {
        return html`<${RawResultPane} result=${result} isFail=${false}
            showFull=${showFull} label="Result (cancelled)" />`;
    }

    // Success path \u2014 try the structured renderer first.
    const structured = renderToolOutput(tool, result, params, { showFull });

    if (!structured) {
        // No bespoke renderer or unrecognised shape \u2014 raw view, no toggle
        // (raw is already the only thing on screen).
        return html`<${RawResultPane} result=${result} isFail=${false}
            showFull=${showFull} label="Result" />`;
    }

    // Structured renderer matched \u2014 show it plus a Raw toggle that flips
    // to the raw pane on demand. The toggle button itself lives in its
    // own row so it sits below the structured output regardless of which
    // sub-section produced it.
    const toggleRaw = (e) => {
        e.stopPropagation();
        showRaw.value = !showRaw.value;
    };
    return html`
        ${showRaw.value
            ? html`<${RawResultPane} result=${result} isFail=${false}
                showFull=${showFull} label="Result (raw)" />`
            : structured
        }
        <div class="tc-detail-rawtoggle">
            <button class="tc-show-more" onClick=${toggleRaw}>
                ${showRaw.value ? 'Hide raw' : 'View raw'}
            </button>
        </div>
    `;
}


export function ToolRow({ tool, params, status, result, id, sourceAgent, durationMs }) {
    const expanded = useSignal(false);
    const showFull = useSignal(false);
    const toggle = (e) => {
        e.stopPropagation();
        expanded.value = !expanded.value;
    };

    const summary = toolSummary(tool, params);
    const truncSummary = summary.length > TOOL_SUMMARY_LEN
        ? summary.slice(0, TOOL_SUMMARY_LEN) + '\u2026'
        : summary;

    const isRunning = status === 'running';
    const isFail = status === 'fail';
    const isDone = status === 'done';
    const isCancelled = status === 'cancelled';
    const isDm = tool === 'send_message';

    const statusCls = isFail ? 'tc-fail' : isDone ? 'tc-done'
        : isCancelled ? 'tc-cancelled' : 'tc-running';

    const chevron = expanded.value ? '\u25BC' : '\u25B6';
    const icon = toolIcon(tool);
    const duration = fmtDuration(durationMs);

    // Result size indicator (only shown when result exists and is not trivially small)
    const size = result != null ? resultByteSize(result) : 0;
    const sizeLabel = size >= 100 ? fmtSize(size) : '';

    return html`
        <div class="tc-row ${statusCls} ${isDm ? 'tc-dm' : ''}" role="button" tabindex="0"
             onClick=${toggle} onKeyDown=${(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggle(e); } }}>
            <div class="tc-header">
                <span class="tc-chevron">${chevron}</span>
                ${isRunning
                    ? html`<span class="tc-spinner"></span>`
                    : html`<span class="tc-icon">${icon}</span>`
                }
                <span class="tc-name">${tool}</span>
                ${truncSummary && html`<span class="tc-summary">${truncSummary}</span>`}
                <span class="tc-spacer"></span>
                ${sizeLabel && html`<span class="tc-result-size">${sizeLabel}</span>`}
                ${duration && html`<span class="tc-duration">${duration}</span>`}
                ${isFail && html`<span class="tc-status-badge tc-badge-fail">failed</span>`}
                ${isCancelled && html`<span class="tc-status-badge tc-badge-cancelled">cancelled</span>`}
                ${isDone && html`<span class="tc-status-icon">\u2713</span>`}
            </div>
            ${expanded.value && html`
                <div class="tc-detail" onClick=${(e) => e.stopPropagation()}>
                    ${renderParams(tool, params)}
                    ${html`<${ResultSection} tool=${tool} params=${params}
                        result=${result} isFail=${isFail} isCancelled=${isCancelled}
                        showFull=${showFull} />`}
                </div>
            `}
        </div>
    `;
}


/**
 * ToolGroup wraps consecutive tool calls that were executed in parallel
 * (i.e. appear adjacent in the message list with no text between them).
 * Renders them inside a shared container with a subtle visual grouping.
 */
export function ToolGroup({ children, count }) {
    if (count <= 1) return children;
    return html`
        <div class="tc-group">
            <div class="tc-group-label">${count} tools in parallel</div>
            ${children}
        </div>
    `;
}
