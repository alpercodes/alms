// Centralized API fetch wrapper with auth header injection.

const AUTH_KEY = 'alms_auth_token';

/**
 * Fetch wrapper that injects auth header and handles JSON.
 * @param {string} path - API path (e.g. '/agents')
 * @param {RequestInit & { body?: object }} opts
 * @returns {Promise<any>}
 */
export async function apiFetch(path, opts = {}) {
    const token = localStorage.getItem(AUTH_KEY);
    const headers = { ...opts.headers };
    if (token) headers['Authorization'] = `Bearer ${token}`;
    if (opts.body && typeof opts.body === 'object') {
        headers['Content-Type'] = 'application/json';
        opts.body = JSON.stringify(opts.body);
    }
    const res = await fetch(path, { ...opts, headers });
    if (!res.ok) {
        const body = await res.json().catch(() => ({}));
        throw { status: res.status, ...body };
    }
    // 204 No Content
    if (res.status === 204) return null;
    return res.json();
}

export const get = (path) => apiFetch(path);
export const post = (path, body) => apiFetch(path, { method: 'POST', body });
export const put = (path, body) => apiFetch(path, { method: 'PUT', body });
export const patch = (path, body) => apiFetch(path, { method: 'PATCH', body });
export const del = (path) => apiFetch(path, { method: 'DELETE' });
