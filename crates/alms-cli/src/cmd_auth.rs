// SPDX-License-Identifier: Apache-2.0

//! CLI commands for managing API key credentials.
//!
//! ```text
//! alms auth set <provider>       — set API key (prompts securely)
//! alms auth list                 — list providers with keys (masked)
//! alms auth remove <provider>    — remove a stored key
//! ```
//!
//! # Where a key change lands (#145)
//!
//! A gateway loads `.alms/secrets.json` **once, at boot** and every key
//! lookup afterwards reads that in-memory store. So a `set` that only
//! writes the file leaves an already-running daemon unaware of a key the
//! operator can plainly see on disk, and the next run still fails to
//! authenticate — a failure that looks like success.
//!
//! `set` and `remove` therefore probe `GET /health` first ([`route_key_change`]):
//! when a gateway answers, the change goes over HTTP to *that process*
//! instead of to the file. Nothing is lost by not writing the file
//! ourselves — the handler's `SecretsStore::set_key` persists to the
//! daemon's own secrets file, so the change still survives a restart —
//! and writing both would mean two processes writing one file, where the
//! daemon's next save (it writes its whole map) decides the outcome.

use crate::helpers::{GatewayProbe, api_delete_json, api_put, probe_gateway};
use alms_core::secrets::{self, SecretsStore, VALID_PROVIDERS};
use clap::Subcommand;
use std::path::Path;

#[derive(Subcommand, Debug)]
pub(crate) enum AuthCommands {
    /// Set an API key for a provider
    Set {
        /// Provider name: openai, anthropic, openrouter
        provider: String,
        /// API key (if omitted, reads from stdin)
        key: Option<String>,
    },
    /// List providers with stored keys
    List,
    /// Remove a stored API key
    Remove {
        /// Provider name to remove
        provider: String,
    },
}

/// Where a key change made by `set` / `remove` should land.
#[derive(Debug, PartialEq, Eq)]
enum KeyTarget {
    /// A gateway answered `/health` — send the change to that process so
    /// it takes effect on the next run. It persists the change itself.
    RunningGateway,
    /// Write the secrets file directly. `warning` is set when *something*
    /// answered at the URL but not healthily: it may or may not be an ALMS
    /// gateway, so the file write stands, and the operator is told that a
    /// gateway there would need a restart.
    SecretsFile { warning: Option<String> },
}

impl KeyTarget {
    /// The `target` field of `--json` output.
    fn label(&self) -> &'static str {
        match self {
            KeyTarget::RunningGateway => "gateway",
            KeyTarget::SecretsFile { .. } => "secrets_file",
        }
    }
}

/// Decide where a key change goes from the outcome of the `/health` probe.
///
/// Split out from the commands so the routing rule is testable without a
/// socket; `probe_gateway` (shared with `alms dashboard`) is what turns a
/// URL into the [`GatewayProbe`] fed in here.
fn route_key_change(probe: GatewayProbe, url: &str) -> KeyTarget {
    match probe {
        GatewayProbe::Healthy => KeyTarget::RunningGateway,
        GatewayProbe::Unhealthy(status) => KeyTarget::SecretsFile {
            warning: Some(format!(
                "Warning: {url}/health returned HTTP {status} — wrote the secrets file rather \
                 than asking the gateway. If a gateway is running there, restart it to pick \
                 this up."
            )),
        },
        GatewayProbe::Unreachable(_) => KeyTarget::SecretsFile { warning: None },
    }
}

/// Refusing to fall back to the file after a running gateway rejects the
/// change: writing it then would restore exactly the silent failure #145
/// is about.
fn gateway_refused(url: &str, data_dir: &Path, err: &anyhow::Error) -> anyhow::Error {
    anyhow::anyhow!(
        "The gateway at {url} rejected the change: {err}\n\
         Nothing was written: {} is read only when a gateway boots, so writing it while that \
         one runs would look like success and change nothing.\n\
         Set ALMS_AUTH_TOKEN if the gateway requires a token, or stop the gateway and run this \
         again.",
        secrets::secrets_path(data_dir).display()
    )
}

