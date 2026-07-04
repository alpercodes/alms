import { html } from '../../deps.js';
import { activeSubagents, navigateToSubagentSession } from '../../state/subagents.js';
import { subagentStatusLabel } from '../../utils/subagent-status.js';
import {
    showCancelControl,
    isCancelPending,
    requestSubagentCancel,
    dismissSubagentCancel,
    confirmSubagentCancel,
} from '../../state/subagent-cancel.js';
import { shouldActivateFromClick, shouldActivateFromKey } from '../../utils/card-activation.js';

/**
 * Subagent status bar — the live subagent widget above the message input.
 *
 * A lightweight STATUS indicator: one chip per active subagent showing the
 * subagent's name and a concise label for its most recent activity
 * ("Reasoning…", "Using {tool}", "Writing…"; "Starting…" before the first
 * signal, then "Done" / "Failed"). The subagent's streamed reasoning/token
 * content is deliberately NOT shown here — the full transcript streams to
 * the subagent's own session (#1184). Clicking a chip navigates directly to
 * that session (no expand-in-place panel).
 *
 * Status is driven by the ephemeral `subagent_activity` SSE signals (see
 * `state/subagents.js` / `utils/subagent-status.js`); chip lifecycle (start,
 * completion, auto-removal, reload rehydration) is unchanged.
 *
 * RUNNING chips with a known `sessionId` additionally carry a small ✕
 * control that cancels the subagent — guarded by an inline Yes/No confirm
 * step (the sidebar delete-button pattern; state machine in
 * `state/subagent-cancel.js`) since the chip is a tiny click target and a
 * misclick must not kill a subagent. Terminal chips and chips whose
 * session id hasn't arrived yet render no cancel control at all.
 *
 * The chip root is a `role="button"` div (not a `<button>`) so the nested
 * cancel controls stay valid HTML; navigation activation mirrors the #983
 * whole-card pattern (`utils/card-activation.js`), and every nested
 * control stops propagation so interacting with it never also navigates.
 */
export function SubagentBar() {
    const entries = Object.entries(activeSubagents.value);
    if (entries.length === 0) return null;

    return html`
        <div class="sa-bar" aria-label="Subagent status bar">
            ${entries.map(([name, info]) => {
                const isRunning = info.status === 'running';
                const icon = info.status === 'done' ? '✓' : '✗';
                const label = info.displayName || name;
                const statusLabel = subagentStatusLabel(info);
                // Navigate straight to the subagent's session. The session id
                // can lag the chip by a moment (foreground subagents receive
                // it via `subagent_started`); until then the click is a no-op.
                const navigate = () => {
                    if (info.sessionId) {
                        navigateToSubagentSession(info.sessionId);
                    }
                };
                const onClick = (e) => {
                    if (shouldActivateFromClick(e)) navigate();
                };
                const onKeyDown = (e) => {
                    if (shouldActivateFromKey(e)) {
                        e.preventDefault();
                        navigate();
                    }
                };
                const tooltip = info.task
                    ? `${label}: ${info.task} — open subagent session`
                    : `${label} — open subagent session`;

                // Cancel affordance (RUNNING + known sessionId only, per
                // `showCancelControl`). Two-step: ✕ arms the inline Yes/No
                // confirm; only an explicit Yes calls the cancel endpoint.
                // Every nested handler stops propagation so the chip never
                // also navigates; keyboard activation additionally
                // preventDefaults so the chip root's Enter/Space handler
                // (which honours `defaultPrevented`) stays quiet. Escape
                // dismisses the armed confirm (pairs with the auto-revert
                // timer); on the unarmed ✕ it is a harmless no-op.
                const confirming = isCancelPending(info.sessionId);
                const nestedKey = (handler) => (e) => {
                    e.stopPropagation();
                    if (e.key === 'Escape') {
                        e.preventDefault();
                        dismissSubagentCancel();
                        return;
                    }
                    if (e.key === 'Enter' || e.key === ' ') {
                        e.preventDefault();
                        handler(e);
                    }
                };
                const onCancelClick = (e) => {
                    e.stopPropagation();
                    requestSubagentCancel(info.sessionId);
                };
                const onCancelYes = (e) => {
                    e.stopPropagation();
                    confirmSubagentCancel(info.sessionId);
                };
                const onCancelNo = (e) => {
                    e.stopPropagation();
                    dismissSubagentCancel();
                };

                return html`
                    <div class="sa-chip ${isRunning ? 'running' : info.status}"
                         role="button"
                         tabindex="0"
                         title=${tooltip}
                         onClick=${onClick}
                         onKeyDown=${onKeyDown}>
                        ${isRunning
                            ? html`<span class="tc-spinner"></span>`
                            : html`<span>${icon}</span>`
                        }
                        <span class="sa-chip-name">${label}</span>
                        ${statusLabel && html`<span class="sa-chip-status">${statusLabel}</span>`}
                        ${showCancelControl(info) && (confirming
                            ? html`
                                <span class="sa-cancel-confirm-group" role="group"
                                      aria-label="Confirm cancel subagent">
                                    <span class="sa-cancel-confirm-label">Cancel?</span>
                                    <button class="sa-confirm-btn sa-confirm-yes"
                                            title="Yes, cancel this subagent"
                                            aria-label="Yes, cancel this subagent"
                                            onClick=${onCancelYes}
                                            onKeyDown=${nestedKey(onCancelYes)}>Yes</button>
                                    <button class="sa-confirm-btn sa-confirm-no"
                                            title="No, keep it running"
                                            aria-label="No, keep it running"
                                            onClick=${onCancelNo}
                                            onKeyDown=${nestedKey(onCancelNo)}>No</button>
                                </span>
                            `
                            : html`
                                <button class="sa-chip-cancel"
                                        title="Cancel this subagent"
                                        aria-label="Cancel this subagent"
                                        onClick=${onCancelClick}
                                        onKeyDown=${nestedKey(onCancelClick)}>✕</button>
                            `
                        )}
                    </div>
                `;
            })}
        </div>
    `;
}
