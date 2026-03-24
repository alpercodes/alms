//! Secure API key storage with optional encryption at rest.
//!
//! Keys are stored in a JSON file (`data/secrets.json`). When the
//! `ALMS_MASTER_KEY` environment variable is set, the file is encrypted
//! using AES-256-GCM with an HKDF-SHA256 derived key. Without the env var
//! the file is stored as plain text (with a warning log).
//!
//! Precedence: secrets file > env vars.

use crate::AlmsResult;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use hkdf::Hkdf;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Valid provider/secret names. Single source of truth for CLI, HTTP API, and UI.
pub const VALID_PROVIDERS: &[&str] = &["openai", "anthropic", "openrouter", "telegram"];

/// Environment variable name for the master encryption key.
pub const MASTER_KEY_ENV: &str = "ALMS_MASTER_KEY";

/// Magic byte that identifies an encrypted secrets file (version 1).
const ENCRYPTED_V1: u8 = 0x01;

/// Length of the random salt used in HKDF key derivation.
const SALT_LEN: usize = 32;

/// Length of the AES-GCM nonce (96 bits per NIST recommendation).
const NONCE_LEN: usize = 12;

/// Minimum header size: version(1) + salt(32) + nonce(12) = 45 bytes.
const HEADER_LEN: usize = 1 + SALT_LEN + NONCE_LEN;

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

/// In-memory secrets store backed by an optionally-encrypted JSON file.
#[derive(Clone)]
pub struct SecretsStore {
    path: PathBuf,
    keys: HashMap<String, String>,
    /// Cached master key (read from env on load, reused for save).
    master_key: Option<Vec<u8>>,
}

impl std::fmt::Debug for SecretsStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretsStore")
            .field("path", &self.path)
            .field("providers", &self.keys.keys().collect::<Vec<_>>())
            .field("encrypted", &self.master_key.is_some())
            .finish()
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct SecretsFile {
    #[serde(default)]
    api_keys: HashMap<String, String>,
}

/// Derive a 256-bit AES key from the master key and a random salt using HKDF-SHA256.
fn derive_key(master_key: &[u8], salt: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(salt), master_key);
    let mut okm = [0u8; 32];
    // "alms-secrets-v1" is the info string — binds the derived key to this purpose
    hk.expand(b"alms-secrets-v1", &mut okm)
        .expect("HKDF expand should never fail for 32-byte output");
    okm
}

/// Encrypt JSON bytes using AES-256-GCM.
/// Returns: version(1) || salt(32) || nonce(12) || ciphertext+tag
fn encrypt_data(plaintext: &[u8], master_key: &[u8]) -> AlmsResult<Vec<u8>> {
    let mut salt = [0u8; SALT_LEN];
    rand::thread_rng().fill_bytes(&mut salt);

    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);

    let key = derive_key(master_key, &salt);
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| crate::AlmsError::Runtime(format!("Failed to create cipher: {e}")))?;

    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| crate::AlmsError::Runtime(format!("Encryption failed: {e}")))?;

    let mut out = Vec::with_capacity(HEADER_LEN + ciphertext.len());
    out.push(ENCRYPTED_V1);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt data produced by `encrypt_data`.
fn decrypt_data(data: &[u8], master_key: &[u8]) -> AlmsResult<Vec<u8>> {
    if data.len() < HEADER_LEN {
        return Err(crate::AlmsError::Runtime(
            "Encrypted secrets file is too short (corrupted?)".to_string(),
        ));
    }

    if data[0] != ENCRYPTED_V1 {
        return Err(crate::AlmsError::Runtime(format!(
            "Unknown encrypted secrets version: 0x{:02x}",
            data[0]
        )));
    }

    let salt = &data[1..1 + SALT_LEN];
    let nonce_bytes = &data[1 + SALT_LEN..HEADER_LEN];
    let ciphertext = &data[HEADER_LEN..];

    let key = derive_key(master_key, salt);
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| crate::AlmsError::Runtime(format!("Failed to create cipher: {e}")))?;

    let nonce = Nonce::from_slice(nonce_bytes);
    cipher.decrypt(nonce, ciphertext).map_err(|_| {
        crate::AlmsError::Runtime(
            "Failed to decrypt secrets file. Is ALMS_MASTER_KEY correct?".to_string(),
        )
    })
}

