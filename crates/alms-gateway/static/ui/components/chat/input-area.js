import { html, useRef, useEffect } from '../../deps.js';
import { activeSessionId } from '../../state/sessions.js';
import { activeAgentId, agents } from '../../state/agents.js';
import { activeRunId, runs } from '../../state/runs.js';
import { nextMsgId } from '../../state/chat.js';
import { transformMessages } from '../../state/chat-actions.js';
import { messageQueue } from '../../state/queue.js';
import { createRun, cancelRun as apiCancelRun } from '../../api/runs.js';
import {
    beginOptimisticMessage,
    getPendingMessages,
    setPendingRunId,
    rollbackOptimisticMessage,
} from '../../state/pending-messages.js';
import {
    loadDraft, saveDraft, clearDraft,
    loadQueue, saveQueue,
    planMountDrain,
    consumeAcceptedQueueHead,
} from '../../state/composer-storage.js';
import { IconSend, IconStop } from '../../utils/icons.js';

/**
 * Start a new run with the given text.
 *
 * @param {string} text -- the user message to send
 * @param {object} [opts]
 * @param {string} [opts.sessionId] -- override for activeSessionId.value.
 *   Used by handleRunEnd's queued-message path to avoid a microtask gap
 *   where activeSessionId could change between dequeue and execution
 *   (see issue #526).
 * @param {boolean} [opts.queued] -- true when startQueuedRun owns the
 *   dequeue and any follow-up drain.
 * @returns {Promise<boolean>} whether the submission was accepted by the
 *   composer. A rejected queued head must remain queued.
 */
export async function startRun(text, opts) {
    const sessionId = opts?.sessionId || activeSessionId.value;
    const agentId = activeAgentId.value;
    if (!agentId) {
        if (sessionId) {
            transformMessages(
                msgs => [...msgs, {
                    id: nextMsgId(),
                    type: 'error',
                    text: 'Select an agent before sending a message.',
                }],
                sessionId,
            );
        } else {
            console.warn('[startRun] rejected: no agent or session selected');
        }
        return false;
    }

    if (!sessionId) {
        console.warn('[startRun] rejected: no session selected');
        return false;
    }

    // A run ID is the first unambiguous correlation key emitted by the
    // backend. Until createRun returns one, accept only one submission for
    // this session; otherwise run_created could not identify which pending
    // thinking row belongs to which request.
    if (getPendingMessages(sessionId).length > 0) return false;

    const sentAt = new Date().toISOString();
    const optimisticMessage = {
        id: nextMsgId(),
        type: 'user',
        role: 'user',
        text,
        ts: sentAt,
    };
    beginOptimisticMessage(
        sessionId,
        text,
        optimisticMessage,
        [{ id: nextMsgId(), type: 'thinking', pending: true }],
    );

    try {
        // Per-run config overrides were removed in the #941 pivot — the
        // run body carries only session, agent, and input. Operators
        // change model / provider / posture / budgets via the agent
        // record (Agents panel) or server defaults (Settings modal).
        const runBody = {
            session_id: sessionId,
            agent_id: agentId,
            input: { type: 'text', text },
        };

        const runResp = await createRun(runBody);
        // Attach the run ID to the pending message so reconciliation can
        // match by run ID instead of text content (avoids false-positive
        // deduplication when the user sends identical text twice).
        if (sessionId && runResp?.run_id) {
            setPendingRunId(sessionId, optimisticMessage.id, runResp.run_id);
            // A terminal SSE event may have arrived before createRun returned.
            // Linking settles that pending row; resume the queue now because
            // the earlier terminal-event drain was correctly rejected.
            if (!opts?.queued && !activeRunId.value && messageQueue.value.length > 0) {
                await startQueuedRun(messageQueue.value[0], sessionId);
            }
        }
        // No need to open a per-run SSE stream — the session stream
        // (opened by use-boot.js) receives all events automatically.
        // run_created → token_delta → run_finished all arrive there.
    } catch (err) {
        // Run creation failed -- no run will persist the message.
        rollbackOptimisticMessage(sessionId, { messageId: optimisticMessage.id });
        transformMessages(msgs =>
            [...msgs.filter(m => m.type !== 'thinking'),
             { id: nextMsgId(), type: 'error', text: `Failed to start run: ${err.error?.message || err.message || err.status || 'unknown error'}` }],
            sessionId,
        );
        console.error('[startRun] failed:', err);
    }
    return true;
}

