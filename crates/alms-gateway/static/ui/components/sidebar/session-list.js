import { html, batch, useSignal } from '../../deps.js';
import {
    sessions,
    activeSessionId,
    crossAgentSessions,
    expandedAgentId,
    jobsGroupExpanded,
    replaceSessionScopes,
} from '../../state/sessions.js';
import { agents, activeAgentId, activeAgent } from '../../state/agents.js';
import { replaceMessages } from '../../state/chat-actions.js';
import { activeRunId, selectedRunId, clearRuns, replaceRuns } from '../../state/runs.js';
import { bgRuns, messageQueue } from '../../state/queue.js';
import { auditEvents } from '../../state/audit.js';
import { listSessions, createSession, deleteSession } from '../../api/sessions.js';
import { openSessionStream, closeSessionStream } from '../../hooks/use-session-stream.js';
import { clearAllSubagents } from '../../state/subagents.js';
import { saveActiveSession, switchAgent, fetchCrossAgentSurfaces } from '../../hooks/use-boot.js';
import { bumpSelectGeneration } from '../../state/select-generation.js';
import { navigateToSession } from '../../utils/navigate-session.js';
import { clearComposerState } from '../../state/composer-storage.js';
import {
    expandAgent,
    isAgentExpanded,
    groupSessionsByAgent,
    isOwnedByActiveAgent,
    sortCrossAgentRows,
    filterJobSessions,
} from '../../utils/sidebar-grouping.js';

/**
 * Session type metadata: icon character and CSS class suffix.
 */
const SESSION_TYPE_META = {
    chat:         { icon: '\u25B8', cls: '',              label: 'Chat session' },
    dm:           { icon: '\u2194', cls: 'dm',            label: 'DM conversation' },
    notification: { icon: '\u26A1', cls: 'notification',  label: 'Notification session' },
    job:          { icon: '\u23F0', cls: 'job',           label: 'Job session' },
    subagent:     { icon: '\u2699', cls: 'subagent',      label: 'Subagent session' },
    telegram:     { icon: '\u2709', cls: 'telegram',      label: 'Telegram session' },
};

function getSessionMeta(session) {
    return SESSION_TYPE_META[session.session_type] || SESSION_TYPE_META.chat;
}

/**
 * Whether a session row should render the blinking active-run dot.
 *
 * PURE function of its inputs — the reactive signals are read by the
 * CALLER (`SessionItem`) directly in its render body and passed in. That
 * is load-bearing for reactivity, not a style choice:
 *
 * Pre-#1211 this read `bgRuns.value` / `activeRunId.value` *inside* the
 * helper. `SessionItem` therefore only subscribed to `activeSessionId`
 * (read in its body for `isActive`) reliably; its subscription to
 * `bgRuns` existed solely through this buried nested read. Under
 * @preact/signals@1.3.0 that left non-selected rows effectively
 * un-subscribed from live `bgRuns` changes, so a `session_activity_started`
 * event on ANOTHER session never re-rendered its row — the dot stayed
 * dark until an unrelated signal (`activeSessionId`, i.e. clicking the
 * row) forced a re-render. That is exactly the #1211 symptom: the dot
 * only lit on the currently-selected session. Backend emission of
 * `session_activity_started` for every run (lifecycle.rs) and the
 * `bgRuns` seed were already correct; the gap was purely that the row
 * did not reactively CONSULT them.
 *
 * Taking the values as arguments forces the reads into the component
 * body, matching the direct-body-read pattern the rest of the UI uses to
 * stay reliably reactive under @preact/signals (see agent-header-bar.js
 * and app.js, which both moved signal reads out of indirection for the
 * same reason).
 *
 * @param {string} sessionId
 * @param {string|null} activeSessionIdValue - `activeSessionId.value`
 * @param {string|null} activeRunIdValue - `activeRunId.value`
 * @param {Object<string, {runId: string|null, finished: boolean}>} bgRunsValue - `bgRuns.value`
 * @returns {boolean}
 */
function hasActiveRun(sessionId, activeSessionIdValue, activeRunIdValue, bgRunsValue) {
    if (sessionId === activeSessionIdValue && activeRunIdValue) return true;
    const bg = bgRunsValue[sessionId];
    return !!(bg && !bg.finished);
}

