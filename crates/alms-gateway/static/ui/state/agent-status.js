/**
 * Agent status signal -- live phase indicator for the agent header bar.
 *
 * Driven by SSE `status` events (ephemeral, not persisted) and `run_created`
 * events (for DM/peer awareness).  Cleared on run end, stream close, or
 * session switch.
 *
 * Phase-to-label mapping:
 *   building_context  -> "Building context..."
 *   summarizing       -> "Summarizing history..."
 *   calling_llm       -> "Thinking..."
 *   executing_tools   -> "Running {detail}..."
 *   dm                -> "Chatting with {detail}..."
 *   null              -> null (idle)
 */

import { signal, computed } from '../deps.js';

/**
 * Raw phase state: { phase: string|null, detail: string|null }
 * Updated by setAgentPhase() and clearAgentPhase().
 */
export const agentPhase = signal({ phase: null, detail: null });

/**
 * Human-readable status label derived from the raw phase.
 * Returns null when idle (no active phase).
 */
export const agentStatus = computed(() => {
    const { phase, detail } = agentPhase.value;
    if (!phase) return null;

    switch (phase) {
        case 'building_context':
            return 'Building context\u2026';
        case 'summarizing':
            return 'Summarizing history\u2026';
        case 'calling_llm':
            return 'Thinking\u2026';
        case 'executing_tools':
            return detail ? `Running ${detail}\u2026` : 'Running tools\u2026';
        case 'dm':
            return detail ? `Chatting with ${detail}\u2026` : 'In conversation\u2026';
        default:
            return null;
    }
});

/**
 * Set the current agent phase.
 * @param {string} phase - One of the backend phase constants or 'dm'.
 * @param {string|null} [detail] - Extra info (tool names, peer name).
 */
export function setAgentPhase(phase, detail) {
    agentPhase.value = { phase, detail: detail || null };
}

/** Clear the agent phase (idle state). */
export function clearAgentPhase() {
    agentPhase.value = { phase: null, detail: null };
}
