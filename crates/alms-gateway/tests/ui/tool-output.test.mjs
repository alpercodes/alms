// JS-level tests for `static/ui/utils/tool-output.js` — the per-tool
// output-side renderers that ship in #873.
//
// Reusability note
// ----------------
// The `loadToolOutputModule()` + `__scanFull` stub below is general-
// purpose: it works for any Node-side test of an htm-importing module
// in `static/ui/`. The pattern is also used by `history.test.mjs`
// (#898). If you reach for it from a third place, consider extracting
// it to a shared `tests/ui/_harness.mjs` rather than copy-pasting
// (see #952 review nit 11). For now duplication is small enough to
// keep inline.
//
// Strategy
// --------
// `tool-output.js` imports `html` (htm + Preact `h`) from `../deps.js`.
// htm is a CDN ES module not vendored into the repo, so we can't `import`
// it under Node. Instead we stub `html` with a deterministic tagged-
// template helper that turns the template strings + interpolated values
// into a plain "VNode-like" tree we can walk in assertions:
//
//   { kind: "elem", tag, classes: [...], children: [...] }
//   { kind: "text", text }
//
// The shape is *not* a faithful reproduction of Preact's VNode — it
// elides attribute parsing beyond `class=` because that's all our
// assertions care about (we check that "shell stdout went into a
// .tc-code-block, exit_code went into a .tc-kv-badge", etc.).
//
// What we test
// ------------
// Each test feeds a representative `tool_end` payload to the dispatcher
// `renderToolOutput(tool, result, params)` and asserts:
//   1. The structured renderer returned a non-null tree (i.e. it didn't
//      fall through to the raw-JSON path).
//   2. Specific class names from styles/tool-calls.css appear (proving
//      the structured layout was used).
//   3. Specific text fragments from the payload appear in the rendered
//      tree (proving the renderer plumbed through the right fields).
//   4. The output does NOT look like the raw-JSON fallback (no big
//      `JSON.stringify` blob).
//
// The tests are deliberately thicker on the loud-asymmetry tools called
// out in the issue (shell, fs_read, fs_grep, invoke_agent) and lighter
// on the rest. Lighter coverage is fine per the issue.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import url from 'node:url';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const TOOL_OUTPUT_PATH = path.resolve(
    __dirname,
    '../../static/ui/utils/tool-output.js'
);

/**
 * Load `tool-output.js` under Node by stubbing its single top-level
 * import (`html` from `../deps.js`) with a tagged-template helper that
 * produces the synthetic tree described in the file header.
 */
async function loadToolOutputModule() {
    const src = fs.readFileSync(TOOL_OUTPUT_PATH, 'utf8');

    const importRe =
        /^import\s+\{\s*html\s*\}\s+from\s+['"]\.\.\/deps\.js['"];?\s*$/m;
    if (!importRe.test(src)) {
        throw new Error(
            'tool-output.js: expected a top-level `import { html } from "../deps.js"` line — '
            + 'test rewrite would not apply. Update tool-output.test.mjs if the import shape changed.'
        );
    }

    // Also stub the navigate-session import — added in #952 review fix
    // for the broken "View full session" link. The renderer wires the
    // function into an onClick handler we never invoke from test code,
    // so a no-op stub is sufficient. Failing soft (not throwing) keeps
    // the harness loose: if the import line is removed later, the stub
    // simply won't be applied and the test will surface the real
    // resolution error against the renderer source.
    const navImportRe =
        /^import\s+\{\s*navigateToSession\s*\}\s+from\s+['"]\.\/navigate-session\.js['"];?\s*$/m;

    // Stub `html` as a tagged template. We don't try to be a real parser:
    // we stitch the strings + interpolated values into a single flat
    // string with sentinel placeholders for each interpolation, then run
    // a single tag scanner over the joined string. Interpolated values
    // are stored in an indexed array; when the scanner emits text or
    // attribute content, sentinels expand back into the original values.
    //
    // This shape correctly handles:
    //   - interpolated class strings (e.g. `<li class="x ${cls}">`),
    //   - interpolated attribute values inside a single tag,
    //   - VNodes returned by nested html`...` calls (preserved as
    //     children when they appear inside element bodies),
    //   - arrays from .map(...) (flattened into children).
    //
    // What it does NOT handle (tests don't need it):
    //   - quoted attribute values that contain a literal '<' or '>',
    //   - HTML comments,
    //   - htm "component slot" syntax (the assertions in this file only
    //     ever rely on plain html elements; component slots in
    //     tool-row.js never reach this stub).
    const stub = `
        const __SENTINEL_OPEN = '\\u0001V';
        const __SENTINEL_CLOSE = '\\u0001';

        function __htmlStub(strings, ...values) {
            // Stitch the template into a single string with sentinels
            // marking interpolation points. Each sentinel is "\\u0001V<index>\\u0001"
            // — the leading 0x01 is a control byte that cannot appear in a
            // valid HTML source string, so it never collides with real text.
            let joined = strings[0];
            for (let i = 0; i < values.length; i++) {
                joined += __SENTINEL_OPEN + i + __SENTINEL_CLOSE + strings[i + 1];
            }
            return __scanFull(joined, values);
        }

        function __scanFull(s, values) {
            const stack = [{ kind: 'elem', tag: '#fragment', classes: [], children: [] }];
            const top = () => stack[stack.length - 1];
            let i = 0;
            while (i < s.length) {
                const lt = s.indexOf('<', i);
                if (lt === -1) {
                    __emitText(s.slice(i), values, top());
                    break;
                }
                if (lt > i) {
                    __emitText(s.slice(i, lt), values, top());
                }
                const gt = s.indexOf('>', lt + 1);
                if (gt === -1) break;
                const inside = s.slice(lt + 1, gt);
                if (inside.startsWith('/')) {
                    const tag = inside.slice(1).trim();
                    while (stack.length > 1 && stack[stack.length - 1].tag !== tag) {
                        stack.pop();
                    }
                    if (stack.length > 1) stack.pop();
                    i = gt + 1;
                    continue;
                }
                const selfClose = inside.endsWith('/');
                const head = (selfClose ? inside.slice(0, -1) : inside).trim();
                const spaceIdx = head.search(/\\s/);
                const tag = spaceIdx === -1 ? head : head.slice(0, spaceIdx);
                const attrs = spaceIdx === -1 ? '' : head.slice(spaceIdx + 1);
                // Resolve any sentinel inside the attribute string
                // (e.g. \`class="x \${cls}"\`) so the class regex sees the
                // expanded value.
                const expandedAttrs = __expandSentinels(attrs, values);
                const classMatch = expandedAttrs.match(/class=\\"([^\\"]*)\\"/);
                const classes = classMatch
                    ? classMatch[1].split(/\\s+/).filter(Boolean)
                    : [];
                const elem = { kind: 'elem', tag, classes, children: [] };
                stack[stack.length - 1].children.push(elem);
                if (!selfClose) stack.push(elem);
                i = gt + 1;
            }
            const root = stack[0];
            if (root.children.length === 1
                && root.children[0]
                && root.children[0].kind === 'elem') {
                return root.children[0];
            }
            return root;
        }

        // Replace sentinel "\\u0001V<index>\\u0001" with the corresponding
        // value coerced to string. Used for attribute strings only — text
        // content goes through __emitText which preserves VNode children
        // as separate child entries.
        function __expandSentinels(s, values) {
            return s.replace(/\\u0001V(\\d+)\\u0001/g, (_, idx) => {
                const v = values[Number(idx)];
                if (v === null || v === undefined || v === false || v === true) return '';
                if (v && typeof v === 'object') {
                    // A VNode landing in attribute position is unusual; just
                    // emit its tag name so class-matching doesn't break.
                    return v.tag || '';
                }
                return String(v);
            });
        }

        // Emit a chunk of text content into the given parent. Sentinels
        // inside the chunk are expanded one at a time: scalar values
        // become text nodes, VNodes become element children, arrays are
        // flattened, false/null/undefined are elided.
        function __emitText(chunk, values, parent) {
            if (!chunk) return;
            const re = /\\u0001V(\\d+)\\u0001/g;
            let last = 0;
            let m;
            while ((m = re.exec(chunk)) !== null) {
                const before = chunk.slice(last, m.index);
                const t = before.trim();
                if (t) parent.children.push({ kind: 'text', text: t });
                __pushValue(values[Number(m[1])], parent);
                last = re.lastIndex;
            }
            const tail = chunk.slice(last).trim();
            if (tail) parent.children.push({ kind: 'text', text: tail });
        }

        function __pushValue(v, parent) {
            if (v === null || v === undefined || v === false || v === true) return;
            if (Array.isArray(v)) {
                for (const item of v) __pushValue(item, parent);
                return;
            }
            if (v && typeof v === 'object' && v.kind) {
                parent.children.push(v);
                return;
            }
            const s = String(v);
            if (s.length > 0) parent.children.push({ kind: 'text', text: s });
        }
    `;

    let stubbed = src.replace(
        importRe,
        // Inline stub + re-bind `html` to it.
        stub + '\nconst html = __htmlStub;'
    );
    stubbed = stubbed.replace(
        navImportRe,
        // No-op stub — the test never invokes the link's onClick
        // handler, so a function that does nothing is sufficient.
        'const navigateToSession = () => {};'
    );

    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'alms-tool-output-test-'));
    const tmpFile = path.join(tmpDir, 'tool-output.mjs');
    fs.writeFileSync(tmpFile, stubbed, 'utf8');
    return await import(url.pathToFileURL(tmpFile).href);
}

