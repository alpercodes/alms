/**
 * Read a value from localStorage, parsed as JSON.
 */
export function readStorage(key, fallback = null) {
    try {
        const raw = localStorage.getItem(key);
        return raw ? JSON.parse(raw) : fallback;
    } catch {
        return fallback;
    }
}

/**
 * Write a value to localStorage as JSON.
 */
export function writeStorage(key, value) {
    localStorage.setItem(key, JSON.stringify(value));
}
