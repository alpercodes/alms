import { html } from '../deps.js';

/**
 * Shared model-display helpers.
 *
 * Single source of truth for:
 *   - friendly model names (Claude Opus 4.7, GPT-4o, ...)
 *   - provider inference from the raw id (anthropic / openai / openrouter / google)
 *   - the datalist of suggested models used across every model picker
 *   - small presentational components (ModelBadge, ModelDisplay, ProviderDisplay)
 *
 * Keep this module dependency-light: it only imports html from deps.js.
 *
 * Friendly-name lookup: a small inline table keyed by raw id.  Unknown ids
 * fall back to the raw id (never block on unknowns).  This is deliberately
 * a flat map rather than config-driven because the set is small, changes
 * rarely, and keeping it close to the component code keeps it discoverable.
 */

// ── Model catalog ───────────────────────────────────────────────────────────

/**
 * Friendly-name + provider lookup for known model ids.
 * Keys are raw provider ids (as accepted by the backend).  Unknown ids
 * fall back to the raw id and a guessed provider.
 */
const MODELS = {
    // Anthropic
    'claude-opus-4-7':          { name: 'Claude Opus 4.7',   provider: 'anthropic' },
    'claude-opus-4-6':          { name: 'Claude Opus 4.6',   provider: 'anthropic' },
    'claude-opus-4-20250514':   { name: 'Claude Opus 4',     provider: 'anthropic' },
    'claude-sonnet-4-6':        { name: 'Claude Sonnet 4.6', provider: 'anthropic' },
    'claude-sonnet-4-5':        { name: 'Claude Sonnet 4.5', provider: 'anthropic' },
    'claude-sonnet-4-20250514': { name: 'Claude Sonnet 4',   provider: 'anthropic' },
    'claude-haiku-4-5':         { name: 'Claude Haiku 4.5',  provider: 'anthropic' },
    'claude-3-7-sonnet-20250219': { name: 'Claude 3.7 Sonnet', provider: 'anthropic' },
    'claude-3-5-sonnet-20241022': { name: 'Claude 3.5 Sonnet', provider: 'anthropic' },
    'claude-3-5-haiku-20241022':  { name: 'Claude 3.5 Haiku',  provider: 'anthropic' },

    // OpenAI
    'gpt-4o':        { name: 'GPT-4o',        provider: 'openai' },
    'gpt-4o-mini':   { name: 'GPT-4o mini',   provider: 'openai' },
    'gpt-4.1':       { name: 'GPT-4.1',       provider: 'openai' },
    'gpt-4.1-mini':  { name: 'GPT-4.1 mini',  provider: 'openai' },
    'gpt-4.1-nano':  { name: 'GPT-4.1 nano',  provider: 'openai' },
    'gpt-4':         { name: 'GPT-4',         provider: 'openai' },
    'gpt-4-turbo':   { name: 'GPT-4 Turbo',   provider: 'openai' },
    'gpt-3.5-turbo': { name: 'GPT-3.5 Turbo', provider: 'openai' },
    'o3':            { name: 'o3',            provider: 'openai' },
    'o3-mini':       { name: 'o3-mini',       provider: 'openai' },
    'o4-mini':       { name: 'o4-mini',       provider: 'openai' },

    // OpenRouter (by full slug; provider kept as 'openrouter' for badge consistency)
    // Note: `-preview` ids age fast — keep them out of the catalog and let them
    // fall through to the raw-id display.  Add stable slugs here as they land.
    'deepseek/deepseek-r1':             { name: 'DeepSeek R1',                provider: 'openrouter' },
    'deepseek/deepseek-chat-v3-0324':   { name: 'DeepSeek Chat v3',           provider: 'openrouter' },
};

/**
 * Datalist suggestions for every model picker in the UI.
 * Ordered so the most-commonly-used ids appear first.
 */
export const MODEL_SUGGESTIONS = [
    // Anthropic (current)
    'claude-opus-4-7',
    'claude-sonnet-4-6',
    'claude-haiku-4-5',
    // Anthropic (dated)
    'claude-opus-4-20250514',
    'claude-sonnet-4-20250514',
    'claude-3-7-sonnet-20250219',
    'claude-3-5-haiku-20241022',
    // OpenAI
    'gpt-4o',
    'gpt-4o-mini',
    'gpt-4.1',
    'gpt-4.1-mini',
    'gpt-4.1-nano',
    'o3',
    'o3-mini',
    'o4-mini',
    // OpenRouter (popular picks)
    'deepseek/deepseek-r1',
    'deepseek/deepseek-chat-v3-0324',
];

// ── Formatters ──────────────────────────────────────────────────────────────

/**
 * Return the friendly display name for a model id.
 * Falls back to the raw id when unknown.
 *
 * @param {string | null | undefined} id
 * @returns {string}
 */
export function formatModelName(id) {
    if (!id) return '';
    const entry = MODELS[id];
    if (entry) return entry.name;
    return id;
}

/**
 * Infer the provider for a model id.
 * Returns one of 'anthropic' | 'openai' | 'openrouter' | 'unknown'.
 *
 * Inference rules:
 *   1. Exact match in MODELS table wins.
 *   2. OpenRouter slugs always contain a '/' (e.g. 'google/gemini-...').
 *   3. Ids starting with 'claude' are Anthropic.
 *   4. Ids matching the OpenAI o-series shape (`^o\d`, with or without a dash)
 *      or starting with 'gpt' are OpenAI.  The broad `^o\d` pattern future-
 *      proofs us against new o-series releases (o5, o6, ...) and also covers
 *      no-dash variants like `o1mini`.
 *   5. Otherwise 'unknown' — rendered as a neutral badge.
 *
 * @param {string | null | undefined} id
 * @returns {'anthropic' | 'openai' | 'openrouter' | 'unknown'}
 */
