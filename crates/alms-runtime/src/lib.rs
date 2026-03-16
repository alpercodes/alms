//! ALMS Agent Runtime
//!
//! The runtime crate handles:
//! - LLM client and API communication
//! - Tool registry and execution
//! - Agent loop orchestration

pub mod agent;
pub(crate) mod anthropic;
pub mod context;
pub mod events;
pub mod get_task_result_tool;
pub mod invoke_agent_tool;
pub mod llm_client;
pub mod llm_types;
pub mod read_subagent_session_tool;
pub mod scheduler;
pub mod subagent;
pub mod tools;
pub mod workspace;
pub mod workspace_tool;

pub use agent::{AgentConfig, AgentRuntime, Posture, RunOutput};
pub use context::ContextBuilder;
pub use events::{RuntimeEvent, RuntimeEventSender};
pub use get_task_result_tool::GetTaskResultTool;
pub use invoke_agent_tool::InvokeAgentTool;
pub use llm_client::LlmClient;
pub use llm_types::*;
pub use read_subagent_session_tool::ReadSubagentSessionTool;
pub use scheduler::{JobRun, Scheduler};
pub use subagent::SubagentDispatcher;
pub use tools::ToolRegistry;
pub use workspace::{AgentWorkspace, WorkspaceFile};

use alms_core::AgentId;

/// Agent runtime manager
#[derive(Debug)]
pub struct RuntimeManager {
    llm: LlmClient,
}

impl RuntimeManager {
    /// Create new runtime manager
    pub fn new(llm: LlmClient) -> Self {
        Self { llm }
    }

    /// Create from environment
    pub fn from_env() -> crate::AlmsResult<Self> {
        let llm = LlmClient::from_env()?;
        Ok(Self::new(llm))
    }

    /// Create a new agent runtime
    pub fn create_runtime(&self, agent_id: AgentId, config: AgentConfig) -> AlmsResult<AgentRuntime> {
        AgentRuntime::new(agent_id, config, self.llm.clone())
    }

    /// Create runtime with default config
    pub fn create_runtime_default(&self, agent_id: AgentId) -> AlmsResult<AgentRuntime> {
        AgentRuntime::new(agent_id, AgentConfig::default(), self.llm.clone())
    }
}

// Re-export core types for convenience
pub use alms_core::{AlmsError, AlmsResult};
