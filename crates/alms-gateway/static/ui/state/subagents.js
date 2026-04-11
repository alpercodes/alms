import { signal } from '../deps.js';
import { activeSessionId } from './sessions.js';

/**
 * Active subagents and their tool activity.
 * Shape: { [key]: { status, tools, task, toolInvocationId, displayName, startedAt, sessionId } }
 *
 * Keys are unique identifiers for each subagent invocation:
 *   - Named subagents use the agent name as the key (e.g. "reviewer").
 *   - Unnamed subagents use "subagent-{toolInvocationId_prefix}" so that
 *     concurrent unnamed invocations each get their own slot.
 *
 * `displayName` is the human-friendly label shown in the SubagentBar chip.
 * For named subagents it equals the key; for unnamed ones it is "subagent".
 *
 * `toolInvocationId` is the tool_invocation_id of the parent's invoke_agent
 * call. Stored so that tool_end for invoke_agent can look up the correct
 * subagent entry even when the subagent has no name param.
 *
 * `startedAt` is the Date.now() timestamp when the subagent was started.
 * Used to compute duration when the subagent completes.
 *
 * `sessionId` is the subagent's session UUID, populated from the
 * invoke_agent result or the subagent_completed event. Used for
 * drill-down navigation ("View session").
 */
export const activeSubagents = signal({});

/**
 * When viewing a subagent session, stores the parent session ID so the
 * user can navigate back. Null when viewing a top-level session.
 */
export const parentSessionId = signal(null);

/** Pending auto-remove timers keyed by subagent key. */
const removeTimers = {};

/** Delay (ms) before a completed subagent chip is removed from the bar.
 *  Set to 15 seconds so completed subagents remain visible longer. */
const REMOVE_DELAY_MS = 15000;

/**
 * Track a subagent invocation.
 *
 * @param {string} name - Display name for the subagent chip (from invoke_agent params).
 * @param {string} task - The task description.
 * @param {string} [toolInvocationId] - The invoke_agent tool_invocation_id
 *   from the parent. Stored for matching on tool_end.
 */
export function trackSubagentStart(name, task, toolInvocationId) {
    // For unnamed subagents (name === 'subagent'), derive a unique key from
    // the toolInvocationId so concurrent unnamed invocations do not overwrite
    // each other. Named subagents keep the name as the key.
    const isUnnamed = (name === 'subagent');
    const key = (isUnnamed && toolInvocationId)
        ? 'subagent-' + toolInvocationId.slice(0, 8)
        : name;

    // Cancel any pending removal from a previous invocation with the same key.
    if (removeTimers[key]) {
        clearTimeout(removeTimers[key]);
        delete removeTimers[key];
    }
    activeSubagents.value = {
        ...activeSubagents.value,
        [key]: {
            status: 'running',
            tools: [],
            task: task || '',
            toolInvocationId: toolInvocationId || null,
            displayName: name,
            startedAt: Date.now(),
            sessionId: null,
        },
    };
}

/**
 * Add a tool event to a subagent.
 *
 * The `name` parameter is the backend-assigned source_agent label, which for
 * unnamed subagents looks like "subagent-{task_id_prefix}". This is matched
 * directly against the entry key (which was derived the same way at start
 * time for unnamed subagents) or via the stored toolInvocationId as a
 * fallback.
 */
