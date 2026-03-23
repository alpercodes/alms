import { html, useEffect, useSignal } from '../../deps.js';
import { jobs } from '../../state/jobs.js';
import { agents, activeAgentId } from '../../state/agents.js';
import { activePanelTab } from '../../state/panel.js';
import { listJobs, createJob, cancelJob } from '../../api/jobs.js';
import { fmtDate } from '../../utils/format.js';

const PRESETS = [
    { label: '1m',   cron: '* * * * *',       desc: 'Every minute' },
    { label: '5m',   cron: '*/5 * * * *',     desc: 'Every 5 minutes' },
    { label: '15m',  cron: '*/15 * * * *',    desc: 'Every 15 minutes' },
    { label: '30m',  cron: '*/30 * * * *',    desc: 'Every 30 minutes' },
    { label: '1h',   cron: '0 * * * *',       desc: 'Every hour' },
    { label: '6h',   cron: '0 */6 * * *',     desc: 'Every 6 hours' },
    { label: '12h',  cron: '0 */12 * * *',    desc: 'Every 12 hours' },
    { label: '1d',   cron: '0 0 * * *',       desc: 'Daily at midnight' },
];

/** Human-readable description of a cron expression. */
function describeCron(expr) {
    if (!expr) return '';
    const preset = PRESETS.find(p => p.cron === expr.trim());
    if (preset) return preset.desc;
    const parts = expr.trim().split(/\s+/);
    if (parts.length !== 5) return 'Invalid cron (need 5 fields)';
    return expr;
}

/** Round a Date up to the nearest future minute. */
function defaultRunAt() {
    const d = new Date(Date.now() + 5 * 60_000); // 5 minutes from now
    d.setSeconds(0, 0);
    // Format as local datetime-local input value (YYYY-MM-DDTHH:MM)
    const pad = n => String(n).padStart(2, '0');
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

async function refreshJobs() {
    try {
        const data = await listJobs();
        jobs.value = data.jobs || data || [];
    } catch (err) {
        console.error('[jobs] fetch failed:', err);
    }
}

export function JobsTab() {
    const scheduleMode = useSignal('recurring'); // 'recurring' | 'once'
    const cron = useSignal('');
    const runAt = useSignal(defaultRunAt());
    const prompt = useSignal('');
    const agentId = useSignal(activeAgentId.value || '');
    const error = useSignal('');
    const success = useSignal('');
    const loading = useSignal(false);
    const customMode = useSignal(false);

    useEffect(() => {
        if (activePanelTab.value === 'jobs') refreshJobs();
    }, [activePanelTab.value]);

    useEffect(() => { agentId.value = activeAgentId.value || ''; }, [activeAgentId.value]);

    const cronDesc = describeCron(cron.value);

    // Determine if the form is valid for submission.
    const hasSchedule = scheduleMode.value === 'once'
        ? !!runAt.value
        : !!cron.value.trim();
    const canSubmit = !!agentId.value && hasSchedule && !!prompt.value.trim();

    const onCreate = async () => {
        if (!canSubmit) return;
        error.value = '';
        success.value = '';
        loading.value = true;
        try {
            let schedule;
            if (scheduleMode.value === 'once') {
                // Convert local datetime to UTC ISO string for the backend.
                const localDate = new Date(runAt.value);
                if (isNaN(localDate.getTime())) {
                    error.value = 'Invalid date/time. Please select a valid date.';
                    loading.value = false;
                    return;
                }
                schedule = { type: "once", run_at: localDate.toISOString() };
            } else {
                schedule = { type: "recurring", cron: cron.value.trim() };
            }
            await createJob({
                agent_id: agentId.value,
                schedule,
                prompt: prompt.value.trim(),
            });
            cron.value = '';
            prompt.value = '';
            customMode.value = false;
            runAt.value = defaultRunAt();
            success.value = scheduleMode.value === 'once'
                ? 'Job scheduled (one-time).'
                : 'Recurring job created.';
            // Clear success message after a few seconds.
            setTimeout(() => { success.value = ''; }, 4000);
            await refreshJobs();
        } catch (err) {
            const msg = err.error?.message || err.message || '';
            error.value = msg || 'Failed to create job. Check that all fields are filled and the schedule is valid.';
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

                <div class="schedule-mode-toggle">
                    <button class="cron-btn ${scheduleMode.value === 'recurring' ? 'active' : ''}"
                            onClick=${() => { scheduleMode.value = 'recurring'; }}>
                        Recurring
                    </button>
                    <button class="cron-btn ${scheduleMode.value === 'once' ? 'active' : ''}"
                            onClick=${() => { scheduleMode.value = 'once'; }}>
                        Run once
                    </button>
                </div>

                ${scheduleMode.value === 'recurring' ? html`
                    <div class="cron-presets">
                        ${PRESETS.map(p => html`
                            <button class="cron-btn ${cron.value === p.cron ? 'active' : ''}"
                                    title=${p.desc}
                                    onClick=${() => { cron.value = p.cron; customMode.value = false; }}>
                                ${p.label}
                            </button>
                        `)}
                        <button class="cron-btn ${customMode.value ? 'active' : ''}"
                                title="Custom cron expression"
                                onClick=${() => { customMode.value = true; cron.value = ''; }}>
                            custom
                        </button>
                    </div>

                    ${customMode.value && html`
                        <input class="jobs-input" type="text" placeholder="min hour dom mon dow"
                               value=${cron.value}
                               onInput=${e => { cron.value = e.target.value; }} />
                    `}

                    ${cron.value && html`
                        <div class="cron-preview">${cronDesc}</div>
                    `}
                ` : html`
                    <input class="jobs-input" type="datetime-local"
                           value=${runAt.value}
                           onInput=${e => { runAt.value = e.target.value; }} />
                `}

                <textarea class="jobs-textarea" rows="2" placeholder="Prompt for the agent..."
                          value=${prompt.value}
                          onInput=${e => { prompt.value = e.target.value; }}></textarea>

                ${!agentId.value && html`
                    <div style="color:var(--text-muted); font-size:var(--text-xs); font-style:italic;">
                        No agents available. Create an agent first.
                    </div>
                `}

                <button class="jobs-submit" onClick=${onCreate}
                        disabled=${loading.value || !canSubmit}>
                    ${loading.value ? 'Scheduling...' : 'Schedule'}
                </button>
            </div>

            ${success.value && html`<div class="jobs-success">${success.value}</div>`}
            ${error.value && html`<div class="jobs-error">${error.value}</div>`}

            <div class="jobs-divider"></div>

            ${jobs.value.length === 0
                ? html`<div class="jobs-empty">No scheduled jobs</div>`
                : jobs.value.map(j => html`
                    <div class="job-item">
                        <div class="job-prompt">${j.prompt || j.task || '(no prompt)'}</div>
                        <div class="job-meta">
                            <span>${describeCron(j.schedule?.cron) || (j.schedule?.type === 'once' ? 'Once at ' + fmtDate(j.schedule.run_at) : JSON.stringify(j.schedule))}</span>
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