fn reject_unknown_provider(provider: &str) -> anyhow::Result<()> {
    if !VALID_PROVIDERS.contains(&provider) {
        anyhow::bail!(
            "Unknown provider '{}'. Must be one of: {}",
            provider,
            VALID_PROVIDERS.join(", ")
        );
    }
    Ok(())
}

pub(crate) async fn auth_set(
    client: &reqwest::Client,
    url: &str,
    data_dir: &Path,
    provider: &str,
    key: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    reject_unknown_provider(provider)?;

    let key = match key {
        Some(k) => k,
        None => {
            // Read from stdin (one line). Prompted before the probe so the
            // 3s health deadline never sits between the operator and the
            // prompt.
            eprint!("Enter API key for {}: ", provider);
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            line.trim().to_string()
        }
    };

    if key.is_empty() {
        anyhow::bail!("API key cannot be empty");
    }

    let masked = SecretsStore::masked_key(&key);
    let target = route_key_change(probe_gateway(client, url).await, url);

    match &target {
        KeyTarget::RunningGateway => {
            let body = serde_json::json!({ "provider": provider, "key": key });
            api_put(client, url, "auth/keys", &body)
                .await
                .map_err(|e| gateway_refused(url, data_dir, &e))?;
        }
        KeyTarget::SecretsFile { warning } => {
            if let Some(warning) = warning {
                eprintln!("{warning}");
            }
            let mut store = SecretsStore::load(secrets::secrets_path(data_dir))?;
            store.set_key(provider, &key)?;
        }
    }

    if json {
        println!(
            "{}",
            serde_json::json!({
                "ok": true,
                "provider": provider,
                "key": masked,
                "target": target.label(),
            })
        );
    } else {
        match target {
            KeyTarget::RunningGateway => println!(
                "Saved API key for '{provider}': {masked} — applied to the gateway at {url}, \
                 no restart needed."
            ),
            KeyTarget::SecretsFile { .. } => {
                println!("Saved API key for '{provider}': {masked}")
            }
        }
    }
    Ok(())
}

