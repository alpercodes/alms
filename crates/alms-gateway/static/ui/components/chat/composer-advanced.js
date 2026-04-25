import { html, useSignal, useEffect } from '../../deps.js';
import { localSettings, saveSettings, serverDefaults } from '../../state/settings.js';
import { activeOverrideCount } from '../settings-modal.js';
import { IconChevronDown } from '../../utils/icons.js';

/**
 * Common model suggestions for the datalist (mirrors settings-modal.js).
 * Kept inline so this component is self-contained.
 */
const MODEL_SUGGESTIONS = [
    'gpt-4o', 'gpt-4o-mini', 'gpt-4.1', 'gpt-4.1-mini', 'gpt-4.1-nano',
    'o3', 'o3-mini', 'o4-mini',
    'claude-sonnet-4-20250514', 'claude-opus-4-20250514',
    'claude-3-7-sonnet-20250219', 'claude-3-5-haiku-20241022',
    'google/gemini-2.5-pro-preview', 'google/gemini-2.5-flash-preview',
    'deepseek/deepseek-r1', 'deepseek/deepseek-chat-v3-0324',
];

const REASONING_EFFORTS = ['minimal', 'low', 'medium', 'high'];

/**
 * Pick the effective tri-state mode from a wire value when no explicit
 * UI override is in effect.
 *
 *   null/undefined -> 'inherit'  (omit on wire)
 *   0              -> 'disable'  (Some(0) — explicit disable)
 *   positive int   -> 'custom'   (Some(n) — operator-specified value)
 */
function modeFromWire(wire) {
    if (wire == null) return 'inherit';
    if (wire === 0) return 'disable';
    return 'custom';
}

/**
 * Tri-state budget control — Inherit / Disable / Custom.
 *
 * Owns its own `mode` signal so that flipping into Custom from Inherit
 * (which has an empty value) doesn't immediately collapse back to
 * Inherit. Only writes to `localSettings` once the value is meaningful:
 *   - mode = inherit -> wire = null
 *   - mode = disable -> wire = 0
 *   - mode = custom + non-empty value -> wire = parsed int
 *   - mode = custom + empty value -> wire = null (cleared, but the
 *     Custom-with-empty-input UI state is preserved locally)
 *
 * Stays in sync with external resets (e.g. settings modal Reset, the
 * Reset-all button) by watching the live wire value: when the
 * external wire flips, we adopt its mode unless the user is mid-Custom
 * with an empty input (we keep that state to avoid yanking focus).
 */
function BudgetTriState({ label, wireValue, onWrite }) {
    const mode = useSignal(modeFromWire(wireValue));
    const draft = useSignal(wireValue && wireValue > 0 ? String(wireValue) : '');

    // External resets: when the wire value changes underneath us
    // (Reset-all, settings-modal Reset, etc.), re-derive mode and draft
    // from wire — but only when the wire-derived mode genuinely differs
    // from the local mode signal, so we don't fight the user's input.
    //
    // Special case: when the user has just flipped Inherit -> Custom
    // (and hasn't typed yet), our own onModeChange wrote `null` to the
    // wire. The wire-derived mode is 'inherit', but we want to stay in
    // 'custom' so the number input renders for the user to type into.
    // Detect this by `mode === 'custom' && draft === '' && wire == null`.
    // (Edge corner: a forced external Reset while mid-Custom-empty won't
    //  visually unfreeze the dropdown until the user clicks back to
    //  Inherit. Acceptable; the wire is correctly null in that case.)
    //
    // Tim's #818 nit: this used to run during render. Moved into a
    // useEffect so we never mutate signals from the render pass — the
    // effect re-runs whenever the wire value flips externally.
    useEffect(() => {
        const wireMode = modeFromWire(wireValue);
        if (wireMode !== mode.value && !(mode.value === 'custom' && draft.value === '' && wireValue == null)) {
            mode.value = wireMode;
            draft.value = wireValue && wireValue > 0 ? String(wireValue) : '';
        }
    }, [wireValue]);

    const onModeChange = (e) => {
        const next = e.target.value;
        mode.value = next;
        if (next === 'inherit') {
            onWrite(null);
        } else if (next === 'disable') {
            onWrite(0);
        } else {
            // mode === 'custom' — only write if there's a value already;
            // otherwise wait for the user to fill it in.
            const n = parseInt(draft.value, 10);
            if (!isNaN(n) && n >= 0) onWrite(n);
            else onWrite(null);
        }
    };

    const onDraftChange = (e) => {
        draft.value = e.target.value;
        const n = parseInt(e.target.value, 10);
        if (!isNaN(n) && n >= 0) onWrite(n);
        else onWrite(null);
    };

    return html`
        <div class="composer-advanced-row composer-advanced-row--tristate">
            <span class="composer-advanced-row-label">${label}</span>
            <div class="composer-advanced-tristate">
                <select class="composer-advanced-input composer-advanced-input--mode"
                        value=${mode.value}
                        onChange=${onModeChange}>
                    <option value="inherit">Inherit</option>
                    <option value="disable">Disable</option>
                    <option value="custom">Custom</option>
                </select>
                ${mode.value === 'custom' && html`
                    <input class="composer-advanced-input composer-advanced-input--value"
                           type="number"
                           min="0"
                           step="1024"
                           placeholder="tokens"
                           value=${draft.value}
                           onInput=${onDraftChange} />
                `}
            </div>
        </div>
    `;
}

