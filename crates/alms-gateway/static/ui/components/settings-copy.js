/**
 * Copy shared by the Settings modal and the Agents panel.
 *
 * Both surfaces expose the same Debug and Summary controls -- server-level
 * in the modal, per-agent in the panel -- so the wording lives here to stop
 * the two from drifting when only one is edited. Copy that exists on a
 * single surface stays inline in that component.
 *
 * House style for hints: say what the field does and what a sane value
 * looks like. Keep valid ranges, and anything that stops an operator from
 * setting a value that silently does nothing. Leave derivations, wire
 * shapes and issue numbers to docs/config.md.
 */

/** Debug / context-window inspection toggle. */
export const DEBUG_MODE_HINT =
    'Emits a snapshot of the context window sent to the LLM on every turn, '
    + 'rendered below the chat. Applies from the next run.';

/**
 * Provider-scoping caveat for the reasoning / thinking controls.
 *
 * The two surfaces group these controls differently -- the modal by
 * provider block, the Agents panel by row -- so the noun is a parameter
 * rather than a second copy of the sentence.
 */
export function providerScopeHint(unit) {
    return `Each ${unit} applies only when the agent is on that provider; the others are ignored.`;
}

/** Summary provider + model pair (compaction summaries and episodic memory). */
export const SUMMARY_HINT =
    'Optional cheaper model for compaction summaries and episodic memory. '
    + 'Set provider and model together, or neither.';
