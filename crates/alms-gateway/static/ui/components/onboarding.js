import { html, useSignal, useEffect, useRef } from '../deps.js';
import { createAgent, listAgents } from '../api/agents.js';
import { listKeys, setKey } from '../api/auth.js';
import { agents, replaceAgents } from '../state/agents.js';
import { switchAgent } from '../hooks/use-boot.js';
import { hasStoredKey, ONBOARDING_PROVIDER } from '../utils/onboarding-keys.js';

const KEYS_URL = 'https://openrouter.ai/keys';

/**
 * Step 1 — offer to store a provider key.
 *
 * An offer, never a gate (issue #162): `GET /auth/keys` reports only the
 * secrets store, and that is not the only place a working key can live — see
 * `utils/onboarding-keys.js` for the one that is invisible here. "No key
 * stored" is therefore not "no key". `onSkip` is always one click away and
 * the field is never required.
 */
function KeyStep({ onSaved, onSkip }) {
    const key = useSignal('');
    const error = useSignal('');
    const saving = useSignal(false);
    const input = useRef(null);

    useEffect(() => { input.current?.focus(); }, []);

    const onSubmit = async (e) => {
        e.preventDefault();
        const val = key.value.trim();
        if (!val) return;

        saving.value = true;
        error.value = '';
        try {
            await setKey(ONBOARDING_PROVIDER, val);
            key.value = '';
            onSaved();
        } catch (err) {
            error.value = err.error?.message || err.message || 'Failed to save key';
        } finally {
            saving.value = false;
        }
    };

    return html`
        <form class="onboard-card" onSubmit=${onSubmit}>
            <div class="onboard-step">Step 1 of 2</div>
            <h2>Welcome to ALMS</h2>
            <p>
                An agent answers through an LLM provider, so it needs a key before it can reply
                at all. <strong>OpenRouter</strong> is the recommended one: it is the default
                provider, and one key there covers both defaults —${' '}
                <code>z-ai/glm-5.2</code> for chat and <code>google/gemma-4-31b-it</code> for
                summaries (compaction and episodic memory). Nothing else to configure.
            </p>
            <div>
                <div class="onboard-label-row">
                    <label>OpenRouter API key</label>
                    <a href=${KEYS_URL} target="_blank" rel="noopener noreferrer">Get a key</a>
                </div>
                <input type="password" autocomplete="off" placeholder="sk-or-..."
                    value=${key.value}
                    ref=${input}
                    onInput=${(e) => { key.value = e.target.value; }}
                    disabled=${saving.value} />
                <div class="onboard-hint">
                    Applies to the running gateway immediately — no restart needed.
                </div>
            </div>
            <button class="onboard-btn" type="submit" disabled=${saving.value || !key.value.trim()}>
                ${saving.value ? 'Saving...' : 'Save Key'}
            </button>
            <div>
                <button class="onboard-skip" type="button" onClick=${onSkip} disabled=${saving.value}>
                    Skip for now
                </button>
                <div class="onboard-hint">
                    A key declared in <code>alms.toml</code>${' '}
                    (<code>api_key_env</code> under <code>[llm.providers.openrouter]</code>)
                    works but is not visible from here. You can also set one later in
                    Settings.
                </div>
            </div>
            <div class="onboard-error">${error.value}</div>
            <p class="onboard-footnote">
                The provider, the chat model and the summary model can all be changed later in
                Settings (the gear in the header).
            </p>
        </form>
    `;
}

/**
 * Step 2 — name and create the first agent. Unchanged from the single-step
 * flow this replaced; only its position moved.
 */
