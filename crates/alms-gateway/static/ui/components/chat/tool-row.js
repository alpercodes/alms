import { html, useSignal } from '../../deps.js';
import { toolSummary, fmtSize } from '../../utils/tool-summary.js';
import { renderToolOutput } from '../../utils/tool-output.js';
import { TOOL_SUMMARY_LEN } from '../../utils/constants.js';

/** Threshold (in characters) above which result text is truncated with a toggle. */
const RESULT_TRUNCATE_LEN = 500;

/**
 * Soft cap for long single-line input previews (e.g. `invoke_agent.task`,
 * `send_message.message`, `fs_edit.old_string`). Truncating in the input
 * renderer keeps the expanded detail panel from blowing up vertically when
 * the agent passes a multi-paragraph parameter.
 */
const INPUT_PREVIEW_LEN = 200;

/** Truncate `s` to at most `n` chars, appending an ellipsis when cut. */
function truncateInput(s, n) {
    if (typeof s !== 'string') return '';
    if (s.length <= n) return s;
    return s.slice(0, n) + '…';
}

/**
 * Per-tool list of param fields that `renderParams` truncates with
 * `truncateInput`. When any of these fields exceeds `INPUT_PREVIEW_LEN`
 * the structured panel hides part of the payload, so we surface a
 * "View raw params" toggle (parallel to "View raw" on the result side).
 *
 * Fields that the renderer never truncates (paths, command strings,
 * URLs, agent names, etc.) are intentionally absent — those always
 * appear in full on screen, no recovery path needed.
 */
const TRUNCATED_INPUT_FIELDS = {
    fs_edit: ['old_string', 'new_string'],
    invoke_agent: ['task'],
    send_message: ['message'],
    ignore_message: ['reason'],
    echo: ['message', 'text'],
};

/**
 * Returns true if `renderParams(tool, params)` would truncate at least
 * one field in the rendered output. Used by `ParamsSection` to decide
 * whether to expose a "View raw params" toggle.
 *
 * Exported so the regression tests in
 * `crates/alms-gateway/tests/ui/tool-output.test.mjs` can pin the
 * detection logic against representative payloads.
 */
export function paramsAreTruncated(tool, params) {
    if (!params || typeof params !== 'object') return false;
    const fields = TRUNCATED_INPUT_FIELDS[tool];
    if (!fields) return false;
    for (const field of fields) {
        const v = params[field];
        if (typeof v === 'string' && v.length > INPUT_PREVIEW_LEN) return true;
    }
    return false;
}

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
 *
 * Per-tool branches mirror the input-formatting table in #974: every tool
 * known to the registry has a bespoke renderer so the operator sees a
 * natural representation of the inputs (`$ <command>`, `pattern in path`,
 * `to: <agent>`, etc.) rather than a `{"key": "value"}` JSON dump. The
 * raw-JSON fallback at the bottom is reserved for genuinely unknown
 * tools (e.g. third-party tools registered by the runtime).
 *
 * Long single-line fields (`fs_edit.old_string`, `invoke_agent.task`,
 * `send_message.message`, `ignore_message.reason`, `echo.message`) are
 * truncated at `INPUT_PREVIEW_LEN` to keep the panel compact. Truncation
 * is reversible from the UI: `ParamsSection` wraps this function with a
 * "View raw params" toggle (parallel to "View raw" on the result side)
 * when it detects truncation via `paramsAreTruncated`.
 *
 * Exported so the input-side regression tests in
 * `crates/alms-gateway/tests/ui/tool-output.test.mjs` can pin every shape
 * the dispatcher knows about.
 */
