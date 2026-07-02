import { html } from '../../deps.js';
import { activeSubagents, navigateToSubagentSession } from '../../state/subagents.js';
import { subagentStatusLabel } from '../../utils/subagent-status.js';

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
                const onClick = () => {
                    if (info.sessionId) {
                        navigateToSubagentSession(info.sessionId);
                    }
                };
                const tooltip = info.task
                    ? `${label}: ${info.task} — open subagent session`
                    : `${label} — open subagent session`;

                return html`
                    <button class="sa-chip ${isRunning ? 'running' : info.status}"
                            title=${tooltip}
                            onClick=${onClick}>
                        ${isRunning
                            ? html`<span class="tc-spinner"></span>`
                            : html`<span>${icon}</span>`
                        }
                        <span class="sa-chip-name">${label}</span>
                        ${statusLabel && html`<span class="sa-chip-status">${statusLabel}</span>`}
                    </button>
                `;
            })}
        </div>
    `;
}