const mod = await loadToolOutputModule();
const { renderToolOutput } = mod;

/**
 * Walk a tree produced by the stubbed `html` and collect every text
 * fragment as a single space-separated string. Useful for asserting that
 * a specific value from the payload made it into the rendered output
 * (regardless of which sub-element it landed in).
 */
function collectText(tree) {
    if (!tree) return '';
    if (tree.kind === 'text') return tree.text;
    if (tree.kind === 'elem') {
        return tree.children.map(collectText).filter(Boolean).join(' ');
    }
    return '';
}

/**
 * Walk a tree and collect every CSS class name encountered. Returns a
 * Set so tests can do `assert.ok(classes.has("tc-code-block"))`.
 */
function collectClasses(tree, into) {
    const set = into || new Set();
    if (!tree || tree.kind !== 'elem') return set;
    for (const c of tree.classes) set.add(c);
    for (const child of tree.children) collectClasses(child, set);
    return set;
}

/**
 * Pretty-print the tree for assertion-failure diagnostics. Only invoked
 * via `assert(..., dump(tree))` so it never runs on the green path.
 */
function dump(tree, indent = 0) {
    const pad = '  '.repeat(indent);
    if (!tree) return pad + '(null)';
    if (tree.kind === 'text') return pad + JSON.stringify(tree.text);
    if (tree.kind === 'elem') {
        const head = `${pad}<${tree.tag}`
            + (tree.classes.length ? ` class="${tree.classes.join(' ')}"` : '')
            + '>';
        const body = tree.children.map(c => dump(c, indent + 1)).join('\n');
        return body ? `${head}\n${body}` : head;
    }
    return pad + '(unknown)';
}

// =========================================================================
// shell / shell_exec — primary asymmetry target from #873
// =========================================================================

test('#873 shell: foreground result renders stdout/stderr in monospace, exit_code as a badge', () => {
    const result = {
        exit_code: 0,
        stdout: 'hello world\nline 2\n',
        stderr: 'warning: something\n',
    };
    const tree = renderToolOutput('shell', result, { command: 'echo hi' });
    assert.ok(tree, 'shell renderer must return a tree (not null)');

    const classes = collectClasses(tree);
    const text = collectText(tree);

    assert.ok(classes.has('tc-status-row'),
        'foreground shell must render a status row\n' + dump(tree));
    assert.ok(classes.has('tc-kv-badge'),
        'exit_code must be rendered as a kv-badge pill\n' + dump(tree));
    assert.ok(classes.has('tc-code-block'),
        'stdout/stderr must be rendered as code-block panes\n' + dump(tree));
    assert.ok(classes.has('tc-detail-warn'),
        'stderr pane must carry the warn variant class\n' + dump(tree));

    assert.ok(text.includes('hello world'),
        'stdout text must appear in the rendered output');
    assert.ok(text.includes('warning: something'),
        'stderr text must appear in the rendered output');
    assert.ok(text.includes('exit 0'),
        'exit code text must appear in the badge');

    // Negative: must NOT look like the raw JSON fallback.
    assert.ok(!text.includes('"stdout":'),
        'rendered output must not contain a raw JSON dump of stdout\n' + dump(tree));
    assert.ok(!text.includes('"exit_code":'),
        'rendered output must not contain a raw JSON dump of exit_code\n' + dump(tree));
});

test('#873 shell: non-zero exit code renders the badge with the fail variant', () => {
    const result = { exit_code: 127, stdout: '', stderr: 'command not found\n' };
    const tree = renderToolOutput('shell', result, { command: 'nope' });
    assert.ok(tree);
    const classes = collectClasses(tree);
    assert.ok(classes.has('tc-kv-badge-fail'),
        'non-zero exit must add tc-kv-badge-fail variant\n' + dump(tree));
    assert.ok(collectText(tree).includes('exit 127'));
});

test('#873 shell: background-task submission renders status pill + task_id, no stdout pane', () => {
    const result = {
        task_id: 'bg_1',
        status: 'submitted',
        message: 'Command submitted for background execution.',
    };
    const tree = renderToolOutput('shell', result, { command: 'sleep 30' });
    assert.ok(tree);
    const text = collectText(tree);
    const classes = collectClasses(tree);
    assert.ok(text.includes('submitted'));
    assert.ok(text.includes('bg_1'));
    // No stdout/stderr labels should appear because there isn't any output yet.
    assert.ok(!classes.has('tc-detail-warn'),
        'no stderr pane on a background submit\n' + dump(tree));
});

test('#873 shell_exec: alias is wired to the same renderer', () => {
    const result = { exit_code: 0, stdout: 'ok', stderr: '' };
    const tree = renderToolOutput('shell_exec', result, { command: 'true' });
    assert.ok(tree, 'shell_exec must use the shell renderer');
    assert.ok(collectClasses(tree).has('tc-code-block'));
});

// =========================================================================
// fs_read — primary asymmetry target from #873
// =========================================================================

test('#873 fs_read: file content rendered with path header + monospace pane', () => {
    const content = '     1\thello\n     2\tworld\n';
    const result = {
        content,
        lines_returned: 2,
        total_lines: 2,
        has_more_before: false,
        has_more_after: false,
    };
    const tree = renderToolOutput('fs_read', result, { path: 'src/lib.rs' });
    assert.ok(tree, 'fs_read renderer must return a tree');

    const classes = collectClasses(tree);
    const text = collectText(tree);

    assert.ok(classes.has('tc-file-header'),
        'fs_read must render a path header element\n' + dump(tree));
    assert.ok(classes.has('tc-code-block'),
        'fs_read content must be in a code-block pane\n' + dump(tree));
    assert.ok(text.includes('hello'),
        'file content text must appear in the rendered output');
    assert.ok(text.includes('src/lib.rs') || text.includes('lib.rs'),
        'path must appear in the header');
    assert.ok(text.includes('2 lines (full file)'),
        'footer must show the full-file line count\n' + dump(tree));

    // Negative: not raw-JSON.
    assert.ok(!text.includes('"content":'),
        'rendered output must not contain a raw JSON dump\n' + dump(tree));
});

test('#873 fs_read: partial read shows truncation flags in the footer', () => {
    const result = {
        content: '     1\tline1\n',
        lines_returned: 1,
        has_more_before: false,
        has_more_after: true,
        offset: 0,
        limit: 1,
        byte_budget_exceeded: true,
        note: 'Output truncated at N bytes',
    };
    const tree = renderToolOutput('fs_read', result, { path: 'big.txt' });
    assert.ok(tree);
    const text = collectText(tree);
    assert.ok(text.includes('more after'),
        'footer must surface has_more_after\n' + dump(tree));
    assert.ok(text.includes('byte-budget exceeded'),
        'footer must surface byte_budget_exceeded\n' + dump(tree));
    assert.ok(text.includes('Output truncated'),
        'note field must surface in the rendered output');
});

test('#873 fs_read: empty file is handled gracefully', () => {
    const result = {
        content: '',
        lines_returned: 0,
        has_more_before: false,
        has_more_after: false,
        note: 'File exists but is empty.',
    };
    const tree = renderToolOutput('fs_read', result, { path: 'empty.txt' });
    assert.ok(tree);
    const text = collectText(tree);
    assert.ok(text.includes('File exists but is empty.'),
        'empty-file note must appear');
    // Must not blow up with a code-block pane wrapping an empty string —
    // the renderer should switch to a muted pane.
    const classes = collectClasses(tree);
    assert.ok(classes.has('tc-detail-muted'),
        'empty content must use the muted variant\n' + dump(tree));
});

