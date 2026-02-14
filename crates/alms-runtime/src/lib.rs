//! ALMS Agent Runtime
//! 
//! The runtime crate handles:
//! - LLM client and API communication
//! - Tool registry and execution
//! - Agent loop orchestration

pub mod agent;
pub mod context;
pub mod llm_client;
pub mod llm_types;
pub mod tools;
pub mod workspace;

pub use agent::{AgentConfig, AgentRuntime};
pub use context::ContextBuilder;
pub use llm_client::LlmClient;
pub use llm_types::*;
pub use tools::ToolRegistry;

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
    pub fn create_runtime(&self, agent_id: AgentId, config: AgentConfig) -> AgentRuntime {
        AgentRuntime::new(agent_id, config, self.llm.clone())
    }
    
    /// Create runtime with default config
    pub fn create_runtime_default(&self, agent_id: AgentId) -> AgentRuntime {
        AgentRuntime::new(agent_id, AgentConfig::default(), self.llm.clone())
    }
}

// Re-export core types for convenience
pub use alms_core::{AlmsError, AlmsResult};
