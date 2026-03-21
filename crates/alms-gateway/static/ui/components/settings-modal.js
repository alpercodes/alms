import { html, useSignal, useEffect } from '../deps.js';
import { serverDefaults, localSettings, saveSettings } from '../state/settings.js';

export function SettingsModal({ open, onClose }) {
    const model = useSignal('');
    const maxTokens = useSignal('');
    const posture = useSignal('');
    const saved = useSignal(false);

    // Reset form values when modal opens
    useEffect(() => {
        if (open) {
            model.value = localSettings.value.model || '';
            maxTokens.value = localSettings.value.max_tokens != null
                ? String(localSettings.value.max_tokens)
                : '';
            posture.value = localSettings.value.posture || '';
            saved.value = false;
        }
    }, [open]);

    if (!open) return null;

    const defaults = serverDefaults.value;

    const onSave = () => {
        const updates = {};
        updates.model = model.value.trim() || null;
        const mt = parseInt(maxTokens.value, 10);
        updates.max_tokens = (!isNaN(mt) && mt > 0) ? mt : null;
        updates.posture = posture.value || null;
        saveSettings(updates);
        saved.value = true;
        setTimeout(() => onClose(), 600);
    };

    const onReset = () => {
        saveSettings({ model: null, max_tokens: null, posture: null });
        model.value = '';
        maxTokens.value = '';
        posture.value = '';
        saved.value = true;
        setTimeout(() => onClose(), 600);
    };

    const onOverlayClick = (e) => {
        if (e.target === e.currentTarget) onClose();
    };

    return html`
        <div class="settings-overlay open" onClick=${onOverlayClick}>
            <div class="settings-modal">
                <h2>Settings</h2>

                <div class="settings-row">
                    <label class="settings-label">Model override</label>
                    <input class="settings-input" type="text"
                           placeholder=${defaults.model || 'server default'}
                           value=${model.value}
                           onInput=${e => { model.value = e.target.value; }} />
                    <span class="settings-hint">
                        Override for all runs. Leave empty to use server default (${defaults.model || 'unknown'}).
                    </span>
                </div>

                <div class="settings-grid">
                    <div class="settings-row">
                        <label class="settings-label">Max tokens</label>
                        <input class="settings-input" type="number" min="1"
                               placeholder=${defaults.max_tokens || 4096}
                               value=${maxTokens.value}
                               onInput=${e => { maxTokens.value = e.target.value; }} />
                    </div>

                    <div class="settings-row">
                        <label class="settings-label">Posture</label>
                        <select class="settings-select"
                                value=${posture.value}
                                onChange=${e => { posture.value = e.target.value; }}>
                            <option value="">Default (${defaults.posture || 'guarded'})</option>
                            <option value="full_control">full_control</option>
                            <option value="guarded">guarded</option>
                        </select>
                    </div>
                </div>

                <div class="settings-divider"></div>

                <div class="settings-row">
                    <label class="settings-label">Server info</label>
                    <div class="settings-info">
                        <div>Model: <span class="settings-info-value">${defaults.model || 'unknown'}</span></div>
                        <div>Base URL: <span class="settings-info-value">${defaults.base_url || 'unknown'}</span></div>
                        <div>Context: <span class="settings-info-value">${defaults.context_strategy || 'truncate'}</span></div>
                        <div>Tools: <span class="settings-info-value">${(defaults.enabled_tools || []).length} enabled</span></div>
                        <div>Workspace: <span class="settings-info-value">${defaults.workspace_dir || 'not configured'}</span></div>
                    </div>
                </div>

                <div class="settings-footer">
                    <button class="settings-cancel" onClick=${onReset}>Reset</button>
                    <button class="settings-cancel" onClick=${onClose}>Cancel</button>
                    <button class="settings-save" onClick=${onSave}>
                        ${saved.value ? 'Saved!' : 'Apply'}
                    </button>
                </div>
            </div>
        </div>
    `;
}
