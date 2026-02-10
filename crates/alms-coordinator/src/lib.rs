use alms_core::{AgentId, AlmsResult, SessionId};

pub mod main_agent;

pub use main_agent::MainAgent;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Unique identifier for a subagent task
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub Uuid);

impl TaskId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

/// Types of specialized subagents
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubagentType {
    /// Research and information gathering
    Research,
    /// Code generation and review
    Code,
    /// Data processing and analysis
    Data,
    /// External API integrations
    Integration,
    /// Security analysis
    Security,
    /// General purpose (default)
    General,
}

impl SubagentType {
    pub fn default_capabilities(&self) -> Vec<String> {
        match self {
            SubagentType::Research => vec![
                "web_search".to_string(),
                "read_docs".to_string(),
                "summarize".to_string(),
            ],
            SubagentType::Code => vec![
                "code_gen".to_string(),
                "lint".to_string(),
                "test_run".to_string(),
                "read".to_string(),
                "write".to_string(),
            ],
            SubagentType::Data => vec![
                "query".to_string(),
                "transform".to_string(),
                "visualize".to_string(),
            ],
            SubagentType::Integration => vec![
                "http_request".to_string(),
                "webhook".to_string(),
                "notify".to_string(),
            ],
            SubagentType::Security => vec![
                "scan".to_string(),
                "audit".to_string(),
                "report".to_string(),
            ],
            SubagentType::General => vec!["*".to_string()],
        }
    }
}

/// Request to spawn a subagent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentRequest {
    pub task: String,
    pub agent_type: SubagentType,
    pub timeout: Duration,
    pub capabilities: Vec<String>,
    pub parent_session: SessionId,
}

/// Status of a subagent task
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Progress update from a subagent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressUpdate {
    pub task_id: TaskId,
    pub status: TaskStatus,
    pub progress_percent: u8,
    pub message: String,
    pub partial_result: Option<serde_json::Value>,
}

/// Final result from a subagent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub task_id: TaskId,
    pub status: TaskStatus,
    pub result: serde_json::Value,
    pub execution_time_ms: u64,
    pub tokens_used: Option<usize>,
}

/// Message types for inter-agent communication
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentMessage {
    /// Spawn a new subagent
    SpawnSubagent(SubagentRequest, oneshot::Sender<TaskId>),
    /// Progress update from subagent
    Progress(ProgressUpdate),
    /// Task completed
    Complete(TaskResult),
    /// Cancel a running task
    Cancel(TaskId),
    /// Request subagent to do something
    Request(TaskId, String),
    /// Response from subagent
    Response(TaskId, serde_json::Value),
}

/// Handle to a running subagent
#[derive(Debug)]
pub struct SubagentHandle {
    pub task_id: TaskId,
    pub agent_type: SubagentType,
    pub status: TaskStatus,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub cancel_tx: oneshot::Sender<()>,
}

/// Coordinator manages all agents and message routing
#[derive(Debug)]
pub struct Coordinator {
    /// Main agent ID
    main_agent: AgentId,
    /// Active subagents: TaskId -> SubagentHandle
    subagents: Arc<DashMap<TaskId, SubagentHandle>>,
    /// Message bus: crossbeam channel for agent communication
    message_tx: mpsc::UnboundedSender<AgentMessage>,
    message_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<AgentMessage>>>,
}

impl Coordinator {
    pub fn new(main_agent: AgentId) -> Self {
        let (message_tx, message_rx) = mpsc::unbounded_channel();
        
        Self {
            main_agent,
            subagents: Arc::new(DashMap::new()),
            message_tx,
            message_rx: Arc::new(tokio::sync::Mutex::new(message_rx)),
        }
    }
    
    /// Spawn a new subagent for a task
    pub async fn spawn_subagent(
        &self,
        request: SubagentRequest,
    ) -> AlmsResult<TaskId> {
        let task_id = TaskId::new();
        let agent_type = request.agent_type.clone();
        let (cancel_tx, cancel_rx) = oneshot::channel();
        
        let handle = SubagentHandle {
            task_id,
            agent_type,
            status: TaskStatus::Pending,
            started_at: chrono::Utc::now(),
            cancel_tx,
        };
        
        self.subagents.insert(task_id, handle);
        info!("Spawned subagent {:?} for task {:?}", agent_type, task_id);
        
        // Start subagent execution in background
        let subagents = self.subagents.clone();
        let message_tx = self.message_tx.clone();
        
        tokio::spawn(async move {
            run_subagent(task_id, request, subagents, message_tx, cancel_rx).await;
        });
        
        Ok(task_id)
    }
    
