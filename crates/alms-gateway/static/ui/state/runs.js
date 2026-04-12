import { signal } from '../deps.js';

export const runs = signal([]);
export const activeRunId = signal(null);
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
