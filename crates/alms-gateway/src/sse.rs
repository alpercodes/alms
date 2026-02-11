//! Server-Sent Events (SSE) streaming endpoint for ALMS
//!
//! Provides `/agent/run/stream` for real-time agent execution with event streaming.

use crate::server::AppState; // SessionManager is accessed via state.session_manager
use alms_core::{AgentId, SessionId};
use axum::{
    extract::State,
    http::StatusCode,
    response::sse::{Event, Sse},
    Json,
};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Unique identifier for a run
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunId(pub Uuid);

impl RunId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for RunId {
    fn default() -> Self {
        Self::new()
    }
}

/// Unique identifier for a tool invocation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInvocationId(pub Uuid);

impl ToolInvocationId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ToolInvocationId {
    fn default() -> Self {
        Self::new()
    }
}

/// Request to start a streaming agent run
#[derive(Debug, Deserialize)]
pub struct StreamRunRequest {
    pub context_id: String,
    pub message: String,
    pub agent_id: Option<AgentId>,
    pub session_id: Option<SessionId>,
}

/// SSE event types for streaming agent execution
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SseEventType {
    /// Initial connection established
    Connected {
        run_id: RunId,
        session_id: SessionId,
    },
    /// Run started
    RunStarted {
        run_id: RunId,
        session_id: SessionId,
        message: String,
    },
    /// Token delta from LLM streaming
    TokenDelta {
        run_id: RunId,
        session_id: SessionId,
        delta: String,
    },
    /// Tool execution starting
    ToolStart {
        run_id: RunId,
        session_id: SessionId,
        tool_invocation_id: ToolInvocationId,
        tool_name: String,
        params: serde_json::Value,
    },
    /// Tool execution completed
    ToolEnd {
        run_id: RunId,
        session_id: SessionId,
        tool_invocation_id: ToolInvocationId,
        result: serde_json::Value,
        duration_ms: u64,
    },
    /// Tool execution failed
    ToolError {
        run_id: RunId,
        session_id: SessionId,
        tool_invocation_id: ToolInvocationId,
        error: String,
    },
    /// Subagent spawned
    SubagentStart {
        run_id: RunId,
        session_id: SessionId,
        subagent_run_id: RunId,
        subagent_type: String,
        task: String,
    },
    /// Subagent progress
    SubagentProgress {
        run_id: RunId,
        session_id: SessionId,
        subagent_run_id: RunId,
        progress_percent: u8,
        message: String,
    },
    /// Subagent completed
    SubagentEnd {
        run_id: RunId,
        session_id: SessionId,
        subagent_run_id: RunId,
        result: serde_json::Value,
    },
    /// Subagent failed
    SubagentError {
        run_id: RunId,
        session_id: SessionId,
        subagent_run_id: RunId,
        error: String,
    },
    /// Run completed successfully
    RunComplete {
        run_id: RunId,
        session_id: SessionId,
        final_response: String,
    },
    /// Run failed
    RunError {
        run_id: RunId,
        session_id: SessionId,
        error: String,
    },
    /// Keep-alive ping
    Ping,
}

