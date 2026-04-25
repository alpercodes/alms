import { html, useSignal, useEffect } from '../deps.js';

/**
 * Shared tri-state budget control — Inherit / Disable / Custom.
 *
 * Used by both the composer's "Advanced" expander (#818) and the
 * settings modal's per-run-overrides section (this PR — modal/composer
 * parity). Both surfaces read/write the same `localSettings` keys:
 * `thinking_budget_tokens` (Anthropic) and `gemini_thinking_budget`
 * (Gemini). Preserves the `None` / `Some(0)` / `Some(n>0)` distinction
 * the backend three-layer precedence relies on:
 *
 *   null/undefined -> 'inherit'  (omit on wire)
 *   0              -> 'disable'  (Some(0) — explicit disable)
 *   positive int   -> 'custom'   (Some(n) — operator-specified value)
 *
 * Style-agnostic — every visual class is provided by the caller via
 * `classNames` so the same component can render with composer-style
 * (`composer-advanced-*`) or settings-modal-style (`settings-*`)
 * markup without forking. Defaults match the composer for back-compat.
 *
 * Owns its own `mode` signal so flipping Inherit -> Custom (whose wire
 * is `null` until the user types) doesn't immediately collapse back to
 * Inherit. External resets (Reset-all on either surface) re-derive
 * mode/draft from the wire via a useEffect keyed on `wireValue` —
 * never mutates signals during render. (Tim's #818 nit applies here
 * too; lifted with the fix already in place.)
 */
export function modeFromWire(wire) {
    if (wire == null) return 'inherit';
    if (wire === 0) return 'disable';
    return 'custom';
}

const DEFAULT_CLASSES = {
    row: 'composer-advanced-row composer-advanced-row--tristate',
    label: 'composer-advanced-row-label',
    group: 'composer-advanced-tristate',
    select: 'composer-advanced-input composer-advanced-input--mode',
    input: 'composer-advanced-input composer-advanced-input--value',
};

export function BudgetTriState({ label, wireValue, onWrite, classNames }) {
    const cls = { ...DEFAULT_CLASSES, ...(classNames || {}) };
    const mode = useSignal(modeFromWire(wireValue));
    const draft = useSignal(wireValue && wireValue > 0 ? String(wireValue) : '');

    // External resets: when the wire value changes underneath us
    // (Reset-all from either surface, etc.), re-derive mode and draft
    // from wire. Special case: if the user just flipped Inherit -> Custom
    // and hasn't typed yet (wire is null but mode is 'custom'), keep
    // the Custom UI state so the input stays mounted for typing.
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
        <div class=${cls.row}>
            <span class=${cls.label}>${label}</span>
            <div class=${cls.group}>
                <select class=${cls.select}
                        value=${mode.value}
                        onChange=${onModeChange}>
                    <option value="inherit">Inherit</option>
                    <option value="disable">Disable</option>
                    <option value="custom">Custom</option>
                </select>
                ${mode.value === 'custom' && html`
                    <input class=${cls.input}
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
