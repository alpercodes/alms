//! ALMS Gateway - HTTP/WebSocket API and message routing
//!
//! The gateway provides:
//! - HTTP REST API for session management
//! - WebSocket endpoint for real-time communication  
//! - Integration between channels (Telegram) and agent runtimes
//!
//! ## Usage
//!
//! ```rust,no_run
//! use alms_gateway::Gateway;
//! use alms_gateway::gateway::GatewayConfig;
//!
//! #[tokio::main]
//! async fn main() {
//!     let mut gateway = Gateway::new(GatewayConfig::from_env().unwrap()).unwrap();
//!     gateway.initialize_channels().await.unwrap();
//!     gateway.start().await.unwrap();
//!     gateway.run().await.unwrap();
//! }
//! ```

pub mod agents;
pub mod approvals;
pub mod auth;
pub mod cron_utils;
pub mod event_log;
pub mod gateway;
pub mod jobs;
pub mod runs;
pub mod server;
pub mod settings;
pub mod sse;
pub mod tasks;
pub mod workspace;

// Re-export main types
pub use gateway::{Gateway, GatewayConfig};
pub use runs::{create_run, get_run_status};
pub use server::{AppState, RunManager, serve, serve_with_gateway};
pub use sse::{RunEventStream, SseEventData, event_channel};

use alms_core::AlmsResult;

/// Run the complete ALMS system with all channels
pub async fn run() -> AlmsResult<()> {
    let mut gateway = Gateway::from_env()?;

    gateway.initialize_channels().await?;
    gateway.start().await?;
    gateway.run().await?;

    Ok(())
}