function NameStep({ stepLabel, keySaved }) {
    const name = useSignal('');
    const error = useSignal('');
    const loading = useSignal(false);
    const input = useRef(null);

    useEffect(() => { input.current?.focus(); }, []);

    const onSubmit = async (e) => {
        e.preventDefault();
        const val = name.value.trim();
        if (!val) return;

        // Validate: ASCII letters (either case), digits, hyphens, 1-64 chars.
        // Mirrors `validate_agent_name` in crates/alms-core/src/registry.rs,
        // which admits uppercase since #2 — rejecting it here would have made
        // the very first agent an operator creates un-capitalizable. The
        // backend enforces uniqueness case-insensitively, so a name that
        // differs from an existing one only in case comes back as a 409 and
        // lands in the catch below.
        if (!/^[A-Za-z0-9](?:[A-Za-z0-9-]{0,62}[A-Za-z0-9])?$/.test(val)) {
            error.value = 'Invalid name: letters, digits, hyphens only (1-64 chars, no trailing hyphen)';
            return;
        }

        loading.value = true;
        error.value = '';
        try {
            const resp = await createAgent({ name: val, is_default: true });
            const data = await listAgents();
            replaceAgents(data.agents || []);
            const newId = resp.id || (agents.value.find(a => a.name === val) || {}).id;
            if (newId) {
                await switchAgent(newId);
            } else {
                console.warn('[onboarding] POST /agents returned no id for agent:', val, resp);
            }
        } catch (err) {
            error.value = err.error?.message || err.message || 'Failed to create agent';
        } finally {
            loading.value = false;
        }
    };

    return html`
        <form class="onboard-card" onSubmit=${onSubmit}>
            ${stepLabel && html`<div class="onboard-step">${stepLabel}</div>`}
            <h2>Name your agent</h2>
            ${keySaved && html`<div class="onboard-note">OpenRouter key saved — live now, no restart.</div>`}
            <p>Create your first agent to get started. The agent will introduce itself and learn about you in a short setup conversation.</p>
            <div>
                <label>Agent name</label>
                <input type="text" placeholder="my-agent"
                    value=${name.value}
                    ref=${input}
                    onInput=${(e) => { name.value = e.target.value; }}
                    disabled=${loading.value} />
                <div class="onboard-hint">letters, digits, hyphens (1-64 chars)</div>
            </div>
            <button class="onboard-btn" type="submit" disabled=${loading.value || !name.value.trim()}>
                ${loading.value ? 'Creating...' : 'Create Agent'}
            </button>
            <div class="onboard-error">${error.value}</div>
        </form>
    `;
}

export function OnboardingView() {
    // 'probing' -> awaiting GET /auth/keys, 'key' -> step 1, 'name' -> step 2.
    // The probe only ever skips step 1; it never adds a requirement, so a
    // failed probe lands on 'key' like an empty one.
    const step = useSignal('probing');
    const sawKeyStep = useSignal(false);
    const keySaved = useSignal(false);

    useEffect(() => {
        let cancelled = false;
        const resolve = (next) => {
            if (cancelled) return;
            step.value = next;
            if (next === 'key') sawKeyStep.value = true;
        };
        listKeys()
            .then((data) => resolve(hasStoredKey(data) ? 'name' : 'key'))
            .catch((err) => {
                console.warn('[onboarding] GET /auth/keys failed:', err);
                resolve('key');
            });
        return () => { cancelled = true; };
    }, []);

    // One frame of "checking" beats flashing step 1 and yanking it away from
    // an operator who already has a key stored.
    if (step.value === 'probing') {
        return html`
            <div id="onboarding">
                <div class="onboard-card">
                    <h2>Welcome to ALMS</h2>
                    <p class="onboard-hint">Checking configuration...</p>
                </div>
            </div>
        `;
    }

    return html`
        <div id="onboarding">
            ${step.value === 'key'
                ? html`<${KeyStep}
                    onSaved=${() => { keySaved.value = true; step.value = 'name'; }}
                    onSkip=${() => { step.value = 'name'; }} />`
                : html`<${NameStep}
                    stepLabel=${sawKeyStep.value ? 'Step 2 of 2' : null}
                    keySaved=${keySaved.value} />`
            }
        </div>
    `;
}
