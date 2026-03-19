import { html } from '../../deps.js';
import { activePanel, activePanelTab } from '../../state/panel.js';
import { AgentsTab } from './agents-tab.js';
import { WorkspaceTab } from './workspace-tab.js';
import { JobsTab } from './jobs-tab.js';
import { AuditTab } from './audit-tab.js';

const TABS = ['agents', 'workspace', 'jobs', 'audit'];

function TabBody({ tab }) {
    if (tab === 'agents') return html`<${AgentsTab} />`;
    if (tab === 'workspace') return html`<${WorkspaceTab} />`;
    if (tab === 'jobs') return html`<${JobsTab} />`;
    if (tab === 'audit') return html`<${AuditTab} />`;
    return null;
}

export function PanelContainer() {
    if (!activePanel.value) return null;

    return html`
        <div id="panel" class="open">
            <div class="panel-tabs">
                ${TABS.map(tab => html`
                    <button class="panel-tab ${activePanelTab.value === tab ? 'active' : ''}"
                            onClick=${() => { activePanelTab.value = tab; }}>
                        ${tab.charAt(0).toUpperCase() + tab.slice(1)}
                    </button>
                `)}
                <button class="panel-tab panel-close"
                        onClick=${() => { activePanel.value = null; }}
                        title="Close panel">x</button>
            </div>
            <div class="panel-body">
                <${TabBody} tab=${activePanelTab.value} />
            </div>
        </div>
    `;
}
