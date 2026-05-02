import { html, useEffect, useSignal } from '../../deps.js';
import { activeAgentId } from '../../state/agents.js';
import { wsFiles } from '../../state/workspace.js';
import { activePanelTab } from '../../state/panel.js';
import { getWorkspace, openWorkspaceInExplorer, updateWorkspaceFile } from '../../api/workspace.js';

const WS_FILES = ['personality', 'goals', 'memories', 'user'];

async function loadWorkspace() {
    if (!activeAgentId.value) {
        wsFiles.value = null;
        return;
    }
    try {
        const data = await getWorkspace(activeAgentId.value);
        wsFiles.value = data.files || data;
    } catch (err) {
        if (err.status === 404 || err.error?.code === 'NOT_FOUND') {
            wsFiles.value = 'unavailable';
        } else {
            wsFiles.value = 'error';
        }
    }
}

/**
 * #858: ask the gateway to open the agent's workspace directory in the host
 * file explorer. The button stays inline at the top of the workspace panel
 * so it's discoverable next to the file list it acts on. Success flashes a
 * brief "Opened" pill that fades on its own; errors persist until the user
 * clicks again so the failure mode is impossible to miss.
 *
 * Exported so the JS test harness in `tests/ui/` can drive the click handler
 * directly without spinning up a Preact tree.
 */
export function OpenInExplorerButton({ agentId, doOpen }) {
    const busy = useSignal(false);
    const flash = useSignal(null); // { kind: 'ok' | 'err', text }

    const onClick = async () => {
        if (busy.value || !agentId) return;
        busy.value = true;
        flash.value = null;
        try {
            await doOpen(agentId);
            flash.value = { kind: 'ok', text: 'Opened' };
            setTimeout(() => {
                if (flash.value?.kind === 'ok') flash.value = null;
            }, 2000);
        } catch (err) {
            const code = err?.error?.code;
            const msg = err?.error?.message || err?.message || 'Failed to open workspace';
            // Map known error codes to slightly friendlier short labels — the
            // full server message still goes in the title attribute so the
            // operator can hover for details.
            let label = msg;
            if (code === 'NOT_CONFIGURED') {
                label = 'Workspace dir not configured';
            } else if (code === 'WORKSPACE_PATH_MISSING') {
                label = 'Workspace path is missing on disk';
            } else if (code === 'LAUNCHER_FAILED') {
                label = 'Failed to launch file explorer';
            }
            flash.value = { kind: 'err', text: label, full: msg };
        } finally {
            busy.value = false;
        }
    };

    const onKeyDown = (e) => {
        // Native <button> already activates on Enter/Space, but htm props
        // sometimes route through a wrapper that swallows the default —
        // pin Enter/Space here defensively so the keyboard-accessibility
        // acceptance criterion is closed-by-construction.
        if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            onClick();
        }
    };

    return html`
        <div class="ws-open-row">
            <button class="ws-open-btn"
                    type="button"
                    onClick=${onClick}
                    onKeyDown=${onKeyDown}
                    disabled=${busy.value || !agentId}
                    title="Open this agent's workspace directory in the host file explorer"
                    aria-label="Open workspace in file explorer">
                ${busy.value ? 'Opening...' : 'Open in Explorer'}
            </button>
            ${flash.value && html`
                <span class="ws-flash ${flash.value.kind === 'ok' ? 'ok' : 'err'}"
                      title=${flash.value.full || ''}
                      role=${flash.value.kind === 'err' ? 'alert' : 'status'}>
                    ${flash.value.text}
                </span>
            `}
        </div>
    `;
}

function FileEditor({ agentId, filename, content }) {
    const draft = useSignal(content || '');
    const flash = useSignal('');
    const saving = useSignal(false);

    useEffect(() => { draft.value = content || ''; }, [content]);

    const onSave = async () => {
        if (saving.value) return;
        saving.value = true;
        flash.value = '';
        try {
            await updateWorkspaceFile(agentId, filename, draft.value);
            flash.value = 'Saved';
            setTimeout(() => { flash.value = ''; }, 2000);
            await loadWorkspace();
        } catch (err) {
            flash.value = 'Error: ' + (err.error?.message || err.message || 'save failed');
        } finally {
            saving.value = false;
        }
    };

    return html`
        <div class="ws-file">
            <div class="ws-file-label">${filename}</div>
            <textarea class="ws-textarea"
                      rows="6"
                      value=${draft.value}
                      onInput=${e => { draft.value = e.target.value; }}></textarea>
            <div style="display:flex; align-items:center; gap:var(--space-2);">
                <button class="ws-save" onClick=${onSave} disabled=${saving.value}>
                    ${saving.value ? 'Saving...' : 'Save'}
                </button>
                ${flash.value && html`
                    <span class="ws-flash ${flash.value.startsWith('Error') ? 'err' : 'ok'}">
                        ${flash.value}
                    </span>
                `}
            </div>
        </div>
    `;
}

export function WorkspaceTab() {
    useEffect(() => {
        if (activePanelTab.value === 'workspace') loadWorkspace();
    }, [activePanelTab.value, activeAgentId.value]);

    if (!activeAgentId.value) {
        return html`<div class="ws-notice">No agent selected</div>`;
    }
    if (wsFiles.value === null) {
        return html`<div class="loading-state">Loading...</div>`;
    }
    if (wsFiles.value === 'unavailable') {
        // Workspace dir is not configured server-side OR the agent record
        // doesn't have a workspace yet. The Open-in-Explorer button is
        // intentionally NOT shown here — there's no path to open.
        return html`<div class="ws-notice">Workspace not configured for this agent</div>`;
    }
    if (wsFiles.value === 'error') {
        return html`<div class="ws-notice" style="color:var(--error);">Failed to load workspace</div>`;
    }

    return html`
        <div>
            <${OpenInExplorerButton}
                agentId=${activeAgentId.value}
                doOpen=${openWorkspaceInExplorer} />
            ${WS_FILES.map(f => html`
                <${FileEditor}
                    key=${f}
                    agentId=${activeAgentId.value}
                    filename=${f}
                    content=${wsFiles.value[f + '.md'] || wsFiles.value[f] || ''} />
            `)}
        </div>
    `;
}
