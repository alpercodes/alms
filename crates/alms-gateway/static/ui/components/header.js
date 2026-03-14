import { h, html, computed } from '../deps.js';
import { agents, activeAgentId } from '../state/agents.js';
import { activePanel, activePanelTab } from '../state/panel.js';
import { localSettings, serverDefaults } from '../state/settings.js';
import { switchAgent } from '../hooks/use-boot.js';

const TABS = ['agents', 'workspace', 'jobs', 'audit'];

const effectivePosture = computed(() => {
    const local = localSettings.value.posture;
    const server = serverDefaults.value.posture;
    return local || server || 'full_control';
});

const hasSettingsOverride = computed(() => {
    const s = localSettings.value;
    return !!(s.max_tokens != null || s.posture);
});

function togglePanel(tab) {
    if (activePanel.value === tab) {
        activePanel.value = null;
    } else {
        activePanel.value = tab;
        activePanelTab.value = tab;
    }
}

export function Header({ onOpenSettings, status }) {
    const onAgentChange = (e) => {
        if (e.target.value && e.target.value !== activeAgentId.value) {
            switchAgent(e.target.value);
        }
    };

    const posture = effectivePosture.value;

    return html`
        <header>
            <h1>ALMS</h1>
            <span style="font-size:10px;color:#484f58;flex-shrink:0">v0.2</span>
            <select id="agent-select" title="Active agent"
                    value=${activeAgentId.value || ''}
                    onChange=${onAgentChange}>
                ${agents.value.length === 0
                    ? html`<option value="">No agents</option>`
                    : agents.value.map(a => html`
                        <option value=${a.id}>
                            ${a.name}${a.is_default ? ' *' : ''}${a.needs_bootstrap ? ' (setup needed)' : ''}
                        </option>
                    `)
                }
            </select>
            <span id="posture-badge"
                  title="Execution posture"
                  class=${posture === 'guarded' ? 'guarded' : ''}
                  style="display: inline">
                ${posture}
            </span>
            <span id="status" class=${status.value === 'connected' ? 'ok' : status.value === 'running' ? 'running' : status.value === 'error' ? 'error' : ''}>
                ${status.value}
            </span>
            <div class="header-btns">
                ${TABS.map(tab => html`
                    <button class="hbtn ${activePanel.value === tab ? 'active' : ''}"
                            onClick=${() => togglePanel(tab)}>
                        ${tab.charAt(0).toUpperCase() + tab.slice(1)}
                    </button>
                `)}
                <button class="hbtn ${hasSettingsOverride.value ? 'active' : ''}"
                        id="btn-settings"
                        title=${localSettings.value.posture ? `posture: ${localSettings.value.posture}` : 'Settings'}
                        onClick=${onOpenSettings}>
                    Settings
                </button>
            </div>
        </header>
    `;
}