// Sidebar entry point — delegates to the shared in-app navigator
// (utils/navigate-session.js). Both the runs-tab and tool-output's
// "View full session" link share the same path so behaviour can't
// drift across call sites.
function selectSession(sessionId) {
    return navigateToSession(sessionId, { logPrefix: 'selectSession' });
}

async function newSession() {
    if (!activeAgentId.value) return;

    // Close old stream immediately and invalidate in-flight selectSession()
    // fetches so they do not overwrite the new session state.
    closeSessionStream();
    bumpSelectGeneration();

    try {
        const ctx = 'web-chat-' + Date.now();
        const resp = await createSession(activeAgentId.value, ctx);
        // Reload sessions — cross-agent DM/notification list stays
        // up to date because new chat sessions can race with new
        // notifications arriving for any agent in the same window.
        const [data, cross] = await Promise.all([
            listSessions(activeAgentId.value, {
                includeDms: true,
            }),
            fetchCrossAgentSurfaces(),
        ]);
        batch(() => {
            replaceSessionScopes(data.sessions || [], cross);
            activeSessionId.value = resp.session_id;
            saveActiveSession(activeAgentId.value, resp.session_id);
            selectedRunId.value = null;
            replaceMessages([]);
            messageQueue.value = [];
            replaceRuns(resp.session_id, []);
            auditEvents.value = null;
            clearAllSubagents();
        });
        openSessionStream(resp.session_id);
    } catch (err) {
        console.error('[newSession] failed:', err);
    }
}

/**
 * Format a DM session label from its participants array.
 * e.g. ["alice", "bob"] -> "alice <-> bob"
 */
function dmLabel(session) {
    const parts = session.participants;
    if (Array.isArray(parts) && parts.length >= 2) {
        return parts.join(' <-> ');
    }
    // Fallback: use context_id
    return session.context_id || session.id.slice(0, 8);
}

/**
 * Format a notification session label.
 *
 * The "owning agent" attribution is rendered separately as an
 * `.session-agent-attribution` badge inside `SessionItem`, so the
 * label here stays compact ("notifications") and the badge handles
 * the cross-agent disambiguation. Falls back to context_id when the
 * backend hasn't enriched the session with `agent_name` (legacy data).
 */
function notificationLabel(session) {
    if (session.agent_name) {
        return 'notifications';
    }
    return session.context_id || session.id.slice(0, 8);
}

/**
 * Format a scheduled-job session label (#1197).
 *
 * Job sessions carry `context_id = "job_{job_id}"` where job_id is a
 * full uuid — too long for a sidebar row. Render a compact
 * "job <first-8-chars>" so rows for different jobs stay
 * distinguishable; the row tooltip carries the full context_id.
 * Falls back to the raw context_id for anything unexpectedly shaped.
 */
function jobLabel(session) {
    const ctx = session.context_id || '';
    if (ctx.startsWith('job_') && ctx.length > 4) {
        return 'job ' + ctx.slice(4, 12);
    }
    return ctx || session.id.slice(0, 8);
}

/**
 * Derive a human-readable label for any session type.
 */
function sessionLabel(session) {
    if (session.session_type === 'dm') return dmLabel(session);
    if (session.session_type === 'notification') return notificationLabel(session);
    if (session.session_type === 'job') return jobLabel(session);
    return session.context_id || session.id.slice(0, 8);
}

/**
 * Determine which agent "owns" a cross-agent surface row from the
 * operator's perspective.
 *
 * Notification sessions carry a real `agent_name` set by the backend
 * from the `notifications:{agent}` context_id — that's the recipient
 * agent. Job sessions (#1197) are stored under the owning agent's real
 * `agent_id`, so the name resolves via the in-memory `agents` list —
 * the Jobs group is cross-agent, and without the badge two agents'
 * jobs would render as indistinguishable "job xxxxxxxx" rows. DM
 * sessions carry a `participants` array (both agents share the
 * session) so there's no single owner; the row label already shows
 * both names.
 *
 * Returns null when the session has no clear owning agent or when
 * the field hasn't been enriched (legacy data).
 */