// =========================================================================
// fs_grep — primary asymmetry target from #873
// =========================================================================

test('#873 fs_grep content mode: each match rendered as path:line + snippet', () => {
    const result = {
        matches: [
            {
                file: 'src/foo.rs',
                line: 42,
                content: '    let x = 1;',
                context_before: [],
                context_after: [],
            },
            {
                file: 'src/bar.rs',
                line: 7,
                content: '    fn frobnicate() {',
                context_before: [],
                context_after: [],
            },
        ],
        total: 2,
        truncated: false,
        truncated_lines: 0,
    };
    const tree = renderToolOutput('fs_grep', result, {});
    assert.ok(tree);
    const classes = collectClasses(tree);
    const text = collectText(tree);

    assert.ok(classes.has('tc-match-list'),
        'matches must render as a structured list\n' + dump(tree));
    assert.ok(classes.has('tc-match-content'),
        'content-mode matches must use the content-mode row variant\n' + dump(tree));
    assert.ok(classes.has('tc-match-snippet'),
        'each row must include a snippet element');

    assert.ok(text.includes('src/foo.rs'));
    assert.ok(text.includes('42'));
    assert.ok(text.includes('let x = 1;'));
    assert.ok(text.includes('src/bar.rs'));
    assert.ok(text.includes('frobnicate'));
    assert.ok(text.includes('2 matches'),
        'footer must show match count\n' + dump(tree));

    // Negative.
    assert.ok(!text.includes('"matches":'),
        'must not contain a raw JSON dump of matches\n' + dump(tree));
    assert.ok(!text.includes('"context_before"'),
        'must not leak the context_before key into the output');
});

test('#873 fs_grep files mode: each path rendered as a list row', () => {
    const result = {
        matches: ['src/foo.rs', 'src/bar.rs'],
        total: 2,
        truncated: false,
        truncated_lines: 0,
    };
    const tree = renderToolOutput('fs_grep', result, {});
    assert.ok(tree);
    const classes = collectClasses(tree);
    assert.ok(classes.has('tc-match-files'),
        'files-mode matches must use the files-mode row variant\n' + dump(tree));
    const text = collectText(tree);
    assert.ok(text.includes('src/foo.rs'));
    assert.ok(text.includes('src/bar.rs'));
});

test('#873 fs_grep count mode: each entry rendered as path + count', () => {
    const result = {
        matches: [
            { file: 'src/foo.rs', count: 5 },
            { file: 'src/bar.rs', count: 2 },
        ],
        total_matches: 7,
        truncated: false,
        truncated_lines: 0,
    };
    const tree = renderToolOutput('fs_grep', result, {});
    assert.ok(tree);
    const classes = collectClasses(tree);
    assert.ok(classes.has('tc-match-count'),
        'count-mode matches must use the count-mode row variant\n' + dump(tree));
    const text = collectText(tree);
    assert.ok(text.includes('src/foo.rs'));
    assert.ok(text.includes('5'));
    assert.ok(text.includes('7 matches'),
        'count-mode footer must use total_matches\n' + dump(tree));
});

test('#873 fs_grep: empty match set renders the no-matches state', () => {
    const result = { matches: [], total: 0, truncated: false, truncated_lines: 0 };
    const tree = renderToolOutput('fs_grep', result, {});
    assert.ok(tree);
    const text = collectText(tree);
    assert.ok(text.includes('No matches found'));
});

// =========================================================================
// invoke_agent — primary asymmetry target from #873
// =========================================================================

test('#873 invoke_agent foreground: renders subagent name, response preview, session link', () => {
    const result = {
        response: 'I did the thing successfully and here is what I learned: ...',
        session_id: '11111111-2222-3333-4444-555555555555',
    };
    const tree = renderToolOutput('invoke_agent', result, { name: 'larry' });
    assert.ok(tree);
    const classes = collectClasses(tree);
    const text = collectText(tree);

    assert.ok(classes.has('tc-detail-link'),
        'invoke_agent must render a session link\n' + dump(tree));
    assert.ok(classes.has('tc-kv-badge'),
        'subagent name must be rendered as a kv-badge\n' + dump(tree));
    assert.ok(text.includes('larry'),
        'subagent name must appear in the output');
    assert.ok(text.includes('I did the thing'),
        'response text must appear in the preview');
    assert.ok(text.includes('View full session'),
        'link text must read "View full session"\n' + dump(tree));

    // Negative: must not include the raw JSON form.
    assert.ok(!text.includes('"response":'),
        'must not include a raw JSON dump\n' + dump(tree));
});

test('#873 invoke_agent background: renders task_id and session link, no response pane', () => {
    const result = {
        task_id: 'sub_42',
        session_id: '11111111-2222-3333-4444-555555555555',
    };
    const tree = renderToolOutput('invoke_agent', result, { name: 'larry' });
    assert.ok(tree);
    const text = collectText(tree);
    assert.ok(text.includes('sub_42'),
        'task_id must appear in the rendered output');
    assert.ok(text.includes('background'),
        'background label must appear (header)');
    // The foreground "completed" label should NOT be in the background path.
    assert.ok(!text.includes('completed'),
        'background path must not show the "completed" status\n' + dump(tree));
});

// =========================================================================
// Lighter-coverage tests for the other tools the issue calls out
// =========================================================================

test('#873 fs_write: renders ok badge + path', () => {
    const result = { ok: true, path: 'src/new.rs' };
    const tree = renderToolOutput('fs_write', result, {});
    assert.ok(tree);
    const text = collectText(tree);
    assert.ok(text.includes('ok'));
    assert.ok(text.includes('src/new.rs'));
});

test('#873 fs_edit: renders ok badge + path + replacement count', () => {
    const result = { ok: true, path: 'src/x.rs', replacements: 3 };
    const tree = renderToolOutput('fs_edit', result, {});
    assert.ok(tree);
    const text = collectText(tree);
    assert.ok(text.includes('ok'));
    assert.ok(text.includes('src/x.rs'));
    assert.ok(text.includes('3 replacements'),
        'replacement count must be rendered\n' + dump(tree));
});

// Regression for #952 review item 2 — `workspace_write` returns
// `{ ok, file, mode }` (no `path`), while fs_write/fs_edit return
// `{ ok, path, ... }`. Before the fix, the dispatcher routed to
// `renderFsWriteEditOutput` which only read `result.path`, so the
// guard `if (!path) return null` fired and the structured renderer
// silently fell through to the raw-JSON fallback. Pin both shapes
// here so the renderer has to keep accepting both keys.
test('#873/#952 workspace_write: renders ok badge + file + mode badge', () => {
    const result = { ok: true, file: 'memories', mode: 'append' };
    const tree = renderToolOutput('workspace_write', result, {});
    assert.ok(tree, 'workspace_write must render structured (not raw fallback)');
    const text = collectText(tree);
    const classes = collectClasses(tree);

    assert.ok(classes.has('tc-kv-badge'),
        'ok status must be rendered as a kv-badge\n' + dump(tree));
    assert.ok(classes.has('tc-kv-mono'),
        'file name must be rendered as kv-mono\n' + dump(tree));
    assert.ok(text.includes('ok'),
        'ok badge text must appear');
    assert.ok(text.includes('memories'),
        'file value must appear in the rendered output\n' + dump(tree));
    assert.ok(text.includes('append'),
        'mode value must appear as a meta pill\n' + dump(tree));

    // Negative: must not look like the raw-JSON fallback.
    assert.ok(!text.includes('"file":'),
        'must not contain a raw JSON dump of the payload\n' + dump(tree));
});

test('#873/#952 workspace_write: write mode also renders cleanly', () => {
    const result = { ok: true, file: 'goals', mode: 'write' };
    const tree = renderToolOutput('workspace_write', result, {});
    assert.ok(tree);
    const text = collectText(tree);
    assert.ok(text.includes('goals'));
    assert.ok(text.includes('write'));
});

test('#873 fs_glob: renders file list + total/truncated footer', () => {
    const result = {
        files: ['a.txt', 'b.txt', 'c.txt'],
        total: 5,
        truncated: true,
    };
    const tree = renderToolOutput('fs_glob', result, {});
    assert.ok(tree);
    const text = collectText(tree);
    assert.ok(text.includes('a.txt'));
    assert.ok(text.includes('3 of 5 files'));
    assert.ok(text.includes('truncated'));
});

