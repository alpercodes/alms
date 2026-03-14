import { signal } from '../deps.js';

// Server-reported defaults from GET /settings
export const serverDefaults = signal({});

// User-configurable local settings (persisted to localStorage)
const SETTINGS_KEY = 'alms_settings';

function loadSettings() {
    try {
        return JSON.parse(localStorage.getItem(SETTINGS_KEY)) || {};
    } catch {
        return {};
    }
}

export const localSettings = signal(loadSettings());

export function saveSettings(updates) {
    const merged = { ...localSettings.value, ...updates };
    localStorage.setItem(SETTINGS_KEY, JSON.stringify(merged));
    localSettings.value = merged;
}
