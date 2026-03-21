import { signal } from '../deps.js';
import { listTasks } from '../api/tasks.js';

/**
 * Active subagents and their tool activity.
 * Shape: { [name]: { status: 'running'|'done'|'fail', tools: [...], task: string } }
 */
export const activeSubagents = signal({});

// Polling interval handle for background subagent monitoring
let pollHandle = null;
const POLL_INTERVAL_MS = 3000;

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

/** Clear completed subagents, keep running ones. */
export function clearCompletedSubagents() {
    const filtered = {};
    for (const [name, info] of Object.entries(activeSubagents.value)) {
        if (info.status === 'running') {
            filtered[name] = info;
        }
    }
    activeSubagents.value = filtered;
}

/** Check if any subagents are still running. */
export function hasRunningSubagents() {
    return Object.values(activeSubagents.value).some(s => s.status === 'running');
}

/** Mark all running subagents as done. */
function markAllRunningAsDone() {
    const updated = {};
    for (const [name, info] of Object.entries(activeSubagents.value)) {
        if (info.status === 'running') {
            updated[name] = { ...info, status: 'done' };
        } else {
            updated[name] = info;
        }
    }
    activeSubagents.value = updated;
}

/**
 * Start polling for background subagent completion.
 *
 * After the parent run finishes, if subagents are still running, this
 * polls GET /tasks to detect when they complete. On completion:
 * - Updates SubagentBar status
 * - Calls the provided callback (to reload session history)
 * - Stops polling
 */
export function startSubagentPoll(onAllDone) {
    stopSubagentPoll();
    if (!hasRunningSubagents()) return;

    pollHandle = setInterval(async () => {
        try {
            const data = await listTasks();
            const activeTasks = (data.tasks || []).filter(
                t => t.status === 'Running' || t.status === 'Pending'
            );

            if (activeTasks.length === 0) {
                // All coordinator tasks finished — subagents are done
                markAllRunningAsDone();
                stopSubagentPoll();
                if (onAllDone) onAllDone();
            }
        } catch {
            // Server unreachable — keep polling
        }
    }, POLL_INTERVAL_MS);
}

/** Stop the background polling. */
export function stopSubagentPoll() {
    if (pollHandle) {
        clearInterval(pollHandle);
        pollHandle = null;
    }
}
