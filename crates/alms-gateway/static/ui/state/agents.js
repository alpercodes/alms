import { signal, computed } from '../deps.js';

export const agents = signal([]);
export const activeAgentId = signal(null);

export const activeAgent = computed(() =>
    agents.value.find(a => a.id === activeAgentId.value) || null
);
