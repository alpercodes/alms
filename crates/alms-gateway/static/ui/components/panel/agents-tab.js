import { html, useEffect, useSignal } from '../../deps.js';
import { agents, activeAgentId } from '../../state/agents.js';
import { activePanelTab } from '../../state/panel.js';
import { listAgents, createAgent, deleteAgent, setDefaultAgent } from '../../api/agents.js';
import { switchAgent } from '../../hooks/use-boot.js';

async function refreshAgents() {
    try {
        const data = await listAgents();
        agents.value = data.agents || data || [];
    } catch (err) {
        console.error('[agents] fetch failed:', err);
    }
}

export function AgentsTab() {
    const newName = useSignal('');
    const error = useSignal('');
    const loading = useSignal(false);

    useEffect(() => {
        if (activePanelTab.value === 'agents') refreshAgents();
    }, [activePanelTab.value]);

    const onCreate = async () => {
        const name = newName.value.trim();
        if (!name) return;
        error.value = '';
        loading.value = true;
        try {
            await createAgent({ name });
            newName.value = '';
            await refreshAgents();
        } catch (err) {
            error.value = err.error?.message || err.message || 'Failed to create agent';
        } finally {
            loading.value = false;
        }
    };

    const onDelete = async (id, name) => {
        if (!confirm(`Delete agent "${name}"?`)) return;
        try {
            await deleteAgent(id);
            await refreshAgents();
            // S6: if the deleted agent was active, switch to default or first
            if (id === activeAgentId.value) {
                const def = agents.value.find(a => a.is_default);
                const next = def || agents.value[0] || null;
                if (next) {
                    switchAgent(next.id);
                } else {
                    activeAgentId.value = null;
                }
            }
        } catch (err) {
            error.value = err.error?.message || err.message || 'Failed to delete agent';
        }
    };

    const onSetDefault = async (id) => {
        try {
            await setDefaultAgent(id);
            await refreshAgents();
        } catch (err) {
            error.value = err.error?.message || err.message || 'Failed to set default';
        }
    };

    return html`
        <div class="agent-list-container">
            <div class="agent-create-row">
                <input type="text" placeholder="New agent name..."
                       value=${newName.value}
                       onInput=${e => { newName.value = e.target.value; }}
                       onKeyDown=${e => { if (e.key === 'Enter') onCreate(); }}
                       style="flex:1; background:var(--surface-2); color:var(--text-primary); border:1px solid var(--border-default); padding:var(--space-2); font-family:var(--font-mono); font-size:var(--text-sm);" />
                <button class="agent-card-btn" onClick=${onCreate}
                        disabled=${loading.value}>
                    ${loading.value ? '...' : '+ Create'}
                </button>
            </div>

            ${error.value && html`
                <div class="agent-error">${error.value}</div>
            `}

            ${agents.value.length === 0
                ? html`<div style="color:var(--text-disabled); font-style:italic; padding:var(--space-4); font-size:var(--text-sm);">No agents</div>`
                : agents.value.map(a => html`
                    <div class="agent-card ${a.id === activeAgentId.value ? 'default' : ''}">
                        <div style="display:flex; align-items:center; gap:var(--space-2); margin-bottom:var(--space-2);">
                            <span class="agent-card-name">${a.name}</span>
                            ${a.is_default && html`<span class="agent-badge">default</span>`}
                        </div>
                        ${a.model && html`<div style="font-size:var(--text-xs); color:var(--text-muted); margin-bottom:var(--space-2);">model: ${a.model}</div>`}
                        <div class="agent-card-actions">
                            <button class="agent-card-btn" onClick=${() => switchAgent(a.id)}>Select</button>
                            ${!a.is_default && html`
                                <button class="agent-card-btn" onClick=${() => onSetDefault(a.id)}>Set Default</button>
                            `}
                            <button class="agent-card-btn" onClick=${() => onDelete(a.id, a.name)}
                                    style="color:var(--error);">Delete</button>
                        </div>
                    </div>
                `)
            }
        </div>
    `;
}
