/**
 * Pure coverage-gate predicate for the load-time `reasoning_delta`
 * suppress-set (#1133 Layer 3 / Codex #3 sub-race B).
 *
 * A run that went terminal *during* a session load reports `terminal: true`
 * from `GET /runs/{id}/reasoning`, but its sealed final-turn reasoning is in
 * the loaded message history ONLY IF the step-2 messages GET resolved after
 * the runtime sealed the assistant message. The runtime seals that message
 * STRICTLY BEFORE it flips the run terminal and broadcasts the terminal SSE
 * event, and nothing else mutates history in between, so the terminal event's
 * session-event-log id (`seal_event_id`) is the coverage anchor: the
 * messages-GET high-water mark (`historyHWM`, sampled in the same id space) is
 * at/above `seal_event_id` IFF its history read ran after the seal.
 *
 * Returns `true` only when the loaded history demonstrably covers the seal, so
 * it is safe to suppress the replayed deltas that would otherwise double-render
 * on top of the sealed bubble (sub-race A). Returns `false` when coverage
 * cannot be proven — sub-race B, or a `null`/undefined/non-numeric anchor or
 * HWM — so the caller leaves the deltas to render the final reasoning exactly
 * once. Rendering once is strictly safer than risking zero renders.
 *
 * @param {number|null|undefined} historyHwm - the messages-GET high-water mark
 *   (`lastEventId`, seeded from `historyData.last_event_id`).
 * @param {number|null|undefined} sealEventId - the run's terminal-event id
 *   from the reasoning GET's `seal_event_id` field.
 * @returns {boolean} whether the loaded history covers the sealed reasoning.
 */
export function historyCoversSeal(historyHwm, sealEventId) {
    return (
        typeof sealEventId === 'number'
        && Number.isFinite(sealEventId)
        && historyHwm != null
        && Number.isFinite(Number(historyHwm))
        && Number(historyHwm) >= sealEventId
    );
}
