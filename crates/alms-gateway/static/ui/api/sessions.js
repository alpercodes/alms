import { get, post, del } from './client.js';

export const listSessions = (agentId, opts) => {
    const params = new URLSearchParams();
    if (agentId) params.set('agent_id', agentId);
    if (opts && opts.includeDms) params.set('include_dms', 'true');
    const qs = params.toString();
    return get(`/sessions${qs ? '?' + qs : ''}`);
};

export const createSession = (agentId, contextId) =>
    post('/sessions', { agent_id: agentId, context_id: contextId });

export const getSessionMessages = (sessionId) =>
    get(`/sessions/${sessionId}/messages`);

/**
 * Fetch the single-session metadata envelope (#1065).
 *
 * Singular path `/session/{id}` — see the route registration in
 * `crates/alms-gateway/src/server/routes.rs` for the visual-separation
 * rationale vs the `/sessions/...` cluster.
 *
 * Response shape mirrors the per-entry shape of `listSessions`, plus
 * `parent_session_id` for subagent sessions (uuid or null). Field is
 * omitted for non-subagent session types — callers can shortcut on
 * field presence to detect subagent envelopes.
 */
export const getSession = (sessionId) =>
    get(`/session/${sessionId}`);

export const deleteSession = (sessionId) =>
    del(`/sessions/${sessionId}`);

/**
 * Fetch all tool call records across all runs for a session.
 * Returns { session_id, tool_calls: [...] } where each entry is a
 * flattened ToolCallRecord with an additional run_id field.
 */
export const getSessionToolCalls = (sessionId) =>
    get(`/sessions/${sessionId}/tool-calls`);

/**
 * Cancel an active DM conversation.
 * Cancels in-flight runs and notifies both participating agents.
 * Returns { ok, session_id, context_id, participants, runs_cancelled, reason }.
 */
export const cancelDm = (sessionId) =>
    post(`/sessions/${sessionId}/cancel-dm`);
