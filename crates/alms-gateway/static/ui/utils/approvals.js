/**
 * Normalize approval data from different sources into a consistent shape.
 *
 * The backend uses different field names depending on the source:
 *   - SSE `approval_required` event:  { capability, request, run_id, ... }
 *   - REST `GET /approvals` response: { tool, params, run_id, ... }
 *
 * This function maps both shapes into the canonical UI representation:
 *   { approvalId, tool, params, runId }
 *
 * All code that creates approval card messages should use this function
 * so field-name divergence is handled in exactly one place.
 *
 * @param {object} raw — raw approval data (SSE event or REST object)
 * @returns {{ approvalId: string, tool: string, params: any, runId: string|null }}
 */
export function normalizeApproval(raw) {
    return {
        approvalId: raw.approval_id,
        tool: raw.tool || raw.capability,
        params: raw.params || raw.request,
        runId: raw.run_id || null,
    };
}
