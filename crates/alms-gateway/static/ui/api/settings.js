import { get } from './client.js';

export const fetchSettings = () => get('/settings');
