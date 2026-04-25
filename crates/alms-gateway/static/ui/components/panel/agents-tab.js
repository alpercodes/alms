import { html, useEffect, useSignal } from '../../deps.js';
import { agents, activeAgentId } from '../../state/agents.js';
import { serverDefaults } from '../../state/settings.js';
import { activePanelTab } from '../../state/panel.js';
import { listAgents, createAgent, updateAgent, deleteAgent, setDefaultAgent } from '../../api/agents.js';
import { switchAgent } from '../../hooks/use-boot.js';
import {
    MODEL_SUGGESTIONS,
    ModelDisplay,
    ProviderDisplay,
    formatProviderLabel,
} from '../../utils/model-display.js';

const REASONING_EFFORTS = ['minimal', 'low', 'medium', 'high'];

/**
 * Pick the tri-state mode that corresponds to a stored override value.
 *
 *   null/undefined -> 'inherit'  (no per-agent override; falls back to server)
 *   0              -> 'disable'  (Some(0) — explicit disable for this agent)
 *   any positive   -> 'custom'   (Some(n) — operator-specified value)
 */
function modeFromBudget(value) {
    if (value == null) return 'inherit';
    if (value === 0) return 'disable';
    return 'custom';
}

/**
 * Pick the tri-state mode for the OpenAI reasoning_effort enum.
 *
 *   null/undefined          -> 'inherit'
 *   "low"|"medium"|"high"|"minimal" -> 'custom'
 *
 * `reasoning_effort` has no Some(0) "disable" wire shape — it's
 * `Option<enum>`. So Disable mode doesn't apply; Inherit and Custom
 * are the only valid form modes.
 */
function modeFromEffort(value) {
    if (value == null || value === '') return 'inherit';
    return 'custom';
}

/**
 * Tri-state per-agent budget control — Inherit / Disable / Custom.
 *
 * Owns its own mode + draft signals so flipping Inherit -> Custom
 * doesn't immediately collapse back when the draft is empty (mirrors
 * the composer-advanced pattern from #804 Slice C).
 */
function BudgetTriState({ label, hint, agentValue, mode, draft }) {
    const onModeChange = (e) => {
        mode.value = e.target.value;
        if (mode.value !== 'custom') draft.value = '';
    };
    const onDraftChange = (e) => { draft.value = e.target.value; };

    // Inheritance hint: when in Inherit mode, show what the agent
    // currently inherits to (the server default — though the layered
    // resolved value isn't directly exposed at this scope; render the
    // server default as the most meaningful fallback for the operator).
    return html`
        <div class="settings-row">
            <label class="settings-label">${label}</label>
            <div class="agent-tristate">
                <select class="settings-select agent-tristate-mode"
                        value=${mode.value}
                        onChange=${onModeChange}>
                    <option value="inherit">Inherit (server default)</option>
                    <option value="disable">Disable (0)</option>
                    <option value="custom">Custom</option>
                </select>
                ${mode.value === 'custom' && html`
                    <input class="settings-input agent-tristate-value"
                           type="number" min="0" step="1024"
                           placeholder="tokens"
                           value=${draft.value}
                           onInput=${onDraftChange} />
                `}
            </div>
            ${hint && html`<span class="settings-hint">${hint}</span>`}
        </div>
    `;
}

/**
 * Reasoning-effort tri-state — Inherit / pick effort. Disable mode is
 * not offered because `reasoning_effort` is `Option<enum>` and has no
 * "Some(0)" wire shape; Inherit is the only "no override" path.
 */
function EffortTriState({ label, hint, mode, value }) {
    const onModeChange = (e) => {
        mode.value = e.target.value;
        if (mode.value !== 'custom') value.value = '';
    };
    return html`
        <div class="settings-row">
            <label class="settings-label">${label}</label>
            <div class="agent-tristate">
                <select class="settings-select agent-tristate-mode"
                        value=${mode.value}
                        onChange=${onModeChange}>
                    <option value="inherit">Inherit (server default)</option>
                    <option value="custom">Custom</option>
                </select>
                ${mode.value === 'custom' && html`
                    <select class="settings-select agent-tristate-value"
                            value=${value.value}
                            onChange=${e => { value.value = e.target.value; }}>
                        <option value="">choose...</option>
                        ${REASONING_EFFORTS.map(e => html`<option value=${e} key=${e}>${e}</option>`)}
                    </select>
                `}
            </div>
            ${hint && html`<span class="settings-hint">${hint}</span>`}
        </div>
    `;
}

