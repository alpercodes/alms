import { fetchSettings } from '../api/settings.js';
import { listSessions, createSession, getSessionMessages } from '../api/sessions.js';
import { agents, activeAgentId } from '../state/agents.js';
import { sessions, activeSessionId, showNotifications } from '../state/sessions.js';
import { activeRunId, selectedRunId, runs } from '../state/runs.js';
import { serverDefaults } from '../state/settings.js';
import { replaceMessages } from '../state/chat-actions.js';
import { messageQueue, bgRuns } from '../state/queue.js';
import { wsFiles } from '../state/workspace.js';
import { auditEvents } from '../state/audit.js';
import { agentSwitchLoading } from '../state/loading.js';
import { openSessionStream, closeSessionStream } from './use-session-stream.js';
import { openAgentEventsStream, closeAgentEventsStream } from './use-agent-events.js';
import { bumpSelectGeneration } from '../state/select-generation.js';
import { loadSession } from '../utils/load-session.js';
import { clearAllSubagents } from '../state/subagents.js';

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

function loadActiveSession(agentId, agentSessions) {
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
 * Subagent sessions are reached via SubagentBar's "View session" button,
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
        agents.value = data.agents || [];

        // Determine active agent: localStorage > default > first
        const saved = localStorage.getItem(AGENT_KEY);
        const defaultAgent = agents.value.find(a => a.is_default);
        const firstAgent = agents.value[0];
        const agent = agents.value.find(a => a.id === saved) || defaultAgent || firstAgent;

        if (agent) {
            activeAgentId.value = agent.id;
            localStorage.setItem(AGENT_KEY, agent.id);
            await loadAgentSessions(agent.id);
        }
    } catch (err) {
        console.error('[boot] failed:', err);
        throw err;
    }
}

/**
 * Load sessions for an agent, select the latest, load its history + runs.
 */
async function loadAgentSessions(agentId) {
    const gen = ++switchGeneration;

    try {
        const data = await listSessions(agentId, {
            includeDms: true,
            includeNotifications: showNotifications.value,
        });
        if (gen !== switchGeneration) return; // stale — discard
        const agentSessions = data.sessions || [];
        const dmCount = agentSessions.filter(s => s.session_type === 'dm').length;
        const notifCount = agentSessions.filter(s => s.session_type === 'notification').length;
        if (dmCount > 0 || notifCount > 0) {
            console.debug('[loadAgentSessions] loaded', agentSessions.length, 'sessions,', dmCount, 'DM,', notifCount, 'notification');
        }
        sessions.value = agentSessions;

        // Seed cross-session activity indicators (#856) from the
        // `has_active_run` snapshot so the sidebar's yellow dot shows
        // up immediately on boot / agent switch / reload-mid-run, even
        // before the first SSE event arrives on the agent feed.
        const seedBg = {};
        for (const s of agentSessions) {
            if (s.has_active_run) {
                seedBg[s.id] = { runId: null, finished: false };
            }
        }
        bgRuns.value = seedBg;

        // Open the agent-scoped SSE feed so subsequent transitions
        // (`session_activity_started` / `session_activity_ended`)
        // update bgRuns live.  Closes any previously open stream.
        openAgentEventsStream(agentId);

        // If localStorage points at a hidden session (subagent / job /
        // episodic — these are intentionally excluded from `/sessions`
        // by `is_internal_context_id`), the standard `loadActiveSession`
        // path can't find a match and would either fall back to the
        // first visible session OR — when the agent has no visible
        // chats — drop into the `else` branch and silently create a
        // brand-new empty chat. That's the #1045 "subagent renders empty"
        // symptom: the operator reloaded while viewing a subagent and
        // got either the parent's chat (wrong content) or a freshly
        // minted empty chat (the literal blank pane). Probe the stored
        // id first so the navigation survives reload.
        const hiddenSessionId = await resolveStoredSessionId(agentId, agentSessions);
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
            const selected = loadActiveSession(agentId, agentSessions);
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
            const reloaded = await listSessions(agentId, {
                includeDms: true,
                includeNotifications: showNotifications.value,
            });
            if (gen !== switchGeneration) return; // stale — discard
            sessions.value = reloaded.sessions || [];
            activeSessionId.value = resp.session_id;
            replaceMessages([]);
            runs.value = [];
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
 */
export async function switchAgent(agentId) {
    const agent = agents.value.find(a => a.id === agentId);
    if (!agent) return;

    closeSessionStream(); // close previous session stream
    closeAgentEventsStream(); // close previous agent-scoped events stream (#856)
    bumpSelectGeneration(); // invalidate any in-flight selectSession() fetches

    activeAgentId.value = agentId;
    localStorage.setItem(AGENT_KEY, agentId);
    agentSwitchLoading.value = true;

    // Reset all state
    activeSessionId.value = null;
    activeRunId.value = null;
    selectedRunId.value = null;
    sessions.value = [];
    runs.value = [];
    replaceMessages([]);
    messageQueue.value = [];
    wsFiles.value = null;
    auditEvents.value = null;
    bgRuns.value = {}; // clear stale cross-session activity (#856)
    clearAllSubagents();

    // loadAgentSessions() bumps switchGeneration synchronously (before its
    // first await), so we start the call, then read the updated counter.
    const promise = loadAgentSessions(agentId);
    const gen = switchGeneration;
    try {
        await promise;
    } finally {
        if (gen === switchGeneration) {
            agentSwitchLoading.value = false;
        }
    }
}
