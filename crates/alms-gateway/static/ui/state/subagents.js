import { signal } from '../deps.js';
import { activeSessionId } from './sessions.js';

/**
 * Active subagents and their tool activity.
 * Shape: { [key]: { status, tools, task, toolInvocationId, displayName, startedAt, sessionId, liveActivity } }
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
 *
 * `liveActivity` is a bounded tail of the subagent's in-flight reasoning /
 * extended-thinking text (#1149). The parent's session stream forwards a
 * running subagent's `reasoning_delta` events tagged with `source_agent`;
 * `use-session-stream.js` suppresses those from the PARENT's main chat /
 * reasoning view (so subagent thinking never leaks into the parent run —
 * the #1170 / `get_run_reasoning` invariant) but tees them HERE via
 * `trackSubagentReasoning` so the SubagentBar panel can show what the
 * subagent is thinking in real time instead of sitting on "Waiting for
 * activity..." until the subagent finishes. Bounded to the most recent
 * `LIVE_ACTIVITY_MAX_CHARS` characters so a long thinking trace can't grow
 * the entry without bound. A delta that arrives before the entry exists
 * (#1183 startup race) is buffered in `pendingReasoning` and flushed into
 * `liveActivity` once the entry appears.
 */
export const activeSubagents = signal({});

/**
 * Max characters of subagent live-reasoning tail retained on a panel entry's
 * `liveActivity` field (#1149). The panel surfaces the agent's CURRENT
 * thinking, not its full transcript (that lives on the subagent's own session,
 * reachable via "View session"), so only the most recent slice is kept. This
 * also bounds memory for a long-running subagent whose reasoning stream never
 * stops.
 */
const LIVE_ACTIVITY_MAX_CHARS = 2000;

/**
 * Early-reasoning buffers for forwarded subagent `reasoning_delta` events that
 * arrive BEFORE the matching `activeSubagents` entry exists (#1183). A
 * background subagent's reasoning is routed onto the parent's session stream
 * independently of the `runtime_tx` drain that carries the entry-creating
 * `tool_start (invoke_agent)`, so its first deltas can beat the entry; without
 * buffering they would be dropped and the bar would stay blank.
 *
 * Keyed by the backend `source_agent` label. Each buffer is tail-bounded to
 * `LIVE_ACTIVITY_MAX_CHARS`, the map is LRU-capped at
 * `PENDING_REASONING_MAX_BUFFERS`, and a buffer older than
 * `PENDING_REASONING_MAX_AGE_MS` is discarded at flush time so a stale replayed
 * delta can't front-run a later re-invocation. Dropped on `trackSubagentEnd`
 * and `clearAllSubagents`. Values: { text, updatedAt }.
 */
const pendingReasoning = new Map();

/** Max number of distinct `source_agent` labels buffered at once (#1183). */
const PENDING_REASONING_MAX_BUFFERS = 8;

/** Max age of a pending early-reasoning buffer before it is treated as stale
 *  and discarded instead of flushed (#1183). The race window is sub-second;
 *  30s is a generous ceiling. */
const PENDING_REASONING_MAX_AGE_MS = 30000;

/**
 * Append an early (pre-entry) reasoning delta to the pending buffer for a
 * backend `source_agent` label (#1183). Tail-bounded and LRU-capped — see the
 * `pendingReasoning` doc above. Never creates an `activeSubagents` entry, so
 * the "a late delta must not resurrect a removed chip" invariant holds.
 */
function bufferEarlyReasoning(name, delta) {
    const now = Date.now();
    const existing = pendingReasoning.get(name);
    const fresh = existing
        && (now - existing.updatedAt) <= PENDING_REASONING_MAX_AGE_MS;
    let text = (fresh ? existing.text : '') + delta;
    if (text.length > LIVE_ACTIVITY_MAX_CHARS) {
        text = text.slice(text.length - LIVE_ACTIVITY_MAX_CHARS);
    }
    // Delete-then-set refreshes Map insertion order, so the cap eviction
    // below always drops the least-recently-written label.
    pendingReasoning.delete(name);
    pendingReasoning.set(name, { text, updatedAt: now });
    while (pendingReasoning.size > PENDING_REASONING_MAX_BUFFERS) {
        const oldest = pendingReasoning.keys().next().value;
        pendingReasoning.delete(oldest);
    }
}

