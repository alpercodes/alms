import { signal } from '../deps.js';

export const activePanel = signal(null);       // null | 'workspace' | 'jobs' | 'audit' | 'agents' | 'runs'
export const activePanelTab = signal('agents'); // which tab is shown when panel is open

/** Toggle a panel tab — close if already open, otherwise switch to it. */
export function togglePanel(tab) {
    if (activePanel.value === tab) {
        activePanel.value = null;
    } else {
        activePanel.value = tab;
        activePanelTab.value = tab;
    }
}
