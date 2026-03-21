import { get } from './client.js';

export const listTasks = () => get('/tasks');
export const getTask = (taskId) => get(`/tasks/${taskId}`);
