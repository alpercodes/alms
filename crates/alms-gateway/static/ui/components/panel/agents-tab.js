import { html, useEffect, useSignal } from '../../deps.js';
import { agents, activeAgentId } from '../../state/agents.js';
import { serverDefaults } from '../../state/settings.js';
import { activePanelTab } from '../../state/panel.js';
import { listAgents, createAgent, updateAgent, deleteAgent, setDefaultAgent } from '../../api/agents.js';
import { switchAgent } from '../../hooks/use-boot.js';

async function refreshAgents() {
    try {
        const data = await listAgents();
        agents.value = data.agents || data || [];
    } catch (err) {
        console.error('[agents] fetch failed:', err);
    }
}

/** Small popup modal for editing agent settings. */
function AgentEditModal({ agent, onClose }) {
    const model = useSignal(agent.model || '');
    const posture = useSignal(agent.posture || '');
    const provider = useSignal(agent.provider || '');
    const saving = useSignal(false);
    const error = useSignal('');

    const serverModel = serverDefaults.value.model || 'default';

    const onSave = async () => {
        saving.value = true;
        error.value = '';
        try {
            await updateAgent(agent.id, {
                model: model.value || null,
                posture: posture.value || null,
                provider: provider.value || null,
            });
            await refreshAgents();
            onClose();
        } catch (err) {
            error.value = err.error?.message || err.message || 'Save failed';
        } finally {
            saving.value = false;
        }
    };

    const onOverlayClick = (e) => {
        if (e.target === e.currentTarget) onClose();
    };

    return html`
        <div class="settings-overlay open" onClick=${onOverlayClick}>
            <div class="settings-modal" style="width:400px;">
                <h2>${agent.name}</h2>

                <div class="settings-row">
                    <label class="settings-label">Model</label>
                    <input class="settings-input" type="text"
                           placeholder=${serverModel + ' (server default)'}
                           value=${model.value}
                           onInput=${e => { model.value = e.target.value; }} />
                    <span class="settings-hint">Leave empty to use server default.</span>
                </div>

                <div class="settings-row">
                    <label class="settings-label">Provider</label>
                    <select class="settings-select"
                            value=${provider.value}
                            onChange=${e => { provider.value = e.target.value; }}>
                        <option value="">Server default</option>
                        <option value="openai">OpenAI</option>
                        <option value="anthropic">Anthropic</option>
                        <option value="openrouter">OpenRouter</option>
                    </select>
                </div>

                <div class="settings-row">
                    <label class="settings-label">Posture</label>
                    <select class="settings-select"
                            value=${posture.value}
                            onChange=${e => { posture.value = e.target.value; }}>
                        <option value="">Server default (${serverDefaults.value.posture || 'guarded'})</option>
                        <option value="full_control">full_control</option>
                        <option value="guarded">guarded</option>
                        <option value="autonomous">autonomous</option>
                    </select>
                </div>

                ${error.value && html`<div style="color:var(--error); font-size:var(--text-xs);">${error.value}</div>`}

                <div class="settings-footer">
                    <button class="settings-cancel" onClick=${onClose}>Cancel</button>
                    <button class="settings-save" onClick=${onSave} disabled=${saving.value}>
                        ${saving.value ? '...' : 'Save'}
                    </button>
                </div>
            </div>
        </div>
    `;
}

function AgentCard({ agent, isActive, onEdit }) {
    const error = useSignal('');
    const serverModel = serverDefaults.value.model || 'default';

    const onDelete = async () => {
        if (!confirm('Delete agent "' + agent.name + '"?')) return;
        try {
            await deleteAgent(agent.id);
            await refreshAgents();
            if (agent.id === activeAgentId.value) {
                const def = agents.value.find(a => a.is_default);
                const next = def || agents.value[0] || null;
                if (next) switchAgent(next.id);
                else activeAgentId.value = null;
            }
        } catch (err) {
            error.value = err.error?.message || err.message || 'Delete failed';
        }
    };

    const onSetDefault = async () => {
        try {
            await setDefaultAgent(agent.id);
            await refreshAgents();
        } catch (err) {
            error.value = err.error?.message || err.message || 'Failed';
        }
    };

    return html`
        <div class="agent-card ${isActive ? 'active' : ''}">
            <div class="agent-card-header">
                <span class="agent-card-name">${agent.name}</span>
                ${agent.is_default && html`<span class="agent-badge">default</span>`}
            </div>
            <div class="agent-card-meta">
                model: ${agent.model || serverModel}${!agent.model ? ' (default)' : ''}
            </div>
            ${agent.provider && html`
                <div class="agent-card-meta">provider: ${agent.provider}</div>
            `}
            ${agent.posture && html`
                <div class="agent-card-meta">posture: ${agent.posture}</div>
            `}
            ${error.value && html`<div class="agent-error">${error.value}</div>`}
            <div class="agent-card-actions">
                <button class="agent-card-btn" onClick=${() => switchAgent(agent.id)}>Select</button>
                <button class="agent-card-btn" onClick=${() => onEdit(agent)}>Edit</button>
                ${!agent.is_default && html`
                    <button class="agent-card-btn" onClick=${onSetDefault}>Set Default</button>
                `}
                <button class="agent-card-btn" style="color:var(--error);" onClick=${onDelete}>Delete</button>
            </div>
        </div>
    `;
}

export function AgentsTab() {
    const newName = useSignal('');
    const error = useSignal('');
    const loading = useSignal(false);
    const editingAgent = useSignal(null);

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

            ${error.value && html`<div class="agent-error">${error.value}</div>`}

            ${agents.value.length === 0
                ? html`<div style="color:var(--text-disabled); font-style:italic; padding:var(--space-4); font-size:var(--text-sm);">No agents</div>`
                : agents.value.map(a => html`
                    <${AgentCard} key=${a.id} agent=${a}
                                  isActive=${a.id === activeAgentId.value}
                                  onEdit=${(agent) => { editingAgent.value = agent; }} />
                `)
            }

            ${editingAgent.value && html`
                <${AgentEditModal}
                    agent=${editingAgent.value}
                    onClose=${() => { editingAgent.value = null; }} />
            `}
        </div>
    `;
}
