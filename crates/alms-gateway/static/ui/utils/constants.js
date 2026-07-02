// Shared truncation length constants used across UI components.

/** Max length for tool call summaries in the main chat view (tool-row). */
export const TOOL_SUMMARY_LEN = 80;

/** Max length for generic text previews (context-debug-row, audit-tab). */
export const PREVIEW_LEN = 120;

/** Human-readable labels for DM conversation end reasons.
 *  Shared across SSE handlers, history parsing, and message rendering
 *  to prevent drift between duplicate inline maps. */
export const DM_END_REASON_LABELS = {
    'ignored': 'no further replies',
    'depth_exceeded': 'message limit reached',
    'user_cancelled': 'cancelled by user',
    'errored': 'run failed',
};
