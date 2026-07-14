import { signal } from '../deps.js';
import { activeSessionId } from './sessions.js';
import { clearCancelConfirmForSession, dismissSubagentCancel } from './subagent-cancel.js';

/**
 * Active subagents and their live status, backing the Subagent status bar.
 * Shape: { [key]: { status, task, toolInvocationId, displayName, startedAt,
 *                   sessionId, activity, toolsUsed, countedToolIds } }
 *
 * Keys are unique identifiers for each subagent invocation:
 *   - Named subagents use the agent name as the key (e.g. "reviewer").
 *   - Unnamed subagents use "subagent-{toolInvocationId_prefix}" so that
 *     concurrent unnamed invocations each get their own slot.
 *
 * `displayName` is the human-friendly label shown on the status-bar chip.
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
 * invoke_agent result or the subagent_started/subagent_completed events.
 * Clicking the chip navigates directly to this session.
 *
 * `activity` is the subagent's MOST RECENT coarse activity signal:
 * `{ kind, tool }` where `kind` is one of `reasoning` / `writing` /
 * `tool_start` / `tool_end` and `tool` is the tool name (only for
 * `tool_start`), or `null` before the first signal. Driven by the backend's
 * ephemeral `subagent_activity` SSE events (tagged `source_agent`) via
 * `trackSubagentActivity`. The bar renders it as a concise status label
 * ("Reasoning…", "Using {tool}", "Writing…") — the subagent's actual
 * reasoning/token text is deliberately NOT streamed to the parent anymore;
 * the full transcript lives on the subagent's own session (#1184), reached
 * by clicking the chip. A signal that arrives before the entry exists (the
 * #1183 startup race) is held in `pendingActivity` and applied once the
 * entry appears.
 *
 * `toolsUsed` counts observed `tool_start` signals — once per DISTINCT tool
 * invocation id (`countedToolIds`, #1190) — feeding the completion card's
 * tool count. Counting distinct ids is what makes the count immune to the
 * attach-time snapshot replay (same id re-sent per reconnect) without
 * undercounting parallel same-tool invocations (distinct ids).
 */
export const activeSubagents = signal({});

/**
 * Latest early activity signal per backend `source_agent` label, for signals
 * that arrive BEFORE the matching `activeSubagents` entry exists (#1183
 * startup race: a background subagent's stream is routed independently of the
 * `runtime_tx` drain that carries the entry-creating `tool_start
 * (invoke_agent)`, so its first signal can beat the entry).
 *
 * Only the LATEST signal per label is kept (status is a point-in-time value,
 * unlike the old text buffer this replaces). The map is LRU-capped at
 * `PENDING_ACTIVITY_MAX_LABELS`, and an entry older than
 * `PENDING_ACTIVITY_MAX_AGE_MS` is discarded at take time so a stale replayed
 * signal can't front-run a later re-invocation. Dropped on `trackSubagentEnd`
 * and `clearAllSubagents`. Values:
 * { kind, tool, toolInvocationId, parentToolInvocationId, updatedAt }.
 */
const pendingActivity = new Map();

/** Max number of distinct `source_agent` labels buffered at once. */
const PENDING_ACTIVITY_MAX_LABELS = 8;

/** Max age of a pending early activity signal before it is treated as stale
 *  and discarded instead of applied. The race window is sub-second; 30s is a
 *  generous ceiling. */
const PENDING_ACTIVITY_MAX_AGE_MS = 30000;

/**
 * Record an early (pre-entry) activity signal for a backend `source_agent`
 * label. Keeps only the latest signal per label; LRU-capped — see the
 * `pendingActivity` doc above. Never creates an `activeSubagents` entry, so
 * the "a late signal must not resurrect a removed chip" invariant holds.
 */
function bufferEarlyActivity(name, kind, tool, toolInvocationId, parentToolInvocationId) {
    // Delete-then-set refreshes Map insertion order, so the cap eviction
    // below always drops the least-recently-written label.
    pendingActivity.delete(name);
    pendingActivity.set(name, {
        kind,
        tool: tool || null,
        toolInvocationId: toolInvocationId || null,
        parentToolInvocationId: parentToolInvocationId || null,
        updatedAt: Date.now(),
    });
    while (pendingActivity.size > PENDING_ACTIVITY_MAX_LABELS) {
        const oldest = pendingActivity.keys().next().value;
        pendingActivity.delete(oldest);
    }
}

