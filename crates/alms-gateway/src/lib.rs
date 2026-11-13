// SPDX-License-Identifier: Apache-2.0

//! ALMS Gateway - HTTP/SSE API and message routing
//!
//! The gateway provides:
//! - HTTP REST API for session management
//! - SSE endpoints for live and replayable event delivery
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
pub(crate) mod configuration;
pub mod cron_utils;
pub mod event_log;
pub mod gateway;
pub mod jobs;
pub mod operations;
pub mod runs;
pub mod server;
pub mod session_queue;
pub mod settings;
pub mod sse;
/// Test-only `AppState` builder shared by every test module in this crate.
#[cfg(test)]
pub(crate) mod test_support;
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

/// Env-var test helpers shared between test modules in this crate.
///
/// Tests across `runs/integration_tests/` and `settings.rs::tests` run in the
/// same `cargo test -p alms-gateway` process, so an env var production code
/// reads is shared by all of them. A test that depends on
/// `ALMS_LLM_BUDGET_VALIDATION` pins it with [`BudgetValidationEnvGuard`],
/// which holds the binary's one env lock
/// (`alms_test_support::process_env::lock`) for the test's duration, so a
/// strict-mode test can no longer read a value a warn-mode test set.
#[cfg(test)]
pub(crate) mod test_env_locks {
    use alms_test_support::process_env::{self, VarGuard};

    const VAR: &str = "ALMS_LLM_BUDGET_VALIDATION";

    /// Pins `ALMS_LLM_BUDGET_VALIDATION` for the duration of a test while
    /// holding the env lock, and puts the previous value back on drop.
    pub(crate) struct BudgetValidationEnvGuard {
        // Field order is drop order: restore the var, then release the lock.
        _var: VarGuard,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl BudgetValidationEnvGuard {
        /// Pin the env var to `value`.
        pub(crate) fn set(value: &str) -> Self {
            let lock = process_env::lock();
            Self {
                _var: VarGuard::set(VAR, value),
                _lock: lock,
            }
        }

        /// Pin the env var to "unset".
        pub(crate) fn unset() -> Self {
            let lock = process_env::lock();
            Self {
                _var: VarGuard::remove(VAR),
                _lock: lock,
            }
        }
    }
}