function crossAgentOwner(session) {
    if (session.session_type === 'notification' && session.agent_name) {
        return session.agent_name;
    }
    if (session.session_type === 'job' && session.agent_id) {
        const owner = agents.value.find(a => a.id === session.agent_id);
        return owner ? owner.name : null;
    }
    return null;
}

function SessionItem({ session, activeAgentName }) {
    const confirming = useSignal(false);
    const deleteTimer = useSignal(null);
    const activeSessionIdValue = activeSessionId.value;
    const isActive = session.id === activeSessionIdValue;
    // Read the active-run signals in the component BODY (not only inside
    // hasActiveRun) so @preact/signals reliably subscribes this row to
    // both `activeRunId` and `bgRuns`. Without the body-level reads the
    // dot only lit on the selected session (#1211) — see hasActiveRun's
    // doc comment. All three values are always read (JS evaluates the
    // arguments eagerly), so the subscription can't drop out via
    // short-circuiting inside the predicate.
    const hasRun = hasActiveRun(
        session.id,
        activeSessionIdValue,
        activeRunId.value,
        bgRuns.value,
    );
    const meta = getSessionMeta(session);
    const typeClass = meta.cls ? ' session-item-' + meta.cls : '';

    const onDeleteClick = (e) => {
        e.stopPropagation();
        confirming.value = true;
        deleteTimer.value = setTimeout(() => { confirming.value = false; }, 3000);
    };

    const onDeleteConfirm = async (e) => {
        e.stopPropagation();
        if (deleteTimer.value) { clearTimeout(deleteTimer.value); deleteTimer.value = null; }
        confirming.value = false;
        try {
            await deleteSession(session.id);
            // Discard any persisted composer state (draft + queue) for
            // the deleted session so storage doesn't leak entries for
            // sessions that no longer exist on the backend. (#975 / #981
            // acceptance criterion: "if the session is deleted, both
            // draft and queue for that session are discarded".)
            clearComposerState(session.id);
            // If we deleted the active session, clear it
            if (session.id === activeSessionId.value) {
                closeSessionStream();
                batch(() => {
                    activeSessionId.value = null;
                    selectedRunId.value = null;
                    replaceMessages([]);
                    clearRuns();
                    auditEvents.value = null;
                    clearAllSubagents();
                    // Drop any in-memory queue items belonging to the
                    // session we just deleted — they're orphans now
                    // and shouldn't drift into a fresh session view.
                    messageQueue.value = [];
                });
            }
            // Refresh session list — both per-agent and cross-agent
            // (the deleted row could be a DM or notification owned by
            // a different agent than the one currently active).
            const [data, cross] = await Promise.all([
                listSessions(activeAgentId.value, {
                    includeDms: true,
                }),
                fetchCrossAgentSurfaces(),
            ]);
            replaceSessionScopes(data.sessions || [], cross);
        } catch (err) {
            console.error('[deleteSession] failed:', err);
        }
    };

    const onDeleteCancel = (e) => {
        e.stopPropagation();
        if (deleteTimer.value) { clearTimeout(deleteTimer.value); deleteTimer.value = null; }
        confirming.value = false;
    };

    const label = sessionLabel(session);
    const typeLabel = session.session_type !== 'chat' ? '\nType: ' + session.session_type : '';
    // Cross-agent attribution: notifications and job sessions get an
    // explicit owner badge so identical-looking rows from different
    // agents ("alice notifications" vs "bob notifications", two
    // agents' job rows) stay distinguishable. DMs already show both
    // participants in the label.
    const ownerName = crossAgentOwner(session);
    const ownedByActive = isOwnedByActiveAgent(session, activeAgentName);
    const ownedClass = ownedByActive ? ' session-item-active-agent' : '';

    return html`
        <div class="session-item${typeClass}${ownedClass} ${isActive ? 'active' : ''} ${hasRun ? 'has-run' : ''}"
             role="option"
             aria-selected=${isActive}
             tabindex="0"
             title=${'ID: ' + session.id + '\nContext: ' + session.context_id + typeLabel}
             onClick=${() => selectSession(session.id)}
             onKeyDown=${(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); selectSession(session.id); } }}>
            ${session.session_type !== 'chat' && html`<span class="session-type-icon session-type-icon-${meta.cls || 'default'}" aria-hidden="true" title=${meta.label}>${meta.icon}</span>`}
            <span class="session-label">${label}</span>
            ${ownerName && html`<span class="session-agent-attribution" title=${'Owned by ' + ownerName}>${ownerName}</span>`}
            ${confirming.value
                ? html`
                    <span class="session-delete-confirm-group" role="group" aria-label="Confirm delete">
                        <button class="session-confirm-btn session-confirm-yes"
                                title="Confirm delete (destructive)"
                                aria-label="Confirm delete"
                                onClick=${onDeleteConfirm}
                                onKeyDown=${(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); onDeleteConfirm(e); } }}>Yes</button>
                        <button class="session-confirm-btn session-confirm-no"
                                title="Cancel delete"
                                aria-label="Cancel delete"
                                onClick=${onDeleteCancel}
                                onKeyDown=${(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); onDeleteCancel(e); } }}>No</button>
                    </span>
                `
                : html`
                    <button class="session-delete-btn"
                            title="Delete session"
                            aria-label="Delete session"
                            onClick=${onDeleteClick}
                            onKeyDown=${(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); onDeleteClick(e); } }}>\u00D7</button>
                `
            }
        </div>
    `;
}

