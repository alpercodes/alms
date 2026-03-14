import { signal } from 'https://esm.sh/@preact/signals@1.3.0';

export const activePanel = signal(null);       // null | 'workspace' | 'jobs' | 'audit' | 'agents'
export const activePanelTab = signal('agents'); // which tab is shown when panel is open