async function refreshAgents() {
    try {
        const data = await listAgents();
        agents.value = data.agents || data || [];
    } catch (err) {
        console.error('[agents] fetch failed:', err);
    }
}

/** Small popup modal for editing agent settings. */
export function AgentEditModal({ agent, onClose }) {
    // Existing fields
    const description = useSignal(agent.description || '');
    const model = useSignal(agent.model || '');
    const posture = useSignal(agent.posture || '');
    const provider = useSignal(agent.provider || '');

    // Telegram token controls. The token is never returned by the
    // server (`has_telegram` is the only signal); we offer Set / Replace
    // / Remove actions.
    const telegramAction = useSignal('keep'); // 'keep' | 'set' | 'remove'
    const telegramTokenDraft = useSignal('');

    // Reasoning knob tri-state form-state (#804 Slice B). Mode + draft
    // are independent signals so flipping Inherit -> Custom doesn't
    // collapse back when the draft is empty. On Save, mode + draft +
    // the agent's current value combine into either a value field, a
    // `clear_*: true` flag, or an omission (no change).
    const thinkingMode = useSignal(modeFromBudget(agent.thinking_budget_tokens));
    const thinkingDraft = useSignal(
        agent.thinking_budget_tokens && agent.thinking_budget_tokens > 0
            ? String(agent.thinking_budget_tokens) : ''
    );
    const effortMode = useSignal(modeFromEffort(agent.reasoning_effort));
    const effortValue = useSignal(agent.reasoning_effort || '');
    const geminiMode = useSignal(modeFromBudget(agent.gemini_thinking_budget));
    const geminiDraft = useSignal(
        agent.gemini_thinking_budget && agent.gemini_thinking_budget > 0
            ? String(agent.gemini_thinking_budget) : ''
    );

    const saving = useSignal(false);
    const error = useSignal('');

    const serverModelId = serverDefaults.value.model || '';
    const serverProvider = serverDefaults.value.provider || '';

    /**
     * Build the PUT body from the current form state.
     *
     * For each tri-state knob, we emit one of:
     *   - the value field (Some(n) or Some(0))
     *   - `clear_*: true` (when reverting to inherit AND the agent
     *     currently has a non-None value)
     *   - nothing (when no change is intended)
     *
     * We never send both a value and `clear_*: true` for the same
     * knob — that's a 400 CLEAR_AND_VALUE_CONFLICT.
     */
    const buildPatchBody = () => {
        const body = {};

        // description / model / posture / provider — empty string clears
        // an existing value (handler treats "" as None).
        if (description.value !== (agent.description || '')) {
            body.description = description.value;
        }
        if ((model.value || '') !== (agent.model || '')) {
            body.model = model.value || '';
        }
        if ((posture.value || '') !== (agent.posture || '')) {
            body.posture = posture.value || '';
        }
        if ((provider.value || '') !== (agent.provider || '')) {
            body.provider = provider.value || '';
        }

        // Telegram token: only acts when the operator picks Set or Remove.
        if (telegramAction.value === 'set' && telegramTokenDraft.value.trim()) {
            body.telegram_token = telegramTokenDraft.value.trim();
        } else if (telegramAction.value === 'remove') {
            // Empty string clears the token (handler treats "" as None).
            body.telegram_token = '';
        }

        // thinking_budget_tokens
        const currentThinking = agent.thinking_budget_tokens;
        if (thinkingMode.value === 'inherit') {
            if (currentThinking != null) body.clear_thinking_budget_tokens = true;
        } else if (thinkingMode.value === 'disable') {
            if (currentThinking !== 0) body.thinking_budget_tokens = 0;
        } else if (thinkingMode.value === 'custom') {
            const n = parseInt(thinkingDraft.value, 10);
            if (!isNaN(n) && n >= 0 && n !== currentThinking) {
                body.thinking_budget_tokens = n;
            }
        }

        // reasoning_effort
        const currentEffort = agent.reasoning_effort || null;
        if (effortMode.value === 'inherit') {
            if (currentEffort != null) body.clear_reasoning_effort = true;
        } else if (effortMode.value === 'custom') {
            // Only send if a real value is selected and differs from current.
            if (effortValue.value && effortValue.value !== currentEffort) {
                body.reasoning_effort = effortValue.value;
            }
        }

        // gemini_thinking_budget
        const currentGemini = agent.gemini_thinking_budget;
        if (geminiMode.value === 'inherit') {
            if (currentGemini != null) body.clear_gemini_thinking_budget = true;
        } else if (geminiMode.value === 'disable') {
            if (currentGemini !== 0) body.gemini_thinking_budget = 0;
        } else if (geminiMode.value === 'custom') {
            const n = parseInt(geminiDraft.value, 10);
            if (!isNaN(n) && n >= 0 && n !== currentGemini) {
                body.gemini_thinking_budget = n;
            }
        }

        return body;
    };

    const onSave = async () => {
        saving.value = true;
        error.value = '';
        try {
            const body = buildPatchBody();
            // If the form produced no changes, just close — no PUT
            // needed. (Idempotent close, matches operator expectation.)
            if (Object.keys(body).length === 0) {
                onClose();
                return;
            }
            await updateAgent(agent.id, body);
            await refreshAgents();
            onClose();
        } catch (err) {
            error.value = err.error?.message || err.message || 'Save failed';
        } finally {
            saving.value = false;
        }
    };

    const onOverlayClick = (e) => {
        if (e.target === e.currentTarget) onClose();
    };

    const hasTelegram = !!agent.has_telegram;

    return html`
        <div class="settings-overlay open" onClick=${onOverlayClick}>
            <div class="settings-modal agent-edit-modal">
                <h2>${agent.name}</h2>

                <div class="settings-row">
                    <label class="settings-label">Description</label>
                    <input class="settings-input" type="text"
                           placeholder="optional"
                           value=${description.value}
                           onInput=${e => { description.value = e.target.value; }} />
                </div>

                <div class="settings-row">
                    <label class="settings-label">Model</label>
                    <input class="settings-input" type="text"
                           list="agent-model-suggestions"
                           placeholder=${serverModelId || 'server default'}
                           value=${model.value}
                           onInput=${e => { model.value = e.target.value; }} />
                    <datalist id="agent-model-suggestions">
                        ${MODEL_SUGGESTIONS.map(m => html`<option value=${m} />`)}
                    </datalist>
                    <span class="settings-effective">
                        Effective: <${ModelDisplay} value=${model.value.trim()} defaultValue=${serverModelId} />
                    </span>
                    <span class="settings-hint">Leave empty to use server default.</span>
                </div>

                <div class="settings-row">
                    <label class="settings-label">Provider</label>
                    <select class="settings-select"
                            value=${provider.value}
                            onChange=${e => { provider.value = e.target.value; }}>
                        <option value="">Default (${formatProviderLabel(serverProvider || 'openai')})</option>
                        <option value="openai">OpenAI</option>
                        <option value="anthropic">Anthropic</option>
                        <option value="openrouter">OpenRouter</option>
                    </select>
                    <span class="settings-effective">
                        Effective: <${ProviderDisplay} value=${provider.value} defaultValue=${serverProvider || 'openai'} />
                    </span>
                </div>

                <div class="settings-row">
                    <label class="settings-label">Posture</label>
                    <select class="settings-select"
                            value=${posture.value}
                            onChange=${e => { posture.value = e.target.value; }}>
                        <option value="">Server default (${serverDefaults.value.posture || 'guarded'})</option>
                        <option value="full_control">full_control</option>
                        <option value="guarded">guarded</option>
                        <option value="autonomous">autonomous</option>
                    </select>
                </div>

                <div class="agent-edit-section-divider"></div>
                <div class="agent-edit-section-title">Reasoning &amp; thinking</div>

                <${BudgetTriState}
                    label="Anthropic thinking budget"
                    hint="Inherit = use server default. Disable = Some(0) (force off for this agent). Custom = override with N tokens."
                    agentValue=${agent.thinking_budget_tokens}
                    mode=${thinkingMode}
                    draft=${thinkingDraft} />

                <${EffortTriState}
                    label="OpenAI reasoning effort"
                    hint="Inherit = use server default. Custom picks an effort level for this agent."
                    mode=${effortMode}
                    value=${effortValue} />

                <${BudgetTriState}
                    label="Gemini thinking budget"
                    hint="Inherit = use server default. Disable = Some(0) (force off for this agent). Custom = override with N tokens."
                    agentValue=${agent.gemini_thinking_budget}
                    mode=${geminiMode}
                    draft=${geminiDraft} />

                <div class="agent-edit-section-divider"></div>
                <div class="agent-edit-section-title">Telegram bot</div>

                <div class="settings-row">
                    <label class="settings-label">Token</label>
                    <div class="settings-info-row-header" style="flex-wrap:wrap;gap:6px;">
                        <span class="settings-info-row-value">
                            ${hasTelegram ? 'configured (token hidden)' : 'not configured'}
                        </span>
                        <select class="settings-select agent-tristate-mode"
                                value=${telegramAction.value}
                                onChange=${e => {
                                    telegramAction.value = e.target.value;
                                    if (e.target.value !== 'set') telegramTokenDraft.value = '';
                                }}>
                            <option value="keep">Keep</option>
                            <option value="set">${hasTelegram ? 'Replace' : 'Set'}</option>
                            ${hasTelegram && html`<option value="remove">Remove</option>`}
                        </select>
                    </div>
                    ${telegramAction.value === 'set' && html`
                        <input class="settings-input" type="password"
                               autocomplete="off"
                               placeholder="paste bot token..."
                               value=${telegramTokenDraft.value}
                               onInput=${e => { telegramTokenDraft.value = e.target.value; }} />
                    `}
                    <span class="settings-hint">
                        ${hasTelegram
                            ? 'A token is set but is never displayed. Replace overwrites it; Remove clears it.'
                            : 'Set a bot token to enable a dedicated Telegram polling loop for this agent.'}
                    </span>
                </div>

                ${error.value && html`<div class="inline-error">${error.value}</div>`}

                <div class="settings-footer">
                    <button class="settings-cancel" onClick=${onClose}>Cancel</button>
                    <button class="settings-save" onClick=${onSave} disabled=${saving.value}>
                        ${saving.value ? '...' : 'Save'}
                    </button>
                </div>
            </div>
        </div>
    `;
}

