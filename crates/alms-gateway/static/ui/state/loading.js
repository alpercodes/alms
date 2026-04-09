/**
 * Loading state signals for UX feedback.
 *
 * These signals drive loading indicators across the UI:
 *   - sessionSwitchLoading: true while session history is being fetched
 *   - agentSwitchLoading: true while agent sessions are being loaded
 *   - bootRetryAvailable: true when boot has failed and a retry button should be shown
 */

import { signal } from '../deps.js';

export const sessionSwitchLoading = signal(false);
export const agentSwitchLoading = signal(false);
export const bootRetryAvailable = signal(false);

/** Set by app.js during initialization; callable from any module. */
let _runBoot = null;

export function setRunBoot(fn) { _runBoot = fn; }
export function runBoot() { if (_runBoot) _runBoot(); }
