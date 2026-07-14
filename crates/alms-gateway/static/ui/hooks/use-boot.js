import { fetchSettings } from '../api/settings.js';
import { listSessions, createSession, getSessionMessages } from '../api/sessions.js';
import { agents, activeAgentId, replaceAgents } from '../state/agents.js';
import {
    sessions,
    activeSessionId,
    expandedAgentId,
    replaceSessionScopes,
    resetScopedEntities,
} from '../state/sessions.js';
import { selectedRunId, replaceRuns } from '../state/runs.js';
import { serverDefaults } from '../state/settings.js';
import { replaceMessages } from '../state/chat-actions.js';
import { messageQueue, replaceActivitySnapshot } from '../state/queue.js';
import { wsFiles } from '../state/workspace.js';
import { auditEvents } from '../state/audit.js';
import { agentSwitchLoading } from '../state/loading.js';
import { openSessionStream, closeSessionStream } from './use-session-stream.js';
import { openAgentEventsStream, closeAgentEventsStream } from './use-agent-events.js';
import { bumpSelectGeneration } from '../state/select-generation.js';
import { loadSession } from '../utils/load-session.js';
import { clearAllSubagents } from '../state/subagents.js';
import { defaultExpandedAgent, filterChatSessions } from '../utils/sidebar-grouping.js';

const AGENT_KEY = 'alms_active_agent';

/**
 * Generation counter for loadAgentSessions() concurrency guard.
 * Bumped at the start of each loadAgentSessions() call so that
 * rapid agent switches (A -> B -> A) discard stale fetches.
 */
let switchGeneration = 0;

function sessionStorageKey(agentId) {
    return `alms_active_session_${agentId}`;
}

export function saveActiveSession(agentId, sessionId) {
    if (agentId && sessionId) {
        localStorage.setItem(sessionStorageKey(agentId), sessionId);
    }
}

function loadActiveSession(agentId, agentSessions, preferred) {
    // Caller-supplied preferred session wins (cross-agent navigate
    // uses this so the auto-switched agent lands on the targeted
    // session rather than its most-recent one). Falls back to
    // localStorage > first session if `preferred` isn't found.
    if (preferred) {
        const match = agentSessions.find(s => s.id === preferred);
        if (match) return match;
    }
    const stored = localStorage.getItem(sessionStorageKey(agentId));
    if (stored) {
        const match = agentSessions.find(s => s.id === stored);
        if (match) return match;
    }
    return agentSessions[0] || null;
}

/**
 * Resolve a stored session id to a usable id, even when the session is
 * intentionally hidden from `GET /sessions` (subagent / job / episodic
 * sessions — see `is_internal_context_id` in
 * `crates/alms-gateway/src/runs/mod.rs`). Returns the stored id if a
 * lightweight existence check succeeds, otherwise `null` so the caller
 * falls back to the agent's visible session list. (#1045)
 *
 * Subagent sessions are reached by clicking a Subagent status bar chip,
 * the SubagentCompletionCard, or the Runs tab — all paths persist the
 * subagent session id in `alms_active_session_<agentId>` via
 * `saveActiveSession`. Without this resolver, a page reload that finds
 * a subagent (or job / episodic) session id in localStorage falls
 * straight through to `agentSessions[0]` because the id isn't in the
 * per-agent visible list, and the operator lands on the parent's first
 * chat session with the subagent transcript silently dropped. When the
 * parent agent has no chat sessions at all, `loadAgentSessions` enters
 * the `agentSessions.length === 0` branch and creates an empty new chat
 * — which is the exact "empty chat pane" the issue reports.
 *
 * The existence probe uses `GET /sessions/{id}/messages` because:
 * - It is the same endpoint `loadSession` calls on success, so the
 *   payload becomes warm in the browser's HTTP cache for the subsequent
 *   load (no `cache-control: no-store` on this endpoint, unlike the
 *   embedded UI assets — but this is still cheap regardless).
 * - There is no dedicated `GET /sessions/{id}` metadata endpoint to
 *   probe more cheaply (added complexity not justified for one extra
 *   round-trip on boot).
 *
 * A 404 (deleted or never-existed session) returns `null`, which falls
 * back to the previous behaviour: pick the first visible session, or
 * create one if none exist. Any other error (network / 5xx) is also
 * treated as `null` so a transient backend hiccup never strands the
 * operator on an error chat-pane — they get the agent's first session
 * instead and can retry navigation manually.
 *
 * @param {string} agentId
 * @param {Array} agentSessions Visible session list (post-filter).
 * @returns {Promise<string|null>} The resolved session id, or null.
 */