function AgentCard({ agent, isActive, onEdit }) {
    const error = useSignal('');
    const confirming = useSignal(false);
    const deleteTimer = useSignal(null);
    const serverModelId = serverDefaults.value.model || '';
    const serverProvider = serverDefaults.value.provider || '';

    const onDeleteClick = () => {
        confirming.value = true;
        // Auto-revert after 3 seconds if not confirmed
        deleteTimer.value = setTimeout(() => { confirming.value = false; }, 3000);
    };

    const onDeleteConfirm = async () => {
        if (deleteTimer.value) { clearTimeout(deleteTimer.value); deleteTimer.value = null; }
        confirming.value = false;
        try {
            await deleteAgent(agent.id);
            await refreshAgents();
            if (agent.id === activeAgentId.value) {
                const def = agents.value.find(a => a.is_default);
                const next = def || agents.value[0] || null;
                if (next) switchAgent(next.id);
                else activeAgentId.value = null;
            }
        } catch (err) {
            error.value = err.error?.message || err.message || 'Delete failed';
        }
    };

    const onDeleteCancel = () => {
        if (deleteTimer.value) { clearTimeout(deleteTimer.value); deleteTimer.value = null; }
        confirming.value = false;
    };

    const onSetDefault = async () => {
        try {
            await setDefaultAgent(agent.id);
            await refreshAgents();
        } catch (err) {
            error.value = err.error?.message || err.message || 'Failed';
        }
    };

    return html`
        <div class="agent-card ${isActive ? 'active' : ''}">
            <div class="agent-card-header">
                <span class="agent-card-name">${agent.name}</span>
                ${agent.is_default && html`<span class="agent-badge">default</span>`}
            </div>
            <div class="agent-card-meta agent-card-meta--model">
                <span class="agent-card-meta-label">model:</span>
                <${ModelDisplay} value=${agent.model} defaultValue=${serverModelId} />
            </div>
            ${agent.provider && html`
                <div class="agent-card-meta agent-card-meta--provider">
                    <span class="agent-card-meta-label">provider:</span>
                    <${ProviderDisplay} value=${agent.provider} defaultValue=${serverProvider} />
                </div>
            `}
            ${agent.posture && html`
                <div class="agent-card-meta agent-card-meta--posture">
                    <span class="agent-card-meta-label">posture:</span>
                    <span>${agent.posture}</span>
                </div>
            `}
            ${error.value && html`<div class="agent-error">${error.value}</div>`}
            <div class="agent-card-actions">
                <button class="agent-card-btn" onClick=${() => switchAgent(agent.id)}>Select</button>
                <button class="agent-card-btn" onClick=${() => onEdit(agent)}>Edit</button>
                ${!agent.is_default && html`
                    <button class="agent-card-btn" onClick=${onSetDefault}>Set Default</button>
                `}
                ${confirming.value
                    ? html`
                        <button class="agent-card-btn" style="color:var(--error); font-weight:600;" onClick=${onDeleteConfirm}>Confirm?</button>
                        <button class="agent-card-btn" onClick=${onDeleteCancel}>Cancel</button>
                    `
                    : html`<button class="agent-card-btn" style="color:var(--error);" onClick=${onDeleteClick}>Delete</button>`
                }
            </div>
        </div>
    `;
}

