import { html, useSignal, useEffect } from '../deps.js';
import { serverDefaults, localSettings, saveSettings } from '../state/settings.js';
import { listKeys, setKey, removeKey } from '../api/auth.js';

const PROVIDERS = ['openai', 'anthropic', 'openrouter'];

/** Format large numbers with commas for readability. */
function fmt(n) {
    if (n == null) return '--';
    return Number(n).toLocaleString();
}

/** Format seconds as a human-readable duration. */
function fmtDuration(secs) {
    if (secs == null) return '--';
    if (secs < 60) return `${secs}s`;
    if (secs < 3600) return `${Math.floor(secs / 60)}m`;
    if (secs < 86400) return `${Math.floor(secs / 3600)}h`;
    return `${Math.floor(secs / 86400)}d`;
}

/** Collapsible section wrapper. */
function Section({ title, defaultOpen = false, children }) {
    const open = useSignal(defaultOpen);
    return html`
        <div class="settings-section">
            <button type="button" class="settings-section-toggle"
                    aria-expanded=${open.value}
                    onClick=${(e) => { e.stopPropagation(); open.value = !open.value; }}>
                <span class="settings-section-arrow ${open.value ? 'open' : ''}">\u25B6</span>
                <span class="settings-section-title">${title}</span>
            </button>
            <div class="settings-section-body ${open.value ? 'open' : ''}">
                ${children}
            </div>
        </div>
    `;
}

