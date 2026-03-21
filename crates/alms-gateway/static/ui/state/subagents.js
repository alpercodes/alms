import { signal } from '../deps.js';
import { listTasks } from '../api/tasks.js';

/**
 * Active subagents and their tool activity.
 * Shape: { [name]: { status: 'running'|'done'|'fail', tools: [...], task: string } }
 */
export const activeSubagents = signal({});

// Polling state
let pollHandle = null;
let pollCount = 0;
const POLL_INTERVAL_MS = 3000;
const MAX_POLL_COUNT = 100; // ~5 minutes max polling

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

/** Get names of running subagents. */
function getRunningNames() {
    return Object.entries(activeSubagents.value)
        .filter(([, info]) => info.status === 'running')
        .map(([name]) => name);
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
 * polls GET /tasks and cross-references with our tracked subagent names.
 * When none of our subagents appear in the active task list, we know
 * they're done. Then reloads session history via the callback.
 *
 * Bounded to MAX_POLL_COUNT polls (~5 min) to prevent infinite polling.
 */
export function startSubagentPoll(onAllDone) {
    stopSubagentPoll();
    if (!hasRunningSubagents()) return;

    pollCount = 0;

    pollHandle = setInterval(async () => {
        pollCount++;

        if (pollCount >= MAX_POLL_COUNT) {
            console.warn('[subagent-poll] Max polls reached, stopping');
            markAllRunningAsDone();
            stopSubagentPoll();
            if (onAllDone) onAllDone();
            return;
        }

        if (!hasRunningSubagents()) {
            stopSubagentPoll();
            if (onAllDone) onAllDone();
            return;
        }

        try {
            const data = await listTasks();
            const allTasks = data.tasks || [];

            // Check if any coordinator tasks are still active.
            // Note: this is a global check, not per-subagent, since the
            // tasks API doesn't expose subagent names. Bounded by
            // MAX_POLL_COUNT so it won't poll forever.
            const stillActive = allTasks.some(t =>
                (t.status === 'Running' || t.status === 'Pending')
            );

            // If no active tasks at all, all our subagents must be done
            if (!stillActive) {
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
    pollCount = 0;
}
