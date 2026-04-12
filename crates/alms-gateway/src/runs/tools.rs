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

/// Reads RuntimeEvents from the runtime and forwards them as SSE events.
/// Also stores ApprovalRequired events in the approval store so clients can resolve them.
pub(super) async fn forward_runtime_events(
    mut rx: mpsc::UnboundedReceiver<RuntimeEvent>,
    run_id: RunId,
    session_id: SessionId,
    run_manager: crate::server::RunManager,
    approval_store: ApprovalStore,
) {
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
                        SseEventData::run_warning(run_id, &code, &message, source_agent),
                    )
                    .await;
            }
            RuntimeEvent::ContextDebug {
                messages,
                tool_names,
                total_tokens,
                system_tokens,
                history_message_count,
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
                        ),
                    )
                    .await;
            }
        }
    }
}
