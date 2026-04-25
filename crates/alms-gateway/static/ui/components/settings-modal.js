import { html, useSignal, useEffect, computed } from '../deps.js';
import { serverDefaults, localSettings, saveSettings, refreshServerDefaults } from '../state/settings.js';
import { patchSettings } from '../api/settings.js';
import { listKeys, setKey, removeKey } from '../api/auth.js';
import {
    MODEL_SUGGESTIONS,
    ModelDisplay,
    ProviderDisplay,
    formatProviderLabel,
} from '../utils/model-display.js';
import { BudgetTriState } from './budget-tri-state.js';

const REASONING_EFFORTS = ['minimal', 'low', 'medium', 'high'];

/**
 * Modal-flavored class map for the shared <BudgetTriState>. Keeps the
 * tri-state visually consistent with the rest of the modal's per-run
 * section (settings-row / settings-label / settings-select) instead of
 * the composer's `composer-advanced-*` classes.
 */
const MODAL_TRISTATE_CLASSES = {
    row: 'settings-row',
    label: 'settings-label',
    group: 'settings-tristate-group',
    select: 'settings-select',
    input: 'settings-input settings-tristate-value',
};

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
            <div class="settings-section-body ${open.value ? 'open' : ''}"
                 aria-hidden=${!open.value}>
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

/** Editable row: label + input + optional description. */
function EditRow({ label, desc, children }) {
    return html`
        <div class="settings-info-row">
            <div class="settings-info-row-header" style="flex-wrap:wrap;gap:6px;">
                <span class="settings-info-row-label">${label}</span>
                ${children}
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
                                   autocomplete="off"
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
            ${error.value && html`<div class="inline-error">${error.value}</div>`}
        </div>
    `;
}

/**
 * Count how many per-run overrides are currently active.
 * Exported so the header (and composer Advanced expander) can show an
 * indicator badge.
 *
 * Includes the legacy fields plus `debug_mode` and the three reasoning
 * knobs (`thinking_budget_tokens`, `reasoning_effort`,
 * `gemini_thinking_budget`). For the budget knobs, `Some(0)` (explicit
 * disable) and any positive value both count as "active overrides";
 * only `null`/`undefined` is "inherit". For `debug_mode`, both `true`
 * and `false` count — `null` is inherit.
 */
export const activeOverrideCount = computed(() => {
    const s = localSettings.value;
    let count = 0;
    if (s.provider) count++;
    if (s.model) count++;
    if (s.max_tokens != null) count++;
    if (s.posture) count++;
    // debug_mode is now tri-state on the composer (#818 nit follow-up):
    // null = inherit, true = enable, false = explicit per-run disable.
    // Both `true` and `false` count as active overrides.
    if (s.debug_mode != null) count++;
    if (s.thinking_budget_tokens != null) count++;
    if (s.reasoning_effort) count++;
    if (s.gemini_thinking_budget != null) count++;
    return count;
});

