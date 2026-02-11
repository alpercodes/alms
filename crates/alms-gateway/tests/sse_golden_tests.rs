//! Golden tests for SSE event sequencing
//!
//! Tests that verify event ordering and completeness without requiring a real LLM.

use alms_gateway::sse::{RunId, SessionId, SseEventType, ToolInvocationId, event_channel};
use serde_json::json;

/// Mock LLM that produces deterministic responses for testing
struct MockLlmClient;

impl MockLlmClient {
    fn stream_response(&self, input: &str) -> Vec<String> {
        // Deterministic token chunks based on input
        vec![
            format!("Mock response to: {}", input),
            " (completed)".to_string(),
        ]
    }
}

#[tokio::test]
async fn test_event_sequence_basic_run() {
    // Test that a basic run produces the expected event sequence
    let (tx, mut rx) = event_channel(128);
    let run_id = RunId::new();
    let session_id = SessionId::new();
    
    // Simulate a basic run
    tx.send(alms_gateway::sse::SseEventData {
        event: SseEventType::Connected { run_id, session_id },
        timestamp: chrono::Utc::now(),
    }).await.unwrap();
    
    tx.send(alms_gateway::sse::SseEventData {
        event: SseEventType::RunStarted {
            run_id,
            session_id,
            message: "Hello".to_string(),
        },
        timestamp: chrono::Utc::now(),
    }).await.unwrap();
    
    tx.send(alms_gateway::sse::SseEventData {
        event: SseEventType::TokenDelta {
            run_id,
            session_id,
            delta: "Mock response".to_string(),
        },
        timestamp: chrono::Utc::now(),
    }).await.unwrap();
    
    tx.send(alms_gateway::sse::SseEventData {
        event: SseEventType::RunComplete {
            run_id,
            session_id,
            final_response: "Mock response".to_string(),
        },
        timestamp: chrono::Utc::now(),
    }).await.unwrap();
    
    // Collect events
    let mut events = vec![];
    while let Some(event) = rx.recv().await {
        events.push(event.event);
    }
    
    // Verify sequence
    assert_eq!(events.len(), 4);
    assert!(matches!(events[0], SseEventType::Connected { .. }));
    assert!(matches!(events[1], SseEventType::RunStarted { .. }));
    assert!(matches!(events[2], SseEventType::TokenDelta { .. }));
    assert!(matches!(events[3], SseEventType::RunComplete { .. }));
}

#[tokio::test]
async fn test_event_sequence_with_tool() {
    // Test that tool invocations are properly ordered
    let (tx, mut rx) = event_channel(128);
    let run_id = RunId::new();
    let session_id = SessionId::new();
    let tool_id = ToolInvocationId::new();
    
    // Start run
    tx.send(alms_gateway::sse::SseEventData {
        event: SseEventType::RunStarted {
            run_id,
            session_id,
            message: "Run a command".to_string(),
        },
        timestamp: chrono::Utc::now(),
    }).await.unwrap();
    
    // Tool starts
    tx.send(alms_gateway::sse::SseEventData {
        event: SseEventType::ToolStart {
            run_id,
            session_id,
            tool_invocation_id: tool_id,
            tool_name: "shell_exec".to_string(),
            params: json!({"command": "echo hello"}),
        },
        timestamp: chrono::Utc::now(),
    }).await.unwrap();
    
    // Tool ends
    tx.send(alms_gateway::sse::SseEventData {
        event: SseEventType::ToolEnd {
            run_id,
            session_id,
            tool_invocation_id: tool_id,
            result: json!({"output": "hello"}),
            duration_ms: 50,
        },
        timestamp: chrono::Utc::now(),
    }).await.unwrap();
    
    // Complete
    tx.send(alms_gateway::sse::SseEventData {
        event: SseEventType::RunComplete {
            run_id,
            session_id,
            final_response: "Done".to_string(),
        },
        timestamp: chrono::Utc::now(),
    }).await.unwrap();
    
    // Collect events
    let mut events = vec![];
    while let Some(event) = rx.recv().await {
        events.push(event.event);
    }
    
    // Verify tool sandwich pattern: start -> end, within run
    assert!(matches!(events[0], SseEventType::RunStarted { .. }));
    assert!(matches!(events[1], SseEventType::ToolStart { .. }));
    assert!(matches!(events[2], SseEventType::ToolEnd { .. }));
    assert!(matches!(events[3], SseEventType::RunComplete { .. }));
}

