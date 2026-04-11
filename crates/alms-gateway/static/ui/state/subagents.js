import { signal } from '../deps.js';

/**
 * Active subagents and their tool activity.
 * Shape: { [name]: { status: 'running'|'done'|'fail'|'cancelled', tools: [...], task: string } }
 */
export const activeSubagents = signal({});

/** Pending auto-remove timers keyed by subagent name. */
const removeTimers = {};

/** Delay (ms) before a completed subagent chip is removed from the bar. */
const REMOVE_DELAY_MS = 3000;

/** Track a subagent invocation. */
export function trackSubagentStart(name, task) {
    // Cancel any pending removal from a previous invocation with the same name.
    if (removeTimers[name]) {
        clearTimeout(removeTimers[name]);
        delete removeTimers[name];
    }
    activeSubagents.value = {
        ...activeSubagents.value,
        [name]: { status: 'running', tools: [], task: task || '' },
    };
}

/** Add a tool event to a subagent. */
export function trackSubagentTool(name, tool) {
    const current = activeSubagents.value[name];
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

/** Clear all subagent entries regardless of status. */
export function clearAllSubagents() {
    // Cancel all pending removal timers.
    for (const key of Object.keys(removeTimers)) {
        clearTimeout(removeTimers[key]);
        delete removeTimers[key];
    }
    activeSubagents.value = {};
}
