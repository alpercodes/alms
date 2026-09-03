// SPDX-License-Identifier: Apache-2.0

//! ALMS Agent Runtime
//!
//! The runtime crate handles:
//! - LLM client and API communication
//! - Tool registry and execution
//! - Agent loop orchestration
//!
//! Tool implementations (except the workspace tools) have been extracted to the
//! `alms-tools` crate. The `SubagentDispatcher`, `MessageSender`, and
//! `EventForwarder` traits also live in `alms-tools`.

pub mod agent;
pub(crate) mod anthropic;
pub mod context;
pub mod episodic;
pub mod events;
pub(crate) mod gemini;
pub(crate) mod gemini_cache;
pub mod llm_client;
pub mod llm_types;
pub mod scheduler;
pub mod summary_client;
pub mod tool_output_truncate;
pub mod tools;
pub mod workspace;
pub mod workspace_tool;

pub use agent::{AgentConfig, AgentRuntime, Posture, RunOutput};
pub use events::{RuntimeEvent, RuntimeEventSender};
pub use llm_client::LlmClient;
pub use llm_types::*;
pub use scheduler::Scheduler;
pub use summary_client::build_summary_client;
pub use workspace::{AgentWorkspace, WorkspaceFile};

// Re-export core types for convenience
pub use alms_core::{AlmsError, AlmsResult};

// Re-export the shell output spill helpers (issue #756) so downstream
// callers (gateway startup sweep, subagent plumbing) don't need a direct
// dependency on `alms-sandbox`.
pub use alms_sandbox::shell::spill;

// Re-export the boot-time shell-interpreter initializer (#1121) for the
// same reason: the gateway installs the `[tools].shell_path` override and
// logs the resolved shell at startup without a direct `alms-sandbox` edge.
pub use alms_sandbox::shell::init_shell_resolution;

// Re-export the tool-name-collision warning phrase (#1260). The warning
// is emitted while *this* crate's builder chain assembles a run's tool
// registry, and the gateway's log-asserting tests are what pin it, so it
// travels the same no-direct-`alms-sandbox`-edge route as the two above.
pub use alms_sandbox::TOOL_COLLISION_WARNING;