async function resolveStoredSessionId(agentId, agentSessions) {
    const stored = localStorage.getItem(sessionStorageKey(agentId));
    if (!stored) return null;
    // Already covered by the standard `loadActiveSession` path.
    if (agentSessions.some(s => s.id === stored)) return null;
    try {
        await getSessionMessages(stored);
        return stored;
    } catch (err) {
        // 404 -> session is genuinely gone; clear the stale pointer so
        // future reloads stop probing it. Other errors (network blip,
        // 5xx) leave the pointer in place; the operator can navigate
        // manually and the value may resolve on a later boot.
        // `apiFetch` (api/client.js) attaches `status` to the thrown
        // error envelope, so checking that field directly is sufficient.
        if (err && err.status === 404) {
            localStorage.removeItem(sessionStorageKey(agentId));
        }
        return null;
    }
}

/**
 * Boot sequence: load settings, agents, sessions, and chat history.
 */
export async function boot() {
    try {
        const data = await fetchSettings();
        serverDefaults.value = data;
        replaceAgents(data.agents || []);

        // Determine active agent: localStorage > default > first
        const saved = localStorage.getItem(AGENT_KEY);
        const defaultAgent = agents.value.find(a => a.is_default);
        const firstAgent = agents.value[0];
        const agent = agents.value.find(a => a.id === saved) || defaultAgent || firstAgent;

        if (agent) {
            activeAgentId.value = agent.id;
            // Sidebar accordion default-expands the active agent at
            // boot. Subsequent switchAgent() calls re-sync this; the
            // header click handler in session-list.js can flip it to
            // null on a same-agent click without altering activeAgentId.
            expandedAgentId.value = defaultExpandedAgent(agent.id);
            localStorage.setItem(AGENT_KEY, agent.id);
            await loadAgentSessions(agent.id);
        }
    } catch (err) {
        console.error('[boot] failed:', err);
        throw err;
    }
}

/**
 * Fetch sessions across all agents (no agent_id filter). DMs are
 * stored under `AgentId::nil()` (sentinel), notifications carry their
 * owning agent's id, and chat sessions are agent-keyed. All three
 * surfaces are needed cross-agent for the sidebar:
 *   - DMs / notifications drive the dedicated cross-agent sections so
 *     operators see incoming DMs / outgoing notifications for any of
 *     their agents regardless of which agent is currently selected.
 *   - Chat sessions for non-active agents power the per-agent
 *     accordion header session-count badge so every agent can show
 *     its real count without the operator having to switch into it
 *     first (Alper UX feedback on PR #1010 — the badge was previously
 *     active-only because chat sessions were filtered out here).
 *
 * Returns the raw session list (already sorted last_activity DESC by
 * the backend); the caller decides where to assign it. Errors are
 * logged and an empty array returned so the per-agent path keeps
 * working when the cross-agent fetch fails for any reason.
 */
export async function fetchCrossAgentSurfaces() {
    try {
        // Notifications are always returned by `/sessions` (no opt-in
        // flag); DMs are still gated on `include_dms` so the cross-agent
        // fetch picks them up alongside chats + notifications.
        const data = await listSessions(null, {
            includeDms: true,
        });
        return data.sessions || [];
    } catch (err) {
        console.error('[fetchCrossAgentSurfaces] failed:', err);
        return [];
    }
}

