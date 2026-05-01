/**
 * Shared per-run override marker pip + tooltip generator (#857 follow-up).
 *
 * Lifted out of `settings-modal.js`, `composer-advanced.js`, and
 * `header.js` after Tim's PR #926 review noted byte-identical declarations
 * across all three callers. Keeping a single source of truth means a
 * future tweak to the wording, ARIA labels, or markup propagates to every
 * surface in lock-step.
 *
 * The two callers (modal + composer) wanted slightly different visual
 * styling, so the marker accepts a `className` prop instead of hard-coding
 * a single class — the existing `.settings-override-marker` and
 * `.composer-advanced-override-marker` CSS rules continue to do the
 * surface-specific theming.
 */

import { html } from '../deps.js';

/**
 * Small accent pip rendered next to a per-run override field's label.
 * Returns null when `active` is false so callers can render unconditionally
 * without leaving an empty span in the DOM.
 *
 * @param {object} props
 * @param {boolean} props.active - whether this field is currently overridden
 * @param {string} [props.label] - human-friendly field label for the tooltip
 * @param {string} [props.className] - CSS class for surface-specific theming
 *   (defaults to `settings-override-marker` for backwards compatibility with
 *   the modal call sites — composer call sites pass
 *   `composer-advanced-override-marker`).
 */
export function OverrideMarker({ active, label, className = 'settings-override-marker' }) {
    if (!active) return null;
    const tooltip = label
        ? `${label} is overridden — counted in the per-run overrides badge.`
        : 'This field contributes to the per-run overrides badge.';
    return html`<span class=${className}
                      role="img"
                      aria-label="overridden"
                      title=${tooltip}></span>`;
}

/**
 * Build the tooltip shown on hover of a per-run override badge — the
 * settings-gear badge in the header and the composer's "Advanced"
 * expander badge both share this surface.
 *
 * Lists the human-friendly labels of every contributing field so the
 * count is self-documenting straight from the badge tooltip.
 *
 * @param {number} count - total number of active overrides
 * @param {Set<string>} keys - the set of localSettings keys that are
 *   currently overridden (exposed by `activeOverrideKeys` in
 *   `settings-modal.js`)
 * @param {Object<string,string>} labels - mapping from override key to
 *   display label (typically `OVERRIDE_LABELS` from `settings-modal.js`)
 * @returns {string} the tooltip string, including bullet-listed labels
 *   when keys is non-empty.
 */
export function overrideTooltip(count, keys, labels) {
    const noun = `${count} per-run override${count === 1 ? '' : 's'} active`;
    const names = [];
    keys.forEach(k => names.push(labels[k] || k));
    if (names.length === 0) return noun;
    return `${noun}:\n• ${names.join('\n• ')}`;
}