/// Event data with correlation IDs
#[derive(Debug, Clone, Serialize)]
pub struct SseEventData {
    #[serde(flatten)]
    pub event: SseEventType,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Channel for streaming events
pub type EventSender = mpsc::Sender<SseEventData>;
pub type EventReceiver = mpsc::Receiver<SseEventData>;

/// Create a new event channel
pub fn event_channel(buffer: usize) -> (EventSender, EventReceiver) {
    mpsc::channel(buffer)
}

/// SSE streaming handler for agent runs
pub async fn stream_run(
    State(state): State<AppState>,
    Json(req): Json<StreamRunRequest>,
) -> Result<Sse<ReceiverStream<Result<Event, Infallible>>>, (StatusCode, String)> {
    let run_id = RunId::new();
    
    // Get or create session
    let session = if let Some(ref sid) = req.session_id {
        state.session_manager.get(sid.clone())
            .map_err(|_| (StatusCode::NOT_FOUND, "Session not found".to_string()))?
    } else {
        state.session_manager.get_or_create(
            req.agent_id.unwrap_or_else(AgentId::new),
            req.context_id.clone(),
        )
    };
    
    let session_id = session.id.clone();
    info!("Starting streaming run {} for session {}", run_id.0, session_id.0);
    
    // Create event channel
    let (tx, rx) = event_channel(128);
    let stream = ReceiverStream::new(rx).map(|event: SseEventData| {
        let id = Uuid::new_v4().to_string();
        let event = Event::default()
            .id(id)
            .event(event.event.type_name())
            .json_data(&event)
            .unwrap_or_else(|_| Event::default().data("{}"));
        Ok(event)
    });
    
    // Spawn the agent run in background
    let gateway = state.gateway.clone();
    let run_id_clone = run_id;
    let session_id_clone = session_id.clone();
    let message = req.message;
    
    tokio::spawn(async move {
        if let Err(e) = run_streaming_agent(
            gateway,
            run_id_clone,
            session_id_clone,
            message,
            tx,
        ).await {
            error!("Streaming agent run failed: {}", e);
        }
    });
    
    // Return SSE stream
    let sse = Sse::new(stream)
        .keep_alive(
            axum::response::sse::KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("ping"),
        );
    
    Ok(sse)
}

/// Run the streaming agent
async fn run_streaming_agent(
    gateway: std::sync::Arc<tokio::sync::Mutex<crate::gateway::Gateway>>,
    run_id: RunId,
    session_id: SessionId,
    message: String,
    tx: EventSender,
) -> crate::AlmsResult<()> {
    // Send connected event
    send_event(&tx, SseEventType::Connected {
        run_id,
        session_id: session_id.clone(),
    }).await?;
    
    // Send run started
    send_event(&tx, SseEventType::RunStarted {
        run_id,
        session_id: session_id.clone(),
        message: message.clone(),
    }).await?;
    
    // Get gateway and create runtime
    let gateway_guard = gateway.lock().await;
    let runtime = alms_runtime::AgentRuntime::new(
        gateway_guard.agent_id().clone(),
        gateway_guard.agent_config().clone(),
        gateway_guard.llm().clone(),
    );
    drop(gateway_guard); // Release lock
    
    // For MVP: simulate streaming with the existing runtime
    // In a full implementation, we would emit token deltas and tool events during the run
    match runtime.run(
        &gateway.lock().await.session_manager().clone(),
        &session_id.0.to_string(),
        &message,
    ).await {
        Ok(response) => {
            // For now, emit the full response as a single token delta
            // In production, this would be streamed from the LLM
            send_event(&tx, SseEventType::TokenDelta {
                run_id,
                session_id: session_id.clone(),
                delta: response.clone(),
            }).await?;
            
            // Mark complete
            send_event(&tx, SseEventType::RunComplete {
                run_id,
                session_id: session_id.clone(),
                final_response: response,
            }).await?;
        }
        Err(e) => {
            send_event(&tx, SseEventType::RunError {
                run_id,
                session_id: session_id.clone(),
                error: e.to_string(),
            }).await?;
        }
    }
    
    Ok(())
}

/// Send an event to the stream
async fn send_event(
    tx: &EventSender,
    event: SseEventType,
) -> crate::AlmsResult<()> {
    let data = SseEventData {
        event,
        timestamp: chrono::Utc::now(),
    };
    
    tx.send(data).await
        .map_err(|_| alms_core::AlmsError::Internal("Event channel closed".to_string()))?;
    
    Ok(())
}

impl SseEventType {
    /// Get the event type name for SSE event field
    fn type_name(&self) -> &'static str {
        match self {
            SseEventType::Connected { .. } => "connected",
            SseEventType::RunStarted { .. } => "run_started",
            SseEventType::TokenDelta { .. } => "token_delta",
            SseEventType::ToolStart { .. } => "tool_start",
            SseEventType::ToolEnd { .. } => "tool_end",
            SseEventType::ToolError { .. } => "tool_error",
            SseEventType::SubagentStart { .. } => "subagent_start",
            SseEventType::SubagentProgress { .. } => "subagent_progress",
            SseEventType::SubagentEnd { .. } => "subagent_end",
            SseEventType::SubagentError { .. } => "subagent_error",
            SseEventType::RunComplete { .. } => "run_complete",
            SseEventType::RunError { .. } => "run_error",
            SseEventType::Ping => "ping",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_event_type_names() {
        assert_eq!(SseEventType::Ping.type_name(), "ping");
        
        let connected = SseEventType::Connected {
            run_id: RunId::new(),
            session_id: SessionId::new(),
        };
        assert_eq!(connected.type_name(), "connected");
    }
    
    #[tokio::test]
    async fn test_event_channel() {
        let (tx, mut rx) = event_channel(10);
        
        let event = SseEventData {
            event: SseEventType::Ping,
            timestamp: chrono::Utc::now(),
        };
        
        tx.send(event.clone()).await.unwrap();
        
        let received = rx.recv().await.unwrap();
        assert!(matches!(received.event, SseEventType::Ping));
    }
}