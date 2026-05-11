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
pub mod auth_keys;
pub mod cron_utils;
pub mod event_log;
pub mod gateway;
pub mod jobs;
pub mod runs;
pub mod server;
pub mod session_queue;
pub mod settings;
pub mod sse;
pub mod timeline;
pub mod workspace;

/// Build a structured API error response.
///
/// Used across all HTTP handlers to produce consistent
/// `{ "error": { "code": "...", "message": "..." } }` bodies.
pub(crate) fn api_error(
    status: axum::http::StatusCode,
    code: &str,
    message: impl std::fmt::Display,
) -> (axum::http::StatusCode, axum::Json<serde_json::Value>) {
    (
        status,
        axum::Json(serde_json::json!({
            "error": { "code": code, "message": message.to_string() }
        })),
    )
}

// Re-export main types
pub use gateway::{Gateway, GatewayConfig};
pub use runs::{create_run, get_run_status};
pub use server::{AppState, RunManager, serve, serve_with_config, serve_with_gateway};
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

/// Process-global env-var locks shared between test modules in this crate.
///
/// Tests across `runs/integration_tests.rs` and `settings.rs::tests` run in
/// the same `cargo test -p alms-gateway` process, so any env var read by
/// production code must be serialised across all touch sites — otherwise a
/// strict-mode test in one file races a warn-mode test in another and ends
/// up reading the wrong value. Each mutex MUST guard a disjoint var-set so
/// a test holding one lock can never observe a partial mutation guarded by
/// another lock; cross-cutting tests that need multiple locks should
/// acquire them in a fixed order to avoid deadlock.
#[cfg(test)]
pub(crate) mod test_env_locks {
    /// Serialises every test that mutates `ALMS_LLM_BUDGET_VALIDATION`
    /// (the #919 strict/warn opt-out). Disjoint by construction from the
    /// `ENV_LOCK` mutex in `alms-core/src/config/tests.rs` (different
    /// process during `cargo test -p alms-core`) and from any future
    /// gateway-side env-var mutex covering a different var-set.
    pub(crate) static BUDGET_VALIDATION_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// RAII guard that pins `ALMS_LLM_BUDGET_VALIDATION` to a known value
    /// for the duration of a test, holding
    /// [`BUDGET_VALIDATION_ENV_LOCK`] so no concurrent test mutates it.
    /// Restores the previous value (or removes it) on drop.
    pub(crate) struct BudgetValidationEnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<String>,
    }

    impl BudgetValidationEnvGuard {
        /// Pin the env var to `value`.
        pub(crate) fn set(value: &str) -> Self {
            let lock = BUDGET_VALIDATION_ENV_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var("ALMS_LLM_BUDGET_VALIDATION").ok();
            unsafe {
                std::env::set_var("ALMS_LLM_BUDGET_VALIDATION", value);
            }
            Self { _lock: lock, prev }
        }

        /// Pin the env var to "unset".
        pub(crate) fn unset() -> Self {
            let lock = BUDGET_VALIDATION_ENV_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var("ALMS_LLM_BUDGET_VALIDATION").ok();
            unsafe {
                std::env::remove_var("ALMS_LLM_BUDGET_VALIDATION");
            }
            Self { _lock: lock, prev }
        }
    }

    impl Drop for BudgetValidationEnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => unsafe { std::env::set_var("ALMS_LLM_BUDGET_VALIDATION", v) },
                None => unsafe { std::env::remove_var("ALMS_LLM_BUDGET_VALIDATION") },
            }
        }
    }
}
