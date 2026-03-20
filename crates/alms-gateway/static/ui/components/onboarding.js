import { h, html, useSignal } from '../deps.js';
import { createAgent, listAgents } from '../api/agents.js';
import { agents } from '../state/agents.js';
import { switchAgent } from '../hooks/use-boot.js';

export function OnboardingView() {
    const name = useSignal('');
    const error = useSignal('');
    const loading = useSignal(false);

    const onSubmit = async (e) => {
        e.preventDefault();
        const val = name.value.trim();
        if (!val) return;

        // Validate: lowercase, digits, hyphens, 1-64 chars
        if (!/^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$/.test(val)) {
            error.value = 'Invalid name: lowercase letters, digits, hyphens only (1-64 chars, no trailing hyphen)';
            return;
        }

        loading.value = true;
        error.value = '';
        try {
            const resp = await createAgent({ name: val, is_default: true });
            const data = await listAgents();
            agents.value = data.agents || [];
            const newId = resp.id || (agents.value.find(a => a.name === val) || {}).id;
            if (newId) await switchAgent(newId);
        } catch (err) {
            error.value = err.message || 'Failed to create agent';
        } finally {
            loading.value = false;
        }
    };

    return html`
        <div id="onboarding">
            <form class="onboard-card" onSubmit=${onSubmit}>
                <h2>Welcome to ALMS</h2>
                <p>Create your first agent to get started. The agent will introduce itself and learn about you in a short setup conversation.</p>
                <div>
                    <label>Agent name</label>
                    <input type="text" placeholder="my-agent" autofocus
                        value=${name.value}
                        onInput=${(e) => { name.value = e.target.value; }}
                        disabled=${loading.value} />
                    <div class="onboard-hint">lowercase, digits, hyphens (1-64 chars)</div>
                </div>
                <button class="onboard-btn" type="submit" disabled=${loading.value || !name.value.trim()}>
                    ${loading.value ? 'Creating...' : 'Create Agent'}
                </button>
                <div class="onboard-error">${error.value}</div>
            </form>
        </div>
    `;
}
