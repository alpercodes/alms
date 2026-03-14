import { get, post, del } from './client.js';

export const listJobs = () => get('/jobs');
export const createJob = (body) => post('/jobs', body);
export const cancelJob = (id) => del(`/jobs/${id}`);
