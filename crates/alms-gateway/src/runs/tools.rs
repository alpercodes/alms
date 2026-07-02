//! Runtime event forwarding — bridges `alms-tools` events to `RuntimeEvent`.

use crate::approvals::{ApprovalStore, PendingApproval};
use crate::sse::{SseEventData, ToolInvocationId};
use alms_core::{RunId, SessionId};
use alms_runtime::RuntimeEvent;
use chrono::Utc;
use tokio::sync::mpsc;
use tracing::warn;

// ---------------------------------------------------------------------------
// RuntimeEventForwarder -- bridges alms-tools EventForwarder to RuntimeEvent
// ---------------------------------------------------------------------------

/// Concrete [`EventForwarder`](alms_tools::EventForwarder) that wraps a
/// `RuntimeEventSender` and maps each method to the corresponding
/// `RuntimeEvent` variant.
///
/// This is the bridge that lets tools in `alms-tools` emit events without
/// depending on `alms-runtime`'s `RuntimeEvent` enum.
#[derive(Debug, Clone)]
pub(super) struct RuntimeEventForwarder {
    tx: alms_runtime::RuntimeEventSender,
}

impl RuntimeEventForwarder {
    pub(super) fn new(tx: alms_runtime::RuntimeEventSender) -> Self {
        Self { tx }
    }
}

impl alms_tools::EventForwarder for RuntimeEventForwarder {
    fn forward_tool_start(
        &self,
        invocation_id: uuid::Uuid,
        tool: String,
        params: serde_json::Value,
        source_agent: Option<String>,
        task_id: Option<String>,
    ) {
        let _ = self.tx.send(RuntimeEvent::ToolStart {
            invocation_id,
            tool,
            params,
            source_agent,
            task_id,
        });
    }

    fn forward_tool_end(
        &self,
        invocation_id: uuid::Uuid,
        ok: bool,
        result: serde_json::Value,
        source_agent: Option<String>,
        task_id: Option<String>,
    ) {
        let _ = self.tx.send(RuntimeEvent::ToolEnd {
            invocation_id,
            ok,
            result,
            source_agent,
            task_id,
        });
    }

    fn forward_token_delta(&self, delta: String, source_agent: Option<String>) {
        let _ = self.tx.send(RuntimeEvent::TokenDelta {
            delta,
            source_agent,
        });
    }

    fn forward_reasoning_delta(&self, text: String, source_agent: Option<String>) {
        let _ = self
            .tx
            .send(RuntimeEvent::ReasoningDelta { text, source_agent });
    }

    fn forward_stream_reset(&self) {
        // Untagged: only the #1180 subagent self-forward calls this, on a stream
        // where the subagent is the main agent. `forward_runtime_events` /
        // `forward_subagent_self_events` convert it to the same `stream_reset`
        // SSE a top-level run emits.
        let _ = self
            .tx
            .send(RuntimeEvent::StreamReset { source_agent: None });
    }

    fn forward_status(&self, phase: String, detail: Option<String>) {
        let _ = self.tx.send(RuntimeEvent::Status { phase, detail });
    }

    fn forward_warning(&self, code: String, message: String, source_agent: Option<String>) {
        let _ = self.tx.send(RuntimeEvent::Warning {
            code,
            message,
            source_agent,
        });
    }

    fn forward_run_terminal(&self, _outcome: alms_tools::SubagentRunOutcome) {
        // No-op: the terminal event is only meaningful on a subagent's OWN
        // session (#1180), handled by the self-sink forwarder. On the parent
        // relay / top-level runtime channel the parent already learns of
        // completion via `subagent_completed` (the #1046 guard prevents a
        // double broadcast), so there is nothing to forward here.
    }

    fn forward_subagent_started(
        &self,
        tool_invocation_id: uuid::Uuid,
        subagent_name: Option<String>,
        subagent_session_id: uuid::Uuid,
        background: bool,
    ) {
        let _ = self.tx.send(RuntimeEvent::SubagentStarted {
            tool_invocation_id,
            subagent_name,
            subagent_session_id: SessionId(subagent_session_id),
            background,
        });
    }
}