export function AgentsTab() {
    const newName = useSignal('');
    const error = useSignal('');
    const loading = useSignal(false);
    const editingAgent = useSignal(null);

    useEffect(() => {
        if (activePanelTab.value === 'agents') refreshAgents();
    }, [activePanelTab.value]);

    const onCreate = async () => {
        const name = newName.value.trim();
        if (!name) return;
        error.value = '';
        loading.value = true;
        try {
            const resp = await createAgent({ name });
            if (!resp.id) {
                console.warn('[agents] POST /agents returned no id for agent:', name, resp);
            }
            newName.value = '';
            await refreshAgents();
        } catch (err) {
            error.value = err.error?.message || err.message || 'Failed to create agent';
        } finally {
            loading.value = false;
        }
    };

    return html`
        <div class="agent-list-container">
            <div class="agent-create-row">
                <input type="text" placeholder="New agent name..."
                       aria-label="Agent name"
                       value=${newName.value}
                       onInput=${e => { newName.value = e.target.value; }}
                       onKeyDown=${e => { if (e.key === 'Enter') onCreate(); }} />
                <button class="agent-card-btn" onClick=${onCreate}
                        disabled=${loading.value}>
                    ${loading.value ? '...' : '+ Create'}
                </button>
            </div>

            ${error.value && html`<div class="agent-error">${error.value}</div>`}

            ${agents.value.length === 0
                ? html`<div class="empty-state">No agents</div>`
                : agents.value.map(a => html`
                    <${AgentCard} key=${a.id} agent=${a}
                                  isActive=${a.id === activeAgentId.value}
                                  onEdit=${(agent) => { editingAgent.value = agent; }} />
                `)
            }

            ${editingAgent.value && html`
                <${AgentEditModal}
                    agent=${editingAgent.value}
                    onClose=${() => { editingAgent.value = null; }} />
            `}
        </div>
    `;
}
