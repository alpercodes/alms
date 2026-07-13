import { signal, computed } from '../deps.js';
import { entityState } from './entity-state.js';

export const agents = entityState.agents;
export const activeAgentId = signal(null);

export const activeAgent = computed(() =>
    agents.value.find(a => a.id === activeAgentId.value) || null
);

export function replaceAgents(nextAgents) {
    entityState.replaceAgents(nextAgents);
}
