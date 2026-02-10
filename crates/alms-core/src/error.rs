use thiserror::Error;

#[derive(Error, Debug)]
pub enum AlmsError {
    #[error("Session not found: {0}")]
    SessionNotFound(String),
    
    #[error("Agent not found: {0}")]
    AgentNotFound(String),
    
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),
    
    #[error("Tool execution failed: {0}")]
    ToolExecution(String),
    
    #[error("Channel error: {0}")]
    Channel(String),
    
    #[error("Runtime error: {0}")]
    Runtime(String),
    
    #[error("Sandbox error: {0}")]
    Sandbox(String),
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

pub type AlmsResult<T> = Result<T, AlmsError>;