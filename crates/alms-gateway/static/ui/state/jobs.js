import { entityState } from './entity-state.js';

export const jobs = entityState.jobs;

export function captureJobMutationGeneration() {
    return entityState.getJobMutationGeneration();
}

export function replaceJobs(items, mutationGeneration) {
    entityState.replaceJobs(items, mutationGeneration);
}

export function createOptimisticJob(job) {
    entityState.createOptimisticJob(job);
}

export function confirmOptimisticJobCreate(optimisticId, job) {
    entityState.confirmOptimisticJobCreate(optimisticId, job);
}

export function rollbackOptimisticJobCreate(optimisticId) {
    entityState.rollbackOptimisticJobCreate(optimisticId);
}

export function cancelOptimisticJob(jobId) {
    entityState.cancelOptimisticJob(jobId);
}

export function confirmOptimisticJobCancel(jobId, job) {
    entityState.confirmOptimisticJobCancel(jobId, job);
}

export function rollbackOptimisticJobCancel(jobId) {
    entityState.rollbackOptimisticJobCancel(jobId);
}
