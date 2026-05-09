// Pure-function helpers for the sidebar's "group sessions by agent"
// accordion (issue #980).
//
// Pinned regression target:
//   - issue #980 — operators with multiple agents and many sessions per
//     agent need the sidebar grouped under per-agent collapsible
//     headers. At most one agent group is expanded at a time; clicking
//     another agent header collapses the previous and expands the new
//     one. Clicking the expanded agent's header collapses it without
//     changing the active agent. Clicking a session inside the expanded
//     group selects it without collapsing.
//
// The runtime side of the grouping (Preact rendering, signal wiring,
// switchAgent side-effect) is integration territory we can't easily
// unit-test under Node. What we CAN unit-test — and pin here — is the
// pure-function shape of the expand/collapse decision so a future
// refactor that touches the accordion can't silently regress the
// "at most one expanded at a time" contract or the default-expansion
// rule.

/**
 * Compute the next expanded-agent state given a click on an agent
 * header. Pure function — no DOM, no side effects.
 *
 * Rules:
 *   - Clicking a different agent than the currently-expanded one:
 *     collapse the previous, expand the clicked agent.
 *   - Clicking the same agent that's currently expanded: collapse it
 *     (true toggle). The header click controls the accordion body
 *     only; whether the agent stays the active selected agent is the
 *     caller's decision (the click handler in session-list.js leaves
 *     `activeAgentId` untouched on this branch — the operator can
 *     still see the chat view, dropdown, etc.).
 *   - Clicking when nothing is expanded yet: expand the clicked agent.
 *   - Falsy / missing agentId: no-op (return the previous state).
 *
 * @param {string|null} currentExpandedId — the currently-expanded agent
 *   id, or null when nothing is expanded.
 * @param {string|null|undefined} clickedAgentId — the agent id whose
 *   header was clicked.
 * @returns {string|null} the next expanded-agent id (null = collapsed).
 */
export function expandAgent(currentExpandedId, clickedAgentId) {
    // Defensive: missing / empty clickedAgentId is a no-op. Real call
    // sites should never reach this branch, but a defensive guard
    // keeps the helper safe for any future caller.
    if (!clickedAgentId || typeof clickedAgentId !== 'string') {
        return currentExpandedId == null ? null : currentExpandedId;
    }

    // Same agent already expanded — collapse (true toggle). The
    // accordion body collapses; whether the agent stays "active" is
    // the caller's decision. See the function docstring for the full
    // rationale (Alper UX feedback on PR #1010 — collapsing the active
    // agent shouldn't hide them from the chat view).
    if (clickedAgentId === currentExpandedId) {
        return null;
    }

    // Different agent — collapse the previous (drop currentExpandedId)
    // and expand the new one. At-most-one-at-a-time is enforced by
    // simply replacing the value.
    return clickedAgentId;
}

/**
 * Compute the default expanded-agent id at boot / agent-switch time.
 * The default is the currently-active agent's id — the operator just
 * arrived on a session that belongs to that agent, so it's the
 * obvious group to show open.
 *
 * Returns null when there's no active agent (boot-time before agents
 * load, or after delete-last-agent). The accordion renders all
 * agents collapsed in that case.
 *
 * @param {string|null|undefined} activeAgentId
 * @returns {string|null}
 */
export function defaultExpandedAgent(activeAgentId) {
    if (!activeAgentId || typeof activeAgentId !== 'string') return null;
    return activeAgentId;
}

/**
 * True when an agent's sessions are currently visible (its accordion
 * group is expanded). Pure helper for the SessionList render — keeps
 * the predicate in one place so the accordion contract is a single
 * source of truth.
 *
 * @param {string|null} expandedAgentId
 * @param {string|null|undefined} agentId
 * @returns {boolean}
 */
export function isAgentExpanded(expandedAgentId, agentId) {
    if (!agentId || typeof agentId !== 'string') return false;
    if (!expandedAgentId || typeof expandedAgentId !== 'string') return false;
    return expandedAgentId === agentId;
}

/**
 * Group a flat session list by `agent_id`, preserving the input
 * order within each group. Returns a Map so iteration order is
 * deterministic (Map preserves insertion order in modern JS).
 *
 * Sessions with a falsy / missing `agent_id` are dropped from the
 * grouping — they belong to a separate section (DM sessions use a
 * sentinel agent id, notifications go in their own section, etc.).
 * The caller decides what "user-facing chat session" means; this
 * helper just groups whatever it's handed.
 *
 * @param {Array<{ agent_id?: string }>} sessionList
 * @returns {Map<string, Array<object>>}
 */
export function groupSessionsByAgent(sessionList) {
    const out = new Map();
    if (!Array.isArray(sessionList)) return out;
    for (const s of sessionList) {
        if (!s || typeof s.agent_id !== 'string' || !s.agent_id) continue;
        const arr = out.get(s.agent_id);
        if (arr) {
            arr.push(s);
        } else {
            out.set(s.agent_id, [s]);
        }
    }
    return out;
}

/**
 * True when the active agent participates in / owns a cross-agent
 * surface row. Used by the sidebar to add a subtle visual emphasis
 * to DMs / notifications that "belong" to the agent the operator is
 * currently in.
 *
 * - Notification rows carry a single `agent_name` (the recipient).
 * - DM rows carry a `participants` array of size >= 2 (sender +
 *   recipient — the active agent is "in" the row when it appears).
 * - Other session types are not cross-agent surfaces; this helper
 *   conservatively returns false for them.
 *
 * @param {object} session
 * @param {string|null|undefined} activeAgentName
 * @returns {boolean}
 */
export function isOwnedByActiveAgent(session, activeAgentName) {
    if (!session || !activeAgentName || typeof activeAgentName !== 'string') {
        return false;
    }
    if (session.session_type === 'notification') {
        return session.agent_name === activeAgentName;
    }
    if (session.session_type === 'dm') {
        const parts = session.participants;
        return Array.isArray(parts) && parts.includes(activeAgentName);
    }
    return false;
}

/**
 * Stable sort cross-agent surface rows so:
 *
 *   1. Rows owned by the active agent land above rows owned by
 *      other agents. This pins the operator's "in" view to the top
 *      of the section so they're one glance away.
 *   2. Within each ownership bucket, the input order is preserved
 *      — the backend already sorts `last_activity DESC`, so this
 *      preserves the most-recently-active-first ordering within the
 *      bucket without needing the timestamp here.
 *
 * Returns a new array; does not mutate the input.
 *
 * @param {Array<object>} rows
 * @param {string|null|undefined} activeAgentName
 * @returns {Array<object>}
 */
export function sortCrossAgentRows(rows, activeAgentName) {
    if (!Array.isArray(rows)) return [];
    const decorated = rows.map((s, idx) => ({
        s,
        idx,
        owned: isOwnedByActiveAgent(s, activeAgentName) ? 0 : 1,
    }));
    // Stable tiebreak: original index preserves input order within
    // each ownership bucket. Active-agent rows (owned=0) win.
    decorated.sort((a, b) => a.owned - b.owned || a.idx - b.idx);
    return decorated.map(d => d.s);
}
