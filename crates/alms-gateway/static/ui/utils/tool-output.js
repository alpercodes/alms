// Tool-output rendering helpers — output-side parity with tool-summary.js (#873).
//
// Where `tool-summary.js` formats the *input* parameters of a tool call into a
// one-liner for the collapsed header, the helpers below format the *output*
// (the result object returned by the tool) into structured DOM in the
// expanded detail panel.
//
// Each renderer takes (result, params, opts) and returns either:
//   - an htm template, or
//   - `null` if the renderer wants to fall through to the raw-JSON view.
//
// Conventions matching tool-summary.js / tool-row.js:
//   - htm `html\`...\`` templates (no JSX)
//   - CSS classes only — no inline styles
//   - All strings rendered via interpolation are escaped by Preact, so even
//     untrusted tool output (file content, shell stdout, http bodies) is
//     safe to drop into a `<pre>` directly.
//
// Anything not handled here falls back to the existing raw-JSON renderer in
// tool-row.js (the "Raw" toggle is the universal escape hatch — acceptance
// criterion 2 of the issue).

import { html } from '../deps.js';
import { navigateToSession } from './navigate-session.js';

/**
 * Maximum response-body length (in characters) before the http_get body pane
 * gets a Show-more / Show-less toggle. The shell / fs_read code paths use
 * tool-row.js's existing `RESULT_TRUNCATE_LEN` (500); 2000 is more useful for
 * HTTP bodies which tend to run longer than a typical shell stdout.
 */
const HTTP_BODY_TRUNCATE_LEN = 2000;

/**
 * Maximum length of an invoke_agent response preview in the collapsed view.
 * Matches the input-side preview budget used in tool-summary.js's invoke_agent
 * branch (60 chars there is for the one-liner; the detail pane is allowed
 * more breathing room).
 */
const INVOKE_AGENT_PREVIEW_LEN = 800;

/**
 * Compact a path for display headers — last 2 segments, prefixed with `…/`.
 * Mirrors the helper in tool-row.js (kept in sync intentionally — this
 * module avoids importing from tool-row.js to keep the dep graph one-way).
 */
function shortPath(path) {
    if (!path) return '';
    const parts = path.replace(/\\/g, '/').split('/').filter(Boolean);
    if (parts.length <= 2) return parts.join('/');
    return '…/' + parts.slice(-2).join('/');
}

/**
 * Detect whether a `result` is the structured-error shape that the
 * runtime writes when a tool throws. We render those identically to the
 * default fallback (red error pane) — the per-tool renderers only run on
 * the success path.
 */
function isErrorShape(result) {
    return (
        typeof result === 'object'
        && result !== null
        && typeof result.error === 'string'
    );
}

// ── shell / shell_exec ───────────────────────────────────────────────────

/**
 * Render shell tool output: separate panes for stdout / stderr, a status
 * row showing exit code (and command for background-task results), and a
 * monospace code block for each pane.
 *
 * Payload shapes (from `crates/alms-sandbox/src/shell/mod.rs`):
 *   - foreground:        { exit_code, stdout, stderr }
 *   - background submit: { task_id, status: "submitted", message }
 *   - background done:   { task_id, status: "completed", command, exit_code, stdout, stderr }
 *   - background fail:   { task_id, status: "failed", command, error }
 *   - background other:  { task_id, status: "unknown" | "not_found_or_still_running" }
 */
export function renderShellOutput(result) {
    if (typeof result !== 'object' || result === null) return null;
    if (isErrorShape(result)) return null;

    const taskId = typeof result.task_id === 'string' ? result.task_id : null;
    const status = typeof result.status === 'string' ? result.status : null;
    const command = typeof result.command === 'string' ? result.command : null;
    const exitCode = (typeof result.exit_code === 'number') ? result.exit_code : null;
    const stdout = typeof result.stdout === 'string' ? result.stdout : '';
    const stderr = typeof result.stderr === 'string' ? result.stderr : '';
    const errMsg = typeof result.error === 'string' ? result.error : null;
    const note = typeof result.message === 'string' ? result.message : null;

    // Background task submission — there's no stdout/stderr yet.
    if (taskId && (status === 'submitted' || status === 'unknown'
        || status === 'not_found_or_still_running')) {
        return html`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Status</div>
                <div class="tc-status-row">
                    <span class="tc-kv-badge">${status}</span>
                    <span class="tc-kv-mono">task_id: ${taskId}</span>
                </div>
                ${note && html`
                    <pre class="tc-detail-content">${note}</pre>
                `}
            </div>
        `;
    }

    // Background task that ran but errored before producing stdout/stderr.
    if (taskId && status === 'failed' && errMsg) {
        return html`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Status</div>
                <div class="tc-status-row">
                    <span class="tc-kv-badge tc-kv-badge-fail">${status}</span>
                    ${command && html`<span class="tc-kv-mono">${command}</span>`}
                </div>
            </div>
            <div class="tc-detail-section">
                <div class="tc-detail-label">Error</div>
                <pre class="tc-detail-content tc-detail-error">${errMsg}</pre>
            </div>
        `;
    }

    // Foreground (or completed background) with stdout/stderr/exit_code.
    // The "no shape we recognise" case returns null so tool-row.js falls
    // through to the raw renderer.
    if (exitCode == null && !stdout && !stderr) return null;

    const exitOk = exitCode === 0 || exitCode == null;
    const exitClass = exitOk ? 'tc-kv-badge' : 'tc-kv-badge tc-kv-badge-fail';

    return html`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Status</div>
            <div class="tc-status-row">
                <span class="${exitClass}">
                    exit ${exitCode == null ? '?' : exitCode}
                </span>
                ${taskId && html`<span class="tc-kv-mono">task_id: ${taskId}</span>`}
            </div>
        </div>
        ${stdout && html`
            <div class="tc-detail-section">
                <div class="tc-detail-label">stdout</div>
                <pre class="tc-detail-content tc-code-block">${stdout}</pre>
            </div>
        `}
        ${stderr && html`
            <div class="tc-detail-section">
                <div class="tc-detail-label">stderr</div>
                <pre class="tc-detail-content tc-code-block tc-detail-warn">${stderr}</pre>
            </div>
        `}
    `;
}