/// Check if raw file bytes look like plain text JSON (starts with `{` or whitespace then `{`).
fn is_plaintext_json(data: &[u8]) -> bool {
    // Skip any leading whitespace (spaces, tabs, newlines, BOM)
    let trimmed = data
        .iter()
        .position(|&b| !b.is_ascii_whitespace() && b != 0xEF && b != 0xBB && b != 0xBF);
    match trimmed {
        Some(idx) => data[idx] == b'{',
        None => false, // empty file
    }
}

/// Read the master key from the environment variable, if set.
fn read_master_key() -> Option<Vec<u8>> {
    std::env::var(MASTER_KEY_ENV)
        .ok()
        .filter(|v| !v.is_empty())
        .map(|v| v.into_bytes())
}

impl SecretsStore {
    /// Create an empty in-memory secrets store with no file backing.
    pub fn empty() -> Self {
        Self {
            path: PathBuf::new(),
            keys: HashMap::new(),
            master_key: None,
        }
    }

    /// Load secrets from a file path, creating it if it doesn't exist.
    ///
    /// Reads `ALMS_MASTER_KEY` from the environment to determine encryption.
    ///
    /// If `ALMS_MASTER_KEY` is set:
    ///   - Encrypted file: decrypt with the key
    ///   - Plain text file: load and auto-encrypt (migration)
    ///
    /// If `ALMS_MASTER_KEY` is NOT set:
    ///   - Plain text file: load normally
    ///   - Encrypted file: return an error (key required)
    pub fn load(path: impl Into<PathBuf>) -> AlmsResult<Self> {
        Self::load_with_key(path, read_master_key())
    }

