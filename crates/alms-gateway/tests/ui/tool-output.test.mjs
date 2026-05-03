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