// ── fs_read ──────────────────────────────────────────────────────────────

/**
 * Render fs_read output: file path header + monospace content pane.
 *
 * Payload shape (from `crates/alms-sandbox/src/builtin/fs_read.rs`):
 *   { content, lines_returned, has_more_before, has_more_after,
 *     [total_lines, offset, limit, line_truncated, byte_budget_exceeded, note] }
 *
 * Empty-file shape:  { content: "", lines_returned: 0, has_more_before: false,
 *                      has_more_after: false, note: "File exists but is empty." }
 */
export function renderFsReadOutput(result, params) {
    if (typeof result !== 'object' || result === null) return null;
    if (isErrorShape(result)) return null;
    if (typeof result.content !== 'string') return null;

    const path = (params && params.path) || '';
    const linesReturned = typeof result.lines_returned === 'number'
        ? result.lines_returned : null;
    const totalLines = typeof result.total_lines === 'number'
        ? result.total_lines : null;
    const hasMoreBefore = result.has_more_before === true;
    const hasMoreAfter = result.has_more_after === true;
    const note = typeof result.note === 'string' ? result.note : null;
    const byteBudgetExceeded = result.byte_budget_exceeded === true;
    const lineTruncated = result.line_truncated === true;

    // Build a compact "footer" line summarising the truncation flags so the
    // operator sees them without scrolling the JSON. Keep it terse — the
    // markers are also visible inline in `content` (#902 cat-n line caps).
    const footerBits = [];
    if (linesReturned != null && totalLines != null) {
        if (linesReturned === totalLines) {
            footerBits.push(`${linesReturned} lines (full file)`);
        } else {
            footerBits.push(`${linesReturned} of ${totalLines} lines`);
        }
    } else if (linesReturned != null) {
        footerBits.push(`${linesReturned} lines`);
    }
    if (hasMoreBefore) footerBits.push('more before');
    if (hasMoreAfter) footerBits.push('more after');
    if (byteBudgetExceeded) footerBits.push('byte-budget exceeded');
    if (lineTruncated) footerBits.push('per-line truncated');
    const footer = footerBits.join(' · ');

    // The runtime already returns `cat -n`-style line numbers in
    // `content`; we render them verbatim per the issue note that line
    // numbers in v1 are only kept when they fall out cheap.
    const content = result.content || '';

    return html`
        <div class="tc-detail-section">
            <div class="tc-detail-label tc-file-header">
                ${path ? shortPath(path) : 'File content'}
            </div>
            ${content
                ? html`<pre class="tc-detail-content tc-code-block">${content}</pre>`
                : html`<pre class="tc-detail-content tc-detail-muted">${note || '(empty)'}</pre>`
            }
            ${footer && html`<div class="tc-detail-footer">${footer}</div>`}
            ${content && note && html`<div class="tc-detail-footer">${note}</div>`}
        </div>
    `;
}

// ── fs_write / fs_edit / workspace_write ─────────────────────────────────

/**
 * Render fs_write / fs_edit / workspace_write output: success badge +
 * file/path + (fs_edit) replacement count or (workspace_write) mode.
 *
 * Payload shapes diverge slightly across the three tools — the
 * dispatcher routes all three here so they share visual treatment:
 *   - fs_write:        { ok: true, path }                  (alms-sandbox/src/builtin/fs_write.rs)
 *   - fs_edit:         { ok: true, path, replacements }    (alms-sandbox/src/builtin/fs_edit.rs)
 *   - workspace_write: { ok: true, file, mode }            (alms-runtime/src/workspace_tool.rs)
 *
 * `path` and `file` are interchangeable for display; `mode` is rendered
 * as a meta pill alongside (workspace_write only carries it).
 */
