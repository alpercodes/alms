import { signal } from '../deps.js';
import { fetchSettings } from '../api/settings.js';

// Server-reported defaults from GET /settings.
//
// Per-run config overrides were removed in the #941 pivot, so the only
// surface this state module manages now is the cached server defaults
// (consumed by the header bar's posture badge, the settings modal, and
// the agents tab's "inherit (server default)" hints). Per-tenant
// overrides live on the agent record (Agents panel).
export const serverDefaults = signal({});

/** Re-fetch server defaults after a PATCH /settings update. */
export async function refreshServerDefaults() {
    try {
        const data = await fetchSettings();
        serverDefaults.value = data;
    } catch (err) {
        console.error('[settings] refresh failed:', err);
    }
}
