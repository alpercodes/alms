import { signal } from '../deps.js';

// Foreground message queue (messages waiting to be sent after current run finishes)
export const messageQueue = signal([]);

// Background runs: { [sessionId]: { runId, finished } }
// Populated by use-agent-events.js (live SSE) and use-boot.js (snapshot seed).
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
