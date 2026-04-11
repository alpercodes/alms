import { html } from '../../deps.js';
import { activeAgent } from '../../state/agents.js';
import { activePanel, activePanelTab } from '../../state/panel.js';
import { IconGear, IconFolder } from '../../utils/icons.js';

function togglePanel(tab) {
    if (activePanel.value === tab) {
        activePanel.value = null;
    } else {
        activePanel.value = tab;
        activePanelTab.value = tab;
    }
}

/**
 * Persistent header bar at the top of the chat area showing the active
 * agent name and quick-access buttons for Settings and Workspace panels.
 * Hidden when no agent is selected.
 */
export function AgentHeaderBar({ onOpenSettings }) {
    const agent = activeAgent.value;
    if (!agent) return null;

    return html`
        <div class="agent-header-bar">
            <div class="agent-header-bar-left">
                <span class="agent-header-bar-name">${agent.name}</span>
            </div>
            <div class="agent-header-bar-right">
                <button class="agent-header-bar-btn ${activePanel.value === 'workspace' ? 'active' : ''}"
                        title="Workspace files"
                        aria-label="Open workspace panel"
                        onClick=${() => togglePanel('workspace')}>
                    <${IconFolder} />
                    <span class="agent-header-bar-btn-label">Workspace</span>
                </button>
                <button class="agent-header-bar-btn"
                        title="Agent settings"
                        aria-label="Open settings"
                        onClick=${onOpenSettings}>
                    <${IconGear} />
                    <span class="agent-header-bar-btn-label">Settings</span>
                </button>
            </div>
        </div>
    `;
}
