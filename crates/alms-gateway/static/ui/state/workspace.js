import { signal } from 'https://esm.sh/@preact/signals@1.3.0';

// Workspace files for active agent: { 'personality.md': '...', ... } | 'unavailable' | 'error' | null
export const wsFiles = signal(null);
