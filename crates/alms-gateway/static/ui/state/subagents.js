import { signal } from '../deps.js';

/**
 * Active subagents and their tool activity.
 * Shape: { [name]: { status: 'running'|'done'|'fail'|'cancelled', tools: [...], task: string } }
 */
export const activeSubagents = signal({});

/** Track a subagent invocation. */
export function trackSubagentStart(name, task) {
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

/** Mark a subagent as completed. */
export function trackSubagentEnd(name, status) {
    const current = activeSubagents.value[name];
    if (!current) return;
    activeSubagents.value = {
        ...activeSubagents.value,
        [name]: { ...current, status },
    };
}

/** Clear all subagent entries regardless of status. */
export function clearAllSubagents() {
    activeSubagents.value = {};
}
