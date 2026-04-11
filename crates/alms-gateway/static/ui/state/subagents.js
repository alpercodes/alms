import { signal } from '../deps.js';

/**
 * Active subagents and their tool activity.
 * Shape: { [name]: { status: 'running'|'done'|'fail'|'cancelled', tools: [...], task: string, toolInvocationId?: string } }
 *
 * `toolInvocationId` is the tool_invocation_id of the parent's invoke_agent
 * call. Stored so that tool_end for invoke_agent can look up the correct
 * subagent entry even when the subagent has no name param (unnamed/ephemeral
 * subagents where the backend assigns a label like "subagent-abc123").
 */
export const activeSubagents = signal({});

/** Pending auto-remove timers keyed by subagent name. */
const removeTimers = {};

/** Delay (ms) before a completed subagent chip is removed from the bar. */
const REMOVE_DELAY_MS = 3000;

/**
 * Track a subagent invocation.
 *
 * @param {string} name - Display name for the subagent chip.
 * @param {string} task - The task description.
 * @param {string} [toolInvocationId] - The invoke_agent tool_invocation_id
 *   from the parent. Stored for matching on tool_end.
 */
export function trackSubagentStart(name, task, toolInvocationId) {
    // Cancel any pending removal from a previous invocation with the same name.
    if (removeTimers[name]) {
        clearTimeout(removeTimers[name]);
        delete removeTimers[name];
    }
    activeSubagents.value = {
        ...activeSubagents.value,
        [name]: { status: 'running', tools: [], task: task || '', toolInvocationId: toolInvocationId || null },
    };
}

/**
 * Add a tool event to a subagent.
 *
 * If no entry exists under `name`, checks for an unnamed placeholder entry
 * (key = "subagent") and renames it to the backend-assigned label. This
 * handles ephemeral subagents where trackSubagentStart used "subagent"
 * but the backend labels events with "subagent-{id_prefix}".
 */
export function trackSubagentTool(name, tool) {
    let current = activeSubagents.value[name];

    // Auto-rename: the frontend registered an unnamed subagent as "subagent"
    // but the backend labels forwarded events with "subagent-<id>".  Detect
    // this by checking if an entry keyed "subagent" exists and the incoming
    // name starts with "subagent-".
    if (!current && name.startsWith('subagent-')) {
        const placeholder = activeSubagents.value['subagent'];
        if (placeholder && placeholder.status === 'running') {
            // Rename the placeholder entry to the backend-assigned label.
            const { 'subagent': entry, ...rest } = activeSubagents.value;
            activeSubagents.value = { ...rest, [name]: entry };
            current = entry;
            // Migrate any pending removal timer.
            if (removeTimers['subagent']) {
                clearTimeout(removeTimers['subagent']);
                delete removeTimers['subagent'];
            }
        }
    }

    if (!current) return;
    const tools = [...current.tools];
    const idx = tools.findIndex(t => t.id === tool.id);
    if (idx >= 0) {
        tools[idx] = { ...tools[idx], ...tool };
    } else {
        tools.push(tool);
    }
    activeSubagents.value = {
        ...activeSubagents.value,
        [name]: { ...current, tools },
    };
}

/**
 * Mark a subagent as completed and schedule its removal from the bar.
 *
 * The entry stays visible for REMOVE_DELAY_MS so the user can see the
 * final status (checkmark / X) before it disappears.
 */
export function trackSubagentEnd(name, status) {
    const current = activeSubagents.value[name];
    if (!current) return;
    activeSubagents.value = {
        ...activeSubagents.value,
        [name]: { ...current, status },
    };

    // Schedule auto-removal after a brief delay.
    if (removeTimers[name]) {
        clearTimeout(removeTimers[name]);
    }
    removeTimers[name] = setTimeout(() => {
        delete removeTimers[name];
        const { [name]: _, ...rest } = activeSubagents.value;
        activeSubagents.value = rest;
    }, REMOVE_DELAY_MS);
}

/**
 * Find the subagent name associated with a given invoke_agent tool
 * invocation ID. Returns the name key, or null if not found.
 *
 * Used by tool_end for invoke_agent when the tool params lack a name
 * (unnamed/ephemeral subagents).
 */
export function findSubagentByToolInvocationId(toolInvocationId) {
    if (!toolInvocationId) return null;
    for (const [name, info] of Object.entries(activeSubagents.value)) {
        if (info.toolInvocationId === toolInvocationId) return name;
    }
    return null;
}

/** Clear all subagent entries regardless of status. */
export function clearAllSubagents() {
    // Cancel all pending removal timers.
    for (const key of Object.keys(removeTimers)) {
        clearTimeout(removeTimers[key]);
        delete removeTimers[key];
    }
    activeSubagents.value = {};
}
