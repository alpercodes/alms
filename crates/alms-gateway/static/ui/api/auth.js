import { get, put, del } from './client.js';

export const listKeys = () => get('/auth/keys');
export const setKey = (provider, key) => put('/auth/keys', { provider, key });
export const removeKey = (provider) => del(`/auth/keys/${provider}`);
