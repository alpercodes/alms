// JS-level regression tests for `static/ui/utils/job-summary.js`.
//
// Pinned regression target:
//   - issue #1196 — a completed scheduled-job card truncated its summary to
//     200 chars at write time, so "Show more" revealed only ~one extra line.
//     The fix raises the backend cap to 4000 AND, when a stored summary is
//     truncated, has JobCompletionCard fetch the full persisted output via
//     GET /runs/{id}. These pure helpers make the fetch-or-not / which-text
//     decision unit-testable without a Preact/DOM harness (same pattern as
//     utils/card-activation.js).
//
// The module has no deps.js import, so it loads directly under Node.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import url from 'node:url';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const JOB_SUMMARY_PATH = path.resolve(
    __dirname,
    '../../static/ui/utils/job-summary.js'
);

const {
    summaryLooksTruncated,
    shouldFetchFullOutput,
    resolveDisplayedSummary,
    TRUNCATION_ELLIPSIS,
} = await import(url.pathToFileURL(JOB_SUMMARY_PATH).href);

// ---------------------------------------------------------------------------
// summaryLooksTruncated
// ---------------------------------------------------------------------------

test('#1196: summaryLooksTruncated is true for the backend ellipsis marker', () => {
    assert.equal(TRUNCATION_ELLIPSIS, '...');
    assert.equal(summaryLooksTruncated('a lot of text that was cut...'), true);
});

test('#1196: summaryLooksTruncated is false for a complete summary', () => {
    assert.equal(summaryLooksTruncated('all done, nothing omitted'), false);
    assert.equal(summaryLooksTruncated(''), false);
});

test('#1196: summaryLooksTruncated is defensive against non-strings', () => {
    assert.equal(summaryLooksTruncated(null), false);
    assert.equal(summaryLooksTruncated(undefined), false);
    assert.equal(summaryLooksTruncated(42), false);
    assert.equal(summaryLooksTruncated({}), false);
});

// ---------------------------------------------------------------------------
// shouldFetchFullOutput
// ---------------------------------------------------------------------------

test('#1196 C1: shouldFetchFullOutput keys on the explicit truncated flag', () => {
    // The authoritative signal is the backend `truncated` boolean — a runId
    // plus truncated:true fetches; truncated:false does not, regardless of the
    // summary text.
    assert.equal(
        shouldFetchFullOutput({ runId: 'run-1', summary: 'anything', truncated: true }),
        true,
    );
    assert.equal(
        shouldFetchFullOutput({ runId: 'run-1', summary: 'anything', truncated: false }),
        false,
    );
});

test('#1196 C1: truncated:false wins over a summary that ends with "..."', () => {
    // The false-positive the boolean fixes: a complete summary that naturally
    // ends in "..." must NOT trigger a fetch when the backend says truncated.
    assert.equal(
        shouldFetchFullOutput({ runId: 'run-1', summary: 'to be continued...', truncated: false }),
        false,
    );
});

test('#1196: shouldFetchFullOutput falls back to the ellipsis heuristic when the flag is absent', () => {
    // Legacy markers (persisted before the truncated flag) have no boolean —
    // fall back to the "..." suffix. (In practice they also lack a run_id, so
    // this path is defensive.)
    assert.equal(
        shouldFetchFullOutput({ runId: 'run-1', summary: 'partial output...' }),
        true,
    );
    assert.equal(
        shouldFetchFullOutput({ runId: 'run-1', summary: 'complete summary' }),
        false,
    );
});

test('#1196: shouldFetchFullOutput is false without a runId', () => {
    // No handle to fetch from — must not attempt a fetch even when truncated.
    assert.equal(
        shouldFetchFullOutput({ runId: null, summary: 'partial output...', truncated: true }),
        false,
    );
    assert.equal(shouldFetchFullOutput({ summary: 'partial output...' }), false);
});

test('#1196: shouldFetchFullOutput tolerates being called with no args', () => {
    assert.equal(shouldFetchFullOutput(), false);
    assert.equal(shouldFetchFullOutput({}), false);
});

// ---------------------------------------------------------------------------
// resolveDisplayedSummary
// ---------------------------------------------------------------------------

test('#1196: resolveDisplayedSummary prefers a non-empty fetched output', () => {
    assert.equal(
        resolveDisplayedSummary('truncated...', 'the complete, much longer output'),
        'the complete, much longer output',
    );
});

test('#1196: resolveDisplayedSummary falls back to the stored summary on empty/failed fetch', () => {
    // Graceful degradation: a failed fetch (null) or an empty response must
    // leave the user with the stored summary rather than a blank card.
    assert.equal(resolveDisplayedSummary('stored summary...', null), 'stored summary...');
    assert.equal(resolveDisplayedSummary('stored summary...', undefined), 'stored summary...');
    assert.equal(resolveDisplayedSummary('stored summary...', ''), 'stored summary...');
    assert.equal(resolveDisplayedSummary('stored summary...', 42), 'stored summary...');
});

test('#1196: resolveDisplayedSummary never returns null/undefined', () => {
    assert.equal(resolveDisplayedSummary(undefined, undefined), '');
    assert.equal(resolveDisplayedSummary(null, null), '');
});
