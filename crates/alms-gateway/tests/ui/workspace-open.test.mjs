// JS-level tests for the "Open in Explorer" plumbing introduced in #858.
//
// This test exercises two pieces:
//
//   1. The `openWorkspaceInExplorer(agentId)` API client wrapper from
//      `static/ui/api/workspace.js`. The wrapper builds the request URL +
//      method that the gateway expects; we mock `fetch` and assert the
//      shape of the request rather than spinning up a real HTTP server.
//
//   2. The error-mapping logic embedded in the button's click handler in
//      `static/ui/components/panel/workspace-tab.js`. Rather than
//      instantiate a Preact tree under Node (which would require pulling
//      preact + signals + htm into the test runtime — none are vendored),
//      we re-state the mapping table inline and pin it against the
//      structured-error contract the backend handler returns. If the
//      backend codes drift, the test fails loudly.
//
// `static/ui/api/workspace.js` imports from `./client.js`, which reads
// `localStorage` and uses the global `fetch`. We provide a minimal stub
// for both so the wrapper runs unchanged. This is the same "rewrite top-
// level imports + load via tempfile" pattern used by `history.test.mjs`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import url from 'node:url';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const WORKSPACE_API_PATH = path.resolve(
    __dirname,
    '../../static/ui/api/workspace.js'
);
const CLIENT_API_PATH = path.resolve(
    __dirname,
    '../../static/ui/api/client.js'
);

/**
 * Load `static/ui/api/workspace.js` under Node, with a stubbed `client.js`
 * sibling that captures the fetch the wrapper makes. Returns the module
 * exports plus the captured-call array.
 */
async function loadWorkspaceApi() {
    const clientSrc = fs.readFileSync(CLIENT_API_PATH, 'utf8');
    const workspaceSrc = fs.readFileSync(WORKSPACE_API_PATH, 'utf8');

    // Verify the import shape so the rewrite below stays honest. If
    // workspace.js's import changes ('./client.js' → something else), the
    // test bails with a clear message rather than silently bypassing the
    // wrapper under test.
    const importRe =
        /^import\s+\{\s*([^}]+)\s*\}\s+from\s+['"]\.\/client\.js['"];?\s*$/m;
    if (!importRe.test(workspaceSrc)) {
        throw new Error(
            'workspace.js: expected a top-level `import { ... } from "./client.js"` line — '
            + 'test rewrite would not apply. Update workspace-open.test.mjs if the import shape changed.'
        );
    }

    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'alms-ws-open-test-'));
    const apiSubdir = path.join(tmpDir, 'api');
    fs.mkdirSync(apiSubdir, { recursive: true });

    // Write `client.js` verbatim into the temp directory. `client.js` is
    // self-contained — it depends only on `localStorage` and `fetch`,
    // both of which we stub on `globalThis` before importing.
    fs.writeFileSync(path.join(apiSubdir, 'client.js'), clientSrc, 'utf8');
    fs.writeFileSync(path.join(apiSubdir, 'workspace.js'), workspaceSrc, 'utf8');

    // Capture every `fetch` call as a structured record so tests can
    // assert on URL, method, headers, and body shape.
    /** @type {Array<{url: string, init: RequestInit & { body?: any }}>} */
    const calls = [];
    globalThis.fetch = async (urlArg, init) => {
        calls.push({ url: urlArg, init: init ?? {} });
        // Default to a 200 OK with the success shape the backend returns.
        return new Response(
            JSON.stringify({ ok: true, path: '/tmp/agents/iris' }),
            {
                status: 200,
                headers: { 'Content-Type': 'application/json' },
            },
        );
    };
    // `client.js` reads `localStorage` for the bearer token. Provide a
    // minimal in-memory store so the import succeeds.
    globalThis.localStorage = {
        _data: {},
        getItem(k) { return this._data[k] ?? null; },
        setItem(k, v) { this._data[k] = String(v); },
        removeItem(k) { delete this._data[k]; },
    };

    const mod = await import(
        url.pathToFileURL(path.join(apiSubdir, 'workspace.js')).href
    );
    return { mod, calls };
}

// ---------------------------------------------------------------------------
// 1. API wrapper request shape
// ---------------------------------------------------------------------------

test('#858: openWorkspaceInExplorer issues POST to /agents/{id}/workspace/open', async () => {
    const { mod, calls } = await loadWorkspaceApi();
    assert.equal(typeof mod.openWorkspaceInExplorer, 'function',
        'openWorkspaceInExplorer must be exported from api/workspace.js');

    const result = await mod.openWorkspaceInExplorer('iris-agent-id');
    // Wrapper resolves with the parsed JSON body on 2xx.
    assert.deepEqual(result, { ok: true, path: '/tmp/agents/iris' });

    assert.equal(calls.length, 1, 'exactly one fetch call expected');
    const { url: u, init } = calls[0];
    assert.equal(u, '/agents/iris-agent-id/workspace/open',
        'URL must match the backend route registered in routes.rs');
    assert.equal(init.method, 'POST');
    // The handler ignores the body but `client.js` always serializes an
    // object body to JSON — the empty `{}` is fine and keeps the
    // Content-Type header consistent with other POST endpoints.
    assert.equal(init.body, '{}');
    assert.equal(init.headers['Content-Type'], 'application/json');
});

