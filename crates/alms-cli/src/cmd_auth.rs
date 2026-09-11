// SPDX-License-Identifier: Apache-2.0

//! CLI commands for managing API key credentials.
//!
//! ```text
//! alms auth set <provider>       — set API key (prompts on stdin)
//! alms auth list                 — list providers with keys (masked)
//! alms auth remove <provider>    — remove a stored key
//! ```
//!
//! Omitting the key argument reads it from stdin, which keeps it out of
//! `argv` — so out of `ps` output and shell history. That is the only
//! property claimed: the prompt does **not** suppress terminal echo, so
//! the key is visible as it is typed.
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
//!
//! Both commands write their result to a caller-supplied sink rather than
//! to `println!`, so the `--json` body — including the `target` field a
//! script reads to tell a live change from a file write — is asserted by
//! tests rather than merely exercised. `list` is untouched by #145 and
//! still prints directly.

use crate::helpers::{GatewayProbe, api_delete_json, api_put, probe_gateway};
use alms_core::secrets::{self, SecretsStore, VALID_PROVIDERS};
use clap::Subcommand;
use std::io::Write;
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
///
/// Deliberately names no path. The CLI resolves the secrets file from
/// `data_dir` while the gateway resolves it from its `db_path`
/// (`secrets_path_from_db`), and `ALMS_DB_PATH` can point those at
/// different files — so "the file the gateway reads" is a claim this
/// command is not in a position to make (Tim S1 on #168).
fn gateway_refused(url: &str, err: &anyhow::Error) -> anyhow::Error {
    anyhow::anyhow!(
        "The gateway at {url} rejected the change: {err}\n\
         Nothing was written: a gateway reads its secrets file once, at boot, so writing that \
         file now would look like success and change nothing while this one runs.\n\
         Set ALMS_AUTH_TOKEN if the gateway requires a token, or stop the gateway and run this \
         again."
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
    out: &mut dyn Write,
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
                .map_err(|e| gateway_refused(url, &e))?;
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
        writeln!(
            out,
            "{}",
            serde_json::json!({
                "ok": true,
                "provider": provider,
                "key": masked,
                "target": target.label(),
            })
        )?;
    } else {
        match target {
            KeyTarget::RunningGateway => writeln!(
                out,
                "Saved API key for '{provider}': {masked} — applied to the gateway at {url}, \
                 no restart needed."
            )?,
            KeyTarget::SecretsFile { .. } => {
                writeln!(out, "Saved API key for '{provider}': {masked}")?
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
    out: &mut dyn Write,
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
                .map_err(|e| gateway_refused(url, &e))?;
            // The handler always sends `removed`, so this default is
            // defensive only — and of the two, `false` under-claims where
            // `true` would report a revocation nothing confirmed. On a
            // credential path, prefer the one that makes an operator check
            // (Tim S3 on #168).
            resp.get("removed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
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
        writeln!(
            out,
            "{}",
            serde_json::json!({
                "ok": true,
                "removed": existed,
                "provider": provider,
                "target": target.label(),
            })
        )?;
    } else if !existed {
        writeln!(out, "No API key stored for '{provider}'")?;
    } else {
        match target {
            KeyTarget::RunningGateway => writeln!(
                out,
                "Removed API key for '{provider}' — applied to the gateway at {url}, \
                 no restart needed."
            )?,
            KeyTarget::SecretsFile { .. } => writeln!(out, "Removed API key for '{provider}'")?,
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

    const TEST_KEY: &str = "sk-test-key-0123456789";

    fn health_ok() -> Mock {
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "healthy"
            })))
    }

    /// A gateway that takes a `PUT /auth/keys` and answers like the real
    /// handler does.
    async fn gateway_accepting_keys() -> MockServer {
        let server = MockServer::start().await;
        health_ok().mount(&server).await;
        Mock::given(method("PUT"))
            .and(path("/auth/keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true, "provider": "openrouter", "key": "sk-t...6789",
            })))
            .mount(&server)
            .await;
        server
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

    fn as_json(out: Vec<u8>) -> serde_json::Value {
        serde_json::from_slice(&out).expect("--json must emit one JSON object")
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
        let server = gateway_accepting_keys().await;
        let dir = tempfile::tempdir().unwrap();

        auth_set(
            &api_client().unwrap(),
            &server.uri(),
            dir.path(),
            "openrouter",
            Some(TEST_KEY.into()),
            false,
            &mut std::io::sink(),
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
        assert_eq!(body["key"], TEST_KEY);

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
            Some(TEST_KEY.into()),
            false,
            &mut std::io::sink(),
        )
        .await
        .unwrap();

        assert_eq!(secrets_of(dir.path()).get_key("openrouter"), Some(TEST_KEY));
    }

    /// `target` is the one field a script needs to tell a live change from
    /// a file write, so both of its values are pinned here rather than
    /// left to the routing tests — those prove where the change *went*,
    /// not what the CLI *said* about it (Tim S2 on #168).
    #[tokio::test]
    async fn test_auth_set_json_names_the_target_that_took_the_change() {
        let server = gateway_accepting_keys().await;
        let dir = tempfile::tempdir().unwrap();
        let mut out = Vec::new();

        auth_set(
            &api_client().unwrap(),
            &server.uri(),
            dir.path(),
            "openrouter",
            Some(TEST_KEY.into()),
            true,
            &mut out,
        )
        .await
        .unwrap();

        let body = as_json(out);
        assert_eq!(body["target"], "gateway");
        assert_eq!(body["ok"], true);
        assert_eq!(body["provider"], "openrouter");
        assert_eq!(
            body["key"], "sk-t...6789",
            "the key must be masked in output"
        );

        let mut out = Vec::new();
        auth_set(
            &api_client().unwrap(),
            &dead_url(),
            dir.path(),
            "openrouter",
            Some(TEST_KEY.into()),
            true,
            &mut out,
        )
        .await
        .unwrap();

        assert_eq!(as_json(out)["target"], "secrets_file");
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
            Some(TEST_KEY.into()),
            false,
            &mut std::io::sink(),
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

        let mut out = Vec::new();
        auth_remove(
            &api_client().unwrap(),
            &server.uri(),
            dir.path(),
            "openrouter",
            true,
            &mut out,
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
        let body = as_json(out);
        assert_eq!(body["target"], "gateway");
        assert_eq!(body["removed"], true);
        assert_eq!(
            secrets_of(dir.path()).get_key("openrouter"),
            Some("sk-daemon-owned-key"),
            "the gateway rewrites the file from its own map; the CLI must not race it"
        );
    }

    /// A 2xx whose body does not carry `removed` is not evidence that a
    /// key was revoked. Reporting one anyway is the failure mode worth
    /// avoiding on a credential path: under-claiming makes an operator
    /// check, over-claiming makes them stop (Tim S3 on #168).
    #[tokio::test]
    async fn test_auth_remove_does_not_claim_a_revocation_the_gateway_did_not_confirm() {
        let server = MockServer::start().await;
        health_ok().mount(&server).await;
        Mock::given(method("DELETE"))
            .and(path("/auth/keys/openrouter"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })),
            )
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let mut out = Vec::new();
        auth_remove(
            &api_client().unwrap(),
            &server.uri(),
            dir.path(),
            "openrouter",
            true,
            &mut out,
        )
        .await
        .unwrap();

        assert_eq!(
            as_json(out)["removed"],
            false,
            "a missing `removed` must not be read as a confirmed revocation"
        );
    }

    #[tokio::test]
    async fn test_auth_remove_edits_the_file_when_no_gateway_answers() {
        let dir = tempfile::tempdir().unwrap();
        secrets_of(dir.path())
            .set_key("openrouter", TEST_KEY)
            .unwrap();

        let mut out = Vec::new();
        auth_remove(
            &api_client().unwrap(),
            &dead_url(),
            dir.path(),
            "openrouter",
            true,
            &mut out,
        )
        .await
        .unwrap();

        let body = as_json(out);
        assert_eq!(body["target"], "secrets_file");
        assert_eq!(body["removed"], true);
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
            Some(TEST_KEY.into()),
            false,
            &mut std::io::sink(),
        )
        .await
        .expect_err("unknown providers must be rejected");
        assert!(err.to_string().contains("Unknown provider"));
    }
}