/**
 * Section divider with label and optional line, matching the DM divider style.
 *
 * `role="presentation"` because the divider is a visual cue, not a
 * listbox option (Tim review #2 — the listbox/option contract). The
 * actual a11y grouping is provided by the `<div role="group">` that
 * wraps the section's items downstream.
 */
function SectionDivider({ label, cls, id }) {
    return html`
        <div class="session-section-divider ${cls || ''}" role="presentation" id=${id}>
            <span class="session-section-divider-label">${label}</span>
        </div>
    `;
}

/**
 * Collapsible header for the sidebar's "Jobs" section (#1197).
 *
 * Scheduled-job sessions are surfaced by `GET /sessions` like
 * notifications, but unlike the always-open Notifications section they
 * render under a section header that is COLLAPSED by default:
 * recurring jobs accumulate one long-lived session each and cancelled
 * one-shots can leave dead sessions, so the group keeps them one click
 * away instead of crowding the sidebar. (It's still one row per job —
 * not per firing — see `filterJobSessions`.)
 *
 * Interaction contract mirrors `AgentGroupHeader`: `role="button"`,
 * Enter/Space activation, `aria-expanded`, chevron rotated via the
 * `.session-section-toggle.expanded .agent-group-chevron` rule in
 * styles/session-types.css (the agent accordion's rotation selector is
 * scoped to `.agent-group-header` and doesn't match this divider
 * variant). The body downstream reuses the `.agent-group-body` grid
 * transition so the two accordions animate identically.
 */
function JobsSectionHeader({ expanded, count, headerId }) {
    const onToggle = (e) => {
        e.stopPropagation();
        jobsGroupExpanded.value = !jobsGroupExpanded.value;
    };
    return html`
        <div class="session-section-divider session-divider-job session-section-toggle ${expanded ? 'expanded' : ''}"
             id=${headerId}
             role="button"
             tabindex="0"
             aria-expanded=${expanded}
             title=${expanded ? 'Collapse jobs' : 'Expand jobs'}
             onClick=${onToggle}
             onKeyDown=${(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); onToggle(e); } }}>
            <span class="agent-group-chevron" aria-hidden="true">${'\u25B8'}</span>
            <span class="session-section-divider-label">
                <span class="session-type-icon session-type-icon-job" aria-hidden="true">${'\u23F0'}</span>
                Jobs
            </span>
            <span class="agent-group-count" title=${count + ' job session' + (count === 1 ? '' : 's')}>${count}</span>
        </div>
    `;
}