/**
 * Remove and return the pending early activity signal for a label. Returns
 * `null` when there is none or it is stale (older than
 * `PENDING_ACTIVITY_MAX_AGE_MS`) — stale signals are discarded, not applied.
 */
function takePendingActivity(name) {
    const buf = pendingActivity.get(name);
    if (!buf) return null;
    pendingActivity.delete(name);
    if (Date.now() - buf.updatedAt > PENDING_ACTIVITY_MAX_AGE_MS) return null;
    return {
        kind: buf.kind,
        tool: buf.tool,
        toolInvocationId: buf.toolInvocationId || null,
        parentToolInvocationId: buf.parentToolInvocationId || null,
    };
}

/**
 * Consume the pending early activity signal for a NEW entry keyed by `key`.
 *
 * Named subagents: the backend label equals the entry key, so the direct
 * lookup suffices. UNNAMED subagents are the important case (Codex P2 on PR
 * #1189): the entry key derives from the parent's invoke_agent
 * tool_invocation_id (`subagent-{toolInvocationId_prefix}`) but the backend
 * labels signals with the task id (`subagent-{task_id_prefix}`) — a DIFFERENT
 * id — so an early signal is buffered under a label the direct lookup can
 * never see. Pre-dedup that didn't matter (the next delta re-resolved and
 * migrated the key), but the backend now collapses consecutive same-kind
 * signals, so during a long initial reasoning / tool phase the buffered
 * signal is the ONLY one — without this fallback the chip would sit on
 * "Starting…" for the whole phase.
 *
 * Resolution order (mirrors `resolveForwardedSubagentKey`, #1190):
 *   1. IDENTITY-EXACT: a buffer whose `parentToolInvocationId` equals the
 *      new entry's parent invoke_agent invocation id. With concurrent
 *      unnamed subagents this is the only consumption that cannot seed one
 *      subagent's status onto another's chip.
 *   2. Direct label hit (named subagents: label === key).
 *   3. For an unnamed key, the OLDEST unnamed-labelled (`subagent-*`) buffer
 *      (skipping stale ones) — legacy best-effort first-match for
 *      correlator-less buffers, the same ambiguity class as
 *      `resolveForwardedSubagentKey`'s running-entry fallback; status is
 *      self-correcting on the next live signal. The entry key is NOT
 *      migrated here; the next live signal still performs the normal key
 *      migration. Correlator-TAGGED buffers are excluded: they belong to a
 *      specific invocation, and consuming one for a different entry would
 *      seed the wrong chip — the exact cross-attach this fix removes.
 *
 * @param {string} key - The new entry's `activeSubagents` key.
 * @param {string|null} [parentToolInvocationId] - The new entry's parent
 *   invoke_agent tool-invocation-id (the buffer correlator).
 * @returns {{kind: string, tool: string|null, toolInvocationId: string|null,
 *            parentToolInvocationId: string|null}|null}
 */
