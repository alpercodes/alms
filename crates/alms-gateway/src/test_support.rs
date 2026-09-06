// SPDX-License-Identifier: Apache-2.0

//! Test-only `AppState` construction.
//!
//! Every handler / lifecycle test needs an `AppState`, and until this
//! module existed each test file built its own from `Gateway::new` +
//! `AppState::new` + three channels — eighteen copies of the same twenty
//! lines, differing only in `db_path`, the LLM config and the channel
//! capacity. One builder, one place to change when `AppState::new` grows
//! a parameter.

use std::path::PathBuf;
use std::sync::Arc;

use alms_coordinator::SubagentCompletion;
use alms_coordinator::message_bus::{DmEvent, RunTrigger};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::gateway::{Gateway, GatewayConfig};
use crate::server::AppState;

/// An `AppState` plus its shutdown token and the receiving ends of the
/// three loop channels it was wired to. Tests that drive the completion /
/// trigger / DM-event loops themselves read from these; tests that don't
/// let them drop (a dropped bounded receiver makes sends fail fast rather
/// than block, so nothing hangs).
pub(crate) type AppStateWithChannels = (
    AppState,
    CancellationToken,
    mpsc::UnboundedReceiver<SubagentCompletion>,
    mpsc::Receiver<RunTrigger>,
    mpsc::Receiver<DmEvent>,
);

/// Builder for a test `AppState`. Defaults to `GatewayConfig::default()`:
/// in-memory session storage (no SQLite), the default LLM config, no
/// workspace dir.
#[derive(Default)]
pub(crate) struct TestAppState {
    config: GatewayConfig,
}

impl TestAppState {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Back the session manager with a SQLite store at `path`, so
    /// `session_manager.store()` returns `Some(..)` and the agent registry
    /// works. `":memory:"` for the usual case; a file path for tests that
    /// reopen the database to check restart-visible state.
    pub(crate) fn db_path(mut self, path: &str) -> Self {
        self.config.db_path = Some(path.to_string());
        self
    }

    /// [`Self::db_path`] with `":memory:"`.
    pub(crate) fn in_memory_sqlite(self) -> Self {
        self.db_path(":memory:")
    }

    /// Replace the LLM config wholesale — for the fixtures that point the
    /// client at a scripted local listener.
    pub(crate) fn llm_config(mut self, llm_config: alms_runtime::LlmConfig) -> Self {
        self.config.llm_config = llm_config;
        self
    }

    /// Mock-mode LLM: runs complete without touching the network.
    pub(crate) fn mock_llm(self) -> Self {
        self.llm_config(alms_runtime::LlmConfig {
            mock: true,
            ..alms_runtime::LlmConfig::default()
        })
    }

    /// Enable the workspace API rooted at `dir`.
    pub(crate) fn workspace_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.config.workspace_dir = Some(dir.into());
        self
    }

    /// Build the state and drop the channel receivers.
    pub(crate) fn build(self) -> AppState {
        self.build_with_channels().0
    }

    /// Build the state and hand back the receivers and shutdown token.
    pub(crate) fn build_with_channels(self) -> AppStateWithChannels {
        let gateway = Gateway::new(self.config).expect("test GatewayConfig must construct");
        let scheduler = Arc::new(alms_runtime::Scheduler::new());
        let shutdown_token = CancellationToken::new();
        let (completion_tx, completion_rx) = mpsc::unbounded_channel();
        // Bounded (#842 / B11) to match the production channel shape.
        let (trigger_tx, trigger_rx) = mpsc::channel(64);
        let (dm_event_tx, dm_event_rx) = mpsc::channel(64);
        let state = AppState::new(
            gateway,
            scheduler,
            shutdown_token.clone(),
            completion_tx,
            trigger_tx,
            dm_event_tx,
        )
        .expect("AppState::new");
        (
            state,
            shutdown_token,
            completion_rx,
            trigger_rx,
            dm_event_rx,
        )
    }
}
