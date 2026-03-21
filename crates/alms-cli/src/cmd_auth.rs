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
    let providers = store.list_providers();

    if json {
        let entries: Vec<serde_json::Value> = VALID_PROVIDERS
            .iter()
            .map(|p| {
                serde_json::json!({
                    "provider": p,
                    "configured": providers.contains(&p.to_string()),
                    "key": store.get_masked(p),
                    "source": if providers.contains(&p.to_string()) {
                        "secrets"
                    } else if alms_core::config::select_llm_api_key_from_env(p).is_some() {
                        "env"
                    } else {
                        "none"
                    },
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else {
        println!("{:<15} {:<12} {}", "PROVIDER", "SOURCE", "KEY");
        println!("{}", "-".repeat(50));
        for p in VALID_PROVIDERS {
            let env_key = alms_core::config::select_llm_api_key_from_env(p);
            let (source, masked) = if providers.contains(&p.to_string()) {
                ("secrets", store.get_masked(p).unwrap_or_default())
            } else if let Some(k) = env_key {
                ("env var", SecretsStore::masked_key(&k))
            } else {
                ("not set", String::new())
            };
            println!("{:<15} {:<12} {}", p, source, masked);
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
