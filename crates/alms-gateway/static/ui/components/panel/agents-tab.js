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

function AgentCard({ agent, isActive }) {
    const editing = useSignal(false);
    const model = useSignal(agent.model || '');
    const systemPrompt = useSignal(agent.system_prompt || '');
    const posture = useSignal(agent.posture || '');
    const saving = useSignal(false);
    const error = useSignal('');

    const serverModel = serverDefaults.value.model || 'default';

    const onSave = async () => {
        saving.value = true;
        error.value = '';
        try {
            await updateAgent(agent.id, {
                model: model.value || null,
                system_prompt: systemPrompt.value || null,
                posture: posture.value || null,
            });
            await refreshAgents();
            editing.value = false;
        } catch (err) {
            error.value = err.error?.message || err.message || 'Save failed';
        } finally {
            saving.value = false;
        }
    };

    const onCancel = () => {
        model.value = agent.model || '';
        systemPrompt.value = agent.system_prompt || '';
        posture.value = agent.posture || '';
        error.value = '';
        editing.value = false;
    };

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

    if (editing.value) {
        return html`
            <div class="agent-card ${isActive ? 'active' : ''}">
                <div class="agent-card-header">
                    <span class="agent-card-name">${agent.name}</span>
                    ${agent.is_default && html`<span class="agent-badge">default</span>`}
                </div>

                <label class="agent-field-label">Model</label>
                <input class="agent-field-input" type="text"
                       placeholder=${serverModel + ' (server default)'}
                       value=${model.value}
                       onInput=${e => { model.value = e.target.value; }} />

                <label class="agent-field-label">System prompt</label>
                <textarea class="agent-field-input agent-field-textarea" rows="3"
                          placeholder="Uses server default if empty"
                          value=${systemPrompt.value}
                          onInput=${e => { systemPrompt.value = e.target.value; }}></textarea>

                <label class="agent-field-label">Posture</label>
                <select class="agent-field-input"
                        value=${posture.value}
                        onChange=${e => { posture.value = e.target.value; }}>
                    <option value="">Server default (${serverDefaults.value.posture || 'full_control'})</option>
                    <option value="full_control">full_control</option>
                    <option value="guarded">guarded</option>
                </select>

                ${error.value && html`<div class="agent-error">${error.value}</div>`}

                <div class="agent-card-actions">
                    <button class="agent-card-btn agent-btn-save" onClick=${onSave}
                            disabled=${saving.value}>
                        ${saving.value ? '...' : 'Save'}
                    </button>
                    <button class="agent-card-btn" onClick=${onCancel}>Cancel</button>
                </div>
            </div>
        `;
    }

    return html`
        <div class="agent-card ${isActive ? 'active' : ''}">
            <div class="agent-card-header">
                <span class="agent-card-name">${agent.name}</span>
                ${agent.is_default && html`<span class="agent-badge">default</span>`}
            </div>
            <div class="agent-card-meta">
                model: ${agent.model || serverModel}${!agent.model ? ' (default)' : ''}
            </div>
            ${agent.posture && html`
                <div class="agent-card-meta">posture: ${agent.posture}</div>
            `}
            ${agent.system_prompt && html`
                <div class="agent-card-meta agent-card-prompt">
                    prompt: ${agent.system_prompt.slice(0, 80)}${agent.system_prompt.length > 80 ? '\u2026' : ''}
                </div>
            `}
            <div class="agent-card-actions">
                <button class="agent-card-btn" onClick=${() => switchAgent(agent.id)}>Select</button>
                <button class="agent-card-btn" onClick=${() => { editing.value = true; }}>Edit</button>
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
                    <${AgentCard} key=${a.id} agent=${a} isActive=${a.id === activeAgentId.value} />
                `)
            }
        </div>
    `;
}
