//! Core agent types and traits
//!
//! This module defines the fundamental types for agents in ALMS.

use serde::{Deserialize, Serialize};

/// Agent capability - what an agent can do
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Capability {
    /// Can execute shell commands
    Shell,
    /// Can read files
    Read,
    /// Can write files
    Write,
    /// Can make HTTP requests
    Http,
    /// Can search the web
    Search,
    /// Can execute code
    CodeExecution,
    /// Custom capability
    Custom(String),
}

/// Agent role in the system
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentRole {
    /// Main coordinator agent
    Main,
    /// Subagent for specific tasks
    Subagent,
    /// Tool execution agent
    Tool,
}

/// Configuration for an agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Agent role
    pub role: AgentRole,
    /// Capabilities this agent has
    pub capabilities: Vec<Capability>,
    /// Maximum concurrent tasks
    pub max_concurrent: usize,
    /// Timeout for task execution (seconds)
    pub timeout_secs: u64,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            role: AgentRole::Subagent,
            capabilities: vec![Capability::Read, Capability::Http],
            max_concurrent: 4,
            timeout_secs: 300,
        }
    }
}

impl AgentConfig {
    /// Create config for a main agent
    pub fn main_agent() -> Self {
        Self {
            role: AgentRole::Main,
            capabilities: vec![
                Capability::Read,
                Capability::Write,
                Capability::Http,
                Capability::Search,
            ],
            max_concurrent: 8,
            timeout_secs: 600,
        }
    }
    
    /// Create config for a code subagent
    pub fn code_subagent() -> Self {
        Self {
            role: AgentRole::Subagent,
            capabilities: vec![
                Capability::Read,
                Capability::Write,
                Capability::CodeExecution,
            ],
            max_concurrent: 2,
            timeout_secs: 600,
        }
    }
    
    /// Create config for a research subagent
    pub fn research_subagent() -> Self {
        Self {
            role: AgentRole::Subagent,
            capabilities: vec![
                Capability::Http,
                Capability::Search,
            ],
            max_concurrent: 4,
            timeout_secs: 300,
        }
    }
}