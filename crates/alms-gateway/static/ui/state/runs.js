import { signal } from '../deps.js';

export const runs = signal([]);
export const activeRunId = signal(null);
/** Which run is highlighted/selected in the run list (UI-only, not "running"). */
export const selectedRunId = signal(null);
