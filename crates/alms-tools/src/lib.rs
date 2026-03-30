//! ALMS Tool implementations and supporting traits.
//!
//! This crate contains:
//! - Tool implementations for agent-to-agent communication, subagent dispatch,
//!   session recall, and agent discovery.
//! - The `SubagentDispatcher` and `MessageSender` traits (implemented by
//!   alms-coordinator).
//! - The `EventForwarder` trait for type-erased event forwarding from subagents
//!   back to the gateway's SSE stream.
//!
//! **Does NOT depend on** `alms-runtime` or `alms-coordinator` (no cycles).
//! Tools are registered on an `AgentRuntime` by the gateway (`alms-gateway`).

pub mod event_forwarder;
pub mod get_task_result;
pub mod ignore_message;
pub mod invoke_agent;
pub mod list_agents;
pub mod list_my_sessions;
pub mod message_sender;
pub mod read_messages;
pub mod read_session;
pub mod read_subagent_session;
pub mod send_message;
pub mod subagent;

// Re-export tool structs for ergonomic imports.
pub use get_task_result::GetTaskResultTool;
pub use ignore_message::IgnoreMessageTool;
pub use invoke_agent::InvokeAgentTool;
pub use list_agents::ListAgentsTool;
pub use list_my_sessions::ListMySessionsTool;
pub use read_messages::ReadMessagesTool;
pub use read_session::ReadSessionTool;
pub use read_subagent_session::ReadSubagentSessionTool;
pub use send_message::SendMessageTool;

// Re-export traits and supporting types.
pub use event_forwarder::EventForwarder;
pub use message_sender::{ConversationEndReason, DeliveryReceipt, MessageSender, SendError};
pub use subagent::{PollResult, SubagentDispatcher};
