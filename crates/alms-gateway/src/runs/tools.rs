//! Runtime event forwarding — bridges `alms-tools` events to `RuntimeEvent`.

use crate::approvals::{ApprovalStore, PendingApproval};
use crate::sse::{SseEventData, ToolInvocationId};
use alms_core::{RunId, SessionId};
use alms_runtime::RuntimeEvent;
use chrono::Utc;
use tokio::sync::mpsc;

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
        }
    }
}