/**
 * Remove and return the pending early-reasoning buffer for a label (#1183).
 * Returns '' when there is no buffer or the buffer is stale (older than
 * `PENDING_REASONING_MAX_AGE_MS`) — stale text is discarded, not flushed.
 */
function takePendingReasoning(name) {
    const buf = pendingReasoning.get(name);
    if (!buf) return '';
    pendingReasoning.delete(name);
    if (Date.now() - buf.updatedAt > PENDING_REASONING_MAX_AGE_MS) return '';
    return buf.text;
}

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

/** Terminal subagent statuses — entries in these states are scheduled for
 *  auto-removal from the bar after `REMOVE_DELAY_MS`. */
const TERMINAL_STATUSES = new Set(['done', 'fail']);

/**
 * Arm (or re-arm) the auto-remove timer for a single subagent entry.
 *
 * The entry stays visible for `REMOVE_DELAY_MS` so the operator can see the
 * final status (checkmark / X) before it disappears. Any pending timer for
 * the same key is cleared first so repeated calls don't stack timers.
 *
 * Centralised so `trackSubagentEnd` and the rehydrate-time invariant sweep
 * (`rearmTerminalRemoveTimers`) schedule removal identically. The callback
 * re-reads `activeSubagents.value` at fire time and only deletes the entry if
 * it is still the one we scheduled (a fresh re-invocation under the same key
 * re-arms its own timer via `trackSubagentStart`, which cancels this one).
 *
 * @param {string} key - The `activeSubagents` map key.
 */