export function renderFsWriteEditOutput(result) {
    if (typeof result !== 'object' || result === null) return null;
    if (isErrorShape(result)) return null;

    // Accept either `path` (fs_write/fs_edit) or `file` (workspace_write).
    // The two never co-exist on the same payload — picking either is safe.
    const target = (typeof result.path === 'string' && result.path)
        || (typeof result.file === 'string' && result.file)
        || null;
    const replacements = typeof result.replacements === 'number'
        ? result.replacements : null;
    const mode = typeof result.mode === 'string' ? result.mode : null;
    const ok = result.ok === true;
    if (!target) return null;

    return html`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Result</div>
            <div class="tc-status-row">
                <span class="tc-kv-badge ${ok ? '' : 'tc-kv-badge-fail'}">
                    ${ok ? 'ok' : 'failed'}
                </span>
                <span class="tc-kv-mono">${target}</span>
                ${replacements != null && html`
                    <span class="tc-kv-meta">
                        ${replacements} ${replacements === 1 ? 'replacement' : 'replacements'}
                    </span>
                `}
                ${mode && html`
                    <span class="tc-kv-meta">${mode}</span>
                `}
            </div>
        </div>
    `;
}

// ── fs_grep ──────────────────────────────────────────────────────────────

/**
 * Render fs_grep output: structured match list keyed by mode.
 *
 * Payload shapes (from `crates/alms-sandbox/src/builtin/fs_grep.rs`):
 *   - files mode:    { matches: ["path", ...], total, truncated, truncated_lines }
 *   - content mode:  { matches: [{file, line, content, context_before[], context_after[]}, ...],
 *                      total, truncated, truncated_lines }
 *   - count mode:    { matches: [{file, count}, ...], total_matches, truncated, truncated_lines }
 */
export function renderFsGrepOutput(result) {
    if (typeof result !== 'object' || result === null) return null;
    if (isErrorShape(result)) return null;
    if (!Array.isArray(result.matches)) return null;

    const matches = result.matches;
    const truncated = result.truncated === true;
    const truncatedLines = typeof result.truncated_lines === 'number'
        && result.truncated_lines > 0 ? result.truncated_lines : 0;

    // Empty match set — render an explicit "no matches" state instead of
    // an empty list which looks like a render bug.
    if (matches.length === 0) {
        return html`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Matches</div>
                <div class="tc-detail-footer">No matches found.</div>
            </div>
        `;
    }

    // Mode dispatch — sniff by entry shape rather than carrying a `mode`
    // field through the API. files mode produces strings; content mode
    // produces { file, line, content, ... }; count mode produces { file, count }.
    const first = matches[0];
    let body;
    if (typeof first === 'string') {
        // files mode
        body = html`
            <ul class="tc-match-list">
                ${matches.map(p => html`
                    <li class="tc-match-row tc-match-files">
                        <span class="tc-match-path">${p}</span>
                    </li>
                `)}
            </ul>
        `;
    } else if (first && typeof first.count === 'number'
        && typeof first.file === 'string') {
        // count mode
        body = html`
            <ul class="tc-match-list">
                ${matches.map(m => html`
                    <li class="tc-match-row tc-match-count">
                        <span class="tc-match-path">${m.file}</span>
                        <span class="tc-kv-meta">${m.count}</span>
                    </li>
                `)}
            </ul>
        `;
    } else if (first && typeof first.file === 'string'
        && typeof first.line === 'number') {
        // content mode — when the operator passed `context_before` /
        // `context_after`, the runtime returns those lines alongside the
        // match. Render them as dimmed siblings of the match line so the
        // context the operator paid for actually shows up. The arrays
        // are empty when no context was requested (default case).
        body = html`
            <ul class="tc-match-list">
                ${matches.map(m => {
                    const before = Array.isArray(m.context_before) ? m.context_before : [];
                    const after = Array.isArray(m.context_after) ? m.context_after : [];
                    return html`
                        <li class="tc-match-row tc-match-content">
                            <div class="tc-match-loc">
                                <span class="tc-match-path">${m.file}</span>
                                <span class="tc-match-sep">:</span>
                                <span class="tc-match-line">${m.line}</span>
                            </div>
                            ${before.length > 0 && html`
                                <pre class="tc-match-snippet tc-match-context">${before.join('\n')}</pre>
                            `}
                            <pre class="tc-match-snippet">${m.content || ''}</pre>
                            ${after.length > 0 && html`
                                <pre class="tc-match-snippet tc-match-context">${after.join('\n')}</pre>
                            `}
                        </li>
                    `;
                })}
            </ul>
        `;
    } else {
        // Unknown shape — fall through to raw view.
        return null;
    }

    const totalMatches = typeof result.total_matches === 'number'
        ? result.total_matches : null;
    const total = typeof result.total === 'number' ? result.total : null;
    const footerBits = [];
    if (totalMatches != null) {
        footerBits.push(`${totalMatches} match${totalMatches === 1 ? '' : 'es'}`);
    } else if (total != null) {
        footerBits.push(`${total} match${total === 1 ? '' : 'es'}`);
    }
    if (truncated) footerBits.push('output truncated');
    if (truncatedLines > 0) {
        footerBits.push(`${truncatedLines} per-line truncated`);
    }

    return html`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Matches</div>
            ${body}
            ${footerBits.length > 0 && html`
                <div class="tc-detail-footer">${footerBits.join(' · ')}</div>
            `}
        </div>
    `;
}

// ── fs_glob ──────────────────────────────────────────────────────────────

/**
 * Render fs_glob output: bare file list + total/truncated footer.
 *
 * Payload shape (from `crates/alms-sandbox/src/builtin/fs_glob.rs`):
 *   { files: ["path", ...], total, truncated }
 */