#[tokio::test]
async fn test_event_sequence_with_subagent() {
    // Test subagent lifecycle events
    let (tx, mut rx) = event_channel(128);
    let run_id = RunId::new();
    let session_id = SessionId::new();
    let subagent_run_id = RunId::new();
    
    // Start
    tx.send(alms_gateway::sse::SseEventData {
        event: SseEventType::RunStarted {
            run_id,
            session_id,
            message: "Delegate task".to_string(),
        },
        timestamp: chrono::Utc::now(),
    }).await.unwrap();
    
    // Subagent starts
    tx.send(alms_gateway::sse::SseEventData {
        event: SseEventType::SubagentStart {
            run_id,
            session_id,
            subagent_run_id,
            subagent_type: "Research".to_string(),
            task: "Research topic".to_string(),
        },
        timestamp: chrono::Utc::now(),
    }).await.unwrap();
    
    // Progress
    tx.send(alms_gateway::sse::SseEventData {
        event: SseEventType::SubagentProgress {
            run_id,
            session_id,
            subagent_run_id,
            progress_percent: 50,
            message: "Working...".to_string(),
        },
        timestamp: chrono::Utc::now(),
    }).await.unwrap();
    
    // Subagent ends
    tx.send(alms_gateway::sse::SseEventData {
        event: SseEventType::SubagentEnd {
            run_id,
            session_id,
            subagent_run_id,
            result: json!({"findings": "Some results"}),
        },
        timestamp: chrono::Utc::now(),
    }).await.unwrap();
    
    // Run complete
    tx.send(alms_gateway::sse::SseEventData {
        event: SseEventType::RunComplete {
            run_id,
            session_id,
            final_response: "Task completed".to_string(),
        },
        timestamp: chrono::Utc::now(),
    }).await.unwrap();
    
    let mut events = vec![];
    while let Some(event) = rx.recv().await {
        events.push(event.event);
    }
    
    // Verify subagent lifecycle: start -> progress -> end
    assert!(matches!(events[1], SseEventType::SubagentStart { .. }));
    assert!(matches!(events[2], SseEventType::SubagentProgress { .. }));
    assert!(matches!(events[3], SseEventType::SubagentEnd { .. }));
}

#[tokio::test]
async fn test_tool_error_handling() {
    // Test that tool errors are properly sequenced
    let (tx, mut rx) = event_channel(128);
    let run_id = RunId::new();
    let session_id = SessionId::new();
    let tool_id = ToolInvocationId::new();
    
    tx.send(alms_gateway::sse::SseEventData {
        event: SseEventType::RunStarted {
            run_id,
            session_id,
            message: "Run failing command".to_string(),
        },
        timestamp: chrono::Utc::now(),
    }).await.unwrap();
    
    tx.send(alms_gateway::sse::SseEventData {
        event: SseEventType::ToolStart {
            run_id,
            session_id,
            tool_invocation_id: tool_id,
            tool_name: "shell_exec".to_string(),
            params: json!({"command": "invalid_command"}),
        },
        timestamp: chrono::Utc::now(),
    }).await.unwrap();
    
    tx.send(alms_gateway::sse::SseEventData {
        event: SseEventType::ToolError {
            run_id,
            session_id,
            tool_invocation_id: tool_id,
            error: "Command not found".to_string(),
        },
        timestamp: chrono::Utc::now(),
    }).await.unwrap();
    
    // Run should still complete even if tool failed
    tx.send(alms_gateway::sse::SseEventData {
        event: SseEventType::RunComplete {
            run_id,
            session_id,
            final_response: "Handled error".to_string(),
        },
        timestamp: chrono::Utc::now(),
    }).await.unwrap();
    
    let mut events = vec![];
    while let Some(event) = rx.recv().await {
        events.push(event.event);
    }
    
    // Error should be before run complete
    assert!(matches!(events[2], SseEventType::ToolError { .. }));
    assert!(matches!(events[3], SseEventType::RunComplete { .. }));
}

#[tokio::test]
async fn test_correlation_ids_preserved() {
    // Test that correlation IDs flow through events
    let (tx, mut rx) = event_channel(128);
    let run_id = RunId::new();
    let session_id = SessionId::new();
    let tool_id = ToolInvocationId::new();
    
    tx.send(alms_gateway::sse::SseEventData {
        event: SseEventType::ToolStart {
            run_id,
            session_id,
            tool_invocation_id: tool_id,
            tool_name: "test".to_string(),
            params: json!({}),
        },
        timestamp: chrono::Utc::now(),
    }).await.unwrap();
    
    if let Some(event) = rx.recv().await {
        match event.event {
            SseEventType::ToolStart { 
                run_id: r, 
                session_id: s, 
                tool_invocation_id: t,
                ..
            } => {
                assert_eq!(r.0, run_id.0);
                assert_eq!(s.0, session_id.0);
                assert_eq!(t.0, tool_id.0);
            }
            _ => panic!("Expected ToolStart event"),
        }
    }
}