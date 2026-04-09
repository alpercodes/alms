import { html, useSignal } from '../../deps.js';
import { post } from '../../api/client.js';

async function resolveApproval(approvalId, decision) {
    try {
        await post(`/approvals/${approvalId}`, { decision });
    } catch (err) {
        console.error('[resolveApproval] failed:', err);
    }
}

export function ApprovalCard({ approvalId, tool, params, resolved, decision }) {
    const submitting = useSignal(false);

    const onApprove = () => {
        if (submitting.value) return;
        submitting.value = true;
        resolveApproval(approvalId, 'approve');
    };
    const onDeny = () => {
        if (submitting.value) return;
        submitting.value = true;
        resolveApproval(approvalId, 'deny');
    };

    if (resolved) {
        const icon = decision === 'approve' ? '\u2713'
            : decision === 'cancelled' ? '\u2013'
            : decision === 'expired' ? '\u2013'
            : '\u2717';
        const label = decision === 'approve' ? 'Approved'
            : decision === 'cancelled' ? 'Cancelled'
            : decision === 'expired' ? 'Expired'
            : 'Denied';
        return html`
            <div class="approval-card resolved">
                <span>${icon} ${label} \u2014 ${tool}</span>
            </div>
        `;
    }

    const disabled = submitting.value;
    return html`
        <div class="approval-card">
            <h3>\u26a0 Approval required \u2014 ${tool}</h3>
            <pre>${JSON.stringify(params, null, 2)}</pre>
            <div class="approval-btns">
                <button class="btn btn-approve" onClick=${onApprove} disabled=${disabled}>
                    ${disabled ? 'Submitting...' : 'Approve'}
                </button>
                <button class="btn btn-deny" onClick=${onDeny} disabled=${disabled}>
                    ${disabled ? 'Submitting...' : 'Deny'}
                </button>
            </div>
        </div>
    `;
}