test('#873 http_get: JSON body pretty-printed with content-type label', () => {
    const result = {
        status: 200,
        content_type: 'application/json',
        body: { hello: 'world', n: 42 },
    };
    const tree = renderToolOutput('http_get', result, { url: 'https://x/y' });
    assert.ok(tree);
    const text = collectText(tree);
    const classes = collectClasses(tree);
    assert.ok(classes.has('tc-code-block'),
        'body pane must use code-block\n' + dump(tree));
    assert.ok(text.includes('200'));
    assert.ok(text.includes('application/json'));
    assert.ok(text.includes('hello'));
    assert.ok(text.includes('world'));
    // Note: the stub joins sibling text nodes with a single space, so the
    // rendered "Body" + " (JSON)" sequence appears as "Body  (JSON)" after
    // joining. We collapse whitespace before substring-matching.
    const normalized = text.replace(/\s+/g, ' ');
    assert.ok(normalized.includes('Body (JSON)'),
        'JSON content-type must annotate the body label\n' + dump(tree));
});

test('#873 http_get: non-2xx status renders the fail badge', () => {
    const result = { status: 503, content_type: 'text/plain', body: 'Service unavailable' };
    const tree = renderToolOutput('http_get', result, { url: 'https://x/y' });
    assert.ok(tree);
    const classes = collectClasses(tree);
    assert.ok(classes.has('tc-kv-badge-fail'),
        'non-2xx status must use the fail-badge variant\n' + dump(tree));
});

test('#873 list_agents: renders one record per agent with name + description', () => {
    const result = {
        agents: [
            { name: 'iris', description: 'Frontend', last_active: '2026-05-02T10:00Z' },
            { name: 'larry', description: 'Bug fix', last_active: '2026-05-01T12:00Z' },
        ],
        count: 2,
    };
    const tree = renderToolOutput('list_agents', result, {});
    assert.ok(tree);
    const classes = collectClasses(tree);
    const text = collectText(tree);
    assert.ok(classes.has('tc-record-list'),
        'list_agents must render a record-list\n' + dump(tree));
    assert.ok(text.includes('iris'));
    assert.ok(text.includes('larry'));
    assert.ok(text.includes('Frontend'));
});

test('#873 read_messages: chat-style rendering with sender/content rows', () => {
    const result = {
        peer: 'larry',
        message_count: 2,
        showing: 2,
        messages: [
            { from: 'you', content: 'hi larry' },
            { from: 'larry', content: 'hi iris' },
        ],
        summary: '',
    };
    const tree = renderToolOutput('read_messages', result, {});
    assert.ok(tree);
    const classes = collectClasses(tree);
    const text = collectText(tree);
    assert.ok(classes.has('tc-chat-list'),
        'read_messages must render a chat-list\n' + dump(tree));
    assert.ok(classes.has('tc-chat-self'),
        '"you" row must use the self variant\n' + dump(tree));
    assert.ok(classes.has('tc-chat-peer'),
        'peer row must use the peer variant\n' + dump(tree));
    assert.ok(text.includes('hi larry'));
    assert.ok(text.includes('hi iris'));
    assert.ok(text.includes('Conversation with larry'));
});

// Regression for #952 review item 3 — read_subagent_session's fallback
// path uses `fallback_message_count` / `fallback_showing` instead of
// the canonical `message_count` / `showing` keys, plus a `note` string
// the operator needs to see. Pin all three so they reach the rendered
// output.
test('#873/#952 read_subagent_session fallback: surfaces fallback counts + note', () => {
    const result = {
        subagent: 'larry',
        summary: null,
        has_summary: false,
        fallback_messages: [
            { role: 'user', content: 'fix the thing' },
            { role: 'assistant', content: 'fixed it' },
        ],
        fallback_message_count: 12,
        fallback_showing: 2,
        note: 'No summary available. Showing the last messages as a fallback.',
    };
    const tree = renderToolOutput('read_subagent_session', result, {});
    assert.ok(tree);
    const text = collectText(tree);
    assert.ok(text.includes('fix the thing'),
        'fallback message content must appear');
    assert.ok(text.includes('showing 2 of 12'),
        'fallback counts must reach the footer\n' + dump(tree));
    assert.ok(text.includes('No summary available'),
        'note string must reach the rendered output\n' + dump(tree));
});

// Regression for #952 review item 4 — read_session's summary-only path
// returns `session_id` but the previous renderer dropped it on the
// no-messages branch. Pin it here.
test('#873/#952 read_session summary-only: surfaces session_id in footer', () => {
    const result = {
        session_id: '11111111-2222-3333-4444-555555555555',
        context_id: 'ctx-x',
        summary: 'Earlier work on issue #99 was completed.',
        has_summary: true,
    };
    const tree = renderToolOutput('read_session', result, {});
    assert.ok(tree);
    const text = collectText(tree);
    assert.ok(text.includes('Earlier work on issue'),
        'summary text must reach the rendered output');
    assert.ok(text.includes('11111111') || text.includes('session:'),
        'session_id must surface in the footer\n' + dump(tree));
});

// Regression for #952 review item 5 — fs_grep content mode silently
// dropped `context_before` / `context_after` when present. Pin them
// here so the operator gets the context they paid the round-trip for.
test('#873/#952 fs_grep content mode: renders context_before / context_after', () => {
    const result = {
        matches: [
            {
                file: 'src/foo.rs',
                line: 42,
                content: '    let x = MATCH_LINE;',
                context_before: ['line above 1', 'line above 2'],
                context_after: ['line below 1'],
            },
        ],
        total: 1,
        truncated: false,
        truncated_lines: 0,
    };
    const tree = renderToolOutput('fs_grep', result, {});
    assert.ok(tree);
    const classes = collectClasses(tree);
    const text = collectText(tree);
    assert.ok(classes.has('tc-match-context'),
        'context lines must use the context variant for dimmed styling\n' + dump(tree));
    assert.ok(text.includes('line above 1'),
        'context_before lines must appear in the rendered output');
    assert.ok(text.includes('line above 2'));
    assert.ok(text.includes('line below 1'),
        'context_after lines must appear in the rendered output');
    assert.ok(text.includes('let x = MATCH_LINE'),
        'main match line must still render');

    // Negative: keys must not leak into the rendered output as JSON.
    assert.ok(!text.includes('"context_before"'),
        'must not leak the context_before key\n' + dump(tree));
});

// =========================================================================
// Dispatcher sanity: unknown tools fall through to null (raw fallback path)
// =========================================================================

test('#873 dispatcher: unknown tool returns null so tool-row falls through to raw', () => {
    const result = { whatever: 1 };
    const tree = renderToolOutput('not_a_real_tool', result, {});
    assert.equal(tree, null,
        'unknown tools must return null so the existing raw renderer takes over');
});

test('#873 dispatcher: null result returns null', () => {
    const tree = renderToolOutput('shell', null, {});
    assert.equal(tree, null);
});

test('#873 dispatcher: tools that return error shapes fall through to raw', () => {
    // The runtime writes `{ error: "..." }` when the tool catches a
    // recoverable error and returns it as a successful Value rather than
    // failing the tool. Per-tool renderers should refuse to dress those
    // up — they're better-served by the raw error pane in tool-row.js.
    const result = { error: 'agent not found' };
    const tree = renderToolOutput('send_message', result, { to: 'x' });
    assert.equal(tree, null,
        'send_message error shape must fall through to the raw fallback');
});

// =========================================================================
// tool-row.js wiring — the regression that bit us in #969 was that
// `utils/tool-output.js` got dropped on the develop branch during a
// release-merge skew. The dispatcher tests above all passed (they only
// load the dispatcher), but in production every tool rendered as raw
// JSON because tool-row.js was never importing or invoking it.
//
// These tests pin the wiring contract at the source-text level so any
// future refactor that drops the import or stops calling the dispatcher
// gets caught at test time rather than in Alper's browser.
// =========================================================================

const TOOL_ROW_PATH = path.resolve(
    __dirname,
    '../../static/ui/components/chat/tool-row.js'
);

test('#969 wiring: tool-row.js imports the dispatcher from utils/tool-output.js', () => {
    const src = fs.readFileSync(TOOL_ROW_PATH, 'utf8');
    // Match either single- or double-quoted import paths and any
    // amount of whitespace, but require the symbol `renderToolOutput`
    // and the path `utils/tool-output.js` (relative to the chat
    // component dir).
    const importRe =
        /import\s*\{[^}]*\brenderToolOutput\b[^}]*\}\s*from\s*['"]\.\.\/\.\.\/utils\/tool-output\.js['"]/;
    assert.ok(importRe.test(src),
        'tool-row.js must import { renderToolOutput } from "../../utils/tool-output.js"\n'
        + 'Without this import the per-tool structured renderers never run and every\n'
        + 'tool result falls through to the raw-JSON view (see #969).');
});

