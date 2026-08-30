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

pub mod dm_filter;
pub mod event_forwarder;
pub mod ignore_message;
pub mod invoke_agent;
pub mod list_agents;
pub mod list_my_sessions;
pub mod message_sender;
pub mod read_messages;
pub mod read_session;
pub mod read_subagent_session;
pub mod send_message;
pub mod session_read;
pub mod subagent;
pub mod subagent_self_sink;

// Re-export tool structs for ergonomic imports.
pub use ignore_message::IgnoreMessageTool;
pub use invoke_agent::InvokeAgentTool;
pub use list_agents::ListAgentsTool;
pub use list_my_sessions::ListMySessionsTool;
pub use read_messages::ReadMessagesTool;
pub use read_session::ReadSessionTool;
pub use read_subagent_session::ReadSubagentSessionTool;
pub use send_message::SendMessageTool;

// Re-export traits and supporting types.
// `Tool` comes from alms-sandbox and is re-exported so a caller holding one
// of this crate's tools can actually invoke it. This is necessary, not just
// convenient: alms-gateway registers every tool in this crate but has NO
// direct alms-sandbox dependency (its Cargo.toml lists core, session,
// runtime, tools, coordinator, channel), so without this line a gateway test
// cannot bring `Tool` into scope to call `execute()` on a tool it built —
// which is how the #1299 send_message fold is pinned. Re-export only; the
// dependency graph in CLAUDE.md is unchanged.
//
// `SandboxResult` / `SandboxError` ride along for the same reason one step
// further: a gateway test that *implements* `Tool` (rather than calling one)
// has to name the error type in `execute`'s signature. #1260's
// collision-warning complement is the first such test.
pub use alms_sandbox::{SandboxError, SandboxResult, Tool};
pub use event_forwarder::{EventForwarder, SubagentRunOutcome, subagent_activity_kind};
pub use message_sender::{ConversationEndReason, DeliveryReceipt, MessageSender, SendError};
pub use subagent::SubagentDispatcher;
pub use subagent_self_sink::SubagentSelfEventSink;
