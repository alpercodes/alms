import { signal, computed } from '../deps.js';

export const sessions = signal([]);
export const activeSessionId = signal(null);

/**
 * Cross-agent session list — every user-facing session for every
 * agent in the tenant.
 *
 * The per-agent `sessions` list above only carries the active agent's
 * sessions. This signal carries everything (chat / DM / notification)
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
 * `hooks/use-boot.js`. Internal types (job, subagent, episodic) are
 * excluded by the backend's listing rules; notifications are always
 * included; DMs are gated on the `include_dms` query flag.
 */
export const crossAgentSessions = signal([]);

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
 * Participants of the active DM session (empty array for non-DM sessions).
 */
export const dmParticipants = computed(() => {
    const s = activeSession.value;
    return (s && s.session_type === 'dm' && Array.isArray(s.participants))
        ? s.participants
        : [];
});
