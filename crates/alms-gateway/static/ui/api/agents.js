import { get, post, put, del } from './client.js';

export const listAgents = () => get('/agents');
export const createAgent = (body) => post('/agents', body);
export const getAgent = (id) => get(`/agents/${id}`);
export const updateAgent = (id, body) => put(`/agents/${id}`, body);
export const deleteAgent = (id) => del(`/agents/${id}`);
export const setDefaultAgent = (id) => post(`/agents/${id}/default`);