pub(crate) fn auth_list(data_dir: &Path, json: bool) -> anyhow::Result<()> {
    let store = SecretsStore::load(secrets::secrets_path(data_dir))?;

    if json {
        let entries: Vec<serde_json::Value> = VALID_PROVIDERS
            .iter()
            .map(|p| {
                let (configured, masked, source) = store.key_status(p);
                serde_json::json!({
                    "provider": p,
                    "configured": configured,
                    "key": masked,
                    "source": source,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else {
        println!("{:<15} {:<16} KEY", "PROVIDER", "SOURCE");
        println!("{}", "-".repeat(55));
        for p in VALID_PROVIDERS {
            let (configured, masked, _source) = store.key_status(p);
            let display_source = if configured {
                "secrets".to_string()
            } else {
                "not set".to_string()
            };
            let display_key = masked.unwrap_or_default();
            println!("{:<15} {:<16} {}", p, display_source, display_key);
        }
    }
    Ok(())
}

pub(crate) async fn auth_remove(
    client: &reqwest::Client,
    url: &str,
    data_dir: &Path,
    provider: &str,
    json: bool,
) -> anyhow::Result<()> {
    reject_unknown_provider(provider)?;

    let target = route_key_change(probe_gateway(client, url).await, url);

    // Same routing as `set`, and for the same reason plus one: a removal
    // that only reaches the file leaves the daemon holding the revoked key
    // *and* writes it back on its next save.
    let existed = match &target {
        KeyTarget::RunningGateway => {
            let resp = api_delete_json(client, url, &format!("auth/keys/{provider}"))
                .await
                .map_err(|e| gateway_refused(url, data_dir, &e))?;
            resp.get("removed")
                .and_then(|v| v.as_bool())
                .unwrap_or(true)
        }
        KeyTarget::SecretsFile { warning } => {
            if let Some(warning) = warning {
                eprintln!("{warning}");
            }
            let mut store = SecretsStore::load(secrets::secrets_path(data_dir))?;
            store.remove_key(provider)?
        }
    };

    if json {
        println!(
            "{}",
            serde_json::json!({
                "ok": true,
                "removed": existed,
                "provider": provider,
                "target": target.label(),
            })
        );
    } else if !existed {
        println!("No API key stored for '{provider}'");
    } else {
        match target {
            KeyTarget::RunningGateway => println!(
                "Removed API key for '{provider}' — applied to the gateway at {url}, \
                 no restart needed."
            ),
            KeyTarget::SecretsFile { .. } => println!("Removed API key for '{provider}'"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::api_client;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn health_ok() -> Mock {
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "healthy"
            })))
    }

    /// A URL nothing is listening on: bind to let the OS pick a free port,
    /// then drop so the probe meets connection-refused.
    fn dead_url() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        format!("http://127.0.0.1:{port}")
    }

    fn secrets_of(dir: &Path) -> SecretsStore {
        SecretsStore::load(secrets::secrets_path(dir)).unwrap()
    }

    #[test]
    fn test_route_key_change_healthy_gateway_takes_the_change() {
        assert_eq!(
            route_key_change(GatewayProbe::Healthy, "http://127.0.0.1:8080"),
            KeyTarget::RunningGateway
        );
    }

    #[test]
    fn test_route_key_change_no_gateway_writes_the_file_without_a_warning() {
        // Nothing is running, so there is nothing to restart and nothing
        // to warn about — this is the first-run path.
        assert_eq!(
            route_key_change(
                GatewayProbe::Unreachable("connection refused".into()),
                "http://127.0.0.1:8080"
            ),
            KeyTarget::SecretsFile { warning: None }
        );
    }

    /// Something answered but not healthily — it may not be a gateway at
    /// all, so the file write stands. The warning is the whole value of
    /// this arm: it is the only path where a key can still land somewhere
    /// a running process will not read.
    #[test]
    fn test_route_key_change_unhealthy_warns_about_a_restart() {
        let target = route_key_change(
            GatewayProbe::Unhealthy(reqwest::StatusCode::BAD_GATEWAY),
            "http://127.0.0.1:8080",
        );
        let KeyTarget::SecretsFile { warning } = target else {
            panic!("an unhealthy probe must still write the file, got {target:?}");
        };
        let warning = warning.expect("an unhealthy probe must warn");
        assert!(warning.contains("502"), "got {warning:?}");
        assert!(warning.contains("restart"), "got {warning:?}");
    }

    /// The bug in #145: with a gateway running, the key has to reach *that
    /// process*. It must not be written to the file the daemon read at
    /// boot and will never read again.
    #[tokio::test]
    async fn test_auth_set_sends_the_key_to_a_running_gateway_and_skips_the_file() {
        let server = MockServer::start().await;
        health_ok().mount(&server).await;
        Mock::given(method("PUT"))
            .and(path("/auth/keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true, "provider": "openrouter", "key": "sk-t...key",
            })))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        auth_set(
            &api_client().unwrap(),
            &server.uri(),
            dir.path(),
            "openrouter",
            Some("sk-test-key-0123456789".into()),
            false,
        )
        .await
        .unwrap();

        let requests = server.received_requests().await.unwrap();
        let put = requests
            .iter()
            .find(|r| r.method == wiremock::http::Method::PUT)
            .expect("the key must be PUT to the running gateway");
        assert_eq!(put.url.path(), "/auth/keys");
        let body: serde_json::Value = serde_json::from_slice(&put.body).unwrap();
        assert_eq!(body["provider"], "openrouter");
        assert_eq!(body["key"], "sk-test-key-0123456789");

        assert!(
            !secrets::secrets_path(dir.path()).exists(),
            "the daemon persists the key itself; a second writer would race its next save"
        );
    }

    /// No gateway: the file write is still the right answer, unchanged.
    #[tokio::test]
    async fn test_auth_set_writes_the_file_when_no_gateway_answers() {
        let dir = tempfile::tempdir().unwrap();
        auth_set(
            &api_client().unwrap(),
            &dead_url(),
            dir.path(),
            "openrouter",
            Some("sk-test-key-0123456789".into()),
            false,
        )
        .await
        .unwrap();

        assert_eq!(
            secrets_of(dir.path()).get_key("openrouter"),
            Some("sk-test-key-0123456789")
        );
    }

    /// A gateway that answers `/health` but rejects the write — a token
    /// the CLI's shell does not have is the common case. Falling back to
    /// the file here would re-create #145 exactly: a file the running
    /// daemon never re-reads, and a success message.
    #[tokio::test]
    async fn test_auth_set_fails_loudly_when_the_gateway_rejects_the_key() {
        let server = MockServer::start().await;
        health_ok().mount(&server).await;
        Mock::given(method("PUT"))
            .and(path("/auth/keys"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": { "code": "UNAUTHORIZED", "message": "Missing or invalid Bearer token" },
            })))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let err = auth_set(
            &api_client().unwrap(),
            &server.uri(),
            dir.path(),
            "openrouter",
            Some("sk-test-key-0123456789".into()),
            false,
        )
        .await
        .expect_err("a rejected key must not be reported as saved");

        let msg = err.to_string();
        assert!(msg.contains("401"), "got {msg:?}");
        assert!(msg.contains("ALMS_AUTH_TOKEN"), "got {msg:?}");
        assert!(
            !secrets::secrets_path(dir.path()).exists(),
            "a rejected key must not be written to a file nothing will re-read"
        );
    }

    /// Revocation has the sharper version of the same failure: a key
    /// removed from the file only is still live in the daemon, and the
    /// daemon's next save writes it back.
    #[tokio::test]
    async fn test_auth_remove_revokes_on_the_running_gateway_and_leaves_the_file_alone() {
        let server = MockServer::start().await;
        health_ok().mount(&server).await;
        Mock::given(method("DELETE"))
            .and(path("/auth/keys/openrouter"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true, "removed": true, "provider": "openrouter",
            })))
            .mount(&server)
            .await;

        // The daemon owns the file in this state; the CLI must not touch it.
        let dir = tempfile::tempdir().unwrap();
        secrets_of(dir.path())
            .set_key("openrouter", "sk-daemon-owned-key")
            .unwrap();

        auth_remove(
            &api_client().unwrap(),
            &server.uri(),
            dir.path(),
            "openrouter",
            false,
        )
        .await
        .unwrap();

        let deletes = server.received_requests().await.unwrap();
        assert!(
            deletes
                .iter()
                .any(|r| r.method == wiremock::http::Method::DELETE
                    && r.url.path() == "/auth/keys/openrouter"),
            "the removal must reach the running gateway"
        );
        assert_eq!(
            secrets_of(dir.path()).get_key("openrouter"),
            Some("sk-daemon-owned-key"),
            "the gateway rewrites the file from its own map; the CLI must not race it"
        );
    }

    #[tokio::test]
    async fn test_auth_remove_edits_the_file_when_no_gateway_answers() {
        let dir = tempfile::tempdir().unwrap();
        secrets_of(dir.path())
            .set_key("openrouter", "sk-test-key-0123456789")
            .unwrap();

        auth_remove(
            &api_client().unwrap(),
            &dead_url(),
            dir.path(),
            "openrouter",
            false,
        )
        .await
        .unwrap();

        assert_eq!(secrets_of(dir.path()).get_key("openrouter"), None);
    }

    /// An unknown provider is rejected before any probe, so a typo never
    /// depends on whether a gateway happens to be up.
    #[tokio::test]
    async fn test_auth_set_rejects_an_unknown_provider_without_a_gateway() {
        let dir = tempfile::tempdir().unwrap();
        let err = auth_set(
            &api_client().unwrap(),
            &dead_url(),
            dir.path(),
            "not-a-provider",
            Some("sk-test-key-0123456789".into()),
            false,
        )
        .await
        .expect_err("unknown providers must be rejected");
        assert!(err.to_string().contains("Unknown provider"));
    }
}