// ---------------------------------------------------------------------------
// Runtime event forwarding
// ---------------------------------------------------------------------------

/// Info needed to cross-forward DM status events to the webchat session.
///
/// When present, key status phases (`calling_llm`, `executing_tools`) are
/// echoed to the agent's user-facing session as `dm_activity_status` events
/// so the status bar can show real-time DM activity.  See #651.
pub(super) struct DmCrossSessionInfo {
    pub agent_id: alms_core::AgentId,
    pub peer_name: String,
}

/// Reads RuntimeEvents from the runtime and forwards them as SSE events.
/// Also stores ApprovalRequired events in the approval store so clients can resolve them.
/// Warning events are persisted as lifecycle markers when `session_manager`
/// and `context_id` are provided and the session is user-facing.
#[allow(clippy::too_many_arguments)]
pub(super) async fn forward_runtime_events(
    mut rx: mpsc::UnboundedReceiver<RuntimeEvent>,
    run_id: RunId,
    session_id: SessionId,
    run_manager: crate::server::RunManager,
    approval_store: ApprovalStore,
    session_manager: std::sync::Arc<alms_session::SessionManager>,
    context_id: String,
    dm_cross_session: Option<DmCrossSessionInfo>,
) {
    // Cache the webchat session ID for DM cross-session forwarding so we
    // don't call `find_user_facing_session` (which does `list_all()` + sort)
    // on every status event.  Resolved lazily on first use.
    let mut cached_webchat_session: Option<Option<SessionId>> = None;

    while let Some(event) = rx.recv().await {
        match event {
            RuntimeEvent::TokenDelta {
                delta,
                source_agent,
            } => {
                run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::token_delta(run_id, &delta, source_agent),
                    )
                    .await;
            }
            RuntimeEvent::ReasoningDelta { text, source_agent } => {
                run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::reasoning_delta(run_id, &text, source_agent),
                    )
                    .await;
            }
            // #1162 sym-2: the streaming attempt painted a partial, then
            // faulted and fell back to a buffered `complete()`. Tell the UI to
            // drop the run's partial before the buffered full text re-streams.
            // Run-scoped and persisted (non-ephemeral) so the reasoning-
            // rehydration endpoint can use it as a boundary; `send_event` also
            // clears the in-memory visible-reply buffer on it.
            RuntimeEvent::StreamReset { source_agent } => {
                run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::stream_reset(run_id, source_agent),
                    )
                    .await;
            }
            RuntimeEvent::ToolStart {
                invocation_id,
                tool,
                params,
                source_agent,
                task_id,
            } => {
                run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::tool_start(
                            run_id,
                            ToolInvocationId(invocation_id),
                            &tool,
                            params,
                            source_agent,
                            task_id,
                        ),
                    )
                    .await;
            }
            RuntimeEvent::ToolEnd {
                invocation_id,
                ok,
                result,
                source_agent,
                task_id,
            } => {
                run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::tool_end(
                            run_id,
                            ToolInvocationId(invocation_id),
                            ok,
                            result,
                            source_agent,
                            task_id,
                        ),
                    )
                    .await;
            }
            RuntimeEvent::Status { phase, detail } => {
                // Cross-forward ALL DM status phases to the webchat session
                // so the status bar stays up to date (#651, #688).
                // Previously only `calling_llm` and `executing_tools` were
                // forwarded, causing the status bar to go blank during
                // `building_context` and `summarizing` phases.
                if let Some(ref dm_info) = dm_cross_session {
                    // Resolve the webchat session once and cache the result.
                    let webchat_sid = *cached_webchat_session.get_or_insert_with(|| {
                        super::find_user_facing_session(&session_manager, dm_info.agent_id)
                            .map(|s| s.id)
                    });

                    if let Some(target_session_id) = webchat_sid {
                        let dummy_run_id = RunId::new();
                        run_manager
                            .send_session_event(
                                target_session_id,
                                dummy_run_id,
                                SseEventData::dm_activity_status(
                                    target_session_id,
                                    &dm_info.peer_name,
                                    &phase,
                                    detail.clone(),
                                ),
                            )
                            .await;
                    }
                }

                run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::status(run_id, &phase, detail),
                    )
                    .await;
            }
            RuntimeEvent::ApprovalRequired {
                approval_id,
                tool,
                params,
                decision_tx,
                source_agent,
            } => {
                let request = serde_json::json!({"tool": &tool, "params": &params});
                approval_store.insert(PendingApproval {
                    approval_id,
                    run_id,
                    tool: tool.clone(),
                    params: params.clone(),
                    requested_at: Utc::now(),
                    decision_tx,
                });
                run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::approval_required(
                            run_id,
                            &approval_id.to_string(),
                            &tool,
                            request,
                            source_agent,
                        ),
                    )
                    .await;
            }
            RuntimeEvent::Warning {
                code,
                message,
                source_agent,
            } => {
                run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::run_warning(run_id, &code, &message, source_agent.clone()),
                    )
                    .await;

                // Persist warning marker so it survives page reloads.
                if !super::is_internal_context_id(&context_id) {
                    super::markers::persist_lifecycle_marker(
                        &session_manager,
                        session_id,
                        "run_warning",
                        message.clone(),
                        serde_json::json!({
                            "run_id": run_id.0.to_string(),
                            "code": code,
                            "source_agent": source_agent,
                        }),
                    );
                }
            }
            RuntimeEvent::ContextDebug {
                messages,
                tool_names,
                total_tokens,
                system_tokens,
                history_message_count,
                agent_id,
                agent_name,
            } => {
                run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::context_debug(
                            run_id,
                            messages,
                            tool_names,
                            total_tokens,
                            system_tokens,
                            history_message_count,
                            agent_id,
                            agent_name,
                        ),
                    )
                    .await;
            }
            // #1105: forwarded into the parent's SSE stream so the
            // SubagentBar's "View session" button can render live during
            // an `invoke_agent` run. Ordering invariant is preserved by
            // the channel FIFO: the parent's `ToolStart` for
            // `invoke_agent` is queued onto `runtime_tx` before
            // `tool.execute()` runs, and `spawn_subagent` only emits
            // `SubagentStarted` after that — so this event is always
            // delivered after the corresponding `tool_start`. Background
            // subagents share the same FIFO because the bg event
            // forwarder task in `runs/lifecycle.rs` forwards
            // `SubagentStarted` back onto the parent's `runtime_tx`
            // (via a `Weak` upgrade of `invoke_agent_fwd`) rather than
            // synthesising SSE on the bg channel directly.
            RuntimeEvent::SubagentStarted {
                tool_invocation_id,
                subagent_name,
                subagent_session_id,
                background,
            } => {
                run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::subagent_started(
                            session_id,
                            ToolInvocationId(tool_invocation_id),
                            subagent_name.clone(),
                            subagent_session_id,
                        ),
                    )
                    .await;

                // #1125 (A1-1): persist a durable `subagent_started`
                // lifecycle marker so a page reload that replays session
                // *history* (rather than the live SSE event log) can still
                // resolve the in-flight chip's session id. The live
                // `subagent_started` SSE above only lands in the per-session
                // event log, which `stream_session_events` replays from
                // `HWM+1` after a reload — so a `subagent_started` emitted
                // before the reload (≤ HWM) is never re-delivered and the
                // foreground chip rehydrates with `sessionId: null`. The
                // marker is recovered by `rehydrateSubagentsFromHistory`,
                // mirroring the `subagent_completion` marker
                // (`notifications.rs`).
                //
                // Scoped to FOREGROUND (`!background`): background subagents
                // are already reload-safe because their
                // `{task_id, session_id}` `invoke_agent` tool result is
                // persisted to history, so a start marker would be redundant.
                // Gated on `!is_internal_context_id` exactly like the
                // `run_warning` marker above so internal / DM contexts don't
                // accrue spurious markers.
                //
                // Marker metadata is symmetric with the live SSE event:
                // `subagent_session_id` (the chip's session id, keyed for
                // rehydration by `tool_invocation_id`) and `subagent_name`
                // only when present (omitted for ephemeral / unnamed
                // invocations, matching the SSE `skip_serializing_if`).
                if !background && !super::is_internal_context_id(&context_id) {
                    let mut extra = serde_json::json!({
                        "tool_invocation_id": tool_invocation_id.to_string(),
                        "subagent_session_id": subagent_session_id.0.to_string(),
                    });
                    if let Some(name) = subagent_name {
                        extra["subagent_name"] = serde_json::json!(name);
                    }
                    super::markers::persist_lifecycle_marker(
                        &session_manager,
                        session_id,
                        "subagent_started",
                        format!(
                            "Subagent{} started.",
                            extra
                                .get("subagent_name")
                                .and_then(|v| v.as_str())
                                .map(|n| format!(" '{n}'"))
                                .unwrap_or_default()
                        ),
                        extra,
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Background-subagent event routing (#1105)
// ---------------------------------------------------------------------------

/// Routes a single [`RuntimeEvent`] arriving on the background-subagent event
/// channel.
///
/// This function carries the #1105 invariant for background subagents:
/// while the parent run is alive, `SubagentStarted` is **forwarded back onto
/// the parent's runtime channel** via `parent_fwd` so the parent's
/// [`forward_runtime_events`] task can convert it to SSE in strict FIFO order
/// with the parent's `tool_start (invoke_agent)` event. The agent loop
/// enqueues the parent `ToolStart` onto `runtime_tx` *before* `tool.execute()`
/// runs, so by the time the bg task observes any event on its own channel the
/// parent's `ToolStart` is already ahead of it on `runtime_tx` — single-channel
/// FIFO from that point preserves the documented ordering.
///
/// Pre-#1105 the bg-channel handler synthesised SSE directly on a
/// session-level channel that was independent of the parent's `runtime_tx`,
/// which raced with the parent's `tool_start` and could deliver
/// `subagent_started` to the client *before* the parent's `tool_start`. The
/// frontend resolver (`findSubagentByToolInvocationId`) would then have no
/// `activeSubagents` entry to attach the `subagent_session_id` to.
///
/// `parent_fwd` encodes whether the parent run is still alive:
/// - `Some(fwd)` — the parent's `runtime_tx` is live (the bg task's
///   `Weak::upgrade` of `invoke_agent_fwd` succeeded). `SubagentStarted` is
///   rerouted through it, preserving the FIFO ordering above; this function
///   returns `None` so the caller does NOT also emit on the session stream
///   (avoids double-delivery).
/// - `None` — the parent run has finished and its `runtime_tx` /
///   `forward_runtime_events` task is gone (`Weak::upgrade` returned `None`).
///   The reroute would be silently dropped (#1125 A1-3), so instead this
///   function returns the `subagent_started` `SseEventData` directly and the
///   caller delivers it via `send_session_event` on the parent's session
///   stream — recovering the #1105 "View session during the run" surface for a
///   background subagent spawned near the end of a parent turn. Ordering is
///   still correct because the parent's `tool_start (invoke_agent)` was already
///   emitted (and persisted to the session event log) before the parent run
///   ended, so it carries a strictly lower session event id than this
///   fallback. The fallback path is independent of `runtime_tx`, so it does NOT
///   reintroduce the #1124 strong-sender deadlock.
///
/// Returns:
/// - `Some(SseEventData)` — the caller should emit this on the parent's
///   session-level SSE stream (`ToolStart` / `ToolEnd` events, plus the
///   dead-parent `SubagentStarted` fallback above).
/// - `None` — the event was handled internally (rerouted via the live
///   `parent_fwd`, auto-denied, or dropped); the caller does nothing.
///
/// The integration tests in `crates/alms-gateway/tests/sse_golden_tests.rs`
/// pin both legs: with a live `parent_fwd` this function returns `None` for
/// `SubagentStarted` *and* the matching event lands on the parent's runtime
/// channel (ordering); with `parent_fwd == None` it returns
/// `Some(subagent_started)` so the session-stream fallback fires (A1-3).
pub fn route_bg_event(
    event: RuntimeEvent,
    parent_fwd: Option<&dyn alms_tools::EventForwarder>,
    bg_run_id: RunId,
    bg_session_id: SessionId,
) -> Option<SseEventData> {
    match event {
        RuntimeEvent::ToolStart {
            invocation_id,
            tool,
            params,
            source_agent,
            task_id,
        } => Some(SseEventData::tool_start(
            bg_run_id,
            ToolInvocationId(invocation_id),
            &tool,
            params,
            source_agent,
            task_id,
        )),
        RuntimeEvent::ToolEnd {
            invocation_id,
            ok,
            result,
            source_agent,
            task_id,
        } => Some(SseEventData::tool_end(
            bg_run_id,
            ToolInvocationId(invocation_id),
            ok,
            result,
            source_agent,
            task_id,
        )),
        RuntimeEvent::ApprovalRequired {
            tool, decision_tx, ..
        } => {
            warn!(
                tool = %tool,
                "Background subagent requested approval -- \
                 not supported, auto-denying"
            );
            let _ = decision_tx.send(false);
            None
        }
        // #1105 / #1125 (A1-3) — two-leg routing keyed on parent liveness.
        //
        // Parent alive (`Some(fwd)`): forward back onto the parent's runtime
        // channel instead of synthesising SSE here. This preserves the FIFO
        // ordering with the parent's `tool_start (invoke_agent)`.
        // `forward_runtime_events` owns the SSE conversion for
        // `SubagentStarted` and emits on the same session stream. We return
        // `None` so the caller does NOT also `send_session_event` (no
        // double-delivery).
        //
        // The `background` flag is preserved verbatim on the reroute. It is
        // always `true` here — this is the bg event channel — so the
        // `forward_runtime_events` arm skips the #1125 (A1-1) durable
        // foreground `subagent_started` marker for background subagents, which
        // are already reload-safe via their persisted `{task_id, session_id}`
        // tool result.
        //
        // Parent dead (`None`): the reroute target is gone, so forwarding it
        // would silently drop the event (#1125 A1-3). Instead emit the
        // `subagent_started` `SseEventData` directly; the caller delivers it
        // via `send_session_event` on the parent's session stream. This does
        // NOT persist a foreground-style history marker (that is #1130's
        // foreground-only path) — background subagents stay reload-safe via
        // their tool result; this only recovers the live #1105 surface. The
        // `bg_session_id` here is the parent's session id, matching the
        // session the live SSE would have landed on.
        RuntimeEvent::SubagentStarted {
            tool_invocation_id,
            subagent_name,
            subagent_session_id,
            background,
        } => match parent_fwd {
            Some(fwd) => {
                fwd.forward_subagent_started(
                    tool_invocation_id,
                    subagent_name,
                    subagent_session_id.0,
                    background,
                );
                None
            }
            None => Some(SseEventData::subagent_started(
                bg_session_id,
                ToolInvocationId(tool_invocation_id),
                subagent_name,
                subagent_session_id,
            )),
        },
        // #1180: forward a background subagent's reasoning deltas onto the
        // parent's SESSION stream (tagged `source_agent`) so the SubagentBar's
        // live tail lights up during a background `invoke_agent` run; the
        // frontend tees them to the bar, away from the parent's own reasoning
        // view. NOT rerouted through the parent's `runtime_tx` like
        // `SubagentStarted`, because (a) these high-frequency deltas risk the
        // #1124 strong-sender deadlock and (b) a background subagent routinely
        // outlives the parent turn (runtime_tx is gone by then). `token_delta`
        // is intentionally NOT forwarded (no bar surface; would only bloat the
        // persisted session log).
        RuntimeEvent::ReasoningDelta { text, source_agent } => Some(SseEventData::reasoning_delta(
            bg_run_id,
            &text,
            source_agent,
        )),
        _ => None,
    }
}
