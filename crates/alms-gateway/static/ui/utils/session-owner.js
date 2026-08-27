// Session-owner derivation (#1212).
//
// Cross-agent surfaces (job / subagent / notification sessions) can be
// opened while a DIFFERENT agent is active in the sidebar:
// `navigateToSession` deliberately skips the agent auto-switch for them,
// so `activeAgent` keeps pointing at whatever agent the operator had
// selected. Any label derived from `activeAgent` then shows the wrong
// name — the #1212 symptom was a job session (owned by agent A) whose
// assistant bubbles were labelled with a peer's name because that peer
// happened to be the active agent.
//
// This helper resolves the agent that OWNS a session, from the session
// envelope itself:
//   1. `agent_name` — set by the backend for notification sessions
//      (from the `notifications:{agent}` context_id) and for subagent
//      sessions (from the `subagent_{parent}_{name}` context_id, or the
//      `(subagent)` marker when the subagent is ephemeral — #1277).
//   2. `agent_id` — resolved against the in-memory agents list. This arm
//      covers chat sessions and JOB sessions, which are stored under the
//      owning agent's real id (`fire_job_run` uses
//      `get_or_create(job.agent_id, "job_{id}")`). It does NOT cover
//      subagent sessions: those are stored under a DERIVED id
//      (`AgentId::deterministic(parent_agent_id, name)` for named ones,
//      a fresh `AgentId::new()` for ephemeral ones) which matches no
//      registered agent, so they depend entirely on arm 1. Mirrors
//      `crossAgentOwner` in components/sidebar/session-list.js.
//
// Returns null when there is no clear single owner:
//   - DM sessions (stored under the AgentId::nil() sentinel, which never
//     matches a real agent) — both participants share the session and
//     messages carry per-sender `fromAgent` attribution instead.
//   - No session / unresolvable agent_id (legacy data, agent deleted).
//
// A null answer means "unknown", NOT "the active agent" — see
// `messageAuthorName` for what callers are allowed to do with it.

/**
 * Resolve the display name of the agent that owns `session`.
 *
 * @param {object|null} session - session envelope ({ agent_id, agent_name, ... })
 * @param {Array<{id: string, name: string}>} agentList - known agents
 * @returns {string|null} owner name, or null when there is no single owner
 */
export function sessionOwnerName(session, agentList) {
    if (!session) return null;
    if (session.agent_name) return session.agent_name;
    if (session.agent_id) {
        const owner = (agentList || []).find(a => a.id === session.agent_id);
        return owner ? owner.name : null;
    }
    return null;
}

/**
 * The name to render as the author of an assistant bubble in `session`.
 *
 * Call sites used to spell this as
 * `sessionOwnerName(...) || activeAgent?.name`, which quietly converted
 * "I could not resolve this session's owner" into "it must be whoever is
 * selected in the sidebar". That is what made #1277 a confident WRONG
 * name rather than a missing one: a subagent session resolves to no
 * registered agent, and opening one does not switch the active agent, so
 * every subagent bubble was labelled with the parent.
 *
 * The rule here is default-deny and deliberately NOT a list of
 * cross-agent session types — an incomplete list is exactly how subagent
 * sessions slipped through #1212's fix. There is exactly one case where
 * `activeAgent` is not a guess:
 *
 *   - the owner resolved: use it, whoever it is;
 *   - nothing is selected at all — no envelope AND no `activeSessionId`
 *     (boot, or after `switchAgent` resets the selection): there is no
 *     other agent in play, and this is the case the old
 *     `|| activeAgent?.name` was actually written for;
 *   - a session is selected but its owner did not resolve: null. The
 *     bubble renders with no name, which is the honest answer and the one
 *     #1277 asks for. A future session type that resolves to neither an
 *     `agent_name` nor a known `agent_id` degrades to blank here instead
 *     of inheriting this bug.
 *
 * ## Why the discriminator is `activeSessionId`, not the envelope
 *
 * Gating the fallback on "is there an envelope" is default-deny about
 * what is IN the envelope but default-allow about the envelope being
 * missing — and for subagent sessions the envelope arrives through
 * exactly one fallible path. They are excluded from `list_sessions`
 * outright (`routes.rs` — "Other internal sessions (subagent, episodic):
 * always excluded"), so the ONLY thing that ever populates
 * `activeSession` for them is the single-session GET in
 * `utils/load-session.js`, inside a `try` whose `catch` is explicitly
 * "Non-fatal — log and continue". A 404 or a network blip there leaves
 * `activeSession` null while a subagent session is very much on screen,
 * and an envelope-gated fallback would put the parent's name back on its
 * bubbles — #1277 verbatim, from a path no envelope-shaped row covers.
 *
 * `activeSessionId` is set on every navigation path before the envelope
 * is fetched, so it separates "a session is selected but I could not
 * identify its owner" (unknown → blank) from "nothing is selected"
 * (the active agent is the only agent in play). The cost is that a
 * selected session whose envelope has not landed yet renders a bare
 * prompt for that window instead of a name; per this PR's own thesis a
 * wrong name is worse than no name, and in practice the window is
 * covered — `newSession` sets the id in the same `batch()` as
 * `replaceSessionScopes`, and the drill-down / switch paths raise
 * `sessionSwitchLoading` (which replaces the message list wholesale)
 * for the duration of the load.
 *
 * Note there is deliberately NO "the session's `agent_id` matches the
 * active agent" escape hatch. `activeAgent` is derived from the same
 * `agents` list `sessionOwnerName` searches (`state/agents.js`), so any
 * session the active agent owns has already resolved through arm 2 — a
 * second path for it would be unreachable code carrying a rationale that
 * cannot be true.
 *
 * @param {object|null} session - session envelope
 * @param {Array<{id: string, name: string}>} agentList - known agents
 * @param {{id: string, name: string}|null} activeAgent - sidebar selection
 * @param {string|null} [activeSessionId] - id of the selected session, set
 *   on every navigation path before its envelope is fetched
 * @returns {string|null} author name, or null to render no name
 */
export function messageAuthorName(session, agentList, activeAgent, activeSessionId) {
    const owner = sessionOwnerName(session, agentList);
    if (owner) return owner;
    if (session || activeSessionId) return null;
    return (activeAgent && activeAgent.name) || null;
}
