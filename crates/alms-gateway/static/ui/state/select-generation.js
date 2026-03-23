/**
 * Shared generation counter for selectSession() concurrency guard.
 *
 * Bumped by selectSession() (session-list.js) and switchAgent() (use-boot.js)
 * so that in-flight fetches from either path are invalidated when the user
 * switches sessions or agents.
 */
export let selectGeneration = 0;

export function bumpSelectGeneration() {
    return ++selectGeneration;
}
