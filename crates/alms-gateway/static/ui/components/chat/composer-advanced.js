import { html, useSignal } from '../../deps.js';
import { localSettings, saveSettings, serverDefaults } from '../../state/settings.js';
import { activeOverrideCount } from '../settings-modal.js';
import { IconChevronDown } from '../../utils/icons.js';
import { BudgetTriState } from '../budget-tri-state.js';

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

// `BudgetTriState` lives in `../budget-tri-state.js` and is shared with
// the settings modal (#822). Its `DEFAULT_CLASSES` already use the
// composer-advanced-* class names, so this surface keeps byte-identical
// markup without passing a `classNames` prop.

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

    // Provider / model per-run overrides are disabled (#865). The
    // controls remain visible-but-inert until a deliberate
    // session-scoped UX exists; the change handlers are wired to no-ops
    // so any residual user input cannot stamp localSettings.
    const onProviderChange = () => {};
    const onModelChange = () => {};
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
                                    value=""
                                    disabled
                                    title="Per-run provider overrides are disabled. Configure provider on the agent."
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
                                   disabled
                                   title="Per-run model overrides are disabled. Configure model on the agent."
                                   placeholder=${defaults.model || 'inherit'}
                                   value=""
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