test('#969 wiring: tool-row.js invokes renderToolOutput on the success path', () => {
    const src = fs.readFileSync(TOOL_ROW_PATH, 'utf8');
    // The dispatcher is named `renderToolOutput`; it must be called
    // somewhere in tool-row.js, not just imported. A bare import
    // would still pass the import test above but produce raw output
    // because nothing wires the dispatcher's return value into the JSX.
    assert.ok(/\brenderToolOutput\s*\(/.test(src),
        'tool-row.js must call renderToolOutput(...) on the success path.\n'
        + 'A dangling import without an invocation regresses to the raw-JSON\n'
        + 'fallback for every tool (see #969).');
});

test('#969 wiring: tool-output.js exists on disk and exports renderToolOutput', () => {
    // The literal regression in #969 was that this file went missing on
    // develop. A node-side existence + export test catches that class
    // of failure even if every dispatcher test above silently shifts
    // to "skip because the module is missing".
    assert.ok(fs.existsSync(TOOL_OUTPUT_PATH),
        'static/ui/utils/tool-output.js must exist — see #969.');
    const src = fs.readFileSync(TOOL_OUTPUT_PATH, 'utf8');
    assert.ok(/export\s+function\s+renderToolOutput\b/.test(src),
        'tool-output.js must export `renderToolOutput` as the dispatcher entry point.');
});

// =========================================================================
// Runtime → renderer shape contract
//
// The dispatcher's per-tool renderers read fields off the tool result
// payload by name (e.g. `result.exit_code`, `result.matches`,
// `result.content`). If the runtime crate ever renames or removes one
// of those fields without updating the dispatcher, the renderer
// silently returns null and tool-row.js falls back to raw — exactly
// the regression class Alper hit.
//
// We can't drive the live Rust runtime from a node test, but we CAN
// pin the shape contract here in canonical fixtures derived from the
// Rust source. The fixtures below are copied from inline `json!({...})`
// calls in the corresponding tools — re-deriving them is the work a
// reviewer does when changing a tool's output. The tests assert the
// dispatcher returns a non-null tree for each canonical shape, which
// is the strongest contract test we can run from the JS side without
// shelling into the binary.
//
// To regenerate after a Rust-side shape change, copy the new
// `json!({...})` body from the source file referenced in each test
// header into the `result` literal below.
// =========================================================================

test('#969 shape contract: shell foreground (crates/alms-sandbox/src/shell/mod.rs)', () => {
    // Pinned from the json!({...}) at shell/mod.rs:330-336 + 572-576.
    const result = { exit_code: 0, stdout: 'ok\n', stderr: '' };
    const tree = renderToolOutput('shell', result, { command: 'echo ok' });
    assert.ok(tree, 'shell foreground shape must render structured output');
});

test('#969 shape contract: fs_read (crates/alms-sandbox/src/builtin/fs_read.rs)', () => {
    // Pinned from the json!({...}) at fs_read.rs:464-492 — content is
    // the only required field; everything else is conditional.
    const result = { content: 'line 1\nline 2\n' };
    const tree = renderToolOutput('fs_read', result, { path: 'README.md' });
    assert.ok(tree, 'fs_read minimal shape must render structured output');

    // Also pin the "partial read with truncation flags" variant.
    const partial = {
        content: 'line 1\n',
        total_lines: 100,
        offset: 0,
        limit: 1,
        line_truncated: true,
    };
    const partialTree = renderToolOutput('fs_read', partial,
        { path: 'big.txt', offset: 0, limit: 1 });
    assert.ok(partialTree, 'fs_read partial-read shape must render structured output');
});

test('#969 shape contract: fs_write (crates/alms-sandbox/src/builtin/fs_write.rs)', () => {
    // Pinned from the json!({...}) success branch in fs_write.rs.
    const result = { ok: true, path: '/tmp/foo.txt' };
    const tree = renderToolOutput('fs_write', result, { path: '/tmp/foo.txt' });
    assert.ok(tree, 'fs_write success shape must render structured output');
});

test('#969 shape contract: fs_grep content mode (crates/alms-sandbox/src/builtin/fs_grep.rs)', () => {
    // Pinned from the content-mode json!({...}) in fs_grep.rs.
    const result = {
        matches: [
            { file: 'src/lib.rs', line: 12, content: 'fn foo() {' },
        ],
        total: 1,
        truncated: false,
        truncated_lines: 0,
    };
    const tree = renderToolOutput('fs_grep', result, { pattern: 'foo' });
    assert.ok(tree, 'fs_grep content-mode shape must render structured output');
});

test('#969 shape contract: invoke_agent foreground (crates/alms-tools/src/invoke_agent*)', () => {
    // Pinned from the foreground response shape — name + response.
    const result = {
        name: 'researcher',
        session_id: '00000000-0000-0000-0000-000000000000',
        response: 'Done.',
    };
    const tree = renderToolOutput('invoke_agent', result,
        { name: 'researcher', task: 'Look something up.' });
    assert.ok(tree, 'invoke_agent foreground shape must render structured output');
});

test('#969 shape contract: http_get (crates/alms-sandbox/src/builtin/http_get.rs)', () => {
    // Pinned from the http_get json!({...}) — status, body, headers.
    const result = {
        status: 200,
        body: '{"hello": "world"}',
        headers: { 'content-type': 'application/json' },
    };
    const tree = renderToolOutput('http_get', result,
        { url: 'https://example.com/api' });
    assert.ok(tree, 'http_get shape must render structured output');
});

// =========================================================================
// #974 — Tool input formatting parity (renderParams in tool-row.js)
//
// The input renderer surface lives in tool-row.js's `renderParams(tool,
// params)` function. To test it under Node we reuse the same html-stub
// strategy as the dispatcher harness above, but also stub the
// non-renderParams imports `tool-row.js` carries (useSignal, toolSummary,
// fmtSize, renderToolOutput, TOOL_SUMMARY_LEN). renderParams does not
// touch any of those at runtime, so the no-op stubs are sufficient.
// =========================================================================

/**
 * Load `tool-row.js` under Node by stubbing its top-level imports with
 * inline shims. Returns the module's exported `renderParams` function.
 */
async function loadRenderParams() {
    const src = fs.readFileSync(TOOL_ROW_PATH, 'utf8');

    // Reuse the html-stub source from loadToolOutputModule(). Rebuilding
    // it here keeps the two harnesses independent — if you tweak this
    // one, the dispatcher harness above is unaffected and vice versa.
    const stub = `
        const __SENTINEL_OPEN = '\\u0001V';
        const __SENTINEL_CLOSE = '\\u0001';

        function __htmlStub(strings, ...values) {
            let joined = strings[0];
            for (let i = 0; i < values.length; i++) {
                joined += __SENTINEL_OPEN + i + __SENTINEL_CLOSE + strings[i + 1];
            }
            return __scanFull(joined, values);
        }

        function __scanFull(s, values) {
            const stack = [{ kind: 'elem', tag: '#fragment', classes: [], children: [] }];
            const top = () => stack[stack.length - 1];
            let i = 0;
            while (i < s.length) {
                const lt = s.indexOf('<', i);
                if (lt === -1) { __emitText(s.slice(i), values, top()); break; }
                if (lt > i) __emitText(s.slice(i, lt), values, top());
                const gt = s.indexOf('>', lt + 1);
                if (gt === -1) break;
                const inside = s.slice(lt + 1, gt);
                if (inside.startsWith('/')) {
                    const tag = inside.slice(1).trim();
                    while (stack.length > 1 && stack[stack.length - 1].tag !== tag) stack.pop();
                    if (stack.length > 1) stack.pop();
                    i = gt + 1;
                    continue;
                }
                const selfClose = inside.endsWith('/');
                const head = (selfClose ? inside.slice(0, -1) : inside).trim();
                const spaceIdx = head.search(/\\s/);
                const tag = spaceIdx === -1 ? head : head.slice(0, spaceIdx);
                const attrs = spaceIdx === -1 ? '' : head.slice(spaceIdx + 1);
                const expandedAttrs = __expandSentinels(attrs, values);
                const classMatch = expandedAttrs.match(/class=\\"([^\\"]*)\\"/);
                const classes = classMatch ? classMatch[1].split(/\\s+/).filter(Boolean) : [];
                const elem = { kind: 'elem', tag, classes, children: [] };
                stack[stack.length - 1].children.push(elem);
                if (!selfClose) stack.push(elem);
                i = gt + 1;
            }
            const root = stack[0];
            if (root.children.length === 1 && root.children[0] && root.children[0].kind === 'elem') {
                return root.children[0];
            }
            return root;
        }

        function __expandSentinels(s, values) {
            return s.replace(/\\u0001V(\\d+)\\u0001/g, (_, idx) => {
                const v = values[Number(idx)];
                if (v === null || v === undefined || v === false || v === true) return '';
                if (v && typeof v === 'object') return v.tag || '';
                return String(v);
            });
        }

        function __emitText(chunk, values, parent) {
            if (!chunk) return;
            const re = /\\u0001V(\\d+)\\u0001/g;
            let last = 0;
            let m;
            while ((m = re.exec(chunk)) !== null) {
                const before = chunk.slice(last, m.index);
                const t = before.trim();
                if (t) parent.children.push({ kind: 'text', text: t });
                __pushValue(values[Number(m[1])], parent);
                last = re.lastIndex;
            }
            const tail = chunk.slice(last).trim();
            if (tail) parent.children.push({ kind: 'text', text: tail });
        }

        function __pushValue(v, parent) {
            if (v === null || v === undefined || v === false || v === true) return;
            if (Array.isArray(v)) { for (const item of v) __pushValue(item, parent); return; }
            if (v && typeof v === 'object' && v.kind) { parent.children.push(v); return; }
            const s = String(v);
            if (s.length > 0) parent.children.push({ kind: 'text', text: s });
        }
    `;

    // Replace the multi-symbol import line with the stub + bindings.
    // tool-row.js's import block is:
    //   import { html, useSignal } from '../../deps.js';
    //   import { toolSummary, fmtSize } from '../../utils/tool-summary.js';
    //   import { renderToolOutput } from '../../utils/tool-output.js';
    //   import { TOOL_SUMMARY_LEN } from '../../utils/constants.js';
    const depsRe =
        /^import\s*\{\s*html\s*,\s*useSignal\s*\}\s*from\s*['"]\.\.\/\.\.\/deps\.js['"];?\s*$/m;
    const summaryRe =
        /^import\s*\{[^}]*\}\s*from\s*['"]\.\.\/\.\.\/utils\/tool-summary\.js['"];?\s*$/m;
    const outputRe =
        /^import\s*\{[^}]*\}\s*from\s*['"]\.\.\/\.\.\/utils\/tool-output\.js['"];?\s*$/m;
    const constsRe =
        /^import\s*\{[^}]*\}\s*from\s*['"]\.\.\/\.\.\/utils\/constants\.js['"];?\s*$/m;

    if (!depsRe.test(src) || !summaryRe.test(src)
        || !outputRe.test(src) || !constsRe.test(src)) {
        throw new Error(
            'tool-row.js: expected imports for html/useSignal, tool-summary, '
            + 'tool-output, and constants. Update the test harness if the '
            + 'imports were refactored.'
        );
    }

    let stubbed = src.replace(
        depsRe,
        stub + '\nconst html = __htmlStub;\nconst useSignal = () => ({ value: false });'
    );
    stubbed = stubbed.replace(
        summaryRe,
        'const toolSummary = () => ""; const fmtSize = () => "";'
    );
    stubbed = stubbed.replace(
        outputRe,
        'const renderToolOutput = () => null;'
    );
    stubbed = stubbed.replace(
        constsRe,
        'const TOOL_SUMMARY_LEN = 80;'
    );

    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'alms-tool-row-test-'));
    const tmpFile = path.join(tmpDir, 'tool-row.mjs');
    fs.writeFileSync(tmpFile, stubbed, 'utf8');
    return await import(url.pathToFileURL(tmpFile).href);
}

