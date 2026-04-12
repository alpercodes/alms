import { html, useEffect, useSignal, batch } from '../../deps.js';
import { activeAgentId } from '../../state/agents.js';
import { activeSessionId } from '../../state/sessions.js';
import { activeRunId, selectedRunId } from '../../state/runs.js';
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
import { getAgentTimeline } from '../../api/timeline.js';

const PAGE_SIZE = 50;

/* ── Event type display config ── */

const EVENT_ICONS = {
    run_started:      '\u25B6',   // right-pointing triangle
    run_completed:    '\u2713',   // checkmark
    run_failed:       '\u2717',   // X mark
    run_cancelled:    '\u2298',   // circled slash
    run_ended:        '\u25A0',   // filled square (generic terminal)
    tool_call:        '\u2699',   // gear
    message_received: '\u25CF',   // filled circle
    message_sent:     '\u25CB',   // empty circle
    marker:           '\u2691',   // flag
};

const EVENT_LABELS = {
    run_started:      'started',
    run_completed:    'completed',
    run_failed:       'failed',
    run_cancelled:    'cancelled',
    run_ended:        'ended',
    tool_call:        'tool',
    message_received: 'message',
    message_sent:     'sent',
    marker:           'marker',
};

const SESSION_TYPE_LABELS = {
    chat:         'chat',
    dm:           'dm',
    subagent:     'sub',
    job:          'job',
    notification: 'notif',
    telegram:     'tg',
    episodic:     'epis',
};

/** Format an ISO timestamp as a relative time string. */
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

/** Format an ISO timestamp as HH:MM. */
function fmtTime(iso) {
    if (!iso) return '';
    const d = new Date(iso);
    return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

/** Determine the date group label for a timestamp. */
function dateGroup(iso) {
    if (!iso) return '';
    const d = new Date(iso);
    const today = new Date();
    const yesterday = new Date();
    yesterday.setDate(yesterday.getDate() - 1);
    if (d.toDateString() === today.toDateString()) return 'Today';
    if (d.toDateString() === yesterday.toDateString()) return 'Yesterday';
    return d.toLocaleDateString([], { weekday: 'short', month: 'short', day: 'numeric' });
}

/** Navigate to a session (same pattern as runs-tab.js). */
async function navigateToSession(sessionId) {
    if (!sessionId || sessionId === activeSessionId.value) return;

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
            logPrefix: 'timelineTab',
        });
    } finally {
        if (gen === selectGeneration) {
            sessionSwitchLoading.value = false;
        }
    }
}

export function TimelineTab() {
    const events = useSignal([]);
    const loading = useSignal(false);
    const loadingMore = useSignal(false);
    const error = useSignal('');
    const hasMore = useSignal(false);
    const nextBefore = useSignal(null);

    const fetchTimeline = async (append = false) => {
        if (!activeAgentId.value) {
            events.value = [];
            return;
        }
        if (append) {
            loadingMore.value = true;
        } else {
            loading.value = true;
        }
        error.value = '';
        try {
            const cursor = append ? nextBefore.value : null;
            const data = await getAgentTimeline(activeAgentId.value, PAGE_SIZE, cursor);
            const fetched = data.events || [];
            if (append) {
                events.value = [...events.value, ...fetched];
            } else {
                events.value = fetched;
            }
            hasMore.value = data.pagination?.has_more || false;
            nextBefore.value = data.pagination?.next_before || null;
        } catch (err) {
            console.error('[TimelineTab] fetch failed:', err);
            error.value = err.error?.message || err.message || 'Failed to load timeline';
            if (!append) events.value = [];
        } finally {
            loading.value = false;
            loadingMore.value = false;
        }
    };

    // Fetch when tab becomes active or agent changes
    useEffect(() => {
        if (activePanelTab.value === 'timeline') fetchTimeline(false);
    }, [activePanelTab.value, activeAgentId.value]);

    if (!activeAgentId.value) {
        return html`<div class="tl-empty">No agent selected</div>`;
    }

    if (loading.value && events.value.length === 0) {
        return html`<div class="loading-state">Loading timeline...</div>`;
    }

    if (error.value) {
        return html`
            <div>
                <div class="tl-error">${error.value}</div>
                <button class="tl-retry" onClick=${() => fetchTimeline(false)}>Retry</button>
            </div>
        `;
    }

    if (events.value.length === 0) {
        return html`<div class="tl-empty">No activity yet</div>`;
    }

    // Pre-compute which events start a new date group (avoids mutable state in render)
    const groupStarts = new Set();
    {
        let prev = '';
        for (const ev of events.value) {
            const g = dateGroup(ev.timestamp);
            if (g !== prev) { groupStarts.add(ev); prev = g; }
        }
    }

    return html`
        <div class="tl-tab">
            <div class="tl-header">
                <span class="tl-count">${events.value.length} event${events.value.length !== 1 ? 's' : ''}</span>
                <button class="tl-refresh" onClick=${() => fetchTimeline(false)}
                        disabled=${loading.value} title="Refresh">
                    ${loading.value ? '...' : '\u21BB'}
                </button>
            </div>
            <div class="tl-list">
                ${events.value.map((ev, idx) => {
                    const group = dateGroup(ev.timestamp);
                    const showGroup = groupStarts.has(ev);
                    const isToolCall = ev.event_type === 'tool_call';
                    const isRun = ev.event_type === 'run_started' || ev.event_type === 'run_completed'
                        || ev.event_type === 'run_failed' || ev.event_type === 'run_cancelled'
                        || ev.event_type === 'run_ended';
                    const toolName = ev.metadata?.tool_name;
                    const evKey = ev.event_type + '-' + ev.timestamp + '-' + (ev.run_id || '') + '-' + idx
                        + (toolName ? '-' + toolName : '');
                    return html`
                        ${showGroup && html`
                            <div class="tl-date-group" key=${'g-' + group}>${group}</div>
                        `}
                        <div class="tl-event tl-event--${ev.event_type}${isToolCall ? ' tl-event--indent' : ''}${isRun ? ' tl-event--run' : ''}"
                             key=${evKey}
                             onClick=${() => navigateToSession(ev.session_id)}
                             title=${'Session ' + (ev.session_id || '').slice(0, 8) + (ev.run_id ? ' | Run ' + ev.run_id.slice(0, 8) : '')}>
                            <span class="tl-time">${fmtTime(ev.timestamp)}</span>
                            <span class="tl-icon tl-icon--${ev.event_type}">${EVENT_ICONS[ev.event_type] || '\u00B7'}</span>
                            <span class="tl-session-badge tl-session-badge--${ev.session_type || 'chat'}">
                                ${SESSION_TYPE_LABELS[ev.session_type] || ev.session_type || 'chat'}
                            </span>
                            <span class="tl-event-label">${EVENT_LABELS[ev.event_type] || ev.event_type}</span>
                            <span class="tl-ago">${timeAgo(ev.timestamp)}</span>
                        </div>
                        ${ev.summary && html`
                            <div class="tl-summary${isToolCall ? ' tl-summary--indent' : ''}"
                                 onClick=${() => navigateToSession(ev.session_id)}>
                                ${ev.summary}
                            </div>
                        `}
                    `;
                })}
            </div>
            ${hasMore.value && html`
                <button class="tl-load-more"
                        onClick=${() => fetchTimeline(true)}
                        disabled=${loadingMore.value}>
                    ${loadingMore.value ? 'Loading...' : 'Load more'}
                </button>
            `}
        </div>
    `;
}