export function renderFsGlobOutput(result) {
    if (typeof result !== 'object' || result === null) return null;
    if (isErrorShape(result)) return null;
    if (!Array.isArray(result.files)) return null;

    const files = result.files;
    const total = typeof result.total === 'number' ? result.total : files.length;
    const truncated = result.truncated === true;

    if (files.length === 0) {
        return html`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Files</div>
                <div class="tc-detail-footer">No files matched.</div>
            </div>
        `;
    }

    const footerBits = [];
    if (total === files.length) {
        footerBits.push(`${total} file${total === 1 ? '' : 's'}`);
    } else {
        footerBits.push(`${files.length} of ${total} files`);
    }
    if (truncated) footerBits.push('output truncated');

    return html`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Files</div>
            <ul class="tc-match-list">
                ${files.map(p => html`
                    <li class="tc-match-row tc-match-files">
                        <span class="tc-match-path">${p}</span>
                    </li>
                `)}
            </ul>
            <div class="tc-detail-footer">${footerBits.join(' · ')}</div>
        </div>
    `;
}

// ── fs_list ──────────────────────────────────────────────────────────────

/**
 * Render fs_list output: directory header + entries grouped by type.
 *
 * Payload shape (from `crates/alms-sandbox/src/builtin/fs_list.rs`):
 *   { path, entries: [{ name, is_dir }] }
 */
export function renderFsListOutput(result) {
    if (typeof result !== 'object' || result === null) return null;
    if (isErrorShape(result)) return null;
    if (!Array.isArray(result.entries)) return null;

    const path = typeof result.path === 'string' ? result.path : '';
    const entries = result.entries;

    if (entries.length === 0) {
        return html`
            <div class="tc-detail-section">
                <div class="tc-detail-label tc-file-header">${path || '/'}</div>
                <div class="tc-detail-footer">Empty directory.</div>
            </div>
        `;
    }

    return html`
        <div class="tc-detail-section">
            <div class="tc-detail-label tc-file-header">${path || '/'}</div>
            <ul class="tc-match-list">
                ${entries.map(e => html`
                    <li class="tc-match-row ${e.is_dir ? 'tc-match-dir' : 'tc-match-file'}">
                        <span class="tc-match-path">
                            ${e.is_dir ? `${e.name || ''}/` : (e.name || '')}
                        </span>
                    </li>
                `)}
            </ul>
            <div class="tc-detail-footer">
                ${entries.length} ${entries.length === 1 ? 'entry' : 'entries'}
            </div>
        </div>
    `;
}

// ── http_get ─────────────────────────────────────────────────────────────

/**
 * Render http_get output: status badge, content-type, body in monospace
 * with a JSON pretty-print path when content_type is application/json.
 * Per-#956 the result also carries a `headers` map; we render it as a
 * native `<details>` block that is collapsed by default so it doesn't
 * dominate the panel when the operator is looking at the body.
 *
 * Payload shape (from `crates/alms-sandbox/src/builtin/http_get.rs`):
 *   { status, content_type, headers, body }
 *
 * `headers` is a `{ name: [value, ...] }` map, with lowercased keys and
 * a `Vec<String>` per name so multi-value headers (`Set-Cookie`, `Vary`)
 * keep their structure. Pre-#956 results without the field render
 * exactly as before — the show-more toggle is hidden when the map is
 * missing or empty.
 */
