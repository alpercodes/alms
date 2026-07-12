//! Gateway implementation of [`SubagentSelfEventSink`] (#1180): streams a
//! subagent's own run events to its own session's SSE log so the fullscreen
//! subagent view streams live, replays, and seals on completion. Content events
//! reuse the top-level run's `RuntimeEventForwarder` → channel →
//! [`RunManager::send_event`] path; the single terminal outcome rides a
//! companion channel so its terminal SSE lands strictly after all content (and
//! evicts the run's text buffer).

use super::tools::RuntimeEventForwarder;
use crate::server::RunManager;
use crate::sse::{SseEventData, ToolInvocationId};
use alms_core::{RunId, SessionId};
use alms_runtime::RuntimeEvent;
use alms_tools::{EventForwarder, SubagentRunOutcome, SubagentSelfEventSink};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Gateway-backed [`SubagentSelfEventSink`].
#[derive(Debug, Clone)]
pub(crate) struct GatewaySubagentSelfSink {
    run_manager: RunManager,
}

impl GatewaySubagentSelfSink {
    pub(crate) fn new(run_manager: RunManager) -> Self {
        Self { run_manager }
    }
}

impl SubagentSelfEventSink for GatewaySubagentSelfSink {
    fn forwarder_for(&self, session_id: SessionId, run_id: RunId) -> Arc<dyn EventForwarder> {
        // Two channels + one drain per subagent: content on `content_tx`
        // (reusing `RuntimeEventForwarder`'s conversion), the single terminal
        // outcome on `terminal_tx`. Both live inside the returned forwarder,
        // which the coordinator drops at end-of-run — the drain reads all
        // content, THEN the terminal, then self-terminates.
        let (content_tx, content_rx) = mpsc::unbounded_channel::<RuntimeEvent>();
        let (terminal_tx, terminal_rx) = mpsc::unbounded_channel::<SubagentRunOutcome>();
        tokio::spawn(forward_subagent_self_events(
            content_rx,
            terminal_rx,
            run_id,
            session_id,
            self.run_manager.clone(),
        ));
        Arc::new(SubagentSelfForwarder {
            content: RuntimeEventForwarder::new(content_tx),
            terminal_tx,
        })
    }
}

/// The self-sink's forwarder: delegates content events to an inner
/// `RuntimeEventForwarder` and routes the terminal outcome to the drain's
/// companion channel. Content and terminal are separate channels, ordered by
/// the drain (all content first), so the terminal SSE never overtakes buffered
/// content regardless of when `forward_run_terminal` is called.
#[derive(Debug)]
struct SubagentSelfForwarder {
    content: RuntimeEventForwarder,
    terminal_tx: mpsc::UnboundedSender<SubagentRunOutcome>,
}

impl EventForwarder for SubagentSelfForwarder {
    fn forward_tool_start(
        &self,
        invocation_id: uuid::Uuid,
        tool: String,
        params: serde_json::Value,
        source_agent: Option<String>,
        task_id: Option<String>,
    ) {
        self.content
            .forward_tool_start(invocation_id, tool, params, source_agent, task_id);
    }
    fn forward_tool_end(
        &self,
        invocation_id: uuid::Uuid,
        ok: bool,
        result: serde_json::Value,
        source_agent: Option<String>,
        task_id: Option<String>,
    ) {
        self.content
            .forward_tool_end(invocation_id, ok, result, source_agent, task_id);
    }
    fn forward_token_delta(&self, delta: String, source_agent: Option<String>) {
        self.content.forward_token_delta(delta, source_agent);
    }
    fn forward_reasoning_delta(&self, text: String, source_agent: Option<String>) {
        self.content.forward_reasoning_delta(text, source_agent);
    }
    fn forward_stream_reset(&self) {
        self.content.forward_stream_reset();
    }
    fn forward_status(&self, phase: String, detail: Option<String>) {
        self.content.forward_status(phase, detail);
    }
    fn forward_warning(&self, code: String, message: String, source_agent: Option<String>) {
        self.content.forward_warning(code, message, source_agent);
    }
    fn forward_run_terminal(&self, outcome: SubagentRunOutcome) {
        let _ = self.terminal_tx.send(outcome);
    }
}

