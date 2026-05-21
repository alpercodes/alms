import { get, post } from './client.js';

export const createRun = (body) => post('/runs', body);

export const getRun = (runId) => get(`/runs/${runId}`);

export const listRuns = (sessionId, limit = 20) =>
    get(`/runs?session_id=${sessionId}&limit=${limit}`);

export const cancelRun = (runId) => post(`/runs/${runId}/cancel`);

// Fetch accumulated extended-thinking ("reasoning") text for an in-flight
// run. Used by `loadSession` to rehydrate the reasoning panel on page
// reload (#1043). Returns `{ run_id, text, last_event_id }`. `text` may be
// empty when the run has produced no reasoning yet; `last_event_id` may be
// null in that case. When non-null, the caller should pass it as the SSE
// `last_event_id` so the live stream does not double-emit deltas that are
// already reflected in `text`.
export const getRunReasoning = (runId) => get(`/runs/${runId}/reasoning`);

// Fetch accumulated visible-reply text for an in-flight run (#1107).
//
// Mirror of `getRunReasoning` for the main chat channel. Visible reply
// text streams via `token_delta` SSE events that are explicitly NOT
// persisted to either the per-run or per-session event log (they are
// flagged ephemeral in the backend's `send_event`), so on a mid-turn
// session switch the only durable source is the per-run in-memory
// accumulator kept by the gateway. Used by `loadSession` to repopulate
// the partial assistant reply when the user switches into a streaming
// session, scoped to the current parent-agent turn (cleared on
// `tool_start` / `tool_end` boundaries). Returns
// `{ run_id, text, last_event_id }`. Same null / empty semantics as
// `getRunReasoning`. DM sessions skip this call client-side because
// visible reply flows through a different surface there.
export const getRunText = (runId) => get(`/runs/${runId}/text`);

export const listApprovals = (sessionId) =>
    get(`/approvals?session_id=${sessionId}`);

export const listAgentRuns = (agentId, limit = 50) =>
    get(`/runs?agent_id=${agentId}&limit=${limit}`);
