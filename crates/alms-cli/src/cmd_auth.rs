//! CLI commands for managing API key credentials.
//!
//! ```text
//! alms auth set <provider>       — set API key (prompts securely)
//! alms auth list                 — list providers with keys (masked)
//! alms auth remove <provider>    — remove a stored key
//! ```

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

pub(crate) fn auth_set(
    data_dir: &Path,
    provider: &str,
    key: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    if !VALID_PROVIDERS.contains(&provider) {
        anyhow::bail!(
            "Unknown provider '{}'. Must be one of: {}",
            provider,
            VALID_PROVIDERS.join(", ")
        );
    }

    let key = match key {
        Some(k) => k,
        None => {
            // Read from stdin (one line)
            eprint!("Enter API key for {}: ", provider);
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            line.trim().to_string()
        }
    };

    if key.is_empty() {
        anyhow::bail!("API key cannot be empty");
    }

    let mut store = SecretsStore::load(secrets::secrets_path(data_dir))?;
    store.set_key(provider, &key)?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "ok": true,
                "provider": provider,
                "key": SecretsStore::masked_key(&key),
            })
        );
    } else {
        println!(
            "Saved API key for '{}': {}",
            provider,
            SecretsStore::masked_key(&key)
        );
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
            let (configured, masked, source) = store.key_status(p);
            let display_source = if configured {
                "secrets".to_string()
            } else if source.starts_with("alias:") {
                let alias_from = source.strip_prefix("alias:").unwrap_or(&source);
                format!("via {alias_from}")
            } else {
                "not set".to_string()
            };
            let display_key = masked.unwrap_or_default();
            println!("{:<15} {:<16} {}", p, display_source, display_key);
        }
    }
    Ok(())
}

pub(crate) fn auth_remove(data_dir: &Path, provider: &str, json: bool) -> anyhow::Result<()> {
    if !VALID_PROVIDERS.contains(&provider) {
        anyhow::bail!(
            "Unknown provider '{}'. Must be one of: {}",
            provider,
            VALID_PROVIDERS.join(", ")
        );
    }
    let mut store = SecretsStore::load(secrets::secrets_path(data_dir))?;
    let existed = store.remove_key(provider)?;

    if json {
        println!(
            "{}",
            serde_json::json!({ "ok": true, "removed": existed, "provider": provider })
        );
    } else if existed {
        println!("Removed API key for '{}'", provider);
    } else {
        println!("No API key stored for '{}'", provider);
    }
    Ok(())
}
