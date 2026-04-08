import { h, html, computed, signal } from '../deps.js';
import { agents, activeAgentId } from '../state/agents.js';
import { activePanel, activePanelTab } from '../state/panel.js';
import { localSettings, serverDefaults } from '../state/settings.js';
import { theme, toggleTheme } from '../state/theme.js';
import { switchAgent } from '../hooks/use-boot.js';
import { IconGear, IconSun, IconMoon, IconMenu, IconX } from '../utils/icons.js';
import { activeOverrideCount } from './settings-modal.js';

/** Sidebar open/close state — shared so Sidebar and backdrop can react */
export const sidebarOpen = signal(false);

export function toggleSidebar() {
    sidebarOpen.value = !sidebarOpen.value;
}

export function closeSidebar() {
    sidebarOpen.value = false;
}

const TABS = ['agents', 'workspace', 'jobs', 'audit'];

const effectivePosture = computed(() => {
    const local = localSettings.value.posture;
    const server = serverDefaults.value.posture;
    return local || server || 'guarded';
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
    const statusClass = status.value === 'connected' ? 'ok'
        : status.value === 'running' ? 'running'
        : status.value === 'error' || status.value === 'offline' ? 'error' : '';

    return html`
        <header>
            <button class="sidebar-toggle-btn" title="Toggle sessions" aria-label="Toggle sessions"
                    onClick=${toggleSidebar}>
                ${sidebarOpen.value ? html`<${IconX} />` : html`<${IconMenu} />`}
            </button>
            <h1>ALMS</h1>
            <span class="header-sep">/</span>
            <select id="agent-select" title="Active agent"
                    value=${activeAgentId.value || ''}
                    onChange=${onAgentChange}>
                ${agents.value.length === 0
                    ? html`<option value="">No agents</option>`
                    : agents.value.map(a => html`
                        <option value=${a.id}>
                            ${a.name}${a.is_default ? ' *' : ''}${a.needs_bootstrap ? ' (setup)' : ''}
                        </option>
                    `)
                }
            </select>

            ${posture === 'guarded' && html`
                <span id="posture-badge" class="guarded">guarded</span>
            `}
            ${posture === 'autonomous' && html`
                <span id="posture-badge" class="autonomous">autonomous</span>
            `}

            <div class="header-spacer"></div>

            <span class="status-dot ${statusClass}"></span>
            <span id="status">${status.value}</span>

            <div class="header-btns">
                ${TABS.map(tab => html`
                    <button class="hbtn ${activePanel.value === tab ? 'active' : ''}"
                            onClick=${() => togglePanel(tab)}>
                        ${tab.charAt(0).toUpperCase() + tab.slice(1)}
                    </button>
                `)}
            </div>

            <button class="header-icon-btn" title="Toggle theme" aria-label="Toggle theme"
                    onClick=${toggleTheme}>
                ${theme.value === 'dark' ? html`<${IconSun} />` : html`<${IconMoon} />`}
            </button>

            <button class="header-icon-btn settings-btn" title="Settings" aria-label="Settings"
                    onClick=${onOpenSettings}>
                <${IconGear} />
                ${activeOverrideCount.value > 0 && html`
                    <span class="settings-override-badge">${activeOverrideCount.value}</span>
                `}
            </button>
        </header>
    `;
}
