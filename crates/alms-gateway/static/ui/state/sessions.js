import { signal, computed } from '../deps.js';
import { agents, activeAgent } from './agents.js';
import { sessionOwnerName, messageAuthorName } from '../utils/session-owner.js';
import { entityState } from './entity-state.js';

export const sessions = entityState.agentSessions;
export const activeSessionId = signal(null);

/**
 * Cross-agent session list — every user-facing session for every
 * agent in the tenant.
 *
 * The `sessions` selector above carries the active agent's scoped chats plus
 * any explicitly pinned active internal envelope. Pinned envelopes survive
 * authoritative list refreshes until `resetScopedEntities()` runs. This
 * cross-agent signal carries everything (chat / DM / notification)
 * for every agent so the sidebar can:
 *   - Render cross-agent DM / notification sections without forcing
 *     an agent switch (DMs are stored under `AgentId::nil()` and
 *     notifications carry their owning agent's id; both are
 *     cross-agent surfaces by design).
 *   - Show a real session-count badge on every agent header (active
 *     and non-active alike) — the per-agent fetch handles the active
 *     agent, this list handles the rest.
 *
 * Populated alongside `sessions` from a second `listSessions(null, …)`
 * call (no agent filter) — see `fetchCrossAgentSurfaces` in
 * `hooks/use-boot.js`. Internal types (subagent, episodic) are
 * excluded by the backend's listing rules; notifications and
 * scheduled-job sessions (#1197) are always included; DMs are gated
 * on the `include_dms` query flag.
 */
export const crossAgentSessions = entityState.crossAgentSessions;
export function replaceSessionScopes(agentSessions, crossSessions) {
    entityState.replaceSessionScopes(agentSessions, crossSessions);
}


export function upsertSession(session, scope = 'pinned') {
    entityState.upsertSession(session, scope);
}

export function resetScopedEntities() {
    entityState.resetScopedState();
}

/**
 * Whether the sidebar's "Jobs" section body is expanded (#1197).
 *
 * Collapsed by default on purpose: recurring jobs accumulate one
 * long-lived session each and cancelled one-shots can leave dead
 * sessions behind — the collapsed group keeps them one click away
 * without crowding the sidebar. Not persisted across reloads (matches
 * the agent accordion, which also re-derives its default at boot).
 */
export const jobsGroupExpanded = signal(false);

/**
 * Which agent's accordion body is currently expanded in the sidebar
 * session list, or `null` when the active agent's group is collapsed.
 *
 * This is a separate signal from `activeAgentId` so the operator can
 * collapse the active agent's session list without un-selecting the
 * agent (Alper UX feedback on PR #1010 — the chat view, dropdown, etc.
 * stay on the active agent; only the sidebar accordion body
 * collapses). The default at boot / on agent-switch is the active
 * agent; `switchAgent` re-syncs this signal explicitly so picking a
 * different agent from anywhere in the UI re-expands it.
 *
 * Pure-function helpers in `utils/sidebar-grouping.js` (`expandAgent`,
 * `defaultExpandedAgent`, `isAgentExpanded`) own the transition rules.
 */
export const expandedAgentId = signal(null);

/**
 * The active session object (if any).
 *
 * Looks at both the per-agent `sessions` list and the cross-agent
 * `crossAgentSessions` list because DMs / notifications can be active
 * even when the operator is "in" a different agent's chat group.
 */
export const activeSession = computed(() => {
    const id = activeSessionId.value;
    if (!id) return null;
    return sessions.value.find(s => s.id === id)
        || crossAgentSessions.value.find(s => s.id === id)
        || null;
});

/**
 * Whether the currently active session is a DM conversation.
 */
export const isDmSession = computed(() => {
    const s = activeSession.value;
    return s ? s.session_type === 'dm' : false;
});

/**
 * Whether the currently active session is a notification session.
 */
export const isNotificationSession = computed(() => {
    const s = activeSession.value;
    return s ? s.session_type === 'notification' : false;
});

/**
 * Whether the currently active session is an internal (read-only) type.
 * Internal types: notification, job, subagent.
 */
export const isInternalSession = computed(() => {
    const s = activeSession.value;
    if (!s) return false;
    return s.session_type === 'notification'
        || s.session_type === 'job'
        || s.session_type === 'subagent';
});

/**
 * Name of the agent that OWNS the active session, or null when there is
 * no single owner (DM sessions, unresolved boot state). See
 * `utils/session-owner.js` for the derivation and the #1212 rationale:
 * job/subagent/notification sessions are cross-agent surfaces that do
 * NOT switch the active agent when opened, so attribution derived from
 * `activeAgent` can show a different agent's name. Consumers should use
 * this where a wrong name is worse than no name at all (the internal
 * session header). Consumers that need a fallback must use
 * `activeMessageAuthorName` rather than `|| activeAgent.value?.name` —
 * see #1277 for what that shorthand cost.
 */
export const activeSessionOwnerName = computed(() =>
    sessionOwnerName(activeSession.value, agents.value)
);

/**
 * Name to render as the author of an assistant bubble in the active
 * session, or null to render no name.
 *
 * Wraps `messageAuthorName`, which owns the rule for when falling back to
 * the sidebar's `activeAgent` is legitimate. This exists as a single
 * computed so the bubble-rendering call sites cannot re-derive (and
 * re-break) that rule independently. See #1212 / #1277.
 *
 * `activeSessionId` is passed alongside the envelope because it is the
 * honest "is a session selected" signal: subagent envelopes reach the
 * store through one best-effort fetch, so an envelope-only gate would
 * fall back to the sidebar's agent whenever that fetch failed.
 */
export const activeMessageAuthorName = computed(() =>
    messageAuthorName(
        activeSession.value,
        agents.value,
        activeAgent.value,
        activeSessionId.value,
    )
);

/**
 * Participants of the active DM session (empty array for non-DM sessions).
 */
export const dmParticipants = computed(() => {
    const s = activeSession.value;
    return (s && s.session_type === 'dm' && Array.isArray(s.participants))
        ? s.participants
        : [];
});