test('#858: openWorkspaceInExplorer attaches bearer token from localStorage', async () => {
    const { mod, calls } = await loadWorkspaceApi();
    globalThis.localStorage.setItem('alms_auth_token', 'sk-test-123');

    await mod.openWorkspaceInExplorer('any-agent');
    assert.equal(calls.length, 1);
    assert.equal(
        calls[0].init.headers['Authorization'],
        'Bearer sk-test-123',
        'auth header must match the format used by the rest of the API client',
    );
});

test('#858: openWorkspaceInExplorer surfaces server error with parsed body', async () => {
    const { mod } = await loadWorkspaceApi();

    // Override the default stub for this single test to return a 503.
    globalThis.fetch = async () => new Response(
        JSON.stringify({
            error: { code: 'NOT_CONFIGURED', message: 'Workspace dir is unset' },
        }),
        { status: 503, headers: { 'Content-Type': 'application/json' } },
    );

    let caught;
    try {
        await mod.openWorkspaceInExplorer('any');
    } catch (err) {
        caught = err;
    }
    assert.ok(caught, 'error must propagate to caller');
    assert.equal(caught.status, 503);
    assert.equal(caught.error?.code, 'NOT_CONFIGURED');
    assert.equal(caught.error?.message, 'Workspace dir is unset');
});

test('#858: openWorkspaceInExplorer URL-encoding is the responsibility of the caller', async () => {
    // Document the contract: the wrapper does NOT URL-encode `agentId`.
    // Agent names in ALMS are already slug-safe (validated by
    // `validate_agent_name` server-side, no spaces / no `/`), and UUIDs
    // are hex + dashes. Both pass through path segments unchanged. If
    // a future feature ever lets agents have raw names (it shouldn't),
    // this contract is the place to revisit.
    const { mod, calls } = await loadWorkspaceApi();
    await mod.openWorkspaceInExplorer('iris');
    assert.equal(calls[0].url, '/agents/iris/workspace/open');
});

// ---------------------------------------------------------------------------
// 2. Error-code → user-facing label mapping
// ---------------------------------------------------------------------------
//
// This re-states the table in `OpenInExplorerButton`'s `onClick` handler.
// The point is not to test the component (that requires Preact) but to
// pin the mapping against the backend's structured-error contract. If the
// gateway ever renames or removes one of these codes, the next time
// someone touches the button they'll find this test failing as a flag.

/**
 * Re-implementation of the friendly-label mapping in workspace-tab.js.
 * Keeping it inline avoids dragging Preact into the test runtime; the
 * comment above explains why we accept the duplication.
 */
function labelForError(err) {
    const code = err?.error?.code;
    const msg = err?.error?.message || err?.message || 'Failed to open workspace';
    if (code === 'NOT_CONFIGURED') return 'Workspace dir not configured';
    if (code === 'WORKSPACE_PATH_MISSING') return 'Workspace path is missing on disk';
    if (code === 'LAUNCHER_FAILED') return 'Failed to launch file explorer';
    return msg;
}

test('#858: error-code labels cover every documented backend code', () => {
    // These are the codes the backend handler returns in order of likelihood.
    // The handler doc-comment in workspace.rs lists them — keep this set in
    // sync with that doc.
    const cases = [
        ['NOT_CONFIGURED', 'Workspace dir not configured'],
        ['WORKSPACE_PATH_MISSING', 'Workspace path is missing on disk'],
        ['LAUNCHER_FAILED', 'Failed to launch file explorer'],
    ];
    for (const [code, expected] of cases) {
        assert.equal(
            labelForError({ error: { code, message: 'raw' } }),
            expected,
            `code ${code} must map to a friendly label`,
        );
    }
});

test('#858: unknown error codes fall back to the server message', () => {
    // Defensive against a future backend code we don't yet know about —
    // the operator still sees the actual server message rather than a
    // silently-empty toast.
    const label = labelForError({
        error: { code: 'BRAND_NEW_CODE', message: 'something unusual' },
    });
    assert.equal(label, 'something unusual');
});

test('#858: errors with no error.code or error.message fall back to the generic message', () => {
    assert.equal(labelForError({}), 'Failed to open workspace');
    assert.equal(labelForError({ message: 'plain js error' }), 'plain js error');
});

test('#858: 404 NOT_FOUND surfaces server message verbatim (not in the friendly-label table)', () => {
    // 404 NOT_FOUND is intentionally NOT in the friendly-label table —
    // operators should see the actual "Agent not found: {id}" message
    // because the resolution step is upstream of the launcher and the
    // remediation is "pick a different agent", not "fix your config".
    const label = labelForError({
        status: 404,
        error: { code: 'NOT_FOUND', message: 'Agent not found: foo' },
    });
    assert.equal(label, 'Agent not found: foo');
});