/**
 * Click handler for an agent header in the sidebar accordion. Routes
 * through `expandAgent` for the pure-function transition decision and
 * triggers `switchAgent` only when the operator picks a *different*
 * agent than the currently active one.
 *
 * The signal `expandedAgentId` is decoupled from `activeAgentId` so
 * the operator can collapse the active agent's accordion body (Alper
 * UX feedback on PR #1010) without un-selecting the agent: the chat
 * view, dropdown, etc. stay on the active agent. `switchAgent` is the
 * one surface that re-syncs both signals (also re-expanding the
 * collapsed-but-active agent group when the operator picks the same
 * agent from the dropdown).
 *
 * Branches:
 *   - Click expanded agent → expandAgent returns null (collapse).
 *     activeAgentId is untouched. No switchAgent call.
 *   - Click collapsed-but-active agent → expandAgent returns its id
 *     (re-expand). activeAgentId already matches. No switchAgent call.
 *   - Click a different agent → expandAgent returns its id; we update
 *     `expandedAgentId` and call `switchAgent` so the rest of the
 *     UI follows.
 */
function onAgentHeaderClick(agentId) {
    const next = expandAgent(expandedAgentId.value, agentId);
    expandedAgentId.value = next;
    if (next && next !== activeAgentId.value) {
        switchAgent(next);
    }
}

/**
 * One row in the per-agent accordion: clickable agent header that
 * expands/collapses the agent's session list. Active agent gets a
 * subtle highlight.
 */
function AgentGroupHeader({ agent, expanded, sessionCount, isActive, headerId }) {
    const onClick = (e) => {
        e.stopPropagation();
        onAgentHeaderClick(agent.id);
    };
    const onKeyDown = (e) => {
        if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            onAgentHeaderClick(agent.id);
        }
    };
    // Tooltip: "Switch to X" for non-active agents (the click will
    // call switchAgent), "Collapse / Expand sessions" for the active
    // agent (the click toggles the accordion body without altering
    // activeAgentId — Alper UX feedback on PR #1010).
    const titleText = isActive
        ? (expanded ? 'Collapse sessions' : 'Expand sessions')
        : 'Switch to ' + agent.name;
    // The chevron is a CSS-rotated triangle. Using the escaped form
    // (\u25B8) for parity with SESSION_TYPE_META — grep-friendlier
    // than the raw glyph and avoids any UTF-8 BOM weirdness in
    // editors (Tim review #5 nit). The CSS rotates this 90° via the
    // .expanded class to indicate the open state.
    return html`
        <div class="agent-group-header ${expanded ? 'expanded' : ''} ${isActive ? 'active' : ''}"
             id=${headerId}
             role="button"
             tabindex="0"
             aria-expanded=${expanded}
             title=${titleText}
             onClick=${onClick}
             onKeyDown=${onKeyDown}>
            <span class="agent-group-chevron" aria-hidden="true">${'\u25B8'}</span>
            <span class="agent-group-name">${agent.name}</span>
            ${sessionCount != null && html`
                <span class="agent-group-count" title=${sessionCount + ' session' + (sessionCount === 1 ? '' : 's')}>
                    ${sessionCount}
                </span>
            `}
        </div>
    `;
}

/**
 * Deduplicate and validate DM sessions for the sidebar.
 *
 * The "Direct messages" section is *cross-agent* — operators want to
 * see DMs involving any of their agents regardless of which agent
 * they're currently in. So this function no longer filters by the
 * active agent's participation; that was the participant filter
 * removed when the section became cross-agent.
 *
 * 1. **Shape filter**: Only include DM sessions that have a
 *    well-formed participants array (>= 2 entries). Sessions with
 *    missing or malformed participants are excluded — the backend
 *    populates this field for well-formed DM sessions and a row
 *    rendered without it can't be labelled meaningfully.
 *
 * 2. **context_id dedup**: If the backend returns multiple entries with
 *    the same context_id (possible from legacy data where a DM session
 *    was stored under both the sentinel and the real agent_id), keep
 *    only the first occurrence. Runs after the shape filter so a
 *    malformed duplicate cannot shadow a valid entry.
 */
