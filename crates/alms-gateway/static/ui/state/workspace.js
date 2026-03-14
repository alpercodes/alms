import { signal } from '../deps.js';

// Workspace files for active agent: { 'personality.md': '...', ... } | 'unavailable' | 'error' | null
export const wsFiles = signal(null);
