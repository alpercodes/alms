/**
 * Format an ISO timestamp to a short time string (HH:MM).
 */
export function fmtTime(iso) {
    if (!iso) return '';
    const d = new Date(iso);
    return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

/**
 * Format an ISO timestamp: time if today, short date otherwise.
 */
export function fmtDate(iso) {
    if (!iso) return '';
    const d = new Date(iso);
    const today = new Date();
    if (d.toDateString() === today.toDateString()) return fmtTime(iso);
    return d.toLocaleDateString([], { month: 'short', day: 'numeric' });
}

/**
 * Format an ISO timestamp for per-message timestamps in the chat view (#855).
 *
 * - Same calendar day: HH:MM only (e.g. "14:32").
 * - Older: short date + time (e.g. "Apr 25 14:32").
 *
 * Returns the empty string for falsy / unparsable input so the caller can
 * conditionally render the surrounding chrome.
 */
export function fmtMessageTime(iso) {
    if (!iso) return '';
    const d = new Date(iso);
    if (isNaN(d.getTime())) return '';
    const today = new Date();
    const time = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    if (d.toDateString() === today.toDateString()) return time;
    const datePart = d.toLocaleDateString([], { month: 'short', day: 'numeric' });
    return `${datePart} ${time}`;
}

/**
 * Scroll an element to the bottom.
 */
export function scrollToBottom(el) {
    if (el) el.scrollTop = el.scrollHeight;
}