export function trackSubagentTool(name, tool) {
    let current = activeSubagents.value[name];

    // Fallback: if no direct key match, search by toolInvocationId prefix.
    // The tool_start handler registers unnamed subagents under
    // "subagent-{toolInvocationId_prefix}" but the backend labels forwarded
    // events with "subagent-{task_id_prefix}" (a different ID). Try to match
    // by finding a running unnamed entry.
    if (!current && name.startsWith('subagent-')) {
        for (const [key, info] of Object.entries(activeSubagents.value)) {
            if (key.startsWith('subagent-') && info.status === 'running') {
                current = info;
                // Migrate the entry to the backend-assigned key so future
                // tool events match directly.
                const { [key]: entry, ...rest } = activeSubagents.value;
                activeSubagents.value = { ...rest, [name]: entry };
                // Migrate any pending removal timer.
                if (removeTimers[key]) {
                    clearTimeout(removeTimers[key]);
                    delete removeTimers[key];
                }
                break;
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
 *
 * For unnamed subagents the backend sends `subagent_name: null`, which
 * resolves to the fallback name "subagent". Since the entry key is
 * "subagent-{id}", a direct lookup would miss. When `name` is "subagent"
 * and no exact match exists, we search for any running entry whose key
 * starts with "subagent-" and end that one instead.
 */
export function trackSubagentEnd(name, status) {
    let key = name;
    let current = activeSubagents.value[key];

    // Fallback for unnamed subagents: the backend sends subagent_name=null,
    // so the caller passes "subagent" — but the entry key is "subagent-{id}".
    if (!current && name === 'subagent') {
        for (const [k, info] of Object.entries(activeSubagents.value)) {
            if (k.startsWith('subagent-') && info.status === 'running') {
                key = k;
                current = info;
                break;
            }
        }
    }

    if (!current) return;
    activeSubagents.value = {
        ...activeSubagents.value,
        [key]: { ...current, status },
    };

    // Schedule auto-removal after a brief delay.
    if (removeTimers[key]) {
        clearTimeout(removeTimers[key]);
    }
    removeTimers[key] = setTimeout(() => {
        delete removeTimers[key];
        const { [key]: _, ...rest } = activeSubagents.value;
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

/**
 * Set the session ID on a subagent entry.
 * Called when the invoke_agent result includes a session_id, or when the
 * subagent_completed event arrives with a session_id.
 *
 * @param {string} name - The subagent key
 * @param {string} sessionId - The subagent's session UUID
 */
export function setSubagentSessionId(name, sessionId) {
    let current = activeSubagents.value[name];
    // Fallback search for unnamed subagents (same logic as trackSubagentEnd)
    let key = name;
    if (!current && name === 'subagent') {
        for (const [k, info] of Object.entries(activeSubagents.value)) {
            if (k.startsWith('subagent-')) {
                key = k;
                current = info;
                break;
            }
        }
    }
    if (!current) return;
    activeSubagents.value = {
        ...activeSubagents.value,
        [key]: { ...current, sessionId },
    };
}

/**
 * Dynamically import the modules needed for session navigation.
 * Uses Promise.all so all modules load in parallel.
 * Dynamic imports break the circular dependency (subagents.js is
 * imported by session-list.js which imports these same modules).
 */
async function loadNavDeps() {
    const [loadMod, sseMod, chatMod, runsMod, genMod, loadingMod] = await Promise.all([
        import('../utils/load-session.js'),
        import('../hooks/use-session-stream.js'),
        import('../state/chat-actions.js'),
        import('../state/runs.js'),
        import('../state/select-generation.js'),
        import('../state/loading.js'),
    ]);
    return {
        loadSession: loadMod.loadSession,
        closeSessionStream: sseMod.closeSessionStream,
        replaceMessages: chatMod.replaceMessages,
        activeRunId: runsMod.activeRunId,
        selectedRunId: runsMod.selectedRunId,
        bumpSelectGeneration: genMod.bumpSelectGeneration,
        selectGeneration: genMod,
        sessionSwitchLoading: loadingMod.sessionSwitchLoading,
    };
}

/**
 * Perform the actual session switch (shared by drill-down and back navigation).
 *
 * @param {string} targetSessionId - The session to navigate to
 * @param {string} logPrefix - Label for diagnostic log messages
 */
async function doSessionSwitch(targetSessionId, logPrefix) {
    const deps = await loadNavDeps();
    const gen = deps.bumpSelectGeneration();
    deps.closeSessionStream();
    activeSessionId.value = targetSessionId;
    deps.activeRunId.value = null;
    deps.selectedRunId.value = null;
    deps.replaceMessages([]);
    clearAllSubagents();
    deps.sessionSwitchLoading.value = true;

    try {
        await deps.loadSession(targetSessionId, {
            isStale: () => gen !== deps.selectGeneration.selectGeneration,
            logPrefix,
        });
    } finally {
        if (gen === deps.selectGeneration.selectGeneration) {
            deps.sessionSwitchLoading.value = false;
        }
    }
}

/**
 * Navigate to a subagent's session by switching the active session.
 * Stores the current session as the parent so the user can navigate back.
 *
 * @param {string} sessionId - The subagent session to navigate to
 */
export function navigateToSubagentSession(sessionId) {
    if (!sessionId) return;
    // Store current session as parent for breadcrumb navigation
    const currentSession = activeSessionId.value;
    if (currentSession) {
        parentSessionId.value = currentSession;
    }
    doSessionSwitch(sessionId, 'navigateToSubagent').catch(err => {
        console.error('[navigateToSubagentSession] failed:', err);
    });
}

/**
 * Navigate back to the parent session from a subagent drill-down view.
 * Clears the parentSessionId signal after navigation.
 */
export function navigateToParentSession() {
    const parent = parentSessionId.value;
    if (!parent) return;
    parentSessionId.value = null;
    doSessionSwitch(parent, 'navigateToParent').catch(err => {
        console.error('[navigateToParentSession] failed:', err);
    });
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
