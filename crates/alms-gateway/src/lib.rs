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
//!     let mut gateway = Gateway::new(GatewayConfig::from_env()).unwrap();
//!     gateway.initialize_channels().await.unwrap();
//!     gateway.start().await.unwrap();
//!     gateway.run().await.unwrap();
//! }
//! ```

pub mod gateway;
pub mod server;

// Re-export main types
pub use gateway::{Gateway, GatewayConfig};
pub use server::{serve, serve_with_gateway};

use alms_core::AlmsResult;

/// Run the complete ALMS system with all channels
pub async fn run() -> AlmsResult<()> {
    let mut gateway = Gateway::from_env()?;
    
    gateway.initialize_channels().await?;
    gateway.start().await?;
    gateway.run().await?;
    
    Ok(())
}