const rowMod = await loadRenderParams();
const { renderParams, paramsAreTruncated } = rowMod;

// ── output-side parity: echo / math / datetime (#974 expanded scope) ─────

test('#974 echo output: string echo renders in monospace pane (not raw JSON)', () => {
    const tree = renderToolOutput('echo', 'hello world', { message: 'hello world' });
    assert.ok(tree, 'echo string result must render structured');
    const text = collectText(tree);
    assert.ok(text.includes('hello world'));
    // Negative: not a raw JSON dump.
    assert.ok(!text.includes('"message"'),
        'echo output must not contain a raw JSON dump\n' + dump(tree));
});

test('#974 echo output: object passthrough is pretty-printed, not raw fallback', () => {
    const tree = renderToolOutput('echo', { hello: 'world' }, {});
    assert.ok(tree, 'echo object result must render structured');
    const text = collectText(tree);
    assert.ok(text.includes('hello'));
    assert.ok(text.includes('world'));
});

test('#974 math output: integer result rendered as a kv-badge pill', () => {
    const tree = renderToolOutput('math', 42, { operation: 'add', a: 10, b: 32 });
    assert.ok(tree, 'math number result must render structured');
    const classes = collectClasses(tree);
    assert.ok(classes.has('tc-kv-badge'),
        'math result must be rendered as a kv-badge\n' + dump(tree));
    assert.ok(collectText(tree).includes('42'));
});

test('#974 math output: non-number result falls through to raw', () => {
    // Defensive: math should never return a non-number, but if it ever
    // does we want to fall through to raw rather than render a misleading
    // pill containing "[object Object]".
    const tree = renderToolOutput('math', { unexpected: true }, {});
    assert.equal(tree, null);
});

test('#974 datetime output: UTC + Local sections render with timezone badges', () => {
    const result = {
        iso: '2026-05-07T10:00:00Z',
        human: 'Thursday, May 7, 2026 10:00 AM',
        timezone: 'UTC',
        local_iso: '2026-05-07T13:00:00+03:00',
        local_human: 'Thursday, May 7, 2026 1:00 PM',
        local_timezone: 'Europe/Istanbul',
        utc_offset: '+03:00',
    };
    const tree = renderToolOutput('datetime', result, {});
    assert.ok(tree, 'datetime result must render structured');
    const classes = collectClasses(tree);
    const text = collectText(tree);
    assert.ok(classes.has('tc-kv-badge'),
        'timezone names must be rendered as kv-badges\n' + dump(tree));
    assert.ok(text.includes('UTC'));
    assert.ok(text.includes('Europe/Istanbul'));
    assert.ok(text.includes('+03:00'));
    assert.ok(text.includes('2026-05-07T10:00:00Z'));
    // Negative.
    assert.ok(!text.includes('"iso":'),
        'datetime output must not contain a raw JSON dump\n' + dump(tree));
});

// =========================================================================
// #974 — Per-tool input renderers (renderParams in tool-row.js)
//
// Each test below feeds a representative params object to renderParams
// and asserts that the rendered tree carries the expected fragments
// (path, pattern, mode, etc.) and that no params object is dumped as
// raw JSON. Negative `assert.ok(!text.includes('{'))` checks pin the
// "no raw fallback" contract from the issue's acceptance criterion.
// =========================================================================

test('#974 input shell: command renders in a code-block', () => {
    const tree = renderParams('shell', { command: 'echo hi' });
    assert.ok(tree);
    const text = collectText(tree);
    const classes = collectClasses(tree);
    assert.ok(classes.has('tc-code-block'),
        'shell command must render in a code-block\n' + dump(tree));
    assert.ok(text.includes('echo hi'));
});

test('#974 input fs_read: file-path header renders without offset/limit', () => {
    const tree = renderParams('fs_read', { path: 'src/lib.rs' });
    assert.ok(tree);
    const classes = collectClasses(tree);
    const text = collectText(tree);
    assert.ok(classes.has('tc-file-header'),
        'fs_read input must render the path as a file header\n' + dump(tree));
    assert.ok(text.includes('src/lib.rs'));
});

test('#974 input fs_read: line range surfaces when offset+limit set', () => {
    const tree = renderParams('fs_read', { path: 'big.rs', offset: 100, limit: 50 });
    assert.ok(tree);
    const text = collectText(tree);
    assert.ok(text.includes('big.rs'));
    assert.ok(text.includes('lines 101'),
        'fs_read input must surface the (offset+1) line range\n' + dump(tree));
    assert.ok(text.includes('150'),
        'fs_read input must surface the (offset+limit) tail\n' + dump(tree));
});