/** Read-only info row: label + value + optional description. */
function InfoRow({ label, value, desc }) {
    return html`
        <div class="settings-info-row">
            <div class="settings-info-row-header">
                <span class="settings-info-row-label">${label}</span>
                <span class="settings-info-row-value">${value}</span>
            </div>
            ${desc && html`<span class="settings-hint">${desc}</span>`}
        </div>
    `;
}

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

                const isAlias = source.startsWith('alias:');
                const aliasFrom = isAlias ? source.split(':')[1] : null;
                const hasKey = configured || isAlias;

                return html`
                    <div class="api-key-row" key=${p}>
                        <span class="api-key-provider">${p}</span>
                        <span class="api-key-value ${hasKey ? 'set' : 'unset'}">
                            ${hasKey ? masked : 'not configured'}
                        </span>
                        ${configured && source === 'secrets' && html`
                            <span class="api-key-source">stored</span>
                        `}
                        ${isAlias && html`
                            <span class="api-key-source">via ${aliasFrom}</span>
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
    const provider = useSignal('');
    const model = useSignal('');
    const maxTokens = useSignal('');
    const posture = useSignal('');
    const saved = useSignal(false);

    useEffect(() => {
        if (open) {
            provider.value = localSettings.value.provider || '';
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
    const ctx = defaults.context || {};
    const sess = defaults.session || {};
    const log = defaults.logging || {};
    const tools = defaults.tools || {};

    const onSave = () => {
        const updates = {};
        updates.provider = provider.value || null;
        updates.model = model.value.trim() || null;
        const mt = parseInt(maxTokens.value, 10);
        updates.max_tokens = (!isNaN(mt) && mt > 0) ? mt : null;
        updates.posture = posture.value || null;
        saveSettings(updates);
        saved.value = true;
        setTimeout(() => onClose(), 600);
    };

    const onReset = () => {
        saveSettings({ provider: null, model: null, max_tokens: null, posture: null });
        provider.value = '';
        model.value = '';
        maxTokens.value = '';
        posture.value = '';
        saved.value = true;
        setTimeout(() => onClose(), 600);
    };

    const onOverlayClick = (e) => {
        if (e.target === e.currentTarget) onClose();
    };

    const enabledTools = tools.enabled || defaults.enabled_tools || [];

    return html`
        <div class="settings-overlay open" onClick=${onOverlayClick}>
            <div class="settings-modal">
                <h2>Settings</h2>

                <!-- ── Security: API Keys ── -->
                <${ApiKeysSection} />

                <div class="settings-divider"></div>

                <!-- ── LLM: Per-run overrides (editable) ── -->
                <div class="settings-grid">
                    <div class="settings-row">
                        <label class="settings-label">Provider override</label>
                        <select class="settings-select"
                                value=${provider.value}
                                onChange=${e => { provider.value = e.target.value; }}>
                            <option value="">Default (${defaults.provider || 'openai'})</option>
                            <option value="openai">OpenAI</option>
                            <option value="anthropic">Anthropic</option>
                            <option value="openrouter">OpenRouter</option>
                        </select>
                    </div>

                    <div class="settings-row">
                        <label class="settings-label">Model override</label>
                        <input class="settings-input" type="text"
                               placeholder=${defaults.model || 'server default'}
                               value=${model.value}
                               onInput=${e => { model.value = e.target.value; }} />
                        <span class="settings-hint">
                            Leave empty to use server default (${defaults.model || 'unknown'}).
                        </span>
                    </div>
                </div>

                <div class="settings-grid">
                    <div class="settings-row">
                        <label class="settings-label">Max tokens</label>
                        <input class="settings-input" type="number" min="1"
                               placeholder=${defaults.max_tokens || 100000}
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

                <!-- ── Context (server-level, read-only) ── -->
                <${Section} key="ctx" title="Context" defaultOpen=${false}>
                    <span class="settings-hint settings-section-desc">
                        Controls how conversation history is assembled for each LLM request. Edit in alms.toml under [context].
                    </span>
                    <${InfoRow} label="Strategy" value=${ctx.strategy || 'truncate'}
                        desc="truncate = drop oldest messages. full = send all. sliding-summary = summarize old + keep recent verbatim." />
                    <${InfoRow} label="Max input tokens" value=${fmt(ctx.max_input_tokens)}
                        desc="Token budget per LLM request (should match your model's context window)." />
                    <${InfoRow} label="Recent window" value=${ctx.recent_window ?? '--'}
                        desc="For sliding-summary: number of recent messages kept verbatim." />
                    <${InfoRow} label="Summary interval" value=${ctx.summary_interval ?? '--'}
                        desc="Messages between summary regenerations (sliding-summary only)." />
                    <${InfoRow} label="Summary model" value=${ctx.summary_model || 'same as default'}
                        desc="Optional cheaper model for generating summaries." />
                <//>

                <!-- ── Session (server-level, read-only) ── -->
                <${Section} key="sess" title="Session" defaultOpen=${false}>
                    <span class="settings-hint settings-section-desc">
                        Controls session storage and retention. Edit in alms.toml under [session].
                    </span>
                    <${InfoRow} label="Max messages" value=${fmt(sess.max_messages)}
                        desc="Maximum messages stored per session." />
                    <${InfoRow} label="Max context tokens" value=${fmt(sess.max_context_tokens)}
                        desc="Maximum tokens retained in session history (storage limit, must be >= context max_input_tokens)." />
                    <${InfoRow} label="Idle timeout" value=${fmtDuration(sess.idle_timeout_secs)}
                        desc="Time before a session is considered idle." />
                    <${InfoRow} label="Auto archive" value=${sess.auto_archive != null ? (sess.auto_archive ? 'yes' : 'no') : '--'}
                        desc="Automatically archive idle sessions." />
                    <${InfoRow} label="Archive TTL" value=${fmtDuration(sess.archive_ttl_secs)}
                        desc="Delete archived sessions after this duration." />
                <//>

                <!-- ── Tools (server-level, read-only) ── -->
                <${Section} key="tools" title="Tools" defaultOpen=${false}>
                    <span class="settings-hint settings-section-desc">
                        Tool execution settings. Edit in alms.toml under [tools].
                    </span>
                    <${InfoRow} label="Shell policy" value=${tools.shell_policy || 'sandboxed'}
                        desc="sandboxed = restrict shell cwd to sandbox root. unrestricted = no cwd restriction." />
                    <${InfoRow} label="Sandbox root" value=${tools.sandbox_root || '.'}
                        desc="Filesystem sandbox root for fs_* tools. Empty = unrestricted." />
                    <${InfoRow} label="Tool timeout" value=${fmtDuration(tools.timeout_secs)}
                        desc="Maximum execution time per tool call." />
                    <${InfoRow} label="Max output" value=${tools.max_output_bytes != null ? `${fmt(tools.max_output_bytes)} bytes` : '--'}
                        desc="Maximum bytes returned from a single tool call." />
                    <${InfoRow} label="Enabled tools" value=${`${enabledTools.length} tools`}
                        desc=${enabledTools.join(', ')} />
                <//>

                <!-- ── Logging (server-level, read-only) ── -->
                <${Section} key="log" title="Logging" defaultOpen=${false}>
                    <span class="settings-hint settings-section-desc">
                        File-based logging settings. Edit in alms.toml under [logging]. Requires restart.
                    </span>
                    <${InfoRow} label="File logging" value=${log.file_enabled != null ? (log.file_enabled ? 'enabled' : 'disabled') : '--'}
                        desc="Whether persistent file logging is active." />
                    <${InfoRow} label="File level" value=${log.file_level || '--'}
                        desc="Log level for file output (trace, debug, info, warn, error)." />
                    <${InfoRow} label="Rotation" value=${log.rotation || '--'}
                        desc="Log rotation policy: daily, hourly, or never." />
                    <${InfoRow} label="Log directory" value=${log.log_dir || 'default (data/logs/)'}
                        desc="Directory where log files are written." />
                <//>

                <div class="settings-divider"></div>

                <!-- ── Server info (compact) ── -->
                <div class="settings-row">
                    <label class="settings-label">Server info</label>
                    <div class="settings-info">
                        <div>Version: <span class="settings-info-value">${defaults.version || 'unknown'}</span></div>
                        <div>Base URL: <span class="settings-info-value">${defaults.base_url || 'unknown'}</span></div>
                        <div>Stream timeout: <span class="settings-info-value">${defaults.stream_chunk_timeout_secs || 60}s</span></div>
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
