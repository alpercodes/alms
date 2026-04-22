import { html, useEffect, useSignal, batch } from '../../deps.js';
import { activeAgentId } from '../../state/agents.js';
import { activeSessionId } from '../../state/sessions.js';
import { activeRunId, selectedRunId, runs, runListGeneration } from '../../state/runs.js';
import { replaceMessages } from '../../state/chat-actions.js';
import { auditEvents } from '../../state/audit.js';
import { messageQueue } from '../../state/queue.js';
import { sessionSwitchLoading } from '../../state/loading.js';
import { activePanelTab } from '../../state/panel.js';
import { clearAllSubagents, parentSessionId } from '../../state/subagents.js';
import { closeSessionStream } from '../../hooks/use-session-stream.js';
import { saveActiveSession } from '../../hooks/use-boot.js';
import { selectGeneration, bumpSelectGeneration } from '../../state/select-generation.js';
import { loadSession } from '../../utils/load-session.js';
import { listAgentRuns } from '../../api/runs.js';

const PAGE_SIZE = 50;

const STATUS_ICONS = {
    completed: '\u2713',  // checkmark
    failed:    '\u2717',  // X
    cancelled: '\u2298',  // circled slash
    running:   '\u22EF',  // midline dots
};

const TRIGGER_LABELS = {
    user:         'user',
    scheduled:    'scheduled',
    subagent:     'subagent',
    dm:           'dm',
    notification: 'notif',
    telegram:     'telegram',
};

const SESSION_TYPE_LABELS = {
    chat:         'chat',
    dm:           'dm',
    subagent:     'sub',
    job:          'job',
    notification: 'notif',
    telegram:     'tg',
};

/** Format milliseconds into a human-readable duration. */
function fmtDuration(ms) {
    if (ms == null) return '--';
    if (ms < 1000) return ms + 'ms';
    if (ms < 60000) return (ms / 1000).toFixed(1) + 's';
    const mins = Math.floor(ms / 60000);
    const secs = Math.round((ms % 60000) / 1000);
    return mins + 'm' + (secs > 0 ? secs + 's' : '');
}

/** Format a token count with k suffix for large values. */
function fmtTokens(n) {
    if (n == null) return '--';
    if (n >= 10000) return (n / 1000).toFixed(0) + 'k';
    if (n >= 1000) return (n / 1000).toFixed(1) + 'k';
    return String(n);
}

/** Format an ISO timestamp as a relative time string (e.g. "2m ago", "3h ago"). */
function timeAgo(iso) {
    if (!iso) return '';
    const diff = Date.now() - new Date(iso).getTime();
    if (diff < 0) return 'just now';
    const secs = Math.floor(diff / 1000);
    if (secs < 60) return secs + 's ago';
    const mins = Math.floor(secs / 60);
    if (mins < 60) return mins + 'm ago';
    const hours = Math.floor(mins / 60);
    if (hours < 24) return hours + 'h ago';
    const days = Math.floor(hours / 24);
    return days + 'd ago';
}

/** Navigate to a session (same pattern as session-list selectSession). */
async function navigateToSession(sessionId) {
    if (sessionId === activeSessionId.value) return;

    const gen = bumpSelectGeneration();

    closeSessionStream();
    batch(() => {
        activeSessionId.value = sessionId;
        activeRunId.value = null;
        selectedRunId.value = null;
        replaceMessages([]);
        messageQueue.value = [];
        auditEvents.value = null;
        clearAllSubagents();
        parentSessionId.value = null;
        sessionSwitchLoading.value = true;
    });

    saveActiveSession(activeAgentId.value, sessionId);

    try {
        await loadSession(sessionId, {
            isStale: () => gen !== selectGeneration,
            logPrefix: 'runsTab',
        });
    } finally {
        if (gen === selectGeneration) {
            sessionSwitchLoading.value = false;
        }
    }
}

