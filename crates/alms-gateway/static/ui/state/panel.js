import { signal } from '../deps.js';

export const activePanel = signal(null);       // null | 'workspace' | 'jobs' | 'audit' | 'agents'
export const activePanelTab = signal('agents'); // which tab is shown when panel is open
