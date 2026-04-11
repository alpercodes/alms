import { html, useSignal, useCallback, useEffect, useRef } from '../../deps.js';
import { activeAgent } from '../../state/agents.js';
import { agentPhase, phaseToLabel } from '../../state/agent-status.js';
import { activePanel, togglePanel } from '../../state/panel.js';
import { IconFolder } from '../../utils/icons.js';
import { AgentEditModal } from '../panel/agents-tab.js';

/**
 * Persistent header bar at the top of the chat area showing the active
 * agent name and quick-access buttons for Workspace and a three-dot
 * context menu with agent-specific settings.
 * Hidden when no agent is selected.
 */
export function AgentHeaderBar() {
    const agent = activeAgent.value;
    // Read agentPhase.value directly (raw signal) and derive the label
    // inline instead of going through the agentStatus computed signal.
    // This avoids a @preact/signals reactivity edge-case where the
    // computed signal occasionally fails to trigger component re-renders.
    const { phase, detail } = agentPhase.value;
    const statusLabel = phaseToLabel(phase, detail);
    const menuOpen = useSignal(false);
    const editOpen = useSignal(false);
    const menuRef = useRef(null);

    const toggleMenu = useCallback(() => {
        menuOpen.value = !menuOpen.value;
    }, []);

    const openAgentSettings = useCallback(() => {
        menuOpen.value = false;
        editOpen.value = true;
    }, []);

    // Close menu when clicking outside
    useEffect(() => {
        if (!menuOpen.value) return;
        const onClickOutside = (e) => {
            if (menuRef.current && !menuRef.current.contains(e.target)) {
                menuOpen.value = false;
            }
        };
        document.addEventListener('click', onClickOutside, true);
        return () => document.removeEventListener('click', onClickOutside, true);
    }, [menuOpen.value]);

    if (!agent) return null;

    return html`
        <div class="agent-header-bar">
            <div class="agent-header-bar-left">
                <span class="agent-header-bar-name">${agent.name}</span>
                ${statusLabel && html`
                    <span class="agent-status-label">${statusLabel}</span>
                `}
            </div>
            <div class="agent-header-bar-right">
                <button class="hbtn agent-bar-btn ${activePanel.value === 'workspace' ? 'active' : ''}"
                        title="Workspace files"
                        aria-label="Open workspace panel"
                        onClick=${() => togglePanel('workspace')}>
                    <${IconFolder} />
                    <span class="agent-bar-btn-label">Workspace</span>
                </button>
                <div class="agent-menu-anchor" ref=${menuRef}>
                    <button class="hbtn agent-bar-btn"
                            title="Agent menu"
                            aria-label="Open agent menu"
                            aria-expanded=${menuOpen.value}
                            onClick=${toggleMenu}>
                        <span class="agent-menu-dots" aria-hidden="true">\u22EF</span>
                    </button>
                    ${menuOpen.value && html`
                        <div class="agent-menu-dropdown">
                            <button class="agent-menu-item" onClick=${openAgentSettings}>
                                Settings
                            </button>
                        </div>
                    `}
                </div>
            </div>

            ${editOpen.value && html`
                <${AgentEditModal}
                    agent=${agent}
                    onClose=${() => { editOpen.value = false; }} />
            `}
        </div>
    `;
}
