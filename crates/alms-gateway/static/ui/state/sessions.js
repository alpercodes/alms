import { signal, computed } from '../deps.js';

export const sessions = signal([]);
export const activeSessionId = signal(null);

/**
 * Whether to show notification sessions in the sidebar.
 * Persisted to localStorage so the preference survives reloads.
 */
export const showNotifications = signal(
    localStorage.getItem('alms_show_notifications') === 'true'
);

/**
 * The active session object (if any).
 * Computed from the sessions list and activeSessionId.
 */
export const activeSession = computed(() => {
    const id = activeSessionId.value;
    if (!id) return null;
    return sessions.value.find(s => s.id === id) || null;
});

/**
 * Whether the currently active session is a DM conversation.
 */
export const isDmSession = computed(() => {
    const s = activeSession.value;
    return s ? s.session_type === 'dm' : false;
});

/**
 * Whether the currently active session is a notification session.
 */
export const isNotificationSession = computed(() => {
    const s = activeSession.value;
    return s ? s.session_type === 'notification' : false;
});

/**
 * Whether the currently active session is an internal (read-only) type.
 * Internal types: notification, job, subagent.
 */
export const isInternalSession = computed(() => {
    const s = activeSession.value;
    if (!s) return false;
    return s.session_type === 'notification'
        || s.session_type === 'job'
        || s.session_type === 'subagent';
});

/**
 * Participants of the active DM session (empty array for non-DM sessions).
 */
export const dmParticipants = computed(() => {
    const s = activeSession.value;
    return (s && s.session_type === 'dm' && Array.isArray(s.participants))
        ? s.participants
        : [];
});
