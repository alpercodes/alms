// JS-level tests for `static/ui/state/summary-status.js`: which session a
// summary failure shows in, and what dismissing one does and does not hide.
//
// The module imports `signal` from `../deps.js`, which pulls in the browser
// bundle, so it is copied into a temp tree next to a stub of that one export
// (the same arrangement as `stream-health.test.mjs`).

import { test, before, after, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import url from 'node:url';
import fs from 'node:fs/promises';
import os from 'node:os';

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));
const MODULE = path.resolve(__dirname, '../../static/ui/state/summary-status.js');

const SESSION_A = '22222222-2222-4222-8222-222222222222';
const SESSION_B = '33333333-3333-4333-8333-333333333333';

function status(sessionId, overrides) {
    return {
        session_id: sessionId,
        failure: {
            code: 'MISSING_KEY',
            provider: 'openrouter',
            model: 'google/gemma-4-31b-it',
            message: 'Conversation summaries failed: no API key is set for OpenRouter.',
            ...overrides,
        },
    };
}

let dir;
let copies = 0;
let m;

before(async () => {
    dir = await fs.mkdtemp(path.join(os.tmpdir(), 'alms-summary-status-test-'));
    await fs.writeFile(path.join(dir, 'package.json'), JSON.stringify({ type: 'module' }));
    await fs.writeFile(
        path.join(dir, 'deps.js'),
        'export const signal = (value) => ({ value });\n',
    );
    await fs.mkdir(path.join(dir, 'state'));
});

after(async () => {
    await fs.rm(dir, { recursive: true, force: true });
});

// A fresh copy per test, because dismissals are module state.
beforeEach(async () => {
    const target = path.join(dir, 'state', `summary-status-${copies++}.js`);
    await fs.copyFile(MODULE, target);
    m = await import(url.pathToFileURL(target).href);
});

test('a failure shows in its own session and not in another', () => {
    m.applySummaryStatus(status(SESSION_A));
    assert.equal(m.visibleSummaryFailure(SESSION_A).code, 'MISSING_KEY');
    assert.equal(m.visibleSummaryFailure(SESSION_B), null);
    assert.equal(m.visibleSummaryFailure(null), null);
});

test('nothing shows before any status arrives', () => {
    assert.equal(m.visibleSummaryFailure(SESSION_A), null);
});

test('a null failure clears it', () => {
    m.applySummaryStatus(status(SESSION_A));
    m.applySummaryStatus({ session_id: SESSION_A, failure: null });
    assert.equal(m.visibleSummaryFailure(SESSION_A), null);
});

test("another session's clear leaves the current failure alone", () => {
    m.applySummaryStatus(status(SESSION_A));
    m.applySummaryStatus({ session_id: SESSION_B, failure: null });
    assert.equal(m.visibleSummaryFailure(SESSION_A).code, 'MISSING_KEY');
});

test('a reopened stream starts from no problem', () => {
    m.applySummaryStatus(status(SESSION_A));
    m.clearSummaryFailure();
    assert.equal(m.visibleSummaryFailure(SESSION_A), null);
});

test('a dismissed failure stays hidden when the same one reports again', () => {
    m.applySummaryStatus(status(SESSION_A));
    m.dismissSummaryFailure();
    assert.equal(m.visibleSummaryFailure(SESSION_A), null);

    m.applySummaryStatus(status(SESSION_A));
    assert.equal(m.visibleSummaryFailure(SESSION_A), null);
});

test('dismissing carries to the next session, so switching does not nag', () => {
    m.applySummaryStatus(status(SESSION_A));
    m.dismissSummaryFailure();
    m.applySummaryStatus(status(SESSION_B));
    assert.equal(m.visibleSummaryFailure(SESSION_B), null);
});

test('dismissing one failure does not hide a different one', () => {
    m.applySummaryStatus(status(SESSION_A));
    m.dismissSummaryFailure();
    m.applySummaryStatus(status(SESSION_A, { code: 'UNREACHABLE' }));
    assert.equal(m.visibleSummaryFailure(SESSION_A).code, 'UNREACHABLE');

    m.applySummaryStatus(status(SESSION_A, { model: 'other/model' }));
    assert.equal(m.visibleSummaryFailure(SESSION_A).model, 'other/model');
});

test('a dismissed failure shows again if it clears and then comes back', () => {
    m.applySummaryStatus(status(SESSION_A));
    m.dismissSummaryFailure();
    m.applySummaryStatus({ session_id: SESSION_A, failure: null });
    m.applySummaryStatus(status(SESSION_A));
    assert.equal(m.visibleSummaryFailure(SESSION_A).code, 'MISSING_KEY');
});

test('dismissing with nothing showing is harmless', () => {
    m.dismissSummaryFailure();
    m.applySummaryStatus(status(SESSION_A));
    assert.equal(m.visibleSummaryFailure(SESSION_A).code, 'MISSING_KEY');
});
