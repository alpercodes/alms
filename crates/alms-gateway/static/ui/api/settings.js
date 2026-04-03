import { get, patch } from './client.js';

export const fetchSettings = () => get('/settings');

/** PATCH /settings — send partial config updates to the server. */
export const patchSettings = (body) => patch('/settings', body);
