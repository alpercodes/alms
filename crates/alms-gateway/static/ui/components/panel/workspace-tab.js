import { html, useEffect, useSignal } from '../../deps.js';
import { activeAgentId } from '../../state/agents.js';
import { wsFiles } from '../../state/workspace.js';
import { activePanelTab } from '../../state/panel.js';
import { getWorkspace, updateWorkspaceFile } from '../../api/workspace.js';

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

function FileEditor({ agentId, filename, content }) {
    const draft = useSignal(content || '');
    const flash = useSignal('');

    useEffect(() => { draft.value = content || ''; }, [content]);

    const onSave = async () => {
        try {
            await updateWorkspaceFile(agentId, filename, draft.value);
            flash.value = 'Saved';
            setTimeout(() => { flash.value = ''; }, 2000);
            await loadWorkspace();
        } catch (err) {
            flash.value = 'Error: ' + (err.error?.message || err.message || 'save failed');
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
                <button class="ws-save" onClick=${onSave}>Save</button>
                ${flash.value && html`
                    <span class="ws-flash" style="color:${flash.value.startsWith('Error') ? 'var(--error)' : 'var(--success)'}">
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
        return html`<div style="color:var(--text-disabled); padding:var(--space-4); font-size:var(--text-sm);">Loading...</div>`;
    }
    if (wsFiles.value === 'unavailable') {
        return html`<div class="ws-notice">Workspace not configured for this agent</div>`;
    }
    if (wsFiles.value === 'error') {
        return html`<div class="ws-notice" style="color:var(--error);">Failed to load workspace</div>`;
    }

    return html`
        <div>
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