export function renderParams(tool, params) {
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
        case 'fs_read': {
            // File-path header + line-range badges when offset/limit are set.
            // Both are optional in the schema; when absent, just a path
            // header is shown (the result-side renderer covers the actual
            // file content).
            const offset = typeof params.offset === 'number' ? params.offset : null;
            const limit = typeof params.limit === 'number' ? params.limit : null;
            const hasRange = offset != null || limit != null;
            const rangeLabel = hasRange
                ? (limit != null
                    ? `lines ${(offset || 0) + 1}–${(offset || 0) + limit}`
                    : `from line ${(offset || 0) + 1}`)
                : null;
            return html`
                <div class="tc-detail-section">
                    <div class="tc-detail-label tc-file-header">${params.path || ''}</div>
                    ${rangeLabel && html`
                        <div class="tc-status-row">
                            <span class="tc-kv-meta">${rangeLabel}</span>
                        </div>
                    `}
                </div>
            `;
        }
        case 'fs_write':
            return html`
                <div class="tc-detail-section">
                    <div class="tc-detail-label tc-file-header">${params.path || ''}</div>
                    <div class="tc-status-row">
                        <span class="tc-kv-badge">
                            ${params.mode === 'append' ? 'append' : 'overwrite'}
                        </span>
                    </div>
                </div>
                ${params.content && html`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Content</div>
                        <pre class="tc-detail-content tc-code-block">${params.content}</pre>
                    </div>
                `}
            `;
        case 'fs_edit': {
            // file-path header + a compact search/replace summary. The full
            // strings are shown in their own panes underneath so the
            // operator can see exactly what was matched and what replaced
            // it. `replace_all` shows up as a meta pill; absence implies
            // the default (uniqueness-enforced) behaviour.
            const replaceAll = params.replace_all === true;
            return html`
                <div class="tc-detail-section">
                    <div class="tc-detail-label tc-file-header">${params.path || ''}</div>
                    <div class="tc-status-row">
                        <span class="tc-kv-badge">
                            ${replaceAll ? 'replace all' : 'replace once'}
                        </span>
                    </div>
                </div>
                ${params.old_string && html`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Find</div>
                        <pre class="tc-detail-content tc-code-block">${truncateInput(params.old_string, INPUT_PREVIEW_LEN)}</pre>
                    </div>
                `}
                ${params.new_string && html`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Replace with</div>
                        <pre class="tc-detail-content tc-code-block">${truncateInput(params.new_string, INPUT_PREVIEW_LEN)}</pre>
                    </div>
                `}
            `;
        }
        case 'fs_list':
            return html`
                <div class="tc-detail-section">
                    <div class="tc-detail-label tc-file-header">${params.path || '.'}</div>
                </div>
            `;
        case 'fs_grep': {
            // `<pattern> in <path>` header. Mode/case/glob badges only show
            // up when they deviate from the defaults so the common case
            // (default content search in cwd) stays visually quiet.
            const mode = typeof params.output_mode === 'string'
                ? params.output_mode : 'files_with_matches';
            const showMode = mode !== 'files_with_matches';
            return html`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Pattern</div>
                    <div class="tc-status-row">
                        <span class="tc-kv-mono">${params.pattern || ''}</span>
                        ${params.path && html`
                            <span class="tc-kv-meta">in</span>
                            <span class="tc-kv-mono">${params.path}</span>
                        `}
                    </div>
                    ${(showMode || params.glob || params.case_insensitive) && html`
                        <div class="tc-status-row">
                            ${showMode && html`<span class="tc-kv-badge">${mode}</span>`}
                            ${params.glob && html`<span class="tc-kv-meta">glob: ${params.glob}</span>`}
                            ${params.case_insensitive && html`<span class="tc-kv-meta">case-insensitive</span>`}
                        </div>
                    `}
                </div>
            `;
        }
        case 'fs_glob':
            return html`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Pattern</div>
                    <div class="tc-status-row">
                        <span class="tc-kv-mono">${params.pattern || ''}</span>
                        ${params.path && html`
                            <span class="tc-kv-meta">in</span>
                            <span class="tc-kv-mono">${params.path}</span>
                        `}
                    </div>
                </div>
            `;
        case 'invoke_agent': {
            const name = params.name || params.subagent_name || '';
            const isBackground = params.background === true;
            return html`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Agent</div>
                    <div class="tc-status-row">
                        ${name
                            ? html`<span class="tc-kv-badge">${name}</span>`
                            : html`<span class="tc-kv-meta">(ephemeral)</span>`
                        }
                        ${isBackground && html`<span class="tc-kv-meta">background</span>`}
                    </div>
                </div>
                ${params.task && html`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Task</div>
                        <pre class="tc-detail-content">${truncateInput(params.task, INPUT_PREVIEW_LEN)}</pre>
                    </div>
                `}
            `;
        }
        case 'http_get':
            if (params.url) {
                return html`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Request</div>
                        <div class="tc-status-row">
                            <span class="tc-kv-badge">GET</span>
                            <span class="tc-kv-mono">${params.url}</span>
                        </div>
                    </div>
                `;
            }
            break;
        case 'workspace_write': {
            const mode = params.mode === 'append' ? 'append' : 'write';
            return html`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Workspace</div>
                    <div class="tc-status-row">
                        <span class="tc-kv-badge">${params.file || ''}</span>
                        <span class="tc-kv-meta">${mode}</span>
                    </div>
                </div>
                ${params.content && html`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Content</div>
                        <pre class="tc-detail-content tc-code-block">${params.content}</pre>
                    </div>
                `}
            `;
        }
        case 'send_message':
            return html`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">To</div>
                    <div class="tc-status-row">
                        <span class="tc-kv-badge">${params.to || ''}</span>
                    </div>
                </div>
                ${params.message && html`
                    <div class="tc-detail-section">
                        <div class="tc-detail-label">Message</div>
                        <pre class="tc-detail-content">${truncateInput(params.message, INPUT_PREVIEW_LEN)}</pre>
                    </div>
                `}
            `;
        case 'read_messages': {
            // Filter summary: `from <agent>`, `last N`. last_n defaults to
            // 20 in the runtime; only show it as a meta pill when set
            // explicitly (the LLM's raw call carries the field even when
            // it equals the default — best-effort detection only).
            const lastN = typeof params.last_n === 'number' ? params.last_n : null;
            return html`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Filter</div>
                    <div class="tc-status-row">
                        ${params.from && html`<span class="tc-kv-meta">from</span><span class="tc-kv-badge">${params.from}</span>`}
                        ${lastN != null && html`<span class="tc-kv-meta">last ${lastN}</span>`}
                    </div>
                </div>
            `;
        }
        case 'read_session': {
            const sid = typeof params.session_id === 'string' && params.session_id
                ? params.session_id : null;
            const lastN = typeof params.last_n === 'number' ? params.last_n : null;
            const summaryOnly = params.summary_only === true;
            return html`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Session</div>
                    <div class="tc-status-row">
                        ${sid && html`<span class="tc-kv-mono">${sid}</span>`}
                        ${summaryOnly && html`<span class="tc-kv-badge">summary only</span>`}
                        ${lastN != null && html`<span class="tc-kv-meta">last ${lastN}</span>`}
                    </div>
                </div>
            `;
        }
        case 'read_subagent_session': {
            const lastN = typeof params.last_n === 'number' ? params.last_n : null;
            const summaryOnly = params.summary_only === true;
            return html`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Subagent</div>
                    <div class="tc-status-row">
                        ${params.name && html`<span class="tc-kv-badge">${params.name}</span>`}
                        ${summaryOnly && html`<span class="tc-kv-badge">summary only</span>`}
                        ${lastN != null && html`<span class="tc-kv-meta">last ${lastN}</span>`}
                    </div>
                </div>
            `;
        }
        case 'ignore_message': {
            const reason = typeof params.reason === 'string' && params.reason.length > 0
                ? params.reason : null;
            return html`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Ignore</div>
                    ${reason
                        ? html`<pre class="tc-detail-content">${truncateInput(reason, INPUT_PREVIEW_LEN)}</pre>`
                        : html`<div class="tc-detail-footer">no reason given</div>`
                    }
                </div>
            `;
        }
        case 'list_my_sessions': {
            const limit = typeof params.limit === 'number' ? params.limit : null;
            const includeCurrent = params.include_current === true;
            // Only render an input section if the LLM actually passed a
            // filter — otherwise the tool has effectively no input and the
            // header alone is enough.
            if (limit == null && !includeCurrent) return null;
            return html`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Filter</div>
                    <div class="tc-status-row">
                        ${limit != null && html`<span class="tc-kv-meta">limit ${limit}</span>`}
                        ${includeCurrent && html`<span class="tc-kv-badge">include current</span>`}
                    </div>
                </div>
            `;
        }
        case 'list_agents':
        case 'datetime':
            // Schemas are `{ "type": "object", "properties": {} }` — the
            // tools take no input, so suppress the params section
            // entirely. The output renderer carries the whole call.
            return null;
        case 'echo':
            return html`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Message</div>
                    <pre class="tc-detail-content">${truncateInput(params.message || params.text || '', INPUT_PREVIEW_LEN)}</pre>
                </div>
            `;
        case 'math': {
            const op = typeof params.operation === 'string' ? params.operation : '';
            const operands = [params.a, params.b, params.n]
                .filter(v => v !== undefined && v !== null);
            return html`
                <div class="tc-detail-section">
                    <div class="tc-detail-label">Expression</div>
                    <div class="tc-status-row">
                        ${op && html`<span class="tc-kv-badge">${op}</span>`}
                        ${operands.length > 0 && html`
                            <span class="tc-kv-mono">(${operands.join(', ')})</span>
                        `}
                    </div>
                </div>
            `;
        }
    }

    // Fallback: raw JSON for tools without specialized rendering. Reached
    // only for tools the dispatcher above doesn't know about — every
    // first-party tool in the registry has a bespoke branch.
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
 * Raw-JSON renderer for the params side. Used as the toggle target when
 * the structured `renderParams` view truncates one of the fields in
 * `TRUNCATED_INPUT_FIELDS`. Symmetric with the result-side
 * `RawResultPane` toggle so the operator always has a recovery path
 * back to the full payload.
 */
