import { signal } from '../deps.js';

const THEME_KEY = 'alms_theme';

function loadTheme() {
    return localStorage.getItem(THEME_KEY) || 'dark';
}

export const theme = signal(loadTheme());

export function toggleTheme() {
    const next = theme.value === 'dark' ? 'light' : 'dark';
    theme.value = next;
    localStorage.setItem(THEME_KEY, next);
    document.documentElement.setAttribute('data-theme', next);
}

// Apply on load
document.documentElement.setAttribute('data-theme', loadTheme());
