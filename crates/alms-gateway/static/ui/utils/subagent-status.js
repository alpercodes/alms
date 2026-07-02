/**
 * Status-label mapping for the Subagent status bar.
 *
 * The bar is a lightweight STATUS indicator: each chip shows a concise label
 * derived from the subagent's most recent coarse activity signal (the
 * ephemeral `subagent_activity` SSE events — see `state/subagents.js`), never
 * the subagent's streamed content. Keep the `kind` strings in sync with the
 * backend's `alms_tools::subagent_activity_kind` constants.
 */

/**
 * Compute the concise status label for a status-bar chip.
 *
 * @param {object} info - An `activeSubagents` entry
 *   ({ status, activity: {kind, tool}|null, ... }).
 * @returns {string} the label to render next to the subagent name.
 */
export function subagentStatusLabel(info) {
    if (!info) return '';
    if (info.status === 'done') return 'Done';
    if (info.status === 'fail') return 'Failed';
    const activity = info.activity;
    if (!activity || !activity.kind) return 'Starting…';
    switch (activity.kind) {
        case 'reasoning':
            return 'Reasoning…';
        case 'writing':
            return 'Writing…';
        case 'tool_start':
            return activity.tool ? `Using ${activity.tool}` : 'Using tool';
        case 'tool_end':
            // The tool finished; the next reasoning/writing signal lands as
            // soon as the subagent's LLM call resumes.
            return 'Running…';
        default:
            // Unknown kind from a newer backend — degrade to the generic
            // in-flight label rather than showing nothing.
            return 'Running…';
    }
}