export function renderHttpGetOutput(result, _params, opts) {
    const showFull = opts && opts.showFull;
    if (typeof result !== 'object' || result === null) return null;
    if (isErrorShape(result)) return null;

    const status = typeof result.status === 'number' ? result.status : null;
    const contentType = typeof result.content_type === 'string'
        ? result.content_type : null;
    const body = result.body;
    if (status == null && body === undefined) return null;

    const isJson = contentType
        && contentType.toLowerCase().includes('application/json');

    // Body normalisation: backend already parses JSON when content_type
    // smells like JSON, so `body` may be an object/array (pretty-print) or
    // a string (render verbatim).
    let bodyText;
    if (typeof body === 'string') {
        bodyText = body;
    } else if (body == null) {
        bodyText = '';
    } else {
        // Object / array — pretty-print regardless of content_type, but
        // ensure JSON-ish.
        try {
            bodyText = JSON.stringify(body, null, 2);
        } catch {
            bodyText = String(body);
        }
    }

    const isLong = bodyText.length > HTTP_BODY_TRUNCATE_LEN;
    const displayBody = (showFull && !showFull.value && isLong)
        ? bodyText.slice(0, HTTP_BODY_TRUNCATE_LEN) + '…'
        : bodyText;

    const toggleFull = (e) => {
        e.stopPropagation();
        if (showFull) showFull.value = !showFull.value;
    };

    const statusOk = status != null && status >= 200 && status < 400;
    const statusCls = statusOk ? 'tc-kv-badge' : 'tc-kv-badge tc-kv-badge-fail';

    // Normalise the headers field into a flat list of [key, value] rows
    // for rendering. The backend returns `{ key: [value, ...] }` already
    // sorted by key (BTreeMap server-side), but we re-sort defensively in
    // case the order is lost in transit / a future provider returns a
    // plain object. Multi-value headers expand to one row per value so
    // `Set-Cookie: a=1` / `Set-Cookie: b=2` render as two separate rows.
    const headerRows = [];
    if (result.headers && typeof result.headers === 'object'
        && !Array.isArray(result.headers)) {
        const keys = Object.keys(result.headers).sort();
        for (const k of keys) {
            const v = result.headers[k];
            if (Array.isArray(v)) {
                for (const item of v) {
                    headerRows.push([k, typeof item === 'string' ? item : String(item)]);
                }
            } else if (typeof v === 'string') {
                // Pre-#956 / non-array shape — fold through verbatim.
                headerRows.push([k, v]);
            }
        }
    }
    const headerCount = headerRows.length;

    return html`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Response</div>
            <div class="tc-status-row">
                ${status != null && html`
                    <span class="${statusCls}">${status}</span>
                `}
                ${contentType && html`
                    <span class="tc-kv-meta">${contentType}</span>
                `}
            </div>
        </div>
        ${bodyText && html`
            <div class="tc-detail-section">
                <div class="tc-detail-label">
                    Body${isJson ? ' (JSON)' : ''}
                </div>
                <pre class="tc-detail-content tc-code-block">${displayBody}</pre>
                ${isLong && showFull && html`
                    <button class="tc-show-more" onClick=${toggleFull}>
                        ${showFull.value ? 'Show less' : 'Show more'}
                    </button>
                `}
            </div>
        `}
        ${headerCount > 0 && html`
            <details class="tc-detail-section tc-http-headers">
                <summary class="tc-detail-label tc-http-headers-summary">
                    Headers (${headerCount})
                </summary>
                <ul class="tc-record-list tc-http-headers-list">
                    ${headerRows.map(([k, v]) => html`
                        <li class="tc-record-row tc-http-header-row">
                            <span class="tc-kv-mono tc-http-header-key">${k}</span>
                            <span class="tc-kv-meta tc-http-header-value">${v}</span>
                        </li>
                    `)}
                </ul>
            </details>
        `}
    `;
}

// ── invoke_agent ─────────────────────────────────────────────────────────

/**
 * Render invoke_agent output: subagent-name header, response preview, and
 * a "View full session" link to the subagent's session.
 *
 * Payload shapes (from `crates/alms-tools/src/invoke_agent.rs`):
 *   - foreground: { response, session_id }
 *   - background: { task_id, session_id }
 */
export function renderInvokeAgentOutput(result, params, opts) {
    const showFull = opts && opts.showFull;
    if (typeof result !== 'object' || result === null) return null;
    if (isErrorShape(result)) return null;

    const taskId = typeof result.task_id === 'string' ? result.task_id : null;
    const sessionId = typeof result.session_id === 'string'
        ? result.session_id : null;
    const response = typeof result.response === 'string' ? result.response : '';
    const name = (params && (params.name || params.subagent_name)) || '';

    // Background: just task + session id, no response yet.
    if (taskId) {
        return html`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Subagent (background)</div>
                <div class="tc-status-row">
                    ${name && html`<span class="tc-kv-badge">${name}</span>`}
                    <span class="tc-kv-mono">task_id: ${taskId}</span>
                </div>
                ${sessionId && html`
                    <button class="tc-detail-link"
                        type="button"
                        onClick=${(e) => {
                            e.stopPropagation();
                            navigateToSession(sessionId, { logPrefix: 'invokeAgentLink' });
                        }}>
                        View full session
                    </button>
                `}
            </div>
        `;
    }

    // Foreground: show preview + link.
    if (!response && !sessionId) return null;

    const isLong = response.length > INVOKE_AGENT_PREVIEW_LEN;
    const displayResponse = (showFull && !showFull.value && isLong)
        ? response.slice(0, INVOKE_AGENT_PREVIEW_LEN) + '…'
        : response;

    const toggleFull = (e) => {
        e.stopPropagation();
        if (showFull) showFull.value = !showFull.value;
    };

    return html`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Subagent</div>
            <div class="tc-status-row">
                ${name && html`<span class="tc-kv-badge">${name}</span>`}
                <span class="tc-kv-meta">completed</span>
            </div>
        </div>
        ${response && html`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Response</div>
                <pre class="tc-detail-content">${displayResponse}</pre>
                ${isLong && showFull && html`
                    <button class="tc-show-more" onClick=${toggleFull}>
                        ${showFull.value ? 'Show less' : 'Show more'}
                    </button>
                `}
            </div>
        `}
        ${sessionId && html`
            <div class="tc-detail-section">
                <button class="tc-detail-link"
                    type="button"
                    onClick=${(e) => {
                        e.stopPropagation();
                        navigateToSession(sessionId, { logPrefix: 'invokeAgentLink' });
                    }}>
                    View full session
                </button>
            </div>
        `}
    `;
}

// ── send_message ─────────────────────────────────────────────────────────

/**
 * Render send_message output: delivery confirmation + DM session link.
 *
 * Payload shape (from `crates/alms-tools/src/send_message.rs`):
 *   - success: { delivered: true, dm_session_id, note }
 *   - depth-exceeded / self-message / not-found: { error }
 */
