import { html, useSignal, useEffect } from '../deps.js';
import { serverDefaults, localSettings, saveSettings } from '../state/settings.js';
import { listKeys, setKey, removeKey } from '../api/auth.js';

const PROVIDERS = ['openai', 'anthropic', 'openrouter'];

function ApiKeysSection() {
    const keys = useSignal([]);
    const editing = useSignal(null);
    const newKey = useSignal('');
    const saving = useSignal(false);
    const error = useSignal('');

    const refresh = async () => {
        try {
            const data = await listKeys();
            keys.value = data.keys || [];
        } catch (err) {
            console.error('[auth] list keys failed:', err);
        }
    };

    useEffect(() => { refresh(); }, []);

    const onSave = async (provider) => {
        if (!newKey.value.trim()) return;
        saving.value = true;
        error.value = '';
        try {
            await setKey(provider, newKey.value.trim());
            newKey.value = '';
            editing.value = null;
            await refresh();
        } catch (err) {
            error.value = err.error?.message || err.message || 'Failed to save key';
        } finally {
            saving.value = false;
        }
    };

    const onRemove = async (provider) => {
        try {
            await removeKey(provider);
            await refresh();
        } catch (err) {
            error.value = err.error?.message || err.message || 'Failed to remove key';
        }
    };

    return html`
        <div class="settings-row">
            <label class="settings-label">API Keys</label>
            ${PROVIDERS.map(p => {
                const info = keys.value.find(k => k.provider === p);
                const configured = info?.configured;
                const source = info?.source || 'none';
                const masked = info?.key || '';

                if (editing.value === p) {
                    return html`
                        <div class="api-key-row" key=${p}>
                            <span class="api-key-provider">${p}</span>
                            <input class="settings-input" type="password"
                                   placeholder="Paste API key..."
                                   value=${newKey.value}
                                   onInput=${e => { newKey.value = e.target.value; }}
                                   onKeyDown=${e => { if (e.key === 'Enter') onSave(p); }} />
                            <div class="api-key-actions">
                                <button class="api-key-btn save" onClick=${() => onSave(p)}
                                        disabled=${saving.value}>
                                    ${saving.value ? '...' : 'Save'}
                                </button>
                                <button class="api-key-btn" onClick=${() => { editing.value = null; newKey.value = ''; }}>
                                    Cancel
                                </button>
                            </div>
                        </div>
                    `;
                }

                return html`
                    <div class="api-key-row" key=${p}>
                        <span class="api-key-provider">${p}</span>
                        <span class="api-key-value ${configured ? 'set' : 'unset'}">
                            ${configured ? masked : 'not configured'}
                        </span>
                        ${configured && source === 'secrets' && html`
                            <span class="api-key-source">stored</span>
                        `}
                        ${configured && source === 'env' && html`
                            <span class="api-key-source">env var</span>
                        `}
                        <div class="api-key-actions">
                            <button class="api-key-btn" onClick=${() => { editing.value = p; newKey.value = ''; }}>
                                ${configured ? 'Change' : 'Set'}
                            </button>
                            ${configured && source === 'secrets' && html`
                                <button class="api-key-btn remove" onClick=${() => onRemove(p)}>Remove</button>
                            `}
                        </div>
                    </div>
                `;
            })}
            ${error.value && html`<div style="color:var(--error); font-size:var(--text-xs);">${error.value}</div>`}
        </div>
    `;
}

export function SettingsModal({ open, onClose }) {
    const model = useSignal('');
    const maxTokens = useSignal('');
    const posture = useSignal('');
    const saved = useSignal(false);

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

                <${ApiKeysSection} />

                <div class="settings-divider"></div>

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
                            <option value="autonomous">autonomous</option>
                        </select>
                    </div>
                </div>

                <div class="settings-divider"></div>

                <div class="settings-row">
                    <label class="settings-label">Server info</label>
                    <div class="settings-info">
                        <div>Version: <span class="settings-info-value">${defaults.version || 'unknown'}</span></div>
                        <div>Model: <span class="settings-info-value">${defaults.model || 'unknown'}</span></div>
                        <div>Base URL: <span class="settings-info-value">${defaults.base_url || 'unknown'}</span></div>
                        <div>Context: <span class="settings-info-value">${defaults.context_strategy || 'truncate'}</span></div>
                        <div>Stream timeout: <span class="settings-info-value">${defaults.stream_chunk_timeout_secs || 60}s</span></div>
                        <div>Tools: <span class="settings-info-value">${(defaults.enabled_tools || []).length} enabled</span></div>
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