/**
 * Load sessions for an agent, select the latest, load its history + runs.
 *
 * @param {string} agentId
 * @param {string} [preferredSessionId] Optional session id to land on
 *   after the agent's sessions load. Used by cross-agent
 *   `navigateToSession` so an auto-switch lands on the targeted
 *   session rather than the agent's localStorage / latest fallback.
 *   Silently falls back to the normal selection rule if the preferred
 *   id isn't in this agent's session list.
 */
async function loadAgentSessions(agentId, preferredSessionId) {
    const gen = ++switchGeneration;

    try {
        // Fan-out: per-agent chat-session list + cross-agent DM /
        // notification list run in parallel. The per-agent call asks
        // only for chats (no `include_dms`) because `SessionList`
        // sources DMs / notifications exclusively from
        // `crossAgentSessions` and filters them out of the per-agent
        // list before grouping (Tim review #2 on PR #1010 — the
        // per-agent flags were redundant payload that never reached
        // the renderer). Notification rows still arrive on the
        // per-agent call because `/sessions` now unconditionally
        // returns them (PR #1100 removed the opt-in toggle), so we
        // filter them out here via `filterChatSessions` before
        // committing the normalized agent scope — Codex P2 on PR #1100 caught
        // that without this filter, an agent whose only session
        // activity is a notification would skip the chat-creation
        // fallback below and land in a read-only notification view at
        // boot. The per-agent `sessions` signal semantically means
        // "this agent's chat sessions" — notifications belong in the
        // sidebar's dedicated cross-agent Notifications section,
        // sourced from `crossAgentSessions`.
        const [data, crossAgent] = await Promise.all([
            listSessions(agentId, {
                includeDms: false,
            }),
            fetchCrossAgentSurfaces(),
        ]);
        if (gen !== switchGeneration) return; // stale — discard
        const agentSessions = filterChatSessions(data.sessions || []);
        replaceSessionScopes(agentSessions, crossAgent);

        // Seed cross-session activity indicators (#856) from the
        // `has_active_run` snapshot so the sidebar's yellow dot shows
        // up immediately on boot / agent switch / reload-mid-run, even
        // before the first SSE event arrives on the global activity feed.
        //
        // #1211: seed from the active agent's chat sessions AND the
        // cross-agent surfaces (`crossAgent`). The sidebar renders
        // cross-agent Jobs / DMs owned by OTHER agents, and those can
        // have a live run at boot/switch time — seeding only
        // `agentSessions` (Tim's PR #1100 note: chat-only after
        // `filterChatSessions`) left their dot dark until the row was
        // selected. `crossAgent` carries `has_active_run` for every
        // surfaced session (notifications are always `false` — they
        // never run — so including them is harmless), and duplicate ids
        // across the two lists collapse to the same `bgRuns` entry.
        replaceActivitySnapshot([...agentSessions, ...crossAgent]);

        // Open the global cross-agent activity SSE feed so subsequent
        // transitions (`session_activity_started` / `session_activity_ended`)
        // update bgRuns live.  Closes any previously open stream. `agentId`
        // is passed only as the teardown/reopen scoping token (#1211 — the
        // feed itself is global, not per-agent).
        openAgentEventsStream(agentId);

        // If a caller-supplied `preferredSessionId` was passed (cross-agent
        // `navigateToSession`), that takes precedence — `loadActiveSession`
        // honours it below. Otherwise: if localStorage points at a hidden
        // session (subagent / job / episodic — these are intentionally
        // excluded from `/sessions` by `is_internal_context_id`), the
        // standard `loadActiveSession` path can't find a match and would
        // either fall back to the first visible session OR — when the agent
        // has no visible chats — drop into the `else` branch and silently
        // create a brand-new empty chat. That's the #1045 "subagent renders
        // empty" symptom: the operator reloaded while viewing a subagent
        // and got either the parent's chat (wrong content) or a freshly
        // minted empty chat (the literal blank pane). Probe the stored id
        // first so the navigation survives reload.
        const hiddenSessionId = preferredSessionId
            ? null
            : await resolveStoredSessionId(agentId, agentSessions);
        if (gen !== switchGeneration) return; // stale — discard
        if (hiddenSessionId) {
            activeSessionId.value = hiddenSessionId;
            // `saveActiveSession` is a no-op when the stored value is
            // already the same — keeping it explicit so the persistence
            // contract is obvious here too.
            saveActiveSession(agentId, hiddenSessionId);
            await loadSession(hiddenSessionId, {
                isStale: () => gen !== switchGeneration,
                logPrefix: 'loadAgentSessions:hidden',
            });
        } else if (agentSessions.length > 0) {
            const selected = loadActiveSession(agentId, agentSessions, preferredSessionId);
            activeSessionId.value = selected.id;
            // Re-persist in case the session list changed
            saveActiveSession(agentId, selected.id);
            // Delegate the run/history/approval/SSE loading to the shared
            // loadSession() function, passing a stale-check callback tied
            // to this function's local switchGeneration counter.
            await loadSession(selected.id, {
                isStale: () => gen !== switchGeneration,
                logPrefix: 'loadAgentSessions',
            });
        } else {
            // Create a first session
            const ctx = 'web-chat-' + Date.now();
            const resp = await createSession(agentId, ctx);
            if (gen !== switchGeneration) return; // stale — discard
            const [reloaded, reloadedCross] = await Promise.all([
                listSessions(agentId, {
                    includeDms: false,
                }),
                fetchCrossAgentSurfaces(),
            ]);
            if (gen !== switchGeneration) return; // stale — discard
            // Same notification-filter contract as the initial fetch
            // above — `/sessions` always returns notifications post
            // PR #1100, but the normalized agent scope is chats only.
            replaceSessionScopes(filterChatSessions(reloaded.sessions || []), reloadedCross);
            activeSessionId.value = resp.session_id;
            replaceMessages([], resp.session_id);
            replaceRuns(resp.session_id, []);
            // Open persistent session stream
            openSessionStream(resp.session_id);
        }
    } catch (err) {
        if (gen !== switchGeneration) return; // stale — discard
        console.error('[loadAgentSessions] failed:', err);
    }
}

