/**
 * Format an ISO timestamp to a short time string (HH:MM).
 */
export function fmtTime(iso) {
    if (!iso) return '';
    const d = new Date(iso);
    return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

/**
 * Format an ISO timestamp to a short date string.
 */
export function fmtDate(iso) {
    if (!iso) return '';
    const d = new Date(iso);
    return d.toLocaleDateString([], { month: 'short', day: 'numeric' });
}

/**
 * Scroll an element to the bottom.
 */
export function scrollToBottom(el) {
    if (el) el.scrollTop = el.scrollHeight;
}