function scheduleSubagentRemoval(key) {
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
 * Re-arm an auto-remove timer for every entry already in a terminal
 * (`done` / `fail`) status that has no pending timer (A1-4 / #1125).
 *
 * Invariant being restored: a terminal subagent chip must always have a
 * live auto-remove timer, otherwise it sticks on the bar until the next
 * session switch wipes the whole map. `clearAllSubagents()` (called at the
 * top of every session switch) cancels every pending timer; if a subagent
 * completion then lands via SSE in the same close→reopen window — or a chip
 * created by a live `tool_start` between `replaceMessages` and rehydrate has
 * its completion consumed while the map was momentarily cleared and is later
 * re-seeded into a terminal state — the entry can carry a terminal status
 * with no timer to remove it. Rehydrate (the single load chokepoint, run on
 * every reload and session-switch-back) sweeps the map and re-arms one, so
 * the stale chip self-heals on the next load instead of lingering.
 *
 * Running entries are untouched — they are removed by their own
 * `trackSubagentEnd` when the subagent completes.
 */
function rearmTerminalRemoveTimers() {
    for (const [key, info] of Object.entries(activeSubagents.value)) {
        if (TERMINAL_STATUSES.has(info.status) && !removeTimers[key]) {
            scheduleSubagentRemoval(key);
        }
    }
}

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
            // #1183: seed with any early reasoning buffered before this entry
            // existed. Named subagents flush here (label == key); unnamed ones
            // flush on the next resolved delta after the key migration.
            liveActivity: takePendingReasoning(key),
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
 * Resolve a forwarded `source_agent` label to a live `activeSubagents` entry,
 * migrating the entry key when the backend label differs from the start-time
 * key (#1149).
 *
 * Mirrors the key-resolution in `trackSubagentTool`: a forwarded subagent
 * event is labelled with the backend `source_agent`. For NAMED subagents that
 * equals the entry key directly. For UNNAMED subagents the entry was
 * registered under `subagent-{toolInvocationId_prefix}` at `tool_start` time
 * but the backend labels forwarded events with `subagent-{task_id_prefix}` (a
 * different id), so we fall back to the first running unnamed entry and migrate
 * it to the backend-assigned key (and its pending removal timer) so subsequent
 * forwarded events for the same subagent match directly.
 *
 * Returns the resolved key, or `null` when no matching entry exists (e.g. the
 * subagent already completed and its chip was auto-removed, or the entry has
 * not been created yet — see the #1183 startup race). A `null` result must
 * never resurrect / create a chip; `trackSubagentReasoning` responds to it by
 * buffering the delta (`pendingReasoning`), other callers no-op.
 *
 * @param {string} name - The backend `source_agent` label.
 * @returns {string|null} the resolved (possibly migrated) entry key.
 */
function resolveForwardedSubagentKey(name) {
    if (activeSubagents.value[name]) return name;
    if (name.startsWith('subagent-')) {
        for (const [key, info] of Object.entries(activeSubagents.value)) {
            if (key.startsWith('subagent-') && info.status === 'running') {
                // Migrate the entry to the backend-assigned key so future
                // forwarded events match directly.
                const { [key]: entry, ...rest } = activeSubagents.value;
                activeSubagents.value = { ...rest, [name]: entry };
                if (removeTimers[key]) {
                    clearTimeout(removeTimers[key]);
                    delete removeTimers[key];
                }
                return name;
            }
        }
    }
    return null;
}

/**
 * Append a chunk of a subagent's in-flight reasoning / extended-thinking text
 * to its panel entry's `liveActivity` tail (#1149).
 *
 * The parent's session SSE stream forwards a running subagent's
 * `reasoning_delta` events tagged with `source_agent`. `use-session-stream.js`
 * suppresses those from the PARENT's main chat / reasoning view (subagent
 * reasoning must never leak into the parent run's reasoning trace — the #1170 /
 * `get_run_reasoning` invariant) and instead tees them here so the SubagentBar
 * panel can render the subagent's live thinking rather than sitting on
 * "Waiting for activity..." until completion.
 *
 * Only the most recent `LIVE_ACTIVITY_MAX_CHARS` characters are retained: the
 * panel shows the subagent's CURRENT thinking, and the full transcript is
 * available via "View session". Keying / migration matches `trackSubagentTool`
 * so forwarded events from unnamed subagents (whose backend label differs from
 * the start-time key) land on the right entry.
 *
 * A delta that cannot be resolved to an entry is BUFFERED, not dropped (#1183,
 * see `pendingReasoning`): a background subagent's reasoning can beat the
 * `tool_start` that creates its entry. The buffer is flushed into
 * `liveActivity` (ahead of the current delta) once the label resolves or at
 * entry creation; buffering never creates an entry, so a gone subagent's late
 * delta can't resurrect a chip.
 *
 * @param {string} name - The backend `source_agent` label for the subagent.
 * @param {string} delta - The reasoning text chunk to append.
 */
export function trackSubagentReasoning(name, delta) {
    if (!delta) return;
    const key = resolveForwardedSubagentKey(name);
    if (!key) {
        // #1183: the entry doesn't exist yet (startup race) or is gone
        // (late replay). Buffer instead of dropping — flushed on resolve,
        // discarded when stale.
        bufferEarlyReasoning(name, delta);
        return;
    }
    const current = activeSubagents.value[key];
    if (!current) return;
    // #1183: flush any reasoning buffered before this entry existed.
    // `resolveForwardedSubagentKey` always resolves to `name` (directly or by
    // migrating the entry to it), so buffer and entry keys coincide; the
    // buffered text is older than `delta`, so it goes first.
    const buffered = takePendingReasoning(name);
    let next = (current.liveActivity || '') + buffered + delta;
    if (next.length > LIVE_ACTIVITY_MAX_CHARS) {
        next = next.slice(next.length - LIVE_ACTIVITY_MAX_CHARS);
    }
    activeSubagents.value = {
        ...activeSubagents.value,
        [key]: { ...current, liveActivity: next },
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
    // #1183: a completed subagent produces no more reasoning; drop its pending
    // buffer so it can't flush into a later re-invocation under the same name.
    // Unconditional (before the lookup) so a completion for an already-removed
    // chip still evicts.
    pendingReasoning.delete(name);
    const key = resolveSubagentKey(name, toolInvocationId, subagentSessionId);
    if (!key) return;
    // The entry key may have been migrated to the backend label (which is
    // what buffers are keyed by) — evict under that label too.
    pendingReasoning.delete(key);
    const current = activeSubagents.value[key];
    if (!current) return;
    activeSubagents.value = {
        ...activeSubagents.value,
        [key]: { ...current, status },
    };

    // Schedule auto-removal after a brief delay.
    scheduleSubagentRemoval(key);
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
    // #1183: drop early-reasoning buffers too — a session switch must never
    // flush a previous session's buffered subagent reasoning into the next.
    pendingReasoning.clear();
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
    // A1-4 / #1125: restore the "terminal chip always has a live auto-remove
    // timer" invariant before doing anything else. A completion that landed in
    // the close→reopen window of a session switch (after `clearAllSubagents`
    // cancelled the timers) can leave a `done` / `fail` entry with no timer,
    // which would otherwise stick until the next switch. Runs unconditionally —
    // even on the empty-history early return below — so a stale terminal chip
    // carried over from a previous session self-heals on the next load.
    rearmTerminalRemoveTimers();

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
    // A1-1 / #1125: tool_invocation_id -> subagent_session_id, harvested from
    // the durable `subagent_started` lifecycle markers the backend persists on
    // FOREGROUND invoke_agent calls. A foreground subagent that is still
    // mid-run leaves its parent's invoke_agent tool row with `status:
    // 'running'` and NO result yet (the parent is blocked), so the result's
    // `session_id` is unavailable — the chip would otherwise rehydrate with
    // `sessionId: null` and a dead "View session" button. We recover the
    // session id from this marker, keyed by the same `tool_invocation_id` the
    // live `subagent_started` SSE path uses, and attach it to the pending
    // foreground chip below.
    const startedSessionByInvocation = new Map(); // tool_invocation_id -> subagent_session_id
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

        if (m.type === 'subagent_started') {
            // A1-1 / #1125: harvest the recovered foreground session id,
            // keyed by tool_invocation_id (always present; subagent_name is
            // omitted for ephemeral/unnamed invocations so we never key on
            // it). Consumed in the second pass to fill the pending chip's
            // sessionId. This record carries no chip of its own — it only
            // supplies the session id the still-running invoke_agent row
            // lacks. It also intentionally short-circuits the stray
            // "Subagent started." notification bubble that the generic
            // synthetic fallback in history.js used to render on reload.
            if (m.toolInvocationId && m.subagentSessionId) {
                startedSessionByInvocation.set(m.toolInvocationId, m.subagentSessionId);
            }
            continue;
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
        const name = params.name || params.subagent_name || 'subagent';
        const task = params.task || '';
        const invocationId = m.id || null;

        // Prefer the session id from the invoke_agent result (present for
        // background rows and for completed foreground rows). For a FOREGROUND
        // subagent still mid-run the parent's row has no result yet, so fall
        // back to the durable `subagent_started` marker harvested above,
        // resolved by tool_invocation_id (A1-1 / #1125). This is what makes
        // the "View session" button live again on reload while a foreground
        // subagent is in flight.
        const subagentSessionId = (result?.session_id)
            || (invocationId ? (startedSessionByInvocation.get(invocationId) || null) : null);

        const isUnnamed = (name === 'subagent');
        const key = (isUnnamed && invocationId)
            ? 'subagent-' + String(invocationId).slice(0, 8)
            : name;

        // Skip if already tracked (live SSE `tool_start` between
        // replaceMessages and this call would have already populated
        // the bar). The live entry has the authoritative start time
        // and any accumulated tool rows; don't overwrite.
        //
        // A1-1 / #1125: one exception — if the live entry exists but has
        // no sessionId yet (the live `tool_start` fired without an inline
        // `subagent_session_id`, which is the common case for foreground),
        // and we recovered one from the `subagent_started` marker, attach
        // it idempotently so the "View session" button goes live. Resolve
        // by tool_invocation_id to avoid the literal-"subagent" first-match
        // fallback when concurrent unnamed subagents are in flight.
        if (activeSubagents.value[key]) {
            const existing = activeSubagents.value[key];
            if (!existing.sessionId && subagentSessionId) {
                setSubagentSessionId(name, subagentSessionId, invocationId);
            }
            continue;
        }

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
            // #1183: same early-reasoning flush as `trackSubagentStart` —
            // forwarded deltas can beat this rehydrate pass on a reload.
            liveActivity: takePendingReasoning(key),
        };
    }

    if (Object.keys(additions).length === 0) return;

    activeSubagents.value = {
        ...activeSubagents.value,
        ...additions,
    };
}