export function getProvider(id) {
    if (!id) return 'unknown';
    const entry = MODELS[id];
    if (entry) return entry.provider;
    if (id.includes('/')) return 'openrouter';
    if (id.startsWith('claude')) return 'anthropic';
    if (id.startsWith('gpt') || /^o\d/.test(id)) return 'openai';
    return 'unknown';
}

/**
 * Short label for a provider, used in the badge.
 */
const PROVIDER_LABELS = {
    anthropic:  'Anthropic',
    openai:     'OpenAI',
    openrouter: 'OpenRouter',
    unknown:    'Custom',
};

/**
 * Short label for a provider id.  Accepts either a model id (in which case
 * the provider is inferred) or a provider id directly.
 */
export function formatProviderLabel(providerOrModelId) {
    if (!providerOrModelId) return PROVIDER_LABELS.unknown;
    if (PROVIDER_LABELS[providerOrModelId]) return PROVIDER_LABELS[providerOrModelId];
    return PROVIDER_LABELS[getProvider(providerOrModelId)];
}

// ── Components ──────────────────────────────────────────────────────────────

/**
 * Small pill showing the provider for a model id.
 *
 * Usage:
 *   html`<${ProviderBadge} modelId=${'claude-opus-4-7'} />`
 *   html`<${ProviderBadge} provider=${'openai'} />`
 */
export function ProviderBadge({ modelId, provider }) {
    const p = provider || getProvider(modelId);
    const label = PROVIDER_LABELS[p] || PROVIDER_LABELS.unknown;
    return html`
        <span class="model-provider-badge model-provider-badge--${p}"
              title=${`Provider: ${label}`}>${label}</span>
    `;
}

/**
 * Inline model label: "Claude Opus 4.7 [Anthropic]".
 * Truncates with ellipsis; full raw id shown on hover.
 *
 * Props:
 *   modelId    — raw provider id (required if the model is known/unknown)
 *   showBadge  — whether to render the provider pill (default: true)
 *   muted      — render in muted style (default: false)
 */
export function ModelBadge({ modelId, showBadge = true, muted = false }) {
    if (!modelId) return null;
    const name = formatModelName(modelId);
    const showRaw = name !== modelId;
    return html`
        <span class="model-display ${muted ? 'model-display--muted' : ''}"
              title=${showRaw ? `${name} (${modelId})` : modelId}>
            <span class="model-name">${name}</span>
            ${showBadge && html`<${ProviderBadge} modelId=${modelId} />`}
        </span>
    `;
}

/**
 * Full model-state display used wherever we surface an *effective* model
 * value that may be either an explicit override or the server default.
 *
 * Props:
 *   value         — explicit model id (override).  Null/empty means "using default".
 *   defaultValue  — the server default model id (fallback).
 *   showBadge     — render the provider pill (default: true)
 *
 * Render contract:
 *   - If `value` is set and differs from `defaultValue`: explicit id, marked as override.
 *   - If `value` is unset OR equals `defaultValue`: "Default (Friendly Name)".
 *   - If both are null: "unknown".
 *
 * Note: explicitly setting `value === defaultValue` is treated as Default — the
 * semantic result is the same as leaving it unset, so we render the Default
 * chip rather than showing "override" purely because of user intent.
 */
export function ModelDisplay({ value, defaultValue, showBadge = true }) {
    const explicit = value && value.trim ? value.trim() : value;
    const hasOverride = !!explicit && explicit !== defaultValue;
    const effective = explicit || defaultValue;

    if (!effective) {
        return html`<span class="model-display model-display--muted">unknown</span>`;
    }

    const name = formatModelName(effective);
    const showRaw = name !== effective;
    const tooltip = showRaw ? `${name} (${effective})` : effective;

    if (hasOverride) {
        return html`
            <span class="model-display" title=${tooltip}>
                <span class="model-override-pill" title="Per-run override">override</span>
                <span class="model-name">${name}</span>
                ${showBadge && html`<${ProviderBadge} modelId=${effective} />`}
            </span>
        `;
    }

    return html`
        <span class="model-display model-display--default" title=${tooltip}>
            <span class="model-default-label">Default</span>
            <span class="model-name">${name}</span>
            ${showBadge && html`<${ProviderBadge} modelId=${effective} />`}
        </span>
    `;
}

/**
 * Full provider-state display — parallel to `<ModelDisplay>` but for the
 * provider field.  Used wherever we surface an *effective* provider value
 * that may be either an explicit override or the server default.
 *
 * Props:
 *   value         — explicit provider id (override).  Null/empty means "using default".
 *   defaultValue  — the server default provider id (fallback).
 *
 * Render contract:
 *   - If `value` is set and differs from `defaultValue`: the provider badge,
 *     marked as override.
 *   - If `value` is unset OR equals `defaultValue`: "Default" chip + provider badge.
 *   - If both are null: "unknown".
 *
 * Note: explicitly setting `value === defaultValue` is treated as Default — the
 * semantic result is the same as leaving it unset, so we render the Default
 * chip rather than showing "override" purely because of user intent.
 */
export function ProviderDisplay({ value, defaultValue }) {
    const explicit = value && value.trim ? value.trim() : value;
    const hasOverride = !!explicit && explicit !== defaultValue;
    const effective = explicit || defaultValue;

    if (!effective) {
        return html`<span class="model-display model-display--muted">unknown</span>`;
    }

    if (hasOverride) {
        return html`
            <span class="model-display">
                <span class="model-override-pill" title="Per-run override">override</span>
                <${ProviderBadge} provider=${effective} />
            </span>
        `;
    }

    return html`
        <span class="model-display model-display--default">
            <span class="model-default-label">Default</span>
            <${ProviderBadge} provider=${effective} />
        </span>
    `;
}