export function RunsTab() {
    const agentRuns = useSignal([]);
    const loading = useSignal(false);
    const error = useSignal('');

    const fetchRuns = async () => {
        if (!activeAgentId.value) {
            agentRuns.value = [];
            return;
        }
        loading.value = true;
        error.value = '';
        try {
            const data = await listAgentRuns(activeAgentId.value, PAGE_SIZE);
            agentRuns.value = data.runs || [];
        } catch (err) {
            console.error('[RunsTab] fetch failed:', err);
            error.value = err.error?.message || err.message || 'Failed to load runs';
            agentRuns.value = [];
        } finally {
            loading.value = false;
        }
    };

    // Fetch when tab becomes active, agent changes, or a run state changes
    useEffect(() => {
        if (activePanelTab.value === 'runs') fetchRuns();
    }, [activePanelTab.value, activeAgentId.value, runListGeneration.value]);

    if (!activeAgentId.value) {
        return html`<div class="runs-tab-empty">No agent selected</div>`;
    }

    if (loading.value && agentRuns.value.length === 0) {
        return html`<div class="loading-state">Loading runs...</div>`;
    }

    if (error.value) {
        return html`
            <div>
                <div class="runs-tab-error">${error.value}</div>
                <button class="runs-tab-retry" onClick=${fetchRuns}>Retry</button>
            </div>
        `;
    }

    if (agentRuns.value.length === 0) {
        return html`<div class="runs-tab-empty">No runs yet</div>`;
    }

    return html`
        <div class="runs-tab">
            <div class="runs-tab-header">
                <span class="runs-tab-count">${agentRuns.value.length} run${agentRuns.value.length !== 1 ? 's' : ''}</span>
                <button class="runs-tab-refresh" onClick=${fetchRuns}
                        disabled=${loading.value} title="Refresh">
                    ${loading.value ? '...' : '\u21BB'}
                </button>
            </div>
            <div class="runs-tab-list">
                ${agentRuns.value.map(run => html`
                    <div class="runs-tab-row runs-tab-row--${run.status || 'unknown'}"
                         key=${run.run_id}
                         onClick=${() => run.session_id && navigateToSession(run.session_id)}
                         title=${'Run ' + run.run_id.slice(0, 8) + ' | Session ' + (run.session_id || '').slice(0, 8)}>
                        <div class="runs-tab-row-top">
                            <span class="runs-tab-status">${STATUS_ICONS[run.status] || '\u00B7'}</span>
                            <span class="runs-tab-trigger runs-tab-trigger--${run.trigger || 'user'}">
                                ${TRIGGER_LABELS[run.trigger] || run.trigger || 'user'}
                            </span>
                            <span class="runs-tab-session-type">
                                ${SESSION_TYPE_LABELS[run.session_type] || run.session_type || ''}
                            </span>
                            <span class="runs-tab-time">${timeAgo(run.ts)}</span>
                        </div>
                        <div class="runs-tab-row-bottom">
                            <span class="runs-tab-duration">${fmtDuration(run.duration_ms)}</span>
                            <span class="runs-tab-tools">${run.tool_call_count != null ? run.tool_call_count + ' tools' : ''}</span>
                            <span class="runs-tab-tokens">
                                ${run.usage
                                    ? fmtTokens(run.usage.prompt_tokens) + ' in / ' + fmtTokens(run.usage.completion_tokens) + ' out' +
                                        (typeof run.usage.reasoning_tokens === 'number' && run.usage.reasoning_tokens > 0
                                            ? ' (+' + fmtTokens(run.usage.reasoning_tokens) + ' reasoning)'
                                            : '') +
                                        // Anthropic prompt caching (#766): show cache-read
                                        // tokens when the run benefited from the cache.
                                        (typeof run.usage.cache_read_input_tokens === 'number' && run.usage.cache_read_input_tokens > 0
                                            ? ' (' + fmtTokens(run.usage.cache_read_input_tokens) + ' cached)'
                                            : '')
                                    : ''}
                            </span>
                        </div>
                    </div>
                `)}
            </div>
        </div>
    `;
}
