import { html, useEffect, useSignal } from '../../deps.js';
import { jobs } from '../../state/jobs.js';
import { agents, activeAgentId } from '../../state/agents.js';
import { activePanelTab } from '../../state/panel.js';
import { listJobs, createJob, cancelJob } from '../../api/jobs.js';
import { fmtDate } from '../../utils/format.js';

async function refreshJobs() {
    try {
        const data = await listJobs();
        jobs.value = data.jobs || data || [];
    } catch (err) {
        console.error('[jobs] fetch failed:', err);
    }
}

export function JobsTab() {
    const cron = useSignal('');
    const prompt = useSignal('');
    const agentId = useSignal(activeAgentId.value || '');
    const error = useSignal('');
    const loading = useSignal(false);

    useEffect(() => {
        if (activePanelTab.value === 'jobs') refreshJobs();
    }, [activePanelTab.value]);

    useEffect(() => { agentId.value = activeAgentId.value || ''; }, [activeAgentId.value]);

    const onCreate = async () => {
        if (!agentId.value || !cron.value.trim() || !prompt.value.trim()) return;
        error.value = '';
        loading.value = true;
        try {
            await createJob({
                agent_id: agentId.value,
                schedule: { type: "recurring", cron: cron.value.trim() },
                prompt: prompt.value.trim(),
            });
            cron.value = '';
            prompt.value = '';
            await refreshJobs();
        } catch (err) {
            error.value = err.error?.message || err.message || 'Failed to create job';
        } finally {
            loading.value = false;
        }
    };

    const onCancel = async (id) => {
        try {
            await cancelJob(id);
            await refreshJobs();
        } catch (err) {
            error.value = err.error?.message || err.message || 'Failed to cancel job';
        }
    };

    return html`
        <div>
            <div class="jobs-form">
                <select class="jobs-select" value=${agentId.value}
                        onChange=${e => { agentId.value = e.target.value; }}>
                    ${agents.value.map(a => html`
                        <option value=${a.id}>${a.name}</option>
                    `)}
                </select>
                <input class="jobs-input" type="text" placeholder="Cron: */5 * * * *"
                       value=${cron.value}
                       onInput=${e => { cron.value = e.target.value; }} />
                <textarea class="jobs-textarea" rows="2" placeholder="Prompt..."
                          value=${prompt.value}
                          onInput=${e => { prompt.value = e.target.value; }}></textarea>
                ${!agentId.value && html`
                    <div style="color:var(--text-muted); font-size:var(--text-xs); font-style:italic;">No agents available. Create an agent first.</div>
                `}
                <button class="jobs-submit" onClick=${onCreate}
                        disabled=${loading.value || !agentId.value}>
                    ${loading.value ? '...' : 'Schedule'}
                </button>
            </div>

            ${error.value && html`<div class="jobs-error">${error.value}</div>`}

            <div class="jobs-divider"></div>

            ${jobs.value.length === 0
                ? html`<div class="jobs-empty">No scheduled jobs</div>`
                : jobs.value.map(j => html`
                    <div class="job-item">
                        <div class="job-prompt">${j.prompt || j.task || '(no prompt)'}</div>
                        <div class="job-meta">
                            <span>${j.schedule?.cron || JSON.stringify(j.schedule)}</span>
                            ${j.next_run_at && html`<span> | next: ${fmtDate(j.next_run_at)}</span>`}
                        </div>
                        <span class="job-status-${j.status || 'active'}">${j.status || 'active'}</span>
                        ${j.status !== 'cancelled' && html`
                            <button class="job-cancel" onClick=${() => onCancel(j.id)}>Cancel</button>
                        `}
                    </div>
                `)
            }
        </div>
    `;
}