    /// Internal: load with an explicit master key (None = no encryption).
    /// This is the core implementation used by both `load()` and tests.
    fn load_with_key(path: impl Into<PathBuf>, master_key: Option<Vec<u8>>) -> AlmsResult<Self> {
        let path = path.into();

        // Track whether the initial load found plaintext JSON so we can
        // decide on migration without re-reading the file (avoids TOCTOU).
        let mut was_plaintext = false;

        let keys = if path.exists() {
            let raw = std::fs::read(&path).map_err(|e| {
                crate::AlmsError::Runtime(format!("Failed to read secrets file: {e}"))
            })?;

            if raw.is_empty() {
                HashMap::new()
            } else if is_plaintext_json(&raw) {
                was_plaintext = true;
                // Plain text JSON — parse it
                let content = String::from_utf8(raw).map_err(|e| {
                    crate::AlmsError::Runtime(format!("Secrets file is not valid UTF-8: {e}"))
                })?;
                let file: SecretsFile = serde_json::from_str(&content).map_err(|e| {
                    crate::AlmsError::Runtime(format!("Failed to parse secrets file: {e}"))
                })?;

                if master_key.is_some() {
                    tracing::info!("Found unencrypted secrets file — will encrypt on next save");
                } else {
                    tracing::warn!(
                        "Secrets stored unencrypted. Set {} for encryption at rest",
                        MASTER_KEY_ENV
                    );
                }

                file.api_keys
            } else if let Some(ref mk) = master_key {
                // Encrypted file — decrypt
                let plaintext = decrypt_data(&raw, mk)?;
                let content = String::from_utf8(plaintext).map_err(|e| {
                    crate::AlmsError::Runtime(format!(
                        "Decrypted secrets file is not valid UTF-8: {e}"
                    ))
                })?;
                let file: SecretsFile = serde_json::from_str(&content).map_err(|e| {
                    crate::AlmsError::Runtime(format!("Failed to parse decrypted secrets: {e}"))
                })?;
                file.api_keys
            } else {
                // Encrypted file but no master key
                return Err(crate::AlmsError::Runtime(format!(
                    "Secrets file is encrypted but {} is not set. \
                     Set the environment variable to decrypt",
                    MASTER_KEY_ENV
                )));
            }
        } else {
            if master_key.is_none() {
                tracing::warn!(
                    "No secrets file found and {} is not set. \
                     Secrets will be stored unencrypted",
                    MASTER_KEY_ENV
                );
            }
            HashMap::new()
        };

        let store = Self {
            path,
            keys,
            master_key,
        };

        // If we loaded plain text and have a master key, encrypt in place (migration).
        // Uses the `was_plaintext` flag from the initial parse — no re-read needed.
        if was_plaintext && store.master_key.is_some() && !store.keys.is_empty() {
            tracing::info!("Migrating secrets file to encrypted format");
            store.save()?;
        }

        Ok(store)
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

    /// Resolve an API key for a provider from the secrets store only.
    ///
    /// Env var fallback has been removed for security — API keys must be
    /// configured via `alms auth set <provider> <key>`.
    pub fn resolve_key(&self, provider: &str) -> Option<String> {
        self.get_key(provider).map(|k| k.to_string())
    }

    /// Detect API key environment variables that are set in the current process.
    ///
    /// Returns a list of `(env_var_name, provider)` tuples for each detected
    /// secret env var. Callers should print a deprecation warning telling users
    /// to migrate to `alms auth set`.
    pub fn detect_deprecated_env_keys() -> Vec<(&'static str, &'static str)> {
        const DEPRECATED_VARS: &[(&str, &str)] = &[
            ("OPENROUTER_API_KEY", "openrouter"),
            ("OPENAI_API_KEY", "openai"),
            ("ANTHROPIC_API_KEY", "anthropic"),
            ("TELEGRAM_BOT_TOKEN", "telegram"),
        ];
        DEPRECATED_VARS
            .iter()
            .filter(|(var, _)| std::env::var(var).ok().filter(|v| !v.is_empty()).is_some())
            .copied()
            .collect()
    }

    /// Save secrets to disk, encrypting if a master key is available.
    ///
    /// Uses atomic write (write-to-temp, fsync, rename) so a crash mid-write
    /// cannot corrupt the secrets file.  On Windows, where rename-over-existing
    /// may fail, we fall back to a direct write.
    fn save(&self) -> AlmsResult<()> {
        let file = SecretsFile {
            api_keys: self.keys.clone(),
        };
        let json = serde_json::to_string_pretty(&file)
            .map_err(|e| crate::AlmsError::Runtime(format!("Failed to serialize secrets: {e}")))?;

        // Ensure parent directory exists
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                crate::AlmsError::Runtime(format!("Failed to create secrets directory: {e}"))
            })?;
        }

        let data = if let Some(ref mk) = self.master_key {
            encrypt_data(json.as_bytes(), mk)?
        } else {
            tracing::warn!(
                "Saving secrets unencrypted. Set {} for encryption at rest",
                MASTER_KEY_ENV
            );
            json.into_bytes()
        };

        // Atomic write: temp file -> fsync -> rename.
        // If rename fails (Windows can reject rename-over-existing), fall back
        // to a direct write so we don't lose the data entirely.
        //
        // On Unix, the temp file is created with mode 0600 to prevent any
        // window where the secrets file is world-readable (TOCTOU fix).
        let tmp_path = self.path.with_extension("tmp");
        let atomic_ok = (|| -> std::io::Result<()> {
            {
                use std::io::Write;

                #[cfg(unix)]
                let mut f = {
                    use std::os::unix::fs::OpenOptionsExt;
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .mode(0o600)
                        .open(&tmp_path)?
                };

                #[cfg(not(unix))]
                let mut f = std::fs::File::create(&tmp_path)?;

                f.write_all(&data)?;
                f.sync_all()?;
            }
            std::fs::rename(&tmp_path, &self.path)?;
            Ok(())
        })();

        if let Err(rename_err) = atomic_ok {
            // Clean up temp file if it exists
            let _ = std::fs::remove_file(&tmp_path);
            tracing::warn!(
                "Atomic rename failed ({}), falling back to direct write",
                rename_err
            );
            std::fs::write(&self.path, &data).map_err(|e| {
                crate::AlmsError::Runtime(format!("Failed to write secrets file: {e}"))
            })?;
        }

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
        // Use load_with_key(None) to avoid env var dependency
        let mut store = SecretsStore::load_with_key(&path, None).unwrap();

        assert!(store.get_key("openai").is_none());

        store.set_key("openai", "sk-test-key-12345").unwrap();
        assert_eq!(store.get_key("openai"), Some("sk-test-key-12345"));

        // Verify persisted (reload without key — should be plain text)
        let store2 = SecretsStore::load_with_key(&path, None).unwrap();
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
        let mut store = SecretsStore::load_with_key(&path, None).unwrap();

        store.set_key("openai", "key1").unwrap();
        store.set_key("anthropic", "key2").unwrap();
        let mut providers = store.list_providers();
        providers.sort();
        assert_eq!(providers, vec!["anthropic", "openai"]);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let master_key = b"test-master-key-very-secret-1234";
        let plaintext = b"hello world, this is secret data";

        let encrypted = encrypt_data(plaintext, master_key).unwrap();
        assert_ne!(encrypted.as_slice(), plaintext);
        assert_eq!(encrypted[0], ENCRYPTED_V1);
        assert!(encrypted.len() > HEADER_LEN);

        let decrypted = decrypt_data(&encrypted, master_key).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_encrypt_wrong_key_fails() {
        let master_key = b"correct-key";
        let wrong_key = b"wrong-key!!";
        let plaintext = b"secret data";

        let encrypted = encrypt_data(plaintext, master_key).unwrap();
        let result = decrypt_data(&encrypted, wrong_key);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("ALMS_MASTER_KEY"),
            "Error should mention ALMS_MASTER_KEY: {err_msg}"
        );
    }

    #[test]
    fn test_encrypt_truncated_data_fails() {
        let master_key = b"test-key";
        let plaintext = b"data";

        let encrypted = encrypt_data(plaintext, master_key).unwrap();
        // Truncate to less than header size
        let truncated = &encrypted[..10];
        let result = decrypt_data(truncated, master_key);
        assert!(result.is_err());
    }

    #[test]
    fn test_is_plaintext_json() {
        assert!(is_plaintext_json(b"{}"));
        assert!(is_plaintext_json(b"  {\"key\": \"val\"}"));
        assert!(is_plaintext_json(b"\n\t{\"api_keys\":{}}"));
        assert!(!is_plaintext_json(b"\x01random-encrypted-bytes"));
        assert!(!is_plaintext_json(b""));
        assert!(!is_plaintext_json(b"   ")); // whitespace only
    }

    #[test]
    fn test_encrypted_secrets_roundtrip_via_store() {
        let mk = b"test-roundtrip-key-32-chars-long!".to_vec();

        let tmp = std::env::temp_dir().join(format!("alms-secrets-enc-{}", uuid::Uuid::new_v4()));
        let path = tmp.join("secrets.json");

        // Create and save encrypted
        let mut store = SecretsStore::load_with_key(&path, Some(mk.clone())).unwrap();
        store.set_key("openai", "sk-encrypted-test-key").unwrap();
        store.set_key("anthropic", "sk-ant-encrypted-key").unwrap();

        // Verify file is NOT plain text JSON
        let raw = std::fs::read(&path).unwrap();
        assert_eq!(raw[0], ENCRYPTED_V1, "File should start with version byte");
        assert!(
            !is_plaintext_json(&raw),
            "File should not look like plain JSON"
        );

        // Re-load and verify contents
        let store2 = SecretsStore::load_with_key(&path, Some(mk)).unwrap();
        assert_eq!(store2.get_key("openai"), Some("sk-encrypted-test-key"));
        assert_eq!(store2.get_key("anthropic"), Some("sk-ant-encrypted-key"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_migration_plaintext_to_encrypted() {
        let tmp =
            std::env::temp_dir().join(format!("alms-secrets-migrate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("secrets.json");

        // Write a plain text secrets file directly
        let plain = SecretsFile {
            api_keys: HashMap::from([("openai".to_string(), "sk-plain-key".to_string())]),
        };
        let json = serde_json::to_string_pretty(&plain).unwrap();
        std::fs::write(&path, &json).unwrap();

        // Verify it's plain text
        assert!(is_plaintext_json(&std::fs::read(&path).unwrap()));

        // Load with master key — should trigger migration
        let mk = b"migration-test-key-long-enough!!".to_vec();
        let store = SecretsStore::load_with_key(&path, Some(mk.clone())).unwrap();
        assert_eq!(store.get_key("openai"), Some("sk-plain-key"));

        // Verify file is now encrypted
        let raw = std::fs::read(&path).unwrap();
        assert_eq!(
            raw[0], ENCRYPTED_V1,
            "File should be encrypted after migration"
        );
        assert!(!is_plaintext_json(&raw));

        // Re-load to confirm encrypted file works
        let store2 = SecretsStore::load_with_key(&path, Some(mk)).unwrap();
        assert_eq!(store2.get_key("openai"), Some("sk-plain-key"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_no_key_encrypted_file_errors() {
        let tmp = std::env::temp_dir().join(format!("alms-secrets-nokey-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("secrets.json");

        // Create an encrypted file using the low-level encrypt function directly
        let plain = SecretsFile {
            api_keys: HashMap::from([("openai".to_string(), "sk-test".to_string())]),
        };
        let json = serde_json::to_string_pretty(&plain).unwrap();
        let encrypted = encrypt_data(json.as_bytes(), b"nokey-test-key-is-long-enough!!!").unwrap();
        std::fs::write(&path, &encrypted).unwrap();

        // Try to load without a key — should fail
        let result = SecretsStore::load_with_key(&path, None);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains(MASTER_KEY_ENV),
            "Error should mention {MASTER_KEY_ENV}: {err_msg}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_no_key_plaintext_fallback() {
        let tmp =
            std::env::temp_dir().join(format!("alms-secrets-nokey-plain-{}", uuid::Uuid::new_v4()));
        let path = tmp.join("secrets.json");

        let mut store = SecretsStore::load_with_key(&path, None).unwrap();
        store.set_key("openai", "sk-plain-fallback").unwrap();

        // Verify file is plain text JSON
        let raw = std::fs::read(&path).unwrap();
        assert!(
            is_plaintext_json(&raw),
            "Without master key, file should be plain text"
        );

        // Verify content
        let content = String::from_utf8(raw).unwrap();
        assert!(content.contains("sk-plain-fallback"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_unique_ciphertexts() {
        // Same plaintext encrypted twice should produce different ciphertexts
        // because salt and nonce are random
        let master_key = b"uniqueness-test-key";
        let plaintext = b"same data";

        let enc1 = encrypt_data(plaintext, master_key).unwrap();
        let enc2 = encrypt_data(plaintext, master_key).unwrap();

        assert_ne!(
            enc1, enc2,
            "Two encryptions of the same data should differ (random salt/nonce)"
        );

        // But both should decrypt to the same thing
        let dec1 = decrypt_data(&enc1, master_key).unwrap();
        let dec2 = decrypt_data(&enc2, master_key).unwrap();
        assert_eq!(dec1, dec2);
        assert_eq!(dec1, plaintext);
    }

    #[test]
    fn test_derive_key_deterministic() {
        let master = b"test-master";
        let salt = b"test-salt-that-is-32-bytes-long!";
        let key1 = derive_key(master, salt);
        let key2 = derive_key(master, salt);
        assert_eq!(key1, key2, "Same inputs must produce same derived key");
    }

    #[test]
    fn test_derive_key_differs_by_salt() {
        let master = b"test-master";
        let salt1 = b"salt-aaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let salt2 = b"salt-bbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let key1 = derive_key(master, salt1);
        let key2 = derive_key(master, salt2);
        assert_ne!(key1, key2, "Different salts must produce different keys");
    }

    #[test]
    fn test_corrupted_ciphertext_fails_gracefully() {
        let master_key = b"corruption-test-key-long-enough!";
        let plaintext = b"sensitive data that must be authenticated";

        let mut encrypted = encrypt_data(plaintext, master_key).unwrap();

        // Flip a byte in the middle of the ciphertext body (past the header).
        // This exercises the GCM authentication tag check specifically —
        // the key is correct, but the ciphertext has been tampered with.
        let flip_idx = HEADER_LEN + (encrypted.len() - HEADER_LEN) / 2;
        encrypted[flip_idx] ^= 0xFF;

        let result = decrypt_data(&encrypted, master_key);
        assert!(
            result.is_err(),
            "Corrupted ciphertext must be rejected by GCM auth tag verification"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("ALMS_MASTER_KEY"),
            "Error should hint at key/corruption issue: {err_msg}"
        );
    }

    #[test]
    fn test_resolve_key_no_env_fallback() {
        // resolve_key must NOT fall back to env vars — only secrets store.
        // Set the env var first, then verify resolve_key ignores it.
        let original = std::env::var("OPENAI_API_KEY").ok();
        unsafe {
            std::env::set_var("OPENAI_API_KEY", "sk-env-should-be-ignored");
        }

        let store = SecretsStore::empty();
        let result = store.resolve_key("openai");

        // Restore original env var before asserting (panic-safe cleanup).
        match &original {
            Some(v) => unsafe { std::env::set_var("OPENAI_API_KEY", v) },
            None => unsafe { std::env::remove_var("OPENAI_API_KEY") },
        }

        assert!(
            result.is_none(),
            "resolve_key must not fall back to env vars, even when OPENAI_API_KEY is set"
        );
    }

    #[test]
    fn test_resolve_key_from_store() {
        let tmp = std::env::temp_dir().join(format!("alms-secrets-{}", uuid::Uuid::new_v4()));
        let path = tmp.join("secrets.json");
        let mut store = SecretsStore::load_with_key(&path, None).unwrap();
        store.set_key("openai", "sk-from-store").unwrap();

        assert_eq!(store.resolve_key("openai"), Some("sk-from-store".into()));
        assert!(store.resolve_key("anthropic").is_none());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_valid_providers_includes_telegram() {
        assert!(
            VALID_PROVIDERS.contains(&"telegram"),
            "VALID_PROVIDERS must include telegram"
        );
    }
}