export function renderSendMessageOutput(result) {
    if (typeof result !== 'object' || result === null) return null;
    if (isErrorShape(result)) return null;

    const delivered = result.delivered === true;
    const dmSessionId = typeof result.dm_session_id === 'string'
        ? result.dm_session_id : null;
    const note = typeof result.note === 'string' ? result.note : null;
    if (!delivered && !dmSessionId) return null;

    return html`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Delivery</div>
            <div class="tc-status-row">
                <span class="tc-kv-badge">${delivered ? 'delivered' : 'pending'}</span>
                ${dmSessionId && html`
                    <span class="tc-kv-mono">session: ${dmSessionId}</span>
                `}
            </div>
            ${note && html`<div class="tc-detail-footer">${note}</div>`}
        </div>
    `;
}

// ── read_messages / read_session / read_subagent_session ────────────────

/**
 * Render a chat-history payload as a stack of compact chat-bubble rows.
 * Shared between `read_messages`, `read_session`, and `read_subagent_session`
 * because all three return a `messages: [...]` array of role/content (or
 * from/content) entries.
 *
 * Payload shapes:
 *   - read_messages:           { peer, message_count, showing,
 *                                messages: [{ from, content }], summary }
 *   - read_session (DM):       { session_id, context_id, message_count,
 *                                showing, messages: [{ role, content, from }],
 *                                summary, note }
 *   - read_session (regular):  { session_id, context_id, message_count,
 *                                showing, messages: [{ role, content }],
 *                                summary }
 *   - read_subagent_session:   { subagent, message_count, showing,
 *                                messages: [{ role, content }], summary }
 *     summary-only fast path:  { subagent, summary, has_summary, message_count }
 */
export function renderChatHistoryOutput(result, _params, opts) {
    const tool = opts && opts.tool;
    if (typeof result !== 'object' || result === null) return null;
    if (isErrorShape(result)) return null;

    const messages = Array.isArray(result.messages) ? result.messages : null;
    const summary = typeof result.summary === 'string' && result.summary.length > 0
        ? result.summary : null;
    const peer = typeof result.peer === 'string' ? result.peer : null;
    const subagent = typeof result.subagent === 'string' ? result.subagent : null;
    const sessionId = typeof result.session_id === 'string'
        ? result.session_id : null;
    const note = typeof result.note === 'string' && result.note.length > 0
        ? result.note : null;

    // read_subagent_session's two output shapes are asymmetric:
    //   - summary-only:    { subagent, summary, has_summary, message_count }
    //   - fallback (no summary): { subagent, summary: null, fallback_messages,
    //                              fallback_message_count, fallback_showing, note }
    // Prefer fallback_* counts when present so the operator sees how many
    // messages were folded into the fallback. The asymmetric naming lives
    // in alms-tools/src/read_subagent_session.rs:144-152.
    const total = typeof result.message_count === 'number'
        ? result.message_count
        : (typeof result.fallback_message_count === 'number'
            ? result.fallback_message_count
            : null);
    const showing = typeof result.showing === 'number'
        ? result.showing
        : (typeof result.fallback_showing === 'number'
            ? result.fallback_showing
            : (messages ? messages.length : null));

    // read_subagent_session / read_session summary-only branch — no
    // `messages` array. Surface session_id in the footer where present
    // (read_session's summary_only path includes it; #952 review).
    if (!messages && summary) {
        const summaryFooterBits = [];
        if (total != null) summaryFooterBits.push(`${total} messages total`);
        if (sessionId) summaryFooterBits.push(`session: ${sessionId.slice(0, 8)}…`);

        return html`
            <div class="tc-detail-section">
                <div class="tc-detail-label">
                    ${subagent ? `Subagent ${subagent}` : 'Summary'}
                </div>
                <pre class="tc-detail-content">${summary}</pre>
                ${summaryFooterBits.length > 0 && html`
                    <div class="tc-detail-footer">${summaryFooterBits.join(' · ')}</div>
                `}
            </div>
        `;
    }

    // fallback_messages path (read_subagent_session when no summary
    // available — runtime falls back to the last-N messages).
    const fallbackMessages = Array.isArray(result.fallback_messages)
        ? result.fallback_messages : null;
    const effectiveMessages = messages || fallbackMessages;
    if (!effectiveMessages) return null;

    const headerLabel = peer
        ? `Conversation with ${peer}`
        : (subagent
            ? `Subagent ${subagent}`
            : (tool === 'read_session' ? 'Session messages' : 'Messages'));

    const footerBits = [];
    if (showing != null && total != null && showing < total) {
        footerBits.push(`showing ${showing} of ${total}`);
    } else if (total != null) {
        footerBits.push(`${total} messages`);
    }
    if (sessionId) footerBits.push(`session: ${sessionId.slice(0, 8)}…`);

    return html`
        <div class="tc-detail-section">
            <div class="tc-detail-label">${headerLabel}</div>
            <ul class="tc-chat-list">
                ${effectiveMessages.map(m => {
                    const sender = (typeof m.from === 'string' && m.from)
                        || (typeof m.role === 'string' && m.role)
                        || '?';
                    const content = typeof m.content === 'string' ? m.content : '';
                    const cls = sender === 'you' || sender === 'user'
                        ? 'tc-chat-self' : 'tc-chat-peer';
                    return html`
                        <li class="tc-chat-row ${cls}">
                            <span class="tc-chat-sender">${sender}</span>
                            <pre class="tc-chat-content">${content}</pre>
                        </li>
                    `;
                })}
            </ul>
            ${note && html`
                <div class="tc-detail-footer">${note}</div>
            `}
            ${footerBits.length > 0 && html`
                <div class="tc-detail-footer">${footerBits.join(' · ')}</div>
            `}
        </div>
        ${summary && html`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Summary</div>
                <pre class="tc-detail-content">${summary}</pre>
            </div>
        `}
    `;
}

