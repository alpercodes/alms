import { html } from '../../deps.js';
import { post } from '../../api/client.js';

async function resolveApproval(approvalId, decision) {
    try {
        await post(`/approvals/${approvalId}`, { decision });
    } catch (err) {
        console.error('[resolveApproval] failed:', err);
    }
}

export function ApprovalCard({ approvalId, tool, params, resolved, decision }) {
    const onApprove = () => resolveApproval(approvalId, 'approve');
    const onDeny = () => resolveApproval(approvalId, 'deny');

    return html`
        <div class="approval-card ${resolved ? 'resolved' : ''}">
            <h3>
                ${resolved
                    ? `${decision === 'approve' ? '\u2713 Approved' : '\u2717 Denied'} \u2014 ${tool}`
                    : `\u26a0 Approval required \u2014 ${tool}`
                }
            </h3>
            <pre>${JSON.stringify(params, null, 2)}</pre>
            ${!resolved && html`
                <div class="approval-btns">
                    <button class="btn btn-approve" onClick=${onApprove}>Approve</button>
                    <button class="btn btn-deny" onClick=${onDeny}>Deny</button>
                </div>
            `}
        </div>
    `;
}