export function SettingsModal({ open, onClose }) {
    const provider = useSignal('');
    const model = useSignal('');
    const maxTokens = useSignal('');
    const posture = useSignal('');
    // debug_mode is tri-state to mirror the composer: '' = inherit
    // (null), 'true' = enable, 'false' = explicit disable. The composer's
    // tri-state introduced meaningful `false`, so the modal must round-trip
    // it without collapsing back to `null` on Apply.
    const debugMode = useSignal('');
    const saved = useSignal(false);

    // Server-level editable signals — Context
    const ctxStrategy = useSignal('');
    const ctxMaxInput = useSignal('');
    const ctxRecentWindow = useSignal('');
    const ctxSummaryInterval = useSignal('');
    const ctxSummaryModel = useSignal('');

    // Server-level editable signals — Session
    const sessMaxMessages = useSignal('');
    const sessMaxCtxTokens = useSignal('');
    const sessIdleTimeout = useSignal('');
    const sessAutoArchive = useSignal(true);
    const sessArchiveTtl = useSignal('');

    // Server-level editable signals — Tools
    const toolsShellPolicy = useSignal('');
    const toolsSandboxRoot = useSignal('');
    const toolsTimeout = useSignal('');
    const toolsMaxOutput = useSignal('');

    // Server-level editable signals — LLM provider-family (#809 / #804 Slice A).
    // Numeric strings are the form representation; an empty string means
    // "don't include this field in the PATCH body" (= leave server alone).
    // Each field is sent on Apply only if it differs from the current
    // server-reported value, mirroring the existing context/session/tools
    // diff-on-Apply pattern.
    const llmAnthropicThinking = useSignal('');
    const llmAnthropicCache = useSignal(true);
    const llmAnthropicCacheTouched = useSignal(false);
    const llmOpenaiEffort = useSignal('');
    const llmGeminiThinking = useSignal('');
    const llmGeminiCache = useSignal(true);
    const llmGeminiCacheTouched = useSignal(false);
    const llmGeminiCacheTtl = useSignal('');

    // Feedback for server settings save
    const serverSaving = useSignal(false);
    const serverError = useSignal('');

    useEffect(() => {
        if (open) {
            const d = serverDefaults.value;
            const ctx = d.context || {};
            const sess = d.session || {};
            const tools = d.tools || {};
            const llm = d.llm || {};
            const llmAnth = llm.anthropic || {};
            const llmOpen = llm.openai || {};
            const llmGem = llm.gemini || {};

            provider.value = localSettings.value.provider || '';
            model.value = localSettings.value.model || '';
            maxTokens.value = localSettings.value.max_tokens != null
                ? String(localSettings.value.max_tokens)
                : '';
            posture.value = localSettings.value.posture || '';
            const dm = localSettings.value.debug_mode;
            debugMode.value = dm == null ? '' : (dm ? 'true' : 'false');

            // Populate server-level fields
            ctxStrategy.value = ctx.strategy || 'truncate';
            ctxMaxInput.value = ctx.max_input_tokens != null ? String(ctx.max_input_tokens) : '';
            ctxRecentWindow.value = ctx.recent_window != null ? String(ctx.recent_window) : '';
            ctxSummaryInterval.value = ctx.summary_interval != null ? String(ctx.summary_interval) : '';
            ctxSummaryModel.value = ctx.summary_model || '';

            sessMaxMessages.value = sess.max_messages != null ? String(sess.max_messages) : '';
            sessMaxCtxTokens.value = sess.max_context_tokens != null ? String(sess.max_context_tokens) : '';
            sessIdleTimeout.value = sess.idle_timeout_secs != null ? String(sess.idle_timeout_secs) : '';
            sessAutoArchive.value = sess.auto_archive != null ? sess.auto_archive : true;
            sessArchiveTtl.value = sess.archive_ttl_secs != null ? String(sess.archive_ttl_secs) : '';

            toolsShellPolicy.value = tools.shell_policy || 'sandboxed';
            toolsSandboxRoot.value = tools.sandbox_root || '.';
            toolsTimeout.value = tools.timeout_secs != null ? String(tools.timeout_secs) : '';
            toolsMaxOutput.value = tools.max_output_bytes != null ? String(tools.max_output_bytes) : '';

            // LLM provider-family populate. The wire shape always emits
            // every key (with `null` for openai.reasoning_effort when
            // unset), so we initialise from whatever the server reports
            // and only PATCH fields the user actively changes.
            llmAnthropicThinking.value = llmAnth.thinking_budget_tokens != null
                ? String(llmAnth.thinking_budget_tokens) : '';
            llmAnthropicCache.value = llmAnth.prompt_cache_enabled != null
                ? !!llmAnth.prompt_cache_enabled : true;
            llmAnthropicCacheTouched.value = false;
            // OpenAI reasoning_effort: '' represents "no override" / "cleared"
            // (server returns `null` here). When the user picks an empty
            // option, we send `""` on the wire to clear an existing override.
            llmOpenaiEffort.value = llmOpen.reasoning_effort || '';
            llmGeminiThinking.value = llmGem.thinking_budget != null
                ? String(llmGem.thinking_budget) : '';
            llmGeminiCache.value = llmGem.cache_enabled != null
                ? !!llmGem.cache_enabled : true;
            llmGeminiCacheTouched.value = false;
            llmGeminiCacheTtl.value = llmGem.cache_ttl_seconds != null
                ? String(llmGem.cache_ttl_seconds) : '';

            saved.value = false;

            serverError.value = '';
        }
    }, [open]);

    if (!open) return null;

    const defaults = serverDefaults.value;
    const ctx = defaults.context || {};
    const sess = defaults.session || {};
    const log = defaults.logging || {};
    const tools = defaults.tools || {};
    const llm = defaults.llm || {};
    const llmAnth = llm.anthropic || {};
    const llmOpen = llm.openai || {};
    const llmGem = llm.gemini || {};

    const onReset = () => {
        // Clear all per-run overrides — including the three reasoning
        // knobs added by #804 Slice C — so the modal's Reset button
        // mirrors the composer Advanced expander's "Reset all". Both
        // surfaces share the same eight localSettings keys.
        saveSettings({
            provider: null,
            model: null,
            max_tokens: null,
            posture: null,
            debug_mode: null,
            thinking_budget_tokens: null,
            reasoning_effort: null,
            gemini_thinking_budget: null,
        });
        provider.value = '';
        model.value = '';
        maxTokens.value = '';
        posture.value = '';
        debugMode.value = '';
        saved.value = true;
        setTimeout(() => onClose(), 600);
    };

    /** Single Apply handler: saves per-run overrides to localStorage AND patches server settings. */
    const onApply = async () => {
        serverSaving.value = true;
        serverError.value = '';
        saved.value = false;

        // 1. Always save per-run overrides to localStorage (this never fails)
        const updates = {};
        updates.provider = provider.value || null;
        updates.model = model.value.trim() || null;
        const mt = parseInt(maxTokens.value, 10);
        updates.max_tokens = (!isNaN(mt) && mt > 0) ? mt : null;
        updates.posture = posture.value || null;
        // Tri-state debug_mode: '' = inherit (null), 'true' / 'false'
        // round-trip as explicit booleans so the modal doesn't clobber an
        // explicit `false` set from the composer.
        if (debugMode.value === '') updates.debug_mode = null;
        else updates.debug_mode = debugMode.value === 'true';
        saveSettings(updates);

        // 2. Build server settings patch — only include fields that changed from server defaults
        const body = {};

        const ctxPatch = {};
        if (ctxStrategy.value && ctxStrategy.value !== (ctx.strategy || '')) {
            ctxPatch.strategy = ctxStrategy.value;
        }
        const newMaxInput = parseInt(ctxMaxInput.value, 10);
        if (!isNaN(newMaxInput) && newMaxInput !== ctx.max_input_tokens) {
            ctxPatch.max_input_tokens = newMaxInput;
        }
        const newRecent = parseInt(ctxRecentWindow.value, 10);
        if (!isNaN(newRecent) && newRecent !== ctx.recent_window) {
            ctxPatch.recent_window = newRecent;
        }
        const newSummaryInt = parseInt(ctxSummaryInterval.value, 10);
        if (!isNaN(newSummaryInt) && newSummaryInt !== ctx.summary_interval) {
            ctxPatch.summary_interval = newSummaryInt;
        }
        if (ctxSummaryModel.value !== (ctx.summary_model || '')) {
            ctxPatch.summary_model = ctxSummaryModel.value;
        }
        if (Object.keys(ctxPatch).length > 0) body.context = ctxPatch;

        const sessPatch = {};
        const newMaxMsg = parseInt(sessMaxMessages.value, 10);
        if (!isNaN(newMaxMsg) && newMaxMsg !== sess.max_messages) {
            sessPatch.max_messages = newMaxMsg;
        }
        const newMaxCtx = parseInt(sessMaxCtxTokens.value, 10);
        if (!isNaN(newMaxCtx) && newMaxCtx !== sess.max_context_tokens) {
            sessPatch.max_context_tokens = newMaxCtx;
        }
        const newIdle = parseInt(sessIdleTimeout.value, 10);
        if (!isNaN(newIdle) && newIdle !== sess.idle_timeout_secs) {
            sessPatch.idle_timeout_secs = newIdle;
        }
        if (sessAutoArchive.value !== sess.auto_archive) {
            sessPatch.auto_archive = sessAutoArchive.value;
        }
        const newTtl = parseInt(sessArchiveTtl.value, 10);
        if (!isNaN(newTtl) && newTtl !== sess.archive_ttl_secs) {
            sessPatch.archive_ttl_secs = newTtl;
        }
        if (Object.keys(sessPatch).length > 0) body.session = sessPatch;

        const toolsPatch = {};
        if (toolsShellPolicy.value && toolsShellPolicy.value !== (tools.shell_policy || '')) {
            toolsPatch.shell_policy = toolsShellPolicy.value;
        }
        if (toolsSandboxRoot.value !== (tools.sandbox_root || '')) {
            toolsPatch.sandbox_root = toolsSandboxRoot.value;
        }
        const newTimeout = parseInt(toolsTimeout.value, 10);
        if (!isNaN(newTimeout) && newTimeout !== tools.timeout_secs) {
            toolsPatch.timeout_secs = newTimeout;
        }
        const newMaxOut = parseInt(toolsMaxOutput.value, 10);
        if (!isNaN(newMaxOut) && newMaxOut !== tools.max_output_bytes) {
            toolsPatch.max_output_bytes = newMaxOut;
        }
        if (Object.keys(toolsPatch).length > 0) body.tools = toolsPatch;

        // LLM provider-family (#809 / #804 Slice A). Each provider sub-block
        // is built independently and only attached if it has at least one
        // changed field. Numeric "cleared" inputs (empty string) are treated
        // as "leave server alone" — the field is omitted from the PATCH.
        // Booleans are only sent when the user actively toggled them, since
        // there's no `null` sentinel for booleans on this surface.
        const llmPatch = {};

        const anthPatch = {};
        const newAnthThink = parseInt(llmAnthropicThinking.value, 10);
        // Allow `0` (= explicit disable) — it differs from `''` (cleared form
        // input = leave alone).
        if (llmAnthropicThinking.value !== '' && !isNaN(newAnthThink)
            && newAnthThink !== llmAnth.thinking_budget_tokens) {
            anthPatch.thinking_budget_tokens = newAnthThink;
        }
        if (llmAnthropicCacheTouched.value
            && llmAnthropicCache.value !== !!llmAnth.prompt_cache_enabled) {
            anthPatch.prompt_cache_enabled = llmAnthropicCache.value;
        }
        if (Object.keys(anthPatch).length > 0) llmPatch.anthropic = anthPatch;

        const openPatch = {};
        // OpenAI reasoning_effort uses `""` as the explicit-clear sentinel.
        // The form's empty option corresponds to "(unset)" -> send `""`
        // to clear an existing PATCH'd value back to null. If the form
        // value matches the server value (both empty, or both equal
        // strings), we don't send the field.
        const currentEffort = llmOpen.reasoning_effort || '';
        if (llmOpenaiEffort.value !== currentEffort) {
            openPatch.reasoning_effort = llmOpenaiEffort.value;
        }
        if (Object.keys(openPatch).length > 0) llmPatch.openai = openPatch;

        const gemPatch = {};
        const newGemThink = parseInt(llmGeminiThinking.value, 10);
        if (llmGeminiThinking.value !== '' && !isNaN(newGemThink)
            && newGemThink !== llmGem.thinking_budget) {
            gemPatch.thinking_budget = newGemThink;
        }
        if (llmGeminiCacheTouched.value
            && llmGeminiCache.value !== !!llmGem.cache_enabled) {
            gemPatch.cache_enabled = llmGeminiCache.value;
        }
        const newGemTtl = parseInt(llmGeminiCacheTtl.value, 10);
        // Pass `0` through to the backend so a 422 surfaces if the operator
        // typed it intentionally — backend is the source-of-truth on
        // validation; we no longer silently swallow it client-side.
        if (llmGeminiCacheTtl.value !== '' && !isNaN(newGemTtl)
            && newGemTtl !== llmGem.cache_ttl_seconds) {
            gemPatch.cache_ttl_seconds = newGemTtl;
        }
        if (Object.keys(gemPatch).length > 0) llmPatch.gemini = gemPatch;

        if (Object.keys(llmPatch).length > 0) body.llm = llmPatch;

        // 3. If there are server-level changes, PATCH them
        if (Object.keys(body).length > 0) {
            try {
                await patchSettings(body);
                await refreshServerDefaults();
            } catch (err) {
                // 422 responses have { errors: ["...", "..."] } spread onto the thrown object
                const msgs = Array.isArray(err.errors) ? err.errors.join('; ') : null;
                serverError.value = msgs || err.message || 'Failed to save server settings';
            }
        }

        // 4. Show success feedback — per-run overrides are always saved even if server patch failed
        saved.value = true;
        serverSaving.value = false;

        // Close after brief feedback, unless there was a server error
        if (!serverError.value) {
            setTimeout(() => onClose(), 600);
        }
    };

    const onOverlayClick = (e) => {
        if (e.target === e.currentTarget) onClose();
    };

    const enabledTools = tools.enabled || defaults.enabled_tools || [];

    // Effective values: what the next run will actually use.
    // (Provider and model Effective lines use <ProviderDisplay> / <ModelDisplay>
    // which derive the effective value internally.)
    const effMaxTokens = maxTokens.value ? parseInt(maxTokens.value, 10) : (defaults.max_tokens || 100000);
    const effPosture = posture.value || defaults.posture || 'guarded';

    return html`
        <div class="settings-overlay open" onClick=${onOverlayClick}>
            <div class="settings-modal">
                <h2>Settings</h2>

                <!-- Security: API Keys -->
                <${ApiKeysSection} />

                <div class="settings-divider"></div>

                <!-- Per-run overrides (editable) -->
                <div class="settings-overrides-header">
                    <span class="settings-label">Per-run overrides</span>
                    <span class="settings-hint">
                        Applied to every new run. Leave empty to use server defaults.
                    </span>
                </div>

                <div class="settings-grid">
                    <div class="settings-row">
                        <label class="settings-label">Provider</label>
                        <select class="settings-select"
                                value=${provider.value}
                                onChange=${e => { provider.value = e.target.value; }}>
                            <option value="">Default (${formatProviderLabel(defaults.provider || 'openai')})</option>
                            <option value="openai">OpenAI</option>
                            <option value="anthropic">Anthropic</option>
                            <option value="openrouter">OpenRouter</option>
                        </select>
                        <span class="settings-effective">
                            Effective: <${ProviderDisplay} value=${provider.value} defaultValue=${defaults.provider || 'openai'} />
                        </span>
                    </div>

                    <div class="settings-row">
                        <label class="settings-label">Model</label>
                        <input class="settings-input" type="text"
                               list="model-suggestions"
                               placeholder=${defaults.model || 'server default'}
                               value=${model.value}
                               onInput=${e => { model.value = e.target.value; }} />
                        <datalist id="model-suggestions">
                            ${MODEL_SUGGESTIONS.map(m => html`<option value=${m} />`)}
                        </datalist>
                        <span class="settings-effective">
                            Effective: <${ModelDisplay} value=${model.value.trim()} defaultValue=${defaults.model} />
                        </span>
                    </div>
                </div>

                <div class="settings-grid">
                    <div class="settings-row">
                        <label class="settings-label">Max tokens</label>
                        <input class="settings-input" type="number" min="1" step="1000"
                               placeholder=${defaults.max_tokens || 100000}
                               value=${maxTokens.value}
                               onInput=${e => { maxTokens.value = e.target.value; }} />
                        <span class="settings-effective">
                            Effective: ${fmt(effMaxTokens)}
                        </span>
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
                        <span class="settings-effective">
                            Effective: ${effPosture}
                        </span>
                    </div>
                </div>

                <div class="settings-grid">
                    <div class="settings-row">
                        <label class="settings-label">Debug mode</label>
                        <select class="settings-select"
                                value=${debugMode.value}
                                onChange=${e => { debugMode.value = e.target.value; }}>
                            <option value="">Inherit (default)</option>
                            <option value="true">On</option>
                            <option value="false">Off</option>
                        </select>
                        <span class="settings-hint">
                            When On, shows the full context window sent to the LLM before each response.
                            Off explicitly disables it for the next run even if the agent has it enabled.
                        </span>
                    </div>
                </div>

                <!--
                    Reasoning & thinking knobs (#804 Slice C parity).

                    These three knobs were previously only surfaced via the
                    composer's Advanced expander (#818). They share the same
                    `localSettings` keys, so a value set from either surface
                    is visible to both — this section just makes the modal a
                    full editor too. Live-write to localSettings (no Apply
                    needed for these specific rows) so changes round-trip
                    through the same path as the composer.
                -->
                <div class="settings-overrides-subheader">
                    <span class="settings-label">Reasoning &amp; thinking</span>
                    <span class="settings-hint">
                        Provider-specific. Silently ignored when the effective
                        provider doesn't support the knob.
                    </span>
                </div>

                <div class="settings-grid">
                    <${BudgetTriState}
                        label="Anthropic thinking budget"
                        wireValue=${localSettings.value.thinking_budget_tokens}
                        onWrite=${(v) => saveSettings({ thinking_budget_tokens: v })}
                        classNames=${MODAL_TRISTATE_CLASSES} />

                    <div class="settings-row">
                        <label class="settings-label">OpenAI reasoning effort</label>
                        <select class="settings-select"
                                value=${localSettings.value.reasoning_effort || ''}
                                onChange=${e => saveSettings({ reasoning_effort: e.target.value || null })}>
                            <option value="">Inherit (server default)</option>
                            ${REASONING_EFFORTS.map(eff => html`
                                <option value=${eff} key=${eff}>${eff}</option>
                            `)}
                        </select>
                    </div>
                </div>

                <div class="settings-grid">
                    <${BudgetTriState}
                        label="Gemini thinking budget"
                        wireValue=${localSettings.value.gemini_thinking_budget}
                        onWrite=${(v) => saveSettings({ gemini_thinking_budget: v })}
                        classNames=${MODAL_TRISTATE_CLASSES} />
                </div>

                <div class="settings-divider"></div>

                <!-- Context (server-level, editable) -->
                <${Section} key="ctx" title="Context" defaultOpen=${false}>
                    <span class="settings-hint settings-section-desc">
                        Controls how conversation history is assembled for each LLM request. Changes apply to the next run.
                    </span>
                    <${EditRow} label="Strategy"
                        desc="truncate = drop oldest messages. full = send all. sliding-summary = summarize old + keep recent verbatim.">
                        <select class="settings-select settings-input-sm"
                                value=${ctxStrategy.value}
                                onChange=${e => { ctxStrategy.value = e.target.value; }}>
                            <option value="truncate">truncate</option>
                            <option value="full">full</option>
                            <option value="sliding-summary">sliding-summary</option>
                        </select>
                    <//>
                    <${EditRow} label="Max input tokens"
                        desc="Token budget per LLM request (should match your model's context window).">
                        <input class="settings-input settings-input-sm" type="number" min="1" step="1000"
                               value=${ctxMaxInput.value}
                               onInput=${e => { ctxMaxInput.value = e.target.value; }} />
                    <//>
                    <${EditRow} label="Recent window"
                        desc="For sliding-summary: number of recent messages kept verbatim.">
                        <input class="settings-input settings-input-sm" type="number" min="1"
                               value=${ctxRecentWindow.value}
                               onInput=${e => { ctxRecentWindow.value = e.target.value; }} />
                    <//>
                    <${EditRow} label="Summary interval"
                        desc="Messages between summary regenerations (sliding-summary only).">
                        <input class="settings-input settings-input-sm" type="number" min="0"
                               value=${ctxSummaryInterval.value}
                               onInput=${e => { ctxSummaryInterval.value = e.target.value; }} />
                    <//>
                    <${EditRow} label="Summary model"
                        desc="Optional cheaper model for generating summaries. Leave empty for default.">
                        <input class="settings-input settings-input-sm" type="text"
                               placeholder="same as default"
                               list="model-suggestions"
                               value=${ctxSummaryModel.value}
                               onInput=${e => { ctxSummaryModel.value = e.target.value; }} />
                        <span class="settings-effective">
                            <${ModelDisplay} value=${ctxSummaryModel.value.trim()} defaultValue=${defaults.model} />
                        </span>
                    <//>
                <//>

                <!-- Session (server-level, editable) -->
                <${Section} key="sess" title="Session" defaultOpen=${false}>
                    <span class="settings-hint settings-section-desc">
                        Controls session storage and retention. Changes apply to the next run.
                    </span>
                    <${EditRow} label="Max messages"
                        desc="Maximum messages stored per session.">
                        <input class="settings-input settings-input-sm" type="number" min="1"
                               value=${sessMaxMessages.value}
                               onInput=${e => { sessMaxMessages.value = e.target.value; }} />
                    <//>
                    <${EditRow} label="Max context tokens"
                        desc="Maximum tokens retained in session history (must be >= context max_input_tokens).">
                        <input class="settings-input settings-input-sm" type="number" min="1" step="1000"
                               value=${sessMaxCtxTokens.value}
                               onInput=${e => { sessMaxCtxTokens.value = e.target.value; }} />
                    <//>
                    <${EditRow} label="Idle timeout (seconds)"
                        desc="Time before a session is considered idle.">
                        <input class="settings-input settings-input-sm" type="number" min="0"
                               value=${sessIdleTimeout.value}
                               onInput=${e => { sessIdleTimeout.value = e.target.value; }} />
                    <//>
                    <${EditRow} label="Auto archive"
                        desc="Automatically archive idle sessions.">
                        <label class="settings-toggle">
                            <input type="checkbox"
                                   checked=${sessAutoArchive.value}
                                   onChange=${e => { sessAutoArchive.value = e.target.checked; }} />
                            <span>${sessAutoArchive.value ? 'enabled' : 'disabled'}</span>
                        </label>
                    <//>
                    <${EditRow} label="Archive TTL (seconds)"
                        desc="Delete archived sessions after this duration.">
                        <input class="settings-input settings-input-sm" type="number" min="0"
                               value=${sessArchiveTtl.value}
                               onInput=${e => { sessArchiveTtl.value = e.target.value; }} />
                    <//>
                <//>

                <!-- Tools (server-level, editable) -->
                <${Section} key="tools" title="Tools" defaultOpen=${false}>
                    <span class="settings-hint settings-section-desc">
                        Tool execution settings. Changes apply to the next run.
                    </span>
                    <${EditRow} label="Shell policy"
                        desc="sandboxed = restrict shell cwd to sandbox root. unrestricted = no cwd restriction.">
                        <select class="settings-select settings-input-sm"
                                value=${toolsShellPolicy.value}
                                onChange=${e => { toolsShellPolicy.value = e.target.value; }}>
                            <option value="sandboxed">sandboxed</option>
                            <option value="unrestricted">unrestricted</option>
                        </select>
                    <//>
                    <${EditRow} label="Sandbox root"
                        desc="Filesystem sandbox root for fs_* tools. Empty = unrestricted.">
                        <input class="settings-input settings-input-sm" type="text"
                               value=${toolsSandboxRoot.value}
                               onInput=${e => { toolsSandboxRoot.value = e.target.value; }} />
                    <//>
                    <${EditRow} label="Tool timeout (seconds)"
                        desc="Maximum execution time per tool call.">
                        <input class="settings-input settings-input-sm" type="number" min="1"
                               value=${toolsTimeout.value}
                               onInput=${e => { toolsTimeout.value = e.target.value; }} />
                    <//>
                    <${EditRow} label="Max output (bytes)"
                        desc="Maximum bytes returned from a single tool call.">
                        <input class="settings-input settings-input-sm" type="number" min="1"
                               value=${toolsMaxOutput.value}
                               onInput=${e => { toolsMaxOutput.value = e.target.value; }} />
                    <//>
                    <${InfoRow} label="Enabled tools" value=${`${enabledTools.length} tools`}
                        desc=${enabledTools.join(', ')} />
                <//>

                <!-- LLM Providers (server-level, editable) — #809 / #804 Slice A -->
                <${Section} key="llm" title="LLM Providers" defaultOpen=${false}>
                    <span class="settings-hint settings-section-desc">
                        Server-level reasoning &amp; caching defaults. Mutations propagate to the next HTTP-triggered run without restart; Telegram-triggered runs use a boot-time snapshot until the daemon is restarted.
                    </span>

                    <h4 class="settings-llm-subhead">Anthropic</h4>
                    <${EditRow} label="Thinking budget tokens"
                        desc="0 = extended thinking off. Leave blank to keep the current server value. The wire surface has no clear sentinel — once PATCHed, revert by editing settings.json + restart.">
                        <input class="settings-input settings-input-sm" type="number" min="0" step="1024"
                               placeholder=${llmAnth.thinking_budget_tokens != null ? String(llmAnth.thinking_budget_tokens) : 'unset'}
                               value=${llmAnthropicThinking.value}
                               onInput=${e => { llmAnthropicThinking.value = e.target.value; }} />
                    <//>
                    <${EditRow} label="Prompt cache enabled"
                        desc="Anthropic prefix caching (5-minute TTL). Server-level only.">
                        <label class="settings-toggle">
                            <input type="checkbox"
                                   checked=${llmAnthropicCache.value}
                                   onChange=${e => {
                                       llmAnthropicCache.value = e.target.checked;
                                       llmAnthropicCacheTouched.value = true;
                                   }} />
                            <span>${llmAnthropicCache.value ? 'enabled' : 'disabled'}</span>
                        </label>
                    <//>

                    <h4 class="settings-llm-subhead">OpenAI / OpenRouter</h4>
                    <${EditRow} label="Reasoning effort"
                        desc="Applies to o-series, GPT-5, and reasoning-capable Grok models. Auto-stripped on non-reasoning models. Choose Unset to clear an existing override.">
                        <select class="settings-select settings-input-sm"
                                value=${llmOpenaiEffort.value}
                                onChange=${e => { llmOpenaiEffort.value = e.target.value; }}>
                            <option value="">Unset (no override)</option>
                            <option value="minimal">minimal</option>
                            <option value="low">low</option>
                            <option value="medium">medium</option>
                            <option value="high">high</option>
                        </select>
                    <//>

                    <h4 class="settings-llm-subhead">Gemini</h4>
                    <${EditRow} label="Thinking budget"
                        desc="0 = extended thinking off. Leave blank to keep the current server value. Once PATCHed, this value can only be reverted by editing settings.json + restart.">
                        <input class="settings-input settings-input-sm" type="number" min="0" step="1024"
                               placeholder=${llmGem.thinking_budget != null ? String(llmGem.thinking_budget) : 'unset'}
                               value=${llmGeminiThinking.value}
                               onInput=${e => { llmGeminiThinking.value = e.target.value; }} />
                    <//>
                    <${EditRow} label="Cache enabled"
                        desc="Gemini context caching via cachedContents. Server-level only.">
                        <label class="settings-toggle">
                            <input type="checkbox"
                                   checked=${llmGeminiCache.value}
                                   onChange=${e => {
                                       llmGeminiCache.value = e.target.checked;
                                       llmGeminiCacheTouched.value = true;
                                   }} />
                            <span>${llmGeminiCache.value ? 'enabled' : 'disabled'}</span>
                        </label>
                    <//>
                    <${EditRow} label="Cache TTL (seconds)"
                        desc="Lifetime of a Gemini cache entry. Must be > 0.">
                        <input class="settings-input settings-input-sm" type="number" min="1" step="60"
                               placeholder=${llmGem.cache_ttl_seconds != null ? String(llmGem.cache_ttl_seconds) : '300'}
                               value=${llmGeminiCacheTtl.value}
                               onInput=${e => { llmGeminiCacheTtl.value = e.target.value; }} />
                    <//>
                <//>

                <!-- Logging (server-level, read-only) -->
                <${Section} key="log" title="Logging" defaultOpen=${false}>
                    <span class="settings-hint settings-section-desc">
                        File-based logging settings. Requires restart to change.
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

                <!-- Server info (compact) -->
                <div class="settings-row">
                    <label class="settings-label">Server info</label>
                    <div class="settings-info">
                        <div>Version: <span class="settings-info-value">${defaults.version || 'unknown'}</span></div>
                        <div>Base URL: <span class="settings-info-value">${defaults.base_url || 'unknown'}</span></div>
                        <div>Stream timeout: <span class="settings-info-value">${defaults.stream_chunk_timeout_secs || 60}s</span></div>
                    </div>
                </div>

                ${serverError.value && html`
                    <div class="settings-error">
                        Per-run overrides saved. Server settings failed: ${serverError.value}
                    </div>
                `}

                <div class="settings-footer">
                    <button class="settings-cancel" onClick=${onReset}>Reset</button>
                    <button class="settings-cancel" onClick=${onClose}>Cancel</button>
                    <button class="settings-save" onClick=${onApply}
                            disabled=${serverSaving.value}>
                        ${serverSaving.value ? 'Saving...' : (saved.value ? 'Saved!' : 'Apply')}
                    </button>
                </div>
            </div>
        </div>
    `;
}