test('#974 input fs_write: file header + overwrite badge', () => {
    const tree = renderParams('fs_write', { path: 'a.txt', content: 'hi' });
    assert.ok(tree);
    const text = collectText(tree);
    const classes = collectClasses(tree);
    assert.ok(classes.has('tc-file-header'));
    assert.ok(text.includes('a.txt'));
    assert.ok(text.includes('overwrite'),
        'fs_write input must show overwrite badge by default\n' + dump(tree));
    assert.ok(text.includes('hi'));
});

test('#974 input fs_write: append mode surfaces correctly', () => {
    const tree = renderParams('fs_write', { path: 'log.txt', mode: 'append', content: 'x' });
    assert.ok(tree);
    const text = collectText(tree);
    assert.ok(text.includes('append'));
});

test('#974 input fs_edit: file header + replace summary + find/replace blocks', () => {
    const tree = renderParams('fs_edit', {
        path: 'src/foo.rs',
        old_string: 'let x = 1;',
        new_string: 'let x = 2;',
    });
    assert.ok(tree, 'fs_edit must render structured input (was raw JSON before #974)');
    const classes = collectClasses(tree);
    const text = collectText(tree);
    assert.ok(classes.has('tc-file-header'),
        'fs_edit input must use file-header\n' + dump(tree));
    assert.ok(text.includes('src/foo.rs'));
    assert.ok(text.includes('replace once'),
        'default replace mode label must be "replace once"\n' + dump(tree));
    assert.ok(text.includes('let x = 1;'));
    assert.ok(text.includes('let x = 2;'));
    // Negative: must not dump the params object as JSON.
    assert.ok(!text.includes('"old_string"'),
        'fs_edit must not dump params as raw JSON\n' + dump(tree));
});

test('#974 input fs_edit: replace_all surfaces as the badge', () => {
    const tree = renderParams('fs_edit', {
        path: 'a.rs',
        old_string: 'foo',
        new_string: 'bar',
        replace_all: true,
    });
    assert.ok(tree);
    assert.ok(collectText(tree).includes('replace all'));
});

test('#974 input fs_list: path header', () => {
    const tree = renderParams('fs_list', { path: 'src' });
    assert.ok(tree, 'fs_list must render structured input (was raw JSON before #974)');
    const classes = collectClasses(tree);
    const text = collectText(tree);
    assert.ok(classes.has('tc-file-header'));
    assert.ok(text.includes('src'));
    assert.ok(!text.includes('{'),
        'fs_list input must not include a raw JSON dump\n' + dump(tree));
});

test('#974 input fs_grep: pattern in path with default mode is unbadged', () => {
    const tree = renderParams('fs_grep', { pattern: 'fn ', path: 'src' });
    assert.ok(tree, 'fs_grep must render structured input (was raw JSON before #974)');
    const text = collectText(tree);
    assert.ok(text.includes('fn'));
    assert.ok(text.includes('src'));
    assert.ok(text.includes('in'),
        'fs_grep should render `<pattern> in <path>`\n' + dump(tree));
    // Default output_mode (files_with_matches) must NOT show as a badge —
    // the badge is reserved for explicit deviation.
    assert.ok(!text.includes('files_with_matches'),
        'default mode must not appear as a badge\n' + dump(tree));
});

test('#974 input fs_grep: non-default output_mode + glob + case-insensitive surface', () => {
    const tree = renderParams('fs_grep', {
        pattern: 'TODO',
        path: 'src',
        output_mode: 'content',
        glob: '*.rs',
        case_insensitive: true,
    });
    assert.ok(tree);
    const text = collectText(tree);
    assert.ok(text.includes('content'));
    assert.ok(text.includes('*.rs'));
    assert.ok(text.includes('case-insensitive'));
});

test('#974 input fs_glob: pattern in path', () => {
    const tree = renderParams('fs_glob', { pattern: '**/*.rs', path: 'src' });
    assert.ok(tree, 'fs_glob must render structured input (was raw JSON before #974)');
    const text = collectText(tree);
    assert.ok(text.includes('**/*.rs'));
    assert.ok(text.includes('src'));
});

test('#974 input http_get: GET <url> rendered in status row', () => {
    const tree = renderParams('http_get', { url: 'https://example.com/api' });
    assert.ok(tree);
    const text = collectText(tree);
    const classes = collectClasses(tree);
    assert.ok(classes.has('tc-kv-badge'),
        'GET method must render as a kv-badge\n' + dump(tree));
    assert.ok(text.includes('GET'));
    assert.ok(text.includes('https://example.com/api'));
});

test('#974 input workspace_write: file badge + mode label', () => {
    const tree = renderParams('workspace_write', {
        file: 'memories',
        mode: 'append',
        content: 'foo',
    });
    assert.ok(tree, 'workspace_write must render structured input (was raw JSON before #974)');
    const text = collectText(tree);
    const classes = collectClasses(tree);
    assert.ok(classes.has('tc-kv-badge'));
    assert.ok(text.includes('memories'));
    assert.ok(text.includes('append'));
    assert.ok(text.includes('foo'));
    // Negative.
    assert.ok(!text.includes('"file"'));
});

test('#974 input invoke_agent: name badge + task preview', () => {
    const tree = renderParams('invoke_agent', { name: 'reviewer', task: 'Review my PR.' });
    assert.ok(tree);
    const text = collectText(tree);
    const classes = collectClasses(tree);
    assert.ok(classes.has('tc-kv-badge'));
    assert.ok(text.includes('reviewer'));
    assert.ok(text.includes('Review my PR.'));
});

test('#974 input invoke_agent: task longer than INPUT_PREVIEW_LEN is truncated', () => {
    const longTask = 'A'.repeat(500);
    const tree = renderParams('invoke_agent', { name: 'iris', task: longTask });
    assert.ok(tree);
    const text = collectText(tree);
    // The raw 500-A blob should NOT appear; the truncated form (200 A's
    // + ellipsis) should.
    assert.ok(!text.includes('A'.repeat(500)),
        'long task must be truncated\n' + dump(tree));
    assert.ok(text.includes('…'),
        'truncation ellipsis must appear\n' + dump(tree));
});

test('#974 input invoke_agent: ephemeral subagent (no name)', () => {
    const tree = renderParams('invoke_agent', { task: 'Hello' });
    assert.ok(tree);
    const text = collectText(tree);
    assert.ok(text.includes('ephemeral'),
        'missing name must surface as `(ephemeral)`\n' + dump(tree));
});

test('#974 input invoke_agent: background flag surfaces as meta', () => {
    const tree = renderParams('invoke_agent', { name: 'l', task: 't', background: true });
    assert.ok(tree);
    assert.ok(collectText(tree).includes('background'));
});

test('#974 input send_message: to badge + message preview', () => {
    const tree = renderParams('send_message', { to: 'larry', message: 'help me' });
    assert.ok(tree);
    const text = collectText(tree);
    const classes = collectClasses(tree);
    assert.ok(classes.has('tc-kv-badge'));
    assert.ok(text.includes('larry'));
    assert.ok(text.includes('help me'));
});

test('#974 input read_messages: filter summary `from <agent>` + `last N`', () => {
    const tree = renderParams('read_messages', { from: 'larry', last_n: 5 });
    assert.ok(tree, 'read_messages must render structured input (was raw JSON before #974)');
    const text = collectText(tree);
    assert.ok(text.includes('from'));
    assert.ok(text.includes('larry'));
    assert.ok(text.includes('last 5'));
    assert.ok(!text.includes('"from"'));
});

test('#974 input read_session: session_id + summary-only badge + last N', () => {
    const tree = renderParams('read_session', {
        session_id: '11111111-2222-3333-4444-555555555555',
        last_n: 10,
        summary_only: true,
    });
    assert.ok(tree, 'read_session must render structured input (was raw JSON before #974)');
    const text = collectText(tree);
    assert.ok(text.includes('11111111'));
    assert.ok(text.includes('summary only'));
    assert.ok(text.includes('last 10'));
});

test('#974 input read_subagent_session: subagent name + run summary', () => {
    const tree = renderParams('read_subagent_session', {
        name: 'researcher',
        last_n: 20,
    });
    assert.ok(tree, 'read_subagent_session must render structured input (was raw JSON before #974)');
    const text = collectText(tree);
    const classes = collectClasses(tree);
    assert.ok(classes.has('tc-kv-badge'));
    assert.ok(text.includes('researcher'));
    assert.ok(text.includes('last 20'));
});

test('#974 input list_agents: returns null (no input — output renderer covers the call)', () => {
    const tree = renderParams('list_agents', {});
    assert.equal(tree, null,
        'list_agents takes no input — params section must be suppressed');
});