/// Drain a subagent's own (untagged) content onto its session SSE log, then emit
/// the terminal event once all content has been forwarded (#1180).
///
/// Content: `send_event` persists the non-ephemeral events (`reasoning_delta`,
/// `tool_start`/`tool_end`, `run_warning`, `stream_reset`) — making
/// `last_event_id` non-null and the stream replayable — and fans out
/// `token_delta`/`status` live only. `stream_reset` (#1162 sym-2) retracts a
/// painted partial before the buffered re-emit. The catch-all keeps the match
/// total for the variants a subagent never self-forwards.
///
/// Terminal: `content_rx` closes only when the forwarder is dropped at
/// end-of-run (after `forward_run_terminal` ran), so the terminal SSE
/// (`run_finished`/`run_error`/`run_cancelled`) is guaranteed to land after all
/// content — sealing the fullscreen bubble + clearing the spinner, mirroring a
/// top-level run. It also evicts the run's text buffer, which the coordinator's
/// `update_run` terminal path leaves behind, and then drops the run's per-run
/// SSE senders (`remove_senders`) so live `GET /runs/{run_id}/events`
/// subscribers see the terminal event and a clean stream close instead of a
/// forever-open connection — again mirroring a top-level run. The session
/// stream is a separate registry and stays open.
async fn forward_subagent_self_events(
    mut content_rx: mpsc::UnboundedReceiver<RuntimeEvent>,
    mut terminal_rx: mpsc::UnboundedReceiver<SubagentRunOutcome>,
    run_id: RunId,
    session_id: SessionId,
    run_manager: RunManager,
) {
    while let Some(event) = content_rx.recv().await {
        let sse = match event {
            RuntimeEvent::TokenDelta {
                delta,
                source_agent,
            } => SseEventData::token_delta(run_id, &delta, source_agent),
            RuntimeEvent::ReasoningDelta { text, source_agent } => {
                SseEventData::reasoning_delta(run_id, &text, source_agent)
            }
            RuntimeEvent::ToolStart {
                invocation_id,
                tool,
                params,
                source_agent,
                task_id,
            } => SseEventData::tool_start(
                run_id,
                ToolInvocationId(invocation_id),
                &tool,
                params,
                source_agent,
                task_id,
            ),
            RuntimeEvent::ToolEnd {
                invocation_id,
                ok,
                result,
                source_agent,
                task_id,
            } => SseEventData::tool_end(
                run_id,
                ToolInvocationId(invocation_id),
                ok,
                result,
                source_agent,
                task_id,
            ),
            RuntimeEvent::Status { phase, detail } => SseEventData::status(run_id, &phase, detail),
            RuntimeEvent::Warning {
                code,
                message,
                source_agent,
            } => SseEventData::run_warning(run_id, &code, &message, source_agent),
            RuntimeEvent::StreamReset { source_agent } => {
                SseEventData::stream_reset(run_id, source_agent)
            }
            _ => continue,
        };
        run_manager.send_event(run_id, session_id, sse).await;
    }

    // All content drained — emit the terminal event (if any) and evict the
    // run's in-flight text buffer.
    if let Some(outcome) = terminal_rx.recv().await {
        run_manager.evict_run_text_buffer(run_id);
        let sse = match outcome {
            SubagentRunOutcome::Completed { usage } => {
                SseEventData::run_finished(run_id, true, usage)
            }
            SubagentRunOutcome::Failed { error } => SseEventData::run_error(run_id, &error),
            SubagentRunOutcome::Cancelled => SseEventData::run_cancelled(run_id),
        };
        run_manager.send_event(run_id, session_id, sse).await;
    }

    // Close the run's per-run SSE streams, mirroring the top-level terminal
    // paths in `lifecycle.rs` (which call `remove_senders` after the terminal
    // broadcast). Without this, a client on `GET /runs/{run_id}/events` for a
    // LIVE subagent run kept its connection open forever after the run ended
    // (late subscribers were fine — replay-and-close). Ordering matters:
    // `send_event` above delivers the terminal SSE to live per-run
    // subscribers FIRST; dropping their senders here then ends their streams
    // cleanly. Runs unconditionally so a forwarder dropped without a terminal
    // outcome (abnormal end) still releases its subscribers. Touches only the
    // per-run `event_senders` registry — `session_senders` is separate, so
    // the fullscreen view's session stream stays open across runs.
    run_manager.remove_senders(run_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #1180: events forwarded through the sink land on the subagent session's
    /// SSE log — non-ephemeral ones (`reasoning_delta`, `tool_start`/`tool_end`)
    /// persist (non-null `last_event_id`, replayable), `token_delta` is live-only.
    #[tokio::test]
    async fn test_self_sink_writes_replayable_untagged_events_to_subagent_session_log() {
        let rm = RunManager::new();
        let sink = GatewaySubagentSelfSink::new(rm.clone());
        let session_id = SessionId::new();
        let run_id = RunId::new();

        // Before: the subagent session has no events (the #1180 bug state).
        assert_eq!(rm.latest_session_event_id(session_id).await, None);

        let fwd = sink.forwarder_for(session_id, run_id);
        // Untagged events, as the coordinator's self-forward produces them.
        fwd.forward_reasoning_delta("thinking".to_string(), None);
        fwd.forward_tool_start(
            uuid::Uuid::new_v4(),
            "echo".to_string(),
            serde_json::json!({"x": 1}),
            None,
            None,
        );
        fwd.forward_token_delta("hello ".to_string(), None);
        // #1180 / #1162 sym-2: StreamReset must reach the self stream so a
        // painted partial is retracted before the buffered re-emit.
        fwd.forward_stream_reset();

        // The drain task is async; poll until the three non-ephemeral events land.
        let mut events = Vec::new();
        for _ in 0..100 {
            events = rm.session_events_from(session_id, 0).await;
            if events.len() >= 3 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        // last_event_id is now non-null -> the fullscreen view has a replay log.
        assert!(
            rm.latest_session_event_id(session_id).await.is_some(),
            "subagent session last_event_id must be non-null after streaming"
        );

        let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert!(
            types.contains(&"reasoning_delta"),
            "reasoning_delta must be in the replay log, got {types:?}"
        );
        assert!(
            types.contains(&"tool_start"),
            "tool_start must be in the replay log, got {types:?}"
        );
        // #1162 sym-2: the partial-retraction reaches the subagent session log.
        assert!(
            types.contains(&"stream_reset"),
            "stream_reset must be in the replay log, got {types:?}"
        );
        // token_delta is ephemeral -> live-only, never persisted to the log.
        assert!(
            !types.contains(&"token_delta"),
            "token_delta must NOT be persisted to the session replay log, got {types:?}"
        );

        // Untagged, so the frontend renders them as the subagent's own activity.
        for e in &events {
            let tagged = e
                .data
                .get("source_agent")
                .map(|v| !v.is_null())
                .unwrap_or(false);
            assert!(
                !tagged,
                "subagent own-session events must be untagged, got {:?}",
                e.data
            );
        }
    }

    /// #1180: at end-of-run the self-sink emits the matching terminal SSE to the
    /// subagent's OWN session — AFTER all content — and evicts its text buffer.
    /// Covered for Completed / Failed / Cancelled. The terminal event is
    /// non-ephemeral, so `last_event_id` is non-null even for a token-only run.
    #[tokio::test]
    async fn test_self_sink_emits_terminal_sse_and_evicts_buffer() {
        for (outcome, expected_type) in [
            (
                SubagentRunOutcome::Completed {
                    usage: alms_core::TokenUsage::default(),
                },
                "run_finished",
            ),
            (
                SubagentRunOutcome::Failed {
                    error: "boom".to_string(),
                },
                "run_error",
            ),
            (SubagentRunOutcome::Cancelled, "run_cancelled"),
        ] {
            let rm = RunManager::new();
            let sink = GatewaySubagentSelfSink::new(rm.clone());
            let session_id = SessionId::new();
            let run_id = RunId::new();
            let fwd = sink.forwarder_for(session_id, run_id);

            // A live per-run subscriber (`GET /runs/{run_id}/events`) and a
            // live session subscriber, both connected BEFORE the run ends —
            // the pair the Codex P2 stream-close finding is about.
            let mut run_rx = rm.subscribe_run(run_id);
            let _session_subscription = rm.subscribe_session(session_id);

            // An untagged visible token_delta populates run_text_buffers[run_id]
            // (the buffer that would otherwise leak — finding 2).
            fwd.forward_token_delta("partial ".to_string(), None);
            fwd.forward_run_terminal(outcome);
            // Drop the forwarder so the content channel closes and the drain
            // proceeds to the terminal.
            drop(fwd);

            // Poll until the terminal event lands as the LAST session event.
            let mut last_type = None;
            for _ in 0..100 {
                let events = rm.session_events_from(session_id, 0).await;
                if let Some(e) = events.last() {
                    last_type = Some(e.event_type.clone());
                    if e.event_type == expected_type {
                        break;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert_eq!(
                last_type.as_deref(),
                Some(expected_type),
                "terminal SSE must be the last event on the subagent session"
            );
            // Non-ephemeral -> last_event_id non-null even for a token-only run.
            assert!(
                rm.latest_session_event_id(session_id).await.is_some(),
                "terminal event must give the token-only run a non-null last_event_id"
            );
            // Finding 2: the run's text buffer is evicted after terminal.
            assert!(
                rm.run_text_buffer_snapshot(run_id).is_none(),
                "run_text_buffers[self_run_id] must be evicted after terminal"
            );

            // Codex P2 (stream close): the live per-run subscriber receives
            // the terminal SSE and then its stream CLOSES — the drain drops
            // the run's `event_senders` entry after the terminal broadcast,
            // mirroring a top-level run. Pre-fix, `recv()` here would pend
            // forever (connection open until client disconnect / shutdown),
            // so the collect loop is bounded by a timeout for a clean
            // failure instead of a hung test.
            let per_run_events = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                let mut types = Vec::new();
                while let Some(e) = run_rx.recv().await {
                    types.push(e.event_type.clone());
                }
                types
            })
            .await
            .expect("per-run SSE stream must close after the terminal event");
            // Ordering: the terminal event reaches the live subscriber BEFORE
            // the close (it is the last thing on the channel).
            assert_eq!(
                per_run_events.last().map(String::as_str),
                Some(expected_type),
                "live per-run subscriber must receive the terminal SSE, then close; got {per_run_events:?}"
            );
            // The per-run sender registry entry is gone (the close above
            // proves `remove_senders` already ran, so no polling needed).
            assert!(
                !rm.event_senders.contains_key(&run_id),
                "event_senders[self_run_id] must be removed after terminal"
            );
            // The SESSION stream is a separate registry and must stay open —
            // the fullscreen subagent view relies on it across runs.
            assert!(
                rm.session_senders.contains_key(&session_id),
                "session_senders must be untouched by the per-run close"
            );
        }
    }
}
