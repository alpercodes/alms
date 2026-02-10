//! Channel adapters for messaging platforms
//! 
//! This crate provides adapters for various messaging platforms including:
//! - Telegram (via Bot API)
//! 
//! ## Example Usage
//! 
//! ```rust,no_run
//! use alms_channel::telegram::TelegramChannel;
//! use alms_core::{Channel, ChannelConfig};
//! 
//! #[tokio::main]
//! async fn main() {
//!     let mut channel = TelegramChannel::new();
//!     let config = ChannelConfig {
//!         token: "YOUR_BOT_TOKEN".to_string(),
//!         ..Default::default()
//!     };
//!     
//!     channel.initialize(config).await.unwrap();
//!     channel.start().await.unwrap();
//!     
//!     let mut rx = channel.receive_updates().await.unwrap();
//!     
//!     while let Some(message) = rx.recv().await {
//!         println!("Received: {:?}", message);
//!     }
//! }
//! ```

pub mod telegram;

/// Re-export commonly used types
pub use alms_core::{Channel, ChannelConfig, IncomingMessage, OutgoingMessage, ChatId, UserId, MessageId};

/// Legacy ChannelAdapter struct (deprecated, use Channel trait implementations)
#[derive(Debug)]
pub struct ChannelAdapter;

impl ChannelAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ChannelAdapter {
    fn default() -> Self {
        Self::new()
    }
}
