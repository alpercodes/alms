// Pure helpers for the scheduled-job completion card's fetch-on-expand
// behaviour (#1196).
//
// Kept dependency-free (no deps.js / preact import) so they can be
// unit-tested under Node like `utils/card-activation.js`. The component in
// `components/chat/job-completion-card.js` imports these to decide whether a
// stored summary was truncated and, if so, which text to display after the
// full persisted output is fetched via `GET /runs/{id}`.

// The ellipsis the backend appends when it truncates a job summary at the
// JOB_SUMMARY_MAX_CHARS cap (see `notify_job_completion` /
// `SseEventData::job_completed`). Only used as a LEGACY fallback signal now:
// the authoritative "there is more to fetch" flag is the explicit `truncated`
// boolean on the SSE payload / marker metadata (#1196 C1). The suffix check
// remains for markers persisted before that flag existed — which, lacking a
// run_id, never actually trigger a fetch anyway.
export const TRUNCATION_ELLIPSIS = '...';

/**
 * Legacy heuristic: whether a stored job summary ends with the backend's
 * truncation ellipsis. Superseded by the explicit `truncated` flag; kept as a
 * fallback for pre-#1196 markers. Note it has a benign false positive — a
 * summary that naturally ends in "..." looks truncated — which the boolean
 * path avoids entirely.
 *
 * @param {unknown} summary
 * @returns {boolean}
 */
export function summaryLooksTruncated(summary) {
    return typeof summary === 'string' && summary.endsWith(TRUNCATION_ELLIPSIS);
}

/**
 * Decide whether "Show more" should fetch the full persisted output.
 *
 * Requires a run id to fetch from. Prefers the explicit `truncated` flag
 * (backend-authoritative); falls back to the ellipsis heuristic only when the
 * flag is absent (legacy markers). A complete summary is already the full
 * text, so expanding it needs no network round-trip.
 *
 * @param {{ runId?: string|null, summary?: unknown, truncated?: unknown }} args
 * @returns {boolean}
 */
export function shouldFetchFullOutput({ runId, summary, truncated } = {}) {
    if (!runId) return false;
    if (typeof truncated === 'boolean') return truncated;
    return summaryLooksTruncated(summary);
}

/**
 * Choose the text to display given the stored (possibly truncated) summary and
 * a fetch result. Prefers a non-empty fetched full output; otherwise falls
 * back to the stored summary so a failed / empty fetch degrades gracefully.
 * Never returns null/undefined.
 *
 * @param {string} storedSummary
 * @param {unknown} fetchedOutput - the `response` field of GET /runs/{id}
 * @returns {string}
 */
export function resolveDisplayedSummary(storedSummary, fetchedOutput) {
    if (typeof fetchedOutput === 'string' && fetchedOutput.length > 0) {
        return fetchedOutput;
    }
    return storedSummary || '';
}
