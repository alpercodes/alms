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
//      (extracted from the `notifications:{agent}` context_id).
//   2. `agent_id` — resolved against the in-memory agents list. Job and
//      subagent sessions are stored under the owning agent's real id
//      (`fire_job_run` uses `get_or_create(job.agent_id, "job_{id}")`),
//      so this arm covers them. Mirrors `crossAgentOwner` in
//      components/sidebar/session-list.js.
//
// Returns null when there is no clear single owner:
//   - DM sessions (stored under the AgentId::nil() sentinel, which never
//     matches a real agent) — both participants share the session and
//     messages carry per-sender `fromAgent` attribution instead.
//   - No session / unresolvable agent_id (legacy data, agent deleted).
// Callers fall back to the previous `activeAgent`-based behaviour in
// that case, so ordinary chat sessions render exactly as before.

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
