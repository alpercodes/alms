import { signal } from 'https://esm.sh/@preact/signals@1.3.0';

// Foreground message queue (messages waiting to be sent after current run finishes)
export const messageQueue = signal([]);

// Background runs: { [sessionId]: { runId, queue, shadowEs, finished, status } }
export const bgRuns = signal({});

export function setBgRun(sessionId, data) {
    bgRuns.value = { ...bgRuns.value, [sessionId]: data };
}

export function removeBgRun(sessionId) {
    const { [sessionId]: _, ...rest } = bgRuns.value;
    bgRuns.value = rest;
}