/**
 * Submit a queued entry and remove only that exact head after acceptance.
 *
 * @param {{text: string}} entry
 * @param {string} sessionId
 * @returns {Promise<boolean>}
 */
export async function startQueuedRun(entry, sessionId) {
    const queueAtStart = messageQueue.value;
    if (queueAtStart[0] !== entry) return false;

    const accepted = await startRun(entry.text, { sessionId, queued: true });
    const currentQueue = messageQueue.value;
    const next = consumeAcceptedQueueHead(currentQueue, entry, accepted);
    if (next === currentQueue) {
        // A session switch may replace the visible queue while createRun is
        // pending. Commit the accepted dequeue to its owning session's storage
        // without touching the newly visible session.
        const storedNext = consumeAcceptedQueueHead(queueAtStart, entry, accepted);
        if (storedNext === queueAtStart) return false;
        saveQueue(sessionId, storedNext);
        return true;
    }
    messageQueue.value = next;
    saveQueue(sessionId, next);
    if (!activeRunId.value && next.length > 0) {
        await startQueuedRun(next[0], sessionId);
    }
    return true;
}

function sendMessage(promptRef) {
    const text = promptRef.current.value.trim();
    if (!text || !activeSessionId.value || !activeAgentId.value) return;
    const sessionId = activeSessionId.value;
    // Keep the draft in place while the previous create request is still
    // acquiring its run ID. The user can send again once correlation is safe.
    if (!activeRunId.value && getPendingMessages(sessionId).length > 0) return;
    promptRef.current.value = '';
    promptRef.current.style.height = 'auto';
    // Clear any persisted draft for this session — the operator pressed
    // Send, so the in-progress draft is no longer in-progress regardless
    // of whether it goes straight to a run or gets queued behind one.
    // (Acceptance criterion for #981.)
    clearDraft(sessionId);

    if (activeRunId.value) {
        const next = [...messageQueue.value, { text }];
        messageQueue.value = next;
        // Persist the queue per-session so a session-switch round-trip
        // doesn't drop messages the operator has already queued behind
        // an in-flight run. (#975)
        saveQueue(sessionId, next);
        promptRef.current.focus();
        return;
    }

    startRun(text);
}

async function cancelCurrentRun() {
    if (!activeRunId.value) return;
    try {
        await apiCancelRun(activeRunId.value);
    } catch { /* SSE event will handle UI */ }
}

/**
 * Resize the composer textarea to fit its content, capped at 150px.
 * Called from both `onInput` (live typing) and the mount effect
 * (restored draft) so a multi-line restored draft renders at its
 * natural height instead of staying at the 1-row default.
 */
function autoGrow(el) {
    el.style.height = 'auto';
    el.style.height = Math.min(el.scrollHeight, 150) + 'px';
}