/**
 * Switch to a different agent: reset state, load sessions.
 *
 * @param {string} agentId
 * @param {{ targetSessionId?: string }} [opts] When `targetSessionId`
 *   is set, the agent-switch lands on that session instead of the
 *   agent's localStorage / latest default. Used by cross-agent
 *   `navigateToSession` to auto-switch into the owning agent's
 *   accordion group when the operator clicks a session that doesn't
 *   belong to the active agent.
 */
export async function switchAgent(agentId, opts) {
    const agent = agents.value.find(a => a.id === agentId);
    if (!agent) return;

    closeSessionStream(); // close previous session stream
    closeAgentEventsStream(); // close previous agent-scoped events stream (#856)
    bumpSelectGeneration(); // invalidate any in-flight selectSession() fetches

    activeAgentId.value = agentId;
    // Re-sync the sidebar accordion expansion to the new active agent
    // — switchAgent is the canonical "operator picked this agent"
    // surface (dropdown, cross-agent navigate, header click on a
    // different agent), so any previously-collapsed state should
    // reset to expanded for the new agent.
    expandedAgentId.value = defaultExpandedAgent(agentId);
    localStorage.setItem(AGENT_KEY, agentId);
    agentSwitchLoading.value = true;

    // Reset all state
    activeSessionId.value = null;
    selectedRunId.value = null;
    resetScopedEntities();
    replaceMessages([]);
    messageQueue.value = [];
    wsFiles.value = null;
    auditEvents.value = null;
    clearAllSubagents();

    // loadAgentSessions() bumps switchGeneration synchronously (before its
    // first await), so we start the call, then read the updated counter.
    const promise = loadAgentSessions(agentId, opts && opts.targetSessionId);
    const gen = switchGeneration;
    try {
        await promise;
    } finally {
        if (gen === switchGeneration) {
            agentSwitchLoading.value = false;
        }
    }
}
