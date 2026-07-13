import { signal } from '../deps.js';
import { entityState } from './entity-state.js';

export const runs = entityState.runs;
export const activeRunId = entityState.activeRunId;
/** Which run is highlighted/selected in the run list (UI-only, not "running"). */
export const selectedRunId = signal(null);

/**
 * Generation counter incremented when SSE events indicate a run state change
 * (run_created, run_finished, run_error, run_cancelled).  The RunsTab
 * component subscribes to this signal to trigger a re-fetch of the runs list
 * so the panel always shows up-to-date status.
 */
export const runListGeneration = signal(0);

export function bumpRunListGeneration() {
    runListGeneration.value++;
}

export function replaceRuns(sessionId, nextRuns) {
    entityState.replaceRuns(sessionId, nextRuns);
}

export function clearRuns(sessionId) {
    entityState.clearRuns(sessionId);
}

export function upsertRun(run, cursor = null, streamEpoch = null) {
    entityState.upsertRun(run, cursor, streamEpoch);
}

export function setRunStatus(runId, status, options) {
    entityState.setRunStatus(runId, status, options);
}
