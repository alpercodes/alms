import { signal } from '../deps.js';

// Foreground message queue (messages waiting to be sent after current run finishes)
export const messageQueue = signal([]);

// Background session activity: { [sessionId]: { runId, finished } }.
// `runId` is the most recent started run when known, or null when seeded from
// a snapshot / retained by an ended event because another run remains active.
// Drives the sidebar's cross-session activity dot via hasActiveRun() in
// session-list.js. See #856 / #909.
export const bgRuns = signal({});

export function setBgRun(sessionId, data) {
    bgRuns.value = { ...bgRuns.value, [sessionId]: data };
}

export function removeBgRun(sessionId) {
    const { [sessionId]: _, ...rest } = bgRuns.value;
    bgRuns.value = rest;
}
