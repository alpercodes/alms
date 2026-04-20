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
    'claude-opus-4-7':            { name: 'Claude Opus 4.7',     provider: 'anthropic' },
    'claude-opus-4-6':            { name: 'Claude Opus 4.6',     provider: 'anthropic' },
    'claude-sonnet-4-6':          { name: 'Claude Sonnet 4.6',   provider: 'anthropic' },
    'claude-sonnet-4-5':          { name: 'Claude Sonnet 4.5',   provider: 'anthropic' },
    'claude-haiku-4-5':           { name: 'Claude Haiku 4.5',    provider: 'anthropic' },

    // OpenAI
    'gpt-5.4':       { name: 'GPT-5.4',       provider: 'openai' },
    'gpt-5.4-mini':  { name: 'GPT-5.4 mini',  provider: 'openai' },
    'gpt-5.4-nano':  { name: 'GPT-5.4 nano',  provider: 'openai' },
    'gpt-4.1':       { name: 'GPT-4.1',       provider: 'openai' },
    'gpt-4.1-mini':  { name: 'GPT-4.1 mini',  provider: 'openai' },
    'gpt-4.1-nano':  { name: 'GPT-4.1 nano',  provider: 'openai' },
    'gpt-4o':        { name: 'GPT-4o',        provider: 'openai' },
    'gpt-4o-mini':   { name: 'GPT-4o mini',   provider: 'openai' },
    'o4-mini':       { name: 'o4-mini',       provider: 'openai' },
    'o3':            { name: 'o3',            provider: 'openai' },
    'o3-mini':       { name: 'o3-mini',       provider: 'openai' },

    // xAI (Grok, direct API)
    'grok-4.20':     { name: 'Grok 4.20',     provider: 'xai' },
    'grok-4-fast':   { name: 'Grok 4 Fast',   provider: 'xai' },
    'grok-3':        { name: 'Grok 3',        provider: 'xai' },
    'grok-3-mini':   { name: 'Grok 3 mini',   provider: 'xai' },

    // DeepSeek (direct API — no `/` slug; routed to DeepSeek's own endpoint)
    'deepseek-chat':     { name: 'DeepSeek Chat (V3)', provider: 'deepseek' },
    'deepseek-reasoner': { name: 'DeepSeek Reasoner (R1)', provider: 'deepseek' },

    // Mistral (direct API)
    'mistral-large-latest':  { name: 'Mistral Large',   provider: 'mistral' },
    'mistral-medium-latest': { name: 'Mistral Medium',  provider: 'mistral' },
    'mistral-small-latest':  { name: 'Mistral Small',   provider: 'mistral' },
    'codestral-latest':      { name: 'Codestral',       provider: 'mistral' },
    'ministral-8b-latest':   { name: 'Ministral 8B',    provider: 'mistral' },
    'open-mistral-nemo':     { name: 'Mistral Nemo',    provider: 'mistral' },

    // Groq (hosted open models — bare id, no `/` slug)
    'llama-3.3-70b-versatile':        { name: 'Llama 3.3 70B',              provider: 'groq' },
    'llama-3.1-8b-instant':           { name: 'Llama 3.1 8B (Instant)',     provider: 'groq' },
    'deepseek-r1-distill-llama-70b':  { name: 'DeepSeek R1 Distill (70B)',  provider: 'groq' },
    'qwen-2.5-32b':                   { name: 'Qwen 2.5 32B',               provider: 'groq' },

    // Ollama (local — id is `name:tag`)
    'qwen2.5-coder:32b': { name: 'Qwen 2.5 Coder 32B',   provider: 'ollama' },
    'deepseek-r1:7b':    { name: 'DeepSeek R1 7B',       provider: 'ollama' },
    'llama3.3:70b':      { name: 'Llama 3.3 70B',        provider: 'ollama' },

    // OpenRouter (by full slug; provider kept as 'openrouter' for badge consistency)
    // Note: `-preview` ids age fast — keep them out of the catalog and let them
    // fall through to the raw-id display.  Add stable slugs here as they land.
    'deepseek/deepseek-r1':             { name: 'DeepSeek R1',                provider: 'openrouter' },
    'deepseek/deepseek-chat-v3-0324':   { name: 'DeepSeek Chat v3',           provider: 'openrouter' },
    'z-ai/glm-5.1':                     { name: 'GLM 5.1',                    provider: 'openrouter' },
    'minimax/minimax-m2.7':             { name: 'MiniMax M2.7',               provider: 'openrouter' },
    'xiaomi/mimo-v2-pro':               { name: 'MiMo v2-pro',                provider: 'openrouter' },
    'moonshotai/kimi-k2.6':             { name: 'Kimi K2.6',                  provider: 'openrouter' },
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
    'claude-opus-4-6',
    // OpenAI
    'gpt-5.4',
    'gpt-5.4-mini',
    'gpt-5.4-nano',
    'gpt-4.1',
    'gpt-4.1-mini',
    'gpt-4o',
    'gpt-4o-mini',
    'o4-mini',
    'o3',
    // xAI
    'grok-4.20',
    'grok-4-fast',
    'grok-3-mini',
    // DeepSeek (direct)
    'deepseek-chat',
    'deepseek-reasoner',
    // Mistral
    'mistral-large-latest',
    'mistral-small-latest',
    'codestral-latest',
    // Groq (hosted open models)
    'llama-3.3-70b-versatile',
    'llama-3.1-8b-instant',
    'deepseek-r1-distill-llama-70b',
    // Ollama (local)
    'qwen2.5-coder:32b',
    'deepseek-r1:7b',
    'llama3.3:70b',
    // OpenRouter (popular picks)
    'deepseek/deepseek-r1',
    'deepseek/deepseek-chat-v3-0324',
    'z-ai/glm-5.1',
    'minimax/minimax-m2.7',
    'xiaomi/mimo-v2-pro',
    'moonshotai/kimi-k2.6',
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
 * Returns one of 'anthropic' | 'openai' | 'openrouter' | 'xai' | 'deepseek' |
 * 'mistral' | 'groq' | 'ollama' | 'google' | 'unknown'.
 *
 * Inference rules (checked in order):
 *   1. Exact match in MODELS table wins.
 *   2. OpenRouter slugs always contain a '/' (e.g. 'google/gemini-...').
 *   3. Ollama-style `name:tag` ids contain a ':' (and did not match rule 2).
 *   4. Ids starting with 'claude' are Anthropic.
 *   5. Ids starting with 'gpt' or matching the OpenAI o-series shape (`^o\d`,
 *      with or without a dash) are OpenAI.  The broad `^o\d` pattern future-
 *      proofs us against new o-series releases (o5, o6, ...) and also covers
 *      no-dash variants like `o1mini`.
 *   6. Ids starting with 'grok' are xAI.
 *   7. Ids starting with 'deepseek-' (direct, no slug) are DeepSeek.
 *   8. Ids starting with 'mistral-' / 'codestral-' / 'ministral-' /
 *      'open-mistral-' / 'open-mixtral-' are Mistral.
 *   9. Bare 'llama-*' ids are treated as Groq by default — Groq is the
 *      most common public API surface for Llama; Ollama users hit rule 3
 *      via the `name:tag` shape, and OpenRouter users hit rule 2 via the
 *      slug shape.  Not perfect for local vLLM-style deployments, but a
 *      reasonable default; badge falls back to 'unknown' only if we're
 *      genuinely unsure.
 *  10. Ids starting with 'gemini-' are Google (pre-added for #764; no
 *      entries in MODELS yet).
 *  11. Otherwise 'unknown' — rendered as a neutral badge.
 *
 * @param {string | null | undefined} id
 * @returns {'anthropic' | 'openai' | 'openrouter' | 'xai' | 'deepseek' | 'mistral' | 'groq' | 'ollama' | 'google' | 'unknown'}
 */
export function getProvider(id) {
    if (!id) return 'unknown';
    const entry = MODELS[id];
    if (entry) return entry.provider;
    if (id.includes('/')) return 'openrouter';
    if (id.includes(':')) return 'ollama';
    if (id.startsWith('claude')) return 'anthropic';
    if (id.startsWith('gpt') || /^o\d/.test(id)) return 'openai';
    if (id.startsWith('grok')) return 'xai';
    if (id.startsWith('deepseek-')) return 'deepseek';
    if (
        id.startsWith('mistral-') ||
        id.startsWith('codestral-') ||
        id.startsWith('ministral-') ||
        id.startsWith('open-mistral-') ||
        id.startsWith('open-mixtral-')
    ) {
        return 'mistral';
    }
    if (id.startsWith('llama-')) return 'groq';
    if (id.startsWith('gemini-')) return 'google';
    return 'unknown';
}

/**
 * Short label for a provider, used in the badge.
 */
const PROVIDER_LABELS = {
    anthropic:  'Anthropic',
    openai:     'OpenAI',
    openrouter: 'OpenRouter',
    xai:        'xAI',
    deepseek:   'DeepSeek',
    mistral:    'Mistral',
    groq:       'Groq',
    ollama:     'Ollama',
    google:     'Google',
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