// ── list_agents ──────────────────────────────────────────────────────────

/**
 * Render list_agents output: tabular list of name / description / last_active.
 *
 * Payload shape (from `crates/alms-tools/src/list_agents.rs`):
 *   { agents: [{ name, description, last_active }], count }
 */
export function renderListAgentsOutput(result) {
    if (typeof result !== 'object' || result === null) return null;
    if (isErrorShape(result)) return null;
    if (!Array.isArray(result.agents)) return null;

    const agents = result.agents;
    if (agents.length === 0) {
        return html`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Agents</div>
                <div class="tc-detail-footer">No other agents available.</div>
            </div>
        `;
    }

    return html`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Agents</div>
            <ul class="tc-record-list">
                ${agents.map(a => html`
                    <li class="tc-record-row">
                        <div class="tc-record-head">
                            <span class="tc-kv-badge">${a.name || '?'}</span>
                            ${a.last_active && html`
                                <span class="tc-kv-meta">${a.last_active}</span>
                            `}
                        </div>
                        ${a.description && html`
                            <div class="tc-record-body">${a.description}</div>
                        `}
                    </li>
                `)}
            </ul>
            <div class="tc-detail-footer">
                ${agents.length} ${agents.length === 1 ? 'agent' : 'agents'}
            </div>
        </div>
    `;
}

// ── list_my_sessions ─────────────────────────────────────────────────────

/**
 * Render list_my_sessions output: tabular list of session entries.
 *
 * Payload shape (from `crates/alms-tools/src/list_my_sessions.rs`):
 *   { sessions: [{ session_id, context_type, context_id, source_label,
 *                  message_count, last_activity, summary }],
 *     showing, total }
 */
export function renderListMySessionsOutput(result) {
    if (typeof result !== 'object' || result === null) return null;
    if (isErrorShape(result)) return null;
    if (!Array.isArray(result.sessions)) return null;

    const sessions = result.sessions;
    const total = typeof result.total === 'number' ? result.total : sessions.length;
    const showing = typeof result.showing === 'number' ? result.showing : sessions.length;

    if (sessions.length === 0) {
        return html`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Sessions</div>
                <div class="tc-detail-footer">No sessions found.</div>
            </div>
        `;
    }

    const footerBits = [];
    if (showing < total) footerBits.push(`showing ${showing} of ${total}`);
    else footerBits.push(`${total} sessions`);

    return html`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Sessions</div>
            <ul class="tc-record-list">
                ${sessions.map(s => {
                    const sid = typeof s.session_id === 'string'
                        ? s.session_id.slice(0, 8) + '…' : '?';
                    return html`
                        <li class="tc-record-row">
                            <div class="tc-record-head">
                                ${s.context_type && html`
                                    <span class="tc-kv-badge">${s.context_type}</span>
                                `}
                                <span class="tc-kv-mono">${sid}</span>
                                ${typeof s.message_count === 'number' && html`
                                    <span class="tc-kv-meta">
                                        ${s.message_count} msg${s.message_count === 1 ? '' : 's'}
                                    </span>
                                `}
                                ${s.last_activity && html`
                                    <span class="tc-kv-meta">${s.last_activity}</span>
                                `}
                            </div>
                            ${s.source_label && html`
                                <div class="tc-record-body">${s.source_label}</div>
                            `}
                            ${s.summary && html`
                                <div class="tc-record-body tc-record-summary">
                                    ${s.summary}
                                </div>
                            `}
                        </li>
                    `;
                })}
            </ul>
            <div class="tc-detail-footer">${footerBits.join(' · ')}</div>
        </div>
    `;
}

// ── echo ─────────────────────────────────────────────────────────────────

/**
 * Render echo tool output. The runtime returns either:
 *   - the literal `message` string when present (so `result` is a string), or
 *   - the input params object verbatim when no `message` field was passed.
 *
 * Both shapes get a single monospace pane so the operator sees what was
 * echoed without scanning a JSON blob.
 */
export function renderEchoOutput(result) {
    if (isErrorShape(result)) return null;
    let text;
    if (typeof result === 'string') {
        text = result;
    } else if (result && typeof result === 'object') {
        // Object passthrough — pretty-print so it's still legible without
        // the raw-JSON toggle.
        try {
            text = JSON.stringify(result, null, 2);
        } catch {
            return null;
        }
    } else if (typeof result === 'number' || typeof result === 'boolean') {
        text = String(result);
    } else {
        return null;
    }
    return html`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Echoed</div>
            <pre class="tc-detail-content">${text}</pre>
        </div>
    `;
}

