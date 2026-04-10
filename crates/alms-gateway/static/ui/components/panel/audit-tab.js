import { html, useEffect, useSignal } from '../../deps.js';
import { auditEvents } from '../../state/audit.js';
import { activeSessionId } from '../../state/sessions.js';
import { activePanelTab } from '../../state/panel.js';
import { getAudit } from '../../api/audit.js';
import { fmtDate } from '../../utils/format.js';

const PAGE_SIZE = 50;

async function loadAudit(limit) {
    if (!activeSessionId.value) {
        auditEvents.value = null;
        return;
    }
    try {
        const data = await getAudit(activeSessionId.value, limit);
        auditEvents.value = data.events || data || [];
    } catch {
        auditEvents.value = [];
    }
}

export function AuditTab() {
    const auditLimit = useSignal(PAGE_SIZE);
    const loadingMore = useSignal(false);

    useEffect(() => {
        if (activePanelTab.value === 'audit') {
            auditLimit.value = PAGE_SIZE;
            loadAudit(PAGE_SIZE);
        }
    }, [activePanelTab.value, activeSessionId.value]);

    const onLoadMore = async () => {
        loadingMore.value = true;
        try {
            const newLimit = auditLimit.value + PAGE_SIZE;
            auditLimit.value = newLimit;
            await loadAudit(newLimit);
        } catch (err) {
            console.error('[AuditTab] loadMore failed:', err);
        } finally {
            loadingMore.value = false;
        }
    };

    if (!activeSessionId.value) {
        return html`<div class="audit-empty">No session selected</div>`;
    }
    if (auditEvents.value === null) {
        return html`<div class="loading-state">Loading...</div>`;
    }
    if (auditEvents.value.length === 0) {
        return html`<div class="audit-empty">No audit events</div>`;
    }

    // Show "Load more" if we got exactly the limit (more may exist)
    const mayHaveMore = auditEvents.value.length >= auditLimit.value;

    return html`
        <div>
            ${auditEvents.value.map((ev, i) => html`
                <div class="audit-event" key=${ev.id || `audit-${ev.timestamp || ''}-${i}`}>
                    <span class="audit-tool">${ev.tool || ev.action || 'unknown'}</span>
                    <span class="${ev.decision === 'deny' ? 'audit-deny' : ev.decision === 'error' ? 'audit-error' : 'audit-allow'}">
                        ${ev.decision === 'deny' ? 'denied' : ev.decision === 'error' ? 'error' : 'allowed'}
                    </span>
                    ${ev.timestamp && html`<span class="audit-time">${fmtDate(ev.timestamp)}</span>`}
                    ${ev.params && html`
                        <div class="audit-params">${JSON.stringify(ev.params).slice(0, 120)}</div>
                    `}
                </div>
            `)}
            ${mayHaveMore && html`
                <button class="audit-load-more"
                        onClick=${onLoadMore}
                        disabled=${loadingMore.value}>
                    ${loadingMore.value ? 'Loading...' : 'Load more'}
                </button>
            `}
        </div>
    `;
}