function takePendingActivityForKey(key, parentToolInvocationId) {
    if (parentToolInvocationId) {
        for (const [label, buf] of pendingActivity) {
            if (buf.parentToolInvocationId === parentToolInvocationId) {
                return takePendingActivity(label);
            }
        }
    }
    // A correlator-TAGGED buffer under this label belongs to a specific
    // invocation. If it were THIS entry's, the exact scan above would have
    // consumed it — so reaching here means it is a DIFFERENT invocation's
    // (e.g. a prior same-named run within the pending window): leave it for
    // the TTL / completion eviction rather than seed the wrong chip with
    // stale status (#1190 r4, Codex).
    if (!pendingActivity.get(key)?.parentToolInvocationId) {
        const direct = takePendingActivity(key);
        if (direct) return direct;
    }
    if (!key.startsWith('subagent-')) return null;
    // Iterate over a snapshot: takePendingActivity deletes while we scan.
    for (const label of [...pendingActivity.keys()]) {
        if (!label.startsWith('subagent-')) continue;
        if (pendingActivity.get(label)?.parentToolInvocationId) continue;
        const buf = takePendingActivity(label);
        if (buf) return buf;
    }
    return null;
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
 *  auto-removal from the bar after `REMOVE_DELAY_MS`. Matches the status
 *  strings `subagent_completed` serializes (notifications.rs): a cancelled
 *  background subagent arrives as `'cancelled'` and must be treated as
 *  terminal like done/fail — otherwise the chip falls through to its stale
 *  activity label ("Writing…" / "Starting…") until auto-removal. */
const TERMINAL_STATUSES = new Set(['done', 'fail', 'cancelled']);

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
        const { [key]: removed, ...rest } = activeSubagents.value;
        // Codex P2 (PR #1192): removing a chip must also drop an armed
        // cancel-confirm targeting its session — named subagents reuse the
        // session id, so a surviving confirm would pre-arm the next
        // invocation's chip.
        if (removed) {
            clearCancelConfirmForSession(removed.sessionId);
        }
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
 * Derive a new entry's activity seed from a consumed early-signal buffer
 * (#1183): the display `activity` ({kind, tool} — the buffer's invocation id
 * is deliberately NOT part of the rendered activity), the initial `toolsUsed`
 * count, and the set of already-counted tool invocation ids. Seeding the id
 * into `countedToolIds` is what keeps a later snapshot replay of the SAME
 * in-progress tool_start (attach-time replay, #1190) from double counting a
 * buffered tool_start that was already counted here at entry creation.
 *
 * @param {{kind: string, tool: string|null, toolInvocationId: string|null,
 *          parentToolInvocationId: string|null}|null} buffered
 * @returns {{activity: {kind: string, tool: string|null}|null,
 *            toolsUsed: number, countedToolIds: Set<string>}}
 */
function seedFromBufferedActivity(buffered) {
    if (!buffered) {
        return { activity: null, toolsUsed: 0, countedToolIds: new Set() };
    }
    const isToolStart = buffered.kind === 'tool_start';
    return {
        activity: { kind: buffered.kind, tool: buffered.tool },
        toolsUsed: isToolStart ? 1 : 0,
        countedToolIds: (isToolStart && buffered.toolInvocationId)
            ? new Set([buffered.toolInvocationId])
            : new Set(),
    };
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
    // #1183: seed with any activity signal that raced ahead of this entry —
    // including an unnamed subagent's backend-labelled buffer (see
    // `takePendingActivityForKey`; the backend dedup means it may be the only
    // signal for a long phase). A buffered tool_start counts toward
    // `toolsUsed` exactly like the live path in `trackSubagentActivity`, and
    // its invocation id seeds `countedToolIds` so a snapshot replay of the
    // same in-progress tool can't recount it (#1190).
    const seed = seedFromBufferedActivity(
        takePendingActivityForKey(key, toolInvocationId || null),
    );
    activeSubagents.value = {
        ...activeSubagents.value,
        [key]: {
            status: 'running',
            task: task || '',
            toolInvocationId: toolInvocationId || null,
            displayName: name,
            startedAt: Date.now(),
            sessionId: null,
            activity: seed.activity,
            toolsUsed: seed.toolsUsed,
            countedToolIds: seed.countedToolIds,
        },
    };
}

/**
 * Sentinel returned by `resolveForwardedSubagentKey` when a signal's
 * correlator resolves to a TERMINAL entry: the signal belongs to a finished
 * invocation and must be DISCARDED — not applied, and crucially not buffered
 * (see the resolver's precedence doc, #1190 r4).
 */
const RESOLVE_DROP = Symbol('drop-stale-subagent-signal');

/**
 * Resolve a forwarded `source_agent` label to a live `activeSubagents` entry,
 * migrating the entry key when the backend label differs from the start-time
 * key (#1149).
 *
 * A forwarded subagent event is labelled with the backend `source_agent`. For
 * NAMED subagents that equals the entry key directly. For UNNAMED subagents
 * the entry was registered under `subagent-{toolInvocationId_prefix}` at
 * `tool_start` time but the backend labels forwarded events with
 * `subagent-{task_id_prefix}` (a different id).
 *
 * Resolution precedence for CORRELATOR-TAGGED signals (#1190 r3/r4 — make
 * every step identity-exact; the ambiguous first-match loop is never used):
 *   1. Entry whose stored `toolInvocationId` equals the correlator, RUNNING
 *      → resolve to it (migrating to the backend label so label-keyed
 *      operations — buffer eviction, later legacy signals — stay
 *      consistent). This is the only correct resolver when 2+ UNNAMED
 *      subagents run concurrently: the correlator-less first-match loop
 *      below could migrate subagent B's status onto subagent A's chip, and
 *      the migration STICKS (Codex P2, r3).
 *   2. Same entry but TERMINAL → return [`RESOLVE_DROP`]: the signal belongs
 *      to a FINISHED invoke_agent invocation. It must be dropped, NOT
 *      buffered (Codex P2, r4): a name-keyed buffer would be consumed by the
 *      NEXT invocation of the same-named subagent inside the 30s pending
 *      window (`takePendingActivityForKey`'s direct-name lookup at entry
 *      creation), seeding the fresh chip with the completed run's stale
 *      activity/tool count. Dropping (rather than resolving) also keeps a
 *      terminal entry from being migrated, which would orphan its pending
 *      auto-remove timer.
 *   3. No entry with that correlator at all → check the entry under the
 *      EXACT label (only the first-match LOOP is ambiguous; the label hit
 *      itself is exact — Tim, r4), but gated on its stored id:
 *        - stored `toolInvocationId` ABSENT → take the label hit. This is
 *          the nonstandard creation path the fallback exists for (an entry
 *          created without an id can never id-match), which would otherwise
 *          buffer every signal forever and pin the chip on "Starting…".
 *        - stored id PRESENT → [`RESOLVE_DROP`] (Codex P2, r5). It is
 *          necessarily DIFFERENT (an equal id would have id-matched in
 *          step 1), so the signal's invocation is gone or replaced — e.g. a
 *          same-named re-invocation overwrote the entry with a fresh id,
 *          and this is a delayed straggler from the finished run. Taking
 *          the label hit would pin the stale activity/tool count onto the
 *          fresh chip: the exact stale-signal class the correlator exists
 *          to prevent.
 *   4. Otherwise → `null` (the caller buffers, #1183).
 *
 * Correlator-LESS signals (pre-#1190 backends) keep the legacy order:
 * direct label hit, then the first running `subagent-*` entry + migration —
 * ambiguous under concurrency by construction, exactly like
 * `resolveSubagentKey`'s name fallback.
 *
 * Returns the resolved key; `null` when no matching entry exists (the entry
 * has not been created yet — see the #1183 startup race — and the caller
 * buffers; a `null` must never resurrect / create a chip); or
 * [`RESOLVE_DROP`] when the signal provably belongs to a finished invocation
 * and must be discarded outright.
 *
 * @param {string} name - The backend `source_agent` label.
 * @param {string|null} [parentToolInvocationId] - The parent invoke_agent
 *   tool-invocation-id carried by the signal (absent on legacy backends).
 * @returns {string|symbol|null} the resolved (possibly migrated) entry key,
 *   `null` (buffer), or `RESOLVE_DROP` (discard).
 */
function resolveForwardedSubagentKey(name, parentToolInvocationId) {
    if (parentToolInvocationId) {
        const exact = findSubagentByToolInvocationId(parentToolInvocationId);
        if (exact) {
            if (activeSubagents.value[exact]?.status === 'running') {
                return migrateEntryToBackendLabel(exact, name);
            }
            // Terminal id-match: a straggler for a FINISHED invocation.
            return RESOLVE_DROP;
        }
        const labelEntry = activeSubagents.value[name];
        if (labelEntry) {
            // Stored correlator ABSENT → nonstandard creation path: the
            // exact label hit this fallback exists for (r4). A stored id is
            // necessarily DIFFERENT here (equal would have id-matched
            // above): the signal's invocation is gone or replaced by a
            // same-named re-invocation — a straggler. Drop it, or it would
            // pin the finished run's stale activity/tool count onto the
            // fresh chip (Codex P2, r5).
            if (!labelEntry.toolInvocationId) return name;
            return RESOLVE_DROP;
        }
        // NEVER the ambiguous first-match loop for a tagged signal —
        // attaching it to some other subagent's chip is the r3 bug.
        return null;
    }
    if (activeSubagents.value[name]) return name;
    if (name.startsWith('subagent-')) {
        for (const [key, info] of Object.entries(activeSubagents.value)) {
            if (key.startsWith('subagent-') && info.status === 'running') {
                return migrateEntryToBackendLabel(key, name);
            }
        }
    }
    return null;
}

/**
 * Move a resolved entry to the backend-assigned `subagent-*` label so future
 * label-keyed lookups match directly, CLEARING any pending removal timer
 * under the old key (it is not re-armed under the new one). Safe only for
 * RUNNING entries — they never have a timer — which is why every caller
 * filters on `status === 'running'` before migrating: migrating a terminal
 * entry would silently cancel its auto-removal and strand the chip.
 * No-op when the entry already lives under that key or when the label is not
 * an unnamed-style backend label (named subagents: label === key always).
 *
 * @param {string} key - The entry's current `activeSubagents` key.
 * @param {string} name - The backend `source_agent` label.
 * @returns {string} the entry's key after (possible) migration.
 */
function migrateEntryToBackendLabel(key, name) {
    if (key === name || !name.startsWith('subagent-')) return key;
    const { [key]: entry, ...rest } = activeSubagents.value;
    activeSubagents.value = { ...rest, [name]: entry };
    if (removeTimers[key]) {
        clearTimeout(removeTimers[key]);
        delete removeTimers[key];
    }
    return name;
}

/**
 * Record a subagent's most recent coarse activity signal on its status-bar
 * entry.
 *
 * The backend emits ephemeral `subagent_activity` SSE events (tagged
 * `source_agent`) describing WHAT KIND of thing the subagent is doing —
 * `reasoning`, `writing`, `tool_start` (with the tool name) or `tool_end` —
 * instead of streaming the subagent's actual reasoning/token content to the
 * parent. The Subagent status bar renders the latest signal as a concise
 * label ("Reasoning…", "Using {tool}", "Writing…"); the full transcript
 * streams to the subagent's OWN session (#1184), reached by clicking the
 * chip.
 *
 * Keying / migration uses `resolveForwardedSubagentKey`: identity-exact via
 * the signal's `parentToolInvocationId` correlator when present (#1190 —
 * required for correctness with concurrent unnamed subagents), label /
 * first-match fallback for legacy correlator-less signals.
 *
 * A signal that cannot be resolved to an entry is BUFFERED, not dropped
 * (#1183, see `pendingActivity`): a background subagent's first signal can
 * beat the `tool_start` that creates its entry — and because the backend
 * deduplicates consecutive same-kind signals, a dropped signal might not be
 * re-sent until the next kind transition. Buffering never creates an entry,
 * so a gone subagent's late signal can't resurrect a chip.
 *
 * `tool_start` signals increment the entry's `toolsUsed` counter — which
 * feeds the completion card's tool count — once per DISTINCT tool invocation
 * id (idempotency, #1190): an attach-time snapshot replay
 * (`attach_session_stream` re-sends the CURRENT in-progress tool_start on
 * every reconnect) carries the SAME id as the live signal and is recognised
 * instead of recounted, while parallel invocations of the same tool
 * (`run_tool_calls_parallel` starts non-conflicting calls concurrently — two
 * same-tool tool_starts with NO interposed tool_end) carry distinct ids and
 * each count. A name-based heuristic cannot distinguish those two cases;
 * the id can. Signals from a legacy backend without an id fall back to
 * counting every tool_start (the pre-#1190 behaviour).
 *
 * @param {string} name - The backend `source_agent` label for the subagent.
 * @param {string} kind - Activity kind: reasoning|writing|tool_start|tool_end.
 * @param {string|null} [tool] - Tool name (only for kind === 'tool_start').
 * @param {string|null} [toolInvocationId] - The subagent's own (CHILD) tool
 *   invocation id (tool kinds only) — the `toolsUsed` idempotency key.
 * @param {string|null} [parentToolInvocationId] - The PARENT invoke_agent
 *   tool-invocation-id — the chip-resolution correlator (#1190).
 */
export function trackSubagentActivity(name, kind, tool, toolInvocationId, parentToolInvocationId) {
    if (!kind) return;
    const key = resolveForwardedSubagentKey(name, parentToolInvocationId);
    // The correlator matched a TERMINAL entry: a straggler from a finished
    // invocation. Discard it — buffering would seed a same-named
    // re-invocation's fresh chip with stale status (#1190 r4).
    if (key === RESOLVE_DROP) return;
    if (!key) {
        // #1183: the entry doesn't exist yet (startup race) or is gone
        // (late replay). Buffer instead of dropping — applied on entry
        // creation, discarded when stale.
        bufferEarlyActivity(name, kind, tool, toolInvocationId, parentToolInvocationId);
        return;
    }
    const current = activeSubagents.value[key];
    if (!current) return;
    // The entry is live now: this signal supersedes any buffered one.
    pendingActivity.delete(name);
    // Count DISTINCT tool invocation ids (#1190). Defensive `instanceof`:
    // entries created by this module always carry the set, but re-seeded /
    // legacy-shaped entries may not.
    const counted = current.countedToolIds instanceof Set
        ? current.countedToolIds
        : new Set();
    let toolsUsed = current.toolsUsed || 0;
    let nextCounted = counted;
    if (kind === 'tool_start') {
        if (toolInvocationId) {
            if (!counted.has(toolInvocationId)) {
                nextCounted = new Set(counted);
                nextCounted.add(toolInvocationId);
                toolsUsed += 1;
            }
        } else {
            // Legacy backend without ids: count every tool_start.
            toolsUsed += 1;
        }
    }
    activeSubagents.value = {
        ...activeSubagents.value,
        [key]: {
            ...current,
            activity: { kind, tool: tool || null },
            toolsUsed,
            countedToolIds: nextCounted,
        },
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
 * @param {string} status - Terminal status ('done' | 'fail' | 'cancelled').
 * @param {string} [toolInvocationId] - The parent's invoke_agent tool_invocation_id.
 * @param {string} [subagentSessionId] - The completing subagent's session UUID.
 */
export function trackSubagentEnd(name, status, toolInvocationId, subagentSessionId) {
    // #1183: a completed subagent produces no more activity; drop its pending
    // signal so it can't apply to a later re-invocation under the same name.
    // Unconditional (before the lookup) so a completion for an already-removed
    // chip still evicts.
    pendingActivity.delete(name);
    // Codex P2 (PR #1192): a terminal subagent must never leave an armed
    // cancel-confirm behind. Named subagents reuse the SAME session id
    // across re-invocations, so a confirm armed on this invocation would
    // otherwise pre-arm the NEXT invocation's chip inside the auto-revert
    // window — its Yes button would fire with no confirming first click.
    // Cleared by the event's session id even when the chip is already gone
    // (mirrors the unconditional pendingActivity eviction above), and again
    // below by the resolved entry's stored session id (the two can differ
    // when the event carries no session id).
    clearCancelConfirmForSession(subagentSessionId);
    const key = resolveSubagentKey(name, toolInvocationId, subagentSessionId);
    if (!key) return;
    // The entry key may have been migrated to the backend label (which is
    // what pending signals are keyed by) — evict under that label too.
    pendingActivity.delete(key);
    const current = activeSubagents.value[key];
    if (!current) return;
    clearCancelConfirmForSession(current.sessionId);
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
        // does. Without this, the operator clicks a subagent chip,
        // lands on the subagent transcript, then reloads
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
        clearRuns: runsMod.clearRuns,
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
    deps.clearRuns();
    deps.selectedRunId.value = null;
    deps.replaceMessages([], targetSessionId);
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
    // #1183: drop early activity signals too — a session switch must never
    // apply a previous session's buffered subagent status to the next.
    pendingActivity.clear();
    // Codex P2 (PR #1192): a session switch also dismisses any armed
    // cancel-confirm — the armed chip (or drilled-down view) is no longer
    // on screen, and the pending state must not leak into the next view.
    dismissSubagentCancel();
    activeSubagents.value = {};
}

/**
 * Rehydrate `activeSubagents` from a freshly-loaded chat-history snapshot.
 *
 * Called by `loadSession()` after `replaceMessages(...)` so that page
 * reloads and session switches re-create the Subagent status bar chips for any
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
 *   that get a status-bar chip.
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
 * additive so a status-bar entry created by a live SSE `tool_start`
 * that fired between `replaceMessages` and this call is not stomped.
 * The session-switch path explicitly calls `clearAllSubagents()`
 * before `loadSession()` (see `doSessionSwitch` above), so it always
 * starts from an empty bar; the page-reload path goes through
 * `boot()` which also starts from an empty bar.
 *
 * The subagent's activity status (and `toolsUsed` count) is not
 * reconstructed here — status is a live, point-in-time signal, and the
 * detailed history lives on the subagent's own session (one click away).
 * The chip re-renders correctly with just the metadata available on the
 * parent's `invoke_agent` tool row; the next live `subagent_activity`
 * signal repopulates the status label as activity continues post-reload.
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

        // #1183: same early-signal application as `trackSubagentStart` —
        // forwarded activity signals can beat this rehydrate pass on a
        // reload (including an unnamed subagent's backend-labelled buffer;
        // a buffered tool_start counts toward `toolsUsed` and seeds
        // `countedToolIds` so a snapshot replay can't recount it, #1190).
        const seed = seedFromBufferedActivity(
            takePendingActivityForKey(key, invocationId || null),
        );
        additions[key] = {
            status: 'running',
            task,
            toolInvocationId: invocationId,
            displayName: name,
            startedAt,
            sessionId: subagentSessionId,
            activity: seed.activity,
            toolsUsed: seed.toolsUsed,
            countedToolIds: seed.countedToolIds,
        };
    }

    if (Object.keys(additions).length === 0) return;

    activeSubagents.value = {
        ...activeSubagents.value,
        ...additions,
    };
}
