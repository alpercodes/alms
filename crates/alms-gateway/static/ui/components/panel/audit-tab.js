import { html, useEffect } from '../../deps.js';
import { auditEvents } from '../../state/audit.js';
import { activeSessionId } from '../../state/sessions.js';
import { activePanelTab } from '../../state/panel.js';
import { getAudit } from '../../api/audit.js';
import { fmtDate } from '../../utils/format.js';

async function loadAudit() {
    if (!activeSessionId.value) {
        auditEvents.value = null;
        return;
    }
    try {
        const data = await getAudit(activeSessionId.value);
        auditEvents.value = data.events || data || [];
    } catch {
        auditEvents.value = [];
    }
}

export function AuditTab() {
    useEffect(() => {
        if (activePanelTab.value === 'audit') loadAudit();
    }, [activePanelTab.value, activeSessionId.value]);

    if (!activeSessionId.value) {
        return html`<div class="audit-empty">No session selected</div>`;
    }
    if (auditEvents.value === null) {
        return html`<div style="color:var(--text-disabled); padding:var(--space-4); font-size:var(--text-sm);">Loading...</div>`;
    }
    if (auditEvents.value.length === 0) {
        return html`<div class="audit-empty">No audit events</div>`;
    }

    return html`
        <div>
            ${auditEvents.value.map((ev, i) => html`
                <div class="audit-event" key=${i}>
                    <span class="audit-tool">${ev.tool || ev.action || 'unknown'}</span>
                    <span class="${ev.decision === 'deny' ? 'audit-deny' : 'audit-allow'}">
                        ${ev.decision === 'deny' ? 'denied' : 'allowed'}
                    </span>
                    ${ev.timestamp && html`<span class="audit-time">${fmtDate(ev.timestamp)}</span>`}
                    ${ev.params && html`
                        <div class="audit-params">${JSON.stringify(ev.params).slice(0, 120)}</div>
                    `}
                </div>
            `)}
        </div>
    `;
}
