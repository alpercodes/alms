import { get } from './client.js';

/**
 * Fetch the cross-channel timeline for an agent.
 * @param {string} agentId - Agent UUID or name slug
 * @param {number} [limit=50] - Max events to return
 * @param {string|null} [before] - ISO timestamp cursor for pagination
 * @returns {Promise<{ agent_id: string, events: Array, pagination: { has_more: boolean, next_before: string|null } }>}
 */
export function getAgentTimeline(agentId, limit = 50, before = null) {
    let path = `/agents/${agentId}/timeline?limit=${limit}`;
    if (before) path += `&before=${encodeURIComponent(before)}`;
    return get(path);
}