export function InputArea() {
    const promptRef = useRef(null);
    const hasAgent = agents.value.length > 0;
    const hasSession = !!activeSessionId.value;
    const hasActiveAgent = !!activeAgentId.value;
    const canSend = hasAgent && hasActiveAgent && hasSession;
    const isRunning = !!activeRunId.value;
    const placeholder = hasActiveAgent ? 'Send a message...' : 'Select an agent to send a message';
    const sessionId = activeSessionId.value;

    // Restore the persisted draft + queue whenever the active session
    // changes (also fires on first mount). The textarea is uncontrolled
    // so we drive its `.value` imperatively via the ref. The queue is a
    // global signal — every session-switch reducer wipes it to `[]`
    // (navigate-session, session-list newSession, timeline-tab,
    // switchAgent), so this effect re-hydrates it from the per-session
    // storage entry rather than racing those reducers. (#975 / #981)
    useEffect(() => {
        const el = promptRef.current;
        if (el) {
            const draft = loadDraft(sessionId);
            el.value = draft;
            // Re-run the auto-grow sizing so a multi-line restored draft
            // shows at its natural height instead of staying at 1 row.
            autoGrow(el);
        }
        const restoredQueue = loadQueue(sessionId);
        // Mount-effect drain — closes the orphan-queue window where the
        // run that the queue was waiting on has already finished by the
        // time we get back to this session. Two scenarios this catches
        // that the SSE drain in use-session-stream.js can't:
        //
        //   1. Cross-session-return: user starts a run on A, queues M1,
        //      switches to B (in-memory queue wiped to []), the run on
        //      A finishes while on B (SSE drain reads empty in-memory
        //      queue and skips), then comes back to A. Without this
        //      drain M1 sits in the queue forever.
        //
        //   2. Page-reload-during-run race: on reload, boot()'s SSE
        //      stream can deliver `run_finished` *before* this mount
        //      effect runs. The SSE drain reads the (still-empty)
        //      in-memory queue and skips. When this effect later
        //      re-hydrates from storage there's no live drain trigger
        //      left.
        //
        // The fix in both cases: when the mount effect sees a non-empty
        // restored queue with no active run, submit the head and remove it
        // only after the composer accepts it. Idempotent with the SSE drain
        // because both gate on `messageQueue.value.length > 0`, and exact
        // head identity prevents a stale async drain from removing a replacement.
        // There is exactly one
        // mount and one run-end event per restoration. (#975)
        //
        // The decision is delegated to `planMountDrain` so the contract
        // is pinned by a unit test without needing a Preact tree.
        // `activeAgentId` is threaded through because `startRun` short-
        // circuits on a null agent — peeling the head without an agent
        // would silently lose it. See planMountDrain doc for the matrix.
        const plan = planMountDrain({
            restoredQueue,
            activeRunId: activeRunId.value,
            activeAgentId: activeAgentId.value,
        });
        if (restoredQueue.length > 0) {
            // Hydrate the visible queue regardless of whether we drain —
            // the operator should see what was queued. When a drain
            // fires we re-write the remainder right after.
            messageQueue.value = restoredQueue;
        }
        if (plan.drain) {
            startQueuedRun(plan.head, sessionId).catch(err => {
                console.error('[queue] mount drain failed:', err);
            });
        }
    }, [sessionId]);

    const onKeyDown = (e) => {
        if (e.key === 'Enter' && !e.shiftKey) {
            e.preventDefault();
            sendMessage(promptRef);
        }
    };

    const onInput = () => {
        const el = promptRef.current;
        if (el) {
            autoGrow(el);
            // Persist the in-progress draft per-session. saveDraft()
            // removes the storage entry when the value is empty so a
            // user clearing the textarea also clears the stored draft.
            // (#981)
            saveDraft(sessionId, el.value);
        }
    };

    return html`
        <div id="input-area">
            <div class="input-container">
                <textarea id="prompt" ref=${promptRef} rows="1"
                          placeholder=${placeholder}
                          aria-label="Message input"
                          disabled=${!canSend}
                          onKeyDown=${onKeyDown}
                          onInput=${onInput}></textarea>
                ${isRunning
                    ? html`<button id="cancel-run" title="Stop run" aria-label="Stop run"
                                   onClick=${cancelCurrentRun}><${IconStop} /></button>`
                    : html`<button id="send" disabled=${!canSend}
                                   title="Send (Enter)" aria-label="Send message"
                                   onClick=${() => sendMessage(promptRef)}><${IconSend} /></button>`
                }
            </div>
        </div>
    `;
}