    /// Cancel a running subagent
    pub fn cancel_subagent(&self, task_id: TaskId) -> AlmsResult<()> {
        if let Some((_, handle)) = self.subagents.remove(&task_id) {
            let _ = handle.cancel_tx.send(());
            info!("Cancelled subagent {:?}", task_id);
            Ok(())
        } else {
            Err(alms_core::AlmsError::AgentNotFound(task_id.0.to_string()))
        }
    }
    
    /// Get status of a subagent
    pub fn get_status(&self, task_id: TaskId) -> Option<TaskStatus> {
        self.subagents.get(&task_id).map(|h| h.status)
    }
    
    /// List all active subagents
    pub fn list_active(&self) -> Vec<(TaskId, SubagentType, TaskStatus)> {
        self.subagents
            .iter()
            .map(|e| (e.key().clone(), e.value().agent_type.clone(), e.value().status))
            .collect()
    }
    
    /// Get message sender (for agents to send messages)
    pub fn message_sender(&self) -> mpsc::UnboundedSender<AgentMessage> {
        self.message_tx.clone()
    }
    
    /// Process incoming messages (call this in a loop)
    pub async fn process_messages(&self) -> Option<AgentMessage> {
        let mut rx = self.message_rx.lock().await;
        rx.recv().await
    }
}

/// Run a subagent task
async fn run_subagent(
    task_id: TaskId,
    request: SubagentRequest,
    subagents: Arc<DashMap<TaskId, SubagentHandle>>,
    message_tx: mpsc::UnboundedSender<AgentMessage>,
    mut cancel_rx: oneshot::Receiver<()>,
) {
    // Update status to running
    if let Some(mut handle) = subagents.get_mut(&task_id) {
        handle.status = TaskStatus::Running;
    }
    
    // Send initial progress
    let _ = message_tx.send(AgentMessage::Progress(ProgressUpdate {
        task_id,
        status: TaskStatus::Running,
        progress_percent: 0,
        message: format!("Starting {:?} subagent for: {}", request.agent_type, request.task),
        partial_result: None,
    }));
    
    // Simulate subagent work (replace with actual implementation)
    let start = std::time::Instant::now();
    
    tokio::select! {
        _ = tokio::time::sleep(request.timeout) => {
            // Timeout
            warn!("Subagent {:?} timed out", task_id);
            
            let _ = message_tx.send(AgentMessage::Complete(TaskResult {
                task_id,
                status: TaskStatus::Failed,
                result: serde_json::json!({"error": "Timeout"}),
                execution_time_ms: start.elapsed().as_millis() as u64,
                tokens_used: None,
            }));
            
            if let Some(mut handle) = subagents.get_mut(&task_id) {
                handle.status = TaskStatus::Failed;
            }
        }
        _ = &mut cancel_rx => {
            // Cancelled
            info!("Subagent {:?} cancelled", task_id);
            
            let _ = message_tx.send(AgentMessage::Complete(TaskResult {
                task_id,
                status: TaskStatus::Cancelled,
                result: serde_json::json!({"cancelled": true}),
                execution_time_ms: start.elapsed().as_millis() as u64,
                tokens_used: None,
            }));
            
            if let Some(mut handle) = subagents.get_mut(&task_id) {
                handle.status = TaskStatus::Cancelled;
            }
        }
        _ = execute_task(task_id, &request, &message_tx) => {
            // Completed normally
            info!("Subagent {:?} completed", task_id);
            
            if let Some(mut handle) = subagents.get_mut(&task_id) {
                handle.status = TaskStatus::Completed;
            }
        }
    }
    
    // Cleanup after delay
    tokio::time::sleep(Duration::from_secs(60)).await;
    subagents.remove(&task_id);
    debug!("Cleaned up subagent {:?}", task_id);
}

/// Execute the actual task (placeholder for real implementation)
async fn execute_task(
    task_id: TaskId,
    request: &SubagentRequest,
    message_tx: &mpsc::UnboundedSender<AgentMessage>,
) {
    // Simulate work with progress updates
    for i in 1..=5 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        
        let _ = message_tx.send(AgentMessage::Progress(ProgressUpdate {
            task_id,
            status: TaskStatus::Running,
            progress_percent: i * 20,
            message: format!("Working... {}%", i * 20),
            partial_result: None,
        }));
    }
    
    // Send completion
    let _ = message_tx.send(AgentMessage::Complete(TaskResult {
        task_id,
        status: TaskStatus::Completed,
        result: serde_json::json!({
            "task": request.task,
            "agent_type": format!("{:?}", request.agent_type),
            "capabilities": request.capabilities,
        }),
        execution_time_ms: 500,
        tokens_used: Some(1000),
    }));
}