/**
 * Composer "Advanced" expander — per-run override controls living below
 * the chat textarea.
 *
 * Reads/writes `localSettings` directly (debounced through saveSettings).
 * Every field is optional and falls back to the server default; the
 * three reasoning knobs (`thinking_budget_tokens`, `reasoning_effort`,
 * `gemini_thinking_budget`) preserve the `None` (inherit) vs `Some(0)`
 * (explicit disable) distinction in their tri-state controls.
 *
 * The expander stays collapsed by default so the regular chat surface
 * isn't visually noisier for users who never touch overrides.
 */
export function ComposerAdvanced() {
    const open = useSignal(false);

    const s = localSettings.value;
    const defaults = serverDefaults.value || {};
    const overrideCount = activeOverrideCount.value;

    const onProviderChange = (e) => saveSettings({ provider: e.target.value || null });
    const onModelChange = (e) => saveSettings({ model: e.target.value.trim() || null });
    const onMaxTokensChange = (e) => {
        const n = parseInt(e.target.value, 10);
        saveSettings({ max_tokens: !isNaN(n) && n > 0 ? n : null });
    };
    const onPostureChange = (e) => saveSettings({ posture: e.target.value || null });
    // Tri-state debug_mode (Tim's #818 nit): the previous boolean
    // checkbox could only emit `true` or `null`, which meant a user
    // could never explicitly send `debug_mode: false` to override an
    // agent-level `true` for a single run. The wire (CreateRunRequest)
    // already accepts `Option<bool>` and `apply_overrides` honors
    // `Some(false)`, so the missing piece was UI surfaceability.
    //
    //   '' (empty)   -> wire null (inherit)
    //   'true'       -> wire true
    //   'false'      -> wire false (explicit per-run disable)
    const debugWireStr = s.debug_mode == null ? '' : (s.debug_mode ? 'true' : 'false');
    const onDebugChange = (e) => {
        const v = e.target.value;
        if (v === '') saveSettings({ debug_mode: null });
        else if (v === 'true') saveSettings({ debug_mode: true });
        else saveSettings({ debug_mode: false });
    };

    // OpenAI reasoning_effort — single dropdown ('' = inherit)
    const onReasoningEffort = (e) => {
        const v = e.target.value;
        saveSettings({ reasoning_effort: v || null });
    };

    const writeThinking = (v) => saveSettings({ thinking_budget_tokens: v });
    const writeGemini = (v) => saveSettings({ gemini_thinking_budget: v });

    const onResetAll = () => {
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
    };

    return html`
        <div class="composer-advanced ${open.value ? 'is-open' : ''}">
            <button type="button"
                    class="composer-advanced-toggle"
                    aria-expanded=${open.value}
                    onClick=${() => { open.value = !open.value; }}>
                <span class="composer-advanced-chevron ${open.value ? 'is-open' : ''}">
                    <${IconChevronDown} />
                </span>
                <span class="composer-advanced-label">Advanced</span>
                ${overrideCount > 0 && html`
                    <span class="composer-advanced-badge"
                          title=${`${overrideCount} per-run override${overrideCount === 1 ? '' : 's'} active`}>
                        ${overrideCount}
                    </span>
                `}
            </button>

            ${open.value && html`
                <div class="composer-advanced-panel">
                    <div class="composer-advanced-hint">
                        Per-run overrides applied to every new message until reset. Empty/Inherit falls back to the server default.
                    </div>

                    <div class="composer-advanced-grid">
                        <label class="composer-advanced-row">
                            <span class="composer-advanced-row-label">Provider</span>
                            <select class="composer-advanced-input"
                                    value=${s.provider || ''}
                                    onChange=${onProviderChange}>
                                <option value="">Inherit (${defaults.provider || 'server default'})</option>
                                <option value="openai">openai</option>
                                <option value="anthropic">anthropic</option>
                                <option value="openrouter">openrouter</option>
                            </select>
                        </label>

                        <label class="composer-advanced-row">
                            <span class="composer-advanced-row-label">Model</span>
                            <input class="composer-advanced-input"
                                   type="text"
                                   list="composer-advanced-models"
                                   placeholder=${defaults.model || 'inherit'}
                                   value=${s.model || ''}
                                   onChange=${onModelChange} />
                            <datalist id="composer-advanced-models">
                                ${MODEL_SUGGESTIONS.map(m => html`<option value=${m} />`)}
                            </datalist>
                        </label>

                        <label class="composer-advanced-row">
                            <span class="composer-advanced-row-label">Max tokens</span>
                            <input class="composer-advanced-input"
                                   type="number"
                                   min="1"
                                   step="1000"
                                   placeholder=${defaults.max_tokens || 'inherit'}
                                   value=${s.max_tokens != null ? s.max_tokens : ''}
                                   onChange=${onMaxTokensChange} />
                        </label>

                        <label class="composer-advanced-row">
                            <span class="composer-advanced-row-label">Posture</span>
                            <select class="composer-advanced-input"
                                    value=${s.posture || ''}
                                    onChange=${onPostureChange}>
                                <option value="">Inherit (${defaults.posture || 'guarded'})</option>
                                <option value="full_control">full_control</option>
                                <option value="guarded">guarded</option>
                                <option value="autonomous">autonomous</option>
                            </select>
                        </label>
                    </div>

                    <div class="composer-advanced-section-divider"></div>
                    <div class="composer-advanced-section-title">
                        Reasoning &amp; thinking
                        <span class="composer-advanced-section-hint">
                            Provider-specific. Silently ignored on other providers.
                        </span>
                    </div>

                    <div class="composer-advanced-grid">
                        <${BudgetTriState}
                            label="Anthropic thinking budget"
                            wireValue=${s.thinking_budget_tokens}
                            onWrite=${writeThinking} />

                        <label class="composer-advanced-row">
                            <span class="composer-advanced-row-label">OpenAI reasoning effort</span>
                            <select class="composer-advanced-input"
                                    value=${s.reasoning_effort || ''}
                                    onChange=${onReasoningEffort}>
                                <option value="">Inherit</option>
                                ${REASONING_EFFORTS.map(e => html`
                                    <option value=${e} key=${e}>${e}</option>
                                `)}
                            </select>
                        </label>

                        <${BudgetTriState}
                            label="Gemini thinking budget"
                            wireValue=${s.gemini_thinking_budget}
                            onWrite=${writeGemini} />

                        <label class="composer-advanced-row">
                            <span class="composer-advanced-row-label">Debug mode</span>
                            <select class="composer-advanced-input"
                                    value=${debugWireStr}
                                    onChange=${onDebugChange}>
                                <option value="">Inherit</option>
                                <option value="true">On</option>
                                <option value="false">Off</option>
                            </select>
                        </label>
                    </div>

                    <div class="composer-advanced-footer">
                        <button type="button"
                                class="composer-advanced-reset"
                                onClick=${onResetAll}
                                disabled=${overrideCount === 0}>
                            Reset all
                        </button>
                    </div>
                </div>
            `}
        </div>
    `;
}
