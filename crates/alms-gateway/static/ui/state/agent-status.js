/**
 * Agent status signal -- live phase indicator for the agent header bar.
 *
 * Driven by SSE `status` events (ephemeral, not persisted), `run_created`
 * events (for DM/peer awareness), and `tool_start`/`tool_end` events
 * (for per-tool granularity).  Cleared on run end, stream close, or
 * session switch.
 *
 * Phase-to-label mapping:
 *   building_context  -> "Building context..."
 *   summarizing       -> "Summarizing history..."
 *   calling_llm       -> "Thinking..."
 *   executing_tools   -> "Running {detail}..."  (batch-level, from status event)
 *   tool_active       -> "Running {detail}..."  (per-tool, from tool_start event)
 *   dm                -> "Chatting with {detail}..."
 *   null              -> null (idle)
 *
 * DM phase tracking:
 *   When the run source is "peer:<name>", `dmPeer` is set to the peer name.
 *   The DM context is shown as a fallback when no more specific phase
 *   (e.g. tool execution) is active.  Backend status events (building_context,
 *   calling_llm) that would show generic "Thinking..." are replaced with
 *   "Chatting with {peer}..." to maintain DM awareness.  Tool execution
 *   phases are allowed through so users can see what tool is running.
 */

import { signal, computed } from '../deps.js';

/**
 * Raw phase state: { phase: string|null, detail: string|null }
 * Updated by setAgentPhase() and clearAgentPhase().
 */
export const agentPhase = signal({ phase: null, detail: null });

/**
 * When non-null, the current run is a DM run and this holds the peer name.
 * Used to show "Chatting with {peer}..." as a fallback phase when no more
 * specific activity (tool execution) is happening.
 */
export const dmPeer = signal(null);

/**
 * Derive a human-readable status label from a raw phase + detail pair.
 * Returns null when idle (phase is null/undefined).
 *
 * Extracted as a pure function so callers that read agentPhase.value
 * directly (e.g. AgentHeaderBar) can derive the label without going
 * through the computed signal -- this avoids a subtle @preact/signals
 * reactivity edge-case where computed signals accessed via .value in
 * function component render bodies occasionally fail to trigger
 * re-renders (observed with @preact/signals@1.3.0 + Preact 10.24.3).
 */
export function phaseToLabel(phase, detail) {
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
        case 'tool_active':
            return detail ? `Running ${detail}\u2026` : 'Running tool\u2026';
        case 'dm':
            return detail ? `Chatting with ${detail}\u2026` : 'In conversation\u2026';
        default:
            return null;
    }
}

/**
 * Human-readable status label derived from the raw phase.
 * Returns null when idle (no active phase).
 *
 * NOTE: Some components read agentPhase.value directly and call
 * phaseToLabel() instead of using this computed signal, to work
 * around a @preact/signals reactivity edge-case.  Both approaches
 * produce the same labels.
 */
export const agentStatus = computed(() => {
    const { phase, detail } = agentPhase.value;
    return phaseToLabel(phase, detail);
});

/**
 * Set the current agent phase.
 * @param {string} phase - One of the backend phase constants, 'tool_active', or 'dm'.
 * @param {string|null} [detail] - Extra info (tool names, peer name).
 */
export function setAgentPhase(phase, detail) {
    agentPhase.value = { phase, detail: detail || null };
}

/** Clear the agent phase (idle state) and reset DM peer tracking. */
export function clearAgentPhase() {
    agentPhase.value = { phase: null, detail: null };
    dmPeer.value = null;
}

/**
 * Mark the current run as a DM conversation with the given peer.
 * Sets the initial "Chatting with..." phase and stores the peer name
 * for fallback display during non-tool phases.
 * @param {string} peer - The peer agent name.
 */
export function setDmContext(peer) {
    dmPeer.value = peer;
    agentPhase.value = { phase: 'dm', detail: peer };
}

/**
 * Revert to the DM fallback phase if a DM context is active,
 * otherwise clear to the given phase.
 * Used after tool_end to return to "Chatting with {peer}..." in DM runs
 * or to the last backend status phase in non-DM runs.
 * @param {string|null} [fallbackPhase] - Phase to revert to if not in DM.
 */
export function revertPhase(fallbackPhase) {
    if (dmPeer.value) {
        agentPhase.value = { phase: 'dm', detail: dmPeer.value };
    } else if (fallbackPhase) {
        agentPhase.value = { phase: fallbackPhase, detail: null };
    }
    // If neither DM nor fallback, leave the current phase as-is.
    // The next status event from the backend will update it.
}
