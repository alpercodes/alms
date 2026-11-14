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

/// Give a test gateway a directory of its own for everything it would
/// otherwise put in the working directory, which is the crate directory and
/// shared by every test and every run:
///
/// * `data_dir`, unless the test set one: `AppState::new` loads
///   `settings.json` from it at boot and `PATCH /settings` writes it there.
///   Without one it falls back to `./.alms`, so a test booted with whatever
///   the last PATCH test persisted.
/// * `secrets_path`, when the database is in memory (`:memory:` or none):
///   the default rule puts `secrets.json` beside the database, which for an
///   in-memory one means `./secrets.json` or `./.alms/secrets.json`. A key
///   one test stored there was loaded by the next (`sk-or-test` reached
///   `key_revocation`'s scripted LLM that way). A file database keeps the
///   production rule: its tests own the directory the database is in.
///
/// The directory is deleted when the last clone of the config is dropped,
/// which is when the `AppState` built from it goes.
pub(crate) fn isolated(mut config: GatewayConfig) -> GatewayConfig {
    let dir = tempfile::Builder::new()
        .prefix("alms-gateway-test-")
        .tempdir()
        .expect("create a test state directory");
    if config.data_dir.is_none() {
        config.data_dir = Some(dir.path().to_path_buf());
    }
    let in_memory = config.db_path.as_deref().is_none_or(|p| p == ":memory:");
    if config.secrets_path.is_none() && in_memory {
        config.secrets_path = Some(dir.path().join("secrets.json"));
    }
    config.test_state_dir = Some(Arc::new(dir));
    config
}

/// Builder for a test `AppState`. Defaults to `GatewayConfig::default()`:
/// in-memory session storage (no SQLite), the default LLM config, no
/// workspace dir, and a state directory of its own ([`isolated`]).
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

    /// Use `dir` as the data directory, so `settings.json` and run spill
    /// files go there instead of under the test's working directory.
    pub(crate) fn data_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.config.data_dir = Some(dir.into());
        self
    }

    /// Enable the workspace API rooted at `dir`.
    pub(crate) fn workspace_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.config.workspace_dir = Some(dir.into());
        self
    }

    /// The configured default agent ID, as `alms gateway` passes the
    /// sidecar's (or `ALMS_AGENT_ID`).
    pub(crate) fn agent_id(mut self, id: alms_core::AgentId) -> Self {
        self.config.agent_id = Some(id);
        self
    }

    /// Set the server-level `[context]` summary pair, as `alms.toml` would.
    pub(crate) fn summary_pair(mut self, provider: &str, model: &str) -> Self {
        let ctx = &mut self.config.agent_config.context_config;
        ctx.summary_provider = Some(provider.to_string());
        ctx.summary_model = Some(model.to_string());
        self
    }

    /// Build the state and drop the channel receivers.
    pub(crate) fn build(self) -> AppState {
        self.build_with_channels().0
    }

    /// Build the state and hand back the receivers and shutdown token.
    pub(crate) fn build_with_channels(self) -> AppStateWithChannels {
        let gateway =
            Gateway::new(isolated(self.config)).expect("test GatewayConfig must construct");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A key one test stores must not be what the next test boots with. On an
    /// in-memory gateway both used to read and write `./secrets.json` in the
    /// crate directory, and the file outlived the run too.
    #[tokio::test]
    async fn test_states_do_not_share_secrets() {
        let a = TestAppState::new().in_memory_sqlite().build();
        a.secrets
            .write()
            .set_key("openrouter", "sk-isolation-probe-from-a")
            .unwrap();
        let b = TestAppState::new().in_memory_sqlite().build();
        assert_eq!(
            b.secrets.read().get_key("openrouter"),
            None,
            "a fresh test state loaded another state's key"
        );

        // The same without a database: `./.alms/secrets.json` before.
        let c = TestAppState::new().build();
        c.secrets
            .write()
            .set_key("openrouter", "sk-isolation-probe-from-c")
            .unwrap();
        let d = TestAppState::new().build();
        assert_eq!(d.secrets.read().get_key("openrouter"), None);
    }

    /// The settings twin: `PATCH /settings` on one state persists
    /// `settings.json` in its data directory, and a fresh state must boot
    /// with the compiled defaults, not with that file.
    #[tokio::test]
    async fn test_states_do_not_share_settings() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        let default_strategy = TestAppState::new()
            .build()
            .agent_config
            .read()
            .context_config
            .strategy
            .clone();
        let changed = if default_strategy == "compact" {
            "truncate"
        } else {
            "compact"
        };

        let a = TestAppState::new().build();
        let response = crate::server::routes::protected_router()
            .with_state(a.clone())
            .oneshot(
                Request::patch("/settings")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "context": { "strategy": changed } }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(a.agent_config.read().context_config.strategy, changed);

        let b = TestAppState::new().build();
        assert_eq!(
            b.agent_config.read().context_config.strategy,
            default_strategy,
            "a fresh test state booted with another state's persisted settings"
        );
    }
}