// ── math ─────────────────────────────────────────────────────────────────

/**
 * Render math tool output. The runtime returns a bare JSON number
 * (Value::from(i64) or Value::from(f64)) — render it as a single
 * monospace pill so the operator sees `42` rather than `42` floating
 * inside `tc-detail-content`'s pre block.
 */
export function renderMathOutput(result) {
    if (isErrorShape(result)) return null;
    if (typeof result !== 'number') return null;
    return html`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Result</div>
            <div class="tc-status-row">
                <span class="tc-kv-badge">${result}</span>
            </div>
        </div>
    `;
}

// ── datetime ─────────────────────────────────────────────────────────────

/**
 * Render datetime tool output: structured rows for UTC + local time. The
 * payload shape (from `crates/alms-sandbox/src/builtin/datetime.rs`):
 *   { iso, human, timezone,
 *     local_iso, local_human, local_timezone, utc_offset }
 */
export function renderDatetimeOutput(result) {
    if (typeof result !== 'object' || result === null) return null;
    if (isErrorShape(result)) return null;

    const iso = typeof result.iso === 'string' ? result.iso : null;
    const human = typeof result.human === 'string' ? result.human : null;
    const tz = typeof result.timezone === 'string' ? result.timezone : null;
    const localIso = typeof result.local_iso === 'string' ? result.local_iso : null;
    const localHuman = typeof result.local_human === 'string' ? result.local_human : null;
    const localTz = typeof result.local_timezone === 'string' ? result.local_timezone : null;
    const offset = typeof result.utc_offset === 'string' ? result.utc_offset : null;

    if (!iso && !localIso) return null;

    return html`
        ${(iso || human) && html`
            <div class="tc-detail-section">
                <div class="tc-detail-label">UTC</div>
                <div class="tc-status-row">
                    ${tz && html`<span class="tc-kv-badge">${tz}</span>`}
                    ${human && html`<span class="tc-kv-meta">${human}</span>`}
                </div>
                ${iso && html`<pre class="tc-detail-content">${iso}</pre>`}
            </div>
        `}
        ${(localIso || localHuman) && html`
            <div class="tc-detail-section">
                <div class="tc-detail-label">Local</div>
                <div class="tc-status-row">
                    ${localTz && html`<span class="tc-kv-badge">${localTz}</span>`}
                    ${offset && html`<span class="tc-kv-meta">${offset}</span>`}
                    ${localHuman && html`<span class="tc-kv-meta">${localHuman}</span>`}
                </div>
                ${localIso && html`<pre class="tc-detail-content">${localIso}</pre>`}
            </div>
        `}
    `;
}

// ── ignore_message ───────────────────────────────────────────────────────

/**
 * Render ignore_message output: a one-liner with the reason.
 *
 * Payload shape (from `crates/alms-tools/src/ignore_message.rs`):
 *   { ignored: true, reason }
 */
export function renderIgnoreMessageOutput(result) {
    if (typeof result !== 'object' || result === null) return null;
    if (isErrorShape(result)) return null;
    if (result.ignored !== true) return null;

    const reason = typeof result.reason === 'string' ? result.reason : '';
    return html`
        <div class="tc-detail-section">
            <div class="tc-detail-label">Ignored</div>
            <div class="tc-status-row">
                <span class="tc-kv-badge">ignored</span>
                ${reason && html`<span class="tc-kv-meta">${reason}</span>`}
            </div>
        </div>
    `;
}

// ── Dispatcher ───────────────────────────────────────────────────────────

/**
 * Dispatch by tool name to the appropriate renderer. Returns `null` when
 * the tool has no bespoke renderer or when the renderer rejects the
 * payload shape — in that case tool-row.js falls through to the raw-JSON
 * renderer that has always been there.
 */
export function renderToolOutput(tool, result, params, opts) {
    if (result == null) return null;

    switch (tool) {
        case 'shell':
        case 'shell_exec':
            return renderShellOutput(result);
        case 'fs_read':
            return renderFsReadOutput(result, params);
        case 'fs_write':
        case 'workspace_write':
        case 'fs_edit':
            return renderFsWriteEditOutput(result);
        case 'fs_grep':
            return renderFsGrepOutput(result);
        case 'fs_glob':
            return renderFsGlobOutput(result);
        case 'fs_list':
            return renderFsListOutput(result);
        case 'http_get':
            return renderHttpGetOutput(result, params, opts);
        case 'invoke_agent':
            return renderInvokeAgentOutput(result, params, opts);
        case 'send_message':
            return renderSendMessageOutput(result);
        case 'read_messages':
        case 'read_session':
        case 'read_subagent_session':
            return renderChatHistoryOutput(result, params, { ...opts, tool });
        case 'list_agents':
            return renderListAgentsOutput(result);
        case 'list_my_sessions':
            return renderListMySessionsOutput(result);
        case 'ignore_message':
            return renderIgnoreMessageOutput(result);
        case 'echo':
            return renderEchoOutput(result);
        case 'math':
            return renderMathOutput(result);
        case 'datetime':
            return renderDatetimeOutput(result);
        default:
            return null;
    }
}

