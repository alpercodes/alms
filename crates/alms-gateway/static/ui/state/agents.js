import { signal, computed } from 'https://esm.sh/@preact/signals@1.3.0';

export const agents = signal([]);
export const activeAgentId = signal(null);

export const activeAgent = computed(() =>
    agents.value.find(a => a.id === activeAgentId.value) || null
);
