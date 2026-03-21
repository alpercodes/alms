//! Secure API key storage.
//!
//! Keys are stored in a JSON file (`data/secrets.json`) with restrictive
//! file permissions (0600 on Unix). This avoids requiring env vars and
//! follows the pattern of tools like `~/.aws/credentials`.
//!
//! Precedence: secrets file > env vars.

use crate::AlmsResult;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Valid LLM provider names. Single source of truth for CLI, HTTP API, and UI.
pub const VALID_PROVIDERS: &[&str] = &["openai", "anthropic", "openrouter"];

/// Resolve the default secrets file path from a data directory.
pub fn secrets_path(data_dir: &Path) -> PathBuf {
    data_dir.join("secrets.json")
}

/// Resolve secrets path from an optional database file path.
/// Falls back to the default data directory if no db_path is provided.
pub fn secrets_path_from_db(db_path: Option<&str>) -> PathBuf {
    db_path
        .and_then(|p| Path::new(p).parent().map(|d| d.join("secrets.json")))
        .unwrap_or_else(|| secrets_path(Path::new("./data")))
}

/// In-memory secrets store backed by a JSON file.
#[derive(Clone)]
pub struct SecretsStore {
    path: PathBuf,
    keys: HashMap<String, String>,
}

impl std::fmt::Debug for SecretsStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretsStore")
            .field("path", &self.path)
            .field("providers", &self.keys.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct SecretsFile {
    #[serde(default)]
    api_keys: HashMap<String, String>,
}

impl SecretsStore {
    /// Create an empty in-memory secrets store with no file backing.
    pub fn empty() -> Self {
        Self {
            path: PathBuf::new(),
            keys: HashMap::new(),
        }
    }

    /// Load secrets from a file path, creating it if it doesn't exist.
    pub fn load(path: impl Into<PathBuf>) -> AlmsResult<Self> {
        let path = path.into();
        let keys = if path.exists() {
            let content = std::fs::read_to_string(&path).map_err(|e| {
                crate::AlmsError::Runtime(format!("Failed to read secrets file: {e}"))
            })?;
            let file: SecretsFile = serde_json::from_str(&content).map_err(|e| {
                crate::AlmsError::Runtime(format!("Failed to parse secrets file: {e}"))
            })?;
            file.api_keys
        } else {
            HashMap::new()
        };
        Ok(Self { path, keys })
    }

    /// Get an API key for a provider. Returns None if not set.
    pub fn get_key(&self, provider: &str) -> Option<&str> {
        self.keys.get(provider).map(|s| s.as_str())
    }

    /// Set an API key for a provider and persist to disk.
    pub fn set_key(&mut self, provider: &str, key: &str) -> AlmsResult<()> {
        self.keys.insert(provider.to_string(), key.to_string());
        self.save()
    }

    /// Remove an API key for a provider and persist to disk.
    pub fn remove_key(&mut self, provider: &str) -> AlmsResult<bool> {
        let existed = self.keys.remove(provider).is_some();
        if existed {
            self.save()?;
        }
        Ok(existed)
    }

    /// List providers that have keys set (no values exposed).
    pub fn list_providers(&self) -> Vec<String> {
        self.keys.keys().cloned().collect()
    }

    /// Mask a key for display: show first 4 and last 4 chars.
    /// Safe for non-ASCII keys (uses char boundaries).
    pub fn masked_key(key: &str) -> String {
        let chars: Vec<char> = key.chars().collect();
        if chars.len() <= 12 {
            return "*".repeat(chars.len());
        }
        let prefix: String = chars[..4].iter().collect();
        let suffix: String = chars[chars.len() - 4..].iter().collect();
        format!("{prefix}...{suffix}")
    }

    /// Get a masked version of a key for a provider.
    pub fn get_masked(&self, provider: &str) -> Option<String> {
        self.keys.get(provider).map(|k| Self::masked_key(k))
    }

    /// Resolve an API key for a provider: secrets file first, then env vars.
    pub fn resolve_key(&self, provider: &str) -> Option<String> {
        // Secrets file takes precedence
        if let Some(key) = self.get_key(provider) {
            return Some(key.to_string());
        }
        // Fall back to env vars
        crate::config::select_llm_api_key_from_env(provider)
    }

    /// Save secrets to disk with restrictive permissions.
    fn save(&self) -> AlmsResult<()> {
        let file = SecretsFile {
            api_keys: self.keys.clone(),
        };
        let content = serde_json::to_string_pretty(&file)
            .map_err(|e| crate::AlmsError::Runtime(format!("Failed to serialize secrets: {e}")))?;

        // Ensure parent directory exists
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                crate::AlmsError::Runtime(format!("Failed to create secrets directory: {e}"))
            })?;
        }

        std::fs::write(&self.path, &content)
            .map_err(|e| crate::AlmsError::Runtime(format!("Failed to write secrets file: {e}")))?;

        // Set restrictive permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&self.path, perms).map_err(|e| {
                crate::AlmsError::Runtime(format!("Failed to set secrets file permissions: {e}"))
            })?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_get_remove() {
        let tmp = std::env::temp_dir().join(format!("alms-secrets-{}", uuid::Uuid::new_v4()));
        let path = tmp.join("secrets.json");
        let mut store = SecretsStore::load(&path).unwrap();

        assert!(store.get_key("openai").is_none());

        store.set_key("openai", "sk-test-key-12345").unwrap();
        assert_eq!(store.get_key("openai"), Some("sk-test-key-12345"));

        // Verify persisted
        let store2 = SecretsStore::load(&path).unwrap();
        assert_eq!(store2.get_key("openai"), Some("sk-test-key-12345"));

        // Remove
        assert!(store.remove_key("openai").unwrap());
        assert!(store.get_key("openai").is_none());
        assert!(!store.remove_key("openai").unwrap()); // already gone

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_masked_key() {
        assert_eq!(
            SecretsStore::masked_key("sk-test-key-12345678"),
            "sk-t...5678"
        );
        assert_eq!(SecretsStore::masked_key("short"), "*****");
    }

    #[test]
    fn test_list_providers() {
        let tmp = std::env::temp_dir().join(format!("alms-secrets-{}", uuid::Uuid::new_v4()));
        let path = tmp.join("secrets.json");
        let mut store = SecretsStore::load(&path).unwrap();

        store.set_key("openai", "key1").unwrap();
        store.set_key("anthropic", "key2").unwrap();
        let mut providers = store.list_providers();
        providers.sort();
        assert_eq!(providers, vec!["anthropic", "openai"]);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
