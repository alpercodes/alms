import { signal } from '../deps.js';
import { entityState } from './entity-state.js';

// Foreground message queue (messages waiting to be sent after current run finishes)
export const messageQueue = signal([]);

// Read-only compatibility selector over normalized session activity.
export const bgRuns = entityState.backgroundRuns;

export function replaceActivitySnapshot(sessions, cursor = null, streamEpoch = null) {
    entityState.replaceActivitySnapshot(sessions, cursor, streamEpoch);
}

export function applyActivityEvent(type, data, cursor = null, streamEpoch = null) {
    entityState.applyActivityEvent(type, data, cursor, streamEpoch);
}

export function beginActivityReconciliation(replayCeiling = null, streamEpoch = null) {
    return entityState.beginActivityReconciliation(replayCeiling, streamEpoch);
}

export function commitActivityReconciliation(token, sessions) {
    return entityState.commitActivityReconciliation(token, sessions);
}

export function abortActivityReconciliation(token) {
    entityState.abortActivityReconciliation(token);
}
