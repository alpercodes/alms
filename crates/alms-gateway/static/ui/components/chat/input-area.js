import { html, useRef, useEffect } from '../../deps.js';
import { activeSessionId } from '../../state/sessions.js';
import { activeAgentId, agents } from '../../state/agents.js';
import { activeRunId, runs } from '../../state/runs.js';
import { nextMsgId } from '../../state/chat.js';
import { appendMessage, transformMessages } from '../../state/chat-actions.js';
import { messageQueue } from '../../state/queue.js';
import { createRun, cancelRun as apiCancelRun } from '../../api/runs.js';
import { savePendingMessage, setPendingRunId, clearPendingMessage } from '../../state/pending-messages.js';
import {
    loadDraft, saveDraft, clearDraft,
    loadQueue, saveQueue,
    planMountDrain,
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
 */
export async function startRun(text, opts) {
    const sessionId = opts?.sessionId || activeSessionId.value;
    const agentId = activeAgentId.value;
    if (!agentId) {
        transformMessages(msgs =>
            [...msgs,
             { id: nextMsgId(), type: 'error', text: 'Select an agent before sending a message.' }]
        );
        return;
    }

    appendMessage(
        { id: nextMsgId(), type: 'user', role: 'user', text, ts: new Date().toISOString() },
        { id: nextMsgId(), type: 'thinking', pending: true },
    );

    // Track the optimistically-appended user message so it can be
    // re-injected if the user switches sessions before the backend
    // persists it to the session history.  (Fixes message-loss on
    // rapid session switch while agent is thinking.)
    if (sessionId) {
        savePendingMessage(sessionId, text);
    }

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
            setPendingRunId(sessionId, runResp.run_id);
        }
        // No need to open a per-run SSE stream — the session stream
        // (opened by use-boot.js) receives all events automatically.
        // run_created → token_delta → run_finished all arrive there.
    } catch (err) {
        // Run creation failed -- no run will persist the message.
        if (sessionId) clearPendingMessage(sessionId);
        transformMessages(msgs =>
            [...msgs.filter(m => m.type !== 'thinking'),
             { id: nextMsgId(), type: 'error', text: `Failed to start run: ${err.error?.message || err.message || err.status || 'unknown error'}` }]
        );
        console.error('[startRun] failed:', err);
    }
}

function sendMessage(promptRef) {
    const text = promptRef.current.value.trim();
    if (!text || !activeSessionId.value || !activeAgentId.value) return;
    const sessionId = activeSessionId.value;
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
        // restored queue with no active run, peel off the head and start
        // a run with it. Idempotent with the SSE drain because both gate
        // on `messageQueue.value.length > 0` and there is exactly one
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
            messageQueue.value = plan.remaining;
            saveQueue(sessionId, plan.remaining);
            startRun(plan.head.text, { sessionId });
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