test('#974 input list_my_sessions: no params returns null', () => {
    const tree = renderParams('list_my_sessions', {});
    assert.equal(tree, null);
});

test('#974 input list_my_sessions: with limit, surfaces filter section', () => {
    const tree = renderParams('list_my_sessions', { limit: 5, include_current: true });
    assert.ok(tree);
    const text = collectText(tree);
    assert.ok(text.includes('limit 5'));
    assert.ok(text.includes('include current'));
});

test('#974 input ignore_message: reason rendered in monospace pane', () => {
    const tree = renderParams('ignore_message', { reason: 'nothing to add' });
    assert.ok(tree, 'ignore_message must render structured input (was raw JSON before #974)');
    const text = collectText(tree);
    assert.ok(text.includes('nothing to add'));
});

test('#974 input ignore_message: missing reason renders the no-reason hint', () => {
    const tree = renderParams('ignore_message', {});
    assert.ok(tree);
    assert.ok(collectText(tree).includes('no reason given'));
});

test('#974 input echo: message renders in monospace pane', () => {
    const tree = renderParams('echo', { message: 'hello' });
    assert.ok(tree);
    assert.ok(collectText(tree).includes('hello'));
});

test('#974 input math: operation badge + operands tuple', () => {
    const tree = renderParams('math', { operation: 'add', a: 10, b: 32 });
    assert.ok(tree);
    const text = collectText(tree);
    const classes = collectClasses(tree);
    assert.ok(classes.has('tc-kv-badge'));
    assert.ok(text.includes('add'));
    assert.ok(text.includes('10'));
    assert.ok(text.includes('32'));
});

test('#974 input datetime: returns null (no input — output renderer carries everything)', () => {
    const tree = renderParams('datetime', {});
    assert.equal(tree, null);
});

// Final sweep: pin the "no raw-JSON for any first-party tool" contract.
// Any tool name from the registry must produce a non-null params tree
// for a representative non-empty params object.
test('#974 acceptance: every first-party tool produces a structured input', () => {
    const cases = [
        ['shell', { command: 'ls' }],
        ['shell_exec', { command: 'ls' }],
        ['fs_read', { path: 'a' }],
        ['fs_write', { path: 'a', content: 'b' }],
        ['fs_edit', { path: 'a', old_string: 'x', new_string: 'y' }],
        ['fs_list', { path: '.' }],
        ['fs_grep', { pattern: 'p' }],
        ['fs_glob', { pattern: '**' }],
        ['http_get', { url: 'https://x' }],
        ['workspace_write', { file: 'memories', content: 'x' }],
        ['invoke_agent', { task: 't' }],
        ['send_message', { to: 'l', message: 'hi' }],
        ['read_messages', { from: 'l' }],
        ['read_session', { session_id: 'abc' }],
        ['read_subagent_session', { name: 'r' }],
        ['ignore_message', { reason: 'x' }],
        ['echo', { message: 'x' }],
        ['math', { operation: 'add', a: 1, b: 2 }],
    ];
    for (const [tool, params] of cases) {
        const tree = renderParams(tool, params);
        assert.ok(tree, `${tool}: must render a structured input (got null)`);
        const text = collectText(tree);
        // Heuristic raw-JSON detection: a real raw fallback would include
        // `{"<key>":` for one of the params we passed.
        for (const key of Object.keys(params)) {
            assert.ok(!text.includes(`"${key}":`),
                `${tool}: must not dump params as raw JSON (saw "${key}":)\n` + dump(tree));
        }
    }
});

// Tools with "no input" semantics must explicitly suppress the params
// section so the operator doesn't see an empty "Parameters" header.
test('#974 acceptance: zero-input tools render null params', () => {
    for (const tool of ['list_agents', 'list_my_sessions', 'datetime']) {
        const tree = renderParams(tool, {});
        assert.equal(tree, null,
            `${tool}: no-input tool must suppress the params section`);
    }
});

// =========================================================================
// #1005 review fix — `paramsAreTruncated` detection for the
// "View raw params" toggle (parallel to "View raw" on the result side).
//
// The toggle should appear only when one of the truncated input fields
// (`fs_edit.old_string`/`new_string`, `invoke_agent.task`,
// `send_message.message`, `ignore_message.reason`, `echo.message`) is
// longer than `INPUT_PREVIEW_LEN` (200). For everything else — including
// tools whose renderer never truncates anything — the structured panel
// is the full payload and no toggle is shown.
// =========================================================================

test('#1005 paramsAreTruncated: short fs_edit strings are not truncated', () => {
    assert.equal(
        paramsAreTruncated('fs_edit', {
            path: 'a.rs', old_string: 'foo', new_string: 'bar',
        }),
        false,
    );
});

test('#1005 paramsAreTruncated: long fs_edit.old_string triggers truncation', () => {
    assert.equal(
        paramsAreTruncated('fs_edit', {
            path: 'a.rs',
            old_string: 'A'.repeat(500),
            new_string: 'bar',
        }),
        true,
    );
});

test('#1005 paramsAreTruncated: long fs_edit.new_string triggers truncation', () => {
    assert.equal(
        paramsAreTruncated('fs_edit', {
            path: 'a.rs',
            old_string: 'foo',
            new_string: 'B'.repeat(500),
        }),
        true,
    );
});

test('#1005 paramsAreTruncated: long invoke_agent.task triggers truncation', () => {
    assert.equal(
        paramsAreTruncated('invoke_agent', {
            name: 'iris', task: 'C'.repeat(500),
        }),
        true,
    );
});

test('#1005 paramsAreTruncated: short invoke_agent.task is not truncated', () => {
    assert.equal(
        paramsAreTruncated('invoke_agent', { name: 'iris', task: 'short task' }),
        false,
    );
});

test('#1005 paramsAreTruncated: long send_message.message triggers truncation', () => {
    assert.equal(
        paramsAreTruncated('send_message', {
            to: 'larry', message: 'D'.repeat(500),
        }),
        true,
    );
});

test('#1005 paramsAreTruncated: long ignore_message.reason triggers truncation', () => {
    assert.equal(
        paramsAreTruncated('ignore_message', { reason: 'E'.repeat(500) }),
        true,
    );
});

test('#1005 paramsAreTruncated: long echo.message triggers truncation', () => {
    assert.equal(
        paramsAreTruncated('echo', { message: 'F'.repeat(500) }),
        true,
    );
});

test('#1005 paramsAreTruncated: tools without truncated fields always return false', () => {
    // shell.command, fs_read.path, http_get.url, workspace_write.content —
    // none of these are truncated by `renderParams`, so the toggle must
    // not appear regardless of length.
    assert.equal(
        paramsAreTruncated('shell', { command: 'G'.repeat(500) }),
        false,
        'shell.command is not in the truncated-fields list — no toggle expected',
    );
    assert.equal(
        paramsAreTruncated('fs_read', { path: 'H'.repeat(500) }),
        false,
    );
    assert.equal(
        paramsAreTruncated('http_get', { url: 'https://x/' + 'I'.repeat(500) }),
        false,
    );
    assert.equal(
        paramsAreTruncated('workspace_write', {
            file: 'memories', mode: 'append', content: 'J'.repeat(500),
        }),
        false,
        'workspace_write.content is rendered in full — no toggle expected',
    );
});

test('#1005 paramsAreTruncated: missing or null params is safe (returns false)', () => {
    assert.equal(paramsAreTruncated('fs_edit', null), false);
    assert.equal(paramsAreTruncated('fs_edit', undefined), false);
    assert.equal(paramsAreTruncated('fs_edit', {}), false);
});

test('#1005 paramsAreTruncated: unknown tool returns false', () => {
    assert.equal(
        paramsAreTruncated('unknown_third_party_tool', { something: 'X'.repeat(500) }),
        false,
        'unknown tools fall through to the raw-JSON fallback in renderParams '
        + '— that path shows the full payload already, no toggle needed',
    );
});

test('#1005 paramsAreTruncated: at-boundary length (== INPUT_PREVIEW_LEN) is not truncated', () => {
    // INPUT_PREVIEW_LEN is 200; truncateInput cuts strictly when length > n.
    assert.equal(
        paramsAreTruncated('echo', { message: 'A'.repeat(200) }),
        false,
        'message with length === INPUT_PREVIEW_LEN renders in full',
    );
    assert.equal(
        paramsAreTruncated('echo', { message: 'A'.repeat(201) }),
        true,
        'one char over INPUT_PREVIEW_LEN must trigger the toggle',
    );
});
