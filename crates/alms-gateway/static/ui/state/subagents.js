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
 * Resolution order (A1-2 / #1125):
 *   1. `toolInvocationId` — exact correlation with the parent's invoke_agent
 *      call. This is the only reliable disambiguator when two unnamed /
 *      ephemeral subagents run concurrently (both arrive with
 *      `subagent_name: null` → caller name "subagent"). The
 *      `subagent_completed` SSE event now carries `tool_invocation_id`
 *      (matching the `subagent_started` / `tool_start` paths).
 *   2. `subagentSessionId` — match the entry whose stored sessionId equals
 *      the completing subagent's session. Useful when the id was attached
 *      earlier (via `subagent_started` or the invoke_agent result) but the
 *      tool_invocation_id is unavailable.
 *   3. `name` — legacy first-match fallback. For unnamed subagents the
 *      backend sends `subagent_name: null` → caller passes "subagent", but
 *      the entry key is "subagent-{id}", so we search for any running entry
 *      whose key starts with "subagent-". Named subagents match by key
 *      directly. This path is preserved last so named-subagent behaviour and
 *      older events without the id keep working unchanged.
 *
 * @param {string} name - The subagent key / display name ("subagent" for unnamed).
 * @param {string} status - Terminal status ('done' | 'fail').
 * @param {string} [toolInvocationId] - The parent's invoke_agent tool_invocation_id.
 * @param {string} [subagentSessionId] - The completing subagent's session UUID.
 */
export function trackSubagentEnd(name, status, toolInvocationId, subagentSessionId) {
    const key = resolveSubagentKey(name, toolInvocationId, subagentSessionId);
    if (!key) return;
    const current = activeSubagents.value[key];
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
 * Find the subagent entry key whose stored sessionId matches the given
 * session UUID. Returns the key, or null if not found.
 *
 * Used as the second-tier resolver (after tool_invocation_id) for
 * `subagent_completed` / `setSubagentSessionId` so concurrent unnamed
 * subagents resolve to the right chip even when the tool_invocation_id is
 * unavailable but the session id was attached earlier (e.g. via
 * `subagent_started`). See A1-2 / #1125.
 */
export function findSubagentBySessionId(sessionId) {
    if (!sessionId) return null;
    for (const [name, info] of Object.entries(activeSubagents.value)) {
        if (info.sessionId === sessionId) return name;
    }
    return null;
}

/**
 * Resolve the activeSubagents map key for a subagent event using the A1-2
 * (#1125) resolution order: tool_invocation_id first, then session id, then
 * the name first-match fallback. Returns the resolved key, or null if no
 * entry matched.
 *
 * The name fallback preserves the legacy behaviour: named subagents match
 * by key directly; the literal "subagent" (unnamed, no id available) matches
 * the first running "subagent-{id}" entry. Keeping name LAST means older
 * events without a tool_invocation_id and all named-subagent flows behave
 * exactly as before.
 *
 * @param {string} name - The subagent key / display name ("subagent" for unnamed).
 * @param {string} [toolInvocationId] - The parent's invoke_agent tool_invocation_id.
 * @param {string} [sessionId] - The subagent's session UUID.
 * @returns {string|null}
 */
function resolveSubagentKey(name, toolInvocationId, sessionId) {
    // 1. Exact correlation by the parent's invoke_agent tool_invocation_id.
    const byInvocation = findSubagentByToolInvocationId(toolInvocationId);
    if (byInvocation) return byInvocation;

    // 2. Match by the subagent's session id (attached earlier).
    const bySession = findSubagentBySessionId(sessionId);
    if (bySession) return bySession;

    // 3. Legacy name fallback.
    if (activeSubagents.value[name]) return name;
    if (name === 'subagent') {
        for (const [k, info] of Object.entries(activeSubagents.value)) {
            if (k.startsWith('subagent-') && info.status === 'running') {
                return k;
            }
        }
    }
    return null;
}

/**
 * Set the session ID on a subagent entry.
 * Called when the invoke_agent result includes a session_id, when the
 * subagent_started event fires, or when the subagent_completed event arrives
 * with a session_id.
 *
 * Resolution order mirrors `trackSubagentEnd` (A1-2 / #1125): resolve the
 * target entry by `toolInvocationId` first, then by an already-set matching
 * `sessionId` (idempotent re-attach), then by name. The name first-match
 * fallback is preserved last so named-subagent and pre-id flows are unchanged.
 *
 * Callers that already pre-resolve the key (e.g. the `tool_start` /
 * `subagent_started` handlers do `findSubagentByToolInvocationId(toolId) || name`)
 * can omit `toolInvocationId`; passing it is preferred so resolution is robust
 * for concurrent unnamed subagents.
 *
 * @param {string} name - The subagent key / display name ("subagent" for unnamed).
 * @param {string} sessionId - The subagent's session UUID.
 * @param {string} [toolInvocationId] - The parent's invoke_agent tool_invocation_id.
 */
export function setSubagentSessionId(name, sessionId, toolInvocationId) {
    const key = resolveSubagentKey(name, toolInvocationId, sessionId);
    if (!key) return;
    const current = activeSubagents.value[key];
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
    const [loadMod, sseMod, chatMod, runsMod, genMod, loadingMod, bootMod, agentsMod] = await Promise.all([
        import('../utils/load-session.js'),
        import('../hooks/use-session-stream.js'),
        import('../state/chat-actions.js'),
        import('../state/runs.js'),
        import('../state/select-generation.js'),
        import('../state/loading.js'),
        // `saveActiveSession` lives in use-boot.js. We pull it in here so
        // the subagent drill-down path persists the active session id to
        // localStorage the same way the sidebar / runs-tab navigation
        // does. Without this, the operator clicks "View session" on a
        // subagent chip, lands on the subagent transcript, then reloads
        // and falls back to the parent agent's first chat — the #1045
        // symptom. Dynamic import is used because the static one would
        // create the circular subagents.js <-> use-boot.js dependency
        // the rest of this helper is already designed to avoid.
        import('../hooks/use-boot.js'),
        import('../state/agents.js'),
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
        saveActiveSession: bootMod.saveActiveSession,
        activeAgentId: agentsMod.activeAgentId,
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
    // Persist the new active session so a subsequent reload lands the
    // operator back on this subagent / parent session rather than on
    // the agent's stale first-visible-chat fallback. The sidebar /
    // runs-tab navigation paths already do this via `navigate-session.
    // js`, but the subagent drill-down path in this file historically
    // didn't, which is the immediate cause of #1045's reload-loss.
    // Pair this with `resolveStoredSessionId` in `use-boot.js` so the
    // pointer remains useful even when the target is a hidden session
    // type (subagent / job / episodic).
    if (deps.activeAgentId.value) {
        deps.saveActiveSession(deps.activeAgentId.value, targetSessionId);
    }
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

/**
 * Rehydrate `activeSubagents` from a freshly-loaded chat-history snapshot.
 *
 * Called by `loadSession()` after `replaceMessages(...)` so that page
 * reloads and session switches re-create the SubagentBar chips for any
 * subagent invocations that are still in flight server-side. Without
 * this, the bar lives only in SSE-event memory and silently disappears
 * across reload / session-switch boundaries until the run reaches a
 * terminal status — see #1041.
 *
 * Detection rules, matching what `use-session-stream.js` does at live
 * `tool_start` / `tool_end` time:
 *
 *   - Foreground `invoke_agent` rows with `status === 'running'` are
 *     still in flight (parent is blocked on the subagent). Add to the
 *     bar with `status: 'running'`.
 *   - Background `invoke_agent` rows whose result has `task_id` are
 *     still in flight unless a matching `subagent_completed` history
 *     marker is paired with this specific invocation. Add to the bar
 *     with `status: 'running'` when no completion marker is paired.
 *
 * Foreground rows with `status: 'done' | 'fail'` and no `task_id` in
 * the result are fully completed — the result is already in chat
 * history and there is no live subagent to track.
 *
 * Background-pairing detail (codex P1 on PR #1049):
 *   Named subagents deliberately reuse the same `session_id` across
 *   invocations (see `alms-coordinator/src/lib.rs` —
 *   `test_named_subagent_persistent_session`). The persisted
 *   `subagent_completion` marker carries only `session_id`, not
 *   `task_id`, so naive `Set<session_id>` membership would let an
 *   older completion marker incorrectly suppress a still-running
 *   newer invocation against the same session. We instead walk
 *   `messages` in chronological order and pair each completion marker
 *   with the oldest still-unpaired background invocation for that
 *   session (FIFO queue per `session_id`). Rows that remain unpaired
 *   at the end of the pass are the genuinely-in-flight invocations
 *   that get a SubagentBar chip.
 *
 *   Tim's review on #1049 (suggestion 1) verified that FIFO vs LIFO
 *   is observably equivalent today — the "later-wins-same-key" rule
 *   collapses both strategies onto identical final state for named
 *   subagents (shared map key), and unnamed concurrent invocations
 *   don't share a `session_id` so their queues are independent. The
 *   pairing-count contract (one completion terminates one invocation)
 *   is what fixes the bug; FIFO is the simplest implementation and
 *   matches the chronological mental model.
 *
 *   The chronological-order invariant is implicit (caller-side: the
 *   sqlite `ORDER BY seq` clause + `mapHistoryMessages` timestamp
 *   interleave). A defensive one-shot `console.warn` at function
 *   entry detects a future refactor that breaks ordering (Tim's
 *   suggestion 2 — cheap detection of a load-bearing invariant).
 *
 * Existing entries are preserved (no clear): this function is purely
 * additive so a SubagentBar entry created by a live SSE `tool_start`
 * that fired between `replaceMessages` and this call is not stomped.
 * The session-switch path explicitly calls `clearAllSubagents()`
 * before `loadSession()` (see `doSessionSwitch` above), so it always
 * starts from an empty bar; the page-reload path goes through
 * `boot()` which also starts from an empty bar.
 *
 * The subagent's inner tool-call history (the tool rows visible inside
 * the panel) is intentionally not reconstructed here — that data lives
 * on the subagent's own session and would require a second round-trip
 * per active subagent. The chip + panel chrome (name, task, spinner,
 * "View session" button) re-renders correctly with just the metadata
 * available on the parent's `invoke_agent` tool row; subsequent live
 * SSE `tool_start` events with `source_agent` set populate the inner
 * list as activity continues post-reload.
 *
 * @param {Array} messages - chat messages array (post-`mapHistoryMessages`,
 *   typically `chatMessages.value` immediately after `replaceMessages`).
 */
export function rehydrateSubagentsFromHistory(messages) {
    if (!Array.isArray(messages) || messages.length === 0) return;

    // Single chronological pass: maintain a per-session FIFO queue of
    // pending background invocations. Each `subagent_completed` marker
    // pops the oldest queued invocation for its session, so re-invocations
    // of a named subagent (which share `session_id`) pair one-to-one with
    // their own completion markers instead of letting an older marker
    // incorrectly suppress a newer still-in-flight invocation. See codex
    // P1 finding on PR #1049.
    //
    // Why FIFO specifically? Tim's review on #1049 (suggestion 1) noted
    // that FIFO vs LIFO is observably equivalent today: named subagents
    // share a map key (`name`), so "later-wins-same-key" collapses both
    // strategies onto the same final state; unnamed concurrent
    // invocations don't share a `session_id` (UUID v4 per dispatch) so
    // their pairing queues are independent. The pairing-count contract
    // (one completion marker terminates one invocation) is what fixes
    // the codex P1 bug — FIFO is just the simplest implementation and
    // matches the chronological mental model. If a future refactor
    // breaks the same-key collapse (e.g. unnamed concurrent backgrounds
    // gain stable map keys), FIFO would be the correct behaviour by
    // construction.
    //
    // `messages` is the post-`mapHistoryMessages` array, which is
    // chronological because that helper interleaves entries by
    // `timestamp` and `GET /sessions/{id}/messages` serves them
    // `ORDER BY seq` (monotonic with insertion). A defensive
    // out-of-order check below flags the next refactor that breaks the
    // ordering invariant (Tim's suggestion 2). Pending rows that
    // survive the pass are the still-running invocations.
    const pendingBySession = new Map(); // sessionId -> array of pending background rows
    const candidateRows = []; // foreground-running + still-pending background rows in order
    const additions = {};
    let lastSeenTsMs = -Infinity;
    let orderingWarned = false;

    for (const m of messages) {
        // Defensive: detect a future refactor that breaks the
        // chronological-order invariant our FIFO pairing relies on.
        // Cheap and silent in the happy path. Only warns once per call
        // so a malformed history doesn't spam the console.
        if (!orderingWarned && m && typeof m.ts === 'string') {
            const tsMs = Date.parse(m.ts);
            if (Number.isFinite(tsMs)) {
                if (tsMs < lastSeenTsMs) {
                    console.warn(
                        '[rehydrateSubagentsFromHistory] messages are not in chronological order; '
                        + 'FIFO pairing of subagent invocations to completion markers may be wrong. '
                        + 'See PR #1049 / Tim review suggestion 2.',
                    );
                    orderingWarned = true;
                } else {
                    lastSeenTsMs = tsMs;
                }
            }
        }

        if (m.type === 'subagent_completed') {
            // Pair with the oldest unpaired background invocation for
            // this session. Without a session_id we cannot pair (and
            // the marker is informational only); ignore it.
            //
            // A1-2 / #1125: the persisted subagent_completion marker now
            // also carries `m.toolInvocationId` (surfaced by history.js).
            // The session-id FIFO pairing here is already correct for the
            // two cases that matter — unnamed concurrent backgrounds don't
            // share a session_id (UUID v4 per dispatch) so their queues are
            // independent, and named subagents collapse onto a shared map
            // key making FIFO/pairing observably equivalent (see the long
            // note above). tool_invocation_id is available if a future
            // refactor ever needs exact per-invocation pairing here; not
            // wired in now to keep this pass low-risk.
            if (!m.sessionId) continue;
            const queue = pendingBySession.get(m.sessionId);
            if (queue && queue.length > 0) {
                const matched = queue.shift();
                matched.paired = true;
            }
            continue;
        }

        if (m.type !== 'tool' || m.tool !== 'invoke_agent') continue;

        const result = (typeof m.result === 'object' && m.result) || null;
        const isBackground = !!(result && result.task_id);
        const subagentSessionId = result?.session_id || null;

        if (isBackground) {
            // Background subagent: parent's invoke_agent row completes
            // quickly with { task_id, session_id }; queue it for
            // pairing against a later `subagent_completed` marker. If
            // unpaired at the end of the pass, the subagent is still
            // running.
            const row = { msg: m, paired: false };
            candidateRows.push(row);
            if (subagentSessionId) {
                let queue = pendingBySession.get(subagentSessionId);
                if (!queue) {
                    queue = [];
                    pendingBySession.set(subagentSessionId, queue);
                }
                queue.push(row);
            }
            // A background row with no session_id cannot be paired by
            // any completion marker, so it always survives the pass.
            continue;
        }

        // Foreground subagent: the parent blocks until the subagent
        // finishes, so an unfinished row literally is the indicator
        // that the subagent is still in flight. Done/fail means the
        // tool_result is already in history — nothing to track.
        if (m.status === 'running') {
            candidateRows.push({ msg: m, paired: false });
        }
    }

    for (const row of candidateRows) {
        if (row.paired) continue;
        const m = row.msg;
        const params = m.params || {};
        const result = (typeof m.result === 'object' && m.result) || null;
        const subagentSessionId = result?.session_id || null;

        const name = params.name || params.subagent_name || 'subagent';
        const task = params.task || '';
        const invocationId = m.id || null;

        const isUnnamed = (name === 'subagent');
        const key = (isUnnamed && invocationId)
            ? 'subagent-' + String(invocationId).slice(0, 8)
            : name;

        // Skip if already tracked (live SSE `tool_start` between
        // replaceMessages and this call would have already populated
        // the bar). The live entry has the authoritative start time
        // and any accumulated tool rows; don't overwrite.
        if (activeSubagents.value[key]) continue;

        // If the same key shows up twice in candidateRows (a named
        // subagent re-invoked while a previous invocation is still in
        // flight is impossible — the parent always awaits its named
        // session via the in-process registry — but unnamed concurrent
        // invocations land here), the later row wins, which matches
        // the live `trackSubagentStart` behaviour.
        const startedAt = m.ts ? Date.parse(m.ts) || Date.now() : Date.now();

        additions[key] = {
            status: 'running',
            tools: [],
            task,
            toolInvocationId: invocationId,
            displayName: name,
            startedAt,
            sessionId: subagentSessionId,
        };
    }

    if (Object.keys(additions).length === 0) return;

    activeSubagents.value = {
        ...activeSubagents.value,
        ...additions,
    };
}