function filteredDmSessions(allSessions) {
    const seen = new Set();
    const result = [];
    for (const s of allSessions) {
        if (s.session_type !== 'dm') continue;
        // Shape filter: exclude DMs with missing/malformed participants
        if (!Array.isArray(s.participants) || s.participants.length < 2) continue;
        // Deduplicate by context_id (same DM pair = same context)
        const key = s.context_id || s.id;
        if (seen.has(key)) continue;
        seen.add(key);
        result.push(s);
    }
    return result;
}

export function SessionList() {
    const allSessions = sessions.value;
    const crossAgent = crossAgentSessions.value;
    const allAgents = agents.value;
    const activeAgentIdValue = activeAgentId.value;
    const expandedId = expandedAgentId.value;
    const agentName = activeAgent.value ? activeAgent.value.name : null;

    // Sessions are loaded per-active-agent today (see use-boot.js's
    // `loadAgentSessions`). The accordion still renders ALL agents as
    // collapsible group headers \u2014 only the active agent's group has
    // real chat session items underneath; collapsed groups show just
    // the header with the agent's name. Clicking another agent's
    // header calls `switchAgent` which loads that agent's sessions.
    //
    // Group the loaded sessions by agent_id so a session that snuck in
    // for a different agent (race during agent-switch, etc.) lands in
    // the correct group rather than under the active agent.
    //
    // The filter list mirrors backend `is_internal_context_id`
    // (`crates/alms-gateway/src/runs/mod.rs`) one-to-one \u2014 `dm`,
    // `notification`, `job`, `subagent`, `episodic`. `dm`,
    // `notification` and (since #1197) `job` get their own cross-agent
    // sections rendered below; `subagent` / `episodic` are
    // internal-context sessions the operator should never see as
    // ordinary chat rows. The backend `/sessions` listing excludes
    // subagent/episodic by default and now always returns job rows
    // (#1197) alongside notifications, and the #1065 resolver-led-boot
    // fix in `utils/load-session.js` Step 0 injects internal envelopes
    // directly into `sessions.value` so the active subagent session
    // can be resolved by `activeSession` / `isInternalSession`.
    // Without listing every internal type here, that injection would
    // leak subagent/episodic rows \u2014 and job rows would duplicate into
    // the chat groups (Codex P2 on PR #1074). Keep this list in
    // lock-step with the backend prefix list if a new internal
    // context_id family is added.
    const chatSessions = allSessions.filter(s =>
        s.session_type !== 'dm'
        && s.session_type !== 'notification'
        && s.session_type !== 'job'
        && s.session_type !== 'subagent'
        && s.session_type !== 'episodic'
    );
    const grouped = groupSessionsByAgent(chatSessions);

    // Per-agent session counts for the header badge. The active
    // agent's count comes from the in-memory `sessions` list above
    // (the per-agent fetch is the freshest source for that agent).
    // For non-active agents we fall back to `crossAgentSessions`,
    // which is fetched without an `agent_id` filter and therefore
    // carries chat sessions for every agent in the tenant. This lets
    // every agent header show a real count instead of going blank
    // until the operator switches into the agent. (Alper UX feedback
    // on PR #1010 \u2014 Tim flagged the active-only badge in his review;
    // using cross-agent data keeps the count truthful for all agents
    // without an extra round-trip.)
    const crossAgentChats = crossAgent.filter(s =>
        s.session_type !== 'dm'
        && s.session_type !== 'notification'
        && s.session_type !== 'job'
    );
    const crossAgentGrouped = groupSessionsByAgent(crossAgentChats);

    // Cross-agent surfaces: DMs, notifications and job sessions are
    // aggregated across all agents from `crossAgentSessions` rather
    // than the per-agent `sessions` list. Operators want to see
    // incoming DMs / outgoing notifications / scheduled-job activity
    // regardless of which agent is currently selected, so these
    // sections always render the full cross-agent set (DMs and
    // notifications sorted with active-agent rows pinned to the top;
    // job rows keep the backend's last-activity ordering).
    const dmSessions = sortCrossAgentRows(filteredDmSessions(crossAgent), agentName);
    const notifSessions = sortCrossAgentRows(
        crossAgent.filter(s => s.session_type === 'notification'),
        agentName
    );
    const jobSessions = filterJobSessions(crossAgent);
    const jobsExpanded = jobsGroupExpanded.value;

    const noAgents = !allAgents || allAgents.length === 0;
    const noContent = noAgents
        && dmSessions.length === 0
        && notifSessions.length === 0
        && jobSessions.length === 0;

    return html`
        <div class="sidebar-section" style="flex:1; min-height:0">
            <div class="sidebar-label">Sessions</div>
            <div id="session-list" role="listbox" aria-label="Sessions">
                ${noContent
                    ? html`<div class="empty-state">No sessions</div>`
                    : null
                }
                ${allAgents.map(agent => {
                    const expanded = isAgentExpanded(expandedId, agent.id);
                    const isActive = agent.id === activeAgentIdValue;
                    // The active agent's session items render from the
                    // per-agent in-memory list; non-active agents
                    // render header-only and switch+load on click.
                    const groupSessions = grouped.get(agent.id) || [];
                    // Count source: per-agent list for the active
                    // agent, cross-agent list for everyone else. Both
                    // are populated from the same backend query (one
                    // filtered by agent_id, one not) so the values
                    // agree at steady state \u2014 the per-agent list is
                    // just the freshest snapshot for the active agent
                    // and gets preferred to keep counts in lock-step
                    // with the rendered items.
                    const count = isActive
                        ? groupSessions.length
                        : (crossAgentGrouped.get(agent.id) || []).length;
                    const headerId = 'agent-group-header-' + agent.id;
                    // The body wrapper below is *always* rendered (with
                    // class="agent-group-body" and data-expanded
                    // reflecting the state) so the CSS height
                    // transition has both endpoints to animate
                    // between. WAI-ARIA listbox descendants must be
                    // role="option" or role="group" wrappers \u2014 wrapping
                    // the session items in role="group" with
                    // aria-labelledby pointing at the agent header
                    // restores the listbox/option contract that #1008
                    // standardised on. (Tim review #2.)
                    return html`
                        <div class="agent-group" key=${agent.id}>
                            <${AgentGroupHeader}
                                agent=${agent}
                                expanded=${expanded}
                                sessionCount=${count}
                                isActive=${isActive}
                                headerId=${headerId} />
                            <div class="agent-group-body"
                                 role="group"
                                 aria-labelledby=${headerId}
                                 data-expanded=${expanded}>
                                <div class="agent-group-sessions">
                                    ${groupSessions.length === 0
                                        ? html`<div class="empty-state agent-group-empty">No sessions</div>`
                                        : groupSessions.map(s => html`
                                            <${SessionItem} key=${s.id} session=${s} activeAgentName=${agentName} />
                                        `)
                                    }
                                </div>
                            </div>
                        </div>
                    `;
                })}
                ${dmSessions.length > 0 && html`
                    <${SectionDivider} label="Direct messages"
                                       cls="session-divider-dm"
                                       id="session-section-dms" />
                    <div role="group" aria-labelledby="session-section-dms">
                        ${dmSessions.map(s => html`
                            <${SessionItem} key=${s.id} session=${s} activeAgentName=${agentName} />
                        `)}
                    </div>
                `}
                ${notifSessions.length > 0 && html`
                    <${SectionDivider} label="Notifications"
                                       cls="session-divider-notification"
                                       id="session-section-notifications" />
                    <div role="group" aria-labelledby="session-section-notifications">
                        ${notifSessions.map(s => html`
                            <${SessionItem} key=${s.id} session=${s} activeAgentName=${agentName} />
                        `)}
                    </div>
                `}
                ${jobSessions.length > 0 && html`
                    <${JobsSectionHeader} expanded=${jobsExpanded}
                                          count=${jobSessions.length}
                                          headerId="session-section-jobs" />
                    <div class="agent-group-body"
                         role="group"
                         aria-labelledby="session-section-jobs"
                         data-expanded=${jobsExpanded}>
                        <div class="agent-group-sessions">
                            ${jobSessions.map(s => html`
                                <${SessionItem} key=${s.id} session=${s} activeAgentName=${agentName} />
                            `)}
                        </div>
                    </div>
                `}
            </div>
            <button id="new-session-btn" onClick=${newSession}>+ New session</button>
        </div>
    `;
}