function RawParamsPane({ params }) {
    const text = formatJson(params);
    if (!text) return null;
    return html`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Parameters (raw)</div>
            <pre class="tc-detail-content">${text}</pre>
        </div>
    `;
}

/**
 * Wraps the per-tool `renderParams` output with an optional "View raw
 * params" toggle when any field in the structured panel has been
 * truncated by `truncateInput`. The toggle is parallel to the
 * "View raw" toggle on the result side (`ResultSection`) \u2014 symmetric
 * UX so a long `fs_edit.old_string` / `invoke_agent.task` /
 * `send_message.message` / `ignore_message.reason` / `echo.message` is
 * always recoverable from the panel.
 *
 * When no truncation is in play (the common case \u2014 short messages,
 * paths, commands, URLs, agent names), the toggle is suppressed and
 * the rendered DOM is byte-identical to the pre-toggle behaviour.
 */
function ParamsSection({ tool, params }) {
    const showRawParams = useSignal(false);
    const structured = renderParams(tool, params);
    if (!structured) return null;

    if (!paramsAreTruncated(tool, params)) {
        return structured;
    }

    const toggleRaw = (e) => {
        e.stopPropagation();
        showRawParams.value = !showRawParams.value;
    };
    return html`
        ${showRawParams.value
            ? html`<${RawParamsPane} params=${params} />`
            : structured
        }
        <div class="tc-detail-rawtoggle">
            <button class="tc-show-more" onClick=${toggleRaw}>
                ${showRawParams.value ? 'Hide raw params' : 'View raw params'}
            </button>
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
                    ${html`<${ParamsSection} tool=${tool} params=${params} />`}
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